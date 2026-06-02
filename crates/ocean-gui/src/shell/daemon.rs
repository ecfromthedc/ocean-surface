use std::env;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

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

    pub fn stream_agent_events(
        &self,
        base_url: &str,
        on_event: impl FnMut(AgentEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        let response = self
            .http
            .get(agent_events_url(base_url))
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
pub fn agent_events_url(base_url: &str) -> String {
    format!("{}/v1/agent/events", base_url.trim_end_matches('/'))
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentTurnRequest {
    pub prompt: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Surface marker used by the daemon to select medium-appropriate agent guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AgentTurnResponse {
    pub ok: bool,
    pub turn_id: String,
    pub session_id: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

fn read_sse_events<R: BufRead>(
    mut reader: R,
    mut on_event: impl FnMut(AgentEvent) -> Result<(), String>,
) -> Result<(), String> {
    let mut line = String::new();
    let mut data = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).map_err(|error| error.to_string())?;
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

fn flush_sse_data(
    data: &mut String,
    on_event: &mut impl FnMut(AgentEvent) -> Result<(), String>,
) -> Result<(), String> {
    if data.trim().is_empty() {
        data.clear();
        return Ok(());
    }

    let event = serde_json::from_str::<AgentEvent>(data).map_err(|error| error.to_string())?;
    data.clear();
    on_event(event)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;

    use super::{
        AgentEvent, DaemonHealth, HealthResponse, NativeDaemonState, agent_events_url,
        agent_turns_url, health_url, read_sse_events,
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
            agent_events_url("http://127.0.0.1:4780/"),
            "http://127.0.0.1:4780/v1/agent/events"
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

        read_sse_events(Cursor::new(input), |event| {
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
}
