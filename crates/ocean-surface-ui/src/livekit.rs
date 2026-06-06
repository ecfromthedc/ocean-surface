//! Web LiveKit participation (OCEAN-83).
//!
//! The credential/token flow already worked end-to-end: the proxy serves a
//! `livekit_token_path` in `/api/config`, and `POST`ing to it reverse-proxies
//! to the daemon, which mints a room JWT. What was missing was *participation* —
//! the web surface had the room id + token path but no way to actually join,
//! leave, toggle mic/camera, or see who else is in the room.
//!
//! This module adds that, the same way the map/video components work: thin
//! `extern "C"` bindings into a JS bridge defined in `index.html`
//! (`window.oceanLiveKit*`), which drives the official `livekit-client` web
//! SDK. The Rust side owns the UI state (join state, mic/camera, roster) as
//! Leptos signals; the JS side owns the actual `Room` connection and relays
//! participant changes back through a callback.
//!
//! Deliberately scoped to the JS SDK + this surface. Nothing here touches the
//! daemon's Rust LiveKit SDK (that lives in the native `ocean-gui` crate) or
//! cross-surface presence — those remain follow-ups.

use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::daemon::Daemon;

#[wasm_bindgen]
extern "C" {
    /// Mint a token via `token_path` (the working token flow) and connect to the
    /// room with the `livekit-client` SDK. `participants_cb` is invoked with a
    /// JSON array of the roster whenever participants/tracks change. Resolves to
    /// a JSON status string: `{ ok, room?, error? }`.
    #[wasm_bindgen(js_name = oceanLiveKitConnect, catch)]
    async fn ocean_livekit_connect(
        token_path: &str,
        surface_id: &str,
        participant_id: &str,
        display_name: &str,
        participants_cb: &JsValue,
    ) -> Result<JsValue, JsValue>;

    /// Disconnect from the room (stops local tracks). Resolves `{ ok }`.
    #[wasm_bindgen(js_name = oceanLiveKitDisconnect, catch)]
    async fn ocean_livekit_disconnect() -> Result<JsValue, JsValue>;

    /// Enable/disable the local microphone track. Resolves `{ ok, mic }`.
    #[wasm_bindgen(js_name = oceanLiveKitSetMic, catch)]
    async fn ocean_livekit_set_mic(enabled: bool) -> Result<JsValue, JsValue>;

    /// Enable/disable the local camera track. Resolves `{ ok, camera }`.
    #[wasm_bindgen(js_name = oceanLiveKitSetCamera, catch)]
    async fn ocean_livekit_set_camera(enabled: bool) -> Result<JsValue, JsValue>;

    /// Attach any already-subscribed remote video for `identity` into its tile
    /// container (`lk-tile-<identity>`). Called when a tile mounts so a track
    /// that subscribed *before* the tile rendered still gets its `<video>`.
    #[wasm_bindgen(js_name = oceanLiveKitAttachTile)]
    fn ocean_livekit_attach_tile(identity: &str);
}

/// One row in the participant roster, as relayed from the JS bridge.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Participant {
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
    /// Whether this participant currently has a subscribed remote video track
    /// (drives whether the grid draws a live `<video>` tile vs. an avatar).
    #[serde(default)]
    pub has_video: bool,
}

/// A roster callback payload from the JS bridge. Normally an array of
/// participants, but the reconnect path sends `{ "status": "reconnecting" }`
/// so the panel can show an indicator without nuking the roster type.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RosterMsg {
    Status { status: String },
    Roster(Vec<Participant>),
}

/// Where we are in the join lifecycle. Drives the button label + disabled state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JoinState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

/// Pull `{ ok, error? }` out of a status JSON string returned by the bridge.
fn status_ok(js: &JsValue) -> (bool, Option<String>) {
    let s = js.as_string().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let err = v
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    (ok, err)
}

