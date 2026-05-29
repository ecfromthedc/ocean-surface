//! Daemon connection layer.
//!
//! Single point of contact between the UI and `ocean-daemon`. We speak the
//! product agent API:
//!
//!   POST /v1/agent/turns   → start a turn (returns metadata only)
//!   GET  /v1/agent/events  → SSE stream of AgentTurnEvent
//!
//! All reply text and tool output arrives as events on the SSE stream; the
//! POST returns once the turn completes but carries no payload beyond
//! turn_id / session_id / status. We push events into a Leptos signal so
//! the rest of the UI reacts naturally.

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
    },
    AssistantTextDelta {
        turn_id: String,
        delta: String,
    },
    ThinkingDelta {
        turn_id: String,
        delta: String,
    },
    ToolCallStarted {
        turn_id: String,
        call: ToolCallSummary,
    },
    ToolCallChunk {
        turn_id: String,
        call_id: String,
        chunk: String,
    },
    ToolCallFinished {
        turn_id: String,
        call_id: String,
        result: ToolResult,
    },
    TurnFinished {
        turn_id: String,
        status: String,
        #[serde(default)]
        error: Option<String>,
    },
    #[serde(other)]
    Other,
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
            match Request::get("/api/config").send().await {
                Ok(resp) => match resp.json::<ProxyConfig>().await {
                    Ok(cfg) => {
                        if !cfg.daemon_url.trim().is_empty() {
                            daemon.url.set(cfg.daemon_url);
                        }
                        // Record voice readiness in its own signal so the SSE
                        // status transitions in connect() don't clobber it.
                        daemon.voice_ready.set(cfg.has_auth);
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

        spawn_local(async move {
            loop {
                let events_url = format!("{}/v1/agent/events", url.trim_end_matches('/'));
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
                    let Ok((_event_name, msg)) = msg else { continue };
                    let Some(data) = msg.data().as_string() else {
                        continue;
                    };
                    let Ok(evt) = serde_json::from_str::<AgentEvent>(&data) else {
                        log::warn!("unparseable sse event: {data}");
                        continue;
                    };
                    apply_event(&evt, turns, session_id, streaming);
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
        let url = self.url.get_untracked();
        let cwd = self.cwd.get_untracked();
        let session_id = self.session_id.get_untracked();
        let turns = self.turns;
        let streaming = self.streaming;
        let status = self.status;

        // Echo the user prompt immediately.
        turns.update(|t| t.push(Turn::user(prompt.clone())));
        streaming.set(true);

        spawn_local(async move {
            let body = AgentTurnRequest {
                prompt: &prompt,
                cwd: &cwd,
                session_id: session_id.as_deref(),
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
                    return;
                }
            };
            match res {
                Ok(resp) => match resp.json::<AgentTurnResponse>().await {
                    Ok(r) if r.ok => {
                        // session_id arrives via SessionCreated on the SSE
                        // stream too; this is just a belt-and-braces capture.
                        // streaming flips off when turn_finished arrives.
                    }
                    Ok(r) => {
                        status.set(format!(
                            "turn failed: {}",
                            r.error.unwrap_or_else(|| "unknown error".into())
                        ));
                        streaming.set(false);
                    }
                    Err(err) => {
                        status.set(format!("decode error: {err}"));
                        streaming.set(false);
                    }
                },
                Err(err) => {
                    status.set(format!("post error: {err}"));
                    streaming.set(false);
                }
            }
        });
    }
}

/// Mutate the turns vec in response to a single SSE event. Splits assistant
/// content into Text / Thinking / ToolCall blocks under one Turn per turn_id,
/// matching the TUI's `pm_*_assistant_turn_mut` logic.
fn apply_event(
    event: &AgentEvent,
    turns: RwSignal<Vec<Turn>>,
    session_id: RwSignal<Option<String>>,
    streaming: RwSignal<bool>,
) {
    match event {
        AgentEvent::SessionCreated { session_id: sid, .. } => {
            session_id.set(Some(sid.clone()));
        }
        AgentEvent::TurnStarted { .. } => {
            // Assistant turn will be lazily created on the first delta.
        }
        AgentEvent::AssistantTextDelta { turn_id, delta } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                match turn.blocks.last_mut() {
                    Some(Block::Text(buf)) => buf.push_str(delta),
                    _ => turn.blocks.push(Block::Text(delta.clone())),
                }
            });
        }
        AgentEvent::ThinkingDelta { turn_id, delta } => {
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
        AgentEvent::ToolCallStarted { turn_id, call } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                let args = serde_json::to_string(&call.args_json)
                    .unwrap_or_else(|_| "{}".into());
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
        } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                for block in turn.blocks.iter_mut().rev() {
                    if let Block::ToolCall {
                        call_id: id, output, ..
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
        } => {
            turns.update(|t| {
                let turn = ensure_assistant_turn(t, turn_id);
                for block in turn.blocks.iter_mut().rev() {
                    if let Block::ToolCall {
                        call_id: id,
                        output,
                        status,
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
                            break;
                        }
                    }
                }
            });
        }
        AgentEvent::TurnFinished { .. } => {
            streaming.set(false);
        }
        AgentEvent::Other => {}
    }
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

/// Best-effort default cwd. In the browser there's no real cwd, so we send
/// "/" and let the user override later via a settings panel.
fn default_cwd() -> String {
    "/".into()
}
