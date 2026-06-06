use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::daemon::{LiveKitTokenRequest, LiveKitTokenResponse};
use super::surface::{DEFAULT_LIVEKIT_ROOM_ID, SurfaceCanvasContext, SurfaceMode, SurfaceState};

pub const DEFAULT_SURFACE_ID: &str = "gpui:local";
pub const DEFAULT_PARTICIPANT_ID: &str = "human:local";
pub const DEFAULT_DISPLAY_NAME: &str = "Ocean operator";

const ROOM_METADATA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceLiveKitJoinState {
    NotJoined,
    RequestingToken,
    TokenReady,
    Joining,
    Joined,
    Failed,
}

/// One row in the LiveKit participant roster as observed by the native client.
///
/// Mirrors the web surface's `Participant` shape (see
/// `ocean-surface-ui/src/livekit.rs`) so both clients present an equivalent
/// presence view: identity/name, whether the row is the local participant, and
/// live mic/camera/speaking flags derived from track publications.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLiveKitParticipant {
    pub identity: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub mic: bool,
    #[serde(default)]
    pub camera: bool,
    #[serde(default)]
    pub speaking: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLiveKitCredentials {
    pub url: String,
    pub room: String,
    #[serde(skip_serializing)]
    pub token: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLiveKitState {
    surface_id: String,
    room_id: String,
    participant_id: String,
    display_name: String,
    mic_enabled: bool,
    camera_enabled: bool,
    join_state: SurfaceLiveKitJoinState,
    #[serde(skip_serializing_if = "Option::is_none")]
    credentials: Option<SurfaceLiveKitCredentials>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roster: Vec<SurfaceLiveKitParticipant>,
}

impl Default for SurfaceLiveKitState {
    fn default() -> Self {
        Self {
            surface_id: DEFAULT_SURFACE_ID.to_string(),
            room_id: DEFAULT_LIVEKIT_ROOM_ID.to_string(),
            participant_id: DEFAULT_PARTICIPANT_ID.to_string(),
            display_name: DEFAULT_DISPLAY_NAME.to_string(),
            mic_enabled: false,
            camera_enabled: false,
            join_state: SurfaceLiveKitJoinState::NotJoined,
            credentials: None,
            last_error: None,
            roster: Vec::new(),
        }
    }
}

impl SurfaceLiveKitState {
    #[must_use]
    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    #[must_use]
    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn mic_enabled(&self) -> bool {
        self.mic_enabled
    }

    #[must_use]
    pub fn camera_enabled(&self) -> bool {
        self.camera_enabled
    }

    #[must_use]
    pub fn join_state(&self) -> SurfaceLiveKitJoinState {
        self.join_state
    }

    #[must_use]
    pub fn credentials(&self) -> Option<&SurfaceLiveKitCredentials> {
        self.credentials.as_ref()
    }

    #[must_use]
    pub fn roster(&self) -> &[SurfaceLiveKitParticipant] {
        &self.roster
    }

    /// Replace the live participant roster with the latest snapshot relayed
    /// from the native LiveKit client thread. Rows are sorted local-first then
    /// by identity so the presence list renders deterministically.
    pub fn set_roster(&mut self, mut roster: Vec<SurfaceLiveKitParticipant>) {
        roster.sort_by(|a, b| {
            b.local
                .cmp(&a.local)
                .then_with(|| a.identity.cmp(&b.identity))
        });
        self.roster = roster;
    }

    pub fn toggle_mic(&mut self) -> bool {
        self.mic_enabled = !self.mic_enabled;
        self.mic_enabled
    }

    pub fn set_mic_enabled(&mut self, enabled: bool) {
        self.mic_enabled = enabled;
    }

    pub fn toggle_camera(&mut self) -> bool {
        self.camera_enabled = !self.camera_enabled;
        self.camera_enabled
    }

    pub fn begin_token_request(&mut self) -> LiveKitTokenRequest {
        self.join_state = SurfaceLiveKitJoinState::RequestingToken;
        self.last_error = None;
        self.credentials = None;

        LiveKitTokenRequest {
            surface_id: self.surface_id.clone(),
            participant_id: self.participant_id.clone(),
            display_name: self.display_name.clone(),
            can_publish: true,
            can_subscribe: true,
        }
    }

    pub fn apply_token_response(&mut self, response: LiveKitTokenResponse) -> Result<(), String> {
        if !response.ok {
            let error = response.error.unwrap_or_else(|| "token denied".to_string());
            self.mark_failed(error.clone());
            return Err(error);
        }

        self.room_id = response.room.clone();
        self.credentials = Some(SurfaceLiveKitCredentials {
            url: response.url,
            room: response.room,
            token: response.token,
            expires_at: response.expires_at,
        });
        self.join_state = SurfaceLiveKitJoinState::TokenReady;
        self.last_error = None;
        Ok(())
    }

    pub fn mark_joining(&mut self) {
        self.join_state = SurfaceLiveKitJoinState::Joining;
        self.last_error = None;
    }

    pub fn mark_joined(&mut self) {
        self.join_state = SurfaceLiveKitJoinState::Joined;
        self.last_error = None;
    }

    pub fn mark_disconnected(&mut self, reason: impl Into<String>) {
        self.join_state = SurfaceLiveKitJoinState::NotJoined;
        self.credentials = None;
        self.last_error = Some(reason.into());
        self.roster.clear();
    }

    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.join_state = SurfaceLiveKitJoinState::Failed;
        self.credentials = None;
        self.last_error = Some(error.into());
        self.roster.clear();
    }

    #[must_use]
    pub fn status_label(&self) -> String {
        match self.join_state {
            SurfaceLiveKitJoinState::NotJoined => "not joined".to_string(),
            SurfaceLiveKitJoinState::RequestingToken => "requesting token".to_string(),
            SurfaceLiveKitJoinState::TokenReady => format!("ready {}", self.room_id),
            SurfaceLiveKitJoinState::Joining => format!("joining {}", self.room_id),
            SurfaceLiveKitJoinState::Joined => format!("joined {}", self.room_id),
            SurfaceLiveKitJoinState::Failed => self
                .last_error
                .as_ref()
                .map(|error| format!("failed {error}"))
                .unwrap_or_else(|| "failed".to_string()),
        }
    }

    #[must_use]
    pub fn participant_attributes(&self, surface_session_id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ocean.client".to_string(), "ocean-gui".to_string()),
            ("ocean.surface_id".to_string(), self.surface_id.clone()),
            (
                "ocean.surface_session_id".to_string(),
                surface_session_id.to_string(),
            ),
            ("ocean.room_id".to_string(), self.room_id.clone()),
            (
                "ocean.participant_id".to_string(),
                self.participant_id.clone(),
            ),
            (
                "ocean.mic_enabled".to_string(),
                self.mic_enabled.to_string(),
            ),
            (
                "ocean.camera_enabled".to_string(),
                self.camera_enabled.to_string(),
            ),
        ])
    }

    #[must_use]
    pub fn room_metadata(
        &self,
        surface: &SurfaceState,
        agent_session_id: Option<&str>,
    ) -> SurfaceRoomMetadata {
        let context = surface.turn_context();
        let active_canvas_id = surface.active_canvas_id().map(str::to_string);
        let canvases = context
            .canvases
            .iter()
            .map(SurfaceCanvasRoomMetadata::from)
            .collect();

        SurfaceRoomMetadata {
            version: ROOM_METADATA_VERSION,
            room_id: self.room_id.clone(),
            surface_id: self.surface_id.clone(),
            surface_session_id: context.session_id,
            agent_session_id: agent_session_id.map(str::to_string),
            active_pane_id: context.active_pane_id,
            active_canvas_id,
            media: SurfaceMediaMetadata {
                mic_enabled: self.mic_enabled,
                camera_enabled: self.camera_enabled,
            },
            canvases,
        }
    }

    pub fn room_metadata_json(
        &self,
        surface: &SurfaceState,
        agent_session_id: Option<&str>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.room_metadata(surface, agent_session_id))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceRoomMetadata {
    pub version: u32,
    pub room_id: String,
    pub surface_id: String,
    pub surface_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub active_pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_canvas_id: Option<String>,
    pub media: SurfaceMediaMetadata,
    pub canvases: Vec<SurfaceCanvasRoomMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SurfaceMediaMetadata {
    pub mic_enabled: bool,
    pub camera_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceCanvasRoomMetadata {
    pub canvas_id: String,
    pub tldraw_room_id: String,
    pub mode: SurfaceMode,
    pub revision: u64,
    pub component_count: usize,
    pub selection_count: usize,
}

impl From<&SurfaceCanvasContext> for SurfaceCanvasRoomMetadata {
    fn from(canvas: &SurfaceCanvasContext) -> Self {
        Self {
            canvas_id: canvas.canvas_id.clone(),
            tldraw_room_id: canvas.tldraw_room_id.clone(),
            mode: canvas.mode,
            revision: canvas.revision,
            component_count: canvas.components.len(),
            selection_count: canvas.selection.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::surface::{LedgerComponent, SurfaceMode};

    #[test]
    fn token_request_uses_surface_participant_and_publish_permissions() {
        let mut state = SurfaceLiveKitState::default();

        let request = state.begin_token_request();

        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::RequestingToken);
        assert_eq!(state.room_id(), DEFAULT_LIVEKIT_ROOM_ID);
        assert_eq!(request.surface_id, DEFAULT_SURFACE_ID);
        assert_eq!(request.participant_id, DEFAULT_PARTICIPANT_ID);
        assert_eq!(request.display_name, DEFAULT_DISPLAY_NAME);
        assert!(request.can_publish);
        assert!(request.can_subscribe);
    }

    #[test]
    fn token_response_stores_join_credentials_without_serializing_secret() {
        let mut state = SurfaceLiveKitState::default();

        state
            .apply_token_response(LiveKitTokenResponse {
                ok: true,
                url: "wss://livekit.example.test".to_string(),
                room: "project:surface-alpha".to_string(),
                token: "secret-jwt".to_string(),
                expires_at: "2026-06-03T22:00:00Z".to_string(),
                error: None,
            })
            .expect("token response should apply");

        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::TokenReady);
        assert_eq!(state.room_id(), "project:surface-alpha");
        assert_eq!(
            state
                .credentials()
                .map(|credentials| credentials.token.as_str()),
            Some("secret-jwt")
        );
        let serialized = serde_json::to_string(&state).expect("state should serialize");
        assert!(!serialized.contains("secret-jwt"));
    }

    #[test]
    fn denied_token_clears_credentials_and_marks_failure() {
        let mut state = SurfaceLiveKitState::default();
        state
            .apply_token_response(LiveKitTokenResponse {
                ok: true,
                url: "wss://livekit.example.test".to_string(),
                room: "project:surface-alpha".to_string(),
                token: "secret-jwt".to_string(),
                expires_at: "2026-06-03T22:00:00Z".to_string(),
                error: None,
            })
            .expect("initial token should apply");

        let result = state.apply_token_response(LiveKitTokenResponse {
            ok: false,
            url: String::new(),
            room: "project:surface-alpha".to_string(),
            token: String::new(),
            expires_at: String::new(),
            error: Some("room disabled".to_string()),
        });

        assert_eq!(result, Err("room disabled".to_string()));
        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::Failed);
        assert!(state.credentials().is_none());
        assert_eq!(state.status_label(), "failed room disabled");
    }

    #[test]
    fn join_state_transitions_track_livekit_client_lifecycle() {
        let mut state = SurfaceLiveKitState::default();
        state
            .apply_token_response(LiveKitTokenResponse {
                ok: true,
                url: "wss://livekit.example.test".to_string(),
                room: "project:surface-alpha".to_string(),
                token: "secret-jwt".to_string(),
                expires_at: "2026-06-03T22:00:00Z".to_string(),
                error: None,
            })
            .expect("token response should apply");

        state.mark_joining();
        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::Joining);
        assert!(state.credentials().is_some());

        state.mark_joined();
        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::Joined);
        assert!(state.credentials().is_some());

        state.mark_disconnected("left room");
        assert_eq!(state.join_state(), SurfaceLiveKitJoinState::NotJoined);
        assert!(state.credentials().is_none());
    }

    #[test]
    fn room_metadata_is_session_rooted_and_lists_all_canvas_summaries() {
        let mut surface = SurfaceState::default();
        let storyboard_canvas = surface.open_canvas_pane("Storyboard", SurfaceMode::Storyboard);
        surface.upsert_component(
            "canvas:main",
            LedgerComponent::markdown_card("brief-1", 40.0, 40.0, "Sales brief"),
        );
        surface.upsert_component(
            &storyboard_canvas,
            LedgerComponent::markdown_card("frame-1", 80.0, 80.0, "Opening frame"),
        );

        let metadata =
            SurfaceLiveKitState::default().room_metadata(&surface, Some("agent-session-1"));

        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.surface_session_id, "surface:main");
        assert_eq!(
            metadata.agent_session_id.as_deref(),
            Some("agent-session-1")
        );
        assert_eq!(metadata.active_canvas_id.as_deref(), Some("canvas:2"));
        assert_eq!(metadata.canvases.len(), 2);
        assert!(
            metadata
                .canvases
                .iter()
                .any(|canvas| { canvas.canvas_id == "canvas:main" && canvas.component_count == 1 })
        );
        assert!(metadata.canvases.iter().any(|canvas| {
            canvas.canvas_id == storyboard_canvas && canvas.mode == SurfaceMode::Storyboard
        }));
    }

    #[test]
    fn participant_attributes_track_media_intent() {
        let mut state = SurfaceLiveKitState::default();
        state.toggle_mic();
        state.toggle_camera();

        let attributes = state.participant_attributes("surface:main");

        assert_eq!(attributes["ocean.client"], "ocean-gui");
        assert_eq!(attributes["ocean.surface_session_id"], "surface:main");
        assert_eq!(attributes["ocean.mic_enabled"], "true");
        assert_eq!(attributes["ocean.camera_enabled"], "true");
    }

    #[test]
    fn roster_sorts_local_first_then_by_identity_and_clears_on_disconnect() {
        let mut state = SurfaceLiveKitState::default();
        state.set_roster(vec![
            SurfaceLiveKitParticipant {
                identity: "remote-b".to_string(),
                name: "Bea".to_string(),
                local: false,
                mic: true,
                ..SurfaceLiveKitParticipant::default()
            },
            SurfaceLiveKitParticipant {
                identity: "human:local".to_string(),
                name: "Operator".to_string(),
                local: true,
                ..SurfaceLiveKitParticipant::default()
            },
            SurfaceLiveKitParticipant {
                identity: "remote-a".to_string(),
                name: "Ada".to_string(),
                local: false,
                camera: true,
                ..SurfaceLiveKitParticipant::default()
            },
        ]);

        let roster = state.roster();
        assert_eq!(roster.len(), 3);
        assert!(roster[0].local);
        assert_eq!(roster[0].identity, "human:local");
        assert_eq!(roster[1].identity, "remote-a");
        assert_eq!(roster[2].identity, "remote-b");

        state.mark_disconnected("left room");
        assert!(state.roster().is_empty());
    }

    #[test]
    fn mic_intent_can_be_cleared_after_publish_failure() {
        let mut state = SurfaceLiveKitState::default();

        assert!(state.toggle_mic());
        state.set_mic_enabled(false);

        assert!(!state.mic_enabled());
        assert_eq!(
            state.participant_attributes("surface:main")["ocean.mic_enabled"],
            "false"
        );
    }
}
