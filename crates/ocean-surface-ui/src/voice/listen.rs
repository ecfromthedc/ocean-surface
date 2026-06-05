//! Continuous hands-free listening engine.
//!
//! Push-to-talk records only while a pointer is held. The hands-free modes
//! (continuous + wake-word) instead keep a long-lived mic stream open and let
//! **voice-activity detection** decide when an utterance starts and ends:
//!
//! 1. Acquire the mic once and route it through an `AnalyserNode`.
//! 2. On every animation frame, read the time-domain samples, compute
//!    [`vad::rms`], and feed it to a [`vad::VadCore`].
//! 3. On `SpeechStart`, barge in (stop any TTS) and begin a `MediaRecorder`
//!    segment. On `SpeechEnd`, stop the segment — its `onstop` uploads to STT,
//!    and the transcript flows through the same [`super::deliver_transcript`]
//!    path, which routes it through the active hands-free state machine.
//!
//! The loop holds onto its closures and nodes for its lifetime; [`stop`] tears
//! everything down and releases the mic.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AnalyserNode, AudioContext, MediaStream, MediaStreamAudioSourceNode, MediaStreamConstraints,
};

use super::vad::{self, VadCore, VadEvent};
use super::{
    segment_recorder_start, segment_recorder_stop, segment_recorder_stop_discard, SegmentRecorder,
};
use crate::tts;

/// Tuning for the hands-free VAD. Frame cadence is the browser's rAF (~16ms),
/// so the hangover count is generous to avoid clipping natural pauses.
const SPEAK_THRESHOLD: f32 = 0.018;
const HANGOVER_FRAMES: u32 = 35; // ~0.55s of trailing silence ends an utterance.
/// Ignore blips shorter than this many speaking frames (rejects clicks/coughs).
const MIN_SPEECH_FRAMES: u32 = 6;

/// Shared, optional handle to the self-rescheduling animation-frame closure.
/// `None` once torn down; the `Rc` lets the running frame reschedule itself.
type FrameCell = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// Everything the running listen loop must keep alive.
#[derive(Default)]
pub struct ListenLoop {
    ctx: Option<AudioContext>,
    stream: Option<MediaStream>,
    _source: Option<MediaStreamAudioSourceNode>,
    analyser: Option<AnalyserNode>,
    raf_handle: Option<i32>,
    /// Owns the self-rescheduling frame closure for the loop's lifetime.
    frame_cell: Option<FrameCell>,
    segment: Rc<RefCell<SegmentRecorder>>,
    running: Rc<RefCell<bool>>,
}

