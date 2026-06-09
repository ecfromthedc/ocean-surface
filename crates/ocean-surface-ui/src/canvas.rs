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

use crate::daemon::{
    ActorId, CanvasComponentPatch, CanvasMergeState, CanvasPatchEntry, ComponentVersion,
    LamportClock, MergeDecision, SurfacePatch, SurfacePatchEnvelope,
};

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
    /// Per-component version vector for the convergent merge (OCEAN-270). The web
    /// ledger is the merge point where the agent's streamed patches (and, later,
    /// concurrent operator edits) meet; this gates `apply` so a superseded write
    /// is dropped and two concurrent writes to the same component converge to one
    /// deterministic winner regardless of arrival order.
    merge_state: CanvasMergeState,
    /// This ledger's Lamport clock — observes each incoming version and assigns
    /// one to unversioned (daemon-relayed) patches at apply time.
    clock: LamportClock,
}

impl WebCanvasLedger {
    /// Build a ledger by replaying every recorded patch entry in order, through
    /// the convergent merge. The web surface stores the raw envelopes; this
    /// collapses that log into current state for rendering. Because the merge is
    /// commutative and the log is replayed whole each frame, the same multiset of
    /// versioned envelopes always yields the same scene.
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
            ledger.apply_envelope(&entry.envelope);
        }
        ledger
    }

    /// Fold one **envelope** through the convergent merge (OCEAN-270), then apply
    /// the patch if it won. This is the merge gate: for a patch that contends for
    /// a single component ([`SurfacePatch::target_component`]),
    ///
    /// - a carried version is `observe`d (clock jumps past it) then `merge`d;
    /// - an unversioned patch (the daemon relays `version: None`) is stamped from
    ///   this ledger's clock — the surface ledger is the merge point;
    /// - [`Applied`](MergeDecision::Applied) → apply the patch;
    ///   [`Superseded`](MergeDecision::Superseded) → **skip** (a higher version
    ///   already won — this is how a stale/out-of-order patch is dropped and two
    ///   replicas converge).
    ///
    /// Patches that don't target a single component apply directly (never gated).
    pub fn apply_envelope(&mut self, envelope: &SurfacePatchEnvelope) {
        if let Some(id) = envelope.patch.target_component() {
            let incoming = match &envelope.version {
                Some(v) => {
                    self.clock.observe(v.rev);
                    v.clone()
                }
                None => ComponentVersion::new(
                    self.clock.tick(),
                    ActorId::from_actor(&envelope.actor),
                ),
            };
            if self.merge_state.merge(id, incoming) == MergeDecision::Superseded {
                return; // a higher version already won — drop this patch
            }
        }
        self.apply(&envelope.patch);
    }

    fn component_mut(&mut self, id: &str) -> Option<&mut LedgerComponent> {
        self.components.iter_mut().find(|c| c.id == id)
    }

    /// Fold one patch op into the ledger. Mirrors the native
    /// `CanvasLedger::apply_inner` decisions (sans provenance/revision, which the
    /// web render doesn't surface).
    ///
    /// This is the raw apply; the convergent-merge gate lives in
    /// [`apply_envelope`](Self::apply_envelope), which is what
    /// [`from_entries`](Self::from_entries) drives. Calling `apply` directly skips
    /// the merge (used where there is no version context, e.g. unit tests).
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

