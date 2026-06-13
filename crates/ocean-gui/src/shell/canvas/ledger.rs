//! The native [`CanvasLedger`] — agent-native surface state for the GPUI face.
//!
//! Implements gpui_masterbuild.md §5 (domain model) and §6 (placement rules),
//! Slice 4. The ledger:
//!
//! - stores visible surface state (`components`, `edges`, `selection`, `viewport`),
//! - allocates x/y placement and avoids collisions (the app owns placement),
//! - keeps component and edge ids stable,
//! - bumps `revision` on every mutation,
//! - records every applied patch in `patch_log` (undo/redo + sync foundation),
//! - exposes a compact, stable context to the next agent turn.
//!
//! No rendering lives here — that is Slice 5.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::layout::{
    DEFAULT_COMPONENT_HEIGHT, DEFAULT_COMPONENT_WIDTH, LayoutEngine, next_available_slot,
};
use super::patch::{
    ActorId, ActorRef, CanvasComponentPatch, CanvasEdgePatch, CanvasId, CanvasMergeState,
    ComponentId, ComponentVersion, EdgeId, Endpoint, FocusTarget, LamportClock, LayoutStrategy,
    LayoutTarget, MergeDecision, PatchId, Rect, SurfaceId, SurfacePatch, SurfacePatchEnvelope,
    Viewport,
};

// ---------------------------------------------------------------------------
// Domain enums (§5)
// ---------------------------------------------------------------------------

/// The interaction mode of a canvas. Mirrors the surface modes the agent reasons
/// about; kept as an open-ish enum with a sensible default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanvasMode {
    /// Free-form board (default).
    #[default]
    Freeform,
    /// Workflow / node-graph builder.
    Workflow,
    /// Kanban-style lanes.
    Kanban,
    /// Storyboard frames.
    Storyboard,
}

/// The component kinds the canvas can hold (§5). Templates (`brief_card`,
/// `workflow_node`, …) are layered on top via `kind`-string + content, but the
/// structural primitive is always one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Card,
    TextBlock,
    Frame,
    Node,
    Port,
    EdgeLabel,
    Lane,
    Table,
    MediaSlot,
    Stat,
}

impl ComponentKind {
    /// Map a patch `kind` string (which may be a template name like `brief_card`)
    /// onto a structural [`ComponentKind`]. Unknown / template names fall back to
    /// [`ComponentKind::Card`] — the agent's exact `kind` string is preserved on
    /// the component's `template` field, so nothing is lost.
    pub fn from_patch_kind(kind: &str) -> Self {
        match kind {
            "card" => Self::Card,
            "text_block" | "textblock" => Self::TextBlock,
            "frame" => Self::Frame,
            "node" => Self::Node,
            "port" => Self::Port,
            "edge_label" | "edgelabel" => Self::EdgeLabel,
            "lane" => Self::Lane,
            "table" => Self::Table,
            "media_slot" | "mediaslot" => Self::MediaSlot,
            "stat" => Self::Stat,
            // Templates: brief_card / kanban_column / storyboard_frame / etc.
            k if k.ends_with("_card") => Self::Card,
            k if k.ends_with("_node") => Self::Node,
            k if k.ends_with("_column") || k.ends_with("_lane") => Self::Lane,
            k if k.ends_with("_frame") => Self::Frame,
            _ => Self::Card,
        }
    }
}

/// Semantic of an edge between two endpoints (§5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Flow,
    Dependency,
    Reference,
    /// Any edge kind not in the known set, carried as its raw name.
    #[serde(untagged)]
    Other(String),
}

impl EdgeKind {
    fn from_opt(kind: Option<String>) -> Self {
        match kind.as_deref() {
            None => Self::Reference,
            Some("flow") => Self::Flow,
            Some("dependency") => Self::Dependency,
            Some("reference") => Self::Reference,
            Some(other) => Self::Other(other.to_string()),
        }
    }
}

/// How an edge is routed for rendering. The data layer keeps this abstract;
/// concrete waypoints arrive with the renderer slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRoute {
    /// Straight line between endpoints (default).
    #[default]
    Straight,
    /// Orthogonal / right-angle routing.
    Orthogonal,
    /// Smooth bezier.
    Bezier,
}

/// A connection point on a component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

// ---------------------------------------------------------------------------
// Component + edge (§5)
// ---------------------------------------------------------------------------

/// A placed component on the canvas (§5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasComponent {
    pub id: ComponentId,
    pub kind: ComponentKind,
    /// The agent's original `kind` string (e.g. `brief_card`), preserved so the
    /// renderer can pick a template even when `kind` collapsed to a primitive.
    pub template: String,
    pub rect: Rect,
    pub z_index: i32,
    pub content: Value,
    pub ports: Vec<Port>,
    pub children: Vec<ComponentId>,
    pub metadata: Value,
    pub created_by: ActorRef,
    pub updated_by: ActorRef,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// An edge between two endpoints (§5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasEdge {
    pub id: EdgeId,
    pub from: Endpoint,
    pub to: Endpoint,
    pub kind: EdgeKind,
    pub label: Option<String>,
    pub route: EdgeRoute,
    pub metadata: Value,
}

