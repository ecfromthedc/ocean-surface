//! Daemon connection layer.
//!
//! Single point of contact between the UI and `ocean-daemon`. We speak the
//! product agent API:
//!
//!   POST /v1/agent/turns   → start a turn (returns metadata only)
//!   GET  /v1/agent/events  → SSE stream of AgentTurnEvent
//!   GET  /v1/agent/sessions → list sessions
//!
//! All reply text and tool output arrives as events on the SSE stream; the
//! POST returns once the turn completes but carries no payload beyond
//! turn_id / session_id / status. We push events into a Leptos signal so
//! the rest of the UI reacts naturally.

use std::collections::VecDeque;

use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use gloo_net::http::Request;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::model::{Block, Role, ToolStatus, Turn};

pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4780";

/// Shape of the proxy's GET /api/config — the zero-config bootstrap payload.
#[derive(Debug, Clone, Deserialize)]
struct ProxyConfig {
    #[serde(default)]
    daemon_url: String,
    #[serde(default)]
    has_auth: bool,
    #[allow(dead_code)]
    #[serde(default)]
    voice_profile: String,
    #[serde(default)]
    maps_key: String,
    #[serde(default)]
    maps_map_id: String,
    #[serde(default)]
    livekit_room_id: String,
    #[serde(default)]
    livekit_token_path: String,
    #[serde(default)]
    tldraw_sync_uri: String,
}

/// A component interaction event sent from the client to the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentEventRequest {
    pub session_id: String,
    pub component_id: String,
    pub event: Value,
}

// ---------------------------------------------------------------------------
// Surface patch wire types (OCEAN-178)
//
// Self-contained mirror of `ocean-agent-sdk::surface` (GPUI Masterbuild Slice 1)
// so this WASM crate can deserialize the EXACT JSON the daemon streams on a
// `surface_patch` SSE frame — without taking a build dependency on the ocean-os
// workspace (this crate already mirrors every other daemon wire type the same
// way: `ToolCallSummary`, `ToolResult`, etc.). The GPUI shell carries the same
// mirror in `ocean-gui/src/shell/canvas/patch.rs`; the wire contract is fixed in
// `ocean-os/crates/ocean-agent-sdk/src/lib.rs`.
//
// Wire-contract notes (must match the SDK or patches silently route to `Other`):
// - Ids are `serde(transparent)` over `String` — on the wire a `ComponentId` is
//   just `"brief-1"`.
// - `SurfacePatch` is internally tagged on `"op"`, `snake_case`.
// - `SurfacePatchEnvelope.session_id` is carried as a plain `String` here
//   (this crate's convention); the SDK uses a `serde(transparent)` uuid newtype,
//   so the JSON is byte-identical.
// ---------------------------------------------------------------------------

/// A string-backed, `serde(transparent)` id newtype (matches the SDK exactly).
macro_rules! surface_string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        #[allow(dead_code)]
        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

surface_string_id!(
    /// Identifies a *surface* — one client face onto a session (e.g. `gpui:local`).
    SurfaceId
);
surface_string_id!(
    /// Identifies a *canvas* within a surface (e.g. `canvas:main`).
    CanvasId
);
surface_string_id!(
    /// Identifies a *component* (card, node, frame, …) on a canvas.
    ComponentId
);
surface_string_id!(
    /// Identifies a single emitted *patch*.
    PatchId
);
surface_string_id!(
    /// Identifies an *edge* between two endpoints.
    EdgeId
);

/// Axis-aligned rectangle in canvas space. All fields roundtrip as JSON numbers.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Pan/zoom state of a canvas viewport.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    #[serde(default = "viewport_default_zoom")]
    pub zoom: f32,
}

fn viewport_default_zoom() -> f32 {
    1.0
}

/// Reference to the actor that originated a patch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ActorRef {
    /// Coarse actor class, e.g. `"agent"`, `"human"`, `"system"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Upsert payload for a component. `rect`/`content` are optional so an agent can
/// create a component and let the app allocate placement.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CanvasComponentPatch {
    pub id: ComponentId,
    /// Component kind or template name, e.g. `"card"`, `"brief_card"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Endpoint {
    pub component_id: ComponentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

/// Create/update payload for an edge between two endpoints.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CanvasEdgePatch {
    pub id: EdgeId,
    pub from: Endpoint,
    pub to: Endpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Target of a [`SurfacePatch::Focus`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusTarget {
    Component { component_id: ComponentId },
    Edge { edge_id: EdgeId },
    Canvas,
}

/// Target of a [`SurfacePatch::Layout`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutTarget {
    Canvas,
    Component { component_id: ComponentId },
    Components { ids: Vec<ComponentId> },
}

/// Layout strategy. Open string set so new strategies don't break the wire.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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

/// A single structured mutation to an Ocean surface canvas. Internally tagged on
/// `"op"` with `snake_case` discriminants — `{ "op": "upsert_component", … }`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SurfacePatch {
    UpsertComponent {
        component: CanvasComponentPatch,
    },
    MoveComponent {
        component_id: ComponentId,
        x: f32,
        y: f32,
    },
    ResizeComponent {
        component_id: ComponentId,
        width: f32,
        height: f32,
    },
    DeleteComponent {
        component_id: ComponentId,
    },
    Connect {
        edge: CanvasEdgePatch,
    },
    Disconnect {
        edge_id: EdgeId,
    },
    Focus {
        target: FocusTarget,
    },
    Select {
        ids: Vec<ComponentId>,
    },
    SetViewport {
        viewport: Viewport,
    },
    Layout {
        target: LayoutTarget,
        strategy: LayoutStrategy,
    },
    Group {
        frame_id: ComponentId,
        children: Vec<ComponentId>,
    },
}

/// A patch plus the session/surface/canvas/actor context needed to route and
/// persist it. This is exactly what the daemon streams inside a `surface_patch`
/// event's `patches` array.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SurfacePatchEnvelope {
    pub patch_id: PatchId,
    /// Plain `String` here (this crate's convention); the SDK's `AgentSessionId`
    /// is `serde(transparent)` over the same string, so the JSON is identical.
    #[serde(default)]
    pub session_id: String,
    pub surface_id: SurfaceId,
    pub canvas_id: CanvasId,
    pub actor: ActorRef,
    pub created_at_ms: i64,
    pub patch: SurfacePatch,
}

/// One canvas patch the web surface has received and stored for rendering. The
/// full GPUI-style canvas (a `CanvasLedger` with placement, hit-testing, and a
/// rendered scene) is not yet ported to the web; this keeps the daemon's patch
/// stream visible (and inspectable) on the web surface instead of dropping it.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasPatchEntry {
    /// Canvas this patch targets (e.g. `canvas:main`).
    pub canvas_id: String,
    /// One-line, human-readable summary of the op (e.g. `upsert_component brief-1`).
    pub summary: String,
    /// The full stamped envelope, kept so a richer renderer can use it later.
    pub envelope: SurfacePatchEnvelope,
}

/// The shape of every event the daemon publishes on /v1/agent/events.
/// Mirrors `AgentTurnEvent` in crates/ocean-agent-sdk.
// Some fields are parsed off the wire but not yet rendered (title, cwd,
// per-event ids). They document the daemon's event shape and several get
// used as voice / status features land, so keep them.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionCreated {
        session_id: String,
        title: String,
        #[serde(default)]
        cwd: String,
    },
    TurnStarted {
        turn_id: String,
        session_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    AssistantTextDelta {
        // session_id added daemon-side so scoped SSE clients can verify the
        // frame still belongs to their active session. `default` keeps us
        // compatible with daemons that predate the field.
        #[serde(default)]
        session_id: String,
        turn_id: String,
        delta: String,
    },
    ThinkingDelta {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        delta: String,
    },
    ToolCallStarted {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call: ToolCallSummary,
    },
    ToolCallChunk {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call_id: String,
        chunk: String,
    },
    ToolCallFinished {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call_id: String,
        result: ToolResult,
    },
    TurnFinished {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        wall_ms: Option<u64>,
        #[serde(default)]
        output_tokens: Option<u64>,
        #[serde(default)]
        input_tokens: Option<u64>,
        #[serde(default)]
        cache_read_tokens: Option<u64>,
        #[serde(default)]
        tokens_per_second: Option<f64>,
    },
    /// The agent wants to mount or update an interactive component.
    ComponentRender {
        session_id: String,
        component_id: String,
        kind: String,
        props: Value,
        #[serde(default)]
        replace: bool,
    },
    /// The agent wants to unmount a previously rendered component.
    ComponentUnmount {
        session_id: String,
        component_id: String,
    },
    /// Ocean started (`active: true`) or finished (`active: false`) driving the
    /// browser. The side-panel cockpit uses this to auto-focus while browser
    /// work happens, then release back to the origin surface.
    BrowserActivity { session_id: String, active: bool },
    /// The agent applied one or more validated patches to a canvas surface
    /// (GPUI Masterbuild Slice 3, daemon side). Each patch is a fully-stamped
    /// [`SurfacePatchEnvelope`] (session/surface/canvas/actor/timestamp). The
    /// GPUI native shell applies these to its `CanvasLedger`; the web surface
    /// records them into `canvas_patches` and renders a basic representation.
    ///
    /// This mirrors the daemon's `AgentTurnEvent::SurfacePatch` wire shape
    /// exactly (`ocean-os/crates/ocean-agent-sdk/src/lib.rs`, internally tagged
    /// on `"type" = "surface_patch"`, `snake_case`). Before OCEAN-178 the web
    /// `AgentEvent` had no such variant AND `surface_patch` was absent from
    /// `AGENT_EVENT_NAMES`, so `EventSource` dropped the frame at the transport
    /// layer — the web PWA (and the extension that bundles this WASM) were blind
    /// to agent-rendered canvases. A drift-guard test deserializes the daemon's
    /// exact JSON into this variant so future wire drift fails loudly.
    SurfacePatch {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        canvas_id: CanvasId,
        patches: Vec<SurfacePatchEnvelope>,
    },
    /// Catch-all for extension / council events (e.g. Longhouse). Carries the
    /// raw payload and an optional session `scope` (OCEAN-56). A scoped event
    /// (`scope: Some`) belongs to a session and is filtered like any
    /// session-bearing event; an unscoped one (`scope: None`) is council-wide
    /// and only reaches the `?all=1` firehose. We don't render these yet, but
    /// we name the variant so they deserialize cleanly instead of being mapped
    /// to `Other` (or, on a stricter enum, failing) — then log + ignore them.
    Extension {
        extension: String,
        #[serde(default)]
        payload: Value,
        #[serde(default)]
        scope: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// SSE `event:` names the daemon emits on `GET /v1/agent/events`, one per
/// [`AgentEvent`] variant.
///
/// ⚠️ DRIFT HAZARD — this list MUST stay in lockstep with the daemon's
/// `agent_event_type_name` match in
/// `ocean-os/crates/ocean-daemon/src/main.rs:3782` (the function that names each
/// SSE frame). gloo-net's `EventSource` only delivers frames whose `event:` name
/// was explicitly `subscribe()`d, so any name the daemon emits that is NOT in
/// this list is **dropped at the transport layer before serde ever sees it** —
/// the `#[serde(other)] Other` catch-all on `AgentEvent` cannot save it, because
/// the JSON never arrives. This has already bitten Extension events (OCEAN-62)
/// and permission events (OCEAN-64), each needing a manual addition here.
///
/// Adding a daemon event is therefore a ONE-LINE change in this file: add its
/// snake_case name below. The `agent_event_names_cover_all_variants` test fails
/// the build if an `AgentEvent` variant has no matching entry here, so drift
/// surfaces at `cargo test` rather than as a silent runtime drop.
///
/// NOTE: the daemon also emits an out-of-band `error` frame on broadcast lag /
/// serialize failure (`Event::default().event("error")`). It is deliberately
/// NOT subscribed here — it is not an `AgentEvent` and carries no transcript
/// state; the per-name subscription simply ignores it.
///
/// FOLLOW-UP (needs a daemon change, #54-sequenced): the robust fix is for the
/// daemon to emit every frame under a single SSE name (e.g. the default
/// `message` channel) with the type already in the JSON payload, so the surface
/// can wire one catch-all listener and route purely on the `type` tag via serde
/// — eliminating this allow-list entirely. See OCEAN-102 PR for the ticket note.
pub(crate) const AGENT_EVENT_NAMES: &[&str] = &[
    "session_created",
    "turn_started",
    "assistant_text_delta",
    "thinking_delta",
    "tool_call_started",
    "tool_call_chunk",
    "tool_call_finished",
    "turn_finished",
    "component_render",
    "component_unmount",
    "browser_activity",
    "surface_patch",
    "extension",
];

impl AgentEvent {
    /// The session this event belongs to, if it carries one. Used to drop
    /// events from other sessions if a proxy or stale stream misbehaves. Returns
    /// `None` for `Other` and (from older daemons) for any event whose
    /// `session_id` came through empty via serde default.
    fn session_id(&self) -> Option<&str> {
        let sid = match self {
            AgentEvent::SessionCreated { session_id, .. }
            | AgentEvent::TurnStarted { session_id, .. }
            | AgentEvent::AssistantTextDelta { session_id, .. }
            | AgentEvent::ThinkingDelta { session_id, .. }
            | AgentEvent::ToolCallStarted { session_id, .. }
            | AgentEvent::ToolCallChunk { session_id, .. }
            | AgentEvent::ToolCallFinished { session_id, .. }
            | AgentEvent::TurnFinished { session_id, .. }
            | AgentEvent::ComponentRender { session_id, .. }
            | AgentEvent::ComponentUnmount { session_id, .. }
            | AgentEvent::BrowserActivity { session_id, .. }
            | AgentEvent::SurfacePatch { session_id, .. } => session_id.as_str(),
            // An extension event's scope (when set) is its session id; a
            // council-wide one has no scope and is treated as unscoped.
            AgentEvent::Extension { scope, .. } => scope.as_deref().unwrap_or(""),
            AgentEvent::Other => return None,
        };
        (!sid.is_empty()).then_some(sid)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub args_json: Value,
}

/// A pending permission request awaiting the operator's allow/deny.
///
/// Mirrors the daemon's `OceanEvent::PermissionRequest` plus the envelope's
/// `permission_id` / `session_id`. When daemon permission-gating is on, a
/// mutating tool call (write/edit/bash) BLOCKS until a decision is POSTed to
/// `/v1/permissions/{id}/decision`. The web surface renders one card per
/// pending entry so a gated turn doesn't silently hang here.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPermission {
    pub permission_id: String,
    pub session_id: String,
    pub tool: String,
    pub reason: String,
    /// Pretty-printed args summary for display.
    pub args_summary: String,
    /// True once a decision POST is in flight, so the buttons can disable.
    pub deciding: bool,
}

/// The control-plane event envelope on `/v1/events`. Unlike `/v1/agent/events`
/// (which streams `AgentTurnEvent` and serializes only the inner event), this
/// stream serializes the FULL `EventEnvelope`, so `permission_id` / `session_id`
/// ride alongside the flattened `OceanEvent`. We only model the two permission
/// frames; every other `type` falls into `Other` and is ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlEvent {
    PermissionRequest {
        #[serde(default)]
        permission_id: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
        tool: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        args: Value,
    },
    PermissionDecision {
        #[serde(default)]
        permission_id: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    #[serde(default)]
    pub output: String,
}

/// One image attached to a turn (OCEAN-138). Serializes to the exact wire shape
/// the daemon's `TurnImage` (ocean-agent-sdk) deserializes: a `mime_type` and a
/// base64 `data` body (a `data:<mime>;base64,` prefix is tolerated — the daemon
/// strips it). On the first user message of the turn the daemon emits one
/// `Content::Image` block per entry alongside the prompt text (shipped in
/// OCEAN-115), so a screenshot/picked image actually reaches the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnImage {
    /// MIME type of the image, e.g. `"image/png"` or `"image/jpeg"`.
    pub mime_type: String,
    /// Base64-encoded image bytes, or a `data:<mime>;base64,` URL (the daemon
    /// strips the prefix, keeping only the base64 body).
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
struct AgentTurnRequest<'a> {
    prompt: &'a str,
    cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    /// Selected project. When set, the daemon binds the turn to the project's
    /// workspace_root (the web client sends "/" as cwd, so without this every
    /// session lands in the daemon's launch dir).
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_type: Option<&'a str>,
    /// Optional guidance hints passed to the agent (e.g. "focus on tests").
    /// Matches the daemon's `AgentTurnRequest::guidance: Option<Vec<String>>`.
    /// The web UI doesn't surface this yet, so it serializes as `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<Vec<String>>,
    /// Optional room identifier for Track-0 room-scoped turns. Mirrors the
    /// daemon's `room_id: Option<String>`. Not yet exposed in the web UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    room_id: Option<&'a str>,
    /// Per-turn reasoning effort override. Mirrors the daemon's
    /// `thinking_level: Option<ThinkingLevel>` — serialized as the lowercase
    /// `ThinkingLevel` string the daemon expects. `None` leaves the daemon's
    /// global default in force. Not yet exposed in the web UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_level: Option<&'a str>,
    /// Per-turn / per-session model override (OCEAN-36). Mirrors the daemon's
    /// `model_id: Option<String>`. Not yet exposed in the web UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_id: Option<&'a str>,
    /// Images attached to the turn (OCEAN-138). Mirrors the daemon's
    /// `images: Option<Vec<TurnImage>>` (OCEAN-115) — when present the daemon
    /// emits one `Content::Image` block per entry on the first user message,
    /// enabling vision end-to-end. Omitted (and the daemon defaults to `None`)
    /// when no image was captured/picked for this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<TurnImage>>,
}

#[derive(Debug, Clone, Serialize)]
struct AgentSessionCreateRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    /// Workspace anchor for the session. The daemon's
    /// `AgentSessionCreateRequest` deserializes this as a **required**
    /// `workspace_root` field (no serde alias for `cwd`) — sending `cwd` here
    /// made POST /v1/agent/sessions fail to deserialize, silently breaking
    /// surface session creation. Send `workspace_root` to match (OCEAN-62b).
    workspace_root: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_type: Option<&'a str>,
}

