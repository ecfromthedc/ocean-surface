use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::canvas::CanvasLedger;
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
///
/// `active_canvas_id` is the canvas the collaborator is currently viewing,
/// parsed from the [`SurfaceRoomMetadata`] they publish to the room (the receive
/// side of the §11 compact-pointer contract). It is the key that scopes
/// per-canvas presence: a collaborator whose `active_canvas_id` differs from the
/// operator's active canvas is NOT shown on that canvas (§14 Slice 10 — the
/// canvas_revision/canvas_id guard, applied on receipt). `None` when the
/// participant has published no canvas pointer yet (e.g. a voice-only peer, or
/// the moment before their first metadata arrives).
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
    /// The canvas this collaborator is viewing, from their published room
    /// metadata. Scopes presence to the active canvas; see the type docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_canvas_id: Option<String>,
    /// The revision of the collaborator's active canvas, from their published
    /// metadata. Compact pointer only (§11) — lets a viewer tell that a
    /// collaborator's canvas advanced without carrying the document. `None`
    /// when the peer published no native canvas revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_revision: Option<u64>,
}

impl SurfaceLiveKitParticipant {
    /// Parse the `active_canvas_id` (+ `canvas_revision`) a remote participant is
    /// viewing out of the [`SurfaceRoomMetadata`] JSON they published to the room.
    ///
    /// This is the **receive** side of the §11 compact-pointer contract: the
    /// local client publishes its active-canvas pointer via
    /// [`SurfaceLiveKitState::room_metadata_for`]; every other client reads that
    /// same compact JSON off the peer's LiveKit metadata to learn which canvas the
    /// peer is on. Only the two compact pointers are read — never any component
    /// body. A blank or unparseable payload yields `(None, None)` (a voice-only
    /// peer, or a pre-first-metadata frame), which scopes the peer onto no canvas.
    ///
    /// Only the real LiveKit session (`feature = "livekit"`) reads a peer's
    /// metadata off the wire, so this is dead code in the default build; the
    /// scoping it feeds ([`presence_on_canvas`]) is exercised there and in tests.
    #[cfg_attr(not(any(feature = "livekit", test)), allow(dead_code))]
    #[must_use]
    pub fn canvas_pointer_from_metadata(metadata: &str) -> (Option<String>, Option<u64>) {
        if metadata.trim().is_empty() {
            return (None, None);
        }
        match serde_json::from_str::<SurfaceRoomMetadata>(metadata) {
            Ok(meta) => (meta.active_canvas_id, meta.canvas_revision),
            Err(_) => (None, None),
        }
    }
}

