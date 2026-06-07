//! Native [`OceanCanvasView`] — renders the agent surface directly from a
//! [`CanvasLedger`] with GPUI primitives (Slice 5, gpui_masterbuild.md §9).
//!
//! This is the native replacement for the tldraw webview projection: the canvas
//! is drawn from Ocean's own ledger, so an agent-created card appears without any
//! web layer mounted (Gate D).
//!
//! # Structure
//!
//! - **Pure helpers** (top of file) compute everything window-free: per-kind
//!   styling, port anchor positions, edge endpoint geometry, grid line offsets,
//!   the component summary line. These are unit-tested headlessly.
//! - **[`OceanCanvasView`]** holds the view-local interaction state — the
//!   `viewport` (pan/zoom), `hover`, and `focus` — and its [`Render`] impl turns
//!   the ledger plus those helpers into an absolutely-positioned GPUI element
//!   tree. The element tree is *not* exercised in tests (it needs a live window);
//!   the geometry it is built from is.
//!
//! The view reads its ledger through a small [`LedgerSource`] handle so the shell
//! can own the canvas state and hand the view a borrow each frame.

use gpui::{
    div, px, Context, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Render, ScrollWheelEvent, Styled, Window,
};
use serde_json::Value;

use super::hit_test::{hit_test, paint_order, Vec2, ViewportTransform};
use super::ledger::{CanvasComponent, CanvasLedger, ComponentKind};
use super::patch::{ActorRef, ComponentId, EdgeId, Rect, SurfacePatch, Viewport};
use super::templates::{NodeStatus, TemplateContent};
use crate::shell::theme;

// ===========================================================================
// Pure helpers (window-free, unit-tested)
// ===========================================================================

/// Spacing of the background grid in **canvas** units. Scaled by zoom at paint.
pub const GRID_SIZE: f32 = 24.0;

/// How big a port anchor is drawn, in canvas units.
pub const PORT_RADIUS: f32 = 5.0;

/// The visual style for one component kind: fill, border, and accent (title)
/// colors. Pure data — `Hsla` carries no window dependency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComponentStyle {
    pub fill: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    /// Border thickness in canvas units.
    pub border_width: f32,
}

/// Pick the base style for a structural [`ComponentKind`]. Selection / focus /
/// active-write highlights are layered on top of this by the renderer.
pub fn style_for_kind(kind: ComponentKind) -> ComponentStyle {
    // Three families: containers (frame/lane) read quietly, work objects
    // (card/text/stat/media/table) read as raised panels, graph objects
    // (node/port) carry the accent.
    match kind {
        ComponentKind::Frame | ComponentKind::Lane => ComponentStyle {
            fill: theme::panel(),
            border: theme::rule(),
            accent: theme::muted(),
            border_width: 1.0,
        },
        ComponentKind::Node | ComponentKind::Port => ComponentStyle {
            fill: theme::panel_raised(),
            border: theme::accent(),
            accent: theme::accent_dark(),
            border_width: 1.5,
        },
        ComponentKind::Card
        | ComponentKind::TextBlock
        | ComponentKind::Table
        | ComponentKind::MediaSlot
        | ComponentKind::Stat
        | ComponentKind::EdgeLabel => ComponentStyle {
            fill: theme::panel_raised(),
            border: theme::rule(),
            accent: theme::accent_dark(),
            border_width: 1.0,
        },
    }
}

/// Resolve the drawable [`TemplateContent`] for a component, from its preserved
/// template name and `content` JSON. Returns `None` for plain primitives (which
/// fall back to the generic title+summary box). This is the render-side analogue
/// of [`style_for_kind`]: it tells the renderer *what shapes* a templated
/// work-object draws, where `style_for_kind` tells it *what colors*.
pub fn template_content_for(component: &CanvasComponent) -> Option<TemplateContent> {
    TemplateContent::resolve(&component.template, &component.content)
}

/// The accent color for a workflow-node [`NodeStatus`] badge.
pub fn status_color(status: NodeStatus) -> Hsla {
    match status {
        NodeStatus::Idle => theme::muted(),
        NodeStatus::Running => theme::user(),
        NodeStatus::Ok => theme::green(),
        NodeStatus::Error => theme::danger(),
        NodeStatus::Waiting => theme::thinking(),
    }
}

/// The center of a [`Rect`] in canvas space.
pub fn rect_center(rect: &Rect) -> Vec2 {
    Vec2::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
}

/// Default anchor points around a component, in canvas space: the four edge
/// midpoints, returned as `(top, right, bottom, left)`. Edges that don't name a
/// specific port attach to the nearest of these.
pub fn edge_anchors(rect: &Rect) -> [Vec2; 4] {
    [
        Vec2::new(rect.x + rect.w / 2.0, rect.y),            // top
        Vec2::new(rect.x + rect.w, rect.y + rect.h / 2.0),   // right
        Vec2::new(rect.x + rect.w / 2.0, rect.y + rect.h),   // bottom
        Vec2::new(rect.x, rect.y + rect.h / 2.0),            // left
    ]
}

/// Choose the connection points for an edge between two component rects: the
/// pair of edge-midpoint anchors (one per rect) that are closest to each other.
/// Returns `(from_point, to_point)` in canvas space.
pub fn edge_endpoints(from: &Rect, to: &Rect) -> (Vec2, Vec2) {
    let from_anchors = edge_anchors(from);
    let to_anchors = edge_anchors(to);
    let mut best = (from_anchors[0], to_anchors[0]);
    let mut best_dist = f32::MAX;
    for &a in &from_anchors {
        for &b in &to_anchors {
            let d = (a.x - b.x).powi(2) + (a.y - b.y).powi(2);
            if d < best_dist {
                best_dist = d;
                best = (a, b);
            }
        }
    }
    best
}

