//! Wake-word spotting over STT transcripts.
//!
//! In wake-word mode the capture layer hands each VAD-endpointed utterance to
//! STT, then to [`match_wake`]. If the transcript opens with the wake phrase,
//! the speaker has summoned Ocean: any text *after* the phrase is the command
//! to run immediately; a bare wake word (no trailing text) arms a one-shot
//! capture for the speaker's next utterance.
//!
//! This is deliberately a pure string matcher — no audio, no `web-sys` — so the
//! triggering rules are unit-testable and tunable without a browser. STT output
//! is messy (casing, trailing punctuation, "hey, Ocean…"), so we normalize
//! hard before matching.

/// Outcome of testing a transcript against the wake phrases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeMatch {
    /// No wake phrase at the start of the utterance.
    None,
    /// Wake phrase heard with no command after it — arm a follow-up capture.
    Armed,
    /// Wake phrase heard followed by a command to run now.
    Command(String),
}

/// Accepted wake phrases, longest first so "hey ocean" wins over "ocean".
const WAKE_PHRASES: &[&str] = &["hey ocean", "ok ocean", "okay ocean", "hi ocean", "ocean"];

/// Lower-case, strip leading punctuation/filler, collapse whitespace.
///
/// STT gives us things like `"Hey, Ocean —"` or `"  ocean, what's up?"`. We
/// fold case, turn any non-alphanumeric run into a single space, and trim, so
/// the phrase comparison sees a clean `"hey ocean what's up"`-style string.
fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_space = true; // leading => skip leading spaces
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim_end().to_string()
}

/// Test a transcript for a leading wake phrase.
///
/// Matches only when the utterance *starts* with a wake phrase — "tell ocean to
/// stop" is not a summon. Returns the trailing command (if any) as a
/// [`WakeMatch::Command`], a bare phrase as [`WakeMatch::Armed`], else
/// [`WakeMatch::None`].
pub fn match_wake(text: &str) -> WakeMatch {
    let norm = normalize(text);
    if norm.is_empty() {
        return WakeMatch::None;
    }
    for phrase in WAKE_PHRASES {
        if norm == *phrase {
            return WakeMatch::Armed;
        }
        // Require a word boundary after the phrase so "oceanography" or
        // "oceans" don't trigger the bare "ocean" alias.
        let with_space = format!("{phrase} ");
        if let Some(rest) = norm.strip_prefix(&with_space) {
            let rest = rest.trim();
            if rest.is_empty() {
                return WakeMatch::Armed;
            }
            return WakeMatch::Command(rest.to_string());
        }
    }
    WakeMatch::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_hey_ocean_arms() {
        assert_eq!(match_wake("Hey Ocean"), WakeMatch::Armed);
        assert_eq!(match_wake("hey ocean"), WakeMatch::Armed);
    }

    #[test]
    fn punctuation_and_case_are_ignored() {
        assert_eq!(match_wake("Hey, Ocean —"), WakeMatch::Armed);
        assert_eq!(match_wake("  OCEAN!!! "), WakeMatch::Armed);
    }

    #[test]
    fn wake_with_command_returns_remainder() {
        assert_eq!(
            match_wake("Hey Ocean, what's on my calendar?"),
            WakeMatch::Command("what s on my calendar".to_string())
        );
        assert_eq!(
            match_wake("ok ocean summarize the call"),
            WakeMatch::Command("summarize the call".to_string())
        );
    }

    #[test]
    fn bare_ocean_alias_arms() {
        assert_eq!(match_wake("Ocean"), WakeMatch::Armed);
        assert_eq!(
            match_wake("ocean mute yourself"),
            WakeMatch::Command("mute yourself".to_string())
        );
    }

    #[test]
    fn non_leading_mention_does_not_trigger() {
        assert_eq!(match_wake("tell ocean to stop"), WakeMatch::None);
        assert_eq!(match_wake("the pacific ocean is big"), WakeMatch::None);
    }

    #[test]
    fn similar_words_do_not_trigger() {
        // "oceanography" must not match the bare "ocean" alias.
        assert_eq!(match_wake("oceanography lecture"), WakeMatch::None);
        assert_eq!(match_wake("oceans rise"), WakeMatch::None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(match_wake(""), WakeMatch::None);
        assert_eq!(match_wake("   ...  "), WakeMatch::None);
    }

    #[test]
    fn longer_phrase_wins() {
        // "hey ocean" should arm, not be read as bare "ocean" + command "hey"
        // (it can't, since matching is prefix-anchored, but guard the ordering).
        assert_eq!(match_wake("okay ocean"), WakeMatch::Armed);
    }
}
