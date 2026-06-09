//! Live call-mode UI (OCEAN-CALL, Item 6 — purely additive).
//!
//! A live PSTN call, bridged via SIP into a LiveKit room, makes the daemon emit
//! a family of call-intelligence events on its control stream (`GET /v1/events`).
//! This component subscribes to those frames and renders a live call view:
//! a rolling summary strip, a scrolling transcript (interim vs. final segments,
//! labelled by speaker, with Ocean's spoken replies inline), detected action-item
//! chips, and a wake-orb pulse when the wake word fires.
//!
//! ## Why `/v1/events` and not `/v1/agent/events`
//!
//! The agent stream (`/v1/agent/events`) only carries `AgentTurnEvent` and
//! serializes just the inner event. The call events are `OceanEvent` variants
//! (ocean-core), broadcast on the control stream `/v1/events`, which serializes
//! the FULL `EventEnvelope` — so each frame is the flattened `OceanEvent`
//! (`#[serde(tag = "type", rename_all = "snake_case")]`) with `id` / `at` /
//! `session_id` riding alongside. We model only the seven `call_*` frames; the
//! envelope's extra fields are ignored by serde. This mirrors exactly how
//! `Daemon::connect_permission_stream` consumes the permission frames on the same
//! stream — same transport, same per-`event:`-name subscription, same reconnect
//! loop. We deliberately keep this self-contained (no `daemon.rs` changes): the
//! component opens its own `EventSource` against `daemon.url` and owns its signals,
//! so the call view is strictly additive over the existing surface.
//!
//! The daemon's SSE `event:` names for these frames (ocean-daemon `event_name`)
//! are `call_started`, `call_transcript_segment`, `call_summary_updated`,
//! `call_task_detected`, `call_wake_triggered`, `call_agent_spoke`, `call_ended`.

use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen_futures::spawn_local;

use crate::daemon::Daemon;

/// The seven `call_*` frames the daemon broadcasts on `/v1/events`, modelled as
/// the flattened `OceanEvent` wire shape (`tag = "type"`, snake_case). The
/// control stream serializes the whole `EventEnvelope`, so every frame also
/// carries `id` / `at` / `session_id`; serde ignores those unknown fields here.
/// Field names + the `final` rename match `ocean_core::OceanEvent` exactly so the
/// daemon's JSON deserializes without a translation layer.
///
/// `#[allow(dead_code)]`: several fields (`as_of_ms`, `source_quote`,
/// `confidence`, `duration_ms`, etc.) are part of the wire contract and must be
/// present for the frame to deserialize, but the view doesn't render them yet.
/// This mirrors the codebase's `ControlEvent` / response structs that keep
/// wire-complete fields under the same allow.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CallEvent {
    /// A call connected and Ocean joined its room as a server participant.
    CallStarted {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        room_id: String,
        #[serde(default)]
        participants: Vec<String>,
    },
    /// One transcribed segment. `final` is false while streaming STT is still
    /// revising the segment — those render interim (greyed/italic).
    CallTranscriptSegment {
        #[serde(default)]
        speaker: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        start_ms: u64,
        #[serde(rename = "final", default)]
        is_final: bool,
    },
    /// The rolling auto-summary of the call so far.
    CallSummaryUpdated {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        as_of_ms: u64,
    },
    /// A detected task / action-item. Detect-and-notify only — acting on it is
    /// always a separate, human-approved turn, so these render as passive chips.
    CallTaskDetected {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        assignee: Option<String>,
        #[serde(default)]
        due: Option<String>,
        #[serde(default)]
        source_quote: Option<String>,
        #[serde(default)]
        confidence: f32,
    },
    /// The wake word fired; `utterance` is what followed it. Drives the wake orb.
    CallWakeTriggered {
        #[serde(default)]
        utterance: String,
    },
    /// Ocean spoke `text` back into the call via TTS (active lane only). Renders
    /// as Ocean's reply line in the transcript.
    CallAgentSpoke {
        #[serde(default)]
        text: String,
    },
    /// The call ended; collapses/archives the panel.
    CallEnded {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        duration_ms: u64,
    },
    /// Every non-call frame on the shared control stream falls here and is
    /// dropped — the stream also carries permission frames we don't model.
    #[serde(other)]
    Other,
}

