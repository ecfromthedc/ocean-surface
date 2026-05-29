//! Push-to-talk voice capture → STT.
//!
//! A circular "orb" button. Pointer-down starts recording via
//! `getUserMedia` + `MediaRecorder`; pointer-up stops, assembles the chunks
//! into a Blob, POSTs the raw bytes to `/api/stt`, and on `{ok, text}` hands
//! the transcript to a callback (which drops it into the composer + submits).
//!
//! The proxy is same-origin as the served bundle, so `/api/stt` is relative.

use std::cell::RefCell;
use std::rc::Rc;

use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Blob, BlobEvent, MediaRecorder, MediaStream};

/// What the orb is doing right now. Drives styling + the hint label.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecState {
    Idle,
    Recording,
    Transcribing,
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
        move |_| start()
    };
    let on_up = {
        let stop = stop.clone();
        move |_| stop()
    };

    let label = move || match state.get() {
        RecState::Idle => "hold to talk",
        RecState::Recording => "listening… release to send",
        RecState::Transcribing => "transcribing…",
    };
    let orb_class = move || match state.get() {
        RecState::Idle => "voice-orb",
        RecState::Recording => "voice-orb is-recording",
        RecState::Transcribing => "voice-orb is-transcribing",
    };

    // Bridge: stash the transcript callback where start_recording can grab it.
    // We use a thread-local-free approach: pass it into start via a Cell.
    provide_voice_callback(on_transcript, on_status);

    view! {
        <div class="voice-wrap">
            <button
                class=orb_class
                type="button"
                aria-label="push to talk"
                on:pointerdown=on_down
                on:pointerup=on_up.clone()
                on:pointerleave=on_up.clone()
                on:pointercancel=on_up
            >
                <span class="voice-orb__glyph">"🎙"</span>
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

fn deliver_transcript(text: String) {
    VOICE_CB.with(|c| {
        if let Some((cb, _)) = c.borrow().as_ref() {
            cb.run(text);
        }
    });
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
            if let Ok(track) = tracks.get(i as u32).dyn_into::<web_sys::MediaStreamTrack>() {
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

/// Resolve a Blob to its raw bytes via the ArrayBuffer promise.
async fn blob_to_bytes(blob: &Blob) -> Result<Vec<u8>, String> {
    let buf = JsFuture::from(blob.array_buffer())
        .await
        .map_err(|_| "failed to read audio".to_string())?;
    let array = js_sys::Uint8Array::new(&buf);
    Ok(array.to_vec())
}
