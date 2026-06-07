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
    canvas, div, point, px, App, Bounds, Context, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathBuilder, Pixels, Render, ScrollWheelEvent, Styled, Window,
};
use serde_json::Value;

use super::hit_test::{hit_test, paint_order, Vec2, ViewportTransform};
use super::ledger::{CanvasComponent, CanvasEdge, CanvasLedger, ComponentKind, EdgeKind, EdgeRoute};
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

/// The drawable style for an [`EdgeKind`]: stroke color and thickness in canvas
/// units. Distinct kinds read differently so a dependency link, a workflow flow
/// arrow, and a loose reference are visually separable. `Other(_)` (any
/// agent-supplied kind outside the known set) falls back to the muted reference
/// style. Pure data — `Hsla` carries no window dependency.
pub fn edge_style_for_kind(kind: &EdgeKind) -> (Hsla, f32) {
    match kind {
        // Flow (workflow arrows): the accent, drawn boldest — it's the spine of
        // a pipeline and should read first.
        EdgeKind::Flow => (theme::accent(), 2.0),
        // Dependency: a firm but secondary link.
        EdgeKind::Dependency => (theme::accent_dark(), 1.5),
        // Reference / anything unknown: a quiet hairline.
        EdgeKind::Reference | EdgeKind::Other(_) => (theme::muted(), 1.0),
    }
}

/// The polyline an edge follows between two component rects, in **canvas** space,
/// honoring its [`EdgeRoute`]. Always returns at least the two endpoints; routes
/// that bend insert waypoints between them:
///
/// - [`EdgeRoute::Straight`] → `[from, to]` (a single segment).
/// - [`EdgeRoute::Orthogonal`] → `[from, elbow, to]` — an L-shaped right-angle
///   route whose elbow turns the corner at `(to.x, from.y)`.
/// - [`EdgeRoute::Bezier`] → `[from, mid, to]` — the endpoints plus the sampled
///   curve midpoint, enough for the (line-segment) renderer to read a bowed path
///   and to place the label at the true visual middle.
///
/// The renderer draws this as a connected polyline; the geometry is the testable
/// contract `render_edge` consumes.
pub fn edge_route(from: &Rect, to: &Rect, route: EdgeRoute) -> Vec<Vec2> {
    let (a, b) = edge_endpoints(from, to);
    match route {
        EdgeRoute::Straight => vec![a, b],
        EdgeRoute::Orthogonal => {
            let elbow = Vec2::new(b.x, a.y);
            vec![a, elbow, b]
        }
        EdgeRoute::Bezier => {
            let mid = Vec2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            vec![a, mid, b]
        }
    }
}

/// The exact ordered point sequence the renderer strokes for an edge — the
/// polyline handed to the path builder as `move_to(points[0])` then
/// `line_to(..)` through the rest. It is just [`edge_route`], named separately
/// because it is the **drawing-level** contract: the stroke must pass through
/// every one of these points, so the last point is guaranteed to be the target
/// endpoint.
///
/// This exists to lock the invariant Codex caught (OCEAN-192): the earlier
/// renderer collapsed each segment to a single-axis bar, so a diagonal segment's
/// drawn stroke stopped at `(to.x, from.y)` and never reached `(to.x, to.y)`.
/// Stroking the polyline directly (diagonals included) means the drawn path's
/// final point equals the routed endpoint by construction — which this helper
/// makes testable without a window.
pub fn edge_draw_path(from: &Rect, to: &Rect, route: EdgeRoute) -> Vec<Vec2> {
    edge_route(from, to, route)
}

/// The point at which to anchor an edge's label: the midpoint **along** the
/// routed polyline (by cumulative segment length), so the label sits on the
/// drawn path rather than on the straight-line average of the endpoints. Returns
/// `(0,0)` for an empty input (no points to place against).
pub fn edge_label_anchor(points: &[Vec2]) -> Vec2 {
    match points {
        [] => Vec2::new(0.0, 0.0),
        [only] => *only,
        _ => {
            // Total path length, then walk to the half-length point.
            let seg_len = |p: Vec2, q: Vec2| ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
            let total: f32 = points.windows(2).map(|w| seg_len(w[0], w[1])).sum();
            if total <= f32::EPSILON {
                return points[0];
            }
            let target = total / 2.0;
            let mut walked = 0.0;
            for w in points.windows(2) {
                let (p, q) = (w[0], w[1]);
                let len = seg_len(p, q);
                if walked + len >= target {
                    let t = if len > f32::EPSILON {
                        (target - walked) / len
                    } else {
                        0.0
                    };
                    return Vec2::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t);
                }
                walked += len;
            }
            *points.last().unwrap()
        }
    }
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
// Selection + drag decision logic (window-free, unit-tested)
// ===========================================================================

/// How a pointer-down should fold the hit component into the current selection.
/// This is the GPUI-free distillation of the keyboard modifiers on the mouse
/// event, so the selection transition can be unit-tested without a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Plain click: the hit becomes the *only* selection; an empty hit clears.
    Replace,
    /// Shift+click: ADD the hit to the selection (extend). Empty hit is a no-op
    /// on the set (you can't extend by nothing), preserving the current set.
    Extend,
    /// Ctrl/Cmd+click: TOGGLE the hit in/out of the selection. Empty hit leaves
    /// the set unchanged.
    Toggle,
}

impl SelectMode {
    /// Resolve the mode from the two modifier flags the mouse event carries.
    /// Shift takes precedence over the secondary (ctrl/cmd) modifier when both
    /// are held — extend is the more common intent and keeps the rule simple.
    pub fn from_modifiers(shift: bool, secondary: bool) -> Self {
        if shift {
            Self::Extend
        } else if secondary {
            Self::Toggle
        } else {
            Self::Replace
        }
    }
}

/// Compute the next selection set from the current one, the component under the
/// pointer (if any), and the [`SelectMode`]. Pure — this is the testable core of
/// `on_left_down`'s selection branch.
///
/// - **Replace**: `[hit]` if something was hit, else `[]` (empty-canvas clear).
/// - **Extend**: current set with `hit` appended if not already present; an
///   empty hit leaves the set untouched.
/// - **Toggle**: current set with `hit` removed if present, else appended; an
///   empty hit leaves the set untouched.
///
/// Order is preserved (append-at-end) so the most-recently-added id is last,
/// which the renderer can treat as the primary focus if it wants.
pub fn next_selection(
    current: &[ComponentId],
    hit: Option<&ComponentId>,
    mode: SelectMode,
) -> Vec<ComponentId> {
    match (mode, hit) {
        (SelectMode::Replace, Some(id)) => vec![id.clone()],
        (SelectMode::Replace, None) => Vec::new(),
        (SelectMode::Extend, Some(id)) => {
            let mut next = current.to_vec();
            if !next.iter().any(|c| c == id) {
                next.push(id.clone());
            }
            next
        }
        (SelectMode::Toggle, Some(id)) => {
            if current.iter().any(|c| c == id) {
                current.iter().filter(|c| *c != id).cloned().collect()
            } else {
                let mut next = current.to_vec();
                next.push(id.clone());
                next
            }
        }
        // A modifier-click on empty canvas is a no-op on the set.
        (SelectMode::Extend | SelectMode::Toggle, None) => current.to_vec(),
    }
}

/// The selection to apply on a *press* (mouse-down), which differs from a raw
/// [`next_selection`] in one case: a **plain press on a component already in the
/// selection preserves the whole set** (rather than collapsing to just that
/// component). This is what lets the user grab one card of a multi-selection and
/// drag the entire group — the canonical canvas-editor behavior (Figma/tldraw).
/// A plain release without a drag could later collapse to just the hit, but this
/// slice keeps the press behavior simple and group-drag-friendly.
///
/// All other cases defer to [`next_selection`]: plain press on a *non-member*
/// replaces; shift extends; ctrl/cmd toggles; empty-canvas plain press clears.
pub fn press_selection(
    current: &[ComponentId],
    hit: Option<&ComponentId>,
    mode: SelectMode,
) -> Vec<ComponentId> {
    if mode == SelectMode::Replace {
        if let Some(id) = hit {
            if current.iter().any(|c| c == id) {
                // Member of a multi-selection: preserve the set so it can drag.
                return current.to_vec();
            }
        }
    }
    next_selection(current, hit, mode)
}

/// Pixels (in screen space) the pointer must travel from the mouse-down origin
/// before the gesture is treated as a drag rather than a click. Below this the
/// gesture is a pure selection and emits NO `MoveComponent`.
pub const DRAG_THRESHOLD_PX: f32 = 3.0;

/// True if a screen-space movement from `start` to `current` has crossed the
/// [`DRAG_THRESHOLD_PX`] (so the gesture is a drag, not a click). Uses the
/// squared distance to avoid a `sqrt`.
pub fn drag_threshold_crossed(start: Vec2, current: Vec2) -> bool {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    dx * dx + dy * dy > DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX
}

