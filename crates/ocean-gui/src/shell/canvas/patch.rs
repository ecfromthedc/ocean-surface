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

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A patch plus the session/surface/canvas/actor context needed to route and
/// persist it. Appended to the ledger's patch log.
///
/// `session_id` is a plain `String` here (ocean-surface convention) where the SDK
/// uses a `uuid`-backed `AgentSessionId`; the JSON shape is identical because the
/// SDK newtype is `serde(transparent)` over its string form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfacePatchEnvelope {
    pub patch_id: PatchId,
    pub session_id: String,
    pub surface_id: SurfaceId,
    pub canvas_id: CanvasId,
    pub actor: ActorRef,
    pub created_at_ms: i64,
    pub patch: SurfacePatch,
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
}
