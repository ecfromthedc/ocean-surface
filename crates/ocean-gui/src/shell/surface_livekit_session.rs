//! Native LiveKit room session — the heavy, `webrtc-sys`-backed half of the
//! surface voice/video panel.
//!
//! This module is **only compiled with the `livekit` feature** (see
//! `crates/ocean-gui/Cargo.toml`). It owns everything that actually links
//! against native libwebrtc: connecting to the LiveKit room, publishing the
//! local mic/camera, decoding remote video tracks, and translating
//! `livekit::RoomEvent`s into the crate-local [`SurfaceLiveKitClientEvent`]
//! vocabulary the GPUI shell understands.
//!
//! The pure protocol/state types and the command handle live in the always-
//! compiled sibling [`super::surface_livekit_client`], so the GPUI shell
//! (`view.rs`) talks to the same API surface whether or not the `livekit`
//! feature is enabled. When the feature is off, that sibling provides a stub
//! [`super::surface_livekit_client::spawn_surface_livekit_client`] that reports
//! "voice not built in" instead of pulling native WebRTC.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::thread;

use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::{
    LocalAudioTrack, LocalParticipant, LocalTrack, LocalTrackPublication, LocalVideoTrack,
    PlatformAudio, RemoteTrack, RtcAudioSource, TrackSource,
};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::{ConnectionState, Room, RoomEvent, RoomOptions};
use tokio::sync::mpsc::{self, Receiver as ClientCommandReceiver};
use tokio::task::JoinHandle;

use super::surface_livekit::SurfaceLiveKitParticipant;
use super::surface_livekit_client::{
    CLIENT_COMMAND_BUFFER, SurfaceLiveKitClientAction, SurfaceLiveKitClientCommand,
    SurfaceLiveKitClientEvent, SurfaceLiveKitClientHandle, SurfaceLiveKitJoinRequest,
    SurfaceLiveKitSurfaceUpdate, coalesce_surface_update, send_client_event, validate_join_request,
    validate_surface_update,
};
use super::surface_livekit_video::decode_bgra;

pub fn spawn_surface_livekit_client(
    request: SurfaceLiveKitJoinRequest,
    sender: Sender<SurfaceLiveKitClientEvent>,
) -> SurfaceLiveKitClientHandle {
    let (command_sender, command_receiver) = mpsc::channel(CLIENT_COMMAND_BUFFER);
    let handle = SurfaceLiveKitClientHandle::new(command_sender);
    let room = request.credentials.room.clone();
    if let Err(error) = validate_join_request(&request) {
        send_client_event(&sender, SurfaceLiveKitClientEvent::Failed { room, error });
        return handle;
    }

    let failure_sender = sender.clone();
    if let Err(error) = thread::Builder::new()
        .name("ocean-livekit-client".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    send_client_event(
                        &sender,
                        SurfaceLiveKitClientEvent::Failed {
                            room: request.credentials.room,
                            error: format!("failed to start LiveKit runtime: {error}"),
                        },
                    );
                    return;
                }
            };

            runtime.block_on(join_surface_livekit_room(request, command_receiver, sender));
        })
    {
        send_client_event(
            &failure_sender,
            SurfaceLiveKitClientEvent::Failed {
                room,
                error: format!("failed to spawn LiveKit client thread: {error}"),
            },
        );
    }

    handle
}