/// Selection state of the canvas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SelectionState {
    pub component_ids: Vec<ComponentId>,
    #[serde(default)]
    pub edge_ids: Vec<EdgeId>,
}

// ---------------------------------------------------------------------------
// Ledger (§5)
// ---------------------------------------------------------------------------

/// The authoritative, in-memory state of one canvas (§5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasLedger {
    pub canvas_id: CanvasId,
    pub session_id: String,
    pub revision: u64,
    pub mode: CanvasMode,
    pub viewport: Viewport,
    pub components: IndexMap<ComponentId, CanvasComponent>,
    pub edges: IndexMap<EdgeId, CanvasEdge>,
    pub selection: SelectionState,
    pub patch_log: Vec<SurfacePatchEnvelope>,
    pub metadata: Value,
    /// Per-component version vector for the convergent merge (OCEAN-270). The
    /// ledger is the merge point where the operator's local edits and the agent's
    /// streamed patches meet, so the version state lives here. `#[serde(default)]`
    /// lets a snapshot written before this field landed still load (it resumes as
    /// empty and re-seeds from the replayed log).
    #[serde(default)]
    pub merge_state: CanvasMergeState,
    /// This ledger's Lamport clock. Ticks on a local (operator) edit and observes
    /// the revision on each incoming versioned patch, so fresh local writes are
    /// always strictly greater than anything seen. One clock per canvas — no
    /// split-brain.
    #[serde(default)]
    pub clock: LamportClock,
}

impl CanvasLedger {
    /// Create an empty ledger for a canvas within a session.
    pub fn new(
        canvas_id: impl Into<CanvasId>,
        session_id: impl Into<String>,
        mode: CanvasMode,
    ) -> Self {
        Self {
            canvas_id: canvas_id.into(),
            session_id: session_id.into(),
            revision: 0,
            mode,
            viewport: Viewport::default(),
            components: IndexMap::new(),
            edges: IndexMap::new(),
            selection: SelectionState::default(),
            patch_log: Vec::new(),
            metadata: json!({ "grid_size": 24 }),
            merge_state: CanvasMergeState::new(),
            clock: LamportClock::new(),
        }
    }