/// One rendered transcript line. Covers both human/caller segments
/// (`speaker` from STT) and Ocean's spoken replies (`is_agent`).
#[derive(Debug, Clone, PartialEq)]
struct TranscriptLine {
    speaker: String,
    text: String,
    /// Ordering / interim-replacement key from the STT segment (`start_ms`).
    /// Ocean replies use a monotonic synthetic key so they append in order.
    start_ms: u64,
    /// False while STT is still revising this segment — renders greyed/italic.
    is_final: bool,
    /// True for Ocean's own TTS replies, styled distinctly from caller lines.
    is_agent: bool,
}

/// A detected action-item chip.
#[derive(Debug, Clone, PartialEq)]
struct TaskChip {
    task_id: String,
    title: String,
    assignee: Option<String>,
    due: Option<String>,
}

/// Live state for the call view, all `Copy` signal handles so closures can grab
/// them freely. `active` gates the whole panel: false until `call_started`,
/// flipped back off on `call_ended`.
#[derive(Clone, Copy)]
struct CallState {
    active: RwSignal<bool>,
    summary: RwSignal<String>,
    participants: RwSignal<Vec<String>>,
    lines: RwSignal<Vec<TranscriptLine>>,
    tasks: RwSignal<Vec<TaskChip>>,
    /// Bumped on every wake trigger; the orb keys its pulse animation off the
    /// change so each wake re-fires the flash even back-to-back.
    wake_pulse: RwSignal<u64>,
    /// Latest wake utterance, shown briefly next to the orb.
    wake_text: RwSignal<String>,
    /// Monotonic key generator for Ocean's reply lines so they sort after the
    /// caller segment that prompted them rather than colliding on `start_ms`.
    agent_seq: RwSignal<u64>,
}

impl CallState {
    fn new() -> Self {
        Self {
            active: RwSignal::new(false),
            summary: RwSignal::new(String::new()),
            participants: RwSignal::new(Vec::new()),
            lines: RwSignal::new(Vec::new()),
            tasks: RwSignal::new(Vec::new()),
            wake_pulse: RwSignal::new(0),
            wake_text: RwSignal::new(String::new()),
            agent_seq: RwSignal::new(0),
        }
    }

    /// Reset transcript/summary/task state for a fresh call so a second call in
    /// the same session doesn't inherit the previous one's lines.
    fn reset(&self) {
        self.summary.set(String::new());
        self.participants.set(Vec::new());
        self.lines.set(Vec::new());
        self.tasks.set(Vec::new());
        self.wake_text.set(String::new());
        self.agent_seq.set(0);
    }
}