async fn join_surface_livekit_room(
    request: SurfaceLiveKitJoinRequest,
    mut commands: ClientCommandReceiver<SurfaceLiveKitClientCommand>,
    sender: Sender<SurfaceLiveKitClientEvent>,
) {
    let room_id = request.credentials.room.clone();
    send_client_event(
        &sender,
        SurfaceLiveKitClientEvent::Joining {
            room: room_id.clone(),
        },
    );

    let mut options = RoomOptions::default();
    options.auto_subscribe = true;
    options.adaptive_stream = true;
    options.dynacast = true;

    let (room, mut events) = match Room::connect(
        &request.credentials.url,
        &request.credentials.token,
        options,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            send_client_event(
                &sender,
                SurfaceLiveKitClientEvent::Failed {
                    room: room_id,
                    error: error.to_string(),
                },
            );
            return;
        }
    };

    let local_participant = room.local_participant();
    let participant_id = local_participant.identity().to_string();

    if let Err(error) = publish_surface_update(&local_participant, &request.initial_update).await {
        send_client_event(
            &sender,
            SurfaceLiveKitClientEvent::Failed {
                room: room_id.clone(),
                error,
            },
        );
        let _ = room.close().await;
        return;
    }

    let mut published_microphone = None;
    if let Err(error) = reconcile_microphone(
        &room,
        &mut published_microphone,
        request.initial_update.mic_enabled,
        &sender,
        &room_id,
    )
    .await
    {
        send_client_event(
            &sender,
            SurfaceLiveKitClientEvent::MicrophoneFailed {
                room: room_id.clone(),
                error,
            },
        );
    }

    let mut published_camera = None;
    if let Err(error) = reconcile_camera(
        &room,
        &mut published_camera,
        request.initial_update.camera_enabled,
        &sender,
        &room_id,
    )
    .await
    {
        send_client_event(
            &sender,
            SurfaceLiveKitClientEvent::CameraFailed {
                room: room_id.clone(),
                error,
            },
        );
    }

    // Active remote-video decode tasks, keyed by track sid. Each task owns a
    // `NativeVideoStream` and forwards decoded BGRA frames to the GPUI shell.
    let mut video_streams: HashMap<String, JoinHandle<()>> = HashMap::new();

    send_client_event(
        &sender,
        SurfaceLiveKitClientEvent::MetadataPublished {
            room: room_id.clone(),
        },
    );
    send_client_event(
        &sender,
        SurfaceLiveKitClientEvent::Joined {
            room: room_id.clone(),
            participant: participant_id,
        },
    );
    send_client_event(
        &sender,
        SurfaceLiveKitClientEvent::ConnectionState {
            room: room_id.clone(),
            state: connection_state_label(room.connection_state()).to_string(),
        },
    );
    publish_roster(&room, &sender, &room_id);

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(SurfaceLiveKitClientCommand::UpdateSurface(update)) => {
                        let update = match coalesce_surface_update(&mut commands, update) {
                            SurfaceLiveKitClientAction::UpdateSurface(update) => update,
                            SurfaceLiveKitClientAction::Disconnect => {
                                disconnect_surface_room(
                                    &room,
                                    &mut published_microphone,
                                    &sender,
                                    &room_id,
                                    "client disconnect",
                                )
                                .await;
                                return;
                            }
                            SurfaceLiveKitClientAction::Closed => {
                                disconnect_surface_room(
                                    &room,
                                    &mut published_microphone,
                                    &sender,
                                    &room_id,
                                    "control handle dropped",
                                )
                                .await;
                                return;
                            }
                        };
                        if let Err(error) = validate_surface_update(&update) {
                            send_client_event(
                                &sender,
                                SurfaceLiveKitClientEvent::SurfaceStateFailed {
                                    room: room_id.clone(),
                                    error,
                                },
                            );
                            continue;
                        }

                        if let Err(error) = publish_surface_update(&local_participant, &update).await {
                            send_client_event(
                                &sender,
                                SurfaceLiveKitClientEvent::SurfaceStateFailed {
                                    room: room_id.clone(),
                                    error,
                                },
                            );
                        } else {
                            send_client_event(
                                &sender,
                                SurfaceLiveKitClientEvent::SurfaceStatePublished {
                                    room: room_id.clone(),
                                },
                            );
                        }

                        if let Err(error) = reconcile_microphone(
                            &room,
                            &mut published_microphone,
                            update.mic_enabled,
                            &sender,
                            &room_id,
                        )
                        .await
                        {
                            send_client_event(
                                &sender,
                                SurfaceLiveKitClientEvent::MicrophoneFailed {
                                    room: room_id.clone(),
                                    error,
                                },
                            );
                        }

                        if let Err(error) = reconcile_camera(
                            &room,
                            &mut published_camera,
                            update.camera_enabled,
                            &sender,
                            &room_id,
                        )
                        .await
                        {
                            send_client_event(
                                &sender,
                                SurfaceLiveKitClientEvent::CameraFailed {
                                    room: room_id.clone(),
                                    error,
                                },
                            );
                        }
                    }
                    Some(SurfaceLiveKitClientCommand::Disconnect) => {
                        disconnect_surface_room(
                            &room,
                            &mut published_microphone,
                            &sender,
                            &room_id,
                            "client disconnect",
                        )
                        .await;
                        return;
                    }
                    None => {
                        disconnect_surface_room(
                            &room,
                            &mut published_microphone,
                            &sender,
                            &room_id,
                            "control handle dropped",
                        )
                        .await;
                        return;
                    }
                }
            }
            event = events.recv() => {
                let Some(event) = event else {
                    break;
                };
                reconcile_video_streams(&event, &mut video_streams, &sender, &room_id);
                if handle_room_event(event, &room, &sender, &room_id) {
                    break;
                }
            }
        }
    }

    for (_, handle) in video_streams.drain() {
        handle.abort();
    }
    let _ = reconcile_microphone(&room, &mut published_microphone, false, &sender, &room_id).await;
    let _ = reconcile_camera(&room, &mut published_camera, false, &sender, &room_id).await;
    let _ = room.close().await;
}