/// Start continuous listening. Returns a handle that must be kept alive; drop
/// or call [`stop`] to end. Errors (mic denied, no AudioContext) are surfaced
/// via the shell status callback and leave the loop not-running.
pub async fn start() -> Result<Rc<RefCell<ListenLoop>>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let navigator = window.navigator();
    let media = navigator
        .media_devices()
        .map_err(|_| "this browser cannot record audio".to_string())?;

    // Request browser-level acoustic echo cancellation so the mic doesn't pick
    // up Ocean's own TTS coming out of the speakers. This is the first line of
    // defense against the self-trigger loop; the software suppression window
    // below (SELF_SPEAKING) is the second.
    let constraints = MediaStreamConstraints::new();
    let audio_opts = js_sys::Object::new();
    let set = |k: &str, v: bool| {
        let _ = js_sys::Reflect::set(&audio_opts, &JsValue::from_str(k), &JsValue::from_bool(v));
    };
    set("echoCancellation", true);
    set("noiseSuppression", true);
    set("autoGainControl", true);
    constraints.set_audio(&audio_opts);
    let promise = media
        .get_user_media_with_constraints(&constraints)
        .map_err(|_| "microphone unavailable".to_string())?;
    let stream: MediaStream = JsFuture::from(promise)
        .await
        .map_err(|_| "microphone permission denied".to_string())?
        .dyn_into()
        .map_err(|_| "no media stream".to_string())?;

    let ctx = AudioContext::new().map_err(|_| "no audio context".to_string())?;
    let source = ctx
        .create_media_stream_source(&stream)
        .map_err(|_| "cannot tap mic".to_string())?;
    let analyser = ctx
        .create_analyser()
        .map_err(|_| "cannot create analyser".to_string())?;
    analyser.set_fft_size(2048);
    source
        .connect_with_audio_node(&analyser)
        .map_err(|_| "cannot connect analyser".to_string())?;

    let loop_state = Rc::new(RefCell::new(ListenLoop::default()));
    {
        let mut ls = loop_state.borrow_mut();
        ls.ctx = Some(ctx);
        ls.stream = Some(stream.clone());
        ls._source = Some(source);
        ls.analyser = Some(analyser.clone());
        *ls.running.borrow_mut() = true;
    }

    // The per-frame VAD + segment driver. Reschedules itself via rAF until the
    // loop's `running` flag is cleared.
    let vad = Rc::new(RefCell::new(VadCore::new(SPEAK_THRESHOLD, HANGOVER_FRAMES)));
    let speech_frames = Rc::new(RefCell::new(0u32));
    let buf_len = analyser.fft_size() as usize;
    let segment = loop_state.borrow().segment.clone();
    let running = loop_state.borrow().running.clone();
    let stream_for_seg = stream.clone();

    // Rc cell holding the frame closure so it can reschedule itself.
    let frame_cell: FrameCell = Rc::new(RefCell::new(None));
    let frame_cell2 = frame_cell.clone();
    let window2 = window.clone();

    let on_frame = Closure::wrap(Box::new(move || {
        if !*running.borrow() {
            return;
        }
        let mut samples = vec![0.0f32; buf_len];
        analyser.get_float_time_domain_data(&mut samples);
        let energy = vad::rms(&samples);
        let event = vad.borrow_mut().push(energy);

        match event {
            VadEvent::SpeechStart => {
                *speech_frames.borrow_mut() = 1;
                // Barge-in: if Ocean is talking, cut it off so we hear the user.
                if tts::is_playing() {
                    tts::stop();
                }
                segment_recorder_start(&segment, &stream_for_seg);
            }
            VadEvent::None => {
                if vad.borrow().in_speech() {
                    *speech_frames.borrow_mut() += 1;
                }
            }
            VadEvent::SpeechEnd => {
                let frames = *speech_frames.borrow();
                *speech_frames.borrow_mut() = 0;
                if frames >= MIN_SPEECH_FRAMES {
                    // Real utterance → stop segment, which uploads to STT.
                    segment_recorder_stop(&segment);
                } else {
                    // Too short — discard without an STT round-trip.
                    segment_recorder_stop_discard(&segment);
                }
            }
        }

        // Reschedule next frame.
        if let Some(cb) = frame_cell2.borrow().as_ref() {
            let _ = window2.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }) as Box<dyn FnMut()>);

    let handle = window
        .request_animation_frame(on_frame.as_ref().unchecked_ref())
        .map_err(|_| "cannot schedule audio frame".to_string())?;

    // The closure must stay owned by `frame_cell` (shared with `frame_cell2`)
    // so each frame can reschedule the *same* closure. We keep a clone of that
    // Rc in the loop state to tie its lifetime to the loop, rather than moving
    // the closure out of the cell.
    *frame_cell.borrow_mut() = Some(on_frame);
    {
        let mut ls = loop_state.borrow_mut();
        ls.raf_handle = Some(handle);
        ls.frame_cell = Some(frame_cell.clone());
    }

    let _ = stream;
    Ok(loop_state)
}

/// Stop the listen loop: clear the running flag, cancel the pending frame,
/// close the AudioContext, and release the mic tracks.
pub fn stop(loop_state: &Rc<RefCell<ListenLoop>>) {
    let mut ls = loop_state.borrow_mut();
    *ls.running.borrow_mut() = false;

    if let (Some(window), Some(handle)) = (web_sys::window(), ls.raf_handle.take()) {
        let _ = window.cancel_animation_frame(handle);
    }
    segment_recorder_stop_discard(&ls.segment);
    if let Some(stream) = ls.stream.take() {
        let tracks = stream.get_tracks();
        for i in 0..tracks.length() {
            if let Ok(track) = tracks.get(i).dyn_into::<web_sys::MediaStreamTrack>() {
                track.stop();
            }
        }
    }
    if let Some(ctx) = ls.ctx.take() {
        let _ = ctx.close();
    }
    ls.analyser = None;
    ls._source = None;
    // Drop the frame closure last (running is already false, so it won't
    // reschedule even if a final frame is in flight).
    if let Some(cell) = ls.frame_cell.take() {
        *cell.borrow_mut() = None;
    }
}