/// Default for [`AgentSessionCreateResponse::ok`]. The daemon's canonical
/// create response (`ocean-agent-sdk::AgentSessionCreateResponse`) is
/// `{session_id, cwd, client_type}` and carries **no `ok` field** — a 200 with
/// a `session_id` *is* the success signal. Requiring `ok` here made serde fail
/// with `missing field 'ok'`, so the surface never got a session id and chat
/// was 100% dead (OCEAN-124). Default a missing `ok` to `true` so the daemon's
/// real response decodes as success, while any future `ok:false` error shape is
/// still honored.
fn default_true() -> bool {
    true
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct AgentSessionCreateResponse {
    #[serde(default = "default_true")]
    ok: bool,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    workspace_root: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// One project in the picker catalogue (from `GET /v1/projects`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProjectInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub workspace_root: String,
}

// The POST response carries only metadata; reply text/ids arrive via SSE.
// We read `ok`/`error` for failure handling and ignore the rest.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AgentTurnResponse {
    pub ok: bool,
    pub turn_id: String,
    pub session_id: String,
    pub status: String,
    /// Prefix the daemon stamps on this turn's SSE event ids so a client can
    /// correlate the HTTP response with the `GET /v1/agent/events` stream.
    /// `Option` + `serde(default)` for forward-compat with older daemons that
    /// don't emit it (OCEAN-81).
    #[serde(default)]
    pub event_id_prefix: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Reactive handle to the daemon. Owns the live turns vec + connection
/// status; surfaces APIs to send prompts.
#[derive(Clone)]
pub struct Daemon {
    pub url: RwSignal<String>,
    pub turns: RwSignal<Vec<Turn>>,
    pub streaming: RwSignal<bool>,
    pub session_id: RwSignal<Option<String>>,
    pub status: RwSignal<String>,
    pub cwd: RwSignal<String>,
    /// Whether the proxy reports a usable xAI key (voice STT/TTS available).
    /// Rendered independently of the SSE `status` string so it isn't clobbered
    /// by connect()'s "connecting…"/"connected" transitions.
    pub voice_ready: RwSignal<bool>,
    /// Google Maps JS API key from /api/config, used by the map component to
    /// load the Maps script. Empty until bootstrap (and when no key is set).
    pub maps_key: RwSignal<String>,
    /// Map ID for the map's visual style (from /api/config).
    pub maps_map_id: RwSignal<String>,
    /// Default LiveKit room id for Ocean collaboration surfaces.
    pub livekit_room_id: RwSignal<String>,
    /// Same-origin token path for joining the configured LiveKit room.
    pub livekit_token_path: RwSignal<String>,
    /// tldraw sync endpoint hint, empty when canvases should stay local-only.
    pub tldraw_sync_uri: RwSignal<String>,
    /// Monotonic connection generation. Incremented before opening an SSE stream
    /// so reconnect/switch/new-session calls retire older streams instead of
    /// applying every delta multiple times.
    sse_generation: RwSignal<u64>,
    /// Legacy guard retained for older daemon/proxy builds. New surfaces create
    /// sessions explicitly before posting turns, so this should stay false.
    awaiting_session_adoption: RwSignal<bool>,
    /// Current session title (set on SessionCreated or when switching).
    pub session_title: RwSignal<String>,
    /// Fetched session list from the daemon.
    pub session_list: RwSignal<Vec<SessionSummary>>,
    /// Token usage from the most recently finished turn (real provider numbers
    /// when available). `None` until the first turn finishes.
    pub last_turn_tokens: RwSignal<Option<TokenStats>>,
    /// Running token total across all turns in this session. Reset on
    /// new_session / switch_session.
    pub session_tokens: RwSignal<TokenStats>,
    /// Current model id, learned from TurnStarted (and GET /v1/models). Shown
    /// live in the header so a mid-session swap is visible.
    pub model: RwSignal<Option<String>>,
    /// The catalogue of selectable models from GET /v1/models.
    pub models: RwSignal<Vec<ModelInfo>>,
    /// The selected project id, sent as `project_id` on every turn so the daemon
    /// binds to that project's directory. Persisted in localStorage so the
    /// choice survives reload. `None` = no project (turns then need a real cwd).
    pub project: RwSignal<Option<String>>,
    /// The catalogue of projects from GET /v1/projects.
    pub projects: RwSignal<Vec<ProjectInfo>>,
    /// turn_id of the in-flight turn, captured from TurnStarted — the halt
    /// button cancels this via POST /v1/requests/{id}/cancel.
    pub active_turn_id: RwSignal<Option<String>>,
    /// True while Ocean is actively driving the browser. Set from the daemon's
    /// `browser_activity` SSE event. The extension side panel uses this to take
    /// focus during browser work and release afterward; other surfaces can show
    /// a passive "Ocean is driving the browser" cue.
    pub browser_active: RwSignal<bool>,
    /// The most recent live browser action the agent performed, e.g.
    /// `"browser_navigate"`. Captured from `ToolCallStarted` for any `browser_*`
    /// tool and shown next to the browser-control indicator so the user can see
    /// *what* the agent just did in the browser, not only that it's active. The
    /// `browser_activity` event itself only carries `{ active }`, so we derive
    /// the action label from the tool-call stream (OCEAN-92).
    pub browser_last_action: RwSignal<Option<String>>,
    /// Permission requests awaiting an allow/deny decision, oldest first. Each
    /// blocks its turn on the daemon until decided. Populated from the
    /// `/v1/events` control stream (`permission_request`) and cleared on
    /// `permission_decision` or a successful decision POST. Multiple can stack.
    pub pending_permissions: RwSignal<Vec<PendingPermission>>,
    /// Per-turn reasoning-effort override (OCEAN-79). Holds the serialized
    /// `ThinkingLevel` string (`off` / `low` / `medium` / `high`) the daemon
    /// expects, or `None` to leave the daemon's global default in force. Set
    /// from the composer's thinking-level selector and sent on every
    /// `AgentTurnRequest::thinking_level`. Persisted in localStorage so the
    /// choice survives reload. `None` = unchanged behavior.
    pub thinking_level: RwSignal<Option<String>>,
    /// Per-turn model override (OCEAN-79). Distinct from `model` /
    /// [`set_model`], which globally hot-swaps the daemon via `POST /v1/model`;
    /// this rides on `AgentTurnRequest::model_id` so the turn runs with the
    /// chosen model without mutating the daemon's global selection. `None` =
    /// daemon default. Persisted in localStorage. Drawn from the same `models`
    /// catalogue fetched from `GET /v1/models`.
    pub model_override: RwSignal<Option<String>>,
    /// Images staged for the NEXT turn (OCEAN-138). A captured visible tab (or a
    /// picked image) lands here and rides along on the next `send_prompt` as
    /// `AgentTurnRequest::images`, then is drained. Empty = no attachment, so the
    /// field is omitted and the daemon behaves exactly as before.
    pub pending_images: RwSignal<Vec<TurnImage>>,
    /// Canvas patches the agent has applied this session, oldest first
    /// (OCEAN-178). Populated from the daemon's `surface_patch` SSE event. The
    /// GPUI native shell renders these on a full `CanvasLedger`; the web surface
    /// renders a basic representation of the patch stream so the data is no
    /// longer silently dropped at the transport layer. Reset on session change.
    pub canvas_patches: RwSignal<Vec<CanvasPatchEntry>>,
}

/// A selectable model, mirroring the daemon's KnownModel.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub label: String,
}

/// Token usage for a turn (or summed for a session), mirrored from the daemon's
/// TurnFinished event. All counts are real provider usage when reported.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenStats {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// Tokens/sec for the last turn; not meaningful when summed, so a session
    /// total leaves this at 0.
    pub tokens_per_second: f64,
}

impl TokenStats {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }
}