/// Canvas-space offsets of the vertical grid lines visible across a viewport of
/// `width_canvas` canvas units starting at `pan_x`. Returns the canvas x of each
/// line. Used to draw the background grid.
pub fn grid_line_offsets(pan: f32, span_canvas: f32, grid: f32) -> Vec<f32> {
    if grid <= 0.0 || span_canvas <= 0.0 {
        return Vec::new();
    }
    let first = (pan / grid).floor() * grid;
    let mut lines = Vec::new();
    let mut x = first;
    // Bound the loop defensively; a sane viewport yields a few dozen lines.
    let max_lines = (span_canvas / grid).ceil() as i64 + 2;
    let mut count = 0;
    while x <= pan + span_canvas && count <= max_lines {
        lines.push(x);
        x += grid;
        count += 1;
    }
    lines
}

/// A compact one-line label for a component, derived from its content/template.
/// Used as the card body fallback and for accessibility-style summaries.
pub fn component_summary(component: &CanvasComponent) -> String {
    if let Some(title) = component
        .content
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return title.to_string();
    }
    if let Some(text) = component
        .content
        .get("body")
        .or_else(|| component.content.get("text"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return text.lines().next().unwrap_or(text).to_string();
    }
    // Fall back to the template name so an empty card still reads.
    component.template.clone()
}

/// The title shown in a component header: an explicit `title`, else the id.
pub fn component_title(component: &CanvasComponent) -> String {
    component
        .content
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| component.id.to_string())
}

/// How the renderer should outline a component this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineState {
    /// No special outline.
    None,
    /// Pointer is hovering the component.
    Hover,
    /// Component is in the ledger selection.
    Selected,
    /// Component has keyboard/explicit focus.
    Focused,
    /// An agent is actively writing to this component (most prominent).
    ActiveWrite,
}

impl OutlineState {
    /// Resolve the strongest applicable outline for a component given the view
    /// state. Precedence (strongest first): active-write > focus > selection >
    /// hover > none.
    pub fn resolve(
        is_active_write: bool,
        is_focused: bool,
        is_selected: bool,
        is_hovered: bool,
    ) -> Self {
        if is_active_write {
            Self::ActiveWrite
        } else if is_focused {
            Self::Focused
        } else if is_selected {
            Self::Selected
        } else if is_hovered {
            Self::Hover
        } else {
            Self::None
        }
    }

    /// Outline color, or `None` for [`OutlineState::None`].
    pub fn color(self) -> Option<Hsla> {
        match self {
            Self::None => None,
            Self::Hover => Some(theme::muted()),
            Self::Selected => Some(theme::accent()),
            Self::Focused => Some(theme::accent_dark()),
            Self::ActiveWrite => Some(theme::user()),
        }
    }

    /// Outline thickness in canvas units.
    pub fn width(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Hover => 1.0,
            Self::Selected | Self::Focused => 2.0,
            Self::ActiveWrite => 3.0,
        }
    }
}

// ===========================================================================
// OceanCanvasView (GPUI view)
// ===========================================================================

/// View-local interaction state for the native canvas. The authoritative
/// component/edge data lives in the [`CanvasLedger`] the view is handed each
/// frame; this struct only holds the ephemeral viewport + pointer state that is
/// the *view's* responsibility, not the ledger's.
#[derive(Debug, Clone, Default)]
pub struct CanvasInteraction {
    /// Pan/zoom. Mirrors the ledger viewport but is owned by the view so panning
    /// the camera does not require a ledger mutation/revision bump.
    pub viewport: Viewport,
    /// Component currently under the pointer, if any.
    pub hover: Option<ComponentId>,
    /// Component with explicit focus, if any.
    pub focus: Option<ComponentId>,
    /// Component an agent is actively writing to this turn, if any (driven by the
    /// shell from patch events — Slice 6).
    pub active_write: Option<ComponentId>,
}

impl CanvasInteraction {
    /// Current screen↔canvas transform for this interaction state.
    pub fn transform(&self) -> ViewportTransform {
        ViewportTransform::new(self.viewport)
    }

    /// Apply a relative pan in screen pixels (e.g. from a drag or scroll). Pan is
    /// stored in canvas units, so a screen delta is divided by zoom.
    pub fn pan_by_screen(&mut self, dx: f32, dy: f32) {
        let zoom = self.transform().zoom();
        self.viewport.x -= dx / zoom;
        self.viewport.y -= dy / zoom;
    }

    /// Multiply the zoom by `factor`, clamped to a sane range.
    pub fn zoom_by(&mut self, factor: f32) {
        let next = (self.viewport.zoom * factor).clamp(0.2, 4.0);
        self.viewport.zoom = next;
    }

    /// Resolve the outline state for one component id under the current view +
    /// ledger selection.
    pub fn outline_for(&self, id: &ComponentId, ledger: &CanvasLedger) -> OutlineState {
        OutlineState::resolve(
            self.active_write.as_ref() == Some(id),
            self.focus.as_ref() == Some(id),
            ledger.selection.component_ids.iter().any(|c| c == id),
            self.hover.as_ref() == Some(id),
        )
    }
}

