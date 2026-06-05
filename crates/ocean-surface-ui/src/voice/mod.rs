//! Push-to-talk voice capture → STT.
//!
//! A circular "orb" button. Pointer-down starts recording via
//! `getUserMedia` + `MediaRecorder`; pointer-up stops, assembles the chunks
//! into a Blob, POSTs the raw bytes to `/api/stt`, and on `{ok, text}` hands
//! the transcript to a callback (which drops it into the composer + submits).
//!
//! The proxy is same-origin as the served bundle, so `/api/stt` is relative.
//!
//! Submodules add the first-class hands-free capabilities on top of the
//! push-to-talk orb: [`vad`] is the voice-activity-detection state machine
//! that auto-endpoints speech so continuous and wake-word modes don't need a
//! button hold.

pub mod listen;
pub mod mode;
pub mod vad;
pub mod wake;

use std::cell::RefCell;
use std::rc::Rc;

use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Blob, BlobEvent, MediaRecorder, MediaStream};

use crate::tts;

/// What the orb is doing right now. Drives styling + the hint label.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecState {
    Idle,
    Recording,
    Transcribing,
}

/// Live capture state for the hands-free modes (continuous / wake-word).
///
/// The STT backend (`/api/stt`) is a single-shot multipart request — it returns
/// one final transcript with no interim/partial hypotheses — so true streaming
/// token-by-token feedback isn't available. Instead we surface the capture
/// lifecycle visually: the user sees the orb move through
/// `Idle → Listening → Transcribing → (final text)` as they speak, so a
/// hands-free utterance never feels like it vanished into the void.
#[derive(Clone, PartialEq, Eq, Default)]
pub enum HandsFreeStatus {
    /// Ambient: mic open, VAD watching, no active utterance.
    #[default]
    Idle,
    /// VAD detected speech onset — actively recording the user's utterance.
    Listening,
    /// Utterance ended; audio is being transcribed by STT.
    Transcribing,
    /// Last utterance's final transcript (kept briefly for visual confirmation).
    Final(String),
}

#[derive(Deserialize)]
struct SttResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    error: Option<String>,
}

/// Live recorder handles we must hold onto for the lifetime of a capture.
/// MediaRecorder + the stream tracks need explicit stop; the `ondataavailable`
/// closure must outlive the recorder, so we stash it here too.
#[derive(Default)]
struct Recorder {
    recorder: Option<MediaRecorder>,
    stream: Option<MediaStream>,
    chunks: Rc<RefCell<Vec<Blob>>>,
    // Keep closures alive while the recorder is wired up.
    _on_data: Option<Closure<dyn FnMut(BlobEvent)>>,
    _on_stop: Option<Closure<dyn FnMut(web_sys::Event)>>,
}

