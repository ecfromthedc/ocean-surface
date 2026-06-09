//! Surface LiveKit client — **always-compiled** protocol/state facade.
//!
//! This module holds everything the GPUI shell (`view.rs`) needs to talk to a
//! LiveKit room *without* depending on native WebRTC: the join-request /
//! surface-update value types, the [`SurfaceLiveKitClientEvent`] vocabulary, the
//! command [`SurfaceLiveKitClientHandle`], request validation, and the
//! command-coalescing logic.
//!
//! The heavy, `webrtc-sys`-backed room session (connect / publish mic+camera /
//! decode remote video) lives in [`super::surface_livekit_session`] and is only
//! compiled when the `livekit` Cargo feature is enabled. [`spawn_surface_livekit_client`]
//! is re-exported from there under the feature; without it, a stub below reports
//! "voice not built in" so the default `cargo build -p ocean-gui` stays free of
//! native WebRTC.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::Sender;

use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::mpsc::{Receiver as ClientCommandReceiver, Sender as ClientCommandSender};

// `mpsc::channel` is used by the no-feature stub spawn and by the tests; the
// real spawn (with `livekit`) creates its channel in `surface_livekit_session`.
#[cfg(any(test, not(feature = "livekit")))]
use tokio::sync::mpsc;

use super::surface_livekit::{SurfaceLiveKitCredentials, SurfaceLiveKitParticipant};
use super::surface_livekit_video::SurfaceVideoFrame;

#[cfg(feature = "livekit")]
pub use super::surface_livekit_session::spawn_surface_livekit_client;

/// Bound on the in-flight surface-update command queue between the GPUI shell
/// and the LiveKit session thread. Read by [`super::surface_livekit_session`].
pub(super) const CLIENT_COMMAND_BUFFER: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceLiveKitJoinRequest {
    pub credentials: SurfaceLiveKitCredentials,
    pub initial_update: SurfaceLiveKitSurfaceUpdate,
}