/// The collaboration presence panel: Join/Leave, mic + camera toggles, and a
/// live participant roster. Renders nothing until the config bootstrap has
/// supplied a `livekit_token_path` (i.e. a room is configured for this surface).
#[component]
pub fn LiveKitPanel(daemon: Daemon) -> impl IntoView {
    let token_path = daemon.livekit_token_path;
    let room_id = daemon.livekit_room_id;

    let join_state = RwSignal::new(JoinState::Disconnected);
    let mic_on = RwSignal::new(false);
    let camera_on = RwSignal::new(false);
    let participants = RwSignal::new(Vec::<Participant>::new());
    let error = RwSignal::new(Option::<String>::None);
    // True while the SDK is mid-reconnect after a network drop. Drives the
    // "reconnecting…" indicator; cleared when the next roster snapshot lands.
    let reconnecting = RwSignal::new(false);

    // Callback the JS bridge invokes with the roster JSON on every
    // participant/track change. Leaked (`into_js_value`) so it stays callable
    // for the life of the panel — one panel per surface, so a single leak.
    // Stored (Copy handle) so the join closure stays `Fn` — `JsValue` itself is
    // not `Copy`, and the event handlers below are used in reactive contexts
    // that require `Fn`.
    let roster_cb: StoredValue<JsValue> = StoredValue::new(
        Closure::<dyn FnMut(String)>::new(move |json: String| {
            match serde_json::from_str::<RosterMsg>(&json) {
                Ok(RosterMsg::Status { status }) => {
                    // The only status the bridge sends today is "reconnecting".
                    if status == "reconnecting" {
                        reconnecting.set(true);
                    }
                }
                Ok(RosterMsg::Roster(list)) => {
                    // A fresh roster means the connection is live again.
                    reconnecting.set(false);
                    // Keep the local mic/camera toggle state in sync with what
                    // the SDK actually reports (e.g. a track that failed to
                    // publish).
                    if let Some(me) = list.iter().find(|p| p.local) {
                        mic_on.set(me.mic);
                        camera_on.set(me.camera);
                    }
                    participants.set(list);
                }
                Err(_) => {
                    reconnecting.set(false);
                    participants.set(Vec::new());
                }
            }
        })
        .into_js_value(),
    );

    let join = move |_| {
        if join_state.get() != JoinState::Disconnected {
            return;
        }
        let path = token_path.get();
        if path.trim().is_empty() {
            error.set(Some("no LiveKit room configured".into()));
            return;
        }
        error.set(None);
        join_state.set(JoinState::Connecting);
        let cb = roster_cb.get_value();
        spawn_local(async move {
            match ocean_livekit_connect(&path, "web-surface", "web-surface", "Web Surface", &cb)
                .await
            {
                Ok(status) => {
                    let (ok, err) = status_ok(&status);
                    if ok {
                        join_state.set(JoinState::Connected);
                    } else {
                        join_state.set(JoinState::Disconnected);
                        error.set(err.or_else(|| Some("failed to join".into())));
                    }
                }
                Err(_) => {
                    join_state.set(JoinState::Disconnected);
                    error.set(Some("LiveKit SDK failed to load".into()));
                }
            }
        });
    };

    let leave = move |_| {
        spawn_local(async move {
            let _ = ocean_livekit_disconnect().await;
            join_state.set(JoinState::Disconnected);
            mic_on.set(false);
            camera_on.set(false);
            reconnecting.set(false);
            participants.set(Vec::new());
        });
    };

    let toggle_mic = move |_| {
        if join_state.get() != JoinState::Connected {
            return;
        }
        let next = !mic_on.get();
        // Optimistic; the roster callback reconciles to the SDK's real state.
        mic_on.set(next);
        spawn_local(async move {
            let _ = ocean_livekit_set_mic(next).await;
        });
    };

    let toggle_camera = move |_| {
        if join_state.get() != JoinState::Connected {
            return;
        }
        let next = !camera_on.get();
        camera_on.set(next);
        spawn_local(async move {
            let _ = ocean_livekit_set_camera(next).await;
        });
    };

    view! {
        // Only show the panel when a room is configured for this surface.
        <Show when=move || !token_path.get().trim().is_empty()>
            <div class="ocean-livekit">
                <div class="ocean-livekit__bar">
                    <span class="ocean-livekit__room" title="LiveKit room">
                        {move || {
                            let r = room_id.get();
                            if r.is_empty() { "room".to_string() } else { r }
                        }}
                    </span>

                    <Show
                        when=move || join_state.get() == JoinState::Connected
                        fallback=move || {
                            view! {
                                <button
                                    class="ocean-livekit__btn ocean-livekit__btn--join"
                                    type="button"
                                    on:click=join
                                    disabled=move || join_state.get() == JoinState::Connecting
                                >
                                    {move || match join_state.get() {
                                        JoinState::Connecting => "joining…",
                                        _ => "join call",
                                    }}
                                </button>
                            }
                        }
                    >
                        <button
                            class=move || {
                                let on = mic_on.get();
                                format!(
                                    "ocean-livekit__btn ocean-livekit__btn--toggle {}",
                                    if on { "is-on" } else { "is-off" },
                                )
                            }
                            type="button"
                            on:click=toggle_mic
                            title="toggle microphone"
                        >
                            {move || if mic_on.get() { "mic on" } else { "mic off" }}
                        </button>
                        <button
                            class=move || {
                                let on = camera_on.get();
                                format!(
                                    "ocean-livekit__btn ocean-livekit__btn--toggle {}",
                                    if on { "is-on" } else { "is-off" },
                                )
                            }
                            type="button"
                            on:click=toggle_camera
                            title="toggle camera"
                        >
                            {move || if camera_on.get() { "cam on" } else { "cam off" }}
                        </button>
                        <button
                            class="ocean-livekit__btn ocean-livekit__btn--leave"
                            type="button"
                            on:click=leave
                        >
                            "leave"
                        </button>
                    </Show>
                </div>

                <Show when=move || error.get().is_some()>
                    <div class="ocean-livekit__error">
                        {move || error.get().unwrap_or_default()}
                    </div>
                </Show>

                <Show when=move || reconnecting.get()>
                    <div class="ocean-livekit__reconnecting">
                        <span class="ocean-livekit__reconnecting-dot"></span>
                        "reconnecting…"
                    </div>
                </Show>

                // Remote video grid: one tile per remote participant that has a
                // subscribed video track. The <video> itself is mounted by the
                // JS bridge into `lk-tile-<identity>`; this renders the
                // container + label and pokes the bridge on mount so a track
                // that subscribed before the tile rendered still attaches.
                <Show
                    when=move || {
                        join_state.get() == JoinState::Connected
                            && participants.get().iter().any(|p| !p.local && p.has_video)
                    }
                >
                    <div class="ocean-livekit__grid">
                        <For
                            each=move || {
                                participants.get().into_iter().filter(|p| !p.local && p.has_video).collect::<Vec<_>>()
                            }
                            key=|p| p.identity.clone()
                            children=move |p| {
                                let tile_id = format!("lk-tile-{}", p.identity);
                                let attach_id = p.identity.clone();
                                let node_ref = NodeRef::<leptos::html::Div>::new();
                                // Once the tile div is in the DOM, ask the bridge
                                // to (re)attach any already-subscribed video.
                                node_ref.on_load(move |_| {
                                    ocean_livekit_attach_tile(&attach_id);
                                });
                                let label = if p.name.is_empty() {
                                    p.identity.clone()
                                } else {
                                    p.name.clone()
                                };
                                let speaking = p.speaking;
                                view! {
                                    <div
                                        node_ref=node_ref
                                        id=tile_id
                                        class=move || {
                                            format!(
                                                "ocean-livekit__tile {}",
                                                if speaking { "is-speaking" } else { "" },
                                            )
                                        }
                                    >
                                        <span class="ocean-livekit__tile-label">{label}</span>
                                    </div>
                                }
                            }
                        />
                    </div>
                </Show>

                <Show when=move || join_state.get() == JoinState::Connected>
                    <ul class="ocean-livekit__roster">
                        <For
                            each=move || participants.get()
                            key=|p| p.identity.clone()
                            children=move |p| {
                                let label = if p.name.is_empty() {
                                    p.identity.clone()
                                } else {
                                    p.name.clone()
                                };
                                let label = if p.local {
                                    format!("{label} (you)")
                                } else {
                                    label
                                };
                                let speaking = p.speaking;
                                view! {
                                    <li
                                        class=move || {
                                            format!(
                                                "ocean-livekit__participant {}",
                                                if speaking { "is-speaking" } else { "" },
                                            )
                                        }
                                    >
                                        <span class="ocean-livekit__participant-name">{label}</span>
                                        <span class="ocean-livekit__participant-state">
                                            {if p.mic { "🎤" } else { "🔇" }}
                                            {if p.camera { " 📹" } else { "" }}
                                        </span>
                                    </li>
                                }
                            }
                        />
                    </ul>
                </Show>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_roster_array_with_video_flag() {
        let json = r#"[
            {"identity":"me","name":"Me","local":true,"mic":true,"camera":false,"speaking":false,"has_video":false},
            {"identity":"alice","name":"Alice","local":false,"mic":true,"camera":true,"speaking":true,"has_video":true}
        ]"#;
        match serde_json::from_str::<RosterMsg>(json).unwrap() {
            RosterMsg::Roster(list) => {
                assert_eq!(list.len(), 2);
                let alice = list.iter().find(|p| p.identity == "alice").unwrap();
                assert!(alice.has_video);
                assert!(alice.speaking);
                assert!(!alice.local);
                let me = list.iter().find(|p| p.local).unwrap();
                assert!(!me.has_video);
            }
            other => panic!("expected roster, got {other:?}"),
        }
    }

    #[test]
    fn missing_has_video_defaults_false() {
        // OCEAN-83-shaped payloads (no has_video) must still parse.
        let json = r#"[{"identity":"bob","local":false,"mic":false,"camera":false}]"#;
        match serde_json::from_str::<RosterMsg>(json).unwrap() {
            RosterMsg::Roster(list) => assert!(!list[0].has_video),
            other => panic!("expected roster, got {other:?}"),
        }
    }

    #[test]
    fn parses_reconnecting_status() {
        let json = r#"{"status":"reconnecting"}"#;
        match serde_json::from_str::<RosterMsg>(json).unwrap() {
            RosterMsg::Status { status } => assert_eq!(status, "reconnecting"),
            other => panic!("expected status, got {other:?}"),
        }
    }
}