/// Apply one decoded `CallEvent` to the live state. Split out from the SSE loop
/// so it's unit-testable off-target (no `EventSource` needed). Returns nothing;
/// all effects land on the signals.
fn apply_call_event(state: &CallState, evt: CallEvent) {
    match evt {
        CallEvent::CallStarted { participants, .. } => {
            state.reset();
            state.participants.set(participants);
            state.active.set(true);
        }
        CallEvent::CallTranscriptSegment {
            speaker,
            text,
            start_ms,
            is_final,
        } => {
            // A non-final segment is revised in place: STT re-emits the same
            // `start_ms` with longer/cleaner text until it goes final. Match on
            // (start_ms, speaker, non-agent) and replace, else append.
            state.lines.update(|lines| {
                if let Some(existing) = lines
                    .iter_mut()
                    .find(|l| !l.is_agent && l.start_ms == start_ms && l.speaker == speaker)
                {
                    existing.text = text;
                    existing.is_final = is_final;
                } else {
                    lines.push(TranscriptLine {
                        speaker,
                        text,
                        start_ms,
                        is_final,
                        is_agent: false,
                    });
                    lines.sort_by_key(|l| l.start_ms);
                }
            });
        }
        CallEvent::CallSummaryUpdated { summary, .. } => {
            state.summary.set(summary);
        }
        CallEvent::CallTaskDetected {
            task_id,
            title,
            assignee,
            due,
            ..
        } => {
            state.tasks.update(|tasks| {
                // Dedupe by task_id so a re-emitted detection doesn't stack.
                if !tasks.iter().any(|t| t.task_id == task_id) {
                    tasks.push(TaskChip {
                        task_id,
                        title,
                        assignee,
                        due,
                    });
                }
            });
        }
        CallEvent::CallWakeTriggered { utterance } => {
            state.wake_text.set(utterance);
            state.wake_pulse.update(|n| *n = n.wrapping_add(1));
        }
        CallEvent::CallAgentSpoke { text } => {
            // Ocean's reply appends after everything heard so far. Use a key past
            // the current max segment time plus a monotonic bump so successive
            // replies keep their order.
            let seq = state.agent_seq.get_untracked().wrapping_add(1);
            state.agent_seq.set(seq);
            state.lines.update(|lines| {
                let key = lines.iter().map(|l| l.start_ms).max().unwrap_or(0) + seq;
                lines.push(TranscriptLine {
                    speaker: "Ocean".to_string(),
                    text,
                    start_ms: key,
                    is_final: true,
                    is_agent: true,
                });
            });
        }
        CallEvent::CallEnded { .. } => {
            // Collapse/archive the panel. Keep the last transcript/summary in the
            // signals so a re-open (next call_started) resets cleanly; only the
            // `active` gate flips.
            state.active.set(false);
            state.wake_text.set(String::new());
        }
        CallEvent::Other => {}
    }
}

/// SSE `event:` names the daemon tags `call_*` frames with on `/v1/events`
/// (ocean-daemon `event_name`). `EventSource` delivers frames by their `event:`
/// name, so — exactly like the permission stream — we must subscribe to each by
/// name; an unsubscribed name is dropped at the transport layer.
const CALL_EVENT_NAMES: [&str; 7] = [
    "call_started",
    "call_transcript_segment",
    "call_summary_updated",
    "call_task_detected",
    "call_wake_triggered",
    "call_agent_spoke",
    "call_ended",
];

/// Open and hold the `/v1/events` subscription for the seven call frames,
/// reconnecting on drop. Mirrors `Daemon::connect_permission_stream`: same URL,
/// same per-name subscription, same 1–2s backoff. Self-contained — reads only
/// `daemon.url` and pushes into `state`.
fn spawn_call_stream(url: RwSignal<String>, state: CallState) {
    spawn_local(async move {
        loop {
            let base = url.get_untracked();
            let events_url = format!("{}/v1/events", base.trim_end_matches('/'));
            let mut es = match EventSource::new(&events_url) {
                Ok(es) => es,
                Err(_) => {
                    gloo_timers::future::TimeoutFuture::new(2_000).await;
                    continue;
                }
            };

            let mut subs = Vec::new();
            let mut sub_err = false;
            for name in CALL_EVENT_NAMES {
                match es.subscribe(name) {
                    Ok(s) => subs.push(s),
                    Err(_) => {
                        sub_err = true;
                        break;
                    }
                }
            }
            if sub_err {
                gloo_timers::future::TimeoutFuture::new(2_000).await;
                continue;
            }

            let mut stream = futures_util::stream::select_all(subs);
            while let Some(msg) = stream.next().await {
                let Ok((_event_name, msg)) = msg else {
                    continue;
                };
                let Some(data) = msg.data().as_string() else {
                    continue;
                };
                match serde_json::from_str::<CallEvent>(&data) {
                    Ok(evt) => apply_call_event(&state, evt),
                    Err(_) => continue,
                }
            }

            // Stream ended (proxy/daemon dropped it): pause, then reconnect.
            gloo_timers::future::TimeoutFuture::new(1_000).await;
        }
    });
}