/// Maximum number of canvases the web multi-ledger retains (OCEAN-278). Mirrors
/// the native [`MAX_CANVASES`](../../ocean_gui/shell/canvas/ledger_set/constant.MAX_CANVASES.html)
/// intent: a real session holds a handful of canvases at once; past this the
/// least-recently-active ones are dropped so a session that keeps naming fresh
/// canvas ids can't grow the rendered set without bound. Eviction is a backstop —
/// sized so ordinary multi-canvas work never trips it.
pub const MAX_CANVASES: usize = 16;

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
/// # Bounded growth (OCEAN-278)
///
/// This ledger is *derived*: [`CanvasRender`] rebuilds it each frame from the
/// recorded patch log, which is itself capped (`MAX_CANVAS_PATCHES`) and **wiped
/// on every session switch** (see `daemon.rs`). So per-session scoping is inherited
/// — a prior session's canvases can't appear because their patches are gone. What
/// the log cap does *not* bound is the number of distinct `canvas_id`s a single
/// session's patches spread across; [`from_entries`] therefore trims the built set
/// to [`MAX_CANVASES`], evicting the **least-recently-active** canvases (by their
/// newest patch's `created_at_ms`) while always keeping the active one — the
/// operator's `selected` canvas when supplied, else `canvas:main`/the most-recent.
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
    ///
    /// The built set is bounded to [`MAX_CANVASES`] (OCEAN-278): when more distinct
    /// canvases appear in the log, the least-recently-active ones are dropped. The
    /// kept-active canvas — never evicted — is the operator's `selected` when it's
    /// still present, else `canvas:main`, else the most-recently-active; pass the
    /// operator's current tab as `selected` so the canvas they're viewing can't
    /// vanish out from under them just because it went quiet (`None` lets it
    /// default).
    pub fn from_entries(entries: &[CanvasPatchEntry], selected: Option<&str>) -> Self {
        // Group entry references by canvas_id, preserving per-canvas order, and
        // track each canvas's newest patch time for LRU eviction.
        let mut buckets: BTreeMap<String, Vec<CanvasPatchEntry>> = BTreeMap::new();
        let mut last_seen: BTreeMap<String, i64> = BTreeMap::new();
        for entry in entries {
            buckets
                .entry(entry.canvas_id.clone())
                .or_default()
                .push(entry.clone());
            let ts = entry.envelope.created_at_ms;
            last_seen
                .entry(entry.canvas_id.clone())
                .and_modify(|t| *t = (*t).max(ts))
                .or_insert(ts);
        }

        let mut ledger = Self {
            canvases: buckets
                .into_iter()
                .map(|(id, entries)| (id, WebCanvasLedger::from_entries(&entries)))
                .collect(),
        };
        ledger.evict_to_cap(&last_seen, selected);
        ledger
    }

    /// Drop least-recently-active canvases until at most [`MAX_CANVASES`] remain
    /// (OCEAN-278). Recency is each canvas's newest patch `created_at_ms`; the
    /// active canvas — the operator's `selected` if present and still here, else
    /// `canvas:main`, else the most-recently-active — is always kept. Ties on
    /// timestamp fall back to canvas id (lexicographic) so eviction is
    /// deterministic regardless of map iteration nuances.
    fn evict_to_cap(&mut self, last_seen: &BTreeMap<String, i64>, selected: Option<&str>) {
        if self.canvases.len() <= MAX_CANVASES {
            return;
        }

        // The canvas we must never evict.
        let keep_active = self.active_to_keep(last_seen, selected);

        // Order non-active canvases by recency (oldest first), tie-broken by id, so
        // we evict the stalest first and deterministically.
        let mut candidates: Vec<(&String, i64)> = self
            .canvases
            .keys()
            .filter(|id| Some(id.as_str()) != keep_active.as_deref())
            .map(|id| (id, last_seen.get(id).copied().unwrap_or(i64::MIN)))
            .collect();
        candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));

        let evict_count = self.canvases.len() - MAX_CANVASES;
        let doomed: Vec<String> = candidates
            .into_iter()
            .take(evict_count)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            self.canvases.remove(&id);
        }
    }

    /// The canvas that must survive eviction: the operator's `selected` (when still
    /// present), else `canvas:main`, else the most-recently-active canvas. Mirrors
    /// [`resolve_active`](Self::resolve_active)'s preference order so the kept
    /// canvas is the one the view would show.
    fn active_to_keep(
        &self,
        last_seen: &BTreeMap<String, i64>,
        selected: Option<&str>,
    ) -> Option<String> {
        if let Some(sel) = selected {
            if self.canvases.contains_key(sel) {
                return Some(sel.to_string());
            }
        }
        if self.canvases.contains_key(DEFAULT_CANVAS_ID) {
            return Some(DEFAULT_CANVAS_ID.to_string());
        }
        // Most-recently-active, tie-broken by id (descending) for determinism.
        self.canvases
            .keys()
            .max_by(|a, b| {
                let ta = last_seen.get(*a).copied().unwrap_or(i64::MIN);
                let tb = last_seen.get(*b).copied().unwrap_or(i64::MIN);
                ta.cmp(&tb).then_with(|| a.cmp(b))
            })
            .cloned()
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
    // The operator's chosen tab. `None` until they pick one; the active canvas
    // then falls back to `canvas:main` / the first present canvas (see
    // `resolve_active`). Stored as the raw id so a tab that vanishes degrades
    // gracefully rather than pinning a dead canvas.
    let selected = RwSignal::new(None::<String>);

    // The multi-canvas ledger is derived from the patch log; it recomputes
    // whenever a new `surface_patch` frame lands, bucketing each patch onto the
    // canvas it names. Cheap for the bounded log (≤512 entries). The set is capped
    // at `MAX_CANVASES` (OCEAN-278): the operator's selection is passed through so
    // that, if eviction is needed, the tab they're viewing is never the one
    // dropped.
    let multi = Memo::new(move |_| {
        selected.with(|s| MultiCanvasLedger::from_entries(&canvas_patches.get(), s.as_deref()))
    });

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
                version: None,
            },
        }
    }

    /// A `move_component` entry carrying an explicit version, as it would arrive
    /// from another replica. `actor` is `"{kind}:{actor_id}"`.
    fn versioned_move_entry(
        canvas: &str,
        id: &str,
        x: f32,
        rev: u64,
        kind: &str,
        actor_id: &str,
    ) -> CanvasPatchEntry {
        CanvasPatchEntry {
            canvas_id: canvas.to_string(),
            summary: String::new(),
            envelope: SurfacePatchEnvelope {
                patch_id: PatchId::new(format!("p:{id}@{rev}")),
                session_id: "s".to_string(),
                surface_id: SurfaceId::new("web"),
                canvas_id: CanvasId::new(canvas),
                actor: ActorRef {
                    kind: kind.to_string(),
                    id: Some(actor_id.to_string()),
                    label: None,
                },
                created_at_ms: 0,
                patch: SurfacePatch::MoveComponent {
                    component_id: ComponentId::new(id),
                    x,
                    y: 0.0,
                },
                version: Some(ComponentVersion::new(
                    rev,
                    ActorId::new(format!("{kind}:{actor_id}")),
                )),
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
        let multi = MultiCanvasLedger::from_entries(&entries, None);

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
        let multi = MultiCanvasLedger::from_entries(&entries, None);

        let a = multi.canvas("canvas:a").unwrap().component("dup").unwrap().rect;
        let b = multi.canvas("canvas:b").unwrap().component("dup").unwrap().rect;
        assert_eq!((a.x, a.y), (777.0, 888.0), "move applied on canvas:a");
        assert_ne!(
            (b.x, b.y),
            (777.0, 888.0),
            "canvas:b's same-id component is untouched",
        );
    }

    /// An upsert entry on `canvas` stamped with an explicit `created_at_ms`, so
    /// LRU-by-time eviction can be exercised (the bare `entry` helper stamps 0).
    fn entry_at(canvas: &str, id: &str, created_at_ms: i64) -> CanvasPatchEntry {
        let mut e = entry(canvas, upsert(id, "card", json!({})));
        e.envelope.created_at_ms = created_at_ms;
        e
    }

    // ----- Bounded growth + eviction (OCEAN-257 follow-up, OCEAN-278) ------

    #[test]
    fn multi_canvas_set_is_capped() {
        // Far more distinct canvases than the cap arrive in the log; the built set
        // must never exceed MAX_CANVASES.
        let entries: Vec<CanvasPatchEntry> = (0..(MAX_CANVASES + 10))
            .map(|n| entry_at(&format!("canvas:{n}"), "c", n as i64))
            .collect();
        let multi = MultiCanvasLedger::from_entries(&entries, None);
        assert_eq!(
            multi.canvas_ids().len(),
            MAX_CANVASES,
            "the set is trimmed to the cap",
        );
    }

    #[test]
    fn eviction_drops_least_recently_active_canvases() {
        // canvas:0 is the oldest (ts 0), canvas:N the newest. One over the cap →
        // the single oldest non-kept canvas is evicted.
        let entries: Vec<CanvasPatchEntry> = (0..=MAX_CANVASES)
            .map(|n| entry_at(&format!("canvas:{n}"), "c", n as i64))
            .collect();
        let multi = MultiCanvasLedger::from_entries(&entries, None);

        assert_eq!(multi.canvas_ids().len(), MAX_CANVASES);
        // canvas:0 is the stalest and not the kept-active (no canvas:main, newest
        // is kept) → it is the one evicted.
        assert!(
            multi.canvas("canvas:0").is_none(),
            "the least-recently-active canvas was evicted",
        );
        assert!(
            multi.canvas(&format!("canvas:{MAX_CANVASES}")).is_some(),
            "the most-recent canvas is retained",
        );
    }

    #[test]
    fn eviction_always_keeps_canvas_main_even_when_stale() {
        // canvas:main is the oldest (ts 0) — by recency it would be the first
        // evicted, but it's the default active canvas and must be kept.
        let mut entries = vec![entry_at("canvas:main", "m", 0)];
        entries.extend(
            (1..=MAX_CANVASES).map(|n| entry_at(&format!("canvas:{n}"), "c", n as i64)),
        );
        let multi = MultiCanvasLedger::from_entries(&entries, None);

        assert_eq!(multi.canvas_ids().len(), MAX_CANVASES);
        assert!(
            multi.canvas("canvas:main").is_some(),
            "canvas:main is the active default and is never evicted, even when stalest",
        );
        // The stalest *non-main* canvas (canvas:1) is evicted instead.
        assert!(
            multi.canvas("canvas:1").is_none(),
            "the stalest non-active canvas was evicted in canvas:main's place",
        );
    }

    #[test]
    fn eviction_keeps_the_operator_selected_canvas_even_when_stale() {
        // The operator is viewing canvas:1 (stale, ts 1). Even though it's old and
        // not canvas:main, passing it as the selection must spare it from eviction.
        let mut entries = vec![
            entry_at("canvas:main", "m", 1_000), // recent, also kept
            entry_at("canvas:1", "c", 1),        // stale, but selected
        ];
        entries.extend(
            (2..=MAX_CANVASES).map(|n| entry_at(&format!("canvas:{n}"), "c", 100 + n as i64)),
        );
        // Without a selection canvas:1 would be evicted (it's the stalest non-main).
        let unselected = MultiCanvasLedger::from_entries(&entries, None);
        assert!(
            unselected.canvas("canvas:1").is_none(),
            "sanity: canvas:1 is the eviction victim when not selected",
        );

        let selected = MultiCanvasLedger::from_entries(&entries, Some("canvas:1"));
        assert_eq!(selected.canvas_ids().len(), MAX_CANVASES);
        assert!(
            selected.canvas("canvas:1").is_some(),
            "the operator's selected canvas is kept even when stale",
        );
        // canvas:main is also kept (recent + default); the evicted one is the next
        // stalest non-protected canvas, canvas:2.
        assert!(selected.canvas("canvas:main").is_some(), "canvas:main still kept");
        assert!(
            selected.canvas("canvas:2").is_none(),
            "the stalest unprotected canvas (canvas:2) was evicted instead",
        );
    }

    #[test]
    fn at_or_under_cap_evicts_nothing() {
        // Exactly the cap: every canvas is retained, in stable tab order.
        let entries: Vec<CanvasPatchEntry> = (0..MAX_CANVASES)
            .map(|n| entry_at(&format!("canvas:{n:02}"), "c", n as i64))
            .collect();
        let multi = MultiCanvasLedger::from_entries(&entries, None);
        assert_eq!(
            multi.canvas_ids().len(),
            MAX_CANVASES,
            "nothing is evicted at the cap",
        );
    }

    #[test]
    fn resolve_active_prefers_selection_then_main_then_first() {
        let entries = vec![
            entry("canvas:main", upsert("m", "card", json!({}))),
            entry("canvas:zeta", upsert("z", "card", json!({}))),
            entry("canvas:alpha", upsert("a", "card", json!({}))),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries, None);

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
        let multi = MultiCanvasLedger::from_entries(&entries, None);
        // No canvas:main → first in stable (lexicographic) order is canvas:alpha.
        assert_eq!(multi.resolve_active(None), Some("canvas:alpha".to_string()));
    }

    #[test]
    fn empty_log_is_empty_multi_canvas() {
        let multi = MultiCanvasLedger::from_entries(&[], None);
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

    // ----- Convergent merge (OCEAN-270) ------------------------------------
    // The web ledger applies the same per-component LWW as the SDK/native: two
    // concurrent edits to the same component converge to a deterministic winner
    // regardless of arrival order; edits to different components both land; a
    // stale/superseded patch can't stomp a newer one; a replay is a no-op.

    fn x_of(ledger: &WebCanvasLedger, id: &str) -> Option<f32> {
        ledger.component(id).map(|c| c.rect.x)
    }

    /// Seed a component with a versioned upsert at rev 0, so a subsequent
    /// versioned move at rev ≥ 1 cleanly supersedes it (mirrors the native
    /// `seed_component`). Without a version the seed would be clock-assigned a rev
    /// that can outrun a peer move's rev and wrongly drop it — that mixing only
    /// happens in tests; over the wire every agent patch is unversioned and
    /// clock-ordered by arrival.
    fn seed_entry(canvas: &str, id: &str) -> CanvasPatchEntry {
        let mut e = entry(canvas, upsert(id, "card", json!({})));
        e.envelope.actor = ActorRef { kind: "system".to_string(), id: None, label: None };
        e.envelope.version = Some(ComponentVersion::new(0, ActorId::new("system")));
        e
    }

    #[test]
    fn two_concurrent_patches_to_same_component_converge_regardless_of_order() {
        // Operator and agent both move brief-1 at the same logical rev (genuinely
        // concurrent). "human:operator" > "agent:sage" → operator wins, on both
        // arrival orders.
        let seed = seed_entry("canvas:main", "brief-1");
        let op = versioned_move_entry("canvas:main", "brief-1", 100.0, 1, "human", "operator");
        let ag = versioned_move_entry("canvas:main", "brief-1", 900.0, 1, "agent", "sage");

        let a = WebCanvasLedger::from_entries(&[seed.clone(), op.clone(), ag.clone()]);
        let b = WebCanvasLedger::from_entries(&[seed, ag, op]);

        assert_eq!(x_of(&a, "brief-1"), x_of(&b, "brief-1"), "replicas converge");
        assert_eq!(x_of(&a, "brief-1"), Some(100.0), "operator wins the tie deterministically");
    }

    #[test]
    fn concurrent_patches_to_different_components_both_land() {
        let entries = vec![
            seed_entry("canvas:main", "card-a"),
            seed_entry("canvas:main", "card-b"),
            versioned_move_entry("canvas:main", "card-a", 50.0, 1, "human", "operator"),
            versioned_move_entry("canvas:main", "card-b", 60.0, 1, "agent", "sage"),
        ];
        let ledger = WebCanvasLedger::from_entries(&entries);
        assert_eq!(x_of(&ledger, "card-a"), Some(50.0), "operator's card-a landed");
        assert_eq!(x_of(&ledger, "card-b"), Some(60.0), "agent's card-b landed");
    }

    #[test]
    fn stale_patch_cannot_stomp_a_newer_one() {
        // Newer write (rev 2) then a stale older write (rev 1) for the same
        // component arrives late. The stale one must be dropped.
        let entries = vec![
            seed_entry("canvas:main", "c1"),
            versioned_move_entry("canvas:main", "c1", 200.0, 2, "agent", "sage"),
            versioned_move_entry("canvas:main", "c1", 10.0, 1, "human", "operator"),
        ];
        let ledger = WebCanvasLedger::from_entries(&entries);
        assert_eq!(
            x_of(&ledger, "c1"),
            Some(200.0),
            "the stale lower-rev patch did not overwrite the newer one",
        );
    }

    #[test]
    fn replayed_versioned_patch_is_idempotent() {
        let mv = versioned_move_entry("canvas:main", "c1", 42.0, 5, "agent", "sage");
        let entries = vec![
            seed_entry("canvas:main", "c1"),
            mv.clone(),
            mv, // exact redelivery
        ];
        let ledger = WebCanvasLedger::from_entries(&entries);
        assert_eq!(x_of(&ledger, "c1"), Some(42.0), "a replay is a no-op, not a re-apply");
    }

    #[test]
    fn unversioned_sequential_moves_to_same_component_still_apply_in_order() {
        // The live daemon path: agent patches arrive unversioned. The ledger
        // stamps each from its clock, so a later move (higher assigned rev)
        // supersedes the earlier — the last write to a component wins, as before.
        let entries = vec![
            entry("canvas:main", upsert("c1", "card", json!({}))),
            entry(
                "canvas:main",
                SurfacePatch::MoveComponent { component_id: ComponentId::new("c1"), x: 1.0, y: 0.0 },
            ),
            entry(
                "canvas:main",
                SurfacePatch::MoveComponent { component_id: ComponentId::new("c1"), x: 2.0, y: 0.0 },
            ),
        ];
        let ledger = WebCanvasLedger::from_entries(&entries);
        assert_eq!(x_of(&ledger, "c1"), Some(2.0), "the later unversioned move wins");
    }

    #[test]
    fn merge_is_per_canvas_in_the_multi_ledger() {
        // A same-id component on two different canvases must not contend: each
        // canvas has its own merge state, so both moves land on their own canvas.
        let entries = vec![
            seed_entry("canvas:a", "dup"),
            seed_entry("canvas:b", "dup"),
            versioned_move_entry("canvas:a", "dup", 111.0, 1, "human", "operator"),
            versioned_move_entry("canvas:b", "dup", 222.0, 1, "agent", "sage"),
        ];
        let multi = MultiCanvasLedger::from_entries(&entries, None);
        assert_eq!(
            multi.canvas("canvas:a").unwrap().component("dup").unwrap().rect.x,
            111.0,
        );
        assert_eq!(
            multi.canvas("canvas:b").unwrap().component("dup").unwrap().rect.x,
            222.0,
        );
    }
}