    /// Apply a single **local** [`SurfacePatch`] — an edit that originated on this
    /// surface (an operator drag/resize via the [`LedgerSink`], or app-driven
    /// selection). Bumps `revision`, records the patch in the log, and stamps
    /// `actor`/`created_at_ms` onto the log envelope and any touched component.
    ///
    /// [`LedgerSink`]: super::render::LedgerSink
    ///
    /// Convergent merge (OCEAN-270): if the patch contends for a single component
    /// ([`SurfacePatch::target_component`]), this is the **local-edit branch** —
    /// it ticks the Lamport clock and stamps a [`ComponentVersion`] for the
    /// originating actor, then records that winning version. A local edit always
    /// wins at the moment it's made (its `rev` is strictly greater than anything
    /// the clock has seen), so it never has to be gated; a later remote patch is
    /// what gets compared against it.
    ///
    /// Returns the [`ComponentId`]s touched by this patch (for the tool result's
    /// `component_ids`).
    pub fn apply_patch(
        &mut self,
        patch: SurfacePatch,
        actor: ActorRef,
        created_at_ms: i64,
    ) -> Vec<ComponentId> {
        let touched = self.apply_inner(&patch, &actor, created_at_ms);

        // Stamp a converging version for a single-component edit. A local edit
        // ticks the clock so it's strictly greater than anything seen so far —
        // it always wins locally, and the stamp lets a peer/replay resolve order.
        let version = patch.target_component().map(|id| {
            let rev = self.clock.tick();
            let v = ComponentVersion::new(rev, ActorId::from_actor(&actor));
            self.merge_state.merge(id, v.clone());
            v
        });

        self.revision += 1;
        self.patch_log.push(SurfacePatchEnvelope {
            patch_id: PatchId::new(format!("{}@{}", self.canvas_id, self.revision)),
            session_id: self.session_id.clone(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: self.canvas_id.clone(),
            actor,
            created_at_ms,
            patch,
            version,
        });
        touched
    }

    /// Apply a **remote** patch envelope — one that arrived over the wire (an
    /// agent patch streamed via the daemon `SurfacePatch` event), through the
    /// convergent merge (OCEAN-270).
    ///
    /// This is the merge point the doc specifies (`OCEAN_CANVAS_CONVERGENT_MERGE.md`
    /// §"Surface integration"): the daemon relays agent patches with `version: None`,
    /// so the surface ledger is where the version is assigned and the merge runs.
    /// For a patch that contends for a single component:
    ///
    /// - If the envelope **carries** a version, `clock.observe(rev)` (jump past it)
    ///   then `merge`: [`Applied`](MergeDecision::Applied) → apply, advancing the
    ///   stored version; [`Superseded`](MergeDecision::Superseded) → **skip** (a
    ///   higher version already won — this is how a stale/out-of-order patch is
    ///   dropped and both replicas converge).
    /// - If the envelope has **no** version (the live daemon path today), the
    ///   surface assigns one: `clock.tick()` stamps the incoming actor, so the
    ///   agent patch gets a deterministic place in the per-component order. A
    ///   freshly-stamped tick is strictly greater than history, so a normal
    ///   single-writer turn applies as before; concurrent operator edits at the
    ///   same logical rev are resolved by the actor tiebreak.
    ///
    /// Patches that don't target a single component apply directly (never gated).
    /// Returns the touched [`ComponentId`]s, or empty when the patch was superseded.
    pub fn apply_remote_patch(&mut self, envelope: SurfacePatchEnvelope) -> Vec<ComponentId> {
        let SurfacePatchEnvelope {
            actor,
            created_at_ms,
            patch,
            version,
            ..
        } = envelope;

        // Resolve the merge decision + the version to record for a single-component
        // edit. `None` here means "apply directly, no merge gate".
        let stamped: Option<ComponentVersion> = match patch.target_component() {
            Some(id) => {
                let incoming = match version {
                    // The envelope already carries a version: fold its rev into the
                    // clock so future local ticks outrun it, then merge by it.
                    Some(v) => {
                        self.clock.observe(v.rev);
                        v
                    }
                    // No version (daemon relays None today): the surface IS the
                    // merge point, so assign one from this canvas's clock.
                    None => ComponentVersion::new(self.clock.tick(), ActorId::from_actor(&actor)),
                };
                match self.merge_state.merge(id, incoming.clone()) {
                    MergeDecision::Applied => Some(incoming),
                    // Superseded: a higher version already won. Drop the patch
                    // entirely — do not mutate components, bump revision, or log it.
                    MergeDecision::Superseded => return Vec::new(),
                }
            }
            // View-state / edge / multi-component op: not gated by the merge.
            None => None,
        };

        let touched = self.apply_inner(&patch, &actor, created_at_ms);
        self.revision += 1;
        self.patch_log.push(SurfacePatchEnvelope {
            patch_id: PatchId::new(format!("{}@{}", self.canvas_id, self.revision)),
            session_id: self.session_id.clone(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: self.canvas_id.clone(),
            actor,
            created_at_ms,
            patch,
            version: stamped,
        });
        touched
    }

    fn apply_inner(
        &mut self,
        patch: &SurfacePatch,
        actor: &ActorRef,
        now: i64,
    ) -> Vec<ComponentId> {
        match patch {
            SurfacePatch::UpsertComponent { component } => {
                let id = self.upsert_component(component.clone(), actor.clone(), now);
                vec![id]
            }
            SurfacePatch::MoveComponent { component_id, x, y } => {
                if let Some(c) = self.components.get_mut(component_id) {
                    c.rect.x = *x;
                    c.rect.y = *y;
                    c.updated_by = actor.clone();
                    c.updated_at_ms = now;
                    vec![component_id.clone()]
                } else {
                    vec![]
                }
            }
            SurfacePatch::ResizeComponent {
                component_id,
                width,
                height,
            } => {
                if let Some(c) = self.components.get_mut(component_id) {
                    c.rect.w = *width;
                    c.rect.h = *height;
                    c.updated_by = actor.clone();
                    c.updated_at_ms = now;
                    vec![component_id.clone()]
                } else {
                    vec![]
                }
            }
            SurfacePatch::DeleteComponent { component_id } => {
                let removed = self.components.shift_remove(component_id).is_some();
                if removed {
                    // Drop edges that referenced the deleted component.
                    self.edges.retain(|_, e| {
                        &e.from.component_id != component_id && &e.to.component_id != component_id
                    });
                    self.selection.component_ids.retain(|c| c != component_id);
                    vec![component_id.clone()]
                } else {
                    vec![]
                }
            }
            SurfacePatch::Connect { edge } => {
                self.connect(edge.clone());
                vec![edge.from.component_id.clone(), edge.to.component_id.clone()]
            }
            SurfacePatch::Disconnect { edge_id } => {
                self.edges.shift_remove(edge_id);
                self.selection.edge_ids.retain(|e| e != edge_id);
                vec![]
            }
            SurfacePatch::Focus { target } => {
                self.apply_focus(target);
                self.focus_touched(target)
            }
            SurfacePatch::Select { ids } => {
                self.selection.component_ids = ids.clone();
                ids.clone()
            }
            SurfacePatch::SetViewport { viewport } => {
                self.viewport = *viewport;
                vec![]
            }
            SurfacePatch::Layout { target, strategy } => self.apply_layout(target, strategy, now),
            SurfacePatch::Group { frame_id, children } => {
                self.apply_group(frame_id, children, now);
                let mut touched = vec![frame_id.clone()];
                touched.extend(children.iter().cloned());
                touched
            }
        }
    }

    /// Upsert one component, allocating placement when the patch omits a `rect`
    /// (§6: app owns placement, agents never solve collisions). Returns the id.
    fn upsert_component(
        &mut self,
        patch: CanvasComponentPatch,
        actor: ActorRef,
        now: i64,
    ) -> ComponentId {
        let id = patch.id.clone();

        // Resolve placement.
        let rect = match patch.rect {
            Some(r) => r,
            None => {
                // Reuse the existing rect on update; otherwise allocate a slot.
                if let Some(existing) = self.components.get(&id) {
                    existing.rect
                } else {
                    self.allocate_slot(DEFAULT_COMPONENT_WIDTH, DEFAULT_COMPONENT_HEIGHT)
                }
            }
        };

        let kind = ComponentKind::from_patch_kind(&patch.kind);

        if let Some(existing) = self.components.get_mut(&id) {
            // Update in place, preserving creation provenance.
            existing.kind = kind;
            existing.template = patch.kind;
            existing.rect = rect;
            if let Some(z) = patch.z_index {
                existing.z_index = z;
            }
            if !patch.content.is_null() {
                existing.content = patch.content;
            }
            if !patch.metadata.is_null() {
                existing.metadata = patch.metadata;
            }
            existing.updated_by = actor;
            existing.updated_at_ms = now;
        } else {
            let component = CanvasComponent {
                id: id.clone(),
                kind,
                template: patch.kind,
                rect,
                z_index: patch.z_index.unwrap_or(0),
                content: patch.content,
                ports: Vec::new(),
                children: Vec::new(),
                metadata: patch.metadata,
                created_by: actor.clone(),
                updated_by: actor,
                created_at_ms: now,
                updated_at_ms: now,
            };
            self.components.insert(id.clone(), component);
        }
        id
    }

    fn connect(&mut self, patch: CanvasEdgePatch) {
        let edge = CanvasEdge {
            id: patch.id.clone(),
            from: patch.from,
            to: patch.to,
            kind: EdgeKind::from_opt(patch.kind),
            label: patch.label,
            route: EdgeRoute::default(),
            metadata: patch.metadata,
        };
        self.edges.insert(patch.id, edge);
    }

    fn apply_focus(&mut self, target: &FocusTarget) {
        match target {
            FocusTarget::Component { component_id } => {
                self.selection.component_ids = vec![component_id.clone()];
            }
            FocusTarget::Edge { edge_id } => {
                self.selection.edge_ids = vec![edge_id.clone()];
            }
            FocusTarget::Canvas => {
                // Fit-to-content is a viewport concern handled by the renderer;
                // at the data layer we simply clear selection.
                self.selection = SelectionState::default();
            }
        }
    }

    fn focus_touched(&self, target: &FocusTarget) -> Vec<ComponentId> {
        match target {
            FocusTarget::Component { component_id } => vec![component_id.clone()],
            _ => vec![],
        }
    }

    fn apply_group(&mut self, frame_id: &ComponentId, children: &[ComponentId], now: i64) {
        if let Some(frame) = self.components.get_mut(frame_id) {
            for child in children {
                if !frame.children.contains(child) {
                    frame.children.push(child.clone());
                }
            }
            frame.updated_at_ms = now;
        }
    }

    fn apply_layout(
        &mut self,
        target: &LayoutTarget,
        strategy: &LayoutStrategy,
        now: i64,
    ) -> Vec<ComponentId> {
        // Resolve the components in scope, in insertion order.
        let ids: Vec<ComponentId> = match target {
            LayoutTarget::Canvas => self.components.keys().cloned().collect(),
            LayoutTarget::Component { component_id } => self
                .components
                .get(component_id)
                .map(|c| c.children.clone())
                .unwrap_or_default(),
            LayoutTarget::Components { ids } => ids.clone(),
        };

        let refs: Vec<&CanvasComponent> = ids
            .iter()
            .filter_map(|id| self.components.get(id))
            .collect();

        let placements = match strategy {
            LayoutStrategy::Grid | LayoutStrategy::Graph | LayoutStrategy::Tree => {
                let columns = (refs.len() as f32).sqrt().ceil() as usize;
                LayoutEngine::grid(&refs, columns)
            }
            LayoutStrategy::Row => LayoutEngine::row(&refs),
            LayoutStrategy::Column | LayoutStrategy::Stack => LayoutEngine::column(&refs),
            LayoutStrategy::Other(_) => LayoutEngine::grid(&refs, 1),
        };

        let mut touched = Vec::with_capacity(placements.len());
        for (id, rect) in placements {
            if let Some(c) = self.components.get_mut(&id) {
                c.rect = rect;
                c.updated_at_ms = now;
                touched.push(id);
            }
        }
        touched
    }

    /// Allocate a non-overlapping slot of `width`×`height` against the current
    /// component set (§6 "next available slot"). Falls back to the origin if the
    /// bounded scan is somehow exhausted.
    pub fn allocate_slot(&self, width: f32, height: f32) -> Rect {
        let occupied: Vec<Rect> = self.components.values().map(|c| c.rect).collect();
        next_available_slot(&occupied, width, height)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, width, height))
    }

