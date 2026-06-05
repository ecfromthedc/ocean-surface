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
use serde_json::Value;
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
            | AgentEvent::BrowserActivity { session_id, .. } => session_id.as_str(),
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

#[derive(Debug, Clone, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    #[serde(default)]
    pub output: String,
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

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct AgentSessionCreateResponse {
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
        let awaiting_session_adoption = self.awaiting_session_adoption;

        let generation = sse_generation.get_untracked().wrapping_add(1);
        sse_generation.set(generation);
        let Some(active_session_id) = session_id.get_untracked() else {
            status.set("new session".into());
            return;
        };
        let seen_sse_ids: RwSignal<VecDeque<String>> = RwSignal::new(VecDeque::new());

        spawn_local(async move {
            loop {
                if sse_generation.get_untracked() != generation {
                    break;
                }

                let events_url = format!(
                    "{}/v1/agent/events?session_id={}",
                    url.trim_end_matches('/'),
                    active_session_id.as_str()
                );
                status.set("connecting…".into());
                let mut es = match EventSource::new(&events_url) {
                    Ok(es) => es,
                    Err(err) => {
                        status.set(format!("sse connect error: {err}"));
                        gloo_timers::future::TimeoutFuture::new(2_000).await;
                        continue;
                    }
                };
                status.set("connected".into());

                // EventSource delivers events by `event:` name. The daemon
                // names each frame by its AgentTurnEvent type, so we subscribe
                // per type and merge the streams. gloo-net has no
                // `subscribe_multiple`; we build the merged stream ourselves
                // with `futures::stream::select_all`.
                const NAMES: &[&str] = &[
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
                    "extension",
                ];
                let mut subs = Vec::with_capacity(NAMES.len());
                let mut sub_err = None;
                for name in NAMES {
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
                        last_turn_tokens,
                        session_tokens,
                        model,
                        active_turn_id,
                        browser_active,
                        awaiting_session_adoption,
                    );
                }

                if sse_generation.get_untracked() != generation {
                    break;
                }

                status.set("reconnecting…".into());
                gloo_timers::future::TimeoutFuture::new(1_000).await;
            }
        });
    }

    pub fn send_prompt(&self, prompt: String) {
        if prompt.trim().is_empty() {
            return;
        }
        // Echo the user prompt immediately, then dispatch.
        self.turns.update(|t| t.push(Turn::user(prompt.clone())));
        self.streaming.set(true);
        self.dispatch_prompt(prompt, false);
    }

    /// Send a turn to the daemon. `is_retry` marks an auto-recovery resend (the
    /// user prompt was already echoed; don't echo again). If the daemon reports
    /// the supplied session is gone (strict resume), we clear the stale id and
    /// retry once as a fresh session — so a daemon restart is invisible to the
    /// user instead of dead-ending the turn.
    fn dispatch_prompt(&self, prompt: String, is_retry: bool) {
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
        let daemon = self.clone();

        spawn_local(async move {
            if session_id.is_none() {
                let title_hint = session_title_hint(&prompt);
                let body = AgentSessionCreateRequest {
                    title: title_hint.as_deref(),
                    workspace_root: &cwd,
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
                            daemon.dispatch_prompt(prompt, is_retry);
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
            // Attach the active tab's URL + title as guidance so the agent knows
            // what page the user is looking at when they send a turn. Only the
            // single active tab, only on a user-initiated turn — never a passive
            // scrape (OCEAN-70). On the detached web app this is always `None`.
            let active_tab_guidance = active_tab_guidance();
            let body = AgentTurnRequest {
                prompt: &prompt,
                cwd: &cwd,
                session_id: session_id.as_deref(),
                project_id: project.as_deref(),
                client_type: Some(surface_client_type()),
                // The web UI doesn't surface free-form guidance yet; the only
                // guidance we emit is the extension's active-tab context above.
                // The remaining per-turn overrides serialize as `None` so the
                // daemon applies its global defaults, matching the daemon's
                // AgentTurnRequest wire shape (OCEAN-61).
                guidance: active_tab_guidance,
                room_id: None,
                thinking_level: None,
                model_id: None,
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
                            daemon.dispatch_prompt(prompt, true);
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

    /// Start a fresh session. Clears state and leaves session_id as None
    /// so the next prompt creates a new session.
    pub fn new_session(&self) {
        self.turns.set(Vec::new());
        self.session_id.set(None);
        self.awaiting_session_adoption.set(false);
        self.session_title.set(String::new());
        self.status.set("new session".into());
        self.reset_token_stats();
        self.connect();
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
    last_turn_tokens: RwSignal<Option<TokenStats>>,
    session_tokens: RwSignal<TokenStats>,
    model: RwSignal<Option<String>>,
    active_turn_id: RwSignal<Option<String>>,
    browser_active: RwSignal<bool>,
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
            output_tokens,
            input_tokens,
            cache_read_tokens,
            tokens_per_second,
            ..
        } => {
            streaming.set(false);
            awaiting_session_adoption.set(false);
            active_turn_id.set(None);
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
        AgentEvent::Extension { extension, .. } => {
            // No renderer for extension/council events on this surface yet. Log
            // and ignore rather than silently drop, so we can see them in the
            // console while the deck UI is built out (OCEAN-62a).
            log::debug!("ignoring extension event: {extension}");
        }
        AgentEvent::Other => {}
    }
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

/// The surface identity sent to the daemon as `client_type`, so the agent's
/// system prompt is scoped to where the user is actually talking from.
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
}
