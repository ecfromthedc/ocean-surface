//! Spatial render of the agent's canvas patch stream on the **web** surface
//! (OCEAN-248).
//!
//! The GPUI native shell (`ocean-gui`) applies each `surface_patch` envelope to a
//! full `CanvasLedger` and draws a real canvas; before this module the web surface
//! only listed `"N patch(es)"` + raw ids — the agent drew a picture and the human
//! got a changelog. This is the web's own (Leptos/WASM) port of that idea:
//!
//! - [`WebCanvasLedger`] is a client-side ledger that folds the
//!   [`SurfacePatch`](crate::daemon::SurfacePatch) op stream into a live
//!   `id → component` map plus an edge list. It mirrors the *data* decisions of
//!   the native `CanvasLedger` (placement allocation for rect-less upserts, move /
//!   resize / delete / connect / disconnect / group, edge cleanup on delete) so
//!   the same patch stream yields the same spatial layout on both surfaces.
//! - [`CanvasRender`] turns that ledger into a positioned scene: each component is
//!   an absolutely-positioned card at its canvas `x/y`, edges are SVG lines
//!   underneath. The whole scene is wrapped in a transform that fits all content
//!   into the panel (a lightweight "fit to content", no interactive pan/zoom — see
//!   the progressive-scope note on [`CanvasRender`]).
//!
//! The wire types themselves live in `crate::daemon` (the existing self-contained
//! mirror of `ocean-agent-sdk::surface`); this module only consumes them.

use std::collections::BTreeMap;

use leptos::prelude::*;
use serde_json::Value;

use crate::daemon::{CanvasComponentPatch, CanvasPatchEntry, SurfacePatch};

/// The canvas an agent draws to when it names no other. Mirrors the native
/// `DEFAULT_CANVAS_ID` and the `canvas:main` default used across the surface
/// protocol — the tab strip selects this canvas first when it is present.
pub const DEFAULT_CANVAS_ID: &str = "canvas:main";

// ===========================================================================
// Placement constants (mirror ocean-gui canvas/layout.rs so rect-less upserts
// land in the same grid the native surface uses).
// ===========================================================================

const DEFAULT_COMPONENT_WIDTH: f32 = 320.0;
const DEFAULT_COMPONENT_HEIGHT: f32 = 220.0;
const SLOT_ORIGIN_X: f32 = 80.0;
const SLOT_ORIGIN_Y: f32 = 80.0;
const SLOT_GAP: f32 = 32.0;
/// Columns to scan before wrapping to the next row when allocating a slot.
const SLOT_SCAN_COLUMNS: usize = 64;
/// Rows to scan before giving up and stacking at the origin.
const SLOT_SCAN_ROWS: usize = 256;

// ===========================================================================
// Ledger
// ===========================================================================

/// An axis-aligned rectangle in canvas space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && self.x + self.w > other.x
            && self.y < other.y + other.h
            && self.y + self.h > other.y
    }
}

/// A placed component in the web ledger. Keeps just what the renderer needs: the
/// id, the agent's `kind`/template string, geometry, and the free-form content
/// payload (title/body/status/etc.) the card slots read from.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerComponent {
    pub id: String,
    /// The agent's original `kind` string (e.g. `brief_card`, `workflow_node`).
    pub kind: String,
    pub rect: Rect,
    pub z_index: i32,
    pub content: Value,
}

/// Semantic of an edge between two components. The web wire mirror carries the
/// edge kind as a bare `Option<String>` (unlike the native `ocean-gui` ledger,
/// which resolves it to an enum); this is the web renderer's own resolution of
/// that string, matching the native `EdgeKind` variants so a flow / dependency /
/// reference edge reads the same on both surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    Flow,
    Dependency,
    Reference,
    /// Any kind outside the known set, carried as its raw name.
    Other(String),
}

/// A connection between two components, resolved to plain endpoint ids.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub label: Option<String>,
}

/// The client-side canvas state, folded from the patch stream. Insertion order is
/// preserved (a `Vec` keyed by id) so the same patch sequence always renders the
/// same scene — matching the native ledger's `IndexMap` ordering guarantee.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebCanvasLedger {
    pub canvas_id: String,
    pub components: Vec<LedgerComponent>,
    pub edges: Vec<LedgerEdge>,
}

impl WebCanvasLedger {
    /// Build a ledger by replaying every recorded patch entry in order. The web
    /// surface stores the raw envelopes (so a richer renderer can use them later);
    /// this collapses that log into current state for rendering.
    ///
    /// This folds **every** entry into one canvas regardless of `canvas_id`, so it
    /// is correct only when the caller has already bucketed the entries by canvas
    /// (see [`MultiCanvasLedger::from_entries`], which routes each patch to the
    /// ledger named in its envelope). The ledger's `canvas_id` is taken from the
    /// first entry.
    pub fn from_entries(entries: &[CanvasPatchEntry]) -> Self {
        let mut ledger = WebCanvasLedger::default();
        for entry in entries {
            if ledger.canvas_id.is_empty() {
                ledger.canvas_id = entry.canvas_id.clone();
            }
            ledger.apply(&entry.envelope.patch);
        }
        ledger
    }