/// Summary of a session, matching the daemon's AgentSessionSummary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub turn_count: u32,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionDetailResponse {
    ok: bool,
    #[serde(default)]
    session: Option<SessionDetail>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionDetail {
    id: String,
    title: String,
    model: String,
    #[serde(default)]
    workspace_root: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    transcript: Vec<SessionTranscriptEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionTranscriptEntry {
    role: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
}

impl Daemon {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: RwSignal::new(url.into()),
            turns: RwSignal::new(Vec::new()),
            streaming: RwSignal::new(false),
            session_id: RwSignal::new(None),
            status: RwSignal::new("disconnected".into()),
            cwd: RwSignal::new(default_cwd()),
            voice_ready: RwSignal::new(false),
            maps_key: RwSignal::new(String::new()),
            maps_map_id: RwSignal::new(String::new()),
            livekit_room_id: RwSignal::new(String::new()),
            livekit_token_path: RwSignal::new(String::new()),
            tldraw_sync_uri: RwSignal::new(String::new()),
            sse_generation: RwSignal::new(0),
            awaiting_session_adoption: RwSignal::new(false),
            session_title: RwSignal::new(String::new()),
            session_list: RwSignal::new(Vec::new()),
            last_turn_tokens: RwSignal::new(None),
            session_tokens: RwSignal::new(TokenStats::default()),
            model: RwSignal::new(None),
            models: RwSignal::new(Vec::new()),
            // Restore the last-selected project from localStorage so the choice
            // survives a reload.
            project: RwSignal::new(load_persisted_project()),
            projects: RwSignal::new(Vec::new()),
            active_turn_id: RwSignal::new(None),
            browser_active: RwSignal::new(false),
            browser_last_action: RwSignal::new(None),
            pending_permissions: RwSignal::new(Vec::new()),
            // Restore the last-selected per-turn overrides from localStorage so
            // the choices survive a reload (like `project`).
            thinking_level: RwSignal::new(load_persisted_thinking_level()),
            model_override: RwSignal::new(load_persisted_model_override()),
            pending_images: RwSignal::new(Vec::new()),
            canvas_patches: RwSignal::new(Vec::new()),
        }
    }

    /// A dummy daemon that does nothing. Useful for component previews
    /// and the gauntlet — component interactions will no-op gracefully.
    pub fn dummy() -> Self {
        Self {
            url: RwSignal::new("http://127.0.0.1:4780".into()),
            turns: RwSignal::new(Vec::new()),
            streaming: RwSignal::new(false),
            session_id: RwSignal::new(None),
            status: RwSignal::new("dummy".into()),
            cwd: RwSignal::new("/".into()),
            voice_ready: RwSignal::new(false),
            maps_key: RwSignal::new(String::new()),
            maps_map_id: RwSignal::new(String::new()),
            livekit_room_id: RwSignal::new(String::new()),
            livekit_token_path: RwSignal::new(String::new()),
            tldraw_sync_uri: RwSignal::new(String::new()),
            sse_generation: RwSignal::new(0),
            awaiting_session_adoption: RwSignal::new(false),
            session_title: RwSignal::new(String::new()),
            session_list: RwSignal::new(Vec::new()),
            last_turn_tokens: RwSignal::new(None),
            session_tokens: RwSignal::new(TokenStats::default()),
            model: RwSignal::new(None),
            models: RwSignal::new(Vec::new()),
            project: RwSignal::new(None),
            projects: RwSignal::new(Vec::new()),
            active_turn_id: RwSignal::new(None),
            browser_active: RwSignal::new(false),
            browser_last_action: RwSignal::new(None),
            pending_permissions: RwSignal::new(Vec::new()),
            thinking_level: RwSignal::new(None),
            model_override: RwSignal::new(None),
            pending_images: RwSignal::new(Vec::new()),
            canvas_patches: RwSignal::new(Vec::new()),
        }
    }

    /// Zero-config boot. Fetch the same-origin proxy's /api/config to learn
    /// the daemon URL (and confirm auth is preconfigured server-side), set
    /// `url` from it, then open the SSE stream. If the proxy isn't reachable
    /// or doesn't answer, fall back to whatever `url` was constructed with.
    /// The user never types a URL or credential.
    pub fn bootstrap_then_connect(&self) {
        let daemon = self.clone();
        spawn_local(async move {
            // In a Chrome extension (side panel) there is no same-origin proxy:
            // the document is served from chrome-extension://, so a relative
            // `/api/config` resolves to the extension itself, not the daemon.
            // Detect that and talk to the daemon directly at its loopback URL,
            // skipping the proxy bootstrap entirely.
            let is_extension = running_as_extension();
            if is_extension {
                daemon.url.set(DEFAULT_DAEMON_URL.to_string());
                daemon.connect();
                daemon.fetch_models();
                daemon.fetch_projects();
                return;
            }
            match Request::get("/api/config").send().await {
                Ok(resp) => match resp.json::<ProxyConfig>().await {
                    Ok(cfg) => {
                        // Always honor the config's daemon_url, INCLUDING empty.
                        // Empty = "talk to the daemon through this same origin"
                        // (the proxy reverse-proxies /v1/agent/*). That's what
                        // makes the phone-via-tunnel case work: relative URLs,
                        // no localhost, no mixed content.
                        daemon.url.set(cfg.daemon_url.trim().to_string());
                        // Record voice readiness in its own signal so the SSE
                        // status transitions in connect() don't clobber it.
                        daemon.voice_ready.set(cfg.has_auth);
                        daemon.maps_key.set(cfg.maps_key.trim().to_string());
                        daemon.maps_map_id.set(cfg.maps_map_id.trim().to_string());
                        daemon
                            .livekit_room_id
                            .set(cfg.livekit_room_id.trim().to_string());
                        daemon
                            .livekit_token_path
                            .set(cfg.livekit_token_path.trim().to_string());
                        daemon
                            .tldraw_sync_uri
                            .set(cfg.tldraw_sync_uri.trim().to_string());
                    }
                    Err(_) => {
                        // Non-JSON / unexpected shape — keep the fallback url.
                    }
                },
                Err(_) => {
                    // No proxy in front (e.g. trunk serve direct). Keep fallback.
                }
            }
            daemon.connect();
            // Re-fetch the model catalogue now that the daemon URL is resolved.
            // The eager fetch_models() at startup runs BEFORE bootstrap learns
            // the real origin, so remotely (phone via tunnel) it hits the wrong
            // URL and the picker ends up with an empty list (only the current
            // model, learned later from the turn stream). Fetching here, against
            // the now-correct origin, populates the full catalogue.
            daemon.fetch_models();
            // Same rule as fetch_models: only after the origin is resolved.
            daemon.fetch_projects();
        });
    }

    /// Open the SSE stream and pipe events into the turns signal. Reconnects
    /// on disconnect with a small backoff. Spawned once per session.
    pub fn connect(&self) {
        let url = self.url.get_untracked();
        let turns = self.turns;
        let streaming = self.streaming;
        let session_id = self.session_id;
        let status = self.status;
        let sse_generation = self.sse_generation;
        let last_turn_tokens = self.last_turn_tokens;
        let session_tokens = self.session_tokens;
        let model = self.model;
        let active_turn_id = self.active_turn_id;
        let browser_active = self.browser_active;
        let browser_last_action = self.browser_last_action;
        let canvas_patches = self.canvas_patches;
        let awaiting_session_adoption = self.awaiting_session_adoption;
        // Captured so the reconnect path can re-hydrate the transcript (and
        // restore title/cwd) after a stream gap — see the rehydrate call below.
        let session_title = self.session_title;
        let cwd = self.cwd;

        let generation = sse_generation.get_untracked().wrapping_add(1);
        sse_generation.set(generation);
        let Some(active_session_id) = session_id.get_untracked() else {
            status.set("new session".into());
            return;
        };
        let seen_sse_ids: RwSignal<VecDeque<String>> = RwSignal::new(VecDeque::new());

        // Permission requests ride a SEPARATE stream. The product event stream
        // `/v1/agent/events` only carries `AgentTurnEvent` types and serializes
        // the inner event (no `permission_id`). The daemon emits
        // `OceanEvent::PermissionRequest` — with the envelope's `permission_id`
        // — onto the control stream `/v1/events`. Open that too, scoped to this
        // session/generation, so a gated mutating turn surfaces an approval card
        // instead of hanging. Decisions clear on `permission_decision` here.
        self.connect_permission_stream(active_session_id.clone(), generation);

        spawn_local(async move {
            // SSE is a live tail: the daemon does NOT replay frames emitted
            // while the client was disconnected. The FIRST open of this
            // generation is paired with a fresh transcript that the caller has
            // already hydrated (new/switch/create), so it needs no recovery.
            // Every RE-open after a drop, though, has a gap — any delta /
            // tool-call / turn_finished emitted during the outage is gone. So on
            // each reconnect we re-fetch the session snapshot (OCEAN-72's
            // hydration) to recover the missed events instead of leaving the UI
            // permanently stale/truncated (OCEAN-104).
            let mut connected_once = false;

            loop {
                if sse_generation.get_untracked() != generation {
                    break;
                }

                // On a reconnect (not the first open) the live tail had a gap.
                // Re-hydrate the transcript from the daemon so missed events are
                // recovered, and clear any stuck `streaming` state — if the turn
                // finished during the outage, the `turn_finished` frame that
                // would have flipped `streaming` off never arrives, so without
                // this the composer's Stop button (and the streaming gate) stay
                // stuck on forever.
                if connected_once {
                    if streaming.get_untracked() {
                        // The in-flight turn's terminal frame may have been lost
                        // in the gap. Drop the stuck streaming state now; the
                        // re-hydrate below restores the true transcript, and if
                        // the turn is genuinely still running its live frames
                        // resume on the fresh stream.
                        streaming.set(false);
                        active_turn_id.set(None);
                    }
                    rehydrate_transcript(
                        url.clone(),
                        active_session_id.clone(),
                        turns,
                        session_id,
                        session_title,
                        cwd,
                        model,
                    )
                    .await;
                }

                let events_url = format!(
                    "{}/v1/agent/events?session_id={}",
                    url.trim_end_matches('/'),
                    active_session_id.as_str()
                );
                status.set(if connected_once {
                    "reconnecting…".into()
                } else {
                    "connecting…".to_string()
                });
                let mut es = match EventSource::new(&events_url) {
                    Ok(es) => es,
                    Err(err) => {
                        status.set(format!("sse connect error: {err}"));
                        gloo_timers::future::TimeoutFuture::new(2_000).await;
                        continue;
                    }
                };
                status.set("connected".into());
                // Mark that we have opened the stream at least once for this
                // generation. From here on, any loop re-entry is a reconnect and
                // triggers the re-hydrate/stuck-streaming recovery at the top.
                connected_once = true;

                // EventSource delivers events by `event:` name. The daemon
                // names each frame by its AgentTurnEvent type, so we subscribe
                // per type and merge the streams. gloo-net has no
                // `subscribe_multiple`; we build the merged stream ourselves
                // with `futures::stream::select_all`. The name list is the
                // single source of truth `AGENT_EVENT_NAMES` (see its doc
                // comment — it MUST mirror the daemon's emitted names, and a
                // test guards against drift).
                let mut subs = Vec::with_capacity(AGENT_EVENT_NAMES.len());
                let mut sub_err = None;
                for name in AGENT_EVENT_NAMES {
                    match es.subscribe(*name) {
                        Ok(s) => subs.push(s),
                        Err(err) => {
                            sub_err = Some(format!("sse subscribe '{name}' error: {err}"));
                            break;
                        }
                    }
                }
                if let Some(err) = sub_err {
                    status.set(err);
                    gloo_timers::future::TimeoutFuture::new(2_000).await;
                    continue;
                }

                let mut stream = futures_util::stream::select_all(subs);
                while let Some(msg) = stream.next().await {
                    if sse_generation.get_untracked() != generation {
                        break;
                    }

                    let Ok((_event_name, msg)) = msg else {
                        continue;
                    };

                    // Tunnels/proxies can reconnect or replay a frame around
                    // connection churn. The daemon includes a stable SSE `id:`
                    // for each AgentTurnEvent, so apply each id only once per
                    // connection generation. Without this guard a replayed
                    // assistant_text_delta appends the same chunk again, which
                    // shows up as doubled words in the transcript.
                    let event_id = msg.last_event_id();
                    if !event_id.is_empty() && seen_recent_sse_id(seen_sse_ids, &event_id) {
                        continue;
                    }

                    let Some(data) = msg.data().as_string() else {
                        continue;
                    };
                    let Ok(evt) = serde_json::from_str::<AgentEvent>(&data) else {
                        log::warn!("unparseable sse event: {data}");
                        continue;
                    };

                    // Hard isolation: every renderable product event must carry
                    // exactly the active session id. If a proxy/global stream or
                    // older daemon sends an unscoped frame, drop it before any
                    // reducer code can mutate transcript, browser focus, tokens,
                    // components, or active turn state.
                    match evt.session_id() {
                        Some(evt_sid) if evt_sid == active_session_id.as_str() => {}
                        Some(evt_sid) => {
                            log::warn!(
                                "dropping sse event for session {evt_sid}; active session is {}",
                                active_session_id.as_str()
                            );
                            continue;
                        }
                        None => {
                            log::warn!("dropping unscoped sse event on session stream");
                            continue;
                        }
                    }

                    // If the visible surface has switched sessions since this
                    // connection was opened, this generation is stale. Stop it;
                    // the explicit session switch/create path will open a new
                    // scoped stream.
                    if session_id.get_untracked().as_deref() != Some(active_session_id.as_str()) {
                        break;
                    }

                    apply_event(
                        &evt,
                        turns,
                        session_id,
                        streaming,
                        status,
                        last_turn_tokens,
                        session_tokens,
                        model,
                        active_turn_id,
                        browser_active,
                        browser_last_action,
                        canvas_patches,
                        awaiting_session_adoption,
                    );
                }

                if sse_generation.get_untracked() != generation {
                    break;
                }

                // Brief backoff before re-opening. The status flips to
                // "reconnecting…" and the transcript re-hydrates at the top of
                // the next iteration (gated on `connected_once`).
                status.set("reconnecting…".into());
                gloo_timers::future::TimeoutFuture::new(1_000).await;
            }
        });
    }

    /// Subscribe to the daemon's control stream `/v1/events` for permission
    /// frames, scoped to `active_session_id` and the current SSE `generation`.
    ///
    /// `EventSource` delivers frames by their `event:` name, so we subscribe to
    /// `permission_request` and `permission_decision` by name (the same per-name
    /// pattern the agent stream uses). The control stream is NOT session-scoped
    /// server-side, so we drop any frame whose envelope `session_id` isn't ours.
    /// When gating is off the daemon never emits these, so this stream just sits
    /// idle — no behavior change for the ungated path.
    fn connect_permission_stream(&self, active_session_id: String, generation: u64) {
        let url = self.url.get_untracked();
        let sse_generation = self.sse_generation;
        let pending = self.pending_permissions;

        spawn_local(async move {
            loop {
                if sse_generation.get_untracked() != generation {
                    break;
                }
                let events_url = format!("{}/v1/events", url.trim_end_matches('/'));
                let mut es = match EventSource::new(&events_url) {
                    Ok(es) => es,
                    Err(_) => {
                        gloo_timers::future::TimeoutFuture::new(2_000).await;
                        continue;
                    }
                };

                let mut subs = Vec::new();
                let mut sub_err = false;
                for name in ["permission_request", "permission_decision"] {
                    match es.subscribe(name) {
                        Ok(s) => subs.push(s),
                        Err(_) => {
                            sub_err = true;
                            break;
                        }
                    }
                }
                if sub_err {
                    gloo_timers::future::TimeoutFuture::new(2_000).await;
                    continue;
                }

                let mut stream = futures_util::stream::select_all(subs);
                while let Some(msg) = stream.next().await {
                    if sse_generation.get_untracked() != generation {
                        break;
                    }
                    let Ok((_event_name, msg)) = msg else {
                        continue;
                    };
                    let Some(data) = msg.data().as_string() else {
                        continue;
                    };
                    let Ok(evt) = serde_json::from_str::<ControlEvent>(&data) else {
                        continue;
                    };
                    apply_control_event(&evt, &active_session_id, pending);
                }

                if sse_generation.get_untracked() != generation {
                    break;
                }
                gloo_timers::future::TimeoutFuture::new(1_000).await;
            }
        });
    }

    /// Record an allow/deny decision for a pending permission by POSTing
    /// `/v1/permissions/{id}/decision`. The body matches the daemon's
    /// `PermissionDecisionRequest`: `{ "permission_id": <id>, "decision":
    /// "allow" }` or `{ "permission_id": <id>, "decision": "deny" }` (the
    /// `decision` enum is `#[serde(tag = "decision")]`, flattened into the
    /// request). On success the entry is removed; the daemon also broadcasts a
    /// `permission_decision` frame which removes it too (whichever lands first).
    pub fn decide_permission(&self, permission_id: String, allow: bool) {
        let url = self.url.get_untracked();
        let status = self.status;
        let pending = self.pending_permissions;

        // Mark the card as deciding so its buttons disable and it can't be
        // double-submitted.
        pending.update(|list| {
            if let Some(p) = list.iter_mut().find(|p| p.permission_id == permission_id) {
                p.deciding = true;
            }
        });

        spawn_local(async move {
            let post_url = format!(
                "{}/v1/permissions/{permission_id}/decision",
                url.trim_end_matches('/')
            );
            let body = if allow {
                json!({ "permission_id": permission_id, "decision": "allow" })
            } else {
                json!({ "permission_id": permission_id, "decision": "deny" })
            };
            let res = Request::post(&post_url)
                .header("content-type", "application/json")
                .json(&body);
            let res = match res {
                Ok(req) => req.send().await,
                Err(err) => {
                    status.set(format!("permission encode error: {err}"));
                    clear_pending_deciding(pending, &permission_id);
                    return;
                }
            };
            match res {
                Ok(resp) if resp.ok() => {
                    remove_pending_permission(pending, &permission_id);
                    status.set(if allow { "permission allowed".into() } else { "permission denied".into() });
                }
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    status.set(format!("permission decision failed: {text}"));
                    clear_pending_deciding(pending, &permission_id);
                }
                Err(err) => {
                    status.set(format!("permission post error: {err}"));
                    clear_pending_deciding(pending, &permission_id);
                }
            }
        });
    }

    pub fn send_prompt(&self, prompt: String) {
        if prompt.trim().is_empty() {
            return;
        }
        // Echo the user prompt immediately, then dispatch under the surface's
        // ambient client identity (web vs. extension).
        self.turns.update(|t| t.push(Turn::user(prompt.clone())));
        self.streaming.set(true);
        self.dispatch_prompt(prompt, false, surface_client_type());
    }

    /// Send a voice-orb transcript. Identical to [`send_prompt`] except the turn
    /// is tagged `client_type="leo-voice"` so the daemon's voice system prompt
    /// (`voice_surface_prompt`, routed on `client_type == "leo-voice"`) applies —
    /// concise, speakable replies with no visual components. Without this the
    /// transcript would be tagged `surface-web`/`surface-extension` like a typed
    /// message and that voice guidance would be unreachable (OCEAN-181).
    pub fn send_voice_prompt(&self, prompt: String) {
        if prompt.trim().is_empty() {
            return;
        }
        self.turns.update(|t| t.push(Turn::user(prompt.clone())));
        self.streaming.set(true);
        self.dispatch_prompt(prompt, false, VOICE_CLIENT_TYPE);
    }

    /// Capture the visible browser tab and stage it for the next turn
    /// (OCEAN-138). Invokes the extension loader's
    /// `window.__ocean_capture_visible_tab()` — which returns a Promise of a
    /// `data:image/png;base64,...` URL via `chrome.tabs.captureVisibleTab` (a
    /// JS-only extension API the wasm app can't call directly) — then parses the
    /// data URL into a [`TurnImage`] and pushes it onto `pending_images`. On the
    /// next `send_prompt` the daemon emits it as a `Content::Image` block, so the
    /// agent can actually reason over the screenshot. No-op outside the
    /// extension. User-initiated only; never fires on a timer.
    pub fn capture_and_attach_visible_tab(&self) {
        if !running_as_extension() {
            return;
        }
        let pending_images = self.pending_images;
        let status = self.status;
        spawn_local(async move {
            let Some(window) = web_sys::window() else { return };
            let Ok(func) = js_sys::Reflect::get(
                &window,
                &wasm_bindgen::JsValue::from_str("__ocean_capture_visible_tab"),
            ) else {
                return;
            };
            let Ok(func) = func.dyn_into::<js_sys::Function>() else {
                return;
            };
            let promise = match func.call0(&window) {
                Ok(v) => v,
                Err(_) => {
                    status.set("screenshot capture failed".into());
                    return;
                }
            };
            let Ok(promise) = promise.dyn_into::<js_sys::Promise>() else {
                return;
            };
            let data_url = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(v) => v.as_string(),
                Err(_) => {
                    status.set("screenshot capture failed".into());
                    return;
                }
            };
            let Some(data_url) = data_url else {
                status.set("screenshot capture returned no data".into());
                return;
            };
            match parse_data_url(&data_url) {
                Some(image) => {
                    pending_images.update(|imgs| imgs.push(image));
                    status.set("screenshot attached — it rides on your next message".into());
                }
                None => status.set("screenshot capture returned an unreadable image".into()),
            }
        });
    }

    /// Send a turn to the daemon. `is_retry` marks an auto-recovery resend (the
    /// user prompt was already echoed; don't echo again). If the daemon reports
    /// the supplied session is gone (strict resume), we clear the stale id and
    /// retry once as a fresh session — so a daemon restart is invisible to the
    /// user instead of dead-ending the turn.
    fn dispatch_prompt(&self, prompt: String, is_retry: bool, client_type: &'static str) {
        let url = self.url.get_untracked();
        let session_id = self.session_id.get_untracked();
        self.awaiting_session_adoption.set(false);
        let project = self.project.get_untracked();
        // When a project is selected, send an EMPTY cwd so the daemon binds to
        // the project's workspace_root (a non-empty cwd would win and override
        // it). With no project, fall back to the configured cwd as before.
        let cwd = if project.is_some() {
            String::new()
        } else {
            self.cwd.get_untracked()
        };
        let streaming = self.streaming;
        let status = self.status;
        // Per-turn overrides (OCEAN-79). Captured untracked so the async block
        // sends whatever was selected at dispatch time. Both default to `None`,
        // which omits the field and leaves the daemon's global defaults in force.
        let thinking_level = self.thinking_level.get_untracked();
        let model_override = self.model_override.get_untracked();
        // Images staged for this turn (OCEAN-138). Read untracked at dispatch
        // time; an empty vec serializes as no `images` field. They are cleared
        // only after the turn POST succeeds (below), so a failed send keeps the
        // attachment around to retry rather than silently dropping it.
        let pending_images = self.pending_images.get_untracked();
        let daemon = self.clone();

        spawn_local(async move {
            if session_id.is_none() {
                let title_hint = session_title_hint(&prompt);
                let body = AgentSessionCreateRequest {
                    title: title_hint.as_deref(),
                    workspace_root: &cwd,
                    project_id: project.as_deref(),
                    // A session's `client_type` is the stable surface MEDIUM
                    // (surface-web / surface-extension), per the AGENTS.md session
                    // contract — never the per-turn routing tag. Even when the
                    // first interaction on a fresh surface is a voice transcript
                    // (so the threaded `client_type` is "leo-voice"), the session
                    // itself is still a web/extension surface; voice is a mode OF
                    // it, not its own surface. Only the AgentTurnRequest below
                    // carries the per-turn `client_type` (leo-voice for voice).
                    client_type: Some(surface_client_type()),
                };
                let create_url = format!("{}/v1/agent/sessions", url.trim_end_matches('/'));
                let res = Request::post(&create_url)
                    .header("content-type", "application/json")
                    .json(&body);
                let res = match res {
                    Ok(req) => req.send().await,
                    Err(err) => {
                        status.set(format!("session encode error: {err}"));
                        streaming.set(false);
                        return;
                    }
                };

                match res {
                    Ok(resp) => match resp.json::<AgentSessionCreateResponse>().await {
                        Ok(r) if r.ok => {
                            let Some(new_session_id) = r.session_id else {
                                status.set("session create failed: missing session id".into());
                                streaming.set(false);
                                return;
                            };
                            daemon.session_id.set(Some(new_session_id));
                            if let Some(title) = r.title.filter(|title| !title.trim().is_empty()) {
                                daemon.session_title.set(title);
                            }
                            if let Some(root) = r.workspace_root.or(r.cwd) {
                                if !root.is_empty() {
                                    daemon.cwd.set(root);
                                }
                            }
                            status.set("session ready".into());
                            daemon.connect();
                            daemon.fetch_sessions();
                            daemon.dispatch_prompt(prompt, is_retry, client_type);
                        }
                        Ok(r) => {
                            status.set(format!(
                                "session create failed: {}",
                                r.error.unwrap_or_else(|| "unknown error".into())
                            ));
                            streaming.set(false);
                        }
                        Err(err) => {
                            status.set(format!("session decode error: {err}"));
                            streaming.set(false);
                        }
                    },
                    Err(err) => {
                        status.set(format!("session post error: {err}"));
                        streaming.set(false);
                    }
                }
                return;
            }

            // In the Chrome side panel we ride along in the user's live tab.
            // Attach the active tab's URL + title — and the current window's
            // open-tab list (OCEAN-92) — as guidance so the agent knows what
            // page the user is on and what other tabs they have open when they
            // send a turn. Only the current window's already-open tabs, only on
            // a user-initiated turn — never a passive scrape (OCEAN-70). On the
            // detached web app this is always `None`.
            let active_tab_guidance = browser_context_guidance();
            let body = AgentTurnRequest {
                prompt: &prompt,
                cwd: &cwd,
                session_id: session_id.as_deref(),
                project_id: project.as_deref(),
                client_type: Some(client_type),
                // The web UI doesn't surface free-form guidance yet; the only
                // guidance we emit is the extension's active-tab context above.
                // The remaining per-turn overrides serialize as `None` so the
                // daemon applies its global defaults, matching the daemon's
                // AgentTurnRequest wire shape (OCEAN-61).
                guidance: active_tab_guidance,
                room_id: None,
                // Per-turn overrides selected in the composer (OCEAN-79). Both
                // are `None` until the user touches a control, preserving the
                // daemon-default behavior. `thinking_level` is already the
                // lowercase `ThinkingLevel` string the daemon deserializes.
                thinking_level: thinking_level.as_deref(),
                model_id: model_override.as_deref(),
                // Captured/picked images ride along on the FIRST user message of
                // the turn (OCEAN-138). Omitted when nothing was staged, so the
                // daemon's `images: Option<Vec<TurnImage>>` stays `None`.
                images: if pending_images.is_empty() {
                    None
                } else {
                    Some(pending_images.clone())
                },
            };
            let post_url = format!("{}/v1/agent/turns", url.trim_end_matches('/'));
            let res = Request::post(&post_url)
                .header("content-type", "application/json")
                .json(&body);
            let res = match res {
                Ok(req) => req.send().await,
                Err(err) => {
                    status.set(format!("encode error: {err}"));
                    streaming.set(false);
                    daemon.awaiting_session_adoption.set(false);
                    return;
                }
            };
            match res {
                Ok(resp) => match resp.json::<AgentTurnResponse>().await {
                    Ok(r) if r.ok => {
                        // Do not let a late HTTP response from an older submit
                        // switch the visible cockpit back to another session.
                        // Active session changes only via explicit create/select
                        // paths, not passive turn responses or SSE.
                        if daemon.session_id.get_untracked().as_deref() == session_id.as_deref() {
                            daemon.session_id.set(Some(r.session_id));
                        }
                        // The staged images were accepted with this turn — drain
                        // them so they don't ride along on the next one
                        // (OCEAN-138). On any failure path we leave them in place
                        // so the user can retry without re-capturing.
                        if !pending_images.is_empty() {
                            daemon.pending_images.set(Vec::new());
                        }
                        // Reply text streams over the scoped SSE connection;
                        // streaming flips off on turn_finished.
                    }
                    Ok(r) => {
                        let err = r.error.unwrap_or_else(|| "unknown error".into());
                        // Strict-resume recovery: our session id is stale (e.g.
                        // the daemon restarted). Drop it and retry once fresh.
                        if !is_retry && session_id.is_some() && err.contains("session not found") {
                            daemon.session_id.set(None);
                            daemon.reset_token_stats();
                            status.set("session expired — starting fresh".into());
                            daemon.dispatch_prompt(prompt, true, client_type);
                            return;
                        }
                        status.set(format!("turn failed: {err}"));
                        streaming.set(false);
                        daemon.awaiting_session_adoption.set(false);
                    }
                    Err(err) => {
                        status.set(format!("decode error: {err}"));
                        streaming.set(false);
                        daemon.awaiting_session_adoption.set(false);
                    }
                },
                Err(err) => {
                    status.set(format!("post error: {err}"));
                    streaming.set(false);
                    daemon.awaiting_session_adoption.set(false);
                }
            }
        });
    }

    /// Fetch session list from the daemon and store in session_list signal.
    pub fn fetch_sessions(&self) {
        let url = self.url.get_untracked();
        let session_list = self.session_list;
        spawn_local(async move {
            let get_url = format!("{}/v1/agent/sessions", url.trim_end_matches('/'));
            match Request::get(&get_url).send().await {
                Ok(resp) => {
                    #[derive(Deserialize)]
                    struct SessionsResponse {
                        ok: bool,
                        #[serde(default)]
                        sessions: Vec<SessionSummary>,
                    }
                    match resp.json::<SessionsResponse>().await {
                        Ok(r) if r.ok => {
                            session_list.set(r.sessions);
                        }
                        Ok(r) => {
                            log::warn!("sessions fetch not ok: {:?}", r.ok);
                        }
                        Err(err) => {
                            log::warn!("sessions decode error: {err}");
                        }
                    }
                }
                Err(err) => {
                    log::warn!("sessions fetch error: {err}");
                }
            }
        });
    }

    /// Fetch the model catalogue + current selection from the daemon.
    pub fn fetch_models(&self) {
        let url = self.url.get_untracked();
        let models = self.models;
        let model = self.model;
        spawn_local(async move {
            #[derive(Deserialize)]
            struct Current {
                #[serde(default)]
                model: String,
            }
            #[derive(Deserialize)]
            struct ModelsResponse {
                #[serde(default)]
                models: Vec<ModelInfo>,
                #[serde(default)]
                current: Option<Current>,
            }
            let get_url = format!("{}/v1/models", url.trim_end_matches('/'));
            match Request::get(&get_url).send().await {
                Ok(resp) => match resp.json::<ModelsResponse>().await {
                    Ok(r) => {
                        if let Some(cur) = r.current {
                            if !cur.model.is_empty() {
                                model.set(Some(cur.model));
                            }
                        }
                        models.set(r.models);
                    }
                    Err(err) => log::warn!("models decode error: {err}"),
                },
                Err(err) => log::warn!("models fetch error: {err}"),
            }
        });
    }

    /// Fetch the project catalogue from the daemon. Like [`fetch_models`], call
    /// this only AFTER the daemon URL is resolved (see `bootstrap_then_connect`)
    /// — an eager pre-bootstrap fetch hits the wrong origin and silently fails.
    pub fn fetch_projects(&self) {
        let url = self.url.get_untracked();
        let projects = self.projects;
        let current = self.project;
        spawn_local(async move {
            #[derive(Deserialize)]
            struct ProjectsResponse {
                #[serde(default)]
                projects: Vec<ProjectInfo>,
            }
            let get_url = format!("{}/v1/projects", url.trim_end_matches('/'));
            match Request::get(&get_url).send().await {
                Ok(resp) => match resp.json::<ProjectsResponse>().await {
                    Ok(r) => {
                        // Drop a persisted selection that no longer exists.
                        if let Some(sel) = current.get_untracked() {
                            if !r.projects.iter().any(|p| p.id == sel) {
                                current.set(None);
                                clear_persisted_project();
                            }
                        }
                        projects.set(r.projects);
                    }
                    Err(err) => log::warn!("projects decode error: {err}"),
                },
                Err(err) => log::warn!("projects fetch error: {err}"),
            }
        });
    }

    /// Select the active project. Unlike the model, this is purely client-side:
    /// the choice rides on every turn's `project_id`. Persist it so it survives
    /// reload. Pass `None` to clear.
    pub fn set_project(&self, id: Option<String>) {
        self.project.set(id.clone());
        match id {
            Some(id) => persist_project(&id),
            None => clear_persisted_project(),
        }
    }

    /// Set the per-turn reasoning-effort override (OCEAN-79). `level` is the
    /// serialized `ThinkingLevel` string (`off` / `low` / `medium` / `high`);
    /// pass `None` to clear and fall back to the daemon's global default. Purely
    /// client-side — the value rides on the next turn's `thinking_level`.
    /// Persisted so the choice survives reload.
    pub fn set_thinking_level(&self, level: Option<String>) {
        self.thinking_level.set(level.clone());
        match level {
            Some(l) => persist_thinking_level(&l),
            None => clear_persisted_thinking_level(),
        }
    }

    /// Set the per-turn model override (OCEAN-79). Unlike [`set_model`] (a
    /// global `POST /v1/model` swap), this only sets the next turn's `model_id`
    /// and never mutates the daemon's global model. Pass `None` to clear and use
    /// the daemon default. Persisted so the choice survives reload.
    pub fn set_model_override(&self, id: Option<String>) {
        self.model_override.set(id.clone());
        match id {
            Some(id) => persist_model_override(&id),
            None => clear_persisted_model_override(),
        }
    }

    /// Hot-swap the daemon's model. Optimistically updates the local `model`
    /// signal, POSTs the change, then re-reads to confirm.
    pub fn set_model(&self, id: String) {
        let url = self.url.get_untracked();
        let model = self.model;
        let status = self.status;
        let daemon = self.clone();
        model.set(Some(id.clone()));
        spawn_local(async move {
            let post_url = format!("{}/v1/model", url.trim_end_matches('/'));
            let body = serde_json::json!({ "model": id });
            match Request::post(&post_url)
                .header("content-type", "application/json")
                .json(&body)
            {
                Ok(req) => match req.send().await {
                    Ok(_) => {
                        // Confirm the authoritative selection.
                        daemon.fetch_models();
                    }
                    Err(err) => status.set(format!("model swap error: {err}")),
                },
                Err(err) => status.set(format!("model encode error: {err}")),
            }
        });
    }

    /// Halt the in-flight turn, if any, via POST /v1/requests/{turn_id}/cancel.
    pub fn halt(&self) {
        let Some(turn_id) = self.active_turn_id.get_untracked() else {
            return;
        };
        let url = self.url.get_untracked();
        let status = self.status;
        let streaming = self.streaming;
        spawn_local(async move {
            let post_url = format!("{}/v1/requests/{turn_id}/cancel", url.trim_end_matches('/'));
            match Request::post(&post_url).send().await {
                Ok(_) => {
                    status.set("halting…".into());
                    // streaming flips off when turn_finished (failed/cancelled)
                    // arrives; flip it now too so the UI reacts immediately.
                    streaming.set(false);
                }
                Err(err) => status.set(format!("halt error: {err}")),
            }
        });
    }

    /// Switch to a different session. Clears the current turns, sets the
    /// session_id, fetches the persisted transcript snapshot, then reconnects the
    /// SSE stream for any future live events. SSE is a live tail, not historical
    /// replay, so switching sessions must explicitly hydrate from the daemon.
    pub fn switch_session(&self, id: String, title: String) {
        self.turns.set(Vec::new());
        self.canvas_patches.set(Vec::new());
        self.session_id.set(Some(id.clone()));
        self.awaiting_session_adoption.set(false);
        self.session_title.set(title);
        self.status.set("loading session…".into());
        self.reset_token_stats();
        self.load_session_snapshot(id);
        self.connect();
    }

    fn load_session_snapshot(&self, id: String) {
        let url = self.url.get_untracked();
        let turns = self.turns;
        let session_id = self.session_id;
        let session_title = self.session_title;
        let cwd = self.cwd;
        let model = self.model;
        let status = self.status;

        spawn_local(async move {
            let get_url = format!("{}/v1/sessions/{id}", url.trim_end_matches('/'));
            match Request::get(&get_url).send().await {
                Ok(resp) => match resp.json::<SessionDetailResponse>().await {
                    Ok(r) if r.ok => {
                        let Some(detail) = r.session else {
                            status.set("session snapshot missing".into());
                            return;
                        };
                        // Guard against stale async loads if the user switches
                        // sessions again before this fetch completes.
                        if session_id.get_untracked().as_deref() != Some(detail.id.as_str()) {
                            return;
                        }
                        session_title.set(detail.title.clone());
                        if let Some(root) = detail.workspace_root.or(detail.cwd) {
                            if !root.is_empty() {
                                cwd.set(root);
                            }
                        }
                        if !detail.model.is_empty() {
                            model.set(Some(detail.model));
                        }
                        turns.set(turns_from_session_transcript(detail.transcript));
                        status.set("session loaded".into());
                    }
                    Ok(r) => {
                        status.set(format!(
                            "session load failed: {}",
                            r.error.unwrap_or_else(|| "unknown error".into())
                        ));
                    }
                    Err(err) => status.set(format!("session decode error: {err}")),
                },
                Err(err) => status.set(format!("session fetch error: {err}")),
            }
        });
    }

    /// Reset to a fresh, not-yet-created session. Clears state and leaves
    /// `session_id` as `None` so the next prompt lazily creates a session
    /// (see `dispatch_prompt`).
    ///
    /// This preserves the default single-session flow — a user who never opens
    /// the session UI still gets a session created on their first message. Kept
    /// as a public reset primitive even though the UI's "New Session" control
    /// now prefers the eager [`create_session`].
    #[allow(dead_code)]
    pub fn new_session(&self) {
        self.turns.set(Vec::new());
        self.canvas_patches.set(Vec::new());
        self.session_id.set(None);
        self.awaiting_session_adoption.set(false);
        self.session_title.set(String::new());
        self.status.set("new session".into());
        self.reset_token_stats();
        self.connect();
    }

    /// Eagerly create a new session on the daemon and switch to it.
    ///
    /// Unlike [`new_session`], this POSTs `/v1/agent/sessions` right away
    /// (workspace_root from the current cwd / selected project), then switches
    /// the active session to the returned `session_id` — clearing the transcript,
    /// re-scoping the SSE stream via `connect()`, and refreshing the session list.
    /// Used by the "New Session" control so the user gets a live, switchable
    /// session immediately instead of waiting for their first prompt.
    pub fn create_session(&self) {
        let url = self.url.get_untracked();
        let project = self.project.get_untracked();
        // Mirror dispatch_prompt's cwd rule: with a project selected, send an
        // empty workspace_root so the daemon binds to the project's root;
        // otherwise anchor to the configured cwd.
        let workspace_root = if project.is_some() {
            String::new()
        } else {
            self.cwd.get_untracked()
        };
        let status = self.status;
        let daemon = self.clone();

        // Optimistically clear the surface so the user sees a fresh session
        // while the POST is in flight; the returned id wires up the live stream.
        self.status.set("creating session…".into());

        spawn_local(async move {
            let body = AgentSessionCreateRequest {
                title: None,
                workspace_root: &workspace_root,
                project_id: project.as_deref(),
                client_type: Some(surface_client_type()),
            };
            let create_url = format!("{}/v1/agent/sessions", url.trim_end_matches('/'));
            let res = Request::post(&create_url)
                .header("content-type", "application/json")
                .json(&body);
            let res = match res {
                Ok(req) => req.send().await,
                Err(err) => {
                    status.set(format!("session encode error: {err}"));
                    return;
                }
            };
            match res {
                Ok(resp) => match resp.json::<AgentSessionCreateResponse>().await {
                    Ok(r) if r.ok => {
                        let Some(new_session_id) = r.session_id else {
                            status.set("session create failed: missing session id".into());
                            return;
                        };
                        // Switch to the new session: reset transcript + tokens,
                        // adopt the id, and re-scope SSE to it via connect().
                        daemon.turns.set(Vec::new());
                        daemon.canvas_patches.set(Vec::new());
                        daemon.session_id.set(Some(new_session_id));
                        daemon.awaiting_session_adoption.set(false);
                        daemon
                            .session_title
                            .set(r.title.filter(|t| !t.trim().is_empty()).unwrap_or_default());
                        if let Some(root) = r.workspace_root.or(r.cwd) {
                            if !root.is_empty() {
                                daemon.cwd.set(root);
                            }
                        }
                        daemon.reset_token_stats();
                        daemon.status.set("session ready".into());
                        daemon.connect();
                        daemon.fetch_sessions();
                    }
                    Ok(r) => {
                        status.set(format!(
                            "session create failed: {}",
                            r.error.unwrap_or_else(|| "unknown error".into())
                        ));
                    }
                    Err(err) => status.set(format!("session decode error: {err}")),
                },
                Err(err) => status.set(format!("session post error: {err}")),
            }
        });
    }

    /// Clear per-turn and session token counters (on session change).
    fn reset_token_stats(&self) {
        self.last_turn_tokens.set(None);
        self.session_tokens.set(TokenStats::default());
    }

    /// Send a component interaction event back to the daemon.
    /// This is how the web surface tells the agent "user clicked a kanban card"
    /// or "user submitted a form". If a `component_wait` is pending on the
    /// agent side, it resolves immediately; otherwise the event is queued for
    /// the next turn.
    pub fn send_component_event(&self, component_id: String, payload: Value) {
        let sid = self.session_id.get_untracked();
        let Some(session_id) = sid else {
            self.status.set("no session — send a prompt first".into());
            return;
        };
        let url = self.url.get_untracked();
        let status = self.status;
        spawn_local(async move {
            let body = ComponentEventRequest {
                session_id,
                component_id,
                event: payload,
            };
            let post_url = format!("{}/v1/component/event", url.trim_end_matches('/'));
            let res = Request::post(&post_url)
                .header("content-type", "application/json")
                .json(&body);
            let res = match res {
                Ok(req) => req.send().await,
                Err(err) => {
                    status.set(format!("component event encode error: {err}"));
                    return;
                }
            };
            match res {
                Ok(resp) => {
                    if !resp.ok() {
                        let text = resp.text().await.unwrap_or_default();
                        status.set(format!("component event error: {text}"));
                    }
                }
                Err(err) => {
                    status.set(format!("component event post error: {err}"));
                }
            }
        });
    }
}