/// The native canvas view. Renders one [`CanvasLedger`] supplied by `source`.
///
/// The view does **not** own the ledger — the shell does. `source` is a closure
/// the shell installs that yields the active ledger (or `None` when no canvas is
/// active). This keeps the ledger single-sourced in the shell while letting the
/// view render and hit-test against it.
pub struct OceanCanvasView {
    interaction: CanvasInteraction,
    source: LedgerSource,
    sink: Option<LedgerSink>,
}

/// A handle the shell installs so the view can borrow the active ledger each
/// frame without owning it. Boxed closure returning an owned clone keeps the
/// borrow checker happy across the GPUI render boundary; ledgers are small
/// (ids + rects + compact content) so the per-frame clone is cheap relative to
/// the paint, and avoids threading a lifetime through the view.
pub type LedgerSource = std::sync::Arc<dyn Fn() -> Option<CanvasLedger> + Send + Sync>;

/// A handle the shell installs so the view can write a patch back to the *same*
/// authoritative ledger cell the [`LedgerSource`] reads from. The view's pointer
/// handlers can therefore mutate ledger state (e.g. apply a `Select` on click)
/// without owning the ledger or holding a GPUI context — closing the
/// human→agent feedback loop (OCEAN-186). `None` in headless previews/tests
/// where there is no shared cell to write through.
pub type LedgerSink = std::sync::Arc<dyn Fn(SurfacePatch, ActorRef) + Send + Sync>;

impl OceanCanvasView {
    /// Create a view backed by `source`.
    pub fn new(source: LedgerSource) -> Self {
        Self {
            interaction: CanvasInteraction::default(),
            source,
            sink: None,
        }
    }

    /// Install the write-back [`LedgerSink`] so user interactions (e.g. a
    /// component-selecting click) flow back into the authoritative ledger. The
    /// shell calls this once after construction, handing the view a closure that
    /// applies a patch to the *same* shared cell its [`LedgerSource`] reads.
    pub fn set_sink(&mut self, sink: LedgerSink) {
        self.sink = Some(sink);
    }

    /// A view backed by a fixed ledger snapshot (handy for previews/tests).
    pub fn from_ledger(ledger: CanvasLedger) -> Self {
        let ledger = std::sync::Arc::new(ledger);
        Self::new(std::sync::Arc::new(move || Some((*ledger).clone())))
    }

    /// Borrow the current interaction state (viewport/hover/focus).
    pub fn interaction(&self) -> &CanvasInteraction {
        &self.interaction
    }

    /// Mutable access to interaction state, for the shell's pointer handlers.
    pub fn interaction_mut(&mut self) -> &mut CanvasInteraction {
        &mut self.interaction
    }

    /// Resolve the active ledger snapshot for this frame.
    fn ledger(&self) -> Option<CanvasLedger> {
        (self.source)()
    }

    /// Build the element for one component, given the active ledger and transform.
    fn render_component(
        &self,
        component: &CanvasComponent,
        ledger: &CanvasLedger,
        transform: &ViewportTransform,
    ) -> impl IntoElement {
        let screen = transform.canvas_rect_to_screen(component.rect);
        let style = style_for_kind(component.kind);
        let outline = self.interaction.outline_for(&component.id, ledger);

        let (border_color, border_w) = match outline.color() {
            Some(color) => (color, outline.width().max(style.border_width)),
            None => (style.border, style.border_width),
        };

        let pad = transform.scale(8.0).max(2.0);

        let mut node = div()
            .absolute()
            .left(px(screen.x))
            .top(px(screen.y))
            .w(px(screen.w.max(1.0)))
            .h(px(screen.h.max(1.0)))
            .bg(style.fill)
            .border(px(border_w))
            .border_color(border_color)
            .p(px(pad))
            .overflow_hidden();

        // Per-template content shapes (Slice 8): a templated work-object draws
        // real slots (status badge, stat value, media placeholder, tally rows…)
        // instead of the generic title+summary box. Plain primitives fall back.
        if let Some(content) = template_content_for(component) {
            node = node.children(self.render_template_content(&content, component, transform));
        } else {
            node = node.child(self.render_header(component, style, transform));
            // Body line for kinds that carry text content.
            if !matches!(component.kind, ComponentKind::Port | ComponentKind::EdgeLabel) {
                node = node.child(self.render_body_line(component_summary(component), transform));
            }
        }

        node
    }

    /// The mono-font header line (title) shared by every component.
    fn render_header(
        &self,
        component: &CanvasComponent,
        style: ComponentStyle,
        transform: &ViewportTransform,
    ) -> impl IntoElement {
        div()
            .font_family(theme::MONO_FONT)
            .text_size(px(transform.scale(11.0).max(7.0)))
            .text_color(style.accent)
            .whitespace_nowrap()
            .text_ellipsis()
            .child(component_title(component))
    }

    /// A wrapping body paragraph in the UI font.
    fn render_body_line(&self, text: String, transform: &ViewportTransform) -> impl IntoElement {
        div()
            .pt(px(transform.scale(4.0).max(1.0)))
            .font_family(theme::UI_FONT)
            .text_size(px(transform.scale(12.0).max(7.0)))
            .text_color(theme::ink())
            .whitespace_normal()
            .child(text)
    }

    /// A small pill label, e.g. a status badge or a port chip.
    fn render_chip(&self, text: String, color: Hsla, transform: &ViewportTransform) -> impl IntoElement {
        div()
            .px(px(transform.scale(6.0).max(2.0)))
            .py(px(transform.scale(2.0).max(1.0)))
            .border(px(1.0))
            .border_color(color)
            .text_color(color)
            .font_family(theme::MONO_FONT)
            .text_size(px(transform.scale(10.0).max(6.0)))
            .whitespace_nowrap()
            .child(text)
    }