    fn component_mut(&mut self, id: &str) -> Option<&mut LedgerComponent> {
        self.components.iter_mut().find(|c| c.id == id)
    }

    /// Fold one patch op into the ledger. Mirrors the native
    /// `CanvasLedger::apply_inner` decisions (sans provenance/revision, which the
    /// web render doesn't surface).
    pub fn apply(&mut self, patch: &SurfacePatch) {
        match patch {
            SurfacePatch::UpsertComponent { component } => self.upsert(component),
            SurfacePatch::MoveComponent { component_id, x, y } => {
                if let Some(c) = self.component_mut(component_id.as_str()) {
                    c.rect.x = *x;
                    c.rect.y = *y;
                }
            }
            SurfacePatch::ResizeComponent {
                component_id,
                width,
                height,
            } => {
                if let Some(c) = self.component_mut(component_id.as_str()) {
                    c.rect.w = *width;
                    c.rect.h = *height;
                }
            }
            SurfacePatch::DeleteComponent { component_id } => {
                let id = component_id.as_str();
                self.components.retain(|c| c.id != id);
                // Drop edges that referenced the deleted component.
                self.edges.retain(|e| e.from != id && e.to != id);
            }
            SurfacePatch::Connect { edge } => {
                let resolved = LedgerEdge {
                    id: edge.id.as_str().to_string(),
                    from: edge.from.component_id.as_str().to_string(),
                    to: edge.to.component_id.as_str().to_string(),
                    kind: edge_kind_from_opt(edge.kind.as_deref()),
                    label: edge.label.clone(),
                };
                if let Some(existing) = self.edges.iter_mut().find(|e| e.id == resolved.id) {
                    *existing = resolved;
                } else {
                    self.edges.push(resolved);
                }
            }
            SurfacePatch::Disconnect { edge_id } => {
                let id = edge_id.as_str();
                self.edges.retain(|e| e.id != id);
            }
            // Selection / focus / viewport / layout / group don't change the
            // spatial scene this renderer draws (no selection chrome, no
            // interactive viewport yet — see the progressive-scope note). They're
            // valid no-ops here; the native shell honors them fully.
            SurfacePatch::Focus { .. }
            | SurfacePatch::Select { .. }
            | SurfacePatch::SetViewport { .. }
            | SurfacePatch::Layout { .. }
            | SurfacePatch::Group { .. } => {}
        }
    }

    fn upsert(&mut self, patch: &CanvasComponentPatch) {
        let id = patch.id.as_str().to_string();

        // Resolve placement: honor an explicit rect, reuse the existing rect on
        // update, else allocate a non-overlapping slot (app owns placement — §6).
        let rect = match &patch.rect {
            Some(r) => Rect::new(r.x, r.y, r.w, r.h),
            None => {
                if let Some(existing) = self.components.iter().find(|c| c.id == id) {
                    existing.rect
                } else {
                    self.allocate_slot(DEFAULT_COMPONENT_WIDTH, DEFAULT_COMPONENT_HEIGHT)
                }
            }
        };

        if let Some(existing) = self.component_mut(&id) {
            existing.kind = patch.kind.clone();
            existing.rect = rect;
            if let Some(z) = patch.z_index {
                existing.z_index = z;
            }
            if !patch.content.is_null() {
                existing.content = patch.content.clone();
            }
        } else {
            self.components.push(LedgerComponent {
                id,
                kind: patch.kind.clone(),
                rect,
                z_index: patch.z_index.unwrap_or(0),
                content: patch.content.clone(),
            });
        }
    }

    /// Find the first grid slot of `width`×`height` that doesn't overlap any
    /// existing component. Mirrors `ocean-gui`'s `next_available_slot` so rect-less
    /// components land in the same predictable grid on both surfaces.
    fn allocate_slot(&self, width: f32, height: f32) -> Rect {
        for row in 0..SLOT_SCAN_ROWS {
            for column in 0..SLOT_SCAN_COLUMNS {
                let candidate = Rect::new(
                    SLOT_ORIGIN_X + column as f32 * (width + SLOT_GAP),
                    SLOT_ORIGIN_Y + row as f32 * (height + SLOT_GAP),
                    width,
                    height,
                );
                if !self.components.iter().any(|c| c.rect.intersects(&candidate)) {
                    return candidate;
                }
            }
        }
        Rect::new(SLOT_ORIGIN_X, SLOT_ORIGIN_Y, width, height)
    }

    /// Axis-aligned bounding box over every component, or `None` when empty.
    fn bbox(&self) -> Option<Rect> {
        let first = self.components.first()?;
        let mut min_x = first.rect.x;
        let mut min_y = first.rect.y;
        let mut max_x = first.rect.x + first.rect.w;
        let mut max_y = first.rect.y + first.rect.h;
        for c in &self.components[1..] {
            min_x = min_x.min(c.rect.x);
            min_y = min_y.min(c.rect.y);
            max_x = max_x.max(c.rect.x + c.rect.w);
            max_y = max_y.max(c.rect.y + c.rect.h);
        }
        Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
    }

    fn component(&self, id: &str) -> Option<&LedgerComponent> {
        self.components.iter().find(|c| c.id == id)
    }
}