/// Push-to-talk orb. `on_transcript` receives the recognized text.
#[component]
pub fn VoiceOrb(
    /// Called with the transcript once STT returns.
    on_transcript: Callback<String>,
    /// Surface STT / mic errors to the shell (reuses the status line).
    on_status: Callback<String>,
) -> impl IntoView {
    let state = RwSignal::new(RecState::Idle);
    // Shared recorder slot the start/stop handlers both reach into.
    let rec: Rc<RefCell<Recorder>> = Rc::new(RefCell::new(Recorder::default()));

    let start = {
        let rec = rec.clone();
        move || {
            if state.get_untracked() != RecState::Idle {
                return;
            }
            let rec = rec.clone();
            spawn_local(async move {
                match start_recording(rec, state).await {
                    Ok(()) => state.set(RecState::Recording),
                    Err(msg) => {
                        state.set(RecState::Idle);
                        on_status.run(msg);
                    }
                }
            });
        }
    };

    let stop = {
        let rec = rec.clone();
        move || {
            if state.get_untracked() != RecState::Recording {
                return;
            }
            state.set(RecState::Transcribing);
            stop_recording(&rec);
        }
    };

    // Pointer handlers: press-and-hold. pointerleave/cancel also stop so a
    // drag-off doesn't leave the mic hot.
    let on_down = {
        let start = start.clone();
        move |_| {
            // Prime the persistent TTS audio element from a user gesture so
            // mobile Safari trusts subsequent async .play() calls.
            tts::prime();
            start()
        }
    };
    let on_up = {
        let stop = stop.clone();
        move |_| stop()
    };

    // Current interaction mode, cycled by the mode switcher.
    let voice_mode = RwSignal::new(mode::VoiceMode::default());
    // Handle to the running continuous-listen loop (hands-free modes only).
    let listen_handle: Rc<RefCell<Option<Rc<RefCell<listen::ListenLoop>>>>> =
        Rc::new(RefCell::new(None));

    // React to mode changes: tear down the previous mode's listener/router and
    // stand up the new one. Push-to-talk owns no loop; hands-free modes start
    // the continuous listener and install the matching router.
    {
        let listen_handle = listen_handle.clone();
        Effect::new(move |_| {
            let m = voice_mode.get();
            // Stop any running loop from the previous mode.
            if let Some(h) = listen_handle.borrow_mut().take() {
                listen::stop(&h);
            }
            if m.is_hands_free() {
                set_hands_free(Some(mode::HandsFreeState::new(m)));
                let listen_handle = listen_handle.clone();
                spawn_local(async move {
                    match listen::start().await {
                        Ok(h) => *listen_handle.borrow_mut() = Some(h),
                        Err(msg) => {
                            set_hands_free(None);
                            report_status(msg);
                        }
                    }
                });
            } else {
                set_hands_free(None);
            }
        });
    }

    // Cycle PushToTalk → Continuous → WakeWord → … . Prime TTS from the gesture
    // so hands-free TTS replies are allowed on mobile.
    let cycle_mode = move |_| {
        tts::prime();
        voice_mode.update(|m| *m = m.next());
    };

    // Keyboard shortcut: Cmd/Ctrl+Shift+V cycles voice mode from anywhere. The
    // chord avoids clobbering normal typing. Registered once on mount; the
    // listener is leaked intentionally (lives for the app's lifetime).
    {
        if let Some(window) = web_sys::window() {
            let on_key = Closure::wrap(Box::new(move |ev: web_sys::KeyboardEvent| {
                let key = ev.key().to_lowercase();
                if (ev.meta_key() || ev.ctrl_key()) && ev.shift_key() && key == "v" {
                    ev.prevent_default();
                    voice_mode.update(|m| *m = m.next());
                }
            }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
            let _ =
                window.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
            on_key.forget();
        }
    }

    // Reactive hands-free capture status: the listen loop publishes onto this so
    // the orb can show "listening… → transcribing… → final text" per utterance.
    let hf_status = hands_free_status_signal();

    let label = move || {
        let m = voice_mode.get();
        if m.is_hands_free() {
            // Layer the live capture status over the mode's resting label so the
            // user gets per-utterance feedback in the hands-free modes.
            return match hf_status.get() {
                HandsFreeStatus::Idle => m.label().to_string(),
                HandsFreeStatus::Listening => "listening…".to_string(),
                HandsFreeStatus::Transcribing => "transcribing…".to_string(),
                HandsFreeStatus::Final(text) => {
                    // Show a trimmed confirmation of what was heard.
                    let t = text.trim();
                    if t.chars().count() > 28 {
                        let short: String = t.chars().take(27).collect();
                        format!("\u{201c}{short}\u{2026}\u{201d}")
                    } else {
                        format!("\u{201c}{t}\u{201d}")
                    }
                }
            };
        }
        match state.get() {
            RecState::Idle => "hold to talk".to_string(),
            RecState::Recording => "listening… release to send".to_string(),
            RecState::Transcribing => "transcribing…".to_string(),
        }
    };
    let orb_class = move || {
        let m = voice_mode.get();
        let base = format!("voice-orb {}", m.css_modifier());
        if m.is_hands_free() {
            // Hands-free: orb pulses to show it's live-listening, and brightens
            // through the capture lifecycle so the visual matches the hint.
            match hf_status.get() {
                HandsFreeStatus::Listening => format!("{base} is-live is-capturing"),
                HandsFreeStatus::Transcribing => format!("{base} is-live is-transcribing"),
                _ => format!("{base} is-live"),
            }
        } else {
            match state.get() {
                RecState::Idle => base,
                RecState::Recording => format!("{base} is-recording"),
                RecState::Transcribing => format!("{base} is-transcribing"),
            }
        }
    };
    // Pointer handlers only fire push-to-talk; inert in hands-free modes.
    let ptt_down = {
        let on_down = on_down.clone();
        move |ev: web_sys::PointerEvent| {
            if !voice_mode.get_untracked().is_hands_free() {
                on_down(ev);
            }
        }
    };
    let ptt_up = {
        let on_up = on_up.clone();
        move |ev: web_sys::PointerEvent| {
            if !voice_mode.get_untracked().is_hands_free() {
                on_up(ev);
            }
        }
    };
    let ptt_up2 = ptt_up.clone();
    let ptt_up3 = ptt_up.clone();
    let mode_title = move || match voice_mode.get() {
        mode::VoiceMode::PushToTalk => "push-to-talk — tap to switch to continuous",
        mode::VoiceMode::Continuous => "continuous listening — tap to switch to wake-word",
        mode::VoiceMode::WakeWord => "wake word ‘hey Ocean’ — tap to switch to push-to-talk",
    };

    // Bridge: stash the transcript callback where start_recording can grab it.
    // We use a thread-local-free approach: pass it into start via a Cell.
    provide_voice_callback(on_transcript, on_status);

    view! {
        <div class="voice-wrap">
            <button
                class=orb_class
                type="button"
                aria-label="voice input"
                on:pointerdown=ptt_down
                on:pointerup=ptt_up
                on:pointerleave=ptt_up2
                on:pointercancel=ptt_up3
            >
                <span class="voice-orb__glyph">{view! { <crate::icons::Amplitude /> }}</span>
            </button>
            <button
                class="voice-mode-switch"
                type="button"
                title=mode_title
                aria-label="switch voice mode"
                on:click=cycle_mode
            >
                {move || match voice_mode.get() {
                    mode::VoiceMode::PushToTalk => "PTT",
                    mode::VoiceMode::Continuous => "LIVE",
                    mode::VoiceMode::WakeWord => "WAKE",
                }}
            </button>
            <span class="voice-hint">{label}</span>
        </div>
    }
}

// The transcript/status callbacks are component-scoped Copy handles; we keep
// them in a leptos signal that the async upload reads when a capture finishes.
thread_local! {
    static VOICE_CB: RefCell<Option<(Callback<String>, Callback<String>)>> =
        const { RefCell::new(None) };
}

fn provide_voice_callback(on_transcript: Callback<String>, on_status: Callback<String>) {
    VOICE_CB.with(|c| *c.borrow_mut() = Some((on_transcript, on_status)));
}

// In hands-free modes the active router decides whether/what to submit. When
// set, transcripts pass through it; when None (push-to-talk), they go straight
// to the shell callback as before.
thread_local! {
    static HANDS_FREE: RefCell<Option<mode::HandsFreeState>> = const { RefCell::new(None) };
}

// Live hands-free capture status, surfaced to the orb so the user gets visual
// "listening… → transcribing… → final" feedback as they speak. Lazily created
// the first time the orb reads it so the listen loop and STT path can publish
// updates regardless of mount order.
thread_local! {
    static HANDS_FREE_STATUS: RefCell<Option<RwSignal<HandsFreeStatus>>> =
        const { RefCell::new(None) };
}

/// The reactive signal the orb binds its hands-free hint/animation to. Created
/// on first access so both the component and the listen loop share one signal.
fn hands_free_status_signal() -> RwSignal<HandsFreeStatus> {
    HANDS_FREE_STATUS.with(|s| {
        *s.borrow_mut().get_or_insert_with(|| RwSignal::new(HandsFreeStatus::Idle))
    })
}

/// Publish a new hands-free capture status (no-op if the signal isn't live yet).
/// Called from the listen loop on VAD transitions and from the STT path on a
/// final transcript.
pub(super) fn set_hands_free_status(status: HandsFreeStatus) {
    let sig = HANDS_FREE_STATUS.with(|s| s.borrow().as_ref().copied());
    if let Some(sig) = sig {
        // A `Final` confirmation is transient: show what was heard for a beat,
        // then fall back to the mode's ambient label — but only if nothing newer
        // (a fresh utterance) has superseded it in the meantime.
        let is_final = matches!(status, HandsFreeStatus::Final(_));
        sig.set(status.clone());
        if is_final {
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(2_500).await;
                if sig.with_untracked(|cur| *cur == status) {
                    sig.set(HandsFreeStatus::Idle);
                }
            });
        }
    }
}