/// React to track subscription events by spawning/aborting per-track video
/// decode tasks. `TrackSubscribed` for a remote video track starts a
/// `NativeVideoStream` whose decoded BGRA frames stream to the GPUI shell;
/// `TrackUnsubscribed` aborts the matching task and tells the shell to drop the
/// tile.
fn reconcile_video_streams(
    event: &RoomEvent,
    video_streams: &mut HashMap<String, JoinHandle<()>>,
    sender: &Sender<SurfaceLiveKitClientEvent>,
    room_id: &str,
) {
    match event {
        RoomEvent::TrackSubscribed {
            track: RemoteTrack::Video(video_track),
            participant,
            ..
        } => {
            let track_sid = video_track.sid().to_string();
            if video_streams.contains_key(&track_sid) {
                return;
            }
            let participant_identity = participant.identity().to_string();
            let rtc_track = video_track.rtc_track();
            let frame_sender = sender.clone();
            let room = room_id.to_string();
            let identity = participant_identity.clone();
            let sid = track_sid.clone();

            // `NativeVideoStream` defaults to a one-frame queue, so a slow GPUI
            // main thread naturally drops stale frames (latest-wins) instead of
            // backing up. Each decoded frame is converted to BGRA off-thread.
            let handle = tokio::spawn(async move {
                let mut stream = NativeVideoStream::new(rtc_track);
                while let Some(frame) = stream.next().await {
                    if let Some(decoded) = decode_bgra(&identity, &sid, &frame) {
                        send_client_event(
                            &frame_sender,
                            SurfaceLiveKitClientEvent::RemoteVideoFrame {
                                room: room.clone(),
                                frame: decoded,
                            },
                        );
                    }
                }
            });
            video_streams.insert(track_sid.clone(), handle);
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::RemoteVideoSubscribed {
                    room: room_id.to_string(),
                    participant_identity,
                    track_sid,
                },
            );
        }
        RoomEvent::TrackUnsubscribed {
            track: RemoteTrack::Video(video_track),
            participant,
            ..
        } => {
            let track_sid = video_track.sid().to_string();
            if let Some(handle) = video_streams.remove(&track_sid) {
                handle.abort();
            }
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::RemoteVideoRemoved {
                    room: room_id.to_string(),
                    participant_identity: participant.identity().to_string(),
                    track_sid,
                },
            );
        }
        _ => {}
    }
}

async fn disconnect_surface_room(
    room: &Room,
    published_microphone: &mut Option<PublishedMicrophone>,
    sender: &Sender<SurfaceLiveKitClientEvent>,
    room_id: &str,
    reason: &str,
) {
    if let Err(error) =
        reconcile_microphone(room, published_microphone, false, sender, room_id).await
    {
        send_client_event(
            sender,
            SurfaceLiveKitClientEvent::MediaFailed {
                room: room_id.to_string(),
                error,
            },
        );
    }
    let _ = room.close().await;
    send_client_event(
        sender,
        SurfaceLiveKitClientEvent::Disconnected {
            room: room_id.to_string(),
            reason: reason.to_string(),
        },
    );
}

async fn publish_surface_update(
    local_participant: &LocalParticipant,
    update: &SurfaceLiveKitSurfaceUpdate,
) -> Result<(), String> {
    local_participant
        .set_metadata(update.room_metadata.clone())
        .await
        .map_err(|error| format!("failed to publish LiveKit metadata: {error}"))?;
    local_participant
        .set_attributes(HashMap::from_iter(update.participant_attributes.clone()))
        .await
        .map_err(|error| format!("failed to publish LiveKit attributes: {error}"))?;
    Ok(())
}