impl SurfaceLiveKitJoinRequest {
    #[must_use]
    pub fn new(
        credentials: SurfaceLiveKitCredentials,
        room_metadata: String,
        participant_attributes: BTreeMap<String, String>,
        mic_enabled: bool,
        camera_enabled: bool,
    ) -> Self {
        Self {
            credentials,
            initial_update: SurfaceLiveKitSurfaceUpdate::new(
                room_metadata,
                participant_attributes,
                mic_enabled,
                camera_enabled,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceLiveKitSurfaceUpdate {
    pub room_metadata: String,
    pub participant_attributes: BTreeMap<String, String>,
    pub mic_enabled: bool,
    pub camera_enabled: bool,
}

impl SurfaceLiveKitSurfaceUpdate {
    #[must_use]
    pub fn new(
        room_metadata: String,
        participant_attributes: BTreeMap<String, String>,
        mic_enabled: bool,
        camera_enabled: bool,
    ) -> Self {
        Self {
            room_metadata,
            participant_attributes,
            mic_enabled,
            camera_enabled,
        }
    }
}

/// Events the LiveKit session reports up to the GPUI shell. `view.rs` matches on
/// every variant, but only the real session (`feature = "livekit"`) *constructs*
/// the full set — the no-feature stub only emits `Failed` — so the variants are
/// "never constructed" in the default build. They are not dead code; suppress
/// the lint there rather than fragment the enum across cfgs.
#[cfg_attr(not(feature = "livekit"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceLiveKitClientEvent {
    Joining { room: String },
    Joined { room: String, participant: String },
    MetadataPublished { room: String },
    SurfaceStatePublished { room: String },
    SurfaceStateFailed { room: String, error: String },
    MicrophonePublished { room: String, track_sid: String },
    MicrophoneUnpublished { room: String },
    MicrophoneFailed { room: String, error: String },
    CameraPublished { room: String, track_sid: String },
    CameraUnpublished { room: String },
    CameraFailed { room: String, error: String },
    /// A subscribed remote video track started streaming; the GPUI shell can
    /// render a live tile for `participant_identity` once frames arrive.
    RemoteVideoSubscribed {
        room: String,
        participant_identity: String,
        track_sid: String,
    },
    /// A subscribed remote video track ended (unsubscribed / participant left);
    /// the GPUI shell should drop the tile for `track_sid`.
    RemoteVideoRemoved {
        room: String,
        participant_identity: String,
        track_sid: String,
    },
    /// A freshly decoded BGRA frame for a remote video tile. Only the latest
    /// frame per track is delivered (older frames are dropped under load).
    RemoteVideoFrame {
        room: String,
        frame: SurfaceVideoFrame,
    },
    MediaFailed { room: String, error: String },
    ConnectionState { room: String, state: String },
    RosterUpdated {
        room: String,
        participants: Vec<SurfaceLiveKitParticipant>,
    },
    Disconnected { room: String, reason: String },
    Failed { room: String, error: String },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum SurfaceLiveKitClientCommand {
    UpdateSurface(SurfaceLiveKitSurfaceUpdate),
    Disconnect,
}

/// Produced by [`coalesce_surface_update`]; consumed only by the LiveKit
/// session loop, so it is dead code in the no-feature build.
#[cfg_attr(not(any(feature = "livekit", test)), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(super) enum SurfaceLiveKitClientAction {
    UpdateSurface(SurfaceLiveKitSurfaceUpdate),
    Disconnect,
    Closed,
}

#[derive(Clone, Debug)]
pub struct SurfaceLiveKitClientHandle {
    sender: ClientCommandSender<SurfaceLiveKitClientCommand>,
}

impl SurfaceLiveKitClientHandle {
    #[must_use]
    pub(super) fn new(sender: ClientCommandSender<SurfaceLiveKitClientCommand>) -> Self {
        Self { sender }
    }

    pub fn try_update_surface(
        &self,
        update: SurfaceLiveKitSurfaceUpdate,
    ) -> Result<(), SurfaceLiveKitCommandError> {
        self.sender
            .try_send(SurfaceLiveKitClientCommand::UpdateSurface(update))
            .map_err(SurfaceLiveKitCommandError::from)
    }

    pub fn try_disconnect(&self) -> Result<(), SurfaceLiveKitCommandError> {
        self.sender
            .try_send(SurfaceLiveKitClientCommand::Disconnect)
            .map_err(SurfaceLiveKitCommandError::from)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceLiveKitCommandError {
    Full,
    Closed,
}

impl fmt::Display for SurfaceLiveKitCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("LiveKit command queue is full"),
            Self::Closed => formatter.write_str("LiveKit command queue is closed"),
        }
    }
}

impl From<TrySendError<SurfaceLiveKitClientCommand>> for SurfaceLiveKitCommandError {
    fn from(error: TrySendError<SurfaceLiveKitClientCommand>) -> Self {
        match error {
            TrySendError::Full(_) => Self::Full,
            TrySendError::Closed(_) => Self::Closed,
        }
    }
}

// Validation is invoked from the LiveKit session (real spawn) and the tests; in
// the no-feature, non-test build nothing calls it.
#[cfg_attr(not(any(feature = "livekit", test)), allow(dead_code))]
pub fn validate_join_request(request: &SurfaceLiveKitJoinRequest) -> Result<(), String> {
    if request.credentials.url.trim().is_empty() {
        return Err("missing LiveKit url".to_string());
    }
    if request.credentials.room.trim().is_empty() {
        return Err("missing LiveKit room".to_string());
    }
    if request.credentials.token.trim().is_empty() {
        return Err("missing LiveKit token".to_string());
    }
    validate_surface_update(&request.initial_update)
}

#[cfg_attr(not(any(feature = "livekit", test)), allow(dead_code))]
pub fn validate_surface_update(update: &SurfaceLiveKitSurfaceUpdate) -> Result<(), String> {
    if update.room_metadata.trim().is_empty() {
        return Err("missing surface metadata".to_string());
    }
    Ok(())
}

/// Stub used when the `livekit` feature is **disabled**.
///
/// The default `cargo build -p ocean-gui` does not link native WebRTC, so there
/// is no real LiveKit room to join. We still honour the same signature the GPUI
/// shell calls, returning a live [`SurfaceLiveKitClientHandle`] (whose commands
/// are simply dropped) and immediately emitting a `Failed` event so the shell
/// surfaces a clear "voice not built in" status instead of hanging. Rebuild with
/// `--features livekit` to get the real session ([`super::surface_livekit_session`]).
#[cfg(not(feature = "livekit"))]
pub fn spawn_surface_livekit_client(
    request: SurfaceLiveKitJoinRequest,
    sender: Sender<SurfaceLiveKitClientEvent>,
) -> SurfaceLiveKitClientHandle {
    let (command_sender, _command_receiver) = mpsc::channel(CLIENT_COMMAND_BUFFER);
    let handle = SurfaceLiveKitClientHandle::new(command_sender);
    send_client_event(
        &sender,
        SurfaceLiveKitClientEvent::Failed {
            room: request.credentials.room,
            error: "voice/LiveKit support is not built in (rebuild ocean-gui with --features livekit)"
                .to_string(),
        },
    );
    handle
}

// Drains queued surface-update commands to the latest; driven by the LiveKit
// session loop (and the tests). Dead code in the no-feature, non-test build.
#[cfg_attr(not(any(feature = "livekit", test)), allow(dead_code))]
pub(super) fn coalesce_surface_update(
    commands: &mut ClientCommandReceiver<SurfaceLiveKitClientCommand>,
    mut update: SurfaceLiveKitSurfaceUpdate,
) -> SurfaceLiveKitClientAction {
    loop {
        match commands.try_recv() {
            Ok(SurfaceLiveKitClientCommand::UpdateSurface(next_update)) => {
                update = next_update;
            }
            Ok(SurfaceLiveKitClientCommand::Disconnect) => {
                return SurfaceLiveKitClientAction::Disconnect;
            }
            Err(TryRecvError::Empty) => {
                return SurfaceLiveKitClientAction::UpdateSurface(update);
            }
            Err(TryRecvError::Disconnected) => {
                return SurfaceLiveKitClientAction::Closed;
            }
        }
    }
}

pub(super) fn send_client_event(sender: &Sender<SurfaceLiveKitClientEvent>, event: SurfaceLiveKitClientEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_join_request() -> SurfaceLiveKitJoinRequest {
        SurfaceLiveKitJoinRequest::new(
            SurfaceLiveKitCredentials {
                url: "wss://livekit.example.test".to_string(),
                room: "ocean-surface-main".to_string(),
                token: "signed-token".to_string(),
                expires_at: "2026-06-03T22:00:00Z".to_string(),
            },
            r#"{"surface_session_id":"surface:main"}"#.to_string(),
            BTreeMap::from([
                ("ocean.client".to_string(), "ocean-gui".to_string()),
                ("ocean.surface_id".to_string(), "gpui:local".to_string()),
            ]),
            false,
            false,
        )
    }

    #[test]
    fn join_request_requires_credentials_and_metadata() {
        let mut request = valid_join_request();
        assert_eq!(validate_join_request(&request), Ok(()));

        request.credentials.url.clear();
        assert_eq!(
            validate_join_request(&request),
            Err("missing LiveKit url".to_string())
        );

        request = valid_join_request();
        request.credentials.token.clear();
        assert_eq!(
            validate_join_request(&request),
            Err("missing LiveKit token".to_string())
        );

        request = valid_join_request();
        request.initial_update.room_metadata.clear();
        assert_eq!(
            validate_join_request(&request),
            Err("missing surface metadata".to_string())
        );
    }

    #[test]
    fn join_request_preserves_surface_snapshot_and_attributes() {
        let request = valid_join_request();

        assert!(
            request
                .initial_update
                .room_metadata
                .contains("surface:main")
        );
        assert_eq!(
            request.initial_update.participant_attributes["ocean.client"],
            "ocean-gui"
        );
        assert_eq!(request.credentials.room, "ocean-surface-main");
        assert!(!request.initial_update.mic_enabled);
        assert!(!request.initial_update.camera_enabled);
    }

    #[test]
    fn command_handle_reports_full_and_closed_queues() {
        let (sender, receiver): (_, mpsc::Receiver<SurfaceLiveKitClientCommand>) = mpsc::channel(1);
        let handle = SurfaceLiveKitClientHandle::new(sender);
        let update = valid_join_request().initial_update;

        assert_eq!(handle.try_update_surface(update.clone()), Ok(()));
        assert_eq!(
            handle.try_update_surface(update),
            Err(SurfaceLiveKitCommandError::Full)
        );

        drop(receiver);
        assert_eq!(
            handle.try_disconnect(),
            Err(SurfaceLiveKitCommandError::Closed)
        );
    }

    #[test]
    fn coalesces_queued_surface_updates_to_latest_update() {
        let (sender, mut receiver) = mpsc::channel(4);
        let first = valid_join_request().initial_update;
        let mut second = first.clone();
        second.room_metadata = r#"{"surface_session_id":"surface:second"}"#.to_string();
        let mut third = first.clone();
        third.room_metadata = r#"{"surface_session_id":"surface:third"}"#.to_string();

        sender
            .try_send(SurfaceLiveKitClientCommand::UpdateSurface(second))
            .expect("second update should enqueue");
        sender
            .try_send(SurfaceLiveKitClientCommand::UpdateSurface(third))
            .expect("third update should enqueue");

        assert_eq!(
            coalesce_surface_update(&mut receiver, first),
            SurfaceLiveKitClientAction::UpdateSurface(SurfaceLiveKitSurfaceUpdate {
                room_metadata: r#"{"surface_session_id":"surface:third"}"#.to_string(),
                participant_attributes: BTreeMap::from([
                    ("ocean.client".to_string(), "ocean-gui".to_string()),
                    ("ocean.surface_id".to_string(), "gpui:local".to_string()),
                ]),
                mic_enabled: false,
                camera_enabled: false,
            })
        );
    }

    #[test]
    fn disconnect_command_wins_over_queued_surface_updates() {
        let (sender, mut receiver) = mpsc::channel(4);
        let first = valid_join_request().initial_update;
        let second = first.clone();

        sender
            .try_send(SurfaceLiveKitClientCommand::UpdateSurface(second))
            .expect("update should enqueue");
        sender
            .try_send(SurfaceLiveKitClientCommand::Disconnect)
            .expect("disconnect should enqueue");

        assert_eq!(
            coalesce_surface_update(&mut receiver, first),
            SurfaceLiveKitClientAction::Disconnect
        );
    }
}