/// Install (or clear) the hands-free router for the current mode.
fn set_hands_free(router: Option<mode::HandsFreeState>) {
    HANDS_FREE.with(|h| *h.borrow_mut() = router);
}

fn deliver_transcript(text: String) {
    // Route through the hands-free state machine if one is active.
    let routed = HANDS_FREE.with(|h| {
        h.borrow_mut()
            .as_mut()
            .map(|router| router.on_utterance(&text))
    });
    // In hands-free modes, surface what STT actually heard so the user gets a
    // final-text confirmation (closes the listening→transcribing→final loop).
    // Ignored chatter clears back to Idle so the orb doesn't keep showing stale
    // text. Push-to-talk (routed == None) has its own RecState-driven hint.
    if routed.is_some() {
        let confirm = match &routed {
            Some(mode::HandsFreeAction::Submit(cmd)) => HandsFreeStatus::Final(cmd.clone()),
            // Bare wake-word arming or pre-wake chatter: nothing to confirm,
            // drop back to the ambient listening state.
            _ => HandsFreeStatus::Idle,
        };
        set_hands_free_status(confirm);
    }
    let to_submit = match routed {
        // Push-to-talk: no router, submit the raw transcript.
        None => Some(text),
        Some(mode::HandsFreeAction::Submit(cmd)) => Some(cmd),
        // Ignored (chatter before a wake word, or a bare-wake arming turn).
        Some(mode::HandsFreeAction::Ignore) => None,
    };
    if let Some(text) = to_submit {
        VOICE_CB.with(|c| {
            if let Some((cb, _)) = c.borrow().as_ref() {
                cb.run(text);
            }
        });
    }
}

