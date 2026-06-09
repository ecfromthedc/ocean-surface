//! Surface patch wire types, mirrored from
//! `ocean-agent-sdk::surface` (Slice 1).
//!
//! These are a structural copy of the SDK's serde contract so the GPUI canvas can
//! deserialize the exact JSON the runtime `surface_patch` tool emits and the
//! daemon streams, without taking a build dependency on the ocean-os workspace.
//! See the module docs in `mod.rs` for the rationale.
//!
//! The only intentional difference from the SDK is that envelopes carry
//! `session_id` as a `String` (ocean-surface's existing convention) rather than a
//! `uuid`-backed `AgentSessionId`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Identifiers — string-backed, transparent on the wire
// ---------------------------------------------------------------------------

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Construct from anything string-like.
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            /// Borrow the underlying string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
            /// Consume into the owned `String`.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// Identifies a *surface* — one client face onto a session (e.g. `gpui:local`).
    SurfaceId
);
string_id!(
    /// Identifies a *canvas* within a surface (e.g. `canvas:main`).
    CanvasId
);
string_id!(
    /// Identifies a *component* (card, node, frame, …) on a canvas.
    ComponentId
);
string_id!(
    /// Identifies a single emitted *patch*.
    PatchId
);
string_id!(
    /// Identifies an *edge* between two endpoints.
    EdgeId
);

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Axis-aligned rectangle in canvas space. All fields roundtrip as JSON numbers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// True if this rectangle overlaps `other` (touching edges do not count).
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && self.x + self.w > other.x
            && self.y < other.y + other.h
            && self.y + self.h > other.y
    }
}

/// Pan/zoom state of a canvas viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    #[serde(default = "Viewport::default_zoom")]
    pub zoom: f32,
}

impl Viewport {
    fn default_zoom() -> f32 {
        1.0
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Actor — who originated a patch
// ---------------------------------------------------------------------------

/// Reference to the actor that originated a patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    /// Coarse actor class, e.g. `"agent"`, `"human"`, `"system"`.
    pub kind: String,
    /// Optional stable id for the actor (agent name, user id, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional human-friendly label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ActorRef {
    /// An agent actor with an optional name.
    pub fn agent(id: impl Into<Option<String>>) -> Self {
        Self {
            kind: "agent".to_string(),
            id: id.into(),
            label: None,
        }
    }

    /// A human actor with an optional id.
    pub fn human(id: impl Into<Option<String>>) -> Self {
        Self {
            kind: "human".to_string(),
            id: id.into(),
            label: None,
        }
    }

    /// A system actor (the app itself, e.g. when it allocates placement).
    pub fn system() -> Self {
        Self {
            kind: "system".to_string(),
            id: None,
            label: None,
        }
    }
}

impl Default for ActorRef {
    fn default() -> Self {
        Self::system()
    }
}

// ---------------------------------------------------------------------------
// Patch payloads
// ---------------------------------------------------------------------------

/// Upsert payload for a component. `rect`/`content` are optional so an agent can
/// create a component and let the app allocate placement (placement rules §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasComponentPatch {
    pub id: ComponentId,
    /// Component kind or template name, e.g. `"card"`, `"brief_card"`.
    pub kind: String,
    /// Requested placement. If omitted the app allocates a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    /// Optional stacking order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_index: Option<i32>,
    /// Free-form content payload (title/body/etc). Defaults to `null`.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub content: Value,
    /// Free-form metadata that survives a roundtrip untouched.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Endpoint of an edge — either a bare component or a specific port on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub component_id: ComponentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

/// Create/update payload for an edge between two endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasEdgePatch {
    pub id: EdgeId,
    pub from: Endpoint,
    pub to: Endpoint,
    /// Edge kind/semantic, e.g. `"dependency"`, `"flow"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Target of a [`SurfacePatch::Focus`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusTarget {
    /// Focus a single component.
    Component { component_id: ComponentId },
    /// Focus an edge.
    Edge { edge_id: EdgeId },
    /// Focus the whole canvas / fit to content.
    Canvas,
}

