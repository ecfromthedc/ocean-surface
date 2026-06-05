//! Voice-activity detection core.
//!
//! The browser-coupled part (an `AnalyserNode` sampling the live mic) is thin;
//! the part worth testing is the silence/speech **state machine**. Given a
//! stream of per-frame RMS energy values plus a speak threshold and a hangover
//! count, it decides when speech starts and when it has truly ended — the
//! hangover prevents a brief mid-sentence pause from being treated as the end
//! of an utterance.
//!
//! `VadCore` is pure: no `web-sys`, no allocation, fully unit-testable off the
//! browser. The capture layer feeds it `push(rms)` once per audio frame and
//! reacts to the returned [`VadEvent`].

/// What the VAD decided for the frame just pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// No state change this frame.
    None,
    /// Speech just began (silence → speech).
    SpeechStart,
    /// Speech just ended: a run of silence long enough to clear the hangover.
    SpeechEnd,
}

/// Pure speech/silence state machine driven by per-frame RMS energy.
#[derive(Debug, Clone)]
pub struct VadCore {
    /// RMS above this counts as speech for the frame.
    speak_threshold: f32,
    /// How many consecutive silent frames end an utterance (the "hangover").
    hangover_frames: u32,
    /// Are we currently inside an utterance?
    in_speech: bool,
    /// Consecutive silent frames seen while `in_speech`.
    silent_run: u32,
}

impl VadCore {
    /// Create a VAD core.
    ///
    /// - `speak_threshold`: RMS energy (0.0–1.0-ish) above which a frame is
    ///   "speech". Typical mic speech sits around 0.02–0.1.
    /// - `hangover_frames`: number of consecutive silent frames required to
    ///   declare the utterance over. At 100ms/frame, `3` ≈ 300ms of silence.
    pub fn new(speak_threshold: f32, hangover_frames: u32) -> Self {
        Self {
            speak_threshold,
            hangover_frames,
            in_speech: false,
            silent_run: 0,
        }
    }

    /// Are we currently mid-utterance?
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Feed one frame of RMS energy; returns any state transition.
    pub fn push(&mut self, rms: f32) -> VadEvent {
        let loud = rms >= self.speak_threshold;

        if self.in_speech {
            if loud {
                // Still talking — reset the silence counter.
                self.silent_run = 0;
                VadEvent::None
            } else {
                self.silent_run += 1;
                if self.silent_run >= self.hangover_frames {
                    // Enough trailing silence: the utterance is over.
                    self.in_speech = false;
                    self.silent_run = 0;
                    VadEvent::SpeechEnd
                } else {
                    VadEvent::None
                }
            }
        } else if loud {
            // Silence → speech onset.
            self.in_speech = true;
            self.silent_run = 0;
            VadEvent::SpeechStart
        } else {
            VadEvent::None
        }
    }

    /// Force the state machine back to idle (e.g. on mode switch or stop).
    /// Exercised by tests; retained as part of the VAD's public surface.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.in_speech = false;
        self.silent_run = 0;
    }
}

/// Root-mean-square energy of a frame of normalized audio samples.
///
/// The `AnalyserNode` hands us time-domain samples (roughly -1.0..=1.0). RMS is
/// the standard cheap loudness estimate the VAD thresholds against. Returns 0.0
/// for an empty frame rather than NaN so callers never have to special-case it.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_speech_then_endpoints_after_hangover() {
        // 100ms frames; speak threshold 0.02, hangover 300ms (3 frames).
        let mut vad = VadCore::new(0.02, 3);
        assert_eq!(vad.push(0.001), VadEvent::None); // silence
        assert_eq!(vad.push(0.05), VadEvent::SpeechStart); // onset
        assert_eq!(vad.push(0.04), VadEvent::None); // still talking
        assert_eq!(vad.push(0.001), VadEvent::None); // pause frame 1
        assert_eq!(vad.push(0.001), VadEvent::None); // pause frame 2
        assert_eq!(vad.push(0.001), VadEvent::SpeechEnd); // hangover elapsed
    }

    #[test]
    fn brief_pause_does_not_end_speech() {
        let mut vad = VadCore::new(0.02, 3);
        assert_eq!(vad.push(0.05), VadEvent::SpeechStart);
        // One silent frame, then speech resumes — should NOT end.
        assert_eq!(vad.push(0.001), VadEvent::None);
        assert_eq!(vad.push(0.05), VadEvent::None);
        assert!(vad.in_speech());
    }

    #[test]
    fn threshold_is_inclusive() {
        let mut vad = VadCore::new(0.02, 2);
        // Exactly at threshold counts as speech.
        assert_eq!(vad.push(0.02), VadEvent::SpeechStart);
    }

    #[test]
    fn reset_returns_to_idle() {
        let mut vad = VadCore::new(0.02, 2);
        vad.push(0.05);
        assert!(vad.in_speech());
        vad.reset();
        assert!(!vad.in_speech());
        // After reset, next loud frame is a fresh onset.
        assert_eq!(vad.push(0.05), VadEvent::SpeechStart);
    }

    #[test]
    fn silence_throughout_never_fires() {
        let mut vad = VadCore::new(0.02, 3);
        for _ in 0..10 {
            assert_eq!(vad.push(0.0), VadEvent::None);
        }
        assert!(!vad.in_speech());
    }

    #[test]
    fn rms_of_empty_is_zero() {
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0, 0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn rms_of_constant_amplitude_equals_amplitude() {
        // RMS of a constant |x| signal is |x|.
        let r = rms(&[0.5, -0.5, 0.5, -0.5]);
        assert!((r - 0.5).abs() < 1e-6, "rms was {r}");
    }

    #[test]
    fn rms_full_scale_is_one() {
        let r = rms(&[1.0, -1.0, 1.0, -1.0]);
        assert!((r - 1.0).abs() < 1e-6, "rms was {r}");
    }
}