fn report_status(msg: String) {
    VOICE_CB.with(|c| {
        if let Some((_, cb)) = c.borrow().as_ref() {
            cb.run(msg);
        }
    });
}

/// Acquire the mic, build a MediaRecorder, wire data/stop handlers, start it.
async fn start_recording(
    rec: Rc<RefCell<Recorder>>,
    state: RwSignal<RecState>,
) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let navigator = window.navigator();
    let media = navigator
        .media_devices()
        .map_err(|_| "this browser cannot record audio".to_string())?;

    let constraints = web_sys::MediaStreamConstraints::new();
    constraints.set_audio(&JsValue::TRUE);
    let promise = media
        .get_user_media_with_constraints(&constraints)
        .map_err(|_| "microphone unavailable".to_string())?;
    let stream: MediaStream = JsFuture::from(promise)
        .await
        .map_err(|_| "microphone permission denied".to_string())?
        .dyn_into()
        .map_err(|_| "no media stream".to_string())?;

    let recorder = MediaRecorder::new_with_media_stream(&stream)
        .map_err(|_| "cannot create recorder".to_string())?;

    let chunks: Rc<RefCell<Vec<Blob>>> = Rc::new(RefCell::new(Vec::new()));

    // ondataavailable: collect each chunk.
    let on_data = {
        let chunks = chunks.clone();
        Closure::wrap(Box::new(move |ev: BlobEvent| {
            if let Some(blob) = ev.data() {
                if blob.size() > 0.0 {
                    chunks.borrow_mut().push(blob);
                }
            }
        }) as Box<dyn FnMut(BlobEvent)>)
    };
    recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));

    // onstop: assemble blob, upload to /api/stt.
    let mime = recorder.mime_type();
    let on_stop = {
        let chunks = chunks.clone();
        Closure::wrap(Box::new(move |_ev: web_sys::Event| {
            let parts = chunks.borrow();
            let blob = assemble_blob(&parts, &mime);
            drop(parts);
            match blob {
                Some(blob) if blob.size() >= 800.0 => {
                    state.set(RecState::Transcribing);
                    // upload_blob resets state → Idle when it finishes (success
                    // or error), so the orb never gets stuck on "transcribing…".
                    spawn_local(upload_blob(blob, state));
                }
                _ => {
                    state.set(RecState::Idle);
                    report_status("recording too short — try again".into());
                }
            }
        }) as Box<dyn FnMut(web_sys::Event)>)
    };
    recorder.set_onstop(Some(on_stop.as_ref().unchecked_ref()));

    recorder
        .start()
        .map_err(|_| "recorder failed to start".to_string())?;

    let mut slot = rec.borrow_mut();
    slot.recorder = Some(recorder);
    slot.stream = Some(stream);
    slot.chunks = chunks;
    slot._on_data = Some(on_data);
    slot._on_stop = Some(on_stop);
    Ok(())
}