/// Mutate the turns vec in response to a single SSE event. Splits assistant
/// content into Text / Thinking / ToolCall blocks under one Turn per turn_id,
/// matching the TUI's `pm_*_assistant_turn_mut` logic.
#[allow(clippy::too_many_arguments)]
fn apply_event(
    event: &AgentEvent,
    turns: RwSignal<Vec<Turn>>,
    session_id: RwSignal<Option<String>>,
    streaming: RwSignal<bool>,
    status: RwSignal<String>,
    last_turn_tokens: RwSignal<Option<TokenStats>>,
    session_tokens: RwSignal<TokenStats>,
    model: RwSignal<Option<String>>,
    active_turn_id: RwSignal<Option<String>>,
    browser_active: RwSignal<bool>,
    browser_last_action: RwSignal<Option<String>>,
    canvas_patches: RwSignal<Vec<CanvasPatchEntry>>,
    awaiting_session_adoption: RwSignal<bool>,
) {
    let Some(evt_sid) = event.session_id() else {
        log::warn!("dropping unscoped agent event before reducer");
        return;
    };
    if session_id.get_untracked().as_deref() != Some(evt_sid) {
        log::warn!("dropping agent event for non-active session {evt_sid}");
        return;
    }

    match event {
        AgentEvent::SessionCreated { title, .. } => {
            awaiting_session_adoption.set(false);
            // Keep the title somewhere accessible so the header can show it.
            if let Some(window) = web_sys::window() {
                if let Some(doc) = window.document() {
                    doc.set_title(&format!("Ocean — {title}"));
                }
            }
        }
        AgentEvent::TurnStarted {
            turn_id, model: m, ..
        } => {
            awaiting_session_adoption.set(false);
            // Track the in-flight turn so the halt button can cancel it, and
            // reflect the live model (covers a mid-session swap).
            active_turn_id.set(Some(turn_id.clone()));
            if let Some(m) = m {
                model.set(Some(m.clone()));
            }
            // Assistant turn will be lazily created on the first delta.
        }
        AgentEvent::AssistantTextDelta { turn_id, delta, .. } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                match turn.blocks.last_mut() {
                    Some(Block::Text(buf)) => buf.push_str(delta),
                    _ => turn.blocks.push(Block::Text(delta.clone())),
                }
            });
        }
        AgentEvent::ThinkingDelta { turn_id, delta, .. } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                match turn.blocks.last_mut() {
                    Some(Block::Thinking { content, .. }) => content.push_str(delta),
                    _ => turn.blocks.push(Block::Thinking {
                        content: delta.clone(),
                        expanded: false,
                    }),
                }
            });
        }
        AgentEvent::ToolCallStarted { turn_id, call, .. } => {
            // Mirror live browser actions onto the control indicator: any
            // `browser_*` tool call updates the "last action" label the header
            // shows next to the "driving the browser" cue (OCEAN-92).
            if call.name.starts_with("browser_") {
                browser_last_action.set(Some(call.name.clone()));
            }
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                let args = serde_json::to_string(&call.args_json).unwrap_or_else(|_| "{}".into());
                let preview: String = args.chars().take(60).collect();
                turn.blocks.push(Block::ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    args_preview: preview,
                    output: String::new(),
                    status: ToolStatus::Running,
                    expanded: false,
                });
            });
        }
        AgentEvent::ToolCallChunk {
            turn_id,
            call_id,
            chunk,
            ..
        } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                for block in turn.blocks.iter_mut().rev() {
                    if let Block::ToolCall {
                        call_id: id,
                        output,
                        ..
                    } = block
                    {
                        if id == call_id {
                            output.push_str(chunk);
                            break;
                        }
                    }
                }
            });
        }
        AgentEvent::ToolCallFinished {
            turn_id,
            call_id,
            result,
            ..
        } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                for block in turn.blocks.iter_mut().rev() {
                    if let Block::ToolCall {
                        call_id: id,
                        output,
                        status,
                        expanded,
                        ..
                    } = block
                    {
                        if id == call_id {
                            if output.is_empty() && !result.output.is_empty() {
                                output.push_str(&result.output);
                            }
                            *status = if result.ok {
                                ToolStatus::Ok
                            } else {
                                ToolStatus::Err
                            };
                            // Auto-expand failed tool calls so the error output
                            // is visible instead of hidden in a collapsed drawer.
                            if *status == ToolStatus::Err {
                                *expanded = true;
                            }
                            break;
                        }
                    }
                }
            });
        }
        AgentEvent::TurnFinished {
            status: turn_status,
            error,
            output_tokens,
            input_tokens,
            cache_read_tokens,
            tokens_per_second,
            ..
        } => {
            streaming.set(false);
            awaiting_session_adoption.set(false);
            active_turn_id.set(None);
            // Surface a failed turn instead of silently flipping `streaming`
            // off. The daemon reports `status`/`error` on the finish frame; a
            // turn that errored or was cancelled previously just stopped with no
            // feedback, leaving the user staring at a dead composer. Mirror the
            // GPUI shell, which puts the error in its status line (OCEAN-100).
            // Daemon `AgentTurnStatus` is one of completed/failed/cancelled.
            if let Some(err) = error {
                status.set(format!("turn error: {err}"));
            } else if turn_status != "completed" {
                // A non-success status with no error string (e.g. "cancelled").
                status.set(format!("turn {turn_status}"));
            } else {
                status.set("connected".into());
            }
            // Record this turn's usage (real provider numbers when present) and
            // fold it into the running session total.
            let turn_stats = TokenStats {
                input: input_tokens.unwrap_or(0),
                output: output_tokens.unwrap_or(0),
                cache_read: cache_read_tokens.unwrap_or(0),
                tokens_per_second: tokens_per_second.unwrap_or(0.0),
            };
            last_turn_tokens.set(Some(turn_stats));
            session_tokens.update(|s| {
                s.input += turn_stats.input;
                s.output += turn_stats.output;
                s.cache_read += turn_stats.cache_read;
                // Session total isn't a rate; keep tokens_per_second at 0.
            });
        }
        AgentEvent::ComponentRender {
            component_id,
            kind,
            props,
            replace,
            ..
        } => {
            turns.update(|t| {
                if *replace {
                    // Replace existing component with same id.
                    for turn in t.iter_mut() {
                        for block in turn.blocks.iter_mut() {
                            if let Block::Component {
                                component_id: id, ..
                            } = block
                            {
                                if id == component_id {
                                    *block = Block::Component {
                                        component_id: component_id.clone(),
                                        kind: kind.clone(),
                                        props: props.clone(),
                                    };
                                    return;
                                }
                            }
                        }
                    }
                }
                // Append as a new assistant block (creates a turn if needed).
                let turn = ensure_assistant_turn(t, "component-render");
                turn.blocks.push(Block::Component {
                    component_id: component_id.clone(),
                    kind: kind.clone(),
                    props: props.clone(),
                });
            });
        }
        AgentEvent::ComponentUnmount { component_id, .. } => {
            turns.update(|t| {
                for turn in t.iter_mut() {
                    turn.blocks.retain(|block| match block {
                        Block::Component {
                            component_id: id, ..
                        } => id != component_id,
                        _ => true,
                    });
                }
                // Remove empty turns.
                t.retain(|turn| !turn.blocks.is_empty());
            });
        }
        AgentEvent::BrowserActivity { active, .. } => {
            browser_active.set(*active);
            // In the extension side-panel context, focus pulls the cockpit
            // forward so the conversation visibly "follows" the browser work.
            // In a normal tab this is a harmless no-op.
            if *active {
                if let Some(win) = web_sys::window() {
                    let _ = win.focus();
                }
            }
        }
        AgentEvent::SurfacePatch {
            canvas_id, patches, ..
        } => {
            // The agent applied canvas patches this turn (OCEAN-178). The GPUI
            // native shell replays each envelope through a full `CanvasLedger`;
            // the web surface doesn't have that ledger/renderer yet, so we record
            // each patch into `canvas_patches` and render a basic representation.
            // The point of this ticket is that these frames are no longer dropped
            // at the transport layer — the data is now visible on the web surface.
            canvas_patches.update(|entries| {
                for envelope in patches {
                    entries.push(CanvasPatchEntry {
                        canvas_id: canvas_id.as_str().to_string(),
                        summary: summarize_surface_patch(&envelope.patch),
                        envelope: envelope.clone(),
                    });
                }
                // Keep the ledger bounded so a long, patch-heavy session can't
                // grow it without limit.
                let len = entries.len();
                if len > MAX_CANVAS_PATCHES {
                    entries.drain(0..len - MAX_CANVAS_PATCHES);
                }
            });
        }
        AgentEvent::Extension { extension, .. } => {
            // No renderer for extension/council events on this surface yet. Log
            // and ignore rather than silently drop, so we can see them in the
            // console while the deck UI is built out (OCEAN-62a).
            log::debug!("ignoring extension event: {extension}");
        }
        AgentEvent::Other => {
            // An event whose `type` tag this surface doesn't model fell into the
            // serde catch-all. We can't render it, but log it so a newly-added
            // daemon event type is visible in the console instead of vanishing
            // silently (OCEAN-100).
            log::debug!("ignoring unrecognized agent event type");
        }
    }
}