/// Target of a [`SurfacePatch::Layout`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutTarget {
    /// Lay out the entire canvas.
    Canvas,
    /// Lay out the children of one container component.
    Component { component_id: ComponentId },
    /// Lay out an explicit set of components.
    Components { ids: Vec<ComponentId> },
}

/// Layout strategy. Open string set so new strategies can be added without
/// breaking the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutStrategy {
    Grid,
    Stack,
    Row,
    Column,
    Tree,
    Graph,
    /// Any strategy not in the known set, carried as its raw name.
    #[serde(untagged)]
    Other(String),
}

// ---------------------------------------------------------------------------
// Surface patch operation
// ---------------------------------------------------------------------------

/// A single structured mutation to an Ocean surface canvas.
///
/// Internally tagged on `"op"` with `snake_case` discriminants. The §6 minimal
/// JSON shape `{ "op": "upsert_component", "component": { … } }` deserializes
/// directly into [`SurfacePatch::UpsertComponent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SurfacePatch {
    /// Create or update a component.
    UpsertComponent { component: CanvasComponentPatch },
    /// Move a component to an absolute position.
    MoveComponent {
        component_id: ComponentId,
        x: f32,
        y: f32,
    },
    /// Resize a component.
    ResizeComponent {
        component_id: ComponentId,
        width: f32,
        height: f32,
    },
    /// Delete a component.
    DeleteComponent { component_id: ComponentId },
    /// Create or update an edge between two endpoints.
    Connect { edge: CanvasEdgePatch },
    /// Remove an edge.
    Disconnect { edge_id: EdgeId },
    /// Focus a target (component/edge/canvas).
    Focus { target: FocusTarget },
    /// Replace the current selection.
    Select { ids: Vec<ComponentId> },
    /// Set the viewport pan/zoom.
    SetViewport { viewport: Viewport },
    /// Run a layout strategy over a target.
    Layout {
        target: LayoutTarget,
        strategy: LayoutStrategy,
    },
    /// Group components under a frame.
    Group {
        frame_id: ComponentId,
        children: Vec<ComponentId>,
    },
}