    /// Build the drawable elements for a templated component's resolved
    /// [`TemplateContent`]. Each arm matches the §5 template's content shape: a
    /// brief draws title+body+metadata, a workflow node draws a status badge and
    /// port chips, a stat draws a large value, a storyboard draws a media
    /// placeholder + caption, a proposal draws tally rows.
    fn render_template_content(
        &self,
        content: &TemplateContent,
        component: &CanvasComponent,
        transform: &ViewportTransform,
    ) -> Vec<gpui::AnyElement> {
        let title_size = px(transform.scale(11.0).max(7.0));
        let mut out: Vec<gpui::AnyElement> = Vec::new();

        let title_el = |t: &Option<String>, fallback: String| {
            let text = t.clone().unwrap_or(fallback);
            div()
                .font_family(theme::MONO_FONT)
                .text_size(title_size)
                .text_color(theme::accent_dark())
                .whitespace_nowrap()
                .text_ellipsis()
                .child(text)
                .into_any_element()
        };

        match content {
            TemplateContent::Brief { title, body, metadata } => {
                out.push(title_el(title, component.id.to_string()));
                if let Some(body) = body {
                    out.push(self.render_body_line(body.clone(), transform).into_any_element());
                }
                for (k, v) in metadata {
                    out.push(
                        div()
                            .pt(px(transform.scale(2.0).max(1.0)))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(transform.scale(10.0).max(6.0)))
                            .text_color(theme::muted())
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(format!("{k}: {v}"))
                            .into_any_element(),
                    );
                }
            }
            TemplateContent::WorkflowNode { title, status, inputs, outputs } => {
                // Header row: title + status badge.
                out.push(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .child(title_el(title, component.id.to_string()))
                        .child(self.render_chip(
                            status.label().to_string(),
                            status_color(*status),
                            transform,
                        ))
                        .into_any_element(),
                );
                // Port summary line.
                if !inputs.is_empty() || !outputs.is_empty() {
                    out.push(
                        div()
                            .pt(px(transform.scale(4.0).max(1.0)))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(transform.scale(10.0).max(6.0)))
                            .text_color(theme::muted())
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(format!("in {} / out {}", inputs.len(), outputs.len()))
                            .into_any_element(),
                    );
                }
            }
            TemplateContent::KanbanColumn { title, count } => {
                out.push(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .child(title_el(title, component.id.to_string()))
                        .child(self.render_chip(
                            count.unwrap_or(0).to_string(),
                            theme::muted(),
                            transform,
                        ))
                        .into_any_element(),
                );
            }
            TemplateContent::StoryboardFrame { caption, media } => {
                // A media placeholder fills most of the frame; caption sits below.
                out.push(
                    div()
                        .w_full()
                        .h(px(transform.scale(110.0).max(8.0)))
                        .bg(theme::panel())
                        .border(px(1.0))
                        .border_color(theme::rule())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(transform.scale(10.0).max(6.0)))
                                .text_color(theme::muted())
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(media.clone().unwrap_or_else(|| "media".to_string())),
                        )
                        .into_any_element(),
                );
                if let Some(caption) = caption {
                    out.push(self.render_body_line(caption.clone(), transform).into_any_element());
                }
            }
            TemplateContent::Stat { label, value, delta } => {
                // Big value, small label beneath, optional delta chip.
                out.push(
                    div()
                        .font_family(theme::MONO_FONT)
                        .text_size(px(transform.scale(24.0).max(10.0)))
                        .text_color(theme::ink())
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(value.clone().unwrap_or_else(|| "—".to_string()))
                        .into_any_element(),
                );
                let mut footer = div().flex().flex_row().justify_between().items_center().child(
                    div()
                        .font_family(theme::UI_FONT)
                        .text_size(px(transform.scale(11.0).max(7.0)))
                        .text_color(theme::muted())
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label.clone().unwrap_or_else(|| component.id.to_string())),
                );
                if let Some(delta) = delta {
                    footer = footer.child(self.render_chip(delta.clone(), theme::green(), transform));
                }
                out.push(footer.into_any_element());
            }
            TemplateContent::LonghouseProposal { title, body, tally } => {
                out.push(title_el(title, component.id.to_string()));
                if let Some(body) = body {
                    out.push(self.render_body_line(body.clone(), transform).into_any_element());
                }
                for row in tally {
                    out.push(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .pt(px(transform.scale(2.0).max(1.0)))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(transform.scale(11.0).max(7.0)))
                            .text_color(theme::ink())
                            .child(div().whitespace_nowrap().text_ellipsis().child(row.label.clone()))
                            .child(
                                div()
                                    .text_color(theme::accent())
                                    .child(row.count.to_string()),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        out
    }

    /// Build a thin element standing in for an edge between two components. GPUI
    /// 0.2 has no first-class line primitive in this crate's element set, so an
    /// edge is drawn as a 1px-tall accent bar spanning the bounding box between
    /// its endpoints — enough to read connectivity natively. (A proper
    /// path/bezier renderer is a later refinement; the geometry it needs is
    /// already provided by [`edge_endpoints`].)
    fn render_edge(
        &self,
        from_rect: &Rect,
        to_rect: &Rect,
        transform: &ViewportTransform,
    ) -> impl IntoElement {
        let (a_canvas, b_canvas) = edge_endpoints(from_rect, to_rect);
        let a = transform.canvas_to_screen(a_canvas);
        let b = transform.canvas_to_screen(b_canvas);
        let left = a.x.min(b.x);
        let top = a.y.min(b.y);
        let w = (a.x - b.x).abs().max(1.0);
        let h = (a.y - b.y).abs().max(1.0);
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(w))
            .h(px(h))
            .border_b(px(1.5))
            .border_color(theme::accent())
    }
}

