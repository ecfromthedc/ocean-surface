use std::env;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agent::AgentEvent;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(4);
const TURN_TIMEOUT: Duration = Duration::from_secs(180);
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4780";

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub version: String,
    pub backend: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CurrentModel {
    pub model: String,
    #[serde(default)]
    pub provider: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModelsResponse {
    pub ok: bool,
    #[serde(default)]
    pub current: Option<CurrentModel>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProjectInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub workspace_root: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProjectsResponse {
    pub ok: bool,
    #[serde(default)]
    pub projects: Vec<ProjectInfo>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LiveKitTokenRequest {
    pub surface_id: String,
    pub participant_id: String,
    pub display_name: String,
    pub can_publish: bool,
    pub can_subscribe: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LiveKitTokenResponse {
    pub ok: bool,
    pub url: String,
    pub room: String,
    pub token: String,
    pub expires_at: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SessionsResponse {
    pub ok: bool,
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentSessionCreateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Workspace anchor for the session. The daemon's
    /// `AgentSessionCreateRequest` deserializes this as a **required**
    /// `workspace_root` field (no serde alias for `cwd`) — sending `cwd` here
    /// made POST /v1/agent/sessions fail to deserialize, silently breaking
    /// surface session creation. Send `workspace_root` to match (OCEAN-62b).
    pub workspace_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AgentSessionCreateResponse {
    pub ok: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SessionDetailResponse {
    pub ok: bool,
    #[serde(default)]
    pub session: Option<SessionDetail>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SessionDetail {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript: Vec<SessionTranscriptEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SessionTranscriptEntry {
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PermissionStatus {
    pub permission_id: String,
    /// The originating request id. Populated by the `/v1/permissions` poll
    /// snapshot; the control-stream `permission_request` envelope (OCEAN-75)
    /// doesn't carry one, so it defaults to empty when a card is built live.
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub tool: String,
    pub reason: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub created_at: String,
}

/// The control-plane event envelope streamed on `/v1/events`. Unlike
/// `/v1/agent/events` (which serializes only the inner `AgentTurnEvent` and so
/// DROPS the envelope's `permission_id`), this stream serializes the FULL
/// `EventEnvelope`, so `permission_id` / `session_id` ride alongside the
/// flattened `OceanEvent`. The GPUI shell only models the two permission frames
/// (OCEAN-75); every other `type` falls into `Other` and is ignored.
///
/// This mirrors the web surface's `ControlEvent` (OCEAN-64) so the desktop and
/// web surfaces decode the same daemon wire shape.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PermissionsResponse {
    pub ok: bool,
    #[serde(default)]
    pub permissions: Vec<PermissionStatus>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PermissionDecisionRequest {
    pub permission_id: String,
    #[serde(flatten)]
    pub decision: PermissionDecision,
    /// The per-turn secret originally sent on the turn submission
    /// (OCEAN-185 / OCEAN-314). The daemon constant-time-compares this
    /// against the token bound to the gated turn; a missing or wrong token
    /// returns 403. Must match the `decision_token` in the corresponding
    /// `AgentTurnRequest`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_token: Option<String>,
}

impl PermissionDecisionRequest {
    #[must_use]
    pub fn allow(permission_id: impl Into<String>, decision_token: Option<String>) -> Self {
        Self {
            permission_id: permission_id.into(),
            decision: PermissionDecision::Allow,
            decision_token,
        }
    }

    #[must_use]
    pub fn deny(
        permission_id: impl Into<String>,
        reason: impl Into<String>,
        decision_token: Option<String>,
    ) -> Self {
        Self {
            permission_id: permission_id.into(),
            decision: PermissionDecision::Deny {
                reason: Some(reason.into()),
            },
            decision_token,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RequestControlResponse {
    pub ok: bool,
    pub request_id: String,
    pub state: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PermissionControlResponse {
    pub ok: bool,
    pub permission_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ComponentEventRequest {
    pub session_id: String,
    pub component_id: String,
    pub event: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ComponentEventResponse {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub component_id: Option<String>,
}

// ---- Persistent rooms (OCEAN-109, the native counterpart to OCEAN-108) -------
//
// The daemon's persistent-rooms surface lives under `/v1/rooms/persistent/*`
// (OCEAN-65). A room is a durable, named collaboration space with a participant
// roster and an append-only transcript. These wire types mirror
// `ocean_core::Room` / `RoomMessage` / `RoomParticipant` (snake_case on the
// wire) — the same shapes the web surface decodes in `ocean-surface-ui`'s
// `rooms.rs`. `ocean-core` is not a dependency of this surface crate (it lives
// in the daemon repo), so we carry our own decode structs, exactly as the web
// surface does.

/// What kind of actor a participant / message author is. Mirrors
/// `ocean_core::RoomParticipantKind`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomParticipantKind {
    Human,
    Agent,
    Bot,
    Tool,
    System,
}

impl RoomParticipantKind {
    /// A short glyph for the author/roster chip. Agents get the 🤖 glyph so the
    /// roster makes it visually obvious who is auto-convene-able (OCEAN-119,
    /// matching the web surface's roster in OCEAN-117); the rest stay as compact
    /// ASCII markers.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            RoomParticipantKind::Human => "H",
            RoomParticipantKind::Agent => "🤖",
            RoomParticipantKind::Bot => "B",
            RoomParticipantKind::Tool => "T",
            RoomParticipantKind::System => "*",
        }
    }

    /// A lowercase word for the kind — shown next to the glyph so the roster makes
    /// it explicit who's an agent (i.e. auto-convene-able) vs. a human. Mirrors
    /// the web surface's `RoomParticipantKind::label` (OCEAN-117).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RoomParticipantKind::Human => "human",
            RoomParticipantKind::Agent => "agent",
            RoomParticipantKind::Bot => "bot",
            RoomParticipantKind::Tool => "tool",
            RoomParticipantKind::System => "system",
        }
    }
}

/// One participant in a room's roster. Mirrors `ocean_core::RoomParticipant`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct RoomParticipant {
    pub id: String,
    pub kind: RoomParticipantKind,
    pub display_name: String,
}

/// What kind of transcript entry a message is. Mirrors
/// `ocean_core::RoomMessageKind`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomMessageKind {
    Message,
    ParticipantJoined,
    ParticipantLeft,
    System,
}

impl RoomMessageKind {
    /// Whether this entry is an informational/system line rather than a posted
    /// message (rendered muted, without an author chip).
    #[must_use]
    pub fn is_system(self) -> bool {
        !matches!(self, RoomMessageKind::Message)
    }
}

/// One transcript entry. Mirrors `ocean_core::RoomMessage`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct RoomMessage {
    pub seq: u64,
    pub author_id: String,
    pub author_kind: RoomParticipantKind,
    pub kind: RoomMessageKind,
    pub body: String,
    #[serde(default)]
    pub created_at: String,
}

/// How a room's agents are auto-woken. Mirrors `ocean_core::RoomTriggerPolicy`.
/// All flags default off; the daemon reads this on `room_create` and evaluates
/// it on every non-agent-authored message (OCEAN-65 / OCEAN-111). Set once at
/// create time from the GPUI panel's toggles (OCEAN-119), matching the web
/// surface (OCEAN-117).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoomTriggerPolicy {
    /// Wake an agent when it is @-mentioned in the transcript (the common case).
    #[serde(default)]
    pub on_mention: bool,
    /// Wake an agent when someone replies in a thread it participates in.
    #[serde(default)]
    pub on_thread_reply: bool,
    /// Wake an agent when a rendered component emits an interaction event.
    #[serde(default)]
    pub on_component_event: bool,
    /// Optional cron expression for scheduled wake-ups. `None`/empty = no schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_schedule: Option<String>,
}

/// A persistent room. Mirrors `ocean_core::Room` (we read only the fields the
/// panel renders).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct Room {
    /// The room key. `ocean_core::RoomKey` serializes as a bare string, so this
    /// deserializes directly.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub participants: Vec<RoomParticipant>,
    #[serde(default)]
    pub created_at: String,
    /// Last change to roster/metadata/transcript — shown as "last activity".
    #[serde(default)]
    pub updated_at: String,
    /// Optional auto-convene trigger policy. `None` = no automatic triggers.
    #[serde(default)]
    pub trigger_policy: Option<RoomTriggerPolicy>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RoomsListResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub rooms: Vec<Room>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RoomGetResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub room: Option<Room>,
    #[serde(default)]
    pub transcript: Vec<RoomMessage>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RoomMutateResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub room: Option<Room>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RoomTranscriptResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub transcript: Vec<RoomMessage>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CreateRoomRequest {
    pub key: String,
    pub name: String,
    /// Optional trigger policy. Skipped when `None` so the daemon's
    /// `#[serde(default)]` (no triggers) applies; otherwise the daemon stores it
    /// verbatim. The daemon already accepts this at create (OCEAN-117 verified),
    /// so wiring it from GPUI needs no daemon change (OCEAN-119).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_policy: Option<RoomTriggerPolicy>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RoomJoinRequest {
    pub id: String,
    pub display_name: String,
    pub kind: RoomParticipantKind,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RoomPostMessageRequest {
    pub author_id: String,
    pub author_kind: RoomParticipantKind,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DaemonHealth {
    Checking,
    Ready(HealthResponse),
    Offline(String),
}

#[derive(Clone, Debug)]
pub struct NativeDaemonState {
    pub url: String,
    pub health: DaemonHealth,
    pub last_checked: Option<Instant>,
}

impl NativeDaemonState {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            url: env::var("OCEAN_DAEMON_URL")
                .ok()
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string()),
            health: DaemonHealth::Checking,
            last_checked: None,
        }
    }

    pub fn mark_checking(&mut self) {
        self.health = DaemonHealth::Checking;
        self.last_checked = Some(Instant::now());
    }

    pub fn apply_health(&mut self, health: DaemonHealth) {
        self.health = health;
        self.last_checked = Some(Instant::now());
    }

    #[must_use]
    pub fn status_label(&self) -> String {
        match &self.health {
            DaemonHealth::Checking => "checking".to_string(),
            DaemonHealth::Ready(health) if health.ok => "online".to_string(),
            DaemonHealth::Ready(_) => "degraded".to_string(),
            DaemonHealth::Offline(_) => "offline".to_string(),
        }
    }

    #[must_use]
    pub fn backend_label(&self) -> String {
        match &self.health {
            DaemonHealth::Ready(health) => health.backend.clone(),
            DaemonHealth::Checking => "pending".to_string(),
            DaemonHealth::Offline(error) => error.clone(),
        }
    }
}

#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::blocking::Client,
}

impl DaemonClient {
    pub fn new() -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { http })
    }

    pub fn health(&self, base_url: &str) -> DaemonHealth {
        let url = health_url(base_url);
        match self
            .http
            .get(url)
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<HealthResponse>())
        {
            Ok(health) => DaemonHealth::Ready(health),
            Err(error) => DaemonHealth::Offline(error.to_string()),
        }
    }

    pub fn submit_turn(
        &self,
        base_url: &str,
        request: &AgentTurnRequest,
    ) -> Result<AgentTurnResponse, String> {
        let url = agent_turns_url(base_url);
        self.http
            .post(url)
            .timeout(TURN_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<AgentTurnResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn create_session(
        &self,
        base_url: &str,
        request: &AgentSessionCreateRequest,
    ) -> Result<AgentSessionCreateResponse, String> {
        self.http
            .post(agent_session_create_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<AgentSessionCreateResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn fetch_models(&self, base_url: &str) -> Result<ModelsResponse, String> {
        self.http
            .get(models_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<ModelsResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn fetch_projects(&self, base_url: &str) -> Result<ProjectsResponse, String> {
        self.http
            .get(projects_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<ProjectsResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn livekit_token(
        &self,
        base_url: &str,
        room_id: &str,
        request: &LiveKitTokenRequest,
    ) -> Result<LiveKitTokenResponse, String> {
        self.http
            .post(livekit_token_url(base_url, room_id))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<LiveKitTokenResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn set_model(&self, base_url: &str, id: &str) -> Result<(), String> {
        self.http
            .post(model_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .json(&ModelSetRequest {
                model: id.to_string(),
            })
            .send()
            .and_then(|response| response.error_for_status())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn fetch_sessions(&self, base_url: &str) -> Result<SessionsResponse, String> {
        self.http
            .get(agent_sessions_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<SessionsResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn fetch_session(
        &self,
        base_url: &str,
        session_id: &str,
    ) -> Result<SessionDetailResponse, String> {
        self.http
            .get(session_detail_url(base_url, session_id))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<SessionDetailResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn fetch_permissions(&self, base_url: &str) -> Result<PermissionsResponse, String> {
        self.http
            .get(permissions_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<PermissionsResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn cancel_request(
        &self,
        base_url: &str,
        request_id: &str,
    ) -> Result<RequestControlResponse, String> {
        self.http
            .post(request_cancel_url(base_url, request_id))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RequestControlResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn decide_permission(
        &self,
        base_url: &str,
        request: &PermissionDecisionRequest,
    ) -> Result<PermissionControlResponse, String> {
        self.http
            .post(permission_decision_url(base_url, &request.permission_id))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<PermissionControlResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn send_component_event(
        &self,
        base_url: &str,
        request: &ComponentEventRequest,
    ) -> Result<ComponentEventResponse, String> {
        self.http
            .post(component_event_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<ComponentEventResponse>()
            .map_err(|error| error.to_string())
    }

    // ---- Persistent rooms (OCEAN-109) ----------------------------------------

    /// List persistent rooms (`GET /v1/rooms/persistent`).
    pub fn fetch_rooms(&self, base_url: &str) -> Result<RoomsListResponse, String> {
        self.http
            .get(rooms_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomsListResponse>()
            .map_err(|error| error.to_string())
    }

    /// Create a room (`POST /v1/rooms/persistent`).
    pub fn create_room(
        &self,
        base_url: &str,
        request: &CreateRoomRequest,
    ) -> Result<RoomMutateResponse, String> {
        self.http
            .post(rooms_url(base_url))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomMutateResponse>()
            .map_err(|error| error.to_string())
    }

    /// Load a room record + full transcript (`GET /v1/rooms/persistent/{key}`).
    pub fn fetch_room(&self, base_url: &str, key: &str) -> Result<RoomGetResponse, String> {
        self.http
            .get(room_url(base_url, key))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomGetResponse>()
            .map_err(|error| error.to_string())
    }

    /// Tail a room's transcript after `after_seq`
    /// (`GET /v1/rooms/persistent/{key}/transcript?after_seq=N`).
    pub fn fetch_room_transcript(
        &self,
        base_url: &str,
        key: &str,
        after_seq: u64,
    ) -> Result<RoomTranscriptResponse, String> {
        self.http
            .get(room_transcript_url(base_url, key, after_seq))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomTranscriptResponse>()
            .map_err(|error| error.to_string())
    }

    /// Join a room (`POST /v1/rooms/persistent/{key}/participants`).
    pub fn join_room(
        &self,
        base_url: &str,
        key: &str,
        request: &RoomJoinRequest,
    ) -> Result<RoomMutateResponse, String> {
        self.http
            .post(room_participants_url(base_url, key))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomMutateResponse>()
            .map_err(|error| error.to_string())
    }

    /// Leave a room (`DELETE /v1/rooms/persistent/{key}/participants/{id}`).
    pub fn leave_room(
        &self,
        base_url: &str,
        key: &str,
        participant_id: &str,
    ) -> Result<RoomMutateResponse, String> {
        self.http
            .delete(room_participant_url(base_url, key, participant_id))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomMutateResponse>()
            .map_err(|error| error.to_string())
    }

    /// Post a message to a room (`POST /v1/rooms/persistent/{key}/messages`).
    /// `@id` mentions in the body drive the daemon's auto-convene trigger policy.
    pub fn post_room_message(
        &self,
        base_url: &str,
        key: &str,
        request: &RoomPostMessageRequest,
    ) -> Result<RoomMutateResponse, String> {
        self.http
            .post(room_messages_url(base_url, key))
            .timeout(HEALTH_TIMEOUT)
            .json(request)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?
            .json::<RoomMutateResponse>()
            .map_err(|error| error.to_string())
    }

    pub fn stream_agent_events(
        &self,
        base_url: &str,
        session_id: Option<&str>,
        on_event: impl FnMut(AgentEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        let response = self
            .http
            .get(agent_events_url(base_url, session_id))
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?;
        read_sse_events(BufReader::new(response), on_event)
    }

    /// Stream the daemon's CONTROL plane (`/v1/events`) and forward the two
    /// permission frames (OCEAN-75). This is a SEPARATE stream from
    /// `stream_agent_events`: permission frames ride the control envelope (which
    /// carries `permission_id`), not the product agent stream (which drops it).
    /// The control stream is not session-scoped server-side, so callers must
    /// filter by the envelope `session_id` themselves.
    pub fn stream_control_events(
        &self,
        base_url: &str,
        on_event: impl FnMut(ControlEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        let response = self
            .http
            .get(control_events_url(base_url))
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| error.to_string())?;
        read_sse_events(BufReader::new(response), on_event)
    }
}

#[must_use]
pub fn health_url(base_url: &str) -> String {
    format!("{}/health", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn agent_turns_url(base_url: &str) -> String {
    format!("{}/v1/agent/turns", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn agent_events_url(base_url: &str, session_id: Option<&str>) -> String {
    let base = format!("{}/v1/agent/events", base_url.trim_end_matches('/'));
    // Scope the SSE stream to one session when we know it, so the daemon only
    // ships this session's events down this connection (no cross-surface bleed).
    //
    // No session id → bare URL. Under the current daemon contract this is SAFE:
    // `/v1/agent/events` with neither `?session_id=` nor `?all=1` deliberately
    // omits all session-bearing events (it will not adopt or render another
    // surface's transcript). So an unscoped subscription receives nothing to
    // bleed. A product surface must always subscribe scoped; only operator
    // diagnostics opt into the firehose with an explicit `?all=1`.
    match session_id {
        Some(sid) if !sid.is_empty() => format!("{base}?session_id={sid}"),
        _ => base,
    }
}

#[must_use]
pub fn control_events_url(base_url: &str) -> String {
    format!("{}/v1/events", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn agent_sessions_url(base_url: &str) -> String {
    format!("{}/v1/agent/sessions", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn agent_session_create_url(base_url: &str) -> String {
    agent_sessions_url(base_url)
}

#[must_use]
pub fn models_url(base_url: &str) -> String {
    format!("{}/v1/models", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn model_url(base_url: &str) -> String {
    format!("{}/v1/model", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn projects_url(base_url: &str) -> String {
    format!("{}/v1/projects", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn livekit_token_url(base_url: &str, room_id: &str) -> String {
    format!(
        "{}/v1/rooms/{}/livekit-token",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(room_id)
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                write!(&mut encoded, "%{byte:02X}").expect("writing to string should not fail");
            }
        }
    }
    encoded
}

#[must_use]
pub fn permissions_url(base_url: &str) -> String {
    format!("{}/v1/permissions", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn request_cancel_url(base_url: &str, request_id: &str) -> String {
    format!(
        "{}/v1/requests/{}/cancel",
        base_url.trim_end_matches('/'),
        request_id
    )
}

#[must_use]
pub fn permission_decision_url(base_url: &str, permission_id: &str) -> String {
    format!(
        "{}/v1/permissions/{}/decision",
        base_url.trim_end_matches('/'),
        permission_id
    )
}

#[must_use]
pub fn component_event_url(base_url: &str) -> String {
    format!("{}/v1/component/event", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn rooms_url(base_url: &str) -> String {
    format!("{}/v1/rooms/persistent", base_url.trim_end_matches('/'))
}

#[must_use]
pub fn room_url(base_url: &str, key: &str) -> String {
    format!(
        "{}/v1/rooms/persistent/{}",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(key)
    )
}

#[must_use]
pub fn room_transcript_url(base_url: &str, key: &str, after_seq: u64) -> String {
    format!(
        "{}/v1/rooms/persistent/{}/transcript?after_seq={after_seq}",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(key)
    )
}

#[must_use]
pub fn room_participants_url(base_url: &str, key: &str) -> String {
    format!(
        "{}/v1/rooms/persistent/{}/participants",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(key)
    )
}

#[must_use]
pub fn room_participant_url(base_url: &str, key: &str, participant_id: &str) -> String {
    format!(
        "{}/v1/rooms/persistent/{}/participants/{}",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(key),
        percent_encode_path_segment(participant_id)
    )
}

#[must_use]
pub fn room_messages_url(base_url: &str, key: &str) -> String {
    format!(
        "{}/v1/rooms/persistent/{}/messages",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(key)
    )
}

#[must_use]
pub fn session_detail_url(base_url: &str, session_id: &str) -> String {
    format!(
        "{}/v1/sessions/{}",
        base_url.trim_end_matches('/'),
        session_id
    )
}

/// One image attached to a turn (OCEAN-321). Serializes to the exact wire shape
/// the daemon's `TurnImage` (ocean-agent-sdk) deserializes: a `mime_type` and a
/// base64 `data` body (a `data:<mime>;base64,` prefix is tolerated — the daemon
/// strips it). Mirrors `TurnImage` in ocean-surface-ui (OCEAN-138 / OCEAN-115).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnImage {
    /// MIME type of the image, e.g. `"image/png"` or `"image/jpeg"`.
    pub mime_type: String,
    /// Base64-encoded image bytes, or a `data:<mime>;base64,` URL (the daemon
    /// strips the prefix, keeping only the base64 body).
    pub data: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentTurnRequest {
    pub prompt: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Selected project. When set with an empty cwd, the daemon binds the turn
    /// to the project's workspace_root instead of its launch dir.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Surface marker used by the daemon to select medium-appropriate agent guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
    /// Optional guidance hints passed to the agent (e.g. "focus on tests").
    /// Matches the daemon's `AgentTurnRequest::guidance: Option<Vec<String>>`.
    /// The GPUI shell doesn't surface this yet, so it serializes as `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<Vec<String>>,
    /// Optional room identifier for Track-0 room-scoped turns. Mirrors the
    /// daemon's `room_id: Option<String>`. Not yet exposed in the GPUI shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    /// Per-turn reasoning effort override. Mirrors the daemon's
    /// `thinking_level: Option<ThinkingLevel>` — serialized as the lowercase
    /// `ThinkingLevel` string the daemon expects (e.g. "high"). `None` leaves
    /// the daemon's global default in force. Not yet exposed in the GPUI shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// Per-turn / per-session model override (OCEAN-36). Mirrors the daemon's
    /// `model_id: Option<String>`. Not yet exposed in the GPUI shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Per-turn secret binding the permission gate to this submitter
    /// (OCEAN-185 / OCEAN-314). Minted client-side and sent on the turn; the
    /// same value must be replayed on every `/v1/permissions/{id}/decision`
    /// POST for this turn or the daemon returns 403. `None` leaves the gate
    /// unbound (legacy behaviour — any caller can approve), so clients MUST
    /// populate this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_token: Option<String>,
    /// Images attached to the turn (OCEAN-321). Mirrors the daemon's
    /// `images: Option<Vec<TurnImage>>` (OCEAN-115 / OCEAN-138) — when present
    /// the daemon emits one `Content::Image` block per entry on the first user
    /// message, enabling vision end-to-end. Omitted (serde-skipped) when `None`
    /// so existing turns are fully unaffected. The GPUI image-capture/staging UI
    /// is a separate follow-on; this field is the wire contract only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<TurnImage>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Serialize, PartialEq)]
struct ModelSetRequest {
    model: String,
}

fn read_sse_events<R, T>(
    mut reader: R,
    mut on_event: impl FnMut(T) -> Result<(), String>,
) -> Result<(), String>
where
    R: BufRead,
    T: serde::de::DeserializeOwned,
{
    let mut line = String::new();
    let mut data = String::new();

    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if bytes == 0 {
            flush_sse_data(&mut data, &mut on_event)?;
            return Ok(());
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            flush_sse_data(&mut data, &mut on_event)?;
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
}

fn flush_sse_data<T>(
    data: &mut String,
    on_event: &mut impl FnMut(T) -> Result<(), String>,
) -> Result<(), String>
where
    T: serde::de::DeserializeOwned,
{
    if data.trim().is_empty() {
        data.clear();
        return Ok(());
    }

    let event = serde_json::from_str::<T>(data).map_err(|error| error.to_string())?;
    data.clear();
    on_event(event)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;

    use super::{
        AgentEvent, AgentTurnRequest, ComponentEventRequest, ControlEvent, CreateRoomRequest,
        CurrentModel, DaemonHealth, HealthResponse, LiveKitTokenRequest, LiveKitTokenResponse,
        ModelInfo, ModelsResponse, NativeDaemonState, PermissionDecisionRequest, Room,
        RoomGetResponse, RoomJoinRequest, RoomMessageKind, RoomParticipantKind,
        RoomPostMessageRequest, RoomTriggerPolicy, RoomsListResponse, TurnImage, agent_events_url,
        agent_session_create_url, agent_turns_url, component_event_url, control_events_url,
        health_url, livekit_token_url, model_url, models_url, permission_decision_url,
        permissions_url, read_sse_events, request_cancel_url, room_messages_url,
        room_participant_url, room_participants_url, room_transcript_url, room_url, rooms_url,
    };

    #[test]
    fn health_url_trims_trailing_slash() {
        assert_eq!(
            health_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/health"
        );
        assert_eq!(
            agent_turns_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/agent/turns"
        );
        assert_eq!(
            agent_events_url("http://127.0.0.1:4780/", None),
            "http://127.0.0.1:4780/v1/agent/events"
        );
        assert_eq!(
            agent_events_url("http://127.0.0.1:4780/", Some("abc-123")),
            "http://127.0.0.1:4780/v1/agent/events?session_id=abc-123"
        );
        assert_eq!(
            agent_session_create_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/agent/sessions"
        );
        assert_eq!(
            models_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/models"
        );
        assert_eq!(
            model_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/model"
        );
        assert_eq!(
            permissions_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/permissions"
        );
        assert_eq!(
            request_cancel_url("http://127.0.0.1:4780/", "req-1"),
            "http://127.0.0.1:4780/v1/requests/req-1/cancel"
        );
        assert_eq!(
            permission_decision_url("http://127.0.0.1:4780/", "perm-1"),
            "http://127.0.0.1:4780/v1/permissions/perm-1/decision"
        );
        assert_eq!(
            component_event_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/component/event"
        );
        assert_eq!(
            livekit_token_url("http://127.0.0.1:4780/", "project:surface-demo"),
            "http://127.0.0.1:4780/v1/rooms/project%3Asurface-demo/livekit-token"
        );
        assert_eq!(
            livekit_token_url("http://127.0.0.1:4780/", "project/surface demo"),
            "http://127.0.0.1:4780/v1/rooms/project%2Fsurface%20demo/livekit-token"
        );
    }

    #[test]
    fn livekit_token_body_matches_surface_contract() {
        let body = serde_json::to_value(LiveKitTokenRequest {
            surface_id: "gpui:macbook".to_string(),
            participant_id: "human:smathdaddy".to_string(),
            display_name: "Ocean operator".to_string(),
            can_publish: true,
            can_subscribe: true,
        })
        .expect("token request should serialize");

        assert_eq!(
            body,
            serde_json::json!({
                "surface_id": "gpui:macbook",
                "participant_id": "human:smathdaddy",
                "display_name": "Ocean operator",
                "can_publish": true,
                "can_subscribe": true
            })
        );
    }

    #[test]
    fn livekit_token_response_decodes_room_join_payload() {
        let response: LiveKitTokenResponse = serde_json::from_str(
            r#"{
                "ok": true,
                "url": "wss://livekit.example.com",
                "room": "ocean-room-project-surface-demo",
                "token": "jwt",
                "expires_at": "2026-06-03T20:00:00Z"
            }"#,
        )
        .expect("token response should decode");

        assert!(response.ok);
        assert_eq!(response.room, "ocean-room-project-surface-demo");
        assert_eq!(response.token, "jwt");
    }

    #[test]
    fn permission_decision_body_matches_daemon_contract() {
        // With no token (legacy path), `decision_token` is skipped from the wire
        // entirely so a pre-OCEAN-185 daemon sees no unexpected field.
        let allow = serde_json::to_value(PermissionDecisionRequest::allow("perm-1", None))
            .expect("allow should serialize");
        let deny = serde_json::to_value(PermissionDecisionRequest::deny(
            "perm-2",
            "not this one",
            None,
        ))
        .expect("deny should serialize");

        assert_eq!(
            allow,
            serde_json::json!({
                "permission_id": "perm-1",
                "decision": "allow"
            })
        );
        assert_eq!(
            deny,
            serde_json::json!({
                "permission_id": "perm-2",
                "decision": "deny",
                "reason": "not this one"
            })
        );
    }

    /// OCEAN-314: when a `Some(token)` is supplied the serialized decision body
    /// MUST carry that exact `decision_token` value alongside the decision —
    /// this is the wire contract the daemon's OCEAN-185 gate verifies. Mirrors
    /// the ocean-surface-ui `turn_request_emits_decision_token_when_set` test so
    /// both surfaces' wire shapes for the new field are actually exercised.
    #[test]
    fn permission_decision_body_carries_decision_token_when_set() {
        // 64 hex chars, the shape mint produces (two v4 UUIDs concatenated).
        let token = "deadbeef".repeat(8);
        let allow = serde_json::to_value(PermissionDecisionRequest::allow(
            "perm-1",
            Some(token.clone()),
        ))
        .expect("allow should serialize");
        let deny = serde_json::to_value(PermissionDecisionRequest::deny(
            "perm-2",
            "not this one",
            Some(token.clone()),
        ))
        .expect("deny should serialize");

        assert_eq!(
            allow,
            serde_json::json!({
                "permission_id": "perm-1",
                "decision": "allow",
                "decision_token": token,
            }),
            "allow decision must carry the decision_token on the wire when set"
        );
        assert_eq!(
            deny,
            serde_json::json!({
                "permission_id": "perm-2",
                "decision": "deny",
                "reason": "not this one",
                "decision_token": token,
            }),
            "deny decision must carry the decision_token on the wire when set"
        );
    }

    #[test]
    fn component_event_body_matches_daemon_contract() {
        let body = serde_json::to_value(ComponentEventRequest {
            session_id: "s1".to_string(),
            component_id: "confirm-1".to_string(),
            event: serde_json::json!({
                "type": "submit",
                "data": { "ok": true }
            }),
        })
        .expect("component event should serialize");

        assert_eq!(
            body,
            serde_json::json!({
                "session_id": "s1",
                "component_id": "confirm-1",
                "event": {
                    "type": "submit",
                    "data": { "ok": true }
                }
            })
        );
    }

    #[test]
    fn models_response_decodes_current_and_catalogue() {
        let response: ModelsResponse = serde_json::from_str(
            r#"{
                "ok": true,
                "current": {
                    "model": "gpt-5.5",
                    "provider": "openai-codex"
                },
                "models": [
                    {
                        "id": "gpt-5.5",
                        "label": "GPT-5.5 (Codex)",
                        "provider": "openai-codex"
                    }
                ]
            }"#,
        )
        .expect("models response should decode");

        assert!(response.ok);
        assert_eq!(
            response.current,
            Some(CurrentModel {
                model: "gpt-5.5".to_string(),
                provider: "openai-codex".to_string(),
            })
        );
        assert_eq!(
            response.models,
            vec![ModelInfo {
                id: "gpt-5.5".to_string(),
                label: "GPT-5.5 (Codex)".to_string(),
                provider: "openai-codex".to_string(),
            }]
        );
    }

    #[test]
    fn native_daemon_state_reports_backend_when_ready() {
        let mut state = NativeDaemonState {
            url: "http://localhost:4780".to_string(),
            health: DaemonHealth::Checking,
            last_checked: None,
        };

        state.apply_health(DaemonHealth::Ready(HealthResponse {
            ok: true,
            service: "ocean-daemon".to_string(),
            version: "0.1.0".to_string(),
            backend: "ocean-native".to_string(),
        }));

        assert_eq!(state.status_label(), "online");
        assert_eq!(state.backend_label(), "ocean-native");
    }

    #[test]
    fn sse_reader_parses_agent_events() {
        let input = concat!(
            "event: assistant_text_delta\n",
            "data: {\"type\":\"assistant_text_delta\",\"session_id\":\"s1\",\"turn_id\":\"t1\",\"delta\":\"hi\"}\n",
            "\n"
        );
        let (sender, receiver) = mpsc::channel();

        read_sse_events(Cursor::new(input), |event: AgentEvent| {
            sender.send(event).map_err(|error| error.to_string())
        })
        .expect("sse parse");

        assert_eq!(
            receiver.recv().expect("event"),
            AgentEvent::AssistantTextDelta {
                session_id: "s1".to_string(),
                turn_id: "t1".to_string(),
                delta: "hi".to_string()
            }
        );
    }

    #[test]
    fn control_events_url_trims_trailing_slash() {
        assert_eq!(
            control_events_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/events"
        );
    }

    #[test]
    fn sse_reader_parses_control_permission_request() {
        // The control envelope carries `permission_id` alongside the flattened
        // OceanEvent — the field the agent stream drops (OCEAN-75).
        let input = concat!(
            "event: permission_request\n",
            "data: {\"type\":\"permission_request\",\"permission_id\":\"perm-1\",\"session_id\":\"s1\",\"tool\":\"write_file\",\"reason\":\"create file\",\"args\":{\"path\":\"/tmp/x\"}}\n",
            "\n"
        );
        let (sender, receiver) = mpsc::channel();

        read_sse_events(Cursor::new(input), |event: ControlEvent| {
            sender.send(event).map_err(|error| error.to_string())
        })
        .expect("sse parse");

        assert_eq!(
            receiver.recv().expect("event"),
            ControlEvent::PermissionRequest {
                permission_id: Some("perm-1".to_string()),
                session_id: Some("s1".to_string()),
                tool: "write_file".to_string(),
                reason: "create file".to_string(),
                args: serde_json::json!({ "path": "/tmp/x" }),
            }
        );
    }

    #[test]
    fn sse_reader_parses_control_permission_decision() {
        let input = concat!(
            "event: permission_decision\n",
            "data: {\"type\":\"permission_decision\",\"permission_id\":\"perm-1\",\"session_id\":\"s1\"}\n",
            "\n"
        );
        let (sender, receiver) = mpsc::channel();

        read_sse_events(Cursor::new(input), |event: ControlEvent| {
            sender.send(event).map_err(|error| error.to_string())
        })
        .expect("sse parse");

        assert_eq!(
            receiver.recv().expect("event"),
            ControlEvent::PermissionDecision {
                permission_id: Some("perm-1".to_string()),
                session_id: Some("s1".to_string()),
            }
        );
    }

    #[test]
    fn control_stream_ignores_unmodelled_frames() {
        // A non-permission control frame must decode to `Other`, not fail the
        // whole stream — otherwise gating-off daemons (which emit other control
        // events) would error the listener.
        let input = concat!(
            "event: browser_activity\n",
            "data: {\"type\":\"browser_activity\",\"session_id\":\"s1\",\"active\":true}\n",
            "\n"
        );
        let (sender, receiver) = mpsc::channel();

        read_sse_events(Cursor::new(input), |event: ControlEvent| {
            sender.send(event).map_err(|error| error.to_string())
        })
        .expect("sse parse");

        assert_eq!(receiver.recv().expect("event"), ControlEvent::Other);
    }

    #[test]
    fn room_urls_match_daemon_persistent_routes() {
        assert_eq!(
            rooms_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/rooms/persistent"
        );
        assert_eq!(
            room_url("http://127.0.0.1:4780/", "map-fix"),
            "http://127.0.0.1:4780/v1/rooms/persistent/map-fix"
        );
        assert_eq!(
            room_transcript_url("http://127.0.0.1:4780/", "map-fix", 7),
            "http://127.0.0.1:4780/v1/rooms/persistent/map-fix/transcript?after_seq=7"
        );
        assert_eq!(
            room_participants_url("http://127.0.0.1:4780/", "map-fix"),
            "http://127.0.0.1:4780/v1/rooms/persistent/map-fix/participants"
        );
        assert_eq!(
            room_participant_url("http://127.0.0.1:4780/", "map-fix", "web-1"),
            "http://127.0.0.1:4780/v1/rooms/persistent/map-fix/participants/web-1"
        );
        assert_eq!(
            room_messages_url("http://127.0.0.1:4780/", "map-fix"),
            "http://127.0.0.1:4780/v1/rooms/persistent/map-fix/messages"
        );
    }

    #[test]
    fn room_key_with_unsafe_chars_is_percent_encoded() {
        assert_eq!(
            room_url("http://127.0.0.1:4780", "ops room"),
            "http://127.0.0.1:4780/v1/rooms/persistent/ops%20room"
        );
    }

    #[test]
    fn create_room_body_matches_daemon_contract() {
        // No policy → the `trigger_policy` field is skipped entirely, so the
        // daemon's `#[serde(default)]` (no triggers) applies.
        let body = serde_json::to_value(CreateRoomRequest {
            key: "map-fix".to_string(),
            name: "Map Fix".to_string(),
            trigger_policy: None,
        })
        .expect("create body should serialize");

        assert_eq!(
            body,
            serde_json::json!({ "key": "map-fix", "name": "Map Fix" })
        );
    }

    #[test]
    fn create_room_body_carries_trigger_policy_when_set() {
        // A policy with the @mention trigger on + a cron schedule serializes
        // snake_case under `trigger_policy`, matching `ocean_core::RoomTriggerPolicy`
        // (OCEAN-119 / OCEAN-117).
        let body = serde_json::to_value(CreateRoomRequest {
            key: "standup".to_string(),
            name: "Standup".to_string(),
            trigger_policy: Some(RoomTriggerPolicy {
                on_mention: true,
                on_thread_reply: false,
                on_component_event: false,
                on_schedule: Some("0 9 * * *".to_string()),
            }),
        })
        .expect("create body should serialize");

        assert_eq!(
            body,
            serde_json::json!({
                "key": "standup",
                "name": "Standup",
                "trigger_policy": {
                    "on_mention": true,
                    "on_thread_reply": false,
                    "on_component_event": false,
                    "on_schedule": "0 9 * * *"
                }
            })
        );
    }

    #[test]
    fn trigger_policy_skips_empty_schedule() {
        // `on_schedule: None` is skipped so the daemon stores no cron.
        let body = serde_json::to_value(RoomTriggerPolicy {
            on_mention: true,
            on_thread_reply: true,
            on_component_event: false,
            on_schedule: None,
        })
        .expect("policy should serialize");

        assert_eq!(
            body,
            serde_json::json!({
                "on_mention": true,
                "on_thread_reply": true,
                "on_component_event": false
            })
        );
    }

    #[test]
    fn join_and_post_bodies_carry_snake_case_kind() {
        let join = serde_json::to_value(RoomJoinRequest {
            id: "gpui-1".to_string(),
            display_name: "Operator".to_string(),
            kind: RoomParticipantKind::Human,
        })
        .expect("join body should serialize");
        assert_eq!(
            join,
            serde_json::json!({
                "id": "gpui-1",
                "display_name": "Operator",
                "kind": "human"
            })
        );

        let post = serde_json::to_value(RoomPostMessageRequest {
            author_id: "gpui-1".to_string(),
            author_kind: RoomParticipantKind::Human,
            body: "@scout look at this".to_string(),
        })
        .expect("post body should serialize");
        assert_eq!(
            post,
            serde_json::json!({
                "author_id": "gpui-1",
                "author_kind": "human",
                "body": "@scout look at this"
            })
        );
    }

    #[test]
    fn room_list_response_decodes_roster_and_metadata() {
        let response: RoomsListResponse = serde_json::from_str(
            r#"{
                "ok": true,
                "rooms": [
                    {
                        "id": "map-fix",
                        "name": "Map Fix",
                        "participants": [
                            { "id": "gpui-1", "kind": "human", "display_name": "Operator" },
                            { "id": "scout", "kind": "agent", "display_name": "Scout" }
                        ],
                        "created_at": "2026-06-05T12:00:00Z",
                        "updated_at": "2026-06-05T12:34:00Z"
                    }
                ]
            }"#,
        )
        .expect("rooms list should decode");

        assert!(response.ok);
        let room: &Room = &response.rooms[0];
        assert_eq!(room.id, "map-fix");
        assert_eq!(room.participants.len(), 2);
        assert_eq!(room.participants[1].kind, RoomParticipantKind::Agent);
        assert_eq!(room.updated_at, "2026-06-05T12:34:00Z");
    }

    #[test]
    fn room_get_response_decodes_transcript_kinds() {
        let response: RoomGetResponse = serde_json::from_str(
            r#"{
                "ok": true,
                "room": { "id": "map-fix", "name": "Map Fix" },
                "transcript": [
                    { "seq": 1, "author_id": "system", "author_kind": "system", "kind": "participant_joined", "body": "Operator joined" },
                    { "seq": 2, "author_id": "gpui-1", "author_kind": "human", "kind": "message", "body": "hello" }
                ]
            }"#,
        )
        .expect("room get should decode");

        assert!(response.ok);
        assert_eq!(response.transcript.len(), 2);
        assert_eq!(
            response.transcript[0].kind,
            RoomMessageKind::ParticipantJoined
        );
        assert!(response.transcript[0].kind.is_system());
        assert_eq!(response.transcript[1].kind, RoomMessageKind::Message);
        assert!(!response.transcript[1].kind.is_system());
    }

    /// OCEAN-321: with no images staged, the `images` key must be absent from
    /// the wire JSON so the daemon's `Option<Vec<TurnImage>>` stays `None` and
    /// pre-images daemons are unaffected.
    #[test]
    fn turn_request_omits_images_when_none() {
        let request = AgentTurnRequest {
            prompt: "hello".to_string(),
            cwd: "/tmp".to_string(),
            session_id: None,
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            decision_token: None,
            images: None,
        };
        let json = serde_json::to_string(&request).expect("should serialize");
        assert!(
            !json.contains("images"),
            "images must be absent when None, got: {json}"
        );
    }

    /// OCEAN-321: when images are present they must serialize as
    /// `[{mime_type, data}, ...]` — the exact wire shape the daemon's
    /// `TurnImage` (ocean-agent-sdk) deserializes on `/v1/agent/turns`.
    #[test]
    fn turn_request_emits_images_in_daemon_wire_shape() {
        let request = AgentTurnRequest {
            prompt: "describe this".to_string(),
            cwd: "/tmp".to_string(),
            session_id: Some("sess-1".to_string()),
            project_id: None,
            client_type: None,
            guidance: None,
            room_id: None,
            thinking_level: None,
            model_id: None,
            decision_token: None,
            images: Some(vec![TurnImage {
                mime_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            }]),
        };
        let v = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(v["images"][0]["mime_type"], "image/png");
        assert_eq!(v["images"][0]["data"], "aGVsbG8=");
    }
}