/// Convert a **screen-space** drag delta into a **canvas-space** delta for a
/// given zoom. Canvas units are screen pixels divided by the zoom factor (a 20px
/// screen move at 2× zoom is a 10-unit canvas move). Mirrors the pan math in
/// [`CanvasInteraction::pan_by_screen`].
pub fn screen_delta_to_canvas(screen_dx: f32, screen_dy: f32, zoom: f32) -> Vec2 {
    let z = if zoom > 0.0001 { zoom } else { 0.0001 };
    Vec2::new(screen_dx / z, screen_dy / z)
}

/// The new top-left position of `rect` after a canvas-space drag delta — the
/// target a `MoveComponent` patch carries. `MoveComponent` sets an absolute
/// position, so this is `rect.x/y + delta`.
pub fn dragged_position(rect: &Rect, canvas_delta: Vec2) -> Vec2 {
    Vec2::new(rect.x + canvas_delta.x, rect.y + canvas_delta.y)
}

/// The set of components a drag should move, given the hit component and the
/// current selection: if the hit is part of the selection, the WHOLE selection
/// moves together; otherwise just the hit moves (and it becomes the selection).
/// Returns the ids to move in a stable order. An empty hit yields an empty set
/// (drags only start over a component).
pub fn drag_targets(hit: Option<&ComponentId>, selection: &[ComponentId]) -> Vec<ComponentId> {
    match hit {
        None => Vec::new(),
        Some(id) => {
            if selection.iter().any(|c| c == id) {
                selection.to_vec()
            } else {
                vec![id.clone()]
            }
        }
    }
}

/// Whether a press should arm a drag, given the hit component and the
/// **post-press** selection (the set [`press_selection`] resolved). A drag is
/// only armed when the press landed on a component that **remains selected**
/// after the press.
///
/// This guards the deselect-then-drift bug (OCEAN-194, Codex P2 on #48): a
/// Ctrl/Cmd-click that toggles the hit *off* removes it from the post-press
/// selection. Without this guard `drag_targets` would fall back to "just the
/// hit" and a >`DRAG_THRESHOLD_PX` drift would emit a `MoveComponent` for the
/// component the user just deselected. A toggle-off press is a pure deselect —
/// no drag.
///
/// When this returns `true` the hit is guaranteed to be in `post_press_selection`,
/// so [`drag_targets`] resolves to the correct set (the whole selection,
/// including the hit).
pub fn should_arm_drag(hit: Option<&ComponentId>, post_press_selection: &[ComponentId]) -> bool {
    match hit {
        None => false,
        Some(id) => post_press_selection.iter().any(|c| c == id),
    }
}

// ===========================================================================
// Keyboard navigation + fit-to-content (window-free, unit-tested)
// ===========================================================================

/// How far a single arrow-key press nudges the selection, in **canvas** units.
/// A bare arrow is a fine 1-unit nudge; holding Shift jumps a coarse 10 units.
pub const NUDGE_STEP: f32 = 1.0;
/// The coarse nudge step (Shift held). Ten canvas units per press.
pub const NUDGE_STEP_COARSE: f32 = 10.0;

/// Padding, in **canvas** units, left around the component bounding box when an
/// agent `Focus(Canvas)` fits the camera to content. Keeps cards off the edge.
pub const FIT_PADDING: f32 = 48.0;

/// The decoded intent of a key press on the canvas, resolved from the raw key
/// name + modifiers. This is the GPUI-free distillation of a `KeyDownEvent` so
/// the key→action transition can be unit-tested without a window. Anything the
/// canvas does not bind resolves to `None` (the handler ignores it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasKeyAction {
    /// Nudge the current selection by a canvas-space delta (arrow keys).
    Nudge(Vec2),
    /// Clear the selection (Escape).
    ClearSelection,
    /// Delete the current selection (Delete/Backspace).
    DeleteSelection,
    /// Cycle the primary focus/selection to the next (`false`) or previous
    /// (`true` = Shift+Tab) component in stable order.
    CycleFocus { backward: bool },
}

/// Resolve a raw key name + the Shift modifier into a [`CanvasKeyAction`], or
/// `None` if the canvas does not bind the key. Pure — the testable core of
/// [`OceanCanvasView::on_key_down`].
///
/// - Arrow keys → [`CanvasKeyAction::Nudge`] of ±[`NUDGE_STEP`] on one axis
///   ([`NUDGE_STEP_COARSE`] when `shift`).
/// - `escape` → [`CanvasKeyAction::ClearSelection`].
/// - `delete` / `backspace` → [`CanvasKeyAction::DeleteSelection`].
/// - `tab` → [`CanvasKeyAction::CycleFocus`] (`backward` = `shift`).
pub fn canvas_key_action(key: &str, shift: bool) -> Option<CanvasKeyAction> {
    let step = if shift { NUDGE_STEP_COARSE } else { NUDGE_STEP };
    match key {
        "up" => Some(CanvasKeyAction::Nudge(Vec2::new(0.0, -step))),
        "down" => Some(CanvasKeyAction::Nudge(Vec2::new(0.0, step))),
        "left" => Some(CanvasKeyAction::Nudge(Vec2::new(-step, 0.0))),
        "right" => Some(CanvasKeyAction::Nudge(Vec2::new(step, 0.0))),
        "escape" => Some(CanvasKeyAction::ClearSelection),
        "delete" | "backspace" => Some(CanvasKeyAction::DeleteSelection),
        "tab" => Some(CanvasKeyAction::CycleFocus { backward: shift }),
        _ => None,
    }
}

/// The id that primary focus/selection should advance to when Tab (or Shift+Tab)
/// cycles through `order` (the components in a stable order, e.g. insertion or
/// paint order). `current` is the id holding primary focus right now, if any.
///
/// - Empty `order` → `None` (nothing to focus).
/// - No current focus → the first (forward) or last (backward) id.
/// - Current focus present → the next (forward) / previous (backward) id,
///   wrapping around the ends. A `current` not found in `order` falls back to
///   the first/last as if there were no focus.
pub fn cycle_focus_target(
    order: &[ComponentId],
    current: Option<&ComponentId>,
    backward: bool,
) -> Option<ComponentId> {
    if order.is_empty() {
        return None;
    }
    let len = order.len();
    let idx = current.and_then(|c| order.iter().position(|id| id == c));
    let next = match idx {
        None => {
            if backward {
                len - 1
            } else {
                0
            }
        }
        Some(i) => {
            if backward {
                (i + len - 1) % len
            } else {
                (i + 1) % len
            }
        }
    };
    Some(order[next].clone())
}

/// The axis-aligned bounding box of a set of component rects, in canvas space, or
/// `None` for an empty set. Used by [`fit_viewport`] to frame all content.
pub fn components_bbox(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x + first.w;
    let mut max_y = first.y + first.h;
    for r in &rects[1..] {
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.x + r.w);
        max_y = max_y.max(r.y + r.h);
    }
    Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

/// Compute the viewport (pan + zoom) that frames every component's bounding box
/// into a view of `view_size` screen pixels, with `padding` canvas units of slack
/// around the content. This is the **pure, headlessly testable** core of an agent
/// `Focus(Canvas)` (fit-to-content) — `ledger.rs` documents the fit as "a viewport
/// concern handled by the renderer", and this is that renderer math.
///
/// Returns `None` when there is nothing to frame (`component_rects` empty) or the
/// view has no area yet (`view_size` non-positive) — the caller no-ops and leaves
/// the camera where it is.
///
/// The mapping mirrors [`ViewportTransform`]: `screen = (canvas - pan) * zoom`.
/// To fit a padded bbox of size `(bw, bh)` into `(vw, vh)` we pick
/// `zoom = min(vw / bw, vh / bh)` (clamped to the same `[0.2, 4.0]` range the
/// interactive zoom uses), then set `pan` so the bbox center maps to the view
/// center: `pan = bbox_center - view_center / zoom`.
pub fn fit_viewport(component_rects: &[Rect], view_size: Vec2, padding: f32) -> Option<Viewport> {
    if view_size.x <= 0.0 || view_size.y <= 0.0 {
        return None;
    }
    let bbox = components_bbox(component_rects)?;
    let pad = padding.max(0.0);
    // Padded bbox dimensions; guard against a zero-area bbox (e.g. a single
    // degenerate rect) so the zoom fit never divides by zero.
    let bw = (bbox.w + pad * 2.0).max(f32::EPSILON);
    let bh = (bbox.h + pad * 2.0).max(f32::EPSILON);

    let zoom = (view_size.x / bw).min(view_size.y / bh).clamp(0.2, 4.0);

    // Center the (unpadded) bbox center in the view. screen_center = (center -
    // pan) * zoom = view_size/2  =>  pan = center - view_center / zoom.
    let center_x = bbox.x + bbox.w / 2.0;
    let center_y = bbox.y + bbox.h / 2.0;
    let pan_x = center_x - (view_size.x / 2.0) / zoom;
    let pan_y = center_y - (view_size.y / 2.0) / zoom;

    Some(Viewport {
        x: pan_x,
        y: pan_y,
        zoom,
    })
}