async fn reconcile_microphone(
    room: &Room,
    published_microphone: &mut Option<PublishedMicrophone>,
    mic_enabled: bool,
    sender: &Sender<SurfaceLiveKitClientEvent>,
    room_id: &str,
) -> Result<(), String> {
    match (mic_enabled, published_microphone.as_ref()) {
        (true, Some(_)) | (false, None) => Ok(()),
        (true, None) => {
            let audio = PlatformAudio::new()
                .map_err(|error| format!("failed to initialize platform audio: {error}"))?;
            if let Some(device) = audio.recording_devices().next() {
                audio
                    .set_recording_device(&device.id)
                    .map_err(|error| format!("failed to select microphone: {error}"))?;
            } else {
                return Err("no microphone devices available".to_string());
            }

            let track =
                LocalAudioTrack::create_audio_track("ocean-microphone", RtcAudioSource::Device);
            let publication = room
                .local_participant()
                .publish_track(
                    LocalTrack::Audio(track),
                    TrackPublishOptions {
                        source: TrackSource::Microphone,
                        ..TrackPublishOptions::default()
                    },
                )
                .await
                .map_err(|error| format!("failed to publish microphone: {error}"))?;
            let track_sid = publication.sid().to_string();
            *published_microphone = Some(PublishedMicrophone {
                publication,
                _audio: audio,
            });
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::MicrophonePublished {
                    room: room_id.to_string(),
                    track_sid,
                },
            );
            Ok(())
        }
        (false, Some(microphone)) => {
            let sid = microphone.publication.sid();
            room.local_participant()
                .unpublish_track(&sid)
                .await
                .map_err(|error| format!("failed to unpublish microphone: {error}"))?;
            *published_microphone = None;
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::MicrophoneUnpublished {
                    room: room_id.to_string(),
                },
            );
            Ok(())
        }
    }
}

/// Publish or unpublish the local camera track to match `camera_enabled`,
/// mirroring [`reconcile_microphone`].
///
/// ## Honest scope (OCEAN-97)
///
/// This wires the *publish path* end to end: it creates a real
/// `NativeVideoSource`, builds a `LocalVideoTrack` from it, and publishes it to
/// the room with `TrackSource::Camera` (so remote peers — including the web
/// surface — see the camera publication and the presence roster flips its `cam`
/// flag on).
///
/// What it does **not** do yet is *capture* real webcam frames. The livekit
/// 0.7 SDK provides `PlatformAudio` for microphone device enumeration/capture,
/// but there is **no equivalent platform camera capture** — frames must be fed
/// into `NativeVideoSource::capture_frame(...)` from an external capture library
/// (AVFoundation / `nokhwa` / a `CMSampleBuffer` bridge on macOS). Until that
/// capture source is added, the published track carries no frames, so remote
/// peers see the publication but a black/holding tile.
///
/// The held `NativeVideoSource` (`_source`) is the exact hook a future capture
/// loop will push frames into; everything downstream of it already works.
async fn reconcile_camera(
    room: &Room,
    published_camera: &mut Option<PublishedCamera>,
    camera_enabled: bool,
    sender: &Sender<SurfaceLiveKitClientEvent>,
    room_id: &str,
) -> Result<(), String> {
    match (camera_enabled, published_camera.as_ref()) {
        (true, Some(_)) | (false, None) => Ok(()),
        (true, None) => {
            let resolution = VideoResolution {
                width: 1280,
                height: 720,
            };
            let source = NativeVideoSource::new(resolution, false);
            let track = LocalVideoTrack::create_video_track(
                "ocean-camera",
                RtcVideoSource::Native(source.clone()),
            );
            let publication = room
                .local_participant()
                .publish_track(
                    LocalTrack::Video(track),
                    TrackPublishOptions {
                        source: TrackSource::Camera,
                        ..TrackPublishOptions::default()
                    },
                )
                .await
                .map_err(|error| format!("failed to publish camera: {error}"))?;
            let track_sid = publication.sid().to_string();
            *published_camera = Some(PublishedCamera {
                publication,
                _source: source,
            });
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::CameraPublished {
                    room: room_id.to_string(),
                    track_sid,
                },
            );
            Ok(())
        }
        (false, Some(camera)) => {
            let sid = camera.publication.sid();
            room.local_participant()
                .unpublish_track(&sid)
                .await
                .map_err(|error| format!("failed to unpublish camera: {error}"))?;
            *published_camera = None;
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::CameraUnpublished {
                    room: room_id.to_string(),
                },
            );
            Ok(())
        }
    }
}

