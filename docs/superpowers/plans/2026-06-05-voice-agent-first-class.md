# Voice Agent — First-Class Finished Implementation Plan

> **For agentic workers:** Implement task-by-task. Steps use checkbox (`- [ ]`) syntax. This plan runs overnight on a `/loop`; after each work session, commit, then advance the next unchecked task.

**Goal:** Turn the push-to-talk voice orb in `ocean-surface` into a market-ready, first-class voice agent: wake-word ("hey Ocean"), VAD continuous listening, and polished push-to-talk (barge-in, keyboard shortcut, keyless states).

**Architecture:** All voice code lives in `crates/ocean-surface-ui/src/`. The orb currently does hold→record→STT→`on_transcript`→submit→TTS. We keep `on_transcript` as the single submit path and add three new capabilities that all funnel into it: a VAD engine (auto-stop on silence), a wake-word spotter (hands-free trigger), and a mode state machine (push-to-talk / continuous / wake-word) plus barge-in (talking interrupts TTS).

**Tech Stack:** Rust + Leptos (WASM), `web-sys` (Web Audio: `AudioContext`, `AnalyserNode`, `MediaRecorder`), existing `/api/stt` + `/api/tts` proxy endpoints.

**Branch:** `feat/voice-agent-first-class` (isolated; do NOT touch the pre-existing modified files from `feat/longhouse-deck`).

---

## File Structure

- Create: `crates/ocean-surface-ui/src/voice/mod.rs` — re-exports; the `VoiceOrb` component + mode state machine.
- Create: `crates/ocean-surface-ui/src/voice/vad.rs` — voice-activity detection over an `AnalyserNode` (RMS energy + hangover timer).
- Create: `crates/ocean-surface-ui/src/voice/wake.rs` — wake-word spotter (energy-gated STT window matching "hey ocean").
- Create: `crates/ocean-surface-ui/src/voice/capture.rs` — the mic/MediaRecorder plumbing extracted from today's `voice.rs`.
- Modify: `crates/ocean-surface-ui/src/voice.rs` → becomes thin shim or is replaced by the `voice/` module.
- Modify: `crates/ocean-surface-ui/src/tts.rs` — add `stop()` / `is_playing()` for barge-in.
- Modify: `crates/ocean-surface-ui/src/app.rs` — pass mode controls; keyboard shortcut; keyless state already present.

---

## Task 1: VAD engine (the foundation)

**Files:**
- Create: `crates/ocean-surface-ui/src/voice/vad.rs`
- Test: inline `#[cfg(test)]` mod (pure RMS/state-machine logic, no browser).