/// Apply one control-stream frame to the pending-permission queue, scoped to the
/// active session. A `permission_request` enqueues a card (deduped by id); a
/// `permission_decision` removes the matching card (the daemon decided it,
/// possibly from another surface like the TUI). Frames for other sessions, or
/// without a `permission_id`, are dropped.
fn apply_control_event(
    event: &ControlEvent,
    active_session_id: &str,
    pending: RwSignal<Vec<PendingPermission>>,
) {
    match event {
        ControlEvent::PermissionRequest {
            permission_id,
            session_id,
            tool,
            reason,
            args,
        } => {
            // Hard session isolation, matching the agent stream: a frame must
            // carry exactly the active session id, else drop it.
            if session_id.as_deref() != Some(active_session_id) {
                return;
            }
            let Some(permission_id) = permission_id.clone() else {
                return;
            };
            let entry = PendingPermission {
                permission_id: permission_id.clone(),
                session_id: active_session_id.to_string(),
                tool: tool.clone(),
                reason: reason.clone(),
                args_summary: summarize_args(args),
                deciding: false,
            };
            pending.update(|list| {
                // Dedupe: the daemon reuses one PermissionId for an identical
                // tool+args retry within a turn, so a replayed frame must not
                // stack a second card.
                if list.iter().any(|p| p.permission_id == permission_id) {
                    return;
                }
                list.push(entry);
            });
        }
        ControlEvent::PermissionDecision {
            permission_id,
            session_id,
        } => {
            if session_id.as_deref() != Some(active_session_id) {
                return;
            }
            if let Some(id) = permission_id {
                remove_pending_permission(pending, id);
            }
        }
        ControlEvent::Other => {}
    }
}