    /// Convenience accessor used by tests and callers.
    pub fn component(&self, id: &ComponentId) -> Option<&CanvasComponent> {
        self.components.get(id)
    }

    /// Convenience accessor for an edge.
    pub fn edge(&self, id: &EdgeId) -> Option<&CanvasEdge> {
        self.edges.get(id)
    }

    // -----------------------------------------------------------------------
    // Compact context (§5, consumed by Slice 7)
    // -----------------------------------------------------------------------

    /// Produce a compact, stable serialization of the ledger for injection into
    /// the next agent turn. Includes component ids + kinds + rects, edge
    /// summaries, selection, viewport, mode, and revision — everything the agent
    /// needs to reason about existing surface state before emitting patches, and
    /// nothing heavy (no patch log, no per-component provenance, no full content).
    ///
    /// Stable ordering: components and edges follow insertion order
    /// (`IndexMap`), so the same ledger always serializes identically.
    pub fn compact_context(&self) -> CompactCanvasContext {
        CompactCanvasContext {
            canvas_id: self.canvas_id.clone(),
            revision: self.revision,
            mode: self.mode,
            viewport: self.viewport,
            components: self
                .components
                .values()
                .map(|c| CompactComponent {
                    id: c.id.clone(),
                    kind: c.template.clone(),
                    rect: c.rect,
                    title: c
                        .content
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect(),
            edges: self
                .edges
                .values()
                .map(|e| CompactEdge {
                    id: e.id.clone(),
                    from: e.from.component_id.clone(),
                    to: e.to.component_id.clone(),
                })
                .collect(),
            selection: self.selection.component_ids.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Compact context payloads
// ---------------------------------------------------------------------------

/// Compact, agent-facing snapshot of one canvas (output of
/// [`CanvasLedger::compact_context`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactCanvasContext {
    pub canvas_id: CanvasId,
    pub revision: u64,
    pub mode: CanvasMode,
    pub viewport: Viewport,
    pub components: Vec<CompactComponent>,
    pub edges: Vec<CompactEdge>,
    pub selection: Vec<ComponentId>,
}

impl CompactCanvasContext {
    /// JSON string suitable for embedding in a prompt.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// One component in the compact context: id, kind/template, rect, optional title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactComponent {
    pub id: ComponentId,
    pub kind: String,
    pub rect: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One edge in the compact context: id + endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactEdge {
    pub id: EdgeId,
    pub from: ComponentId,
    pub to: ComponentId,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger() -> CanvasLedger {
        CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform)
    }

    fn upsert(id: &str, rect: Option<Rect>) -> SurfacePatch {
        SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new(id),
                kind: "card".to_string(),
                rect,
                z_index: None,
                content: json!({ "title": id }),
                metadata: Value::Null,
            },
        }
    }

    #[test]
    fn upsert_adds_component_and_bumps_revision() {
        let mut l = ledger();
        assert_eq!(l.revision, 0);
        let touched = l.apply_patch(
            upsert("brief-1", Some(Rect::new(10.0, 20.0, 300.0, 200.0))),
            ActorRef::agent(Some("sage".into())),
            1_000,
        );
        assert_eq!(touched, vec![ComponentId::new("brief-1")]);
        assert_eq!(l.revision, 1);
        assert_eq!(l.components.len(), 1);
        let c = l.component(&ComponentId::new("brief-1")).unwrap();
        assert_eq!(c.rect, Rect::new(10.0, 20.0, 300.0, 200.0));
        assert_eq!(c.kind, ComponentKind::Card);
        assert_eq!(l.patch_log.len(), 1);
    }

    #[test]
    fn template_kind_maps_to_card_and_preserves_template_string() {
        let mut l = ledger();
        l.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("b1"),
                    kind: "brief_card".to_string(),
                    rect: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
                    z_index: None,
                    content: Value::Null,
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            0,
        );
        let c = l.component(&ComponentId::new("b1")).unwrap();
        assert_eq!(c.kind, ComponentKind::Card);
        assert_eq!(c.template, "brief_card");
    }

    #[test]
    fn move_resize_delete_mutate_correctly() {
        let mut l = ledger();
        let id = ComponentId::new("c1");
        l.apply_patch(
            upsert("c1", Some(Rect::new(0.0, 0.0, 100.0, 100.0))),
            ActorRef::system(),
            0,
        );

        l.apply_patch(
            SurfacePatch::MoveComponent {
                component_id: id.clone(),
                x: 50.0,
                y: 60.0,
            },
            ActorRef::system(),
            1,
        );
        assert_eq!(l.component(&id).unwrap().rect.x, 50.0);
        assert_eq!(l.component(&id).unwrap().rect.y, 60.0);

        l.apply_patch(
            SurfacePatch::ResizeComponent {
                component_id: id.clone(),
                width: 400.0,
                height: 300.0,
            },
            ActorRef::system(),
            2,
        );
        assert_eq!(l.component(&id).unwrap().rect.w, 400.0);
        assert_eq!(l.component(&id).unwrap().rect.h, 300.0);

        let rev_before = l.revision;
        l.apply_patch(
            SurfacePatch::DeleteComponent {
                component_id: id.clone(),
            },
            ActorRef::system(),
            3,
        );
        assert!(l.component(&id).is_none());
        assert_eq!(l.revision, rev_before + 1);
    }

    #[test]
    fn connect_adds_edge_and_delete_prunes_it() {
        let mut l = ledger();
        l.apply_patch(
            upsert("a", Some(Rect::new(0.0, 0.0, 10.0, 10.0))),
            ActorRef::system(),
            0,
        );
        l.apply_patch(
            upsert("b", Some(Rect::new(100.0, 0.0, 10.0, 10.0))),
            ActorRef::system(),
            0,
        );

        l.apply_patch(
            SurfacePatch::Connect {
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
                    kind: Some("flow".into()),
                    label: Some("then".into()),
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            1,
        );
        assert_eq!(l.edges.len(), 1);
        let e = l.edge(&EdgeId::new("e1")).unwrap();
        assert_eq!(e.kind, EdgeKind::Flow);

        // Deleting an endpoint component prunes the edge.
        l.apply_patch(
            SurfacePatch::DeleteComponent {
                component_id: ComponentId::new("a"),
            },
            ActorRef::system(),
            2,
        );
        assert_eq!(
            l.edges.len(),
            0,
            "edge should be pruned when an endpoint is deleted"
        );
    }

    #[test]
    fn upsert_without_coords_gets_a_deterministic_slot() {
        let mut l = ledger();
        l.apply_patch(upsert("x", None), ActorRef::system(), 0);
        let rect = l.component(&ComponentId::new("x")).unwrap().rect;
        // First allocation is the deterministic origin slot.
        assert!(
            rect.x > 0.0 && rect.y > 0.0,
            "slot should be a positive grid position"
        );

        // A second ledger in the same state allocates the same first slot.
        let mut l2 = ledger();
        l2.apply_patch(upsert("y", None), ActorRef::system(), 0);
        assert_eq!(
            rect,
            l2.component(&ComponentId::new("y")).unwrap().rect,
            "slot allocation must be deterministic"
        );
    }

    #[test]
    fn two_no_coord_upserts_do_not_overlap() {
        let mut l = ledger();
        l.apply_patch(upsert("one", None), ActorRef::system(), 0);
        l.apply_patch(upsert("two", None), ActorRef::system(), 0);
        let r1 = l.component(&ComponentId::new("one")).unwrap().rect;
        let r2 = l.component(&ComponentId::new("two")).unwrap().rect;
        assert!(
            !r1.intersects(&r2),
            "two auto-placed components must not overlap: {r1:?} vs {r2:?}"
        );
    }

    #[test]
    fn select_and_set_viewport_apply() {
        let mut l = ledger();
        l.apply_patch(upsert("c1", None), ActorRef::system(), 0);
        l.apply_patch(
            SurfacePatch::Select {
                ids: vec![ComponentId::new("c1")],
            },
            ActorRef::system(),
            1,
        );
        assert_eq!(l.selection.component_ids, vec![ComponentId::new("c1")]);

        l.apply_patch(
            SurfacePatch::SetViewport {
                viewport: Viewport {
                    x: 5.0,
                    y: 6.0,
                    zoom: 2.0,
                },
            },
            ActorRef::system(),
            2,
        );
        assert_eq!(
            l.viewport,
            Viewport {
                x: 5.0,
                y: 6.0,
                zoom: 2.0
            }
        );
    }

    #[test]
    fn layout_grid_repositions_components() {
        let mut l = ledger();
        for i in 0..4 {
            l.apply_patch(
                upsert(
                    &format!("c{i}"),
                    Some(Rect::new(999.0, 999.0, 100.0, 100.0)),
                ),
                ActorRef::system(),
                0,
            );
        }
        l.apply_patch(
            SurfacePatch::Layout {
                target: LayoutTarget::Canvas,
                strategy: LayoutStrategy::Grid,
            },
            ActorRef::system(),
            1,
        );
        // No two laid-out components overlap.
        let rects: Vec<Rect> = l.components.values().map(|c| c.rect).collect();
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                assert!(
                    !rects[i].intersects(&rects[j]),
                    "grid layout produced overlap"
                );
            }
        }
    }