// ===========================================================================
// Multi-canvas ledger (OCEAN-257)
// ===========================================================================

/// A set of [`WebCanvasLedger`]s keyed by `canvas_id`, so one session can hold
/// several coexisting canvases (a storyboard *and* a workflow board, say) instead
/// of collapsing every patch into one.
///
/// Each [`SurfacePatchEnvelope`](crate::daemon::SurfacePatchEnvelope) already
/// carries the `canvas_id` it targets; before this the web surface ignored that
/// and folded the whole stream into a single ledger. [`from_entries`] buckets the
/// recorded patch log by `canvas_id` and folds each bucket into its own ledger, so
/// a patch lands on — and only on — the canvas it names.
///
/// Canvases are stored in a `BTreeMap` so the tab strip orders them stably
/// (lexicographically by id) no matter the arrival order of patches.
///
/// [`from_entries`]: MultiCanvasLedger::from_entries
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MultiCanvasLedger {
    canvases: BTreeMap<String, WebCanvasLedger>,
}

impl MultiCanvasLedger {
    /// Bucket the recorded patch log by `canvas_id` and fold each bucket into its
    /// own [`WebCanvasLedger`]. Insertion order **within** a canvas is preserved
    /// (the entries keep their relative order), so each canvas renders identically
    /// to the single-canvas path; only the routing across canvases is new.
    pub fn from_entries(entries: &[CanvasPatchEntry]) -> Self {
        // Group entry references by canvas_id, preserving per-canvas order.
        let mut buckets: BTreeMap<String, Vec<CanvasPatchEntry>> = BTreeMap::new();
        for entry in entries {
            buckets
                .entry(entry.canvas_id.clone())
                .or_default()
                .push(entry.clone());
        }

        let canvases = buckets
            .into_iter()
            .map(|(id, entries)| (id, WebCanvasLedger::from_entries(&entries)))
            .collect();

        Self { canvases }
    }

    /// The canvas ids present, in stable (lexicographic) tab order.
    pub fn canvas_ids(&self) -> Vec<String> {
        self.canvases.keys().cloned().collect()
    }

    /// Whether no canvas has been created yet.
    pub fn is_empty(&self) -> bool {
        self.canvases.is_empty()
    }

    /// Borrow the ledger for `canvas_id`, if present.
    pub fn canvas(&self, canvas_id: &str) -> Option<&WebCanvasLedger> {
        self.canvases.get(canvas_id)
    }

    /// Resolve which canvas the view should show given the operator's current
    /// selection: the selected canvas when it still exists, else the default
    /// `canvas:main` when present, else the first canvas in tab order, else `None`
    /// (no canvas at all). This keeps the active tab stable as patches arrive and
    /// degrades sensibly when the selected canvas hasn't appeared yet or vanished.
    pub fn resolve_active(&self, selected: Option<&str>) -> Option<String> {
        if let Some(sel) = selected {
            if self.canvases.contains_key(sel) {
                return Some(sel.to_string());
            }
        }
        if self.canvases.contains_key(DEFAULT_CANVAS_ID) {
            return Some(DEFAULT_CANVAS_ID.to_string());
        }
        self.canvases.keys().next().cloned()
    }
}

/// Resolve an optional edge-kind string into [`EdgeKind`], matching the native
/// `EdgeKind::from_opt` (a missing kind reads as `reference`).
fn edge_kind_from_opt(kind: Option<&str>) -> EdgeKind {
    match kind {
        None => EdgeKind::Reference,
        Some("flow") => EdgeKind::Flow,
        Some("dependency") => EdgeKind::Dependency,
        Some("reference") => EdgeKind::Reference,
        Some(other) => EdgeKind::Other(other.to_string()),
    }
}

// ===========================================================================
// Pure render helpers (kind → visual family, content slots, edge geometry)
// ===========================================================================

/// The coarse visual family a component kind reads as. Drives the card's CSS
/// modifier class so a workflow node, a brief, and a stat look distinct without
/// porting the native per-template pixel layout (progressive scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardFamily {
    /// brief_card / proposal / generic card.
    Card,
    /// workflow_node / node.
    Node,
    /// kanban_column / lane / frame (a quiet container).
    Container,
    /// stat_tile / stat (a single big value).
    Stat,
    /// storyboard_frame / media_slot (a media placeholder).
    Media,
}

impl CardFamily {
    /// The CSS modifier suffix for this family (e.g. `node` → `ocean-canvas-card--node`).
    fn css(self) -> &'static str {
        match self {
            CardFamily::Card => "card",
            CardFamily::Node => "node",
            CardFamily::Container => "container",
            CardFamily::Stat => "stat",
            CardFamily::Media => "media",
        }
    }
}