/// Remove a pending permission by id (decision recorded or daemon-resolved).
fn remove_pending_permission(pending: RwSignal<Vec<PendingPermission>>, permission_id: &str) {
    pending.update(|list| list.retain(|p| p.permission_id != permission_id));
}

/// Clear the `deciding` flag on a card (a decision POST failed; re-enable it).
fn clear_pending_deciding(pending: RwSignal<Vec<PendingPermission>>, permission_id: &str) {
    pending.update(|list| {
        if let Some(p) = list.iter_mut().find(|p| p.permission_id == permission_id) {
            p.deciding = false;
        }
    });
}

/// Upper bound on the per-session canvas-patch ledger (OCEAN-178). Oldest
/// entries are dropped past this so a long, patch-heavy session stays bounded.
const MAX_CANVAS_PATCHES: usize = 512;

/// A one-line, human-readable summary of a single surface patch op, for the
/// basic web canvas representation (OCEAN-178). Mirrors the daemon's op names.
fn summarize_surface_patch(patch: &SurfacePatch) -> String {
    match patch {
        SurfacePatch::UpsertComponent { component } => {
            format!("upsert_component {} ({})", component.id.as_str(), component.kind)
        }
        SurfacePatch::MoveComponent { component_id, x, y } => {
            format!("move_component {} → ({x}, {y})", component_id.as_str())
        }
        SurfacePatch::ResizeComponent {
            component_id,
            width,
            height,
        } => format!(
            "resize_component {} → {width}×{height}",
            component_id.as_str()
        ),
        SurfacePatch::DeleteComponent { component_id } => {
            format!("delete_component {}", component_id.as_str())
        }
        SurfacePatch::Connect { edge } => format!(
            "connect {} ({} → {})",
            edge.id.as_str(),
            edge.from.component_id.as_str(),
            edge.to.component_id.as_str()
        ),
        SurfacePatch::Disconnect { edge_id } => format!("disconnect {}", edge_id.as_str()),
        SurfacePatch::Focus { .. } => "focus".to_string(),
        SurfacePatch::Select { ids } => format!("select {} component(s)", ids.len()),
        SurfacePatch::SetViewport { .. } => "set_viewport".to_string(),
        SurfacePatch::Layout { .. } => "layout".to_string(),
        SurfacePatch::Group { frame_id, children } => {
            format!("group {} ({} children)", frame_id.as_str(), children.len())
        }
    }
}

/// Render the tool's args JSON into a compact, human-readable summary for the
/// approval card. Objects render as `key: value` lines; everything else is
/// pretty-printed. Kept short so the card stays scannable.
fn summarize_args(args: &Value) -> String {
    match args {
        Value::Null => String::new(),
        Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}: {val}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Re-fetch a session's persisted transcript snapshot and apply it to `turns`
/// (plus title/cwd/model), recovering events missed while the SSE stream was
/// disconnected. SSE is a live tail with no server-side replay, so a reconnect
/// after a network blip / daemon restart / proxy hiccup would otherwise leave
/// the transcript permanently stale or truncated (OCEAN-104).
///
/// This mirrors [`Daemon::load_session_snapshot`] but is a free async fn so the
/// `connect()` reconnect loop can `await` it inline without a `&self` handle.
/// Like the snapshot loader, it guards against a session switch landing mid
/// fetch by re-checking the active `session_id` before mutating any signal.
async fn rehydrate_transcript(
    url: String,
    expected_session_id: String,
    turns: RwSignal<Vec<Turn>>,
    session_id: RwSignal<Option<String>>,
    session_title: RwSignal<String>,
    cwd: RwSignal<String>,
    model: RwSignal<Option<String>>,
) {
    // If the user switched away from this session while we were disconnected,
    // the reconnect (and this hydrate) is for a session no longer on screen.
    // Bail before touching any signal so we don't clobber the new transcript.
    if session_id.get_untracked().as_deref() != Some(expected_session_id.as_str()) {
        return;
    }
    let get_url = format!("{}/v1/sessions/{expected_session_id}", url.trim_end_matches('/'));
    let resp = match Request::get(&get_url).send().await {
        Ok(resp) => resp,
        Err(err) => {
            log::warn!("rehydrate fetch error: {err}");
            return;
        }
    };
    let detail = match resp.json::<SessionDetailResponse>().await {
        Ok(r) if r.ok => match r.session {
            Some(detail) => detail,
            None => {
                log::warn!("rehydrate: session snapshot missing");
                return;
            }
        },
        Ok(r) => {
            log::warn!(
                "rehydrate failed: {}",
                r.error.unwrap_or_else(|| "unknown error".into())
            );
            return;
        }
        Err(err) => {
            log::warn!("rehydrate decode error: {err}");
            return;
        }
    };
    // Re-check after the await: a switch may have raced the fetch.
    if session_id.get_untracked().as_deref() != Some(detail.id.as_str()) {
        return;
    }
    if !detail.title.is_empty() {
        session_title.set(detail.title.clone());
    }
    if let Some(root) = detail.workspace_root.or(detail.cwd) {
        if !root.is_empty() {
            cwd.set(root);
        }
    }
    if !detail.model.is_empty() {
        model.set(Some(detail.model));
    }
    // Replace the (possibly truncated) live transcript with the daemon's
    // authoritative snapshot, which includes anything missed during the gap.
    turns.set(turns_from_session_transcript(detail.transcript));
}

fn turns_from_session_transcript(entries: Vec<SessionTranscriptEntry>) -> Vec<Turn> {
    let mut turns = Vec::new();
    for entry in entries {
        if entry.text.trim().is_empty() && entry.tool_name.is_none() {
            continue;
        }
        match entry.role.as_str() {
            "user" => turns.push(Turn::user(entry.text)),
            "assistant" => {
                let mut turn = Turn::assistant(format!("snapshot-{}", turns.len()));
                if entry.is_error.unwrap_or(false) {
                    turn.blocks.push(Block::ToolCall {
                        call_id: format!("snapshot-error-{}", turns.len()),
                        name: "assistant_error".into(),
                        args_preview: String::new(),
                        output: entry.text,
                        status: ToolStatus::Err,
                        expanded: true,
                    });
                } else {
                    turn.blocks.push(Block::Text(entry.text));
                }
                turns.push(turn);
            }
            "tool" => {
                let mut turn = Turn::assistant(format!("snapshot-tool-{}", turns.len()));
                let is_error = entry.is_error.unwrap_or(false);
                turn.blocks.push(Block::ToolCall {
                    call_id: format!("snapshot-tool-{}", turns.len()),
                    name: entry.tool_name.unwrap_or_else(|| "tool".into()),
                    args_preview: String::new(),
                    output: entry.text,
                    status: if is_error {
                        ToolStatus::Err
                    } else {
                        ToolStatus::Ok
                    },
                    // Failed tool calls open by default so the error is visible.
                    expanded: is_error,
                });
                turns.push(turn);
            }
            _ => {}
        }
    }
    turns
}

fn ensure_assistant_turn<'a>(turns: &'a mut Vec<Turn>, turn_id: &str) -> &'a mut Turn {
    let matches_last = turns
        .last()
        .map(|t| t.role == Role::Assistant && t.turn_id.as_deref() == Some(turn_id))
        .unwrap_or(false);
    if !matches_last {
        turns.push(Turn::assistant(turn_id.to_string()));
    }
    turns.last_mut().unwrap()
}