The browser-coupled parts (AnalyserNode) are thin; the testable core is the silence/speech state machine: given a stream of RMS energy samples and thresholds, decide `Speaking` / `Silent` with a hangover (don't cut on a brief pause).

- [ ] **Step 1: Write the failing test** for the pure VAD state machine.

```rust
// in vad.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_speech_then_endpoints_after_hangover() {
        // 100ms frames; speak threshold 0.02, hangover 300ms (3 frames).
        let mut vad = VadCore::new(0.02, 3);
        assert_eq!(vad.push(0.001), VadEvent::None);      // silence
        assert_eq!(vad.push(0.05), VadEvent::SpeechStart); // onset
        assert_eq!(vad.push(0.04), VadEvent::None);        // still talking
        assert_eq!(vad.push(0.001), VadEvent::None);       // pause frame 1
        assert_eq!(vad.push(0.001), VadEvent::None);       // pause frame 2
        assert_eq!(vad.push(0.001), VadEvent::SpeechEnd);  // hangover elapsed
    }
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p ocean-surface-ui vad`. Expected: FAIL (VadCore not defined).

- [ ] **Step 3: Implement `VadCore`** — `new(speak_threshold, hangover_frames)`, `push(rms) -> VadEvent` enum `{ None, SpeechStart, SpeechEnd }`. Track `in_speech: bool` and `silent_run: u32`.

- [ ] **Step 4: Run test, verify it passes.**

- [ ] **Step 5: Commit** — `feat(voice): VAD core state machine with hangover`.

---

## Task 2: Capture extraction + AnalyserNode RMS feed

Extract today's MediaRecorder code from `voice.rs` into `voice/capture.rs` unchanged (pure refactor, keep it compiling), then add an `AnalyserNode` tap that feeds RMS frames into `VadCore`.

- [ ] **Step 1:** Move `start_recording`/`stop_recording`/`Recorder` into `capture.rs`, re-export. Build: `cargo build -p ocean-surface-ui`. Expected: compiles, push-to-talk still works.
- [ ] **Step 2:** Add `fn rms(samples: &[f32]) -> f32` with a unit test (known buffer → known RMS). Commit.
- [ ] **Step 3:** Wire `AudioContext` + `AnalyserNode` on the live stream; on each animation frame compute RMS, push to a `VadCore`, emit events via callback. Manual browser verify.
- [ ] **Step 4: Commit** — `feat(voice): RMS energy tap on live mic via AnalyserNode`.

---

## Task 3: Continuous listening mode

Toggle that flips the orb into always-listening. VAD `SpeechStart` opens a capture, `SpeechEnd` closes it and runs STT → `on_transcript`. No button-hold.

- [ ] **Step 1:** Add `VoiceMode` enum `{ PushToTalk, Continuous, WakeWord }` to `voice/mod.rs` with a test asserting default + transitions.
- [ ] **Step 2:** In Continuous mode, drive capture from VAD events instead of pointer events. Submit each utterance through the existing `on_transcript`.
- [ ] **Step 3:** Guard against echo: pause VAD capture while TTS is playing (see Task 5). Manual verify: speak, pause, see it auto-submit.
- [ ] **Step 4: Commit** — `feat(voice): continuous VAD listening mode`.

---

## Task 4: Wake-word spotter ("hey Ocean")

In WakeWord mode, run a low-cost energy gate; when speech is detected, capture a short window, STT it, and if the text starts with the wake phrase, treat the remainder (or the next utterance) as the command.

- [ ] **Step 1:** `fn matches_wake(text: &str) -> Option<String>` — normalize, match "hey ocean" / "ok ocean" / "ocean", return trailing command if present. Unit tests for hits/misses/punctuation/case.
- [ ] **Step 2:** Wire spotter: VAD segment → STT → `matches_wake`. On match with trailing text, submit it; on bare wake word, arm a one-shot capture for the next utterance.
- [ ] **Step 3:** Cooldown so it can't re-trigger on its own echo. Manual verify.
- [ ] **Step 4: Commit** — `feat(voice): 'hey Ocean' wake-word spotter`.

---

## Task 5: Barge-in + TTS interrupt

Talking while Ocean is speaking should cut the TTS (barge-in), the hallmark of a real voice agent.

- [ ] **Step 1:** Add `pub fn stop()` and `pub fn is_playing() -> bool` to `tts.rs` (pause the persistent audio element, revoke URL). No test (DOM-bound) — manual.
- [ ] **Step 2:** In Continuous/WakeWord, on VAD `SpeechStart` while `tts::is_playing()`, call `tts::stop()` before capturing.
- [ ] **Step 3: Commit** — `feat(voice): barge-in interrupts TTS playback`.

---

## Task 6: Push-to-talk polish + keyboard shortcut

- [ ] **Step 1:** Global shortcut (e.g. hold `Space` outside the textarea, or a dedicated key) to start/stop push-to-talk. Verify it doesn't fight typing.
- [ ] **Step 2:** Mode switcher UI in the composer (cycle PushToTalk → Continuous → WakeWord) with distinct orb states/labels per mode.
- [ ] **Step 3:** Confirm keyless state (xAI key absent) still shows the disabled "voice off" orb gracefully — already present in `app.rs`, just verify under new module.
- [ ] **Step 4: Commit** — `feat(voice): keyboard shortcut + mode switcher UI`.

---

## Task 7: Final pass — build, fmt, integration check

- [ ] **Step 1:** `cargo fmt`, `cargo clippy -p ocean-surface-ui`, fix warnings.
- [ ] **Step 2:** `cargo build -p ocean-surface-ui --release` (or the wasm/trunk build the crate uses) — green.
- [ ] **Step 3:** Run the full proxy + UI locally, exercise all three modes by hand, confirm barge-in.
- [ ] **Step 4: Commit** — `chore(voice): fmt, clippy, build-green for first-class voice agent`.

---

## Done criteria (the goal holds when ALL are true)

1. Three working voice modes: push-to-talk, continuous, wake-word — switchable in UI.
2. VAD auto-endpoints speech (no button-hold needed in hands-free modes).
3. "Hey Ocean" triggers a command hands-free.
4. Barge-in: talking cuts TTS.
5. Keyboard shortcut for push-to-talk.
6. Keyless state degrades gracefully.
7. `cargo build` + `cargo clippy` clean on `ocean-surface-ui`.