    #[test]
    fn group_records_children() {
        let mut l = ledger();
        l.apply_patch(upsert("frame", None), ActorRef::system(), 0);
        l.apply_patch(upsert("child", None), ActorRef::system(), 0);
        l.apply_patch(
            SurfacePatch::Group {
                frame_id: ComponentId::new("frame"),
                children: vec![ComponentId::new("child")],
            },
            ActorRef::system(),
            1,
        );
        assert_eq!(
            l.component(&ComponentId::new("frame")).unwrap().children,
            vec![ComponentId::new("child")]
        );
    }

    #[test]
    fn compact_context_includes_ids_rects_and_edges() {
        let mut l = ledger();
        l.apply_patch(
            upsert("brief-1", Some(Rect::new(10.0, 20.0, 300.0, 200.0))),
            ActorRef::system(),
            0,
        );
        l.apply_patch(
            upsert("proposal-1", Some(Rect::new(400.0, 20.0, 300.0, 200.0))),
            ActorRef::system(),
            0,
        );
        l.apply_patch(
            SurfacePatch::Connect {
                edge: CanvasEdgePatch {
                    id: EdgeId::new("e1"),
                    from: Endpoint {
                        component_id: ComponentId::new("brief-1"),
                        port: None,
                    },
                    to: Endpoint {
                        component_id: ComponentId::new("proposal-1"),
                        port: None,
                    },
                    kind: None,
                    label: None,
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            0,
        );
        l.apply_patch(
            SurfacePatch::Select {
                ids: vec![ComponentId::new("brief-1")],
            },
            ActorRef::system(),
            0,
        );

        let ctx = l.compact_context();
        assert_eq!(ctx.canvas_id, CanvasId::new("canvas:main"));
        assert_eq!(ctx.components.len(), 2);

        let ids: Vec<&str> = ctx.components.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["brief-1", "proposal-1"], "stable insertion order");

        let first = &ctx.components[0];
        assert_eq!(first.rect, Rect::new(10.0, 20.0, 300.0, 200.0));
        assert_eq!(first.title.as_deref(), Some("brief-1"));

        assert_eq!(ctx.edges.len(), 1);
        assert_eq!(ctx.edges[0].from, ComponentId::new("brief-1"));
        assert_eq!(ctx.edges[0].to, ComponentId::new("proposal-1"));
        assert_eq!(ctx.selection, vec![ComponentId::new("brief-1")]);

        // The JSON form carries the ids and rect numbers.
        let s = ctx.to_json();
        assert!(s.contains("brief-1") && s.contains("proposal-1"));
        assert!(s.contains("\"x\":10") || s.contains("\"x\":10.0"));
    }

    #[test]
    fn ledger_roundtrips_through_json() {
        let mut l = ledger();
        l.apply_patch(upsert("c1", None), ActorRef::agent(Some("sage".into())), 42);
        let s = serde_json::to_string(&l).unwrap();
        let back: CanvasLedger = serde_json::from_str(&s).unwrap();
        assert_eq!(back, l);
    }

    // -----------------------------------------------------------------------
    // Convergent merge through the ledger (OCEAN-270). Mirrors the SDK's
    // end-to-end envelope tests, but drives the REAL apply paths the surface
    // uses: `apply_patch` for a local (operator) edit and `apply_remote_patch`
    // for a wire patch. The guarantee: two concurrent edits to the same
    // component converge to a deterministic result regardless of arrival order;
    // edits to different components both land; a stale patch can't stomp a newer.
    // -----------------------------------------------------------------------

    /// A move envelope carrying an explicit version, as if it arrived over the
    /// wire from another replica. `actor` is `"{kind}:{id}"`.
    fn versioned_move_env(
        id: &str,
        x: f32,
        rev: u64,
        kind: &str,
        actor_id: &str,
    ) -> SurfacePatchEnvelope {
        SurfacePatchEnvelope {
            patch_id: PatchId::new(format!("p:{id}@{rev}")),
            session_id: "s".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
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
        }
    }

    /// Seed a component so a subsequent move has something to move, without
    /// disturbing the merge order under test (uses a low rev via remote apply).
    fn seed_component(l: &mut CanvasLedger, id: &str) {
        l.apply_remote_patch(SurfacePatchEnvelope {
            patch_id: PatchId::new(format!("seed:{id}")),
            session_id: "s".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
            actor: ActorRef::system(),
            created_at_ms: 0,
            patch: upsert(id, Some(Rect::new(0.0, 0.0, 10.0, 10.0))),
            version: Some(ComponentVersion::new(0, ActorId::new("system"))),
        });
    }

    #[test]
    fn two_concurrent_remote_patches_to_same_component_converge_regardless_of_order() {
        // Operator and agent both move brief-1 at the same logical rev (genuinely
        // concurrent). "human:operator" > "agent:sage" → operator wins, on both
        // replicas, whichever order each applies them.
        let op = versioned_move_env("brief-1", 100.0, 1, "human", "operator");
        let ag = versioned_move_env("brief-1", 900.0, 1, "agent", "sage");

        let mut a = ledger();
        seed_component(&mut a, "brief-1");
        a.apply_remote_patch(op.clone());
        a.apply_remote_patch(ag.clone());

        let mut b = ledger();
        seed_component(&mut b, "brief-1");
        b.apply_remote_patch(ag);
        b.apply_remote_patch(op);

        let ax = a.component(&ComponentId::new("brief-1")).unwrap().rect.x;
        let bx = b.component(&ComponentId::new("brief-1")).unwrap().rect.x;
        assert_eq!(
            ax, bx,
            "both replicas converge to the same x regardless of order"
        );
        assert_eq!(
            ax, 100.0,
            "deterministic winner is the operator (higher actor id)"
        );
        // And the merge state agrees on the winning version.
        assert_eq!(
            a.merge_state.version(&ComponentId::new("brief-1")),
            b.merge_state.version(&ComponentId::new("brief-1")),
        );
    }

    #[test]
    fn concurrent_remote_patches_to_different_components_both_land() {
        let op = versioned_move_env("card-a", 50.0, 1, "human", "operator");
        let ag = versioned_move_env("card-b", 60.0, 1, "agent", "sage");

        let mut l = ledger();
        seed_component(&mut l, "card-a");
        seed_component(&mut l, "card-b");
        l.apply_remote_patch(op);
        l.apply_remote_patch(ag);

        assert_eq!(
            l.component(&ComponentId::new("card-a")).unwrap().rect.x,
            50.0
        );
        assert_eq!(
            l.component(&ComponentId::new("card-b")).unwrap().rect.x,
            60.0
        );
    }

    #[test]
    fn stale_remote_patch_cannot_stomp_a_newer_one() {
        // A newer write (rev 2) lands, then a stale older write (rev 1) for the
        // same component arrives late. The stale one must be dropped.
        let newer = versioned_move_env("c1", 200.0, 2, "agent", "sage");
        let stale = versioned_move_env("c1", 10.0, 1, "human", "operator");

        let mut l = ledger();
        seed_component(&mut l, "c1");
        let rev_after_newer = {
            l.apply_remote_patch(newer);
            l.revision
        };
        let touched = l.apply_remote_patch(stale);

        assert!(
            touched.is_empty(),
            "a superseded patch reports no touched components"
        );
        assert_eq!(
            l.component(&ComponentId::new("c1")).unwrap().rect.x,
            200.0,
            "the stale lower-rev patch did not overwrite the newer one",
        );
        assert_eq!(
            l.revision, rev_after_newer,
            "a dropped patch does not bump the revision or append to the log",
        );
    }

    #[test]
    fn replayed_remote_patch_is_idempotent() {
        let mv = versioned_move_env("c1", 42.0, 5, "agent", "sage");
        let mut l = ledger();
        seed_component(&mut l, "c1");
        l.apply_remote_patch(mv.clone());
        let rev_after_first = l.revision;
        let touched = l.apply_remote_patch(mv); // exact redelivery

        assert!(touched.is_empty(), "an exact replay is a no-op");
        assert_eq!(
            l.revision, rev_after_first,
            "replay does not bump the revision"
        );
        assert_eq!(l.component(&ComponentId::new("c1")).unwrap().rect.x, 42.0);
    }

    #[test]
    fn local_edit_beats_an_earlier_agent_patch_on_the_same_component() {
        // An agent patch lands (unversioned, the live daemon path → stamped from
        // the clock), then the operator drags the SAME card. The operator's local
        // edit ticks the clock to a strictly greater rev, so it wins.
        let mut l = ledger();
        seed_component(&mut l, "c1");
        // Agent move arrives over the wire with no version (daemon relays None).
        l.apply_remote_patch(SurfacePatchEnvelope {
            patch_id: PatchId::new("p:agent"),
            session_id: "s".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
            actor: ActorRef::agent(Some("sage".into())),
            created_at_ms: 1,
            patch: SurfacePatch::MoveComponent {
                component_id: ComponentId::new("c1"),
                x: 900.0,
                y: 0.0,
            },
            version: None,
        });
        // Operator drag (local edit through the sink path).
        l.apply_patch(
            SurfacePatch::MoveComponent {
                component_id: ComponentId::new("c1"),
                x: 123.0,
                y: 0.0,
            },
            ActorRef::human(Some("operator".into())),
            2,
        );

        assert_eq!(
            l.component(&ComponentId::new("c1")).unwrap().rect.x,
            123.0,
            "the later local operator edit wins over the earlier agent patch",
        );
        // And now a STALE agent redelivery at the old (lower) rev can't undo it.
        let winning_rev = l.merge_state.version(&ComponentId::new("c1")).unwrap().rev;
        l.apply_remote_patch(SurfacePatchEnvelope {
            patch_id: PatchId::new("p:agent-stale"),
            session_id: "s".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
            actor: ActorRef::agent(Some("sage".into())),
            created_at_ms: 1,
            patch: SurfacePatch::MoveComponent {
                component_id: ComponentId::new("c1"),
                x: 900.0,
                y: 0.0,
            },
            version: Some(ComponentVersion::new(1, ActorId::new("agent:sage"))),
        });
        assert_eq!(
            l.component(&ComponentId::new("c1")).unwrap().rect.x,
            123.0,
            "a stale agent patch below the operator's rev is dropped",
        );
        assert!(winning_rev >= 1);
    }

    #[test]
    fn local_edit_stamps_a_version_and_ticks_the_clock() {
        let mut l = ledger();
        l.apply_patch(
            upsert("c1", Some(Rect::new(0.0, 0.0, 10.0, 10.0))),
            ActorRef::human(Some("operator".into())),
            0,
        );
        let env = l.patch_log.last().unwrap();
        let v = env
            .version
            .as_ref()
            .expect("local component edit is versioned");
        assert_eq!(v.actor, ActorId::new("human:operator"));
        assert_eq!(v.rev, 1, "first local write ticks the clock to 1");
        assert!(l.clock.now() >= 1);
        // A view-state op (Select) is NOT versioned.
        l.apply_patch(
            SurfacePatch::Select {
                ids: vec![ComponentId::new("c1")],
            },
            ActorRef::human(Some("operator".into())),
            1,
        );
        assert!(l.patch_log.last().unwrap().version.is_none());
    }
}