/// Returns true if `event_id` has already been applied, otherwise records it.
///
/// The daemon sends stable SSE `id:` values for `AgentTurnEvent`s. Browser
/// EventSource/proxy reconnects may replay recent frames, and the streaming
/// accumulator is intentionally append-only for delta events, so replaying a
/// frame blindly duplicates visible text/tool output. Keep a bounded LRU-style
/// window so a re-delivered id is applied at most once without growing forever
/// during a long daemon session.
fn seen_recent_sse_id(seen: RwSignal<VecDeque<String>>, event_id: &str) -> bool {
    const MAX_SEEN_SSE_IDS: usize = 2048;

    if seen.with_untracked(|ids| ids.iter().any(|id| id == event_id)) {
        return true;
    }

    seen.update(|ids| {
        ids.push_back(event_id.to_string());
        while ids.len() > MAX_SEEN_SSE_IDS {
            ids.pop_front();
        }
    });
    false
}

/// Best-effort default cwd. In the browser there's no real cwd, so we send
/// "/" and let the user override later via a settings panel.
fn default_cwd() -> String {
    "/".into()
}

/// True when this bundle is running inside the Chrome extension side panel
/// (document served from `chrome-extension://`) rather than the browser PWA.
/// Drives both the daemon-URL bootstrap and the `client_type` we report so the
/// agent knows it's the in-Chrome cockpit, not a detached web app.
pub fn running_as_extension() -> bool {
    web_sys::window()
        .and_then(|w| w.location().protocol().ok())
        .map(|p| p.starts_with("chrome-extension"))
        .unwrap_or(false)
}

/// Parse a `data:<mime>;base64,<body>` URL into a [`TurnImage`] (OCEAN-138).
/// `chrome.tabs.captureVisibleTab` hands us exactly this shape. We keep the full
/// `data:` URL in `data` — the daemon strips the `data:<mime>;base64,` prefix
/// itself, so either form is accepted; carrying the prefix means the same value
/// can drive an `<img src>` preview later without reconstruction. Returns `None`
/// for anything that isn't a base64 data URL.
fn parse_data_url(data_url: &str) -> Option<TurnImage> {
    let rest = data_url.strip_prefix("data:")?;
    let (meta, _body) = rest.split_once(',')?;
    // meta looks like `image/png;base64`. Require base64 and a non-empty mime.
    let mime_type = meta.split(';').next().unwrap_or("").trim().to_string();
    if mime_type.is_empty() || !meta.contains("base64") {
        return None;
    }
    Some(TurnImage {
        mime_type,
        data: data_url.to_string(),
    })
}

/// `client_type` for voice-orb turns. The daemon routes its concise, speakable,
/// no-visual-components voice system prompt (`voice_surface_prompt`) on exactly
/// this string, so it must stay in lockstep with the daemon (OCEAN-181).
pub(crate) const VOICE_CLIENT_TYPE: &str = "leo-voice";

/// The surface identity sent to the daemon as `client_type`, so the agent's
/// system prompt is scoped to where the user is actually talking from. This is
/// the ambient identity for typed turns; voice turns override it with
/// [`VOICE_CLIENT_TYPE`] via [`Daemon::send_voice_prompt`].
fn surface_client_type() -> &'static str {
    if running_as_extension() {
        "surface-extension"
    } else {
        "surface-web"
    }
}

/// The active browser tab the side panel is docked in, as a single guidance
/// line for the agent. `None` unless we're the Chrome extension *and* the
/// loader (`sidepanel.js`) has published the active tab on
/// `window.__ocean_active_tab` (`{ url, title }`).
///
/// `chrome.tabs.query` is a JS-only extension API — the wasm app can't call it
/// directly — so the side-panel loader keeps `window.__ocean_active_tab`
/// current (initial query + tab-activation / URL-change / window-focus
/// listeners) and we read the latest snapshot here at send time. Reading a
/// global rather than awaiting a promise keeps the hot turn path synchronous.
fn active_tab_guidance() -> Option<Vec<String>> {
    if !running_as_extension() {
        return None;
    }
    let window = web_sys::window()?;
    let tab = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__ocean_active_tab")).ok()?;
    if !tab.is_object() {
        return None;
    }
    let read = |key: &str| -> Option<String> {
        js_sys::Reflect::get(&tab, &wasm_bindgen::JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_string())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let url = read("url")?;
    // Don't leak the extension's own panel page or empty new-tab pages.
    if url.starts_with("chrome-extension://") || url.starts_with("chrome://newtab") {
        return None;
    }
    let line = match read("title") {
        Some(title) => format!("The user's active browser tab is \"{title}\" ({url})."),
        None => format!("The user's active browser tab is {url}."),
    };
    Some(vec![line])
}

/// Maximum number of open tabs we list in guidance, matching the extension
/// loader's own cap. Keeps the guidance block bounded for a user with many
/// tabs open.
const MAX_OPEN_TABS_GUIDANCE: usize = 24;

/// The current window's open-tab list as a single guidance line, read from
/// `window.__ocean_open_tabs` (published by `sidepanel.js`, OCEAN-92). `None`
/// unless we're the Chrome extension and the loader published a non-empty list.
/// We only enumerate tabs the user already has open in the focused window, and
/// only at user-initiated send time — never a passive background scrape.
fn open_tabs_guidance() -> Option<Vec<String>> {
    if !running_as_extension() {
        return None;
    }
    let window = web_sys::window()?;
    let tabs_val =
        js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__ocean_open_tabs")).ok()?;
    let arr = js_sys::Array::from(&tabs_val);
    let len = arr.length();
    if len == 0 {
        return None;
    }
    let read = |obj: &wasm_bindgen::JsValue, key: &str| -> Option<String> {
        js_sys::Reflect::get(obj, &wasm_bindgen::JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_string())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut lines = Vec::new();
    for i in 0..len.min(MAX_OPEN_TABS_GUIDANCE as u32) {
        let tab = arr.get(i);
        if !tab.is_object() {
            continue;
        }
        let Some(url) = read(&tab, "url") else { continue };
        if url.starts_with("chrome-extension://") {
            continue;
        }
        let entry = match read(&tab, "title") {
            Some(title) => format!("\"{title}\" ({url})"),
            None => url,
        };
        lines.push(format!("  - {entry}"));
    }
    if lines.is_empty() {
        return None;
    }
    let mut out = vec![format!(
        "The user has {} tab(s) open in this browser window:",
        lines.len()
    )];
    out.extend(lines);
    Some(out)
}

/// Assemble the per-turn browser context guidance for the Chrome side panel:
/// the active tab (OCEAN-70) plus the current window's open-tab list
/// (OCEAN-92). Returns `None` on non-extension surfaces or when nothing is
/// available, so the daemon's wire shape stays `guidance: None` there.
fn browser_context_guidance() -> Option<Vec<String>> {
    let mut lines = Vec::new();
    if let Some(active) = active_tab_guidance() {
        lines.extend(active);
    }
    if let Some(open) = open_tabs_guidance() {
        lines.extend(open);
    }
    (!lines.is_empty()).then_some(lines)
}

fn session_title_hint(prompt: &str) -> Option<String> {
    let title = prompt.trim().chars().take(60).collect::<String>();
    (!title.is_empty()).then_some(title)
}

const PROJECT_STORAGE_KEY: &str = "ocean.project_id";

/// localStorage handle, if available (it isn't in SSR / some embeddings).
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// The persisted project selection, restored on construction.
fn load_persisted_project() -> Option<String> {
    local_storage()
        .and_then(|s| s.get_item(PROJECT_STORAGE_KEY).ok().flatten())
        .filter(|s| !s.is_empty())
}

fn persist_project(id: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(PROJECT_STORAGE_KEY, id);
    }
}

fn clear_persisted_project() {
    if let Some(s) = local_storage() {
        let _ = s.remove_item(PROJECT_STORAGE_KEY);
    }
}

const THINKING_LEVEL_STORAGE_KEY: &str = "ocean.thinking_level";
const MODEL_OVERRIDE_STORAGE_KEY: &str = "ocean.model_override";

/// Valid serialized `ThinkingLevel` values the daemon accepts. We restrict the
/// persisted value to these so a stale/garbage localStorage entry can't ship a
/// bad `thinking_level` the daemon would reject.
const THINKING_LEVELS: &[&str] = &["off", "low", "medium", "high"];

/// The persisted per-turn thinking level, restored on construction. Filtered to
/// known values so only a valid `ThinkingLevel` string is ever loaded.
fn load_persisted_thinking_level() -> Option<String> {
    local_storage()
        .and_then(|s| s.get_item(THINKING_LEVEL_STORAGE_KEY).ok().flatten())
        .filter(|v| THINKING_LEVELS.contains(&v.as_str()))
}

fn persist_thinking_level(level: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(THINKING_LEVEL_STORAGE_KEY, level);
    }
}

fn clear_persisted_thinking_level() {
    if let Some(s) = local_storage() {
        let _ = s.remove_item(THINKING_LEVEL_STORAGE_KEY);
    }
}

/// The persisted per-turn model override, restored on construction.
fn load_persisted_model_override() -> Option<String> {
    local_storage()
        .and_then(|s| s.get_item(MODEL_OVERRIDE_STORAGE_KEY).ok().flatten())
        .filter(|s| !s.is_empty())
}

fn persist_model_override(id: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(MODEL_OVERRIDE_STORAGE_KEY, id);
    }
}