impl SurfacePatch {
    /// The single component this patch contends for under the convergent merge
    /// (OCEAN-270), or `None` when the op doesn't last-write-wins one component.
    ///
    /// The per-component ops (`UpsertComponent` / `MoveComponent` /
    /// `ResizeComponent` / `DeleteComponent`) return their target; everything else
    /// returns `None` — `Connect`/`Disconnect` mutate an edge, `Select`/`Focus`/
    /// `SetViewport` are view state, and `Layout`/`Group` touch many components as
    /// a unit. `None`-target patches apply directly, never gated by the merge.
    pub fn target_component(&self) -> Option<&ComponentId> {
        match self {
            SurfacePatch::UpsertComponent { component } => Some(&component.id),
            SurfacePatch::MoveComponent { component_id, .. }
            | SurfacePatch::ResizeComponent { component_id, .. }
            | SurfacePatch::DeleteComponent { component_id } => Some(component_id),
            SurfacePatch::Connect { .. }
            | SurfacePatch::Disconnect { .. }
            | SurfacePatch::Focus { .. }
            | SurfacePatch::Select { .. }
            | SurfacePatch::SetViewport { .. }
            | SurfacePatch::Layout { .. }
            | SurfacePatch::Group { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Convergent-merge types (OCEAN-270) — mirror of
// `ocean-agent-sdk::surface_merge`.
//
// These let the operator and an agent edit the *same* component concurrently
// without clobbering each other: each per-component write carries a
// [`ComponentVersion`] (a Lamport-style revision + a stable [`ActorId`]) with a
// total order, and the ledger merges by that order. See `mod.rs` for the
// no-build-dependency mirror rationale; the JSON is identical to the SDK's.
// ---------------------------------------------------------------------------

/// A stable, orderable identity for whoever originated a write, derived from an
/// [`ActorRef`]. The operator (`human:operator`) and an agent (`agent:sage`) get
/// distinct, comparable ids; the string ordering is the deterministic tiebreak
/// when two concurrent writes share a revision.
///
/// `serde(transparent)`: on the wire an `ActorId` is just `"agent:sage"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorId(pub String);

impl ActorId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derive a stable id from an [`ActorRef`]: `"{kind}:{id}"` when the actor has
    /// an id (`"agent:sage"`, `"human:operator"`), else just the kind
    /// (`"system"`). Same kind+id always yields the same `ActorId`, which is what
    /// makes the tiebreak deterministic across replicas.
    pub fn from_actor(actor: &ActorRef) -> Self {
        match &actor.id {
            Some(id) => Self(format!("{}:{}", actor.kind, id)),
            None => Self(actor.kind.clone()),
        }
    }
}

impl From<&ActorRef> for ActorId {
    fn from(actor: &ActorRef) -> Self {
        Self::from_actor(actor)
    }
}

impl std::fmt::Display for ActorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The logical version stamp on one component's state: a Lamport-style revision
/// counter plus the [`ActorId`] that produced it.
///
/// Derived `Ord` is lexicographic over fields, so **`rev` must stay declared
/// before `actor`**: a higher `rev` always wins; on an equal `rev` (a genuinely
/// concurrent write) the higher `actor` string wins. Every replica computes the
/// same winner from the same pair, regardless of arrival order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ComponentVersion {
    /// Monotonic per-component logical revision. Compared first (field order
    /// matters for the derived `Ord`).
    pub rev: u64,
    /// The actor that authored this revision; the tiebreak on an equal `rev`.
    pub actor: ActorId,
}

impl ComponentVersion {
    pub fn new(rev: u64, actor: ActorId) -> Self {
        Self { rev, actor }
    }

    /// Whether `self` should supersede `other`: strictly greater in the
    /// `(rev, actor)` total order. This is the whole per-component merge decision.
    pub fn supersedes(&self, other: &ComponentVersion) -> bool {
        self > other
    }
}

/// A per-actor Lamport logical clock. `tick()` on a local write (increment and
/// return); `observe(remote_rev)` advances to `max(local, remote)` so the next
/// local tick is strictly greater than anything this actor has seen. Keeping it
/// per-actor is what lets two surfaces tick independently and still converge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LamportClock {
    counter: u64,
}

impl LamportClock {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// Start already advanced to `value` (e.g. resuming a persisted canvas whose
    /// log already reached some revision).
    pub fn at(value: u64) -> Self {
        Self { counter: value }
    }

    /// The current logical time without advancing it.
    pub fn now(&self) -> u64 {
        self.counter
    }

    /// Advance for a local write and return the new logical time.
    pub fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Fold in a revision observed from another actor, advancing to at least that
    /// value. Returns the new logical time.
    pub fn observe(&mut self, remote_rev: u64) -> u64 {
        self.counter = self.counter.max(remote_rev);
        self.counter
    }
}

/// What happened when a versioned write was offered to [`CanvasMergeState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDecision {
    /// The write won and the stored version advanced — apply the patch.
    Applied,
    /// The write lost (concurrent-but-lower, stale, or a replay) — skip the patch.
    /// Skipping is how both replicas converge on the same winner regardless of
    /// order.
    Superseded,
}

impl MergeDecision {
    /// Did this write win (and so should be applied)?
    pub fn applied(self) -> bool {
        matches!(self, MergeDecision::Applied)
    }
}

/// The per-canvas **version vector**: a map from [`ComponentId`] to the current
/// winning [`ComponentVersion`]. The ledger keeps this beside its components so
/// an out-of-order or concurrent write resolves deterministically.
///
/// [`merge`](CanvasMergeState::merge) accepts a write **iff** it strictly
/// supersedes the stored one (`max` over the total order), so the end state is
/// identical no matter what order writes are fed in. Different components live
/// under different keys, so concurrent writes to different components both land.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasMergeState {
    /// Per-component winning versions. A `BTreeMap` gives a deterministic,
    /// canonical-key serialization (the merge result is order-independent anyway).
    versions: std::collections::BTreeMap<ComponentId, ComponentVersion>,
}