impl Render for OceanCanvasView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let transform = self.interaction.transform();
        let ledger = self.ledger();

        // Root: clipped canvas surface with the dark background.
        let mut root = div()
            .relative()
            .size_full()
            .bg(theme::background())
            .overflow_hidden();

        // --- background grid -------------------------------------------------
        // Drawn as faint vertical/horizontal rules. Offsets come from the pure
        // helper so the spacing tracks pan/zoom.
        let grid_step = transform.scale(GRID_SIZE).max(4.0);
        let mut grid = div().absolute().top_0().left_0().right_0().bottom_0();
        // A bounded number of lines keeps this cheap regardless of zoom-out.
        for i in 0..200u32 {
            let off = px(i as f32 * grid_step);
            grid = grid
                .child(
                    div()
                        .absolute()
                        .left(off)
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .bg(theme::frame()),
                )
                .child(
                    div()
                        .absolute()
                        .top(off)
                        .left_0()
                        .right_0()
                        .h(px(1.0))
                        .bg(theme::frame()),
                );
        }
        root = root.child(grid);

        if let Some(ledger) = ledger.as_ref() {
            // --- edges (under components) ------------------------------------
            let mut edge_layer = div().absolute().top_0().left_0().right_0().bottom_0();
            for edge in ledger.edges.values() {
                if let (Some(from), Some(to)) = (
                    ledger.components.get(&edge.from.component_id),
                    ledger.components.get(&edge.to.component_id),
                ) {
                    edge_layer = edge_layer.child(self.render_edge(&from.rect, &to.rect, &transform));
                }
            }
            root = root.child(edge_layer);

            // --- components (in paint order: ascending z, then insertion) ----
            let mut component_layer = div().absolute().top_0().left_0().right_0().bottom_0();
            for (id, _rect) in paint_order(ledger) {
                if let Some(component) = ledger.components.get(&id) {
                    component_layer =
                        component_layer.child(self.render_component(component, ledger, &transform));
                }
            }
            root = root.child(component_layer);
        } else {
            root = root.child(
                div()
                    .absolute()
                    .top(px(16.0))
                    .left(px(16.0))
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("no active canvas"),
            );
        }

        // --- pointer handlers -----------------------------------------------
        // Hover updates focus highlighting; left-click selects via hit_test;
        // scroll pans. These mutate only the view's interaction state and
        // request a repaint; ledger mutations from selection are the shell's
        // concern (Slice 6) and are not wired here.
        root.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _window, cx| {
            view.on_pointer_move(ev.position.x.into(), ev.position.y.into());
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, ev: &MouseDownEvent, _window, cx| {
                view.on_left_down(ev.position.x.into(), ev.position.y.into());
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|view, ev: &ScrollWheelEvent, _window, cx| {
            let delta = ev.delta.pixel_delta(px(1.0));
            view.interaction.pan_by_screen(delta.x.into(), delta.y.into());
            cx.notify();
        }))
    }
}

impl OceanCanvasView {
    /// Handle a pointer move at a screen position relative to the canvas element:
    /// recompute hover via hit-test. Pure-ish (mutates view state only); split
    /// out so it can be driven without a window in tests.
    pub fn on_pointer_move(&mut self, screen_x: f32, screen_y: f32) {
        if let Some(ledger) = self.ledger() {
            let transform = self.interaction.transform();
            self.interaction.hover = hit_test(&ledger, &transform, Vec2::new(screen_x, screen_y));
        }
    }

    /// Handle a left mouse-down at a screen position: focus the component under
    /// the pointer (or clear focus on empty canvas), and mirror that selection
    /// into the authoritative ledger so the next agent turn sees it (OCEAN-186).
    ///
    /// The view-local `interaction.focus` drives the focus outline; the ledger
    /// `selection` is what `compact_context()` feeds the model. Before this they
    /// diverged — a click updated only the view, so the agent saw an empty
    /// selection. We now apply a `Select` patch through the installed
    /// [`LedgerSink`] (reusing the existing `SurfacePatch::Select` apply path, so
    /// the ledger stays the source of truth and its revision bumps consistently).
    /// A click on empty canvas clears the selection (`Select { ids: [] }`).
    pub fn on_left_down(&mut self, screen_x: f32, screen_y: f32) {
        if let Some(ledger) = self.ledger() {
            let transform = self.interaction.transform();
            let hit = hit_test(&ledger, &transform, Vec2::new(screen_x, screen_y));
            self.interaction.focus = hit.clone();

            if let Some(sink) = &self.sink {
                let ids = hit.into_iter().collect::<Vec<_>>();
                sink(SurfacePatch::Select { ids }, ActorRef::human(None));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::{
        ActorRef, CanvasComponentPatch, CanvasEdgePatch, CanvasMode, Endpoint, SurfacePatch,
    };
    use serde_json::json;

    fn ledger() -> CanvasLedger {
        CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform)
    }

    fn upsert(id: &str, kind: &str, rect: Rect, content: Value) -> SurfacePatch {
        SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new(id),
                kind: kind.to_string(),
                rect: Some(rect),
                z_index: None,
                content,
                metadata: Value::Null,
            },
        }
    }

    // ---- per-kind styling --------------------------------------------------