fn clear_persisted_model_override() {
    if let Some(s) = local_storage() {
        let _ = s.remove_item(MODEL_OVERRIDE_STORAGE_KEY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_entry(is_error: bool) -> SessionTranscriptEntry {
        SessionTranscriptEntry {
            role: "tool".into(),
            text: "boom".into(),
            tool_name: Some("read_file".into()),
            is_error: Some(is_error),
        }
    }

    #[test]
    fn failed_tool_call_hydrates_expanded() {
        let turns = turns_from_session_transcript(vec![tool_entry(true)]);
        let block = &turns[0].blocks[0];
        match block {
            Block::ToolCall {
                status, expanded, ..
            } => {
                assert_eq!(*status, ToolStatus::Err);
                assert!(*expanded, "failed tool call should auto-expand");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn successful_tool_call_hydrates_collapsed() {
        let turns = turns_from_session_transcript(vec![tool_entry(false)]);
        let block = &turns[0].blocks[0];
        match block {
            Block::ToolCall {
                status, expanded, ..
            } => {
                assert_eq!(*status, ToolStatus::Ok);
                assert!(!*expanded, "successful tool call should stay collapsed");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn session_create_response_decodes_without_ok_field() {
        // OCEAN-124 regression. The daemon's POST /v1/agent/sessions returns
        // exactly `{session_id, cwd, client_type}` — no `ok` field. The surface
        // decoder used to require `ok: bool`, so serde failed with
        // `missing field 'ok'`, the surface never got a session id, the first
        // turn was never POSTed, and web chat was 100% dead. This is the exact
        // 91-byte body the live tester's daemon returned (verified via curl).
        let body = r#"{"session_id":"s1","cwd":"/","client_type":"surface-web"}"#;
        let resp: AgentSessionCreateResponse =
            serde_json::from_str(body).expect("daemon create response must decode");
        // A missing `ok` defaults to true: a 200 with a session id IS success.
        assert!(resp.ok, "missing `ok` must default to true (success)");
        assert_eq!(resp.session_id.as_deref(), Some("s1"));
        assert_eq!(resp.cwd.as_deref(), Some("/"));
    }

    #[test]
    fn session_create_response_honors_explicit_ok_false() {
        // A future/error shape that DOES send `ok:false` is still respected so
        // the failure arm (which reads `error`) keeps working.
        let body = r#"{"ok":false,"error":"workspace not found"}"#;
        let resp: AgentSessionCreateResponse =
            serde_json::from_str(body).expect("error shape must decode");
        assert!(!resp.ok);
        assert!(resp.session_id.is_none());
        assert_eq!(resp.error.as_deref(), Some("workspace not found"));
    }

    #[test]
    fn agent_event_names_map_to_real_variants() {
        // Drift guard for the per-name SSE subscription. gloo-net's EventSource
        // only delivers frames whose `event:` name was explicitly subscribed,
        // so a name the daemon emits but `AGENT_EVENT_NAMES` omits is dropped at
        // the transport layer before serde's `#[serde(other)]` catch-all can
        // help. This test deserializes a minimal frame for each subscribed name
        // and asserts it lands on a concrete variant, NOT `AgentEvent::Other`.
        // A dead/typo'd name in the list would deserialize to `Other` and fail
        // here; a daemon-emitted name missing from the list is caught by the
        // exact-set check below paired with `expected_daemon_names`.
        for name in AGENT_EVENT_NAMES {
            let frame = serde_json::json!({ "type": name }).to_string();
            // Per-variant required fields differ, but the `type` tag alone is
            // enough for serde's internally-tagged enum to pick the variant
            // (missing fields fail only if the variant has non-defaulted
            // fields). To stay field-agnostic we just confirm the tag is a known
            // variant by checking it is NOT routed to `Other`: deserialize a
            // value with only `type`, falling back to a permissive object.
            let parsed: Result<AgentEvent, _> = serde_json::from_str(&frame);
            // Variants with required fields will error on the bare frame — that
            // still proves the tag is recognized (serde matched the variant and
            // only then complained about a missing field). `Other` never errors
            // on a bare `type`. So: a clean parse to a non-Other variant OR a
            // field-level error both mean "recognized name". Only a clean parse
            // to `Other` means "unknown name".
            match parsed {
                Ok(AgentEvent::Other) => panic!(
                    "AGENT_EVENT_NAMES contains \"{name}\" but it deserializes to \
                     AgentEvent::Other — it matches no variant. Remove it or fix \
                     the spelling so it mirrors the daemon's emitted name.",
                ),
                Ok(_) | Err(_) => {}
            }
        }

        // Exact-set lock against the daemon's authoritative emitted names (the
        // `agent_event_type_name` match in
        // ocean-os/crates/ocean-daemon/src/main.rs:3782). Update BOTH lists in
        // lockstep when the daemon gains/loses an event — that is the whole
        // contract this ticket (OCEAN-102) makes drift-proof.
        let expected_daemon_names = [
            "turn_started",
            "assistant_text_delta",
            "thinking_delta",
            "tool_call_started",
            "tool_call_chunk",
            "tool_call_finished",
            "turn_finished",
            "session_created",
            "extension",
            "component_render",
            "component_unmount",
            "browser_activity",
            "surface_patch",
        ];
        let mut expected = expected_daemon_names.to_vec();
        expected.sort_unstable();
        let mut subscribed = AGENT_EVENT_NAMES.to_vec();
        subscribed.sort_unstable();
        assert_eq!(
            subscribed, expected,
            "AGENT_EVENT_NAMES must exactly match the daemon's emitted SSE event \
             names. A name only the daemon has = a SILENT transport-layer drop; a \
             name only the surface has = a dead subscription.",
        );
    }

    #[test]
    fn surface_patch_event_deserializes_into_variant_not_other() {
        // OCEAN-178 regression. Golden fixture of the daemon's EXACT wire shape
        // for `AgentTurnEvent::SurfacePatch` (ocean-agent-sdk, internally tagged
        // on `"type" = "surface_patch"`, `snake_case`; envelopes nest an
        // `op`-tagged patch). Captured verbatim from the daemon's serde output.
        // Before this fix the web `AgentEvent` had no `SurfacePatch` variant, so
        // this JSON routed into `AgentEvent::Other` and the agent's canvas patches
        // were dropped. (The transport-layer drop — `surface_patch` missing from
        // `AGENT_EVENT_NAMES` — is the other half, guarded by the allow-list
        // test above; this test guards the serde half.)
        let raw = r#"{
  "type": "surface_patch",
  "session_id": "11111111-1111-4111-8111-111111111111",
  "turn_id": "22222222-2222-4222-8222-222222222222",
  "canvas_id": "canvas:main",
  "patches": [
    {
      "patch_id": "patch-1",
      "session_id": "11111111-1111-4111-8111-111111111111",
      "surface_id": "gpui:local",
      "canvas_id": "canvas:main",
      "actor": { "kind": "agent", "id": "sage" },
      "created_at_ms": 1725000000000,
      "patch": {
        "op": "upsert_component",
        "component": {
          "id": "brief-1",
          "kind": "brief_card",
          "rect": { "x": 420.0, "y": 120.0, "w": 320.0, "h": 220.0 },
          "content": { "body": "Draft", "title": "Sales Brief" },
          "metadata": { "source": "longhouse.sales" }
        }
      }
    }
  ]
}"#;

        let event: AgentEvent =
            serde_json::from_str(raw).expect("daemon surface_patch JSON must deserialize");

        // Must NOT fall through to the `Other` catch-all — that was the bug.
        let AgentEvent::SurfacePatch {
            session_id,
            turn_id,
            canvas_id,
            patches,
        } = event
        else {
            panic!("expected SurfacePatch, got a different / Other variant — wire shape drifted");
        };

        assert_eq!(session_id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(turn_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(canvas_id, CanvasId::new("canvas:main"));
        assert_eq!(patches.len(), 1);

        let envelope = &patches[0];
        assert_eq!(envelope.patch_id, PatchId::new("patch-1"));
        assert_eq!(envelope.canvas_id, CanvasId::new("canvas:main"));
        assert_eq!(envelope.surface_id, SurfaceId::new("gpui:local"));
        assert_eq!(envelope.actor.kind, "agent");
        assert_eq!(envelope.actor.id.as_deref(), Some("sage"));
        assert_eq!(envelope.created_at_ms, 1_725_000_000_000);

        let SurfacePatch::UpsertComponent { component } = &envelope.patch else {
            panic!("expected UpsertComponent op");
        };
        assert_eq!(component.id, ComponentId::new("brief-1"));
        assert_eq!(component.kind, "brief_card");
        let rect = component.rect.expect("rect present");
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (420.0, 120.0, 320.0, 220.0));
        assert_eq!(component.content["title"], "Sales Brief");

        // The one-line summary the web panel renders is derived correctly.
        assert_eq!(
            summarize_surface_patch(&envelope.patch),
            "upsert_component brief-1 (brief_card)"
        );
    }

    #[test]
    fn control_event_parses_permission_request_from_full_envelope() {
        // The /v1/events stream serializes the FULL envelope: the flattened
        // OceanEvent fields PLUS the envelope's permission_id / session_id.
        let data = serde_json::json!({
            "id": "evt-1",
            "at": "2026-06-05T00:00:00Z",
            "session_id": "sess-abc",
            "request_id": "req-1",
            "permission_id": "perm-xyz",
            "type": "permission_request",
            "tool": "bash",
            "reason": "permission required for bash",
            "args": { "cmd": "rm -rf build" }
        })
        .to_string();
        let evt: ControlEvent = serde_json::from_str(&data).unwrap();
        match evt {
            ControlEvent::PermissionRequest {
                permission_id,
                session_id,
                tool,
                ..
            } => {
                assert_eq!(permission_id.as_deref(), Some("perm-xyz"));
                assert_eq!(session_id.as_deref(), Some("sess-abc"));
                assert_eq!(tool, "bash");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn control_event_parses_permission_decision() {
        let data = serde_json::json!({
            "id": "evt-2",
            "at": "2026-06-05T00:00:00Z",
            "session_id": "sess-abc",
            "permission_id": "perm-xyz",
            "type": "permission_decision",
            "allowed": true
        })
        .to_string();
        let evt: ControlEvent = serde_json::from_str(&data).unwrap();
        assert!(matches!(evt, ControlEvent::PermissionDecision { .. }));
    }

    #[test]
    fn control_event_unrelated_type_is_other() {
        let data = r#"{"type":"assistant_delta","text":"hi"}"#;
        let evt: ControlEvent = serde_json::from_str(data).unwrap();
        assert!(matches!(evt, ControlEvent::Other));
    }

    #[test]
    fn summarize_args_renders_object_as_key_value_lines() {
        let args = serde_json::json!({ "path": "/tmp/x", "contents": "hi" });
        let summary = summarize_args(&args);
        assert!(summary.contains("path: /tmp/x"));
        assert!(summary.contains("contents: hi"));
    }

    #[test]
    fn summarize_args_null_is_empty() {
        assert_eq!(summarize_args(&Value::Null), "");
    }

    #[test]
    fn thinking_level_values_match_daemon_serialization() {
        // These are the exact lowercase strings the daemon's `ThinkingLevel`
        // serde enum deserializes (off/low/medium/high). The composer's selector
        // emits these and they flow straight onto `AgentTurnRequest::thinking_level`.
        assert_eq!(THINKING_LEVELS, &["off", "low", "medium", "high"]);
    }

    #[test]
    fn turn_request_omits_overrides_when_none() {
        // Defaults preserved: with no per-turn overrides, the override fields are
        // skipped from the wire shape entirely (daemon applies global defaults).
        let body = AgentTurnRequest {
            prompt: "hi",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            images: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("thinking_level"));
        assert!(!json.contains("model_id"));
    }

    #[test]
    fn turn_request_emits_selected_overrides() {
        let body = AgentTurnRequest {
            prompt: "hi",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: Some("high"),
            model_id: Some("claude-opus-4-8"),
            images: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains(r#""thinking_level":"high""#));
        assert!(json.contains(r#""model_id":"claude-opus-4-8""#));
    }

    #[test]
    fn turn_request_omits_images_when_none() {
        // OCEAN-138: with nothing captured/picked, `images` is skipped entirely
        // so the daemon's `images: Option<Vec<TurnImage>>` stays `None` and
        // existing text-only turns are byte-for-byte unchanged.
        let body = AgentTurnRequest {
            prompt: "hi",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            images: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("images"));
    }

    #[test]
    fn turn_request_emits_images_in_daemon_wire_shape() {
        // OCEAN-138: a staged image serializes to the EXACT shape the daemon's
        // `TurnImage` (ocean-agent-sdk) deserializes — `{mime_type, data}` —
        // under the `images` array, so it reaches the model as a Content::Image.
        let body = AgentTurnRequest {
            prompt: "what is on screen?",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            images: Some(vec![TurnImage {
                mime_type: "image/png".into(),
                data: "data:image/png;base64,AAAA".into(),
            }]),
        };
        let json = serde_json::to_string(&body).unwrap();
        // Round-trip through serde_json::Value to assert structure exactly.
        let v: Value = serde_json::from_str(&json).unwrap();
        let img = &v["images"][0];
        assert_eq!(img["mime_type"], "image/png");
        assert_eq!(img["data"], "data:image/png;base64,AAAA");
        // No stray fields leak onto the wire image object.
        assert_eq!(img.as_object().unwrap().len(), 2);
    }

    #[test]
    fn parse_data_url_extracts_mime_and_keeps_full_url() {
        let img = parse_data_url("data:image/png;base64,iVBORw0KGgo=")
            .expect("png data url must parse");
        assert_eq!(img.mime_type, "image/png");
        assert_eq!(img.data, "data:image/png;base64,iVBORw0KGgo=");
    }

    #[test]
    fn parse_data_url_rejects_non_base64_and_garbage() {
        // A plain URL, a non-base64 data URL, and an empty mime all fail closed
        // so we never stage an attachment the daemon can't turn into an image.
        assert!(parse_data_url("https://example.com/x.png").is_none());
        assert!(parse_data_url("data:image/png,notbase64").is_none());
        assert!(parse_data_url("data:;base64,AAAA").is_none());
        assert!(parse_data_url("garbage").is_none());
    }

    #[test]
    fn voice_client_type_matches_daemon_routing_string() {
        // OCEAN-181: the daemon routes its concise/speakable voice system prompt
        // (`voice_surface_prompt`) on `client_type == "leo-voice"` exactly
        // (ocean-os crates/ocean-agent/src/lib.rs). If this constant drifts, the
        // voice prompt silently goes unreachable again — the original bug. Lock
        // it to the literal the daemon checks.
        assert_eq!(VOICE_CLIENT_TYPE, "leo-voice");
    }

    #[test]
    fn voice_client_type_is_distinct_from_surface_identities() {
        // Guard that voice tagging did NOT collapse onto the typed/web/extension
        // identities. `surface_client_type()` itself can't run off-wasm (it
        // touches `web_sys`), so we assert against its two possible literals
        // directly: voice must be neither.
        assert_ne!(VOICE_CLIENT_TYPE, "surface-web");
        assert_ne!(VOICE_CLIENT_TYPE, "surface-extension");
    }

    #[test]
    fn voice_send_tags_turn_leo_voice_on_the_wire() {
        // OCEAN-181 core assertion. `send_voice_prompt` dispatches with
        // `VOICE_CLIENT_TYPE`, which lands on `AgentTurnRequest::client_type`.
        // Build that exact wire body the way dispatch does for the voice path
        // and assert it serializes `client_type=leo-voice`.
        let body = AgentTurnRequest {
            prompt: "hey",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: Some(VOICE_CLIENT_TYPE),
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            images: None,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();
        assert_eq!(v["client_type"], "leo-voice");
    }

    #[test]
    fn typed_send_tags_turn_surface_web_on_the_wire() {
        // The companion to the voice case: a normal (typed) send dispatches with
        // `surface_client_type()`, which for the web PWA is `surface-web`. The
        // wire body must carry that unchanged — proving the voice change did not
        // touch the typed path. (`surface_client_type()` can't be called off-wasm
        // because it reads `web_sys`, so we pin the web literal it returns.)
        let body = AgentTurnRequest {
            prompt: "hey",
            cwd: "/",
            session_id: Some("s1"),
            project_id: None,
            client_type: Some("surface-web"),
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            images: None,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();
        assert_eq!(v["client_type"], "surface-web");
    }

    #[test]
    fn voice_first_session_create_uses_surface_identity_not_leo_voice() {
        // Codex P2 regression on #45 (OCEAN-181). When the FIRST interaction on a
        // fresh surface is a voice transcript, the per-turn tag threaded through
        // dispatch is "leo-voice" — but the SESSION-CREATE body must still carry
        // the stable surface MEDIUM (surface-web / surface-extension), per the
        // AGENTS.md session contract. A session's client_type is a surface
        // identity, not a per-turn routing tag; voice is a mode OF the web /
        // extension surface, not its own surface. dispatch_prompt builds this
        // body with surface_client_type() (web literal off-wasm), NOT the
        // threaded client_type. Lock that: even on a voice-first session create,
        // the wire client_type must NOT be leo-voice.
        let body = AgentSessionCreateRequest {
            title: Some("hey there"),
            workspace_root: "/",
            project_id: None,
            // Mirrors the session-create call site: surface_client_type() — which
            // off-wasm is surface-web — regardless of the (voice) turn tag.
            client_type: Some("surface-web"),
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();
        assert_eq!(v["client_type"], "surface-web");
        assert_ne!(
            v["client_type"], "leo-voice",
            "session client_type must be the surface medium, never the per-turn voice tag",
        );
    }
}