impl CanvasMergeState {
    pub fn new() -> Self {
        Self {
            versions: std::collections::BTreeMap::new(),
        }
    }

    /// The current winning version for a component, if any write has landed.
    pub fn version(&self, id: &ComponentId) -> Option<&ComponentVersion> {
        self.versions.get(id)
    }

    /// Number of components tracked.
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// Offer a versioned write for `id` and get the deterministic decision:
    /// first write for a component always [`Applied`](MergeDecision::Applied);
    /// otherwise `Applied` iff `incoming.supersedes(stored)`, else
    /// [`Superseded`](MergeDecision::Superseded). On `Applied` the stored version
    /// advances to `incoming`. This is the single commutative op the convergence
    /// guarantee rests on.
    pub fn merge(&mut self, id: &ComponentId, incoming: ComponentVersion) -> MergeDecision {
        match self.versions.get(id) {
            Some(stored) if !incoming.supersedes(stored) => MergeDecision::Superseded,
            _ => {
                self.versions.insert(id.clone(), incoming);
                MergeDecision::Applied
            }
        }
    }

    /// The highest revision tracked across all components — used to seed a
    /// [`LamportClock`] past the whole replayed history when resuming a canvas.
    pub fn max_rev(&self) -> u64 {
        self.versions.values().map(|v| v.rev).max().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A patch plus the session/surface/canvas/actor context needed to route and
/// persist it. Appended to the ledger's patch log.
///
/// `session_id` is a plain `String` here (ocean-surface convention) where the SDK
/// uses a `uuid`-backed `AgentSessionId`; the JSON shape is identical because the
/// SDK newtype is `serde(transparent)` over its string form.
///
/// `version` (OCEAN-270) is the optional per-component [`ComponentVersion`] the
/// convergent merge stamps at apply time. `#[serde(default, skip_serializing_if)]`
/// keeps it **additive**: producers that predate the merge layer (and the daemon,
/// which relays `None`) omit it on the wire; the surface ledger is the merge point
/// that assigns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfacePatchEnvelope {
    pub patch_id: PatchId,
    pub session_id: String,
    pub surface_id: SurfaceId,
    pub canvas_id: CanvasId,
    pub actor: ActorRef,
    pub created_at_ms: i64,
    pub patch: SurfacePatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<ComponentVersion>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The EXACT `upsert_component` JSON from gpui_masterbuild.md §6 must
    /// deserialize into `SurfacePatch::UpsertComponent`. This pins the consumer
    /// side of the wire contract against the SDK producer.
    #[test]
    fn deserializes_section_6_upsert_component() {
        let raw = json!({
            "op": "upsert_component",
            "component": {
                "id": "brief-1",
                "kind": "brief_card",
                "rect": { "x": 420, "y": 120, "w": 320, "h": 220 },
                "content": { "title": "Sales Brief", "body": "Draft brief" },
                "metadata": { "source": "longhouse.sales" }
            }
        });

        let patch: SurfacePatch = serde_json::from_value(raw).expect("deserialize §6 shape");
        let SurfacePatch::UpsertComponent { component } = patch else {
            panic!("expected UpsertComponent");
        };
        assert_eq!(component.id, ComponentId::new("brief-1"));
        assert_eq!(component.kind, "brief_card");
        let rect = component.rect.expect("rect present");
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (420.0, 120.0, 320.0, 220.0));
        assert_eq!(component.content["title"], "Sales Brief");
        assert_eq!(component.metadata["source"], "longhouse.sales");
    }

    #[test]
    fn ids_are_transparent_strings() {
        let id = ComponentId::new("brief-1");
        assert_eq!(serde_json::to_value(&id).unwrap(), json!("brief-1"));
    }

    #[test]
    fn move_op_is_snake_case_with_numeric_geometry() {
        let v = serde_json::to_value(SurfacePatch::MoveComponent {
            component_id: ComponentId::new("n1"),
            x: 10.0,
            y: 20.0,
        })
        .unwrap();
        assert_eq!(v["op"], "move_component");
        assert!(v["x"].is_number() && v["y"].is_number());
    }