/// Filter a roster down to the **remote** collaborators whose published active
/// canvas matches `active_canvas_id` — the presence to render *on that canvas*.
///
/// This is the canvas_revision/canvas_id guard from the receive side (§14
/// Slice 10): a collaborator on `canvas:storyboard` must not surface as present
/// on `canvas:workflow`. The local participant is always excluded (you are not a
/// "collaborator" on your own canvas), and `None` for `active_canvas_id` (no
/// canvas focused locally) yields an empty slice — there is no canvas to scope
/// presence onto. Order is preserved from the (already local-first, then
/// identity-sorted) roster, minus the local row, so the overlay is deterministic.
#[must_use]
pub fn presence_on_canvas<'a>(
    roster: &'a [SurfaceLiveKitParticipant],
    active_canvas_id: Option<&str>,
) -> Vec<&'a SurfaceLiveKitParticipant> {
    let Some(active) = active_canvas_id else {
        return Vec::new();
    };
    roster
        .iter()
        .filter(|participant| !participant.local)
        .filter(|participant| participant.active_canvas_id.as_deref() == Some(active))
        .collect()
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

    /// The remote collaborators present **on the given active canvas**, scoped by
    /// the canvas_id each peer published in their room metadata (§14 Slice 10).
    ///
    /// Thin wrapper over [`presence_on_canvas`] against the live roster: the
    /// native surface calls this with [`SurfaceState::active_canvas_id`] /
    /// [`CanvasLedgerSet::active_id`] to render presence markers for exactly the
    /// collaborators looking at the same board, and nobody else.
    #[must_use]
    pub fn presence_on_canvas(
        &self,
        active_canvas_id: Option<&str>,
    ) -> Vec<&SurfaceLiveKitParticipant> {
        presence_on_canvas(&self.roster, active_canvas_id)
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

    /// Build the compact LiveKit room metadata. `active_canvas_id` always tracks
    /// the operator's currently selected pane ([`SurfaceState::active_canvas_id`]);
    /// `canvas_revision` is sourced from the native [`CanvasLedger`] only when the
    /// selected pane IS that native canvas (gpui_masterbuild.md §11 + §14 Slice 10,
    /// the deferred-from-Slice-6 `canvas_revision` hook).
    ///
    /// Hard rule (§11): this payload carries **compact pointers only** —
    /// `{session_id, surface_id, active_canvas_id, canvas_revision}` plus media
    /// intent and per-canvas summary counts. The full canvas document (component
    /// content, ports, edges, patch log, CRDT) is **never** synced through
    /// LiveKit metadata. The native ledger is read only for its `canvas_id` and
    /// `revision`; none of its component bodies cross the wire here.
    #[must_use]
    pub fn room_metadata_for(
        &self,
        surface: &SurfaceState,
        native_ledger: Option<&CanvasLedger>,
        agent_session_id: Option<&str>,
    ) -> SurfaceRoomMetadata {
        let context = surface.turn_context();
        let canvases = context
            .canvases
            .iter()
            .map(SurfaceCanvasRoomMetadata::from)
            .collect();

        // The active canvas pointer ALWAYS follows the operator's currently
        // selected pane (`SurfaceState::active_canvas_id()`). A background native
        // CanvasLedger must NOT override which canvas the operator is viewing:
        // opening a canvas pane mounts a native ledger (e.g. canvas:main) but
        // selecting a different pane only updates `surface`, leaving that ledger
        // in place. Publishing the ledger's id unconditionally would mis-sync
        // collaborators onto a canvas the operator has already switched away from.
        //
        // `canvas_revision` is the native ledger's revision ONLY when the
        // selected/active pane IS that native canvas; otherwise it is omitted —
        // a revision is meaningless for a pointer that doesn't reference the
        // native ledger.
        let active_canvas_id = surface.active_canvas_id().map(str::to_string);
        let canvas_revision = native_ledger.and_then(|ledger| {
            (active_canvas_id.as_deref() == Some(ledger.canvas_id.as_str()))
                .then_some(ledger.revision)
        });

        SurfaceRoomMetadata {
            version: ROOM_METADATA_VERSION,
            room_id: self.room_id.clone(),
            surface_id: self.surface_id.clone(),
            surface_session_id: context.session_id,
            agent_session_id: agent_session_id.map(str::to_string),
            active_pane_id: context.active_pane_id,
            active_canvas_id,
            canvas_revision,
            media: SurfaceMediaMetadata {
                mic_enabled: self.mic_enabled,
                camera_enabled: self.camera_enabled,
            },
            canvases,
        }
    }

    /// JSON encoding of [`Self::room_metadata_for`] — the compact metadata
    /// published to the LiveKit room. The active canvas pointer follows the
    /// selected pane; the revision is attached only when that pane is the native
    /// [`CanvasLedger`]'s canvas.
    pub fn room_metadata_for_json(
        &self,
        surface: &SurfaceState,
        native_ledger: Option<&CanvasLedger>,
        agent_session_id: Option<&str>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.room_metadata_for(surface, native_ledger, agent_session_id))
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
    /// Revision counter of the active native [`CanvasLedger`] — the compact
    /// pointer collaborators use to detect that the active canvas advanced
    /// (§11). `None` when no native ledger is mounted. This is a single integer:
    /// the full canvas document is never carried here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_revision: Option<u64>,
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
    use crate::shell::canvas::{
        ActorRef, CanvasComponentPatch, CanvasId, CanvasLedger, CanvasMode, ComponentId, Rect,
        SurfacePatch,
    };
    use crate::shell::surface::{LedgerComponent, SurfaceMode};
    use serde_json::Value;

    /// Build a native ledger that has had `n` components upserted, so its
    /// `revision` is non-zero. Mirrors the agent patch hot path.
    fn main_pane_id_for(surface: &SurfaceState, canvas_id: &str) -> String {
        surface
            .turn_context()
            .panes
            .into_iter()
            .find(|pane| pane.canvas_id.as_deref() == Some(canvas_id))
            .map(|pane| pane.pane_id)
            .unwrap_or_else(|| panic!("no pane hosts {canvas_id}"))
    }

    fn native_ledger_with_components(canvas_id: &str, n: usize) -> CanvasLedger {
        let mut ledger = CanvasLedger::new(canvas_id, "surface:main", CanvasMode::Freeform);
        for i in 0..n {
            ledger.apply_patch(
                SurfacePatch::UpsertComponent {
                    component: CanvasComponentPatch {
                        id: ComponentId::new(format!("comp-{i}")),
                        kind: "brief_card".to_string(),
                        rect: Some(Rect::new(40.0, 40.0, 240.0, 160.0)),
                        z_index: None,
                        content: serde_json::json!({
                            "title": format!("Secret brief {i}"),
                            "body": "long confidential canvas document body that must not leak",
                        }),
                        metadata: Value::Null,
                    },
                },
                ActorRef::agent(Some("sage".into())),
                1_000 + i as i64,
            );
        }
        ledger
    }

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

        let metadata = SurfaceLiveKitState::default().room_metadata_for(
            &surface,
            None,
            Some("agent-session-1"),
        );

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
    fn room_metadata_sources_active_canvas_and_revision_from_native_ledger() {
        // Native ledger is the source of truth for the active canvas pointer +
        // revision (§14 Slice 10, the deferred-from-Slice-6 canvas_revision hook).
        let surface = SurfaceState::default();
        let ledger = native_ledger_with_components("canvas:main", 3);
        assert_eq!(ledger.revision, 3, "three upserts bump the ledger to rev 3");

        let metadata = SurfaceLiveKitState::default().room_metadata_for(
            &surface,
            Some(&ledger),
            Some("agent-session-1"),
        );

        assert_eq!(metadata.active_canvas_id.as_deref(), Some("canvas:main"));
        assert_eq!(
            metadata.canvas_revision,
            Some(3),
            "canvas_revision must come from the native ledger, not the legacy surface"
        );
    }

    #[test]
    fn room_metadata_without_native_ledger_falls_back_and_omits_revision() {
        // No native ledger mounted yet → legacy SurfaceState active canvas, and
        // no canvas_revision is emitted (skipped, not zero).
        let surface = SurfaceState::default();

        let metadata =
            SurfaceLiveKitState::default().room_metadata_for(&surface, None, Some("agent-1"));

        assert_eq!(metadata.canvas_revision, None);
        let json = serde_json::to_string(&metadata).expect("metadata serializes");
        assert!(
            !json.contains("canvas_revision"),
            "canvas_revision is skipped when absent: {json}"
        );
    }

    #[test]
    fn patch_advancing_ledger_yields_updated_compact_revision() {
        // Simulate the apply_surface_patch_event hot path: a patch advances the
        // native ledger revision, and the next compact metadata reflects it.
        let surface = SurfaceState::default();
        let state = SurfaceLiveKitState::default();

        let before = native_ledger_with_components("canvas:main", 1);
        let after = native_ledger_with_components("canvas:main", 2);

        let meta_before = state.room_metadata_for(&surface, Some(&before), None);
        let meta_after = state.room_metadata_for(&surface, Some(&after), None);

        assert_eq!(meta_before.canvas_revision, Some(1));
        assert_eq!(meta_after.canvas_revision, Some(2));
        assert!(
            meta_after.canvas_revision > meta_before.canvas_revision,
            "a patch event must advance the published canvas_revision"
        );
    }

    #[test]
    fn active_canvas_follows_selected_pane_not_background_native_ledger() {
        // Regression (OCEAN-172 / Codex P2): a native ledger for canvas:main can
        // linger after the operator opens + selects a different pane. The published
        // active_canvas_id MUST follow the operator's selected pane, and the
        // ledger's revision MUST NOT be attached when the operator is no longer
        // viewing the native canvas — otherwise collaborators mis-sync onto a
        // canvas the operator already switched away from.
        let mut surface = SurfaceState::default();
        assert_eq!(surface.active_canvas_id(), Some("canvas:main"));

        // Open a second pane on a different canvas and select it (open_canvas_pane
        // makes the new pane active).
        let other_canvas = surface.open_canvas_pane("Storyboard", SurfaceMode::Storyboard);
        assert_ne!(other_canvas, "canvas:main");
        assert_eq!(surface.active_canvas_id(), Some(other_canvas.as_str()));

        // A native ledger for canvas:main is still mounted in the background.
        let ledger = native_ledger_with_components("canvas:main", 3);

        let metadata = SurfaceLiveKitState::default().room_metadata_for(
            &surface,
            Some(&ledger),
            Some("agent-1"),
        );

        assert_eq!(
            metadata.active_canvas_id.as_deref(),
            Some(other_canvas.as_str()),
            "active_canvas_id must be the selected pane, not the background native ledger"
        );
        assert_eq!(
            metadata.canvas_revision, None,
            "canvas_revision must be omitted when the active pane is not the native ledger's canvas"
        );

        // And when the operator switches BACK to canvas:main, the revision attaches.
        // Find the pane that hosts canvas:main and select it explicitly.
        let main_pane_id = main_pane_id_for(&surface, "canvas:main");
        assert!(surface.set_active_pane(&main_pane_id));
        assert_eq!(surface.active_canvas_id(), Some("canvas:main"));

        let metadata_back = SurfaceLiveKitState::default().room_metadata_for(
            &surface,
            Some(&ledger),
            Some("agent-1"),
        );
        assert_eq!(
            metadata_back.active_canvas_id.as_deref(),
            Some("canvas:main")
        );
        assert_eq!(
            metadata_back.canvas_revision,
            Some(3),
            "revision attaches once the operator is viewing the native canvas again"
        );
    }

    #[test]
    fn room_metadata_is_compact_and_never_carries_the_full_canvas_document() {
        // §11 hard rule: LiveKit metadata carries compact pointers only. The
        // native ledger holds confidential component bodies + a patch log; none
        // of that may appear in the published metadata JSON.
        let surface = SurfaceState::default();
        let ledger = native_ledger_with_components("canvas:main", 4);

        // Sanity: the full ledger DOES contain the heavy fields we must exclude.
        let full_ledger_json = serde_json::to_string(&ledger).expect("ledger serializes");
        assert!(full_ledger_json.contains("Secret brief"));
        assert!(full_ledger_json.contains("patch_log"));

        let metadata = SurfaceLiveKitState::default().room_metadata_for(
            &surface,
            Some(&ledger),
            Some("agent-1"),
        );
        let json = state_metadata_json(&metadata);

        // Compact pointers ARE present.
        assert!(json.contains("\"active_canvas_id\":\"canvas:main\""));
        assert!(json.contains("\"canvas_revision\":4"));

        // The full canvas document is NEVER present.
        assert!(
            !json.contains("Secret brief"),
            "component content must not leak into LiveKit metadata: {json}"
        );
        assert!(
            !json.contains("confidential canvas document body"),
            "component body must not leak into LiveKit metadata: {json}"
        );
        assert!(
            !json.contains("patch_log"),
            "patch log must not leak into LiveKit metadata: {json}"
        );
        assert!(
            !json.contains("created_by") && !json.contains("updated_at_ms"),
            "per-component provenance must not leak into LiveKit metadata: {json}"
        );
    }

    fn state_metadata_json(metadata: &SurfaceRoomMetadata) -> String {
        serde_json::to_string(metadata).expect("metadata serializes")
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

    // ---- Slice 10: canvas-scoped collaborator presence (OCEAN-280) ----

    fn remote_on_canvas(identity: &str, canvas_id: Option<&str>) -> SurfaceLiveKitParticipant {
        SurfaceLiveKitParticipant {
            identity: identity.to_string(),
            name: identity.to_string(),
            local: false,
            active_canvas_id: canvas_id.map(str::to_string),
            ..SurfaceLiveKitParticipant::default()
        }
    }

    #[test]
    fn canvas_pointer_parses_active_canvas_and_revision_from_published_metadata() {
        // The wire shape a peer publishes is exactly SurfaceLiveKitState's compact
        // room metadata; the receive side must read the same pointers back out.
        let surface = SurfaceState::default();
        let ledger = native_ledger_with_components("canvas:storyboard", 2);
        let json = SurfaceLiveKitState::default()
            .room_metadata_for_json(&surface, Some(&ledger), Some("agent-1"))
            .expect("metadata serializes");

        // The active canvas pointer follows the operator's *selected* pane, not the
        // background ledger — so to publish "I'm on storyboard" the pane must be it.
        // Here the default surface pane is canvas:main, so the published pointer is
        // canvas:main with no revision (ledger canvas != active pane). That is the
        // honest contract; assert exactly that.
        let (canvas_id, revision) = SurfaceLiveKitParticipant::canvas_pointer_from_metadata(&json);
        assert_eq!(canvas_id.as_deref(), Some("canvas:main"));
        assert_eq!(revision, None);

        // A hand-built payload where the peer genuinely sits on the native ledger.
        let on_ledger = r#"{"version":1,"room_id":"r","surface_id":"s","surface_session_id":"sess","active_pane_id":"p","active_canvas_id":"canvas:storyboard","canvas_revision":7,"media":{"mic_enabled":false,"camera_enabled":false},"canvases":[]}"#;
        let (canvas_id, revision) =
            SurfaceLiveKitParticipant::canvas_pointer_from_metadata(on_ledger);
        assert_eq!(canvas_id.as_deref(), Some("canvas:storyboard"));
        assert_eq!(revision, Some(7));
    }

    #[test]
    fn canvas_pointer_from_blank_or_garbage_metadata_is_none() {
        // Voice-only peer (no surface metadata) or a pre-first-metadata frame.
        assert_eq!(
            SurfaceLiveKitParticipant::canvas_pointer_from_metadata(""),
            (None, None)
        );
        assert_eq!(
            SurfaceLiveKitParticipant::canvas_pointer_from_metadata("   "),
            (None, None)
        );
        assert_eq!(
            SurfaceLiveKitParticipant::canvas_pointer_from_metadata("not json"),
            (None, None)
        );
    }

    #[test]
    fn presence_is_scoped_to_the_active_canvas_and_a_peer_on_another_canvas_is_hidden() {
        // Two collaborators on different canvases. Viewing canvas:workflow must show
        // only the workflow peer — the storyboard peer is NOT present here. This is
        // the §14 Slice 10 canvas_id guard on the receive side.
        let roster = vec![
            remote_on_canvas("ada", Some("canvas:workflow")),
            remote_on_canvas("bo", Some("canvas:storyboard")),
        ];

        let on_workflow = presence_on_canvas(&roster, Some("canvas:workflow"));
        assert_eq!(on_workflow.len(), 1);
        assert_eq!(on_workflow[0].identity, "ada");

        let on_storyboard = presence_on_canvas(&roster, Some("canvas:storyboard"));
        assert_eq!(on_storyboard.len(), 1);
        assert_eq!(on_storyboard[0].identity, "bo");
    }

    #[test]
    fn switching_active_canvas_changes_the_collaborators_shown() {
        // The same roster, scoped against a different active canvas, yields a
        // different presence set — i.e. switching canvases re-scopes presence.
        let state = {
            let mut state = SurfaceLiveKitState::default();
            state.set_roster(vec![
                remote_on_canvas("ada", Some("canvas:workflow")),
                remote_on_canvas("bo", Some("canvas:storyboard")),
                remote_on_canvas("cy", Some("canvas:workflow")),
            ]);
            state
        };

        let workflow: Vec<&str> = state
            .presence_on_canvas(Some("canvas:workflow"))
            .iter()
            .map(|p| p.identity.as_str())
            .collect();
        assert_eq!(workflow, vec!["ada", "cy"]);

        let storyboard: Vec<&str> = state
            .presence_on_canvas(Some("canvas:storyboard"))
            .iter()
            .map(|p| p.identity.as_str())
            .collect();
        assert_eq!(storyboard, vec!["bo"]);
    }

    #[test]
    fn local_participant_is_never_shown_as_a_collaborator_on_its_own_canvas() {
        // The local row carries our own canvas pointer, but we are not a
        // "collaborator" — presence_on_canvas must exclude `local`.
        let roster = vec![
            SurfaceLiveKitParticipant {
                identity: "human:local".to_string(),
                name: "Operator".to_string(),
                local: true,
                active_canvas_id: Some("canvas:main".to_string()),
                ..SurfaceLiveKitParticipant::default()
            },
            remote_on_canvas("ada", Some("canvas:main")),
        ];

        let present = presence_on_canvas(&roster, Some("canvas:main"));
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].identity, "ada");
        assert!(present.iter().all(|p| !p.local));
    }

    #[test]
    fn presence_is_empty_when_no_canvas_is_active_or_no_peer_published_one() {
        let roster = vec![
            remote_on_canvas("ada", Some("canvas:workflow")),
            remote_on_canvas("voiceonly", None),
        ];

        // No active canvas locally → nothing to scope onto.
        assert!(presence_on_canvas(&roster, None).is_empty());

        // A peer that published no canvas pointer is never on any canvas.
        let only_voice = vec![remote_on_canvas("voiceonly", None)];
        assert!(presence_on_canvas(&only_voice, Some("canvas:workflow")).is_empty());
    }

    #[test]
    fn participant_roundtrips_active_canvas_id_through_serde() {
        let participant = remote_on_canvas("ada", Some("canvas:workflow"));
        let json = serde_json::to_string(&participant).expect("serializes");
        assert!(json.contains("canvas:workflow"));
        let back: SurfaceLiveKitParticipant = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.active_canvas_id.as_deref(), Some("canvas:workflow"));
    }
}
