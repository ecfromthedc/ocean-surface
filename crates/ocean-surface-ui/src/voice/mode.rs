//! Voice interaction mode + the hands-free runtime state machine.
//!
//! The orb supports three modes the user cycles through:
//!
//! - [`VoiceMode::PushToTalk`] — hold the orb to talk (the original behavior).
//! - [`VoiceMode::Continuous`] — always listening; VAD endpoints each utterance
//!   and submits it automatically.
//! - [`VoiceMode::WakeWord`] — listening for "hey Ocean"; only acts after the
//!   wake phrase, then runs the command (or the next utterance).
//!
//! [`HandsFreeState`] is the pure logic that decides, for the continuous and
//! wake-word modes, what to do with each VAD-endpointed utterance: ignore it,
//! arm for a follow-up, or submit it as a command. It owns no audio and no
//! `web-sys`, so the decision rules are unit-tested off the browser. The orb
//! drives it with VAD events + STT transcripts and reacts to the returned
//! [`HandsFreeAction`].

use super::wake::{match_wake, WakeMatch};

/// Which interaction mode the orb is in. Cycles in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceMode {
    /// Hold-to-talk (default, always available even without VAD).
    #[default]
    PushToTalk,
    /// Always listening; every utterance is submitted.
    Continuous,
    /// Listening for the wake word before acting.
    WakeWord,
}

impl VoiceMode {
    /// The next mode when the user taps the mode switcher.
    pub fn next(self) -> Self {
        match self {
            VoiceMode::PushToTalk => VoiceMode::Continuous,
            VoiceMode::Continuous => VoiceMode::WakeWord,
            VoiceMode::WakeWord => VoiceMode::PushToTalk,
        }
    }

    /// Is this a hands-free mode (driven by VAD rather than pointer events)?
    pub fn is_hands_free(self) -> bool {
        matches!(self, VoiceMode::Continuous | VoiceMode::WakeWord)
    }

    /// Short label for the orb hint line.
    pub fn label(self) -> &'static str {
        match self {
            VoiceMode::PushToTalk => "hold to talk",
            VoiceMode::Continuous => "listening",
            VoiceMode::WakeWord => "say \u{201c}hey Ocean\u{201d}",
        }
    }

    /// CSS modifier so each mode can style its orb distinctly.
    pub fn css_modifier(self) -> &'static str {
        match self {
            VoiceMode::PushToTalk => "is-ptt",
            VoiceMode::Continuous => "is-continuous",
            VoiceMode::WakeWord => "is-wake",
        }
    }
}

/// What the orb should do with a freshly transcribed utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandsFreeAction {
    /// Do nothing — not a command for us (e.g. chatter before a wake word).
    Ignore,
    /// Submit this text as a prompt to the agent.
    Submit(String),
}

/// Pure decision logic for the hands-free modes.
///
/// Continuous mode submits every non-empty utterance. Wake-word mode stays
/// dormant until it hears the wake phrase: a phrase with a trailing command
/// submits immediately; a bare phrase *arms* it so the very next utterance is
/// taken as the command.
#[derive(Debug, Clone)]
pub struct HandsFreeState {
    mode: VoiceMode,
    /// Wake-word mode only: heard the wake word, awaiting the command utterance.
    armed: bool,
}

impl HandsFreeState {
    /// Create a state machine for `mode`. Starts disarmed.
    pub fn new(mode: VoiceMode) -> Self {
        Self { mode, armed: false }
    }

    /// Whether wake-word mode is currently armed for a follow-up command.
    /// Exercised by tests; part of the state machine's public surface.
    #[allow(dead_code)]
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Change mode (resets the armed latch). The orb currently rebuilds the
    /// router on mode change rather than mutating in place, but this is kept
    /// for in-place transitions and is covered by tests.
    #[allow(dead_code)]
    pub fn set_mode(&mut self, mode: VoiceMode) {
        self.mode = mode;
        self.armed = false;
    }