/// Map an agent `kind`/template string onto a [`CardFamily`]. Mirrors the native
/// `ComponentKind::from_patch_kind` collapse rules (exact primitives first, then
/// the `_card`/`_node`/`_column`/`_frame` suffix families), defaulting to a card.
pub fn card_family(kind: &str) -> CardFamily {
    match kind {
        "card" => CardFamily::Card,
        "node" => CardFamily::Node,
        "stat" | "stat_tile" => CardFamily::Stat,
        "media_slot" | "mediaslot" | "storyboard_frame" => CardFamily::Media,
        "lane" | "frame" | "kanban_column" => CardFamily::Container,
        "text_block" | "textblock" => CardFamily::Card,
        k if k.ends_with("_node") => CardFamily::Node,
        k if k.ends_with("_column") || k.ends_with("_lane") => CardFamily::Container,
        k if k.ends_with("_frame") => CardFamily::Media,
        // brief_card / proposal_card / anything else → a card.
        _ => CardFamily::Card,
    }
}

/// The card title: an explicit non-empty `content.title`, else the component id.
pub fn card_title(component: &LedgerComponent) -> String {
    str_slot(&component.content, "title").unwrap_or_else(|| component.id.clone())
}

/// The card body line: `content.body`, else `content.text`, else empty. Trimmed to
/// the first line so a multi-paragraph body doesn't blow out the card height.
pub fn card_body(component: &LedgerComponent) -> Option<String> {
    let raw = str_slot(&component.content, "body").or_else(|| str_slot(&component.content, "text"))?;
    Some(raw.lines().next().unwrap_or(&raw).to_string())
}

/// A non-empty string field on a JSON object (mirrors the native `str_slot`).
fn str_slot(content: &Value, key: &str) -> Option<String> {
    content
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The big value a stat tile shows: `content.value` (string or number), else `—`.
pub fn stat_value(component: &LedgerComponent) -> String {
    match component.content.get("value") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "—".to_string(),
    }
}

/// A workflow node's coarse status label + a stable status keyword for styling.
/// Returns `(label, keyword)` where keyword ∈ {idle, running, ok, error, waiting}.
pub fn node_status(component: &LedgerComponent) -> Option<(String, &'static str)> {
    let raw = component.content.get("status").and_then(Value::as_str)?;
    let lowered = raw.to_ascii_lowercase();
    let keyword = match lowered.as_str() {
        "running" | "active" | "in_progress" => "running",
        "ok" | "done" | "complete" | "completed" | "success" => "ok",
        "error" | "failed" | "fail" => "error",
        "waiting" | "blocked" | "pending" => "waiting",
        _ => "idle",
    };
    Some((raw.to_string(), keyword))
}

/// The two screen-space points an edge connects: the pair of edge-midpoint
/// anchors (one per rect) closest to each other. Mirrors the native
/// `edge_endpoints` so the web edges route like the native ones.
pub fn edge_endpoints(from: &Rect, to: &Rect) -> ((f32, f32), (f32, f32)) {
    let anchors = |r: &Rect| {
        [
            (r.x + r.w / 2.0, r.y),       // top
            (r.x + r.w, r.y + r.h / 2.0), // right
            (r.x + r.w / 2.0, r.y + r.h), // bottom
            (r.x, r.y + r.h / 2.0),       // left
        ]
    };
    let from_a = anchors(from);
    let to_a = anchors(to);
    let mut best = (from_a[0], to_a[0]);
    let mut best_dist = f32::MAX;
    for &a in &from_a {
        for &b in &to_a {
            let d = (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2);
            if d < best_dist {
                best_dist = d;
                best = (a, b);
            }
        }
    }
    best
}

/// The CSS class suffix for an edge kind, so a flow / dependency / reference edge
/// reads distinctly (color via CSS).
fn edge_kind_class(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Flow => "flow",
        EdgeKind::Dependency => "dependency",
        EdgeKind::Reference | EdgeKind::Other(_) => "reference",
    }
}

// ===========================================================================
// Render component
// ===========================================================================

/// Outer padding (canvas units) left around the component bbox so cards don't sit
/// flush against the panel edge — the web analogue of the native `FIT_PADDING`.
const FIT_PADDING: f32 = 40.0;

/// Fixed height (CSS px) of the spatial canvas viewport in the surface. The scene
/// is scaled to fit this box; a generous height lets a few cards read at a usable
/// size without taking over the whole column.
const VIEWPORT_HEIGHT_PX: f32 = 340.0;