    #[test]
    fn every_component_kind_has_a_style_and_does_not_panic() {
        // Constructing a ledger with one of each structural kind and resolving
        // the renderer's geometry/style helpers for each must not panic. This is
        // the headless stand-in for "render layer reads every ComponentKind".
        let kinds = [
            ("card", ComponentKind::Card),
            ("text_block", ComponentKind::TextBlock),
            ("frame", ComponentKind::Frame),
            ("node", ComponentKind::Node),
            ("port", ComponentKind::Port),
            ("edge_label", ComponentKind::EdgeLabel),
            ("lane", ComponentKind::Lane),
            ("table", ComponentKind::Table),
            ("media_slot", ComponentKind::MediaSlot),
            ("stat", ComponentKind::Stat),
        ];

        let mut l = ledger();
        for (i, (kind_str, _)) in kinds.iter().enumerate() {
            l.apply_patch(
                upsert(
                    &format!("c{i}"),
                    kind_str,
                    Rect::new(i as f32 * 50.0, 0.0, 40.0, 40.0),
                    json!({ "title": kind_str }),
                ),
                ActorRef::system(),
                0,
            );
        }
        assert_eq!(l.components.len(), kinds.len());

        // Exercise the window-free element-builder inputs for each component:
        // style, outline, summary, title, screen rect. None may panic.
        let transform = ViewportTransform::new(l.viewport);
        for component in l.components.values() {
            let style = style_for_kind(component.kind);
            assert!(style.border_width > 0.0);
            let _ = transform.canvas_rect_to_screen(component.rect);
            let _ = component_title(component);
            let _ = component_summary(component);
            let interaction = CanvasInteraction::default();
            let _ = interaction.outline_for(&component.id, &l);
        }

        // The expected ComponentKind mapping holds for each.
        for ((_, expected), component) in kinds.iter().zip(l.components.values()) {
            assert_eq!(component.kind, *expected);
        }
    }

    #[test]
    fn container_and_node_kinds_get_distinct_styles() {
        let frame = style_for_kind(ComponentKind::Frame);
        let node = style_for_kind(ComponentKind::Node);
        assert_ne!(frame.fill, node.fill, "frame and node should look different");
        assert!(node.border_width >= frame.border_width);
    }

    // ---- summaries / titles ------------------------------------------------