    /// Feed a transcribed utterance; decide what to do with it.
    pub fn on_utterance(&mut self, transcript: &str) -> HandsFreeAction {
        let text = transcript.trim();
        if text.is_empty() {
            return HandsFreeAction::Ignore;
        }
        match self.mode {
            VoiceMode::PushToTalk => {
                // Push-to-talk doesn't flow through here, but be safe: submit.
                HandsFreeAction::Submit(text.to_string())
            }
            VoiceMode::Continuous => HandsFreeAction::Submit(text.to_string()),
            VoiceMode::WakeWord => {
                if self.armed {
                    // We were summoned last utterance; this one is the command.
                    self.armed = false;
                    return HandsFreeAction::Submit(text.to_string());
                }
                match match_wake(text) {
                    WakeMatch::None => HandsFreeAction::Ignore,
                    WakeMatch::Armed => {
                        self.armed = true;
                        HandsFreeAction::Ignore
                    }
                    WakeMatch::Command(cmd) => HandsFreeAction::Submit(cmd),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_cycles_through_all_three() {
        assert_eq!(VoiceMode::default(), VoiceMode::PushToTalk);
        assert_eq!(VoiceMode::PushToTalk.next(), VoiceMode::Continuous);
        assert_eq!(VoiceMode::Continuous.next(), VoiceMode::WakeWord);
        assert_eq!(VoiceMode::WakeWord.next(), VoiceMode::PushToTalk);
    }

    #[test]
    fn only_hands_free_modes_report_hands_free() {
        assert!(!VoiceMode::PushToTalk.is_hands_free());
        assert!(VoiceMode::Continuous.is_hands_free());
        assert!(VoiceMode::WakeWord.is_hands_free());
    }

    #[test]
    fn continuous_submits_every_utterance() {
        let mut s = HandsFreeState::new(VoiceMode::Continuous);
        assert_eq!(
            s.on_utterance("what's the weather"),
            HandsFreeAction::Submit("what's the weather".to_string())
        );
        assert_eq!(
            s.on_utterance("and tomorrow"),
            HandsFreeAction::Submit("and tomorrow".to_string())
        );
    }

    #[test]
    fn continuous_ignores_empty() {
        let mut s = HandsFreeState::new(VoiceMode::Continuous);
        assert_eq!(s.on_utterance("   "), HandsFreeAction::Ignore);
    }

    #[test]
    fn wake_word_ignores_until_summoned() {
        let mut s = HandsFreeState::new(VoiceMode::WakeWord);
        assert_eq!(s.on_utterance("random chatter"), HandsFreeAction::Ignore);
        assert!(!s.is_armed());
    }

    #[test]
    fn wake_word_with_command_submits_immediately() {
        let mut s = HandsFreeState::new(VoiceMode::WakeWord);
        assert_eq!(
            s.on_utterance("hey ocean what's on my calendar"),
            HandsFreeAction::Submit("what s on my calendar".to_string())
        );
        assert!(!s.is_armed());
    }

    #[test]
    fn wake_word_bare_arms_then_next_utterance_is_command() {
        let mut s = HandsFreeState::new(VoiceMode::WakeWord);
        // Bare wake word: armed, nothing submitted yet.
        assert_eq!(s.on_utterance("hey ocean"), HandsFreeAction::Ignore);
        assert!(s.is_armed());
        // Next utterance is taken as the command verbatim (not re-matched).
        assert_eq!(
            s.on_utterance("summarize the call"),
            HandsFreeAction::Submit("summarize the call".to_string())
        );
        assert!(!s.is_armed());
    }

    #[test]
    fn switching_mode_disarms() {
        let mut s = HandsFreeState::new(VoiceMode::WakeWord);
        s.on_utterance("hey ocean");
        assert!(s.is_armed());
        s.set_mode(VoiceMode::Continuous);
        assert!(!s.is_armed());
    }
}