/// Spatial render of the agent's canvas patch stream (OCEAN-248, multi-canvas in
/// OCEAN-257).
///
/// Replaces the old text-list `CanvasPatchesPanel`: folds the recorded patch
/// envelopes into a [`MultiCanvasLedger`] — one [`WebCanvasLedger`] per
/// `canvas_id` — and draws the **active** canvas as positioned cards with SVG
/// edges underneath, inside a fit-to-content scaled scene. A tab strip appears
/// when more than one canvas is present so the operator can switch between them
/// (e.g. a storyboard and a workflow board the agent maintains in parallel).
///
/// **Progressive scope (intentionally deferred, see ticket):** interactive
/// pan/zoom, selection/focus chrome, drag-to-move, and pixel-exact per-template
/// layouts (port chips, tally rows, etc.) are *not* ported — the native GPUI shell
/// owns the rich editor. This delivers the core deliverable: patches route to the
/// canvas they name, multiple canvases coexist, and each renders as positioned
/// visual components+edges instead of a count. The scene auto-fits, so it stays
/// readable as the agent adds cards; a "fit to content" static camera stands in
/// for live viewport control.
#[component]
pub fn CanvasRender(canvas_patches: RwSignal<Vec<CanvasPatchEntry>>) -> impl IntoView {
    // The multi-canvas ledger is derived from the patch log; it recomputes
    // whenever a new `surface_patch` frame lands, bucketing each patch onto the
    // canvas it names. Cheap for the bounded log (≤512 entries).
    let multi = Memo::new(move |_| MultiCanvasLedger::from_entries(&canvas_patches.get()));

    // The operator's chosen tab. `None` until they pick one; the active canvas
    // then falls back to `canvas:main` / the first present canvas (see
    // `resolve_active`). Stored as the raw id so a tab that vanishes degrades
    // gracefully rather than pinning a dead canvas.
    let selected = RwSignal::new(None::<String>);

    // The canvas actually shown this frame, reconciling the selection against
    // what's present.
    let active_id = Memo::new(move |_| {
        multi.with(|m| selected.with(|s| m.resolve_active(s.as_deref())))
    });

    view! {
        <Show
            when=move || !multi.with(MultiCanvasLedger::is_empty)
            fallback=|| view! { <EmptyCanvasHint /> }
        >
            {move || {
                let ids = multi.with(MultiCanvasLedger::canvas_ids);
                let active = active_id.get();
                let tabs = (ids.len() > 1)
                    .then(|| render_tab_strip(&ids, active.as_deref(), selected));
                // The active canvas's scene, or the empty hint if it has no
                // components yet (e.g. only selection/viewport patches landed).
                let body = active
                    .as_deref()
                    .and_then(|id| multi.with(|m| m.canvas(id).map(render_scene_body)))
                    .unwrap_or_else(|| {
                        view! {
                            <div class="ocean-canvas__empty">
                                "Waiting for the agent to place components…"
                            </div>
                        }
                        .into_any()
                    });
                let meta = active
                    .as_deref()
                    .and_then(|id| multi.with(|m| m.canvas(id).map(scene_meta)))
                    .unwrap_or_default();
                let title = active.clone().unwrap_or_else(|| "Canvas".to_string());

                view! {
                    <section class="ocean-canvas" aria-label="agent canvas">
                        <header class="ocean-canvas__head">
                            <span class="ocean-canvas__title">{title}</span>
                            <span class="ocean-canvas__meta">{meta}</span>
                        </header>
                        {tabs}
                        {body}
                    </section>
                }
                .into_any()
            }}
        </Show>
    }
}

/// Render the canvas tab strip: one button per present canvas, the active one
/// marked. Clicking a tab sets the operator's selection. Only shown when more
/// than one canvas exists (a single canvas needs no switcher).
fn render_tab_strip(
    ids: &[String],
    active: Option<&str>,
    selected: RwSignal<Option<String>>,
) -> AnyView {
    let tabs: Vec<AnyView> = ids
        .iter()
        .map(|id| {
            let is_active = active == Some(id.as_str());
            let class = if is_active {
                "ocean-canvas__tab ocean-canvas__tab--active"
            } else {
                "ocean-canvas__tab"
            };
            let id_for_click = id.clone();
            let label = canvas_tab_label(id);
            view! {
                <button
                    type="button"
                    class=class
                    aria-pressed=is_active
                    on:click=move |_| selected.set(Some(id_for_click.clone()))
                >
                    {label}
                </button>
            }
            .into_any()
        })
        .collect();

    view! {
        <div class="ocean-canvas__tabs" role="tablist" aria-label="canvases">
            {tabs}
        </div>
    }
    .into_any()
}

/// A short, human-friendly tab label for a `canvas_id`. Strips a leading
/// `canvas:` prefix so `canvas:storyboard` reads as `storyboard`; falls back to
/// the raw id.
fn canvas_tab_label(canvas_id: &str) -> String {
    canvas_id
        .strip_prefix("canvas:")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(canvas_id)
        .to_string()
}

/// The `"N component(s) · M edge(s)"` meta line for one canvas's ledger.
fn scene_meta(ledger: &WebCanvasLedger) -> String {
    format!(
        "{} component(s) · {} edge(s)",
        ledger.components.len(),
        ledger.edges.len(),
    )
}

/// The empty-state shown once the panel has appeared but no component exists yet
/// (e.g. only selection/viewport patches have arrived). Keeps the section from
/// rendering a blank box.
#[component]
fn EmptyCanvasHint() -> impl IntoView {
    view! {
        <section class="ocean-canvas" aria-label="agent canvas">
            <header class="ocean-canvas__head">
                <span class="ocean-canvas__title">"Canvas"</span>
            </header>
            <div class="ocean-canvas__empty">"Waiting for the agent to place components…"</div>
        </section>
    }
}