    #[test]
    fn unknown_layout_strategy_is_other() {
        let raw = json!({ "op": "layout", "target": "canvas", "strategy": "elk_layered" });
        let patch: SurfacePatch = serde_json::from_value(raw).unwrap();
        let SurfacePatch::Layout { strategy, .. } = patch else {
            panic!("expected Layout");
        };
        assert_eq!(strategy, LayoutStrategy::Other("elk_layered".to_string()));
    }

    // -----------------------------------------------------------------------
    // Convergent merge (OCEAN-270) — mirrors ocean-agent-sdk::surface_merge.
    // -----------------------------------------------------------------------

    fn agent(name: &str) -> ActorId {
        ActorId::from_actor(&ActorRef::agent(Some(name.to_string())))
    }

    fn human() -> ActorId {
        ActorId::from_actor(&ActorRef::human(Some("operator".to_string())))
    }

    #[test]
    fn actor_id_is_stable_distinct_and_transparent() {
        assert_eq!(agent("sage").as_str(), "agent:sage");
        assert_eq!(human().as_str(), "human:operator");
        assert_eq!(ActorId::from_actor(&ActorRef::system()).as_str(), "system");
        assert_eq!(agent("sage"), agent("sage"));
        assert_ne!(agent("sage"), agent("flux"));
        // Transparent on the wire.
        let v = serde_json::to_value(agent("sage")).unwrap();
        assert_eq!(v, json!("agent:sage"));
    }

    #[test]
    fn higher_revision_always_wins() {
        let lo = ComponentVersion::new(1, agent("zzz")); // even with a "bigger" actor
        let hi = ComponentVersion::new(2, agent("aaa"));
        assert!(hi.supersedes(&lo));
        assert!(!lo.supersedes(&hi));
    }

    #[test]
    fn equal_revision_breaks_tie_on_actor_deterministically() {
        let a = ComponentVersion::new(5, agent("flux")); // "agent:flux"
        let b = ComponentVersion::new(5, agent("sage")); // "agent:sage" > "agent:flux"
        assert!(b.supersedes(&a), "higher actor id wins the tie");
        assert!(!a.supersedes(&b));
        assert!(!(a.supersedes(&b) && b.supersedes(&a)), "antisymmetric");
    }

    #[test]
    fn identical_version_does_not_supersede_itself() {
        let v = ComponentVersion::new(3, agent("sage"));
        assert!(!v.supersedes(&v.clone()), "a replay must not win (idempotent)");
    }

    #[test]
    fn lamport_tick_is_monotonic_and_observe_jumps_past_remote() {
        let mut c = LamportClock::new();
        assert_eq!(c.tick(), 1);
        assert_eq!(c.tick(), 2);
        c.observe(9);
        assert_eq!(c.now(), 9);
        assert_eq!(c.tick(), 10, "next local write is strictly greater than any seen");
        c.observe(3);
        assert_eq!(c.now(), 10, "observing something smaller never rewinds");
    }

    #[test]
    fn same_component_converges_regardless_of_order() {
        let id = ComponentId::new("brief-1");
        // Genuinely concurrent: same rev, neither observed the other.
        let operator_write = ComponentVersion::new(1, human()); // "human:operator"
        let agent_write = ComponentVersion::new(1, agent("sage")); // "agent:sage"

        let mut a = CanvasMergeState::new();
        let d1 = a.merge(&id, operator_write.clone());
        let d2 = a.merge(&id, agent_write.clone());

        let mut b = CanvasMergeState::new();
        b.merge(&id, agent_write.clone());
        b.merge(&id, operator_write.clone());

        assert_eq!(a.version(&id), b.version(&id), "replicas converge");
        // "human:operator" > "agent:sage" lexicographically → operator wins.
        assert_eq!(a.version(&id).unwrap(), &operator_write);
        assert_eq!(d1, MergeDecision::Applied);
        assert_eq!(d2, MergeDecision::Superseded);
    }

