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
}

impl PermissionDecisionRequest {
    #[must_use]
    pub fn allow(permission_id: impl Into<String>) -> Self {
        Self {
            permission_id: permission_id.into(),
            decision: PermissionDecision::Allow,
        }
    }

    #[must_use]
    pub fn deny(permission_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            permission_id: permission_id.into(),
            decision: PermissionDecision::Deny {
                reason: Some(reason.into()),
            },
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
pub fn session_detail_url(base_url: &str, session_id: &str) -> String {
    format!(
        "{}/v1/sessions/{}",
        base_url.trim_end_matches('/'),
        session_id
    )
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
        AgentEvent, ComponentEventRequest, ControlEvent, CurrentModel, DaemonHealth, HealthResponse,
        LiveKitTokenRequest, LiveKitTokenResponse, ModelInfo, ModelsResponse, NativeDaemonState,
        PermissionDecisionRequest, agent_events_url, agent_session_create_url, agent_turns_url,
        component_event_url, control_events_url, health_url, livekit_token_url, model_url,
        models_url, permission_decision_url, permissions_url, read_sse_events, request_cancel_url,
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
        let allow = serde_json::to_value(PermissionDecisionRequest::allow("perm-1"))
            .expect("allow should serialize");
        let deny = serde_json::to_value(PermissionDecisionRequest::deny("perm-2", "not this one"))
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
}