/// Stop the recorder (triggers onstop) and release the mic tracks.
fn stop_recording(rec: &Rc<RefCell<Recorder>>) {
    let mut slot = rec.borrow_mut();
    if let Some(recorder) = slot.recorder.take() {
        let _ = recorder.stop();
    }
    if let Some(stream) = slot.stream.take() {
        let tracks = stream.get_tracks();
        for i in 0..tracks.length() {
            if let Ok(track) = tracks.get(i).dyn_into::<web_sys::MediaStreamTrack>() {
                track.stop();
            }
        }
    }
    // Keep _on_stop alive: it fires asynchronously after stop(). Closures are
    // dropped when the Recorder slot is reset on the next start.
}

/// Concatenate the recorded chunks into a single typed Blob.
fn assemble_blob(parts: &[Blob], mime: &str) -> Option<Blob> {
    if parts.is_empty() {
        return None;
    }
    let array = js_sys::Array::new();
    for p in parts {
        array.push(p);
    }
    let bag = web_sys::BlobPropertyBag::new();
    if !mime.is_empty() {
        bag.set_type(mime);
    }
    Blob::new_with_blob_sequence_and_options(&array, &bag).ok()
}

/// POST the audio bytes to /api/stt and deliver the transcript. Always
/// returns the orb to Idle when finished, whatever the outcome.
async fn upload_blob(blob: Blob, state: RwSignal<RecState>) {
    // Read the Blob into an ArrayBuffer → Vec<u8> for the request body.
    let bytes = match blob_to_bytes(&blob).await {
        Ok(b) => b,
        Err(msg) => {
            report_status(msg);
            state.set(RecState::Idle);
            return;
        }
    };
    let mime = blob.type_();
    let content_type = if mime.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime
    };

    let req = Request::post("/api/stt")
        .header("content-type", &content_type)
        .body(bytes);
    let resp = match req {
        Ok(r) => r.send().await,
        Err(err) => {
            report_status(format!("stt encode error: {err}"));
            state.set(RecState::Idle);
            return;
        }
    };
    match resp {
        Ok(r) => match r.json::<SttResponse>().await {
            Ok(s) if s.ok && !s.text.trim().is_empty() => {
                deliver_transcript(s.text.trim().to_string());
            }
            Ok(s) => {
                report_status(s.error.unwrap_or_else(|| "no transcript heard".into()));
            }
            Err(err) => report_status(format!("stt decode error: {err}")),
        },
        Err(err) => report_status(format!("stt request failed: {err}")),
    }
    state.set(RecState::Idle);
}

// ---------------------------------------------------------------------------
// Segment recorder — used by the continuous/wake-word listen loop.
//
// Unlike `Recorder` (push-to-talk), a segment recorder records a single
// VAD-bounded utterance over a mic stream it does NOT own (the listen loop owns
// the stream and AudioContext). On stop it assembles the chunks and uploads to
// STT; the resulting transcript flows through `deliver_transcript`, which routes
// it through the active hands-free state machine.
// ---------------------------------------------------------------------------

/// One in-flight VAD-bounded recording segment.
#[derive(Default)]
pub(super) struct SegmentRecorder {
    recorder: Option<MediaRecorder>,
    chunks: Rc<RefCell<Vec<Blob>>>,
    _on_data: Option<Closure<dyn FnMut(BlobEvent)>>,
    _on_stop: Option<Closure<dyn FnMut(web_sys::Event)>>,
}