    #[test]
    fn higher_revision_wins_in_either_arrival_order() {
        let id = ComponentId::new("c1");
        let early = ComponentVersion::new(1, agent("sage"));
        let late = ComponentVersion::new(2, human());

        let mut s1 = CanvasMergeState::new();
        assert!(s1.merge(&id, early.clone()).applied());
        assert!(s1.merge(&id, late.clone()).applied());

        let mut s2 = CanvasMergeState::new();
        assert!(s2.merge(&id, late.clone()).applied());
        assert_eq!(s2.merge(&id, early.clone()), MergeDecision::Superseded);

        assert_eq!(s1.version(&id), s2.version(&id));
        assert_eq!(s1.version(&id).unwrap(), &late);
    }

    #[test]
    fn different_components_both_land() {
        let a = ComponentId::new("a");
        let b = ComponentId::new("b");
        let mut s = CanvasMergeState::new();
        assert!(s.merge(&a, ComponentVersion::new(1, human())).applied());
        assert!(s.merge(&b, ComponentVersion::new(1, agent("sage"))).applied());
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn replaying_the_same_write_is_idempotent() {
        let id = ComponentId::new("c1");
        let w = ComponentVersion::new(3, agent("sage"));
        let mut s = CanvasMergeState::new();
        assert!(s.merge(&id, w.clone()).applied());
        assert_eq!(s.merge(&id, w.clone()), MergeDecision::Superseded);
        assert_eq!(s.version(&id).unwrap(), &w);
    }

    #[test]
    fn max_rev_seeds_a_resuming_clock() {
        let mut s = CanvasMergeState::new();
        s.merge(&ComponentId::new("a"), ComponentVersion::new(4, human()));
        s.merge(&ComponentId::new("b"), ComponentVersion::new(7, agent("sage")));
        assert_eq!(s.max_rev(), 7);
        let mut clock = LamportClock::at(s.max_rev());
        assert_eq!(clock.tick(), 8);
    }

    #[test]
    fn target_component_routes_per_component_ops_only() {
        let up = SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new("c1"),
                kind: "card".to_string(),
                rect: None,
                z_index: None,
                content: Value::Null,
                metadata: Value::Null,
            },
        };
        assert_eq!(up.target_component(), Some(&ComponentId::new("c1")));
        assert_eq!(
            SurfacePatch::MoveComponent { component_id: ComponentId::new("c1"), x: 0.0, y: 0.0 }
                .target_component(),
            Some(&ComponentId::new("c1"))
        );
        assert_eq!(
            SurfacePatch::DeleteComponent { component_id: ComponentId::new("c1") }.target_component(),
            Some(&ComponentId::new("c1"))
        );
        // View-state / edge / multi-component ops are not gated.
        assert!(SurfacePatch::Select { ids: vec![] }.target_component().is_none());
        assert!(SurfacePatch::Disconnect { edge_id: EdgeId::new("e") }.target_component().is_none());
        assert!(SurfacePatch::Group {
            frame_id: ComponentId::new("f"),
            children: vec![],
        }
        .target_component()
        .is_none());
    }

    #[test]
    fn version_is_additive_on_the_wire() {
        // An envelope without a version omits the field entirely (legacy producers).
        let env = SurfacePatchEnvelope {
            patch_id: PatchId::new("p"),
            session_id: "s".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
            actor: ActorRef::system(),
            created_at_ms: 0,
            patch: SurfacePatch::Select { ids: vec![] },
            version: None,
        };
        let v = serde_json::to_value(&env).unwrap();
        assert!(v.get("version").is_none(), "None version is absent on the wire");

        // And a versioned one carries `{rev, actor}` with the actor transparent.
        let versioned = SurfacePatchEnvelope {
            version: Some(ComponentVersion::new(3, agent("sage"))),
            ..env
        };
        let v = serde_json::to_value(&versioned).unwrap();
        assert_eq!(v["version"]["rev"], 3);
        assert_eq!(v["version"]["actor"], "agent:sage");
    }
}