/// The live call-mode panel (OCEAN-CALL). Hidden until a `call_started` frame
/// arrives; renders the summary strip, wake orb, transcript, and task chips for
/// the active call; collapses on `call_ended`. Additive — mount it anywhere in
/// the chat surface alongside the other inline panels.
#[component]
pub fn CallPanel(daemon: Daemon) -> impl IntoView {
    let state = CallState::new();
    spawn_call_stream(daemon.url, state);

    let CallState {
        active,
        summary,
        participants,
        lines,
        tasks,
        wake_pulse,
        wake_text,
        ..
    } = state;

    let has_summary = move || !summary.get().trim().is_empty();
    let has_tasks = move || !tasks.get().is_empty();
    let has_wake = move || !wake_text.get().trim().is_empty();
    let participant_label = move || {
        let p = participants.get();
        if p.is_empty() {
            "live call".to_string()
        } else {
            p.join(", ")
        }
    };

    view! {
        <Show when=move || active.get() fallback=|| ()>
            <section class="ocean-call" aria-label="live call" role="region">
                <header class="ocean-call__head">
                    <span class="ocean-call__live">
                        <span class="ocean-call__live-dot"></span>
                        "LIVE"
                    </span>
                    <span class="ocean-call__title">{participant_label}</span>
                    // Wake orb: pulses on each wake trigger. Keyed off the
                    // wake_pulse counter so back-to-back wakes re-fire the flash.
                    <span class="ocean-call__wake" class:is-awake=move || has_wake()>
                        {move || {
                            // Touch wake_pulse so the element re-renders (and the
                            // pulse animation restarts) on every trigger.
                            let _ = wake_pulse.get();
                            view! { <span class="ocean-call__wake-orb"></span> }
                        }}
                        <Show when=has_wake fallback=|| ()>
                            <span class="ocean-call__wake-text">
                                {move || wake_text.get()}
                            </span>
                        </Show>
                    </span>
                </header>

                // Rolling auto-summary strip.
                <Show when=has_summary fallback=|| ()>
                    <div class="ocean-call__summary">
                        <span class="ocean-call__summary-label">"Summary"</span>
                        <span class="ocean-call__summary-text">{move || summary.get()}</span>
                    </div>
                </Show>

                // Detected action-item chips (detect-and-notify only).
                <Show when=has_tasks fallback=|| ()>
                    <div class="ocean-call__tasks" aria-label="detected action items">
                        <For
                            each=move || tasks.get()
                            key=|t| t.task_id.clone()
                            children=move |t| {
                                let meta = match (&t.assignee, &t.due) {
                                    (Some(a), Some(d)) => format!("{a} · {d}"),
                                    (Some(a), None) => a.clone(),
                                    (None, Some(d)) => d.clone(),
                                    (None, None) => String::new(),
                                };
                                let has_meta = !meta.is_empty();
                                view! {
                                    <span class="ocean-call__chip" title=t.title.clone()>
                                        <span class="ocean-call__chip-glyph">"✓"</span>
                                        <span class="ocean-call__chip-title">{t.title.clone()}</span>
                                        <Show when=move || has_meta fallback=|| ()>
                                            <span class="ocean-call__chip-meta">{meta.clone()}</span>
                                        </Show>
                                    </span>
                                }
                            }
                        />
                    </div>
                </Show>

                // Scrolling transcript. Interim (non-final) segments render
                // greyed + italic; final segments solid; Ocean's replies styled
                // distinctly and labelled as Ocean.
                <div class="ocean-call__transcript" aria-label="call transcript" aria-live="polite">
                    <Show
                        when=move || !lines.get().is_empty()
                        fallback=|| view! {
                            <div class="ocean-call__waiting">"listening…"</div>
                        }
                    >
                        <For
                            each=move || lines.get()
                            key=|l| (l.is_agent, l.start_ms, l.speaker.clone())
                            children=move |l| {
                                let row_class = if l.is_agent {
                                    "ocean-call__row ocean-call__row--agent"
                                } else if l.is_final {
                                    "ocean-call__row"
                                } else {
                                    "ocean-call__row ocean-call__row--interim"
                                };
                                view! {
                                    <div class=row_class>
                                        <span class="ocean-call__speaker">{l.speaker.clone()}</span>
                                        <span class="ocean-call__text">{l.text.clone()}</span>
                                    </div>
                                }
                            }
                        />
                    </Show>
                </div>
            </section>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flattened envelope on `/v1/events` (id/at/session_id + the call
    /// fields) must deserialize into the right variant via the `type` tag, with
    /// the envelope's extra fields ignored.
    #[test]
    fn transcript_segment_envelope_deserializes() {
        let wire = r#"{
            "id":"evt-1","at":"2026-06-08T00:00:00Z","session_id":"sess-1",
            "type":"call_transcript_segment","speaker":"caller",
            "text":"hello there","start_ms":1200,"final":false
        }"#;
        let evt: CallEvent = serde_json::from_str(wire).expect("deserialize");
        match evt {
            CallEvent::CallTranscriptSegment {
                speaker,
                text,
                start_ms,
                is_final,
            } => {
                assert_eq!(speaker, "caller");
                assert_eq!(text, "hello there");
                assert_eq!(start_ms, 1200);
                assert!(!is_final);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// `call_started` flips the panel active and seeds participants; `call_ended`
    /// collapses it. The whole panel visibility hinges on `active`.
    #[test]
    fn started_then_ended_toggles_active() {
        let state = CallState::new();
        assert!(!state.active.get_untracked());
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c1".into(),
                room_id: "call:abc".into(),
                participants: vec!["+15551234".into()],
            },
        );
        assert!(state.active.get_untracked());
        assert_eq!(state.participants.get_untracked(), vec!["+15551234"]);
        apply_call_event(
            &state,
            CallEvent::CallEnded {
                call_id: "c1".into(),
                duration_ms: 42_000,
            },
        );
        assert!(!state.active.get_untracked());
    }

    /// A non-final segment revised in place (same start_ms + speaker) replaces
    /// rather than appends, then goes final without duplicating.
    #[test]
    fn interim_segment_revises_in_place() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "hel".into(),
                start_ms: 1000,
                is_final: false,
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "hello world".into(),
                start_ms: 1000,
                is_final: true,
            },
        );
        let lines = state.lines.get_untracked();
        assert_eq!(lines.len(), 1, "revision must not duplicate the line");
        assert_eq!(lines[0].text, "hello world");
        assert!(lines[0].is_final);
    }

    /// Ocean's spoken reply appends as an agent line after the caller segments.
    #[test]
    fn agent_spoke_appends_after_segments() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "are you there".into(),
                start_ms: 500,
                is_final: true,
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallAgentSpoke {
                text: "I'm here.".into(),
            },
        );
        let lines = state.lines.get_untracked();
        assert_eq!(lines.len(), 2);
        let agent = lines.last().unwrap();
        assert!(agent.is_agent);
        assert_eq!(agent.speaker, "Ocean");
        assert_eq!(agent.text, "I'm here.");
    }

    /// Detected tasks dedupe by id; the wake counter bumps on each trigger.
    #[test]
    fn tasks_dedupe_and_wake_bumps() {
        let state = CallState::new();
        for _ in 0..2 {
            apply_call_event(
                &state,
                CallEvent::CallTaskDetected {
                    task_id: "t1".into(),
                    title: "Send the deck".into(),
                    assignee: Some("John".into()),
                    due: None,
                    source_quote: None,
                    confidence: 0.9,
                },
            );
        }
        assert_eq!(state.tasks.get_untracked().len(), 1, "same task id dedupes");

        let before = state.wake_pulse.get_untracked();
        apply_call_event(
            &state,
            CallEvent::CallWakeTriggered {
                utterance: "hey Ocean, summarize".into(),
            },
        );
        assert_eq!(state.wake_pulse.get_untracked(), before + 1);
        assert_eq!(state.wake_text.get_untracked(), "hey Ocean, summarize");
    }

    /// A fresh `call_started` resets stale transcript/summary from a prior call.
    #[test]
    fn second_call_resets_state() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c1".into(),
                room_id: "r1".into(),
                participants: vec![],
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallSummaryUpdated {
                summary: "first call".into(),
                as_of_ms: 1,
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallEnded {
                call_id: "c1".into(),
                duration_ms: 1,
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c2".into(),
                room_id: "r2".into(),
                participants: vec![],
            },
        );
        assert!(state.summary.get_untracked().is_empty(), "summary reset");
        assert!(state.lines.get_untracked().is_empty(), "transcript reset");
    }
}