/// Begin recording a segment from `stream`. No-op if one is already recording
/// (a stray SpeechStart shouldn't stack recorders).
pub(super) fn segment_recorder_start(seg: &Rc<RefCell<SegmentRecorder>>, stream: &MediaStream) {
    if seg.borrow().recorder.is_some() {
        return;
    }
    let recorder = match MediaRecorder::new_with_media_stream(stream) {
        Ok(r) => r,
        Err(_) => {
            report_status("cannot create recorder".into());
            return;
        }
    };
    let chunks: Rc<RefCell<Vec<Blob>>> = Rc::new(RefCell::new(Vec::new()));

    let on_data = {
        let chunks = chunks.clone();
        Closure::wrap(Box::new(move |ev: BlobEvent| {
            if let Some(blob) = ev.data() {
                if blob.size() > 0.0 {
                    chunks.borrow_mut().push(blob);
                }
            }
        }) as Box<dyn FnMut(BlobEvent)>)
    };
    recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));

    // onstop: assemble + upload. Guarded by a flag so a "discard" stop skips it.
    let mime = recorder.mime_type();
    let on_stop = {
        let chunks = chunks.clone();
        Closure::wrap(Box::new(move |_ev: web_sys::Event| {
            let parts = chunks.borrow();
            let blob = assemble_blob(&parts, &mime);
            drop(parts);
            if let Some(blob) = blob {
                if blob.size() >= 800.0 {
                    spawn_local(upload_segment(blob));
                }
            }
        }) as Box<dyn FnMut(web_sys::Event)>)
    };
    recorder.set_onstop(Some(on_stop.as_ref().unchecked_ref()));

    if recorder.start().is_err() {
        report_status("recorder failed to start".into());
        return;
    }

    let mut slot = seg.borrow_mut();
    slot.recorder = Some(recorder);
    slot.chunks = chunks;
    slot._on_data = Some(on_data);
    slot._on_stop = Some(on_stop);
}

/// Stop the segment and let its `onstop` upload to STT.
pub(super) fn segment_recorder_stop(seg: &Rc<RefCell<SegmentRecorder>>) {
    let mut slot = seg.borrow_mut();
    if let Some(recorder) = slot.recorder.take() {
        let _ = recorder.stop();
    }
}

/// Stop the segment and throw it away without an STT round-trip (too short, or
/// teardown). We clear the chunks first so the onstop assembles nothing.
pub(super) fn segment_recorder_stop_discard(seg: &Rc<RefCell<SegmentRecorder>>) {
    let mut slot = seg.borrow_mut();
    slot.chunks.borrow_mut().clear();
    if let Some(recorder) = slot.recorder.take() {
        let _ = recorder.stop();
    }
}

/// Upload one segment's audio to /api/stt and deliver the transcript through the
/// hands-free router. Mirrors `upload_blob` but without the RecState machine.
async fn upload_segment(blob: Blob) {
    let bytes = match blob_to_bytes(&blob).await {
        Ok(b) => b,
        Err(msg) => {
            report_status(msg);
            return;
        }
    };
    let mime = blob.type_();
    let content_type = if mime.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime
    };
    let req = Request::post("/api/stt")
        .header("content-type", &content_type)
        .body(bytes);
    let resp = match req {
        Ok(r) => r.send().await,
        Err(err) => {
            report_status(format!("stt encode error: {err}"));
            return;
        }
    };
    if let Ok(r) = resp {
        if let Ok(s) = r.json::<SttResponse>().await {
            if s.ok && !s.text.trim().is_empty() {
                deliver_transcript(s.text.trim().to_string());
            }
        }
    }
}

/// Resolve a Blob to its raw bytes via the ArrayBuffer promise.
async fn blob_to_bytes(blob: &Blob) -> Result<Vec<u8>, String> {
    let buf = JsFuture::from(blob.array_buffer())
        .await
        .map_err(|_| "failed to read audio".to_string())?;
    let array = js_sys::Uint8Array::new(&buf);
    Ok(array.to_vec())
}