/// Build the scaled scene body (viewport + positioned cards + SVG edges) for one
/// canvas's ledger. The enclosing `<section>`/`<header>` (title, meta, tab strip)
/// is owned by [`CanvasRender`]; this renders only the canvas plane so the same
/// body draws for whichever canvas is active.
fn render_scene_body(ledger: &WebCanvasLedger) -> AnyView {
    let bbox = ledger.bbox().unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));

    // Scene dimensions in canvas units (padded bbox). The scaled wrapper maps this
    // onto the fixed-height viewport, preserving aspect ratio (fit to content).
    let scene_w = (bbox.w + FIT_PADDING * 2.0).max(1.0);
    let scene_h = (bbox.h + FIT_PADDING * 2.0).max(1.0);
    // Origin offset so the padded bbox top-left maps to (0,0) in scene space.
    let off_x = FIT_PADDING - bbox.x;
    let off_y = FIT_PADDING - bbox.y;

    // Edge line segments, in scene coordinates, paired with a kind class.
    let edge_lines: Vec<(f32, f32, f32, f32, &'static str, Option<String>)> = ledger
        .edges
        .iter()
        .filter_map(|edge| {
            let from = ledger.component(&edge.from)?;
            let to = ledger.component(&edge.to)?;
            let (a, b) = edge_endpoints(&from.rect, &to.rect);
            Some((
                a.0 + off_x,
                a.1 + off_y,
                b.0 + off_x,
                b.1 + off_y,
                edge_kind_class(&edge.kind),
                edge.label.clone(),
            ))
        })
        .collect();

    // Cards, sorted by z-index (stable within equal z) so higher-z draws last.
    let mut ordered: Vec<&LedgerComponent> = ledger.components.iter().collect();
    ordered.sort_by_key(|c| c.z_index);
    let cards: Vec<AnyView> = ordered
        .into_iter()
        .map(|c| render_card(c, off_x, off_y))
        .collect();

    let viewbox = format!("0 0 {scene_w} {scene_h}");

    view! {
        <div
            class="ocean-canvas__viewport"
            style=format!("height:{VIEWPORT_HEIGHT_PX}px")
        >
            // The scene is sized in canvas units and CSS-scaled to fit the
            // viewport via aspect-ratio + width:100%. SVG edges sit beneath
            // absolutely-positioned card divs sharing the same coordinate box.
            <div
                class="ocean-canvas__scene"
                style=format!(
                    "width:{scene_w}px;height:{scene_h}px;aspect-ratio:{scene_w} / {scene_h}",
                )
            >
                <svg
                    class="ocean-canvas__edges"
                    viewBox=viewbox
                    preserveAspectRatio="none"
                >
                    {edge_lines
                        .into_iter()
                        .map(|(x1, y1, x2, y2, class, label)| {
                            let line_class = format!("ocean-canvas__edge ocean-canvas__edge--{class}");
                            let mid_x = (x1 + x2) / 2.0;
                            let mid_y = (y1 + y2) / 2.0;
                            view! {
                                <line
                                    class=line_class
                                    x1=x1
                                    y1=y1
                                    x2=x2
                                    y2=y2
                                />
                                {label
                                    .filter(|s| !s.is_empty())
                                    .map(|text| {
                                        view! {
                                            <text
                                                class="ocean-canvas__edge-label"
                                                x=mid_x
                                                y=mid_y
                                            >
                                                {text}
                                            </text>
                                        }
                                    })}
                            }
                        })
                        .collect_view()}
                </svg>
                {cards}
            </div>
        </div>
    }
    .into_any()
}