// ===========================================================================
// OceanCanvasView (GPUI view)
// ===========================================================================

/// An in-flight drag of one or more components. While a drag is active the
/// dragged components render at `base rect + offset` (a *preview*); the ledger
/// is only mutated once, on mouse-up, via one `MoveComponent` per moved
/// component. Holding the base rects here means the preview and the final patch
/// both derive from the same anchor, so a click that never crosses the threshold
/// leaves the components exactly where they were.
#[derive(Debug, Clone, PartialEq)]
pub struct DragState {
    /// Screen-space point where the mouse went down (drag origin).
    pub start_screen: Vec2,
    /// Latest screen-space pointer position seen during the drag.
    pub last_screen: Vec2,
    /// Components being moved, paired with their rect at drag start (the anchor
    /// the preview offset and the final `MoveComponent` are computed from).
    pub moving: Vec<(ComponentId, Rect)>,
    /// Whether the pointer has crossed [`DRAG_THRESHOLD_PX`]. Until it does the
    /// gesture is still a click and no preview offset / `MoveComponent` applies.
    pub crossed: bool,
}

impl DragState {
    /// Accumulated canvas-space offset of the drag at the current zoom: the
    /// screen delta from start→last, divided by zoom. `Vec2::ZERO`-ish until the
    /// pointer moves. Returns `(0,0)` before the threshold is crossed so the
    /// preview doesn't twitch on a click.
    pub fn canvas_offset(&self, zoom: f32) -> Vec2 {
        if !self.crossed {
            return Vec2::new(0.0, 0.0);
        }
        screen_delta_to_canvas(
            self.last_screen.x - self.start_screen.x,
            self.last_screen.y - self.start_screen.y,
            zoom,
        )
    }
}

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
    /// Component with explicit focus, if any (the "primary" of a multi-select —
    /// the last one added). Drives the focus outline.
    pub focus: Option<ComponentId>,
    /// The full multi-selection set, mirrored from / into the ledger selection on
    /// each pointer-down. The renderer outlines every id in here; `focus` is the
    /// primary within it. Kept view-local so the highlight is responsive even
    /// before the ledger write-back lands.
    pub selection: Vec<ComponentId>,
    /// An in-flight drag-to-move, if the user is dragging components this gesture.
    pub drag: Option<DragState>,
    /// Component an agent is actively writing to this turn, if any (driven by the
    /// shell from patch events — Slice 6).
    pub active_write: Option<ComponentId>,
    /// The size (screen pixels) of the canvas viewport element as of the last
    /// paint, if it has been painted at least once. Captured each frame so an
    /// agent `Focus(Canvas)` (fit-to-content) can frame the component bbox into
    /// the *actual* view bounds rather than a guess. `None` before first paint.
    pub last_view_size: Option<Vec2>,
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

    /// Adopt a fit-to-content viewport that frames `component_rects` into the
    /// last-painted view bounds (an agent `Focus(Canvas)`). No-ops — leaving the
    /// camera untouched — when there is nothing to frame or the view has not been
    /// painted yet (`last_view_size` is `None`). Returns the adopted viewport on
    /// success so callers can observe/assert it. The actual bbox→camera math is
    /// the pure [`fit_viewport`] helper.
    pub fn fit_to_content(&mut self, component_rects: &[Rect], padding: f32) -> Option<Viewport> {
        let view_size = self.last_view_size?;
        let fitted = fit_viewport(component_rects, view_size, padding)?;
        self.viewport = fitted;
        Some(fitted)
    }

    /// Resolve the outline state for one component id under the current view +
    /// ledger selection.
    ///
    /// A component is "selected" if it is in the ledger selection OR the
    /// view-local multi-selection set — the latter keeps the highlight
    /// responsive before the ledger write-back lands and during a drag. `focus`
    /// (the primary of a multi-select) still wins over plain selection so the
    /// last-clicked component reads strongest.
    pub fn outline_for(&self, id: &ComponentId, ledger: &CanvasLedger) -> OutlineState {
        let selected = ledger.selection.component_ids.iter().any(|c| c == id)
            || self.selection.iter().any(|c| c == id);
        OutlineState::resolve(
            self.active_write.as_ref() == Some(id),
            self.focus.as_ref() == Some(id),
            selected,
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
    /// Keyboard focus handle so the canvas can receive `on_key_down` events
    /// (arrows nudge, Escape clears, Delete removes, Tab cycles). Created lazily
    /// on the first render (which has a `Context`), since the view is also built
    /// in headless previews/tests that have no GPUI context. `None` until the
    /// first paint, and in tests that drive the key handler directly.
    focus_handle: Option<FocusHandle>,
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
            focus_handle: None,
        }
    }

    /// Lazily obtain (creating on first use) the keyboard [`FocusHandle`]. Called
    /// from `render`, which has a `Context`; the handle is what `track_focus` +
    /// `on_key_down` bind so the canvas can take keyboard focus on click.
    fn focus_handle(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.focus_handle
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
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
        // Apply the live drag-preview offset: if this component is being dragged
        // this gesture, draw it at base + the accumulated canvas offset. The
        // ledger rect is untouched until mouse-up (the preview is purely visual).
        let mut canvas_rect = component.rect;
        if let Some(drag) = self.interaction.drag.as_ref() {
            if drag.moving.iter().any(|(id, _)| id == &component.id) {
                let off = drag.canvas_offset(transform.zoom());
                canvas_rect.x += off.x;
                canvas_rect.y += off.y;
            }
        }
        let screen = transform.canvas_rect_to_screen(canvas_rect);
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

    /// Build the drawable element for one [`CanvasEdge`] between two components.
    ///
    /// The edge's routed polyline ([`edge_draw_path`], honoring [`EdgeRoute`]) is
    /// stroked as a single GPUI path via [`gpui::canvas`] + [`PathBuilder`] +
    /// `window.paint_path` — so a **diagonal** straight/bezier segment between
    /// non-axis-aligned components reaches its true endpoint instead of being
    /// collapsed to a horizontal/vertical bar (OCEAN-192, Codex P2). Orthogonal
    /// routes still render axis-aligned, because their polyline legs already are.
    ///
    /// Stroke color/width come from [`edge_style_for_kind`] so a flow arrow, a
    /// dependency link, and a loose reference read distinctly. If the edge carries
    /// a `label`, it is drawn at the polyline midpoint ([`edge_label_anchor`])
    /// using the same mono text idiom as component chips.
    fn render_edge(
        &self,
        edge: &CanvasEdge,
        from_rect: &Rect,
        to_rect: &Rect,
        transform: &ViewportTransform,
    ) -> impl IntoElement {
        let (color, stroke) = edge_style_for_kind(&edge.kind);
        let stroke_px = transform.scale(stroke).max(1.0);
        // Widget-relative screen points (origin = canvas widget top-left). The
        // paint callback shifts these by the element's absolute bounds origin.
        let screen_points: Vec<Vec2> = edge_draw_path(from_rect, to_rect, edge.route)
            .into_iter()
            .map(|p| transform.canvas_to_screen(p))
            .collect();

        let mut layer = div().absolute().top_0().left_0().right_0().bottom_0();

        // Stroke the polyline directly (diagonals included) in a canvas paint
        // pass. paint_path takes window-absolute coordinates, so add the element
        // bounds origin to each widget-relative point.
        layer = layer.child(
            canvas(
                move |_bounds, _window, _cx| {},
                move |bounds: Bounds<Pixels>, _state, window: &mut Window, _cx: &mut App| {
                    if screen_points.len() < 2 {
                        return;
                    }
                    let origin = bounds.origin;
                    let mut builder = PathBuilder::stroke(px(stroke_px));
                    let to_window = |p: &Vec2| point(px(p.x) + origin.x, px(p.y) + origin.y);
                    builder.move_to(to_window(&screen_points[0]));
                    for p in &screen_points[1..] {
                        builder.line_to(to_window(p));
                    }
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0(),
        );

        // Label at the polyline midpoint.
        if let Some(label) = edge.label.as_ref().filter(|s| !s.is_empty()) {
            let points = edge_draw_path(from_rect, to_rect, edge.route);
            let anchor = transform.canvas_to_screen(edge_label_anchor(&points));
            layer = layer.child(
                div()
                    .absolute()
                    // Nudge the label up off the line so the stroke doesn't bisect
                    // the text; left is anchored at the midpoint.
                    .left(px(anchor.x))
                    .top(px(anchor.y - transform.scale(12.0).max(8.0)))
                    .px(px(transform.scale(4.0).max(1.0)))
                    .bg(theme::background())
                    .font_family(theme::MONO_FONT)
                    .text_size(px(transform.scale(10.0).max(6.0)))
                    .text_color(color)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.clone()),
            );
        }

        layer
    }
}

impl Render for OceanCanvasView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let transform = self.interaction.transform();
        let ledger = self.ledger();
        let focus_handle = self.focus_handle(cx);

        // Root: clipped canvas surface with the dark background. `track_focus`
        // makes it a keyboard-focus target so `on_key_down` fires once the canvas
        // is focused (we focus it on mouse-down below).
        let mut root = div()
            .key_context("OceanCanvas")
            .track_focus(&focus_handle)
            .relative()
            .size_full()
            .bg(theme::background())
            .overflow_hidden();

        // Capture the painted size into the interaction state so an agent
        // Focus(Canvas) can fit content to the *actual* view bounds. A zero-paint
        // canvas() layer is the cheapest way to read this frame's bounds; it
        // writes the size back through the view's own entity handle.
        let size_entity = cx.entity();
        root = root.child(
            canvas(
                move |_bounds, _window, _cx| {},
                move |bounds: Bounds<Pixels>, _state, _window: &mut Window, cx: &mut App| {
                    let size = Vec2::new(bounds.size.width.into(), bounds.size.height.into());
                    size_entity.update(cx, |view, _cx| {
                        view.interaction.last_view_size = Some(size);
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0(),
        );

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
                    edge_layer =
                        edge_layer.child(self.render_edge(edge, &from.rect, &to.rect, &transform));
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
        // Hover updates the highlight; left-down selects (honoring shift/ctrl
        // modifiers for multi-select) and arms a drag; move with the button held
        // runs the live drag preview; up finalizes the move via the ledger.
        // Selection + move both mirror into the ledger through the LedgerSink so
        // the next agent turn sees them (OCEAN-186/OCEAN-194). Scroll pans.
        root.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _window, cx| {
            let dragging = ev.pressed_button == Some(MouseButton::Left)
                && view.interaction.drag.is_some();
            if dragging {
                view.on_drag_move(ev.position.x.into(), ev.position.y.into());
            } else {
                view.on_pointer_move(ev.position.x.into(), ev.position.y.into());
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, ev: &MouseDownEvent, window, cx| {
                // Give the canvas keyboard focus so the arrow/Escape/Delete/Tab
                // bindings fire while the user is working on it.
                window.focus(&focus_handle);
                let mode = SelectMode::from_modifiers(ev.modifiers.shift, ev.modifiers.secondary());
                view.on_left_down(ev.position.x.into(), ev.position.y.into(), mode);
                cx.notify();
            }),
        )
        .on_key_down(cx.listener(|view, ev: &KeyDownEvent, _window, cx| {
            let key = ev.keystroke.key.clone();
            if view.handle_key(&key, ev.keystroke.modifiers.shift) {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|view, ev: &MouseUpEvent, _window, cx| {
                view.on_left_up(ev.position.x.into(), ev.position.y.into());
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

    /// Handle a left mouse-down at a screen position with the resolved
    /// [`SelectMode`]: update the (multi-)selection, mirror it into the ledger so
    /// the next agent turn sees it (OCEAN-186), and arm a drag if the press
    /// landed on a component (OCEAN-194).
    ///
    /// Selection (`next_selection`): a plain click replaces the set with the hit
    /// (empty hit clears); shift extends; ctrl/cmd toggles. The view-local
    /// `interaction.selection` is updated immediately (responsive highlight) and
    /// the same set is applied through the [`LedgerSink`] as a `Select` patch so
    /// the ledger stays authoritative and its revision bumps consistently. The
    /// primary `focus` is the hit when there is one, else the last in the set.
    ///
    /// Drag arming: if the press hit a component, record a [`DragState`] whose
    /// `moving` set is the whole selection when the hit is part of it, else just
    /// the hit ([`drag_targets`]). The drag is only *previewed*/committed once the
    /// pointer crosses [`DRAG_THRESHOLD_PX`] — a press-without-move stays a click.
    pub fn on_left_down(&mut self, screen_x: f32, screen_y: f32, mode: SelectMode) {
        if let Some(ledger) = self.ledger() {
            let transform = self.interaction.transform();
            let hit = hit_test(&ledger, &transform, Vec2::new(screen_x, screen_y));

            // Selection transition (pure helper), seeded from the ledger's
            // authoritative set so multi-select survives across clicks. A plain
            // press on an already-selected member preserves the whole set
            // (`press_selection`) so the group can drag together.
            let current = ledger.selection.component_ids.clone();
            let next = press_selection(&current, hit.as_ref(), mode);

            self.interaction.selection = next.clone();
            // Primary focus: the just-hit component, else the most recent in set.
            self.interaction.focus = hit.clone().or_else(|| next.last().cloned());

            if let Some(sink) = &self.sink {
                sink(
                    SurfacePatch::Select { ids: next.clone() },
                    ActorRef::human(None),
                );
            }

            // Arm a drag over the resolved selection (so dragging one of a
            // multi-selection moves the whole set) — but ONLY if the hit remains
            // selected after the press (`should_arm_drag`). A Ctrl/Cmd-click that
            // toggled the hit *off* must not arm a drag, or a >threshold drift
            // would move the component the user just deselected (OCEAN-194 P2).
            // Capture each target's rect at press time as the anchor for the
            // preview + final MoveComponent.
            if should_arm_drag(hit.as_ref(), &next) {
                let moving = drag_targets(hit.as_ref(), &next)
                    .into_iter()
                    .filter_map(|id| ledger.components.get(&id).map(|c| (id, c.rect)))
                    .collect::<Vec<_>>();
                if !moving.is_empty() {
                    self.interaction.drag = Some(DragState {
                        start_screen: Vec2::new(screen_x, screen_y),
                        last_screen: Vec2::new(screen_x, screen_y),
                        moving,
                        crossed: false,
                    });
                } else {
                    self.interaction.drag = None;
                }
            } else {
                self.interaction.drag = None;
            }
        }
    }

    /// Handle pointer motion with the left button held during an armed drag:
    /// update the latest screen point and flip `crossed` once the movement
    /// exceeds [`DRAG_THRESHOLD_PX`]. The render reads `DragState::canvas_offset`
    /// to draw the moving components at base + offset — so this only updates
    /// state; no ledger write happens until mouse-up (keeps the drag smooth and
    /// avoids a patch per mouse-move).
    pub fn on_drag_move(&mut self, screen_x: f32, screen_y: f32) {
        if let Some(drag) = self.interaction.drag.as_mut() {
            let now = Vec2::new(screen_x, screen_y);
            drag.last_screen = now;
            if !drag.crossed && drag_threshold_crossed(drag.start_screen, now) {
                drag.crossed = true;
            }
        }
    }

    /// Handle a left mouse-up: finalize an in-flight drag, then clear drag state.
    ///
    /// If the drag crossed the threshold, apply one `MoveComponent` per moved
    /// component through the [`LedgerSink`], with the absolute target position
    /// computed from each component's press-time rect plus the accumulated
    /// canvas-space delta ([`dragged_position`]). The ledger becomes the source
    /// of truth post-drop; the live offset was only a preview. A sub-threshold
    /// release is a click — it emits NO `MoveComponent` (the selection already
    /// happened on mouse-down).
    pub fn on_left_up(&mut self, screen_x: f32, screen_y: f32) {
        let Some(drag) = self.interaction.drag.take() else {
            return;
        };
        // Use the final pointer position for the delta (not just the last move),
        // so a release that lands past the threshold still commits.
        let crossed = drag.crossed
            || drag_threshold_crossed(drag.start_screen, Vec2::new(screen_x, screen_y));
        if !crossed {
            return;
        }
        let zoom = self.interaction.transform().zoom();
        let delta = screen_delta_to_canvas(
            screen_x - drag.start_screen.x,
            screen_y - drag.start_screen.y,
            zoom,
        );
        if let Some(sink) = &self.sink {
            for (id, base_rect) in &drag.moving {
                let pos = dragged_position(base_rect, delta);
                sink(
                    SurfacePatch::MoveComponent {
                        component_id: id.clone(),
                        x: pos.x,
                        y: pos.y,
                    },
                    ActorRef::human(None),
                );
            }
        }
    }

    /// Handle a keyboard key by its raw name + Shift state, returning `true` if
    /// the canvas consumed it (so the caller can stop propagation). This is the
    /// window-free entry point the `on_key_down` listener drives; tests call it
    /// directly. The mapping is the pure [`canvas_key_action`]; the effect of each
    /// action flows through the [`LedgerSink`] so it reaches the ledger (and the
    /// next agent turn) exactly like a pointer interaction does.
    ///
    /// - **Arrows** nudge every selected component by the canvas-space step via one
    ///   `MoveComponent` each (new position = current rect + delta).
    /// - **Escape** clears the selection (`Select { ids: [] }`) and the view-local
    ///   selection/focus.
    /// - **Delete/Backspace** removes every selected component (`DeleteComponent`
    ///   each) and clears the view-local selection/focus.
    /// - **Tab / Shift+Tab** cycles the primary focus/selection to the next /
    ///   previous component in paint order (`Select` of that single id).
    ///
    /// All branches read the *ledger's* authoritative selection (seeded each
    /// press) so keyboard edits compose with mouse selection. A no-op key (nothing
    /// bound, or an action with an empty selection) returns `false`.
    pub fn handle_key(&mut self, key: &str, shift: bool) -> bool {
        let Some(action) = canvas_key_action(key, shift) else {
            return false;
        };
        let Some(ledger) = self.ledger() else {
            return false;
        };
        let selection = ledger.selection.component_ids.clone();

        match action {
            CanvasKeyAction::Nudge(delta) => {
                if selection.is_empty() {
                    return false;
                }
                if let Some(sink) = &self.sink {
                    for id in &selection {
                        if let Some(c) = ledger.components.get(id) {
                            sink(
                                SurfacePatch::MoveComponent {
                                    component_id: id.clone(),
                                    x: c.rect.x + delta.x,
                                    y: c.rect.y + delta.y,
                                },
                                ActorRef::human(None),
                            );
                        }
                    }
                }
                true
            }
            CanvasKeyAction::ClearSelection => {
                if let Some(sink) = &self.sink {
                    sink(SurfacePatch::Select { ids: Vec::new() }, ActorRef::human(None));
                }
                self.interaction.selection.clear();
                self.interaction.focus = None;
                true
            }
            CanvasKeyAction::DeleteSelection => {
                if selection.is_empty() {
                    return false;
                }
                if let Some(sink) = &self.sink {
                    for id in &selection {
                        sink(
                            SurfacePatch::DeleteComponent {
                                component_id: id.clone(),
                            },
                            ActorRef::human(None),
                        );
                    }
                }
                self.interaction.selection.clear();
                self.interaction.focus = None;
                true
            }
            CanvasKeyAction::CycleFocus { backward } => {
                // Stable order = the renderer's paint order (ascending z, then
                // insertion), so Tab walks what the user sees front-to-back.
                let order: Vec<ComponentId> =
                    paint_order(&ledger).into_iter().map(|(id, _)| id).collect();
                let current = self
                    .interaction
                    .focus
                    .clone()
                    .or_else(|| selection.last().cloned());
                let Some(next) = cycle_focus_target(&order, current.as_ref(), backward) else {
                    return false;
                };
                self.interaction.selection = vec![next.clone()];
                self.interaction.focus = Some(next.clone());
                if let Some(sink) = &self.sink {
                    sink(SurfacePatch::Select { ids: vec![next] }, ActorRef::human(None));
                }
                true
            }
        }
    }
}

impl Focusable for OceanCanvasView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // The handle is created lazily on first render; before that (e.g. a
        // freshly-built view that has not painted), hand out a fresh one so the
        // trait is always satisfiable.
        self.focus_handle
            .clone()
            .unwrap_or_else(|| cx.focus_handle())
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
    fn edge_route_straight_is_just_the_endpoints() {
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 0.0, 100.0, 100.0);
        let pts = edge_route(&from, &to, EdgeRoute::Straight);
        assert_eq!(pts, vec![Vec2::new(100.0, 50.0), Vec2::new(300.0, 50.0)]);
    }

    #[test]
    fn edge_route_orthogonal_turns_at_an_elbow() {
        // `from` upper-left, `to` lower-right: the L-route elbows at (to.x, from.y).
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 300.0, 100.0, 100.0);
        let pts = edge_route(&from, &to, EdgeRoute::Orthogonal);
        let (a, b) = edge_endpoints(&from, &to);
        assert_eq!(pts.len(), 3, "orthogonal route is from -> elbow -> to");
        assert_eq!(pts[0], a);
        assert_eq!(pts[1], Vec2::new(b.x, a.y), "elbow turns the corner");
        assert_eq!(pts[2], b);
        // The two segments are axis-aligned (one horizontal, one vertical).
        assert!((pts[0].y - pts[1].y).abs() < 1e-3, "first leg is horizontal");
        assert!((pts[1].x - pts[2].x).abs() < 1e-3, "second leg is vertical");
    }

    #[test]
    fn edge_route_bezier_carries_the_midpoint() {
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 0.0, 100.0, 100.0);
        let pts = edge_route(&from, &to, EdgeRoute::Bezier);
        let (a, b) = edge_endpoints(&from, &to);
        assert_eq!(pts, vec![a, Vec2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0), b]);
    }

    #[test]
    fn edge_draw_path_reaches_the_endpoint_for_a_diagonal_straight_route() {
        // OCEAN-192 (Codex P2): the bug was the *drawing* step collapsing a
        // diagonal segment to one axis, so a straight edge between non-aligned
        // components stopped at (to.x, from.y) instead of (to.x, to.y). The
        // drawn polyline must pass through — and END at — the real endpoint.
        //
        // `from` right-anchor is (100,50); `to` is lower-right so its closest
        // anchor is its left-midpoint (300,200) — a genuinely diagonal segment.
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 150.0, 100.0, 100.0);
        let (a, b) = edge_endpoints(&from, &to);
        assert_eq!(a, Vec2::new(100.0, 50.0));
        assert_eq!(b, Vec2::new(300.0, 200.0), "target is diagonally offset");

        let path = edge_draw_path(&from, &to, EdgeRoute::Straight);
        // The stroked polyline must start at the source and END at the target —
        // both x AND y of the final point match, not a collapsed (300,50).
        assert_eq!(*path.first().unwrap(), a, "stroke starts at the source anchor");
        let last = *path.last().unwrap();
        assert_eq!(last, b, "stroke reaches the real endpoint, not a collapsed axis");
        assert!(
            (last.y - a.y).abs() > f32::EPSILON,
            "diagonal: the y-delta is preserved end-to-end (regression guard)"
        );
    }

    #[test]
    fn edge_draw_path_orthogonal_stays_axis_aligned_and_reaches_endpoint() {
        // Orthogonal routes must keep rendering axis-aligned (each leg horizontal
        // or vertical) AND still terminate at the true endpoint.
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(300.0, 300.0, 100.0, 100.0);
        let (a, b) = edge_endpoints(&from, &to);
        let path = edge_draw_path(&from, &to, EdgeRoute::Orthogonal);
        assert_eq!(*path.first().unwrap(), a);
        assert_eq!(*path.last().unwrap(), b, "orthogonal route still reaches the endpoint");
        // Every consecutive leg is axis-aligned (shares an x or a y).
        for w in path.windows(2) {
            let axis_aligned =
                (w[0].x - w[1].x).abs() < 1e-3 || (w[0].y - w[1].y).abs() < 1e-3;
            assert!(axis_aligned, "orthogonal legs must stay axis-aligned: {:?}", w);
        }
    }

    #[test]
    fn edge_draw_path_matches_edge_route() {
        // The drawing contract is exactly the route geometry — no collapse.
        let from = Rect::new(10.0, 20.0, 80.0, 40.0);
        let to = Rect::new(400.0, 260.0, 60.0, 60.0);
        for route in [EdgeRoute::Straight, EdgeRoute::Orthogonal, EdgeRoute::Bezier] {
            assert_eq!(
                edge_draw_path(&from, &to, route),
                edge_route(&from, &to, route),
                "{route:?}: drawn path is the routed polyline"
            );
        }
    }

    #[test]
    fn edge_label_anchor_is_the_path_midpoint() {
        // Straight horizontal: midpoint sits halfway along.
        let mid = edge_label_anchor(&[Vec2::new(100.0, 50.0), Vec2::new(300.0, 50.0)]);
        assert_eq!(mid, Vec2::new(200.0, 50.0));

        // L-route 100 across then 100 down: half-length (100) lands at the elbow.
        let l = edge_label_anchor(&[
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 100.0),
        ]);
        assert_eq!(l, Vec2::new(100.0, 0.0), "half the L-length is the elbow");

        // Degenerate inputs don't panic.
        assert_eq!(edge_label_anchor(&[]), Vec2::new(0.0, 0.0));
        assert_eq!(edge_label_anchor(&[Vec2::new(7.0, 9.0)]), Vec2::new(7.0, 9.0));
    }

    #[test]
    fn edge_style_distinguishes_kinds() {
        let (flow_c, flow_w) = edge_style_for_kind(&EdgeKind::Flow);
        let (dep_c, _) = edge_style_for_kind(&EdgeKind::Dependency);
        let (ref_c, ref_w) = edge_style_for_kind(&EdgeKind::Reference);
        // Flow reads boldest, reference thinnest.
        assert!(flow_w > ref_w, "flow stroke wider than reference");
        assert_ne!(flow_c, ref_c, "flow and reference are different colors");
        assert_ne!(flow_c, dep_c, "flow and dependency are different colors");
        // An unknown agent-supplied kind falls back to the reference style.
        let (other_c, other_w) = edge_style_for_kind(&EdgeKind::Other("custom".into()));
        assert_eq!((other_c, other_w), (ref_c, ref_w));
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

        view.on_left_down(10.0, 10.0, SelectMode::Replace);
        view.on_left_up(10.0, 10.0);
        assert_eq!(view.interaction.focus, Some(ComponentId::new("a")));

        view.on_left_down(400.0, 400.0, SelectMode::Replace);
        view.on_left_up(400.0, 400.0);
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

        view.on_left_down(10.0, 10.0, SelectMode::Replace);
        view.on_left_up(10.0, 10.0);
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
        view.on_left_down(400.0, 400.0, SelectMode::Replace);
        view.on_left_up(400.0, 400.0);
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
                    label: Some("approves".into()),
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            0,
        );

        let edge = l.edges.values().next().unwrap();
        let from = &l.components.get(&edge.from.component_id).unwrap().rect;
        let to = &l.components.get(&edge.to.component_id).unwrap().rect;

        // Endpoints the helpers resolve.
        let (a, b) = edge_endpoints(from, to);
        assert_eq!(a, Vec2::new(100.0, 50.0));
        assert_eq!(b, Vec2::new(300.0, 50.0));

        // The full path `render_edge` consumes: the routed polyline (a straight
        // edge by default) plus the styled stroke and the midpoint label anchor.
        let route = edge_route(from, to, edge.route);
        assert_eq!(route, vec![a, b], "default route is the straight segment");
        let (color, width) = edge_style_for_kind(&edge.kind);
        assert_eq!((color, width), edge_style_for_kind(&EdgeKind::Flow));
        assert_eq!(
            edge_label_anchor(&route),
            Vec2::new(200.0, 50.0),
            "label sits at the segment midpoint"
        );
        assert_eq!(edge.label.as_deref(), Some("approves"));
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

    // ---- multi-select transition logic (OCEAN-194) -------------------------

    fn cid(s: &str) -> ComponentId {
        ComponentId::new(s)
    }

    #[test]
    fn select_mode_from_modifiers() {
        // Shift wins over secondary; plain is replace.
        assert_eq!(SelectMode::from_modifiers(false, false), SelectMode::Replace);
        assert_eq!(SelectMode::from_modifiers(true, false), SelectMode::Extend);
        assert_eq!(SelectMode::from_modifiers(false, true), SelectMode::Toggle);
        assert_eq!(SelectMode::from_modifiers(true, true), SelectMode::Extend);
    }

    #[test]
    fn next_selection_replace_picks_only_the_hit_or_clears() {
        let cur = vec![cid("a"), cid("b")];
        // Plain click on a component replaces with just that one.
        assert_eq!(
            next_selection(&cur, Some(&cid("c")), SelectMode::Replace),
            vec![cid("c")]
        );
        // Plain click on empty canvas clears.
        assert_eq!(next_selection(&cur, None, SelectMode::Replace), Vec::<ComponentId>::new());
    }

    #[test]
    fn next_selection_extend_adds_without_duplicating() {
        let cur = vec![cid("a")];
        // Shift+click a new id adds it (append, order preserved).
        assert_eq!(
            next_selection(&cur, Some(&cid("b")), SelectMode::Extend),
            vec![cid("a"), cid("b")]
        );
        // Shift+click an already-selected id is a no-op on the set.
        assert_eq!(
            next_selection(&cur, Some(&cid("a")), SelectMode::Extend),
            vec![cid("a")]
        );
        // Shift+click on empty canvas leaves the set unchanged.
        assert_eq!(next_selection(&cur, None, SelectMode::Extend), cur);
    }

    #[test]
    fn next_selection_toggle_flips_membership() {
        let cur = vec![cid("a"), cid("b")];
        // Ctrl/Cmd+click a selected id removes it.
        assert_eq!(
            next_selection(&cur, Some(&cid("a")), SelectMode::Toggle),
            vec![cid("b")]
        );
        // Ctrl/Cmd+click an unselected id adds it.
        assert_eq!(
            next_selection(&cur, Some(&cid("c")), SelectMode::Toggle),
            vec![cid("a"), cid("b"), cid("c")]
        );
        // Ctrl/Cmd+click empty canvas is a no-op.
        assert_eq!(next_selection(&cur, None, SelectMode::Toggle), cur);
    }

    #[test]
    fn drag_targets_moves_whole_selection_or_just_the_hit() {
        let sel = vec![cid("a"), cid("b"), cid("c")];
        // Dragging a member of the selection moves the whole set.
        assert_eq!(drag_targets(Some(&cid("b")), &sel), sel);
        // Dragging a non-member moves only that one.
        assert_eq!(drag_targets(Some(&cid("z")), &sel), vec![cid("z")]);
        // No hit -> no drag targets.
        assert_eq!(drag_targets(None, &sel), Vec::<ComponentId>::new());
    }

    #[test]
    fn should_arm_drag_only_when_the_hit_survives_the_press() {
        // OCEAN-194 P2 regression: a Toggle press that DESELECTED the hit must
        // NOT arm a drag, or a >threshold drift would move the just-deselected
        // component. `post` is the post-press selection.

        // Toggle-OFF: hit was selected, press removed it -> NOT in post -> no arm.
        let post_after_toggle_off = next_selection(&[cid("a"), cid("b")], Some(&cid("a")), SelectMode::Toggle);
        assert_eq!(post_after_toggle_off, vec![cid("b")], "toggle removed the hit");
        assert!(
            !should_arm_drag(Some(&cid("a")), &post_after_toggle_off),
            "a toggle-off press must not arm a drag for the deselected component"
        );

        // Toggle-ON a non-member: hit ends up selected -> arm.
        let post_after_toggle_on = next_selection(&[cid("a")], Some(&cid("b")), SelectMode::Toggle);
        assert_eq!(post_after_toggle_on, vec![cid("a"), cid("b")]);
        assert!(
            should_arm_drag(Some(&cid("b")), &post_after_toggle_on),
            "toggling a component ON leaves it selected, so a drag arms"
        );

        // Plain press on a member (preserved set): hit stays selected -> arm,
        // and drag_targets returns the whole group.
        let post_member = press_selection(&[cid("a"), cid("b")], Some(&cid("a")), SelectMode::Replace);
        assert_eq!(post_member, vec![cid("a"), cid("b")]);
        assert!(should_arm_drag(Some(&cid("a")), &post_member));
        assert_eq!(
            drag_targets(Some(&cid("a")), &post_member),
            vec![cid("a"), cid("b")],
            "armed drag of a member moves the whole group"
        );

        // Shift-extend: the hit is added and stays selected -> arm with the hit.
        let post_extend = next_selection(&[cid("a")], Some(&cid("b")), SelectMode::Extend);
        assert!(should_arm_drag(Some(&cid("b")), &post_extend));

        // Empty hit (press on blank canvas) never arms.
        assert!(!should_arm_drag(None, &[cid("a")]));
    }

    // ---- drag geometry (OCEAN-194) -----------------------------------------

    #[test]
    fn drag_threshold_distinguishes_click_from_drag() {
        let start = Vec2::new(100.0, 100.0);
        // A 2px move stays a click (below the 3px threshold).
        assert!(!drag_threshold_crossed(start, Vec2::new(102.0, 100.0)));
        // A 5px move is a drag.
        assert!(drag_threshold_crossed(start, Vec2::new(105.0, 100.0)));
        // Exactly at the threshold is NOT crossed (strict >).
        assert!(!drag_threshold_crossed(start, Vec2::new(100.0 + DRAG_THRESHOLD_PX, 100.0)));
    }

    #[test]
    fn screen_delta_divides_by_zoom() {
        // A 20px screen drag at 2x zoom is a 10-unit canvas move.
        assert_eq!(screen_delta_to_canvas(20.0, 40.0, 2.0), Vec2::new(10.0, 20.0));
        // At 1x it passes through.
        assert_eq!(screen_delta_to_canvas(15.0, -5.0, 1.0), Vec2::new(15.0, -5.0));
        // Degenerate zoom doesn't blow up.
        let m = screen_delta_to_canvas(10.0, 10.0, 0.0);
        assert!(m.x.is_finite() && m.y.is_finite());
    }

    #[test]
    fn dragged_position_is_base_plus_canvas_delta() {
        let rect = Rect::new(100.0, 200.0, 50.0, 50.0);
        // 60px right / 30px down screen drag at 1.5x zoom -> 40/20 canvas.
        let delta = screen_delta_to_canvas(60.0, 30.0, 1.5);
        assert_eq!(delta, Vec2::new(40.0, 20.0));
        assert_eq!(dragged_position(&rect, delta), Vec2::new(140.0, 220.0));
    }

    #[test]
    fn drag_state_offset_is_zero_until_threshold_crossed() {
        let mut d = DragState {
            start_screen: Vec2::new(0.0, 0.0),
            last_screen: Vec2::new(2.0, 0.0),
            moving: vec![(cid("a"), Rect::new(0.0, 0.0, 10.0, 10.0))],
            crossed: false,
        };
        // Sub-threshold: no preview offset.
        assert_eq!(d.canvas_offset(1.0), Vec2::new(0.0, 0.0));
        // Once crossed, the offset is the screen delta / zoom.
        d.last_screen = Vec2::new(20.0, 0.0);
        d.crossed = true;
        assert_eq!(d.canvas_offset(2.0), Vec2::new(10.0, 0.0));
    }

    // ---- drag-to-move integration through the ledger (OCEAN-194) -----------

    #[test]
    fn press_selection_preserves_a_member_of_a_multi_selection() {
        // Plain press on a non-member replaces.
        assert_eq!(
            press_selection(&[cid("a"), cid("b")], Some(&cid("c")), SelectMode::Replace),
            vec![cid("c")]
        );
        // Plain press on a MEMBER preserves the whole set (so it can drag).
        assert_eq!(
            press_selection(&[cid("a"), cid("b")], Some(&cid("a")), SelectMode::Replace),
            vec![cid("a"), cid("b")]
        );
        // Modifiers defer to next_selection (extend/toggle unchanged).
        assert_eq!(
            press_selection(&[cid("a")], Some(&cid("a")), SelectMode::Toggle),
            Vec::<ComponentId>::new(),
            "ctrl on a member still toggles it off"
        );
    }

    #[test]
    fn a_real_drag_emits_move_component_for_the_dragged_component() {
        // Dragging a single component past the threshold applies one
        // MoveComponent at base + canvas delta. The ledger is the source of
        // truth after the drop.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        view.on_left_down(10.0, 10.0, SelectMode::Replace);
        view.on_drag_move(40.0, 40.0); // +30,+30 from (10,10): crosses threshold
        view.on_left_up(40.0, 40.0);

        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        let a = led.component(&cid("a")).unwrap();
        // Screen delta (30,30) at zoom 1 -> canvas (30,30); base (0,0) -> (30,30).
        assert_eq!((a.rect.x, a.rect.y), (30.0, 30.0), "drag moved component a to base+delta");
    }

    #[test]
    fn whole_selection_drags_together_through_the_sink() {
        // With two components multi-selected, pressing on one member (no
        // modifier) preserves the set and arms a whole-group drag — both move by
        // the same delta via one MoveComponent each.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "card", Rect::new(200.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        // Select both: plain click a, shift+click b.
        view.on_left_down(10.0, 10.0, SelectMode::Replace);
        view.on_left_up(10.0, 10.0);
        view.on_left_down(210.0, 10.0, SelectMode::Extend);
        view.on_left_up(210.0, 10.0);
        {
            let g = cell.lock().unwrap();
            assert_eq!(
                g.as_ref().unwrap().selection.component_ids,
                vec![cid("a"), cid("b")],
                "shift+click extends the selection in the ledger"
            );
        }

        // Plain press on a (a member) preserves the set; drag +50,0 at 1x.
        view.on_left_down(10.0, 10.0, SelectMode::Replace);
        view.on_drag_move(60.0, 10.0); // +50,0: crosses threshold
        view.on_left_up(60.0, 10.0);

        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        // Both move by the same canvas delta (50,0) from their base positions.
        assert_eq!(led.component(&cid("a")).unwrap().rect.x, 50.0, "a moved with the group");
        assert_eq!(led.component(&cid("b")).unwrap().rect.x, 250.0, "b moved with the group");
    }

    #[test]
    fn a_sub_threshold_click_emits_no_move_component() {
        // Press + release without crossing the threshold is a pure select — the
        // component must NOT move.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(40.0, 40.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        view.on_left_down(50.0, 50.0, SelectMode::Replace);
        view.on_drag_move(51.0, 51.0); // 1px — below threshold
        view.on_left_up(51.0, 51.0);

        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        let a = led.component(&cid("a")).unwrap();
        assert_eq!((a.rect.x, a.rect.y), (40.0, 40.0), "a click must not move the component");
        // But it did select it.
        assert_eq!(led.selection.component_ids, vec![cid("a")]);
    }

    #[test]
    fn a_toggle_off_press_with_drift_does_not_move_the_deselected_component() {
        // OCEAN-194 P2 (Codex on #48), end-to-end: Ctrl/Cmd-click an
        // already-selected component to deselect it, then drift past the
        // threshold before release. The deselected component must NOT receive a
        // MoveComponent — the press armed no drag.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 100.0, 100.0), Value::Null), ActorRef::system(), 0);
        // Pre-select it.
        l.apply_patch(SurfacePatch::Select { ids: vec![cid("a")] }, ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        // Toggle-off press on a (it was selected) then drift 30px.
        view.on_left_down(10.0, 10.0, SelectMode::Toggle);
        view.on_drag_move(40.0, 40.0); // +30,+30: would cross the threshold
        view.on_left_up(40.0, 40.0);

        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        let a = led.component(&cid("a")).unwrap();
        assert_eq!(
            (a.rect.x, a.rect.y),
            (0.0, 0.0),
            "the just-deselected component must not drift/move"
        );
        // And it was deselected.
        assert!(
            led.selection.component_ids.is_empty(),
            "toggle-off cleared the selection"
        );
    }

    // ---- keyboard navigation (OCEAN-196) -----------------------------------

    #[test]
    fn canvas_key_action_maps_arrows_to_axis_nudges() {
        // Bare arrows nudge one fine step on the matching axis.
        assert_eq!(
            canvas_key_action("up", false),
            Some(CanvasKeyAction::Nudge(Vec2::new(0.0, -NUDGE_STEP)))
        );
        assert_eq!(
            canvas_key_action("down", false),
            Some(CanvasKeyAction::Nudge(Vec2::new(0.0, NUDGE_STEP)))
        );
        assert_eq!(
            canvas_key_action("left", false),
            Some(CanvasKeyAction::Nudge(Vec2::new(-NUDGE_STEP, 0.0)))
        );
        assert_eq!(
            canvas_key_action("right", false),
            Some(CanvasKeyAction::Nudge(Vec2::new(NUDGE_STEP, 0.0)))
        );
        // Shift uses the coarse step.
        assert_eq!(
            canvas_key_action("right", true),
            Some(CanvasKeyAction::Nudge(Vec2::new(NUDGE_STEP_COARSE, 0.0)))
        );
    }

    #[test]
    fn canvas_key_action_maps_escape_delete_tab() {
        assert_eq!(canvas_key_action("escape", false), Some(CanvasKeyAction::ClearSelection));
        assert_eq!(canvas_key_action("delete", false), Some(CanvasKeyAction::DeleteSelection));
        assert_eq!(canvas_key_action("backspace", false), Some(CanvasKeyAction::DeleteSelection));
        assert_eq!(
            canvas_key_action("tab", false),
            Some(CanvasKeyAction::CycleFocus { backward: false })
        );
        assert_eq!(
            canvas_key_action("tab", true),
            Some(CanvasKeyAction::CycleFocus { backward: true })
        );
        // An unbound key is ignored.
        assert_eq!(canvas_key_action("q", false), None);
        assert_eq!(canvas_key_action("enter", false), None);
    }

    #[test]
    fn cycle_focus_wraps_forward_and_backward() {
        let order = vec![cid("a"), cid("b"), cid("c")];
        // No current focus: forward -> first, backward -> last.
        assert_eq!(cycle_focus_target(&order, None, false), Some(cid("a")));
        assert_eq!(cycle_focus_target(&order, None, true), Some(cid("c")));
        // From b: forward -> c, backward -> a.
        assert_eq!(cycle_focus_target(&order, Some(&cid("b")), false), Some(cid("c")));
        assert_eq!(cycle_focus_target(&order, Some(&cid("b")), true), Some(cid("a")));
        // Wrap: from last forward -> first; from first backward -> last.
        assert_eq!(cycle_focus_target(&order, Some(&cid("c")), false), Some(cid("a")));
        assert_eq!(cycle_focus_target(&order, Some(&cid("a")), true), Some(cid("c")));
        // Unknown current falls back to first/last.
        assert_eq!(cycle_focus_target(&order, Some(&cid("z")), false), Some(cid("a")));
        // Empty order: nothing to focus.
        assert_eq!(cycle_focus_target(&[], None, false), None);
    }

    #[test]
    fn arrow_key_emits_move_component_for_each_selected() {
        // Two selected components both nudge right by the coarse step via one
        // MoveComponent each, applied through the sink to the ledger.
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(10.0, 10.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "card", Rect::new(200.0, 10.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(
            SurfacePatch::Select { ids: vec![cid("a"), cid("b")] },
            ActorRef::system(),
            0,
        );
        let (mut view, cell) = view_over_shared_cell(l);

        // Shift+Right -> +10 on x for both.
        assert!(view.handle_key("right", true), "arrow with a selection is consumed");
        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        assert_eq!(led.component(&cid("a")).unwrap().rect.x, 20.0);
        assert_eq!(led.component(&cid("b")).unwrap().rect.x, 210.0);
        // y untouched.
        assert_eq!(led.component(&cid("a")).unwrap().rect.y, 10.0);
    }

    #[test]
    fn arrow_key_with_empty_selection_is_a_noop() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(10.0, 10.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);
        assert!(!view.handle_key("up", false), "no selection -> not consumed");
        let g = cell.lock().unwrap();
        assert_eq!(g.as_ref().unwrap().component(&cid("a")).unwrap().rect.y, 10.0);
    }

    #[test]
    fn escape_clears_the_selection_through_the_sink() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(SurfacePatch::Select { ids: vec![cid("a")] }, ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);
        view.interaction_mut().selection = vec![cid("a")];
        view.interaction_mut().focus = Some(cid("a"));

        assert!(view.handle_key("escape", false));
        assert!(view.interaction().selection.is_empty(), "view-local selection cleared");
        assert_eq!(view.interaction().focus, None, "view-local focus cleared");
        let g = cell.lock().unwrap();
        assert!(
            g.as_ref().unwrap().selection.component_ids.is_empty(),
            "ledger selection cleared via Select{{ids: []}}"
        );
    }

    #[test]
    fn delete_removes_selected_components_through_the_sink() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "card", Rect::new(100.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(SurfacePatch::Select { ids: vec![cid("a")] }, ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        assert!(view.handle_key("delete", false));
        let g = cell.lock().unwrap();
        let led = g.as_ref().unwrap();
        assert!(led.component(&cid("a")).is_none(), "selected component deleted");
        assert!(led.component(&cid("b")).is_some(), "unselected component survives");
        assert!(view.interaction().selection.is_empty());
    }

    #[test]
    fn tab_cycles_primary_selection_to_the_next_component() {
        let mut l = ledger();
        l.apply_patch(upsert("a", "card", Rect::new(0.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("b", "card", Rect::new(100.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        l.apply_patch(upsert("c", "card", Rect::new(200.0, 0.0, 50.0, 50.0), Value::Null), ActorRef::system(), 0);
        let (mut view, cell) = view_over_shared_cell(l);

        // No focus yet: Tab selects the first.
        assert!(view.handle_key("tab", false));
        assert_eq!(view.interaction().focus, Some(cid("a")));
        {
            let g = cell.lock().unwrap();
            assert_eq!(g.as_ref().unwrap().selection.component_ids, vec![cid("a")]);
        }
        // Tab again -> b, then c, then wraps to a.
        view.handle_key("tab", false);
        assert_eq!(view.interaction().focus, Some(cid("b")));
        view.handle_key("tab", false);
        assert_eq!(view.interaction().focus, Some(cid("c")));
        view.handle_key("tab", false);
        assert_eq!(view.interaction().focus, Some(cid("a")), "wraps around");
        // Shift+Tab goes back to c.
        view.handle_key("tab", true);
        assert_eq!(view.interaction().focus, Some(cid("c")));
        let g = cell.lock().unwrap();
        assert_eq!(g.as_ref().unwrap().selection.component_ids, vec![cid("c")]);
    }

    // ---- fit-to-content / Focus(Canvas) (OCEAN-196) ------------------------

    #[test]
    fn components_bbox_spans_all_rects() {
        let rects = [
            Rect::new(10.0, 20.0, 30.0, 40.0),   // x:10..40 y:20..60
            Rect::new(100.0, 0.0, 50.0, 50.0),   // x:100..150 y:0..50
            Rect::new(-20.0, 80.0, 10.0, 10.0),  // x:-20..-10 y:80..90
        ];
        let bbox = components_bbox(&rects).unwrap();
        assert_eq!(bbox, Rect::new(-20.0, 0.0, 170.0, 90.0));
        // Empty set -> no bbox.
        assert_eq!(components_bbox(&[]), None);
    }

    /// Assert a fitted viewport actually contains the whole bbox when the same
    /// transform the renderer uses is applied to it.
    fn assert_bbox_contained(vp: Viewport, rects: &[Rect], view: Vec2) {
        let t = ViewportTransform::new(vp);
        let bbox = components_bbox(rects).unwrap();
        let tl = t.canvas_to_screen(Vec2::new(bbox.x, bbox.y));
        let br = t.canvas_to_screen(Vec2::new(bbox.x + bbox.w, bbox.y + bbox.h));
        // Fully on-screen (allowing a hair of float slack).
        assert!(tl.x >= -0.5 && tl.y >= -0.5, "bbox top-left off-screen: {tl:?}");
        assert!(
            br.x <= view.x + 0.5 && br.y <= view.y + 0.5,
            "bbox bottom-right off-screen: {br:?} (view {view:?})"
        );
    }

    #[test]
    fn fit_viewport_frames_and_centers_the_bbox() {
        let view = Vec2::new(800.0, 600.0);
        let rects = [
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(400.0, 200.0, 100.0, 100.0),
        ];
        let vp = fit_viewport(&rects, view, FIT_PADDING).unwrap();
        // The bbox is fully visible under the fitted camera.
        assert_bbox_contained(vp, &rects, view);
        // And it is centered: the bbox center maps to the view center.
        let t = ViewportTransform::new(vp);
        let bbox = components_bbox(&rects).unwrap();
        let center = t.canvas_to_screen(Vec2::new(bbox.x + bbox.w / 2.0, bbox.y + bbox.h / 2.0));
        assert!((center.x - view.x / 2.0).abs() < 1.0, "centered x: {center:?}");
        assert!((center.y - view.y / 2.0).abs() < 1.0, "centered y: {center:?}");
    }

    #[test]
    fn fit_viewport_single_component_is_centered_at_sane_zoom() {
        let view = Vec2::new(800.0, 600.0);
        let rects = [Rect::new(100.0, 100.0, 200.0, 120.0)];
        let vp = fit_viewport(&rects, view, FIT_PADDING).unwrap();
        assert!(vp.zoom >= 0.2 && vp.zoom <= 4.0, "zoom clamped: {}", vp.zoom);
        assert_bbox_contained(vp, &rects, view);
        let t = ViewportTransform::new(vp);
        let center = t.canvas_to_screen(Vec2::new(200.0, 160.0)); // bbox center
        assert!((center.x - 400.0).abs() < 1.0 && (center.y - 300.0).abs() < 1.0);
    }

    #[test]
    fn fit_viewport_empty_or_zero_view_is_a_noop() {
        let view = Vec2::new(800.0, 600.0);
        // No components -> no fit.
        assert_eq!(fit_viewport(&[], view, FIT_PADDING), None);
        // Zero-area view -> no fit (can't divide).
        assert_eq!(
            fit_viewport(&[Rect::new(0.0, 0.0, 10.0, 10.0)], Vec2::new(0.0, 600.0), FIT_PADDING),
            None
        );
    }

    #[test]
    fn fit_to_content_noops_before_first_paint_then_adopts() {
        let rects = [Rect::new(0.0, 0.0, 100.0, 100.0), Rect::new(300.0, 300.0, 100.0, 100.0)];
        let mut i = CanvasInteraction::default();
        // No painted size yet -> the camera does not move.
        assert_eq!(i.fit_to_content(&rects, FIT_PADDING), None);
        assert_eq!(i.viewport, Viewport::default(), "camera untouched before first paint");

        // After a paint records the size, the fit is adopted into the viewport.
        i.last_view_size = Some(Vec2::new(800.0, 600.0));
        let adopted = i.fit_to_content(&rects, FIT_PADDING).expect("fit adopted");
        assert_eq!(i.viewport, adopted, "interaction viewport adopts the fit");
        assert_bbox_contained(i.viewport, &rects, Vec2::new(800.0, 600.0));
    }
}