fn handle_room_event(
    event: RoomEvent,
    room: &Room,
    sender: &Sender<SurfaceLiveKitClientEvent>,
    room_id: &str,
) -> bool {
    match event {
        RoomEvent::Connected { .. } | RoomEvent::Reconnected => {
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::ConnectionState {
                    room: room_id.to_string(),
                    state: "connected".to_string(),
                },
            );
            publish_roster(room, sender, room_id);
        }
        RoomEvent::Reconnecting => send_client_event(
            sender,
            SurfaceLiveKitClientEvent::ConnectionState {
                room: room_id.to_string(),
                state: "reconnecting".to_string(),
            },
        ),
        RoomEvent::ConnectionStateChanged(state) => send_client_event(
            sender,
            SurfaceLiveKitClientEvent::ConnectionState {
                room: room_id.to_string(),
                state: connection_state_label(state).to_string(),
            },
        ),
        // Presence + media changes that alter the roster snapshot. Each one
        // rebuilds the full roster from current room state so the native panel
        // mirrors the web surface's live participant list (OCEAN-83/94).
        RoomEvent::ParticipantConnected(_)
        | RoomEvent::ParticipantDisconnected(_)
        | RoomEvent::TrackSubscribed { .. }
        | RoomEvent::TrackUnsubscribed { .. }
        | RoomEvent::TrackPublished { .. }
        | RoomEvent::TrackUnpublished { .. }
        | RoomEvent::TrackMuted { .. }
        | RoomEvent::TrackUnmuted { .. }
        | RoomEvent::LocalTrackPublished { .. }
        | RoomEvent::LocalTrackUnpublished { .. }
        | RoomEvent::ActiveSpeakersChanged { .. } => {
            publish_roster(room, sender, room_id);
        }
        RoomEvent::Disconnected { reason } => {
            send_client_event(
                sender,
                SurfaceLiveKitClientEvent::Disconnected {
                    room: room_id.to_string(),
                    reason: format!("{reason:?}"),
                },
            );
            return true;
        }
        _ => {}
    }
    false
}

/// Build a roster snapshot from the room's current local + remote participants
/// and relay it to the GPUI shell. Mic/camera flags are derived from each
/// participant's track publications (source + mute state), matching the web
/// surface's presence semantics.
fn publish_roster(room: &Room, sender: &Sender<SurfaceLiveKitClientEvent>, room_id: &str) {
    send_client_event(
        sender,
        SurfaceLiveKitClientEvent::RosterUpdated {
            room: room_id.to_string(),
            participants: build_roster(room),
        },
    );
}

fn build_roster(room: &Room) -> Vec<SurfaceLiveKitParticipant> {
    let local = room.local_participant();
    let local_sources: Vec<(TrackSource, bool)> = local
        .track_publications()
        .values()
        .map(|publication| (publication.source(), publication.is_muted()))
        .collect();
    let mut participants = vec![SurfaceLiveKitParticipant {
        identity: local.identity().to_string(),
        name: non_empty_name(local.name(), local.identity().to_string()),
        local: true,
        mic: has_active_source(&local_sources, TrackSource::Microphone),
        camera: has_active_source(&local_sources, TrackSource::Camera),
        speaking: local.is_speaking(),
    }];

    for remote in room.remote_participants().values() {
        let remote_sources: Vec<(TrackSource, bool)> = remote
            .track_publications()
            .values()
            .map(|publication| (publication.source(), publication.is_muted()))
            .collect();
        participants.push(SurfaceLiveKitParticipant {
            identity: remote.identity().to_string(),
            name: non_empty_name(remote.name(), remote.identity().to_string()),
            local: false,
            mic: has_active_source(&remote_sources, TrackSource::Microphone),
            camera: has_active_source(&remote_sources, TrackSource::Camera),
            speaking: remote.is_speaking(),
        });
    }

    participants
}

/// A participant has an "active" source when it publishes an un-muted track of
/// that source kind (microphone for `mic`, camera for `camera`).
fn has_active_source(sources: &[(TrackSource, bool)], source: TrackSource) -> bool {
    sources
        .iter()
        .any(|(track_source, muted)| *track_source == source && !muted)
}

fn non_empty_name(name: String, fallback: String) -> String {
    if name.trim().is_empty() {
        fallback
    } else {
        name
    }
}

fn connection_state_label(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Disconnected => "disconnected",
        ConnectionState::Connected => "connected",
        ConnectionState::Reconnecting => "reconnecting",
    }
}

struct PublishedMicrophone {
    publication: LocalTrackPublication,
    _audio: PlatformAudio,
}

/// A published local camera track plus the `NativeVideoSource` backing it.
///
/// `_source` is held alive for the lifetime of the publication; it is the sink
/// a real camera-capture loop will push frames into (see [`reconcile_camera`]).
struct PublishedCamera {
    publication: LocalTrackPublication,
    _source: NativeVideoSource,
}