    #[test]
    fn summary_prefers_title_then_body_then_template() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 10.0, 10.0), json!({ "title": "Hi" })), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "card", Rect::new(0.0, 0.0, 10.0, 10.0), json!({ "body": "line one\nline two" })), ActorRef::system(), 0);
        l.apply_patch(upsert("c", "brief_card", Rect::new(0.0, 0.0, 10.0, 10.0), Value::Null), ActorRef::system(), 0);

        assert_eq!(component_summary(l.component(&ComponentId::new("a")).unwrap()), "Hi");
        assert_eq!(component_summary(l.component(&ComponentId::new("b")).unwrap()), "line one");
        assert_eq!(component_summary(l.component(&ComponentId::new("c")).unwrap()), "brief_card");
    }

    #[test]
    fn title_falls_back_to_id_when_no_title_content() {
        let mut l = ledger();
        l.apply_patch(upsert("the-id", "card", Rect::new(0.0, 0.0, 10.0, 10.0), Value::Null), ActorRef::system(), 0);
        assert_eq!(component_title(l.component(&ComponentId::new("the-id")).unwrap()), "the-id");
    }

    // ---- geometry helpers --------------------------------------------------

    #[test]
    fn rect_center_is_the_midpoint() {
        assert_eq!(rect_center(&Rect::new(0.0, 0.0, 100.0, 50.0)), Vec2::new(50.0, 25.0));
    }

    #[test]
    fn edge_endpoints_pick_the_nearest_anchors() {
        // `from` is to the left of `to`; the closest anchors are from.right and to.left.
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 0.0, 100.0, 100.0);
        let (a, b) = edge_endpoints(&from, &to);
        assert_eq!(a, Vec2::new(100.0, 50.0), "from.right midpoint");
        assert_eq!(b, Vec2::new(300.0, 50.0), "to.left midpoint");
    }

    #[test]
    fn edge_endpoints_handle_vertical_stacking() {
        let top = Rect::new(0.0, 0.0, 100.0, 100.0);
        let bottom = Rect::new(0.0, 300.0, 100.0, 100.0);
        let (a, b) = edge_endpoints(&top, &bottom);
        assert_eq!(a, Vec2::new(50.0, 100.0), "top.bottom midpoint");
        assert_eq!(b, Vec2::new(50.0, 300.0), "bottom.top midpoint");
    }

    #[test]
    fn grid_offsets_are_aligned_and_cover_the_span() {
        let pan = 10.0;
        let span = 100.0;
        let grid = 24.0;
        let lines = grid_line_offsets(pan, span, grid);
        // First line is the grid-aligned position at or before pan (0).
        assert_eq!(lines.first(), Some(&0.0));
        // The last line is the final grid stop within the visible span, and the
        // next stop would fall past the far edge — i.e. the span is fully covered.
        let last = *lines.last().unwrap();
        assert!(last <= pan + span, "last line {last} should be within the span");
        assert!(last + grid > pan + span, "next grid line should exceed the far edge");
        // All multiples of the grid.
        assert!(lines.iter().all(|x| (x % grid).abs() < 1e-3));
    }

    #[test]
    fn grid_offsets_empty_for_degenerate_input() {
        assert!(grid_line_offsets(0.0, 0.0, 24.0).is_empty());
        assert!(grid_line_offsets(0.0, 100.0, 0.0).is_empty());
    }

    // ---- outline precedence ------------------------------------------------

    #[test]
    fn outline_precedence_is_active_write_over_focus_over_select_over_hover() {
        assert_eq!(OutlineState::resolve(true, true, true, true), OutlineState::ActiveWrite);
        assert_eq!(OutlineState::resolve(false, true, true, true), OutlineState::Focused);
        assert_eq!(OutlineState::resolve(false, false, true, true), OutlineState::Selected);
        assert_eq!(OutlineState::resolve(false, false, false, true), OutlineState::Hover);
        assert_eq!(OutlineState::resolve(false, false, false, false), OutlineState::None);
    }

    #[test]
    fn none_outline_has_no_color_others_do() {
        assert!(OutlineState::None.color().is_none());
        for s in [OutlineState::Hover, OutlineState::Selected, OutlineState::Focused, OutlineState::ActiveWrite] {
            assert!(s.color().is_some(), "{s:?} should have an outline color");
        }
    }

    #[test]
    fn outline_for_reads_ledger_selection() {
        let mut l = ledger();
        l.apply_patch(upsert("sel", "card", Rect::new(0.0, 0.0, 10.0, 10.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(SurfacePatch::Select { ids: vec![ComponentId::new("sel")] }, ActorRef::system(), 1);
        let interaction = CanvasInteraction::default();
        assert_eq!(
            interaction.outline_for(&ComponentId::new("sel"), &l),
            OutlineState::Selected
        );
    }

    // ---- interaction / hit-test integration --------------------------------

    #[test]
    fn pan_and_zoom_mutate_the_viewport() {
        let mut i = CanvasInteraction::default();
        i.zoom_by(2.0);
        assert_eq!(i.viewport.zoom, 2.0);
        // A 20px screen pan at 2x zoom moves the canvas pan by 10 units.
        i.pan_by_screen(20.0, 0.0);
        assert!((i.viewport.x - (-10.0)).abs() < 1e-3, "pan was {}", i.viewport.x);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut i = CanvasInteraction::default();
        for _ in 0..20 {
            i.zoom_by(2.0);
        }
        assert!(i.viewport.zoom <= 4.0);
        for _ in 0..20 {
            i.zoom_by(0.5);
        }
        assert!(i.viewport.zoom >= 0.2);
    }

    #[test]
    fn pointer_move_sets_hover_via_hit_test() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let mut view = OceanCanvasView::from_ledger(l);

        view.on_pointer_move(50.0, 50.0);
        assert_eq!(view.interaction.hover, Some(ComponentId::new("a")));

        view.on_pointer_move(500.0, 500.0);
        assert_eq!(view.interaction.hover, None);
    }

    #[test]
    fn left_down_focuses_component_under_pointer() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let mut view = OceanCanvasView::from_ledger(l);

        view.on_left_down(10.0, 10.0);
        assert_eq!(view.interaction.focus, Some(ComponentId::new("a")));

        view.on_left_down(400.0, 400.0);
        assert_eq!(view.interaction.focus, None, "click on empty canvas clears focus");
    }

    /// Build a view wired the way the shell wires it: a shared `Arc<Mutex<…>>`
    /// ledger cell read by the source and written by the sink (which applies a
    /// patch via `apply_patch`). Returns the cell so the test can inspect the
    /// authoritative ledger after interactions.
    fn view_over_shared_cell(
        ledger: CanvasLedger,
    ) -> (
        OceanCanvasView,
        std::sync::Arc<std::sync::Mutex<Option<CanvasLedger>>>,
    ) {
        let cell = std::sync::Arc::new(std::sync::Mutex::new(Some(ledger)));
        let read = std::sync::Arc::clone(&cell);
        let source: LedgerSource = std::sync::Arc::new(move || read.lock().ok().and_then(|g| g.clone()));
        let write = std::sync::Arc::clone(&cell);
        let sink: LedgerSink = std::sync::Arc::new(move |patch, actor| {
            if let Ok(mut g) = write.lock() {
                if let Some(l) = g.as_mut() {
                    l.apply_patch(patch, actor, 0);
                }
            }
        });
        let mut view = OceanCanvasView::new(source);
        view.set_sink(sink);
        (view, cell)
    }

    #[test]
    fn left_down_mirrors_selection_into_the_ledger_for_the_next_turn() {
        // OCEAN-186 Bug 1: a user click must reach the ledger selection so the
        // next agent turn's compact_context sees it — not just the view-local
        // focus. A click hitting a component selects it; an empty click clears.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        view.on_left_down(10.0, 10.0);
        assert_eq!(view.interaction.focus, Some(ComponentId::new("a")));
        {
            let guard = cell.lock().unwrap();
            let ledger = guard.as_ref().unwrap();
            assert_eq!(
                ledger.selection.component_ids,
                vec![ComponentId::new("a")],
                "click must mirror the selection into the ledger"
            );
            // And what the next turn actually receives reflects it.
            assert_eq!(
                ledger.compact_context().selection,
                vec![ComponentId::new("a")],
                "compact_context (the turn-injection path) must carry the selection"
            );
        }

        // A click on empty canvas clears the selection in the ledger too.
        view.on_left_down(400.0, 400.0);
        assert_eq!(view.interaction.focus, None);
        {
            let guard = cell.lock().unwrap();
            let ledger = guard.as_ref().unwrap();
            assert!(
                ledger.selection.component_ids.is_empty(),
                "empty-canvas click must clear the ledger selection"
            );
            assert!(ledger.compact_context().selection.is_empty());
        }
    }

    #[test]
    fn agent_set_viewport_reaches_the_renderer_viewport() {
        // OCEAN-186 Bug 2: an agent-applied SetViewport updates ledger.viewport;
        // the renderer reads interaction.viewport. Syncing the agent viewport into
        // the interaction is what actually moves the camera. This asserts the
        // sync semantics the shell performs on patch-apply: after applying
        // SetViewport, copying ledger.viewport into interaction.viewport makes the
        // renderer read the agent-requested viewport.
        let mut l = ledger();
        assert_eq!(l.viewport, Viewport::default());
        let target = Viewport { x: 120.0, y: 80.0, zoom: 2.5 };
        l.apply_patch(SurfacePatch::SetViewport { viewport: target }, ActorRef::agent(Some("sage".into())), 0);
        assert_eq!(l.viewport, target, "SetViewport updates the ledger viewport");

        let mut view = OceanCanvasView::from_ledger(l.clone());
        // Before the sync the view still shows the default camera — the bug.
        assert_eq!(view.interaction().viewport, Viewport::default());

        // The shell adopts the ledger viewport into the view on patch-apply.
        view.interaction_mut().viewport = l.viewport;

        // The renderer reads interaction.viewport (via transform()); it now
        // reflects the agent's SetViewport.
        assert_eq!(view.interaction().viewport, target);
        assert_eq!(view.interaction().transform().zoom(), 2.5);
    }

    #[test]
    fn view_renders_an_edge_between_connected_components_geometry() {
        // Build a ledger with an edge, then confirm the renderer's edge geometry
        // resolves to the expected endpoints (the element itself needs a window;
        // its inputs do not).
        let mut l = ledger();
        l.apply_patch(upsert("a", "node", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "node", Rect::new(300.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(
            SurfacePatch::Connect {
                edge: CanvasEdgePatch {
                    id: EdgeId::new("e1"),
                    from: Endpoint { component_id: ComponentId::new("a"), port: None },
                    to: Endpoint { component_id: ComponentId::new("b"), port: None },
                    kind: Some("flow".into()),
                    label: None,
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            0,
        );

        let edge = l.edges.values().next().unwrap();
        let from = &l.components.get(&edge.from.component_id).unwrap().rect;
        let to = &l.components.get(&edge.to.component_id).unwrap().rect;
        let (a, b) = edge_endpoints(from, to);
        assert_eq!(a, Vec2::new(100.0, 50.0));
        assert_eq!(b, Vec2::new(300.0, 50.0));
    }

    // ---- template content (Slice 8) ----------------------------------------

    use super::super::templates::{NodeStatus, TemplateContent};

    #[test]
    fn templated_component_resolves_drawable_content_primitives_do_not() {
        let mut l = ledger();
        l.apply_patch(
            upsert("brief-1", "brief_card", Rect::new(0.0, 0.0, 320.0, 220.0), json!({ "title": "Brief", "body": "Body" })),
            ActorRef::system(),
            0,
        );
        l.apply_patch(
            upsert("card-1", "card", Rect::new(0.0, 0.0, 100.0, 100.0), json!({ "title": "Plain" })),
            ActorRef::system(),
            0,
        );

        let brief = l.component(&ComponentId::new("brief-1")).unwrap();
        let content = template_content_for(brief).expect("brief_card resolves template content");
        assert!(matches!(content, TemplateContent::Brief { .. }));

        let plain = l.component(&ComponentId::new("card-1")).unwrap();
        assert!(
            template_content_for(plain).is_none(),
            "a plain card has no template content and uses the generic box"
        );
    }

    #[test]
    fn each_template_resolves_its_matching_content_variant() {
        let cases: &[(&str, fn(&TemplateContent) -> bool)] = &[
            ("brief_card", |c| matches!(c, TemplateContent::Brief { .. })),
            ("workflow_node", |c| matches!(c, TemplateContent::WorkflowNode { .. })),
            ("kanban_column", |c| matches!(c, TemplateContent::KanbanColumn { .. })),
            ("storyboard_frame", |c| matches!(c, TemplateContent::StoryboardFrame { .. })),
            ("stat_tile", |c| matches!(c, TemplateContent::Stat { .. })),
            ("longhouse_proposal", |c| matches!(c, TemplateContent::LonghouseProposal { .. })),
        ];
        let mut l = ledger();
        for (i, (kind, _)) in cases.iter().enumerate() {
            l.apply_patch(
                upsert(&format!("t{i}"), kind, Rect::new(0.0, 0.0, 100.0, 100.0), json!({ "title": kind })),
                ActorRef::system(),
                0,
            );
        }
        for (i, (kind, matches_variant)) in cases.iter().enumerate() {
            let c = l.component(&ComponentId::new(format!("t{i}"))).unwrap();
            let content = template_content_for(c).unwrap_or_else(|| panic!("{kind} resolves"));
            assert!(matches_variant(&content), "{kind} resolved the wrong variant");
        }
    }

    #[test]
    fn status_color_is_distinct_per_status() {
        let ok = status_color(NodeStatus::Ok);
        let err = status_color(NodeStatus::Error);
        let running = status_color(NodeStatus::Running);
        assert_ne!(ok, err);
        assert_ne!(ok, running);
        assert_ne!(err, running);
    }

    #[test]
    fn from_ledger_source_yields_the_snapshot() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 10.0, 10.0), Value::Null), ActorRef::system(), 0);
        let view = OceanCanvasView::from_ledger(l);
        let snap = view.ledger().expect("source yields ledger");
        assert!(snap.component(&ComponentId::new("a")).is_some());
    }
}