/// Render one component as an absolutely-positioned card in scene space. The card
/// body varies a little by family (stat shows a big value, media shows a
/// placeholder, node shows a status badge) but all share the same chrome.
fn render_card(component: &LedgerComponent, off_x: f32, off_y: f32) -> AnyView {
    let family = card_family(&component.kind);
    let rect = component.rect;
    let style = format!(
        "left:{}px;top:{}px;width:{}px;height:{}px",
        rect.x + off_x,
        rect.y + off_y,
        rect.w.max(1.0),
        rect.h.max(1.0),
    );
    let class = format!("ocean-canvas-card ocean-canvas-card--{}", family.css());
    let title = card_title(component);
    let kind_label = component.kind.clone();

    // Family-specific body slot.
    let body: AnyView = match family {
        CardFamily::Stat => {
            let value = stat_value(component);
            let label = str_slot(&component.content, "label");
            view! {
                <div class="ocean-canvas-card__stat">{value}</div>
                {label.map(|l| view! { <div class="ocean-canvas-card__sub">{l}</div> })}
            }
            .into_any()
        }
        CardFamily::Media => {
            let caption = str_slot(&component.content, "caption")
                .or_else(|| str_slot(&component.content, "media"));
            view! {
                <div class="ocean-canvas-card__media">"▶"</div>
                {caption.map(|c| view! { <div class="ocean-canvas-card__sub">{c}</div> })}
            }
            .into_any()
        }
        CardFamily::Node => {
            let status = node_status(component);
            let body = card_body(component);
            view! {
                {status
                    .map(|(label, keyword)| {
                        let badge_class = format!(
                            "ocean-canvas-card__badge ocean-canvas-card__badge--{keyword}",
                        );
                        view! { <span class=badge_class>{label}</span> }
                    })}
                {body.map(|b| view! { <div class="ocean-canvas-card__body">{b}</div> })}
            }
            .into_any()
        }
        // Card / Container fall back to title + body line.
        _ => {
            let body = card_body(component);
            view! {
                {body.map(|b| view! { <div class="ocean-canvas-card__body">{b}</div> })}
            }
            .into_any()
        }
    };

    view! {
        <div class=class style=style>
            <div class="ocean-canvas-card__head">
                <span class="ocean-canvas-card__title">{title}</span>
                <span class="ocean-canvas-card__kind">{kind_label}</span>
            </div>
            {body}
        </div>
    }
    .into_any()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{
        ActorRef, CanvasEdgePatch, CanvasId, ComponentId, EdgeId, Endpoint, PatchId, SurfaceId,
        SurfacePatchEnvelope,
    };
    use serde_json::json;

    fn upsert(id: &str, kind: &str, content: Value) -> SurfacePatch {
        SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new(id),
                kind: kind.to_string(),
                // All ledger tests use auto-placement; an explicit rect isn't
                // needed to exercise the apply/geometry logic.
                rect: None,
                z_index: None,
                content,
                metadata: Value::Null,
            },
        }
    }

    fn entry(canvas: &str, patch: SurfacePatch) -> CanvasPatchEntry {
        CanvasPatchEntry {
            canvas_id: canvas.to_string(),
            summary: String::new(),
            envelope: SurfacePatchEnvelope {
                patch_id: PatchId::new("p"),
                session_id: "s".to_string(),
                surface_id: SurfaceId::new("web"),
                canvas_id: CanvasId::new(canvas),
                actor: ActorRef {
                    kind: "agent".to_string(),
                    id: None,
                    label: None,
                },
                created_at_ms: 0,
                patch,
            },
        }
    }

    #[test]
    fn upsert_without_rect_allocates_non_overlapping_slots() {
        let mut ledger = WebCanvasLedger::default();
        ledger.apply(&upsert("a", "card", json!({})));
        ledger.apply(&upsert("b", "card", json!({})));
        assert_eq!(ledger.components.len(), 2);
        let a = ledger.component("a").unwrap().rect;
        let b = ledger.component("b").unwrap().rect;
        assert!(!a.intersects(&b), "auto-placed cards must not overlap");
    }

    #[test]
    fn move_and_resize_update_in_place() {
        let mut ledger = WebCanvasLedger::default();
        ledger.apply(&upsert("a", "card", json!({})));
        ledger.apply(&SurfacePatch::MoveComponent {
            component_id: ComponentId::new("a"),
            x: 500.0,
            y: 600.0,
        });
        ledger.apply(&SurfacePatch::ResizeComponent {
            component_id: ComponentId::new("a"),
            width: 100.0,
            height: 50.0,
        });
        let r = ledger.component("a").unwrap().rect;
        assert_eq!((r.x, r.y, r.w, r.h), (500.0, 600.0, 100.0, 50.0));
        assert_eq!(ledger.components.len(), 1, "upsert+move+resize is one card");
    }

    #[test]
    fn delete_component_drops_incident_edges() {
        let mut ledger = WebCanvasLedger::default();
        ledger.apply(&upsert("a", "card", json!({})));
        ledger.apply(&upsert("b", "card", json!({})));
        ledger.apply(&SurfacePatch::Connect {
            edge: CanvasEdgePatch {
                id: EdgeId::new("e1"),
                from: Endpoint {
                    component_id: ComponentId::new("a"),
                    port: None,
                },
                to: Endpoint {
                    component_id: ComponentId::new("b"),
                    port: None,
                },
                kind: Some("flow".to_string()),
                label: None,
                metadata: Value::Null,
            },
        });
        assert_eq!(ledger.edges.len(), 1);
        ledger.apply(&SurfacePatch::DeleteComponent {
            component_id: ComponentId::new("a"),
        });
        assert_eq!(ledger.components.len(), 1);
        assert!(ledger.edges.is_empty(), "edges on a deleted node are removed");
    }

    #[test]
    fn from_entries_replays_log_into_current_state() {
        let entries = vec![
            entry("canvas:main", upsert("brief-1", "brief_card", json!({ "title": "Brief" }))),
            entry(
                "canvas:main",
                SurfacePatch::MoveComponent {
                    component_id: ComponentId::new("brief-1"),
                    x: 10.0,
                    y: 20.0,
                },
            ),
        ];
        let ledger = WebCanvasLedger::from_entries(&entries);
        assert_eq!(ledger.canvas_id, "canvas:main");
        let c = ledger.component("brief-1").unwrap();
        assert_eq!((c.rect.x, c.rect.y), (10.0, 20.0));
        assert_eq!(card_title(c), "Brief");
    }

    // ----- Multi-canvas routing (OCEAN-257) --------------------------------

    #[test]
    fn patches_route_to_their_named_canvas() {
        // Two canvases, interleaved on the wire — a storyboard frame and a
        // workflow node. Each patch must land on (and only on) the canvas its
        // envelope names, not collapse into one.
        let entries = vec![
            entry(
                "canvas:storyboard",
                upsert("frame-1", "storyboard_frame", json!({ "title": "Scene 1" })),
            ),
            entry(
                "canvas:workflow",
                upsert("node-1", "workflow_node", json!({ "title": "Fetch" })),
            ),
            entry(
                "canvas:storyboard",
                upsert("frame-2", "storyboard_frame", json!({ "title": "Scene 2" })),
            ),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries);

        assert_eq!(multi.canvas_ids().len(), 2, "two distinct canvases must coexist");
        assert_eq!(
            multi.canvas_ids(),
            vec!["canvas:storyboard".to_string(), "canvas:workflow".to_string()],
            "canvas ids are present in stable lexicographic tab order",
        );

        let story = multi.canvas("canvas:storyboard").expect("storyboard present");
        assert_eq!(story.components.len(), 2, "both storyboard frames land here");
        assert!(story.component("frame-1").is_some());
        assert!(story.component("frame-2").is_some());
        assert!(
            story.component("node-1").is_none(),
            "the workflow node must NOT bleed into the storyboard canvas",
        );

        let flow = multi.canvas("canvas:workflow").expect("workflow present");
        assert_eq!(flow.components.len(), 1, "only the workflow node lands here");
        assert!(flow.component("node-1").is_some());
        assert!(flow.component("frame-1").is_none());
    }

    #[test]
    fn move_only_affects_its_own_canvas() {
        // A component id can repeat across canvases; a move on one canvas must not
        // touch a same-id component on another.
        let entries = vec![
            entry("canvas:a", upsert("dup", "card", json!({}))),
            entry("canvas:b", upsert("dup", "card", json!({}))),
            entry(
                "canvas:a",
                SurfacePatch::MoveComponent {
                    component_id: ComponentId::new("dup"),
                    x: 777.0,
                    y: 888.0,
                },
            ),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries);

        let a = multi.canvas("canvas:a").unwrap().component("dup").unwrap().rect;
        let b = multi.canvas("canvas:b").unwrap().component("dup").unwrap().rect;
        assert_eq!((a.x, a.y), (777.0, 888.0), "move applied on canvas:a");
        assert_ne!(
            (b.x, b.y),
            (777.0, 888.0),
            "canvas:b's same-id component is untouched",
        );
    }

    #[test]
    fn resolve_active_prefers_selection_then_main_then_first() {
        let entries = vec![
            entry("canvas:main", upsert("m", "card", json!({}))),
            entry("canvas:zeta", upsert("z", "card", json!({}))),
            entry("canvas:alpha", upsert("a", "card", json!({}))),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries);

        // A live selection wins.
        assert_eq!(
            multi.resolve_active(Some("canvas:zeta")),
            Some("canvas:zeta".to_string()),
        );
        // A selection that isn't present falls back to canvas:main.
        assert_eq!(
            multi.resolve_active(Some("canvas:ghost")),
            Some("canvas:main".to_string()),
        );
        // No selection → canvas:main when present.
        assert_eq!(multi.resolve_active(None), Some("canvas:main".to_string()));
    }

    #[test]
    fn resolve_active_falls_back_to_first_when_no_main() {
        let entries = vec![
            entry("canvas:zeta", upsert("z", "card", json!({}))),
            entry("canvas:alpha", upsert("a", "card", json!({}))),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries);
        // No canvas:main → first in stable (lexicographic) order is canvas:alpha.
        assert_eq!(multi.resolve_active(None), Some("canvas:alpha".to_string()));
    }

    #[test]
    fn empty_log_is_empty_multi_canvas() {
        let multi = MultiCanvasLedger::from_entries(&[]);
        assert!(multi.is_empty());
        assert_eq!(multi.resolve_active(None), None);
        assert!(multi.canvas_ids().is_empty());
    }

    #[test]
    fn canvas_tab_label_strips_canvas_prefix() {
        assert_eq!(canvas_tab_label("canvas:storyboard"), "storyboard");
        assert_eq!(canvas_tab_label("canvas:main"), "main");
        // No prefix, or a bare `canvas:` → raw id.
        assert_eq!(canvas_tab_label("board"), "board");
        assert_eq!(canvas_tab_label("canvas:"), "canvas:");
    }

    #[test]
    fn card_family_collapse_matches_native_rules() {
        assert_eq!(card_family("brief_card"), CardFamily::Card);
        assert_eq!(card_family("workflow_node"), CardFamily::Node);
        assert_eq!(card_family("kanban_column"), CardFamily::Container);
        assert_eq!(card_family("stat_tile"), CardFamily::Stat);
        assert_eq!(card_family("storyboard_frame"), CardFamily::Media);
        assert_eq!(card_family("anything_else"), CardFamily::Card);
    }

    #[test]
    fn edge_endpoints_pick_closest_anchors() {
        // `b` is directly to the right of `a`: the closest anchors are a.right
        // and b.left.
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(300.0, 0.0, 100.0, 100.0);
        let (from, to) = edge_endpoints(&a, &b);
        assert_eq!(from, (100.0, 50.0), "a.right");
        assert_eq!(to, (300.0, 50.0), "b.left");
    }

    #[test]
    fn node_status_keyword_buckets() {
        let node = LedgerComponent {
            id: "n".to_string(),
            kind: "workflow_node".to_string(),
            rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            z_index: 0,
            content: json!({ "status": "Running" }),
        };
        assert_eq!(node_status(&node), Some(("Running".to_string(), "running")));
    }
}
