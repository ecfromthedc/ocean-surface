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

/// The CSS class for a transcript row, driven purely by the line's lane +
/// finality. This is the single source of truth for the "feels live" styling:
/// interim (non-final caller) segments get `--interim` (greyed/italic via CSS)
/// and are promoted to the solid base class the instant `is_final` flips; Ocean
/// replies get `--agent`. Factored out of the view's `For` so the promotion
/// rule is unit-testable without a DOM.
fn row_class(line: &TranscriptLine) -> &'static str {
    if line.is_agent {
        "ocean-call__row ocean-call__row--agent"
    } else if line.is_final {
        "ocean-call__row"
    } else {
        "ocean-call__row ocean-call__row--interim"
    }
}

/// The display label for a transcript line's speaker. Ocean's own replies are
/// always "Ocean"; caller-side speakers arrive as raw STT/diarization tokens
/// (e.g. `caller`, `agent_human`, `speaker_1`) which we humanize for the label —
/// underscores to spaces, title-cased — so the transcript reads like a call
/// thread, not a debug stream. Empty/unknown speakers fall back to "Caller".
fn speaker_label(line: &TranscriptLine) -> String {
    if line.is_agent {
        return "Ocean".to_string();
    }
    let raw = line.speaker.trim();
    if raw.is_empty() {
        return "Caller".to_string();
    }
    raw.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A detected action-item chip.
#[derive(Debug, Clone, PartialEq)]
struct TaskChip {
    task_id: String,
    title: String,
    assignee: Option<String>,
    due: Option<String>,
}

/// The call's connection/turn state, derived purely from the `call_*` frame
/// stream (no extra wire signal needed). Drives the header status pill so the
/// operator always knows what's happening — dialing, who has the floor, whether
/// a barge-in just landed, or that the call wrapped. Ordered loosely by
/// lifecycle; `Interrupted` is the transient barge-in beat between Ocean
/// speaking and the human retaking the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallPhase {
    /// `call_started` landed but no transcript/summary/audio signal yet — the
    /// bridge is connecting and we're waiting on the first frame.
    Connecting,
    /// Live and listening to the caller (the default "floor is the human's"
    /// state). Entered on the first real signal and whenever Ocean finishes.
    Listening,
    /// Ocean currently holds the floor (TTS playing) — set on `call_agent_spoke`.
    OceanSpeaking,
    /// A barge-in just fired: the wake word hit *while Ocean was speaking*, so
    /// Ocean's TTS is cut and the floor snaps back to the human. Transient — the
    /// view shows an "interrupted" beat, then it settles to `Listening`.
    Interrupted,
    /// `call_ended` — the panel is collapsing/archiving.
    Ended,
}

impl CallPhase {
    /// Short, human label for the status pill.
    fn label(self) -> &'static str {
        match self {
            CallPhase::Connecting => "Connecting",
            CallPhase::Listening => "Listening",
            CallPhase::OceanSpeaking => "Ocean speaking",
            CallPhase::Interrupted => "Interrupted",
            CallPhase::Ended => "Ended",
        }
    }

    /// CSS modifier suffix so the pill can colour each state distinctly
    /// (connecting=neutral, listening=accent, speaking=gold, interrupted=warn).
    fn css(self) -> &'static str {
        match self {
            CallPhase::Connecting => "connecting",
            CallPhase::Listening => "listening",
            CallPhase::OceanSpeaking => "speaking",
            CallPhase::Interrupted => "interrupted",
            CallPhase::Ended => "ended",
        }
    }
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
    /// The derived connection/turn state for the status pill (OCEAN-284).
    phase: RwSignal<CallPhase>,
    /// Bumped only when a wake trigger lands *while Ocean was speaking* — i.e. a
    /// real barge-in (OCEAN-243). The header keys a distinct "interrupted" flash
    /// off this so a barge-in reads differently from an idle wake-word pulse.
    barge_pulse: RwSignal<u64>,
    /// Bumped on every `call_summary_updated` so the summary strip can flash a
    /// brief "updated" beat — the rolling summary visibly refreshing rather than
    /// silently swapping text.
    summary_rev: RwSignal<u64>,
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
            phase: RwSignal::new(CallPhase::Connecting),
            barge_pulse: RwSignal::new(0),
            summary_rev: RwSignal::new(0),
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
        self.phase.set(CallPhase::Connecting);
        self.summary_rev.set(0);
        // Note: barge_pulse intentionally NOT reset — it's a monotonic flash
        // counter the view only diffs, never reads absolutely, so carrying it
        // across calls is harmless and avoids a spurious flash on re-open.
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
            // Any caller speech means the floor is the human's: leave the
            // `Connecting` waiting-state and, if Ocean had been speaking, hand
            // the floor back to `Listening`. (A barge-in proper is signalled by
            // `call_wake_triggered`; this just keeps the pill honest when the
            // caller simply talks.)
            state.phase.update(|p| {
                if matches!(p, CallPhase::Connecting | CallPhase::OceanSpeaking) {
                    *p = CallPhase::Listening;
                }
            });
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
            // A summary update is live signal too — clear the connecting beat.
            state.phase.update(|p| {
                if *p == CallPhase::Connecting {
                    *p = CallPhase::Listening;
                }
            });
            // Flash the strip so the rolling summary visibly refreshes.
            state.summary_rev.update(|n| *n = n.wrapping_add(1));
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
            // Barge-in (OCEAN-243): if the wake word fired *while Ocean was
            // speaking*, the human cut Ocean's TTS — surface that distinctly
            // (a flash + an "Interrupted" pill beat) before the floor settles
            // back to the caller. A wake while already listening is just a
            // normal wake — orb pulse only, no interrupt beat.
            let was_speaking = state.phase.get_untracked() == CallPhase::OceanSpeaking;
            if was_speaking {
                state.barge_pulse.update(|n| *n = n.wrapping_add(1));
                state.phase.set(CallPhase::Interrupted);
            } else {
                // Floor is the human's now regardless; keep the pill truthful.
                state.phase.update(|p| {
                    if *p == CallPhase::Connecting {
                        *p = CallPhase::Listening;
                    }
                });
            }
        }
        CallEvent::CallAgentSpoke { text } => {
            // Ocean's reply appends after everything heard so far. Use a key past
            // the current max segment time plus a monotonic bump so successive
            // replies keep their order.
            // Ocean has the floor now (TTS playing) — drives the status pill.
            state.phase.set(CallPhase::OceanSpeaking);
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
            state.phase.set(CallPhase::Ended);
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
        phase,
        barge_pulse,
        summary_rev,
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
    // Connection-state pill text + per-state colour class (OCEAN-284).
    let phase_label = move || phase.get().label();
    let phase_css = move || format!("ocean-call__phase ocean-call__phase--{}", phase.get().css());
    // True only on the transient barge-in beat — drives the header's
    // "Ocean interrupted" cue distinctly from an idle wake-word pulse.
    let is_interrupted = move || phase.get() == CallPhase::Interrupted;

    view! {
        <Show when=move || active.get() fallback=|| ()>
            <section class="ocean-call" aria-label="live call" role="region">
                <header class="ocean-call__head">
                    <span class="ocean-call__live">
                        <span class="ocean-call__live-dot"></span>
                        "LIVE"
                    </span>
                    <span class="ocean-call__title">{participant_label}</span>
                    // Connection-state pill (OCEAN-284): dialing / listening /
                    // Ocean speaking / interrupted / ended, derived from the
                    // event stream so the operator always knows the call's state.
                    <span class=phase_css aria-live="polite">
                        <span class="ocean-call__phase-dot"></span>
                        {phase_label}
                    </span>
                    // Wake orb: pulses on each wake trigger. Keyed off the
                    // wake_pulse counter so back-to-back wakes re-fire the flash.
                    <span
                        class="ocean-call__wake"
                        class:is-awake=move || has_wake()
                        class:is-barge=is_interrupted
                    >
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

                // Barge-in banner (OCEAN-243 → 284): a brief, unmistakable beat
                // when the human cuts Ocean off. Keyed off barge_pulse so it
                // re-fires its flash on every real barge-in, and gated on the
                // transient Interrupted phase so it shows only in that moment.
                <Show when=is_interrupted fallback=|| ()>
                    <div class="ocean-call__barge" role="status">
                        {move || {
                            let _ = barge_pulse.get();
                            view! {
                                <span class="ocean-call__barge-pulse"></span>
                                <span class="ocean-call__barge-text">
                                    "Ocean interrupted — listening"
                                </span>
                            }
                        }}
                    </div>
                </Show>

                // Rolling auto-summary strip. Flashes briefly each time the
                // summary updates (keyed off summary_rev) so the rolling refresh
                // is visible, not a silent text swap.
                <Show when=has_summary fallback=|| ()>
                    {move || {
                        let _ = summary_rev.get();
                        view! {
                            <div class="ocean-call__summary ocean-call__summary--bump">
                                <span class="ocean-call__summary-label">"Summary"</span>
                                <span class="ocean-call__summary-text">{move || summary.get()}</span>
                            </div>
                        }
                    }}
                </Show>

                // Detected action-item chips (detect-and-notify only). A small
                // header labels the group as action items so the chips read as
                // captured to-dos, not raw transcript fragments.
                <Show when=has_tasks fallback=|| ()>
                    <div class="ocean-call__tasks" aria-label="detected action items">
                        <span class="ocean-call__tasks-label">"Action items"</span>
                        <div class="ocean-call__chips">
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
                                    // A11y title: include the meta so hover/SR
                                    // gets assignee+due, not just the bare title.
                                    let chip_title = if has_meta {
                                        format!("{} ({meta})", t.title)
                                    } else {
                                        t.title.clone()
                                    };
                                    view! {
                                        <span class="ocean-call__chip" title=chip_title>
                                            // Inline check glyph (game-icons style
                                            // inline SVG — no emoji/font glyph).
                                            <svg
                                                class="ocean-call__chip-glyph"
                                                viewBox="0 0 24 24" width="1em" height="1em"
                                                fill="none" stroke="currentColor" stroke-width="2.5"
                                                stroke-linecap="round" stroke-linejoin="round"
                                                aria-hidden="true"
                                            >
                                                <path d="M20 6 9 17l-5-5" />
                                            </svg>
                                            <span class="ocean-call__chip-title">{t.title.clone()}</span>
                                            <Show when=move || has_meta fallback=|| ()>
                                                <span class="ocean-call__chip-meta">{meta.clone()}</span>
                                            </Show>
                                        </span>
                                    }
                                }
                            />
                        </div>
                    </div>
                </Show>

                // Scrolling transcript, styled like a real call thread rather
                // than a debug log (OCEAN-284): caller turns sit left in neutral
                // bubbles, Ocean's replies sit right in accent bubbles, each
                // clearly speaker-labelled. Interim (non-final) caller segments
                // render greyed + italic until the streaming STT finalizes them.
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
                                let row_class = row_class(&l);
                                let speaker = speaker_label(&l);
                                view! {
                                    <div class=row_class>
                                        <span class="ocean-call__speaker">{speaker}</span>
                                        <span class="ocean-call__bubble">
                                            <span class="ocean-call__text">{l.text.clone()}</span>
                                        </span>
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

    /// OCEAN-247 core: the live, multi-revision interim→final flow exactly as the
    /// streaming STT (Deepgram, #173) actually emits it — a *run* of growing
    /// interim hypotheses all carrying the same stable `start_ms`, then a final
    /// at that same `start_ms`. Every revision must collapse onto the one line
    /// (in-place, keyed by start_ms+speaker), the visible text must always be the
    /// latest hypothesis, and the line stays interim until the final lands. This
    /// is the "feels live" transcript the ticket is about; the older
    /// `interim_segment_revises_in_place` only covered a single interim.
    #[test]
    fn streaming_interim_run_revises_then_promotes_to_final() {
        let state = CallState::new();
        // Deepgram re-emits the same utterance with growing text at a fixed start.
        let revisions = ["let's", "let's ship", "let's ship it"];
        for text in revisions {
            apply_call_event(
                &state,
                CallEvent::CallTranscriptSegment {
                    speaker: "caller".into(),
                    text: text.into(),
                    start_ms: 100,
                    is_final: false,
                },
            );
            let lines = state.lines.get_untracked();
            assert_eq!(lines.len(), 1, "interim run must stay one line, not stack");
            assert_eq!(lines[0].text, text, "shows the latest hypothesis");
            assert!(!lines[0].is_final, "stays interim mid-revision");
            assert_eq!(
                row_class(&lines[0]),
                "ocean-call__row ocean-call__row--interim",
                "interim renders greyed/italic",
            );
        }
        // The final settles it at the same start_ms.
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "let's ship it friday".into(),
                start_ms: 100,
                is_final: true,
            },
        );
        let lines = state.lines.get_untracked();
        assert_eq!(lines.len(), 1, "final must replace the interim, not append");
        assert_eq!(lines[0].text, "let's ship it friday");
        assert!(lines[0].is_final, "promoted to final");
        assert_eq!(
            row_class(&lines[0]),
            "ocean-call__row",
            "promotion drops the interim class → solid styling",
        );
    }

    /// The row-class promotion rule in isolation: a single `TranscriptLine`
    /// renders interim (greyed/italic) while non-final, solid once final, and the
    /// agent style for Ocean's replies regardless of finality. This is the exact
    /// function the view's `For` uses, so it pins the rendered output.
    #[test]
    fn row_class_reflects_lane_and_finality() {
        let mk = |is_final, is_agent| TranscriptLine {
            speaker: "caller".into(),
            text: "x".into(),
            start_ms: 0,
            is_final,
            is_agent,
        };
        assert_eq!(
            row_class(&mk(false, false)),
            "ocean-call__row ocean-call__row--interim",
        );
        assert_eq!(row_class(&mk(true, false)), "ocean-call__row");
        // Agent replies are always agent-styled (and always arrive final).
        assert_eq!(
            row_class(&mk(true, true)),
            "ocean-call__row ocean-call__row--agent",
        );
    }

    /// An interim and its later final, fed as the literal flattened-envelope JSON
    /// frames the daemon broadcasts on `/v1/events` (`type` tag + the `final`
    /// rename + envelope id/at/session_id), must deserialize and drive the
    /// interim→final promotion end-to-end — proving the wire contract from the
    /// streaming STT lines up with what the panel parses, with no translation.
    #[test]
    fn interim_then_final_wire_frames_drive_promotion() {
        let state = CallState::new();
        let interim = r#"{
            "id":"evt-1","at":"2026-06-08T00:00:00Z","session_id":"sess-1",
            "type":"call_transcript_segment","speaker":"caller",
            "text":"hello wor","start_ms":1200,"final":false
        }"#;
        let final_ = r#"{
            "id":"evt-2","at":"2026-06-08T00:00:01Z","session_id":"sess-1",
            "type":"call_transcript_segment","speaker":"caller",
            "text":"hello world","start_ms":1200,"final":true
        }"#;
        for wire in [interim, final_] {
            let evt: CallEvent = serde_json::from_str(wire).expect("deserialize frame");
            apply_call_event(&state, evt);
        }
        let lines = state.lines.get_untracked();
        assert_eq!(lines.len(), 1, "same start_ms → one promoted line");
        assert_eq!(lines[0].text, "hello world");
        assert!(lines[0].is_final);
        assert_eq!(row_class(&lines[0]), "ocean-call__row");
    }

    /// Two speakers talking over each other: each keeps its own interim line
    /// keyed by (start_ms, speaker), so one speaker's revision never clobbers the
    /// other's. Mirrors diarized streaming output where interims interleave.
    #[test]
    fn interims_from_distinct_speakers_do_not_collide() {
        let state = CallState::new();
        // Same start_ms but different speakers must be two independent lines.
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "yeah".into(),
                start_ms: 300,
                is_final: false,
            },
        );
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "agent_human".into(),
                text: "right".into(),
                start_ms: 300,
                is_final: false,
            },
        );
        // Revise the first speaker's interim — must not touch the second.
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "yeah exactly".into(),
                start_ms: 300,
                is_final: true,
            },
        );
        let lines = state.lines.get_untracked();
        assert_eq!(lines.len(), 2, "distinct speakers stay distinct lines");
        let caller = lines.iter().find(|l| l.speaker == "caller").unwrap();
        let other = lines.iter().find(|l| l.speaker == "agent_human").unwrap();
        assert_eq!(caller.text, "yeah exactly");
        assert!(caller.is_final);
        assert_eq!(other.text, "right", "other speaker's interim untouched");
        assert!(!other.is_final);
    }

    // ── OCEAN-284: connection-state, barge-in, speaker labels ──────────────

    /// The status pill walks the call lifecycle from the frame stream alone:
    /// started→Connecting, first real signal→Listening, Ocean→OceanSpeaking,
    /// caller resumes→Listening, ended→Ended. This is the "always know what's
    /// happening" cue, so it must track the events with no extra wire signal.
    #[test]
    fn phase_tracks_call_lifecycle() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c1".into(),
                room_id: "r1".into(),
                participants: vec![],
            },
        );
        assert_eq!(
            state.phase.get_untracked(),
            CallPhase::Connecting,
            "started but no signal yet → connecting",
        );
        // First caller speech = live signal, floor is the human's.
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "hi".into(),
                start_ms: 10,
                is_final: false,
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::Listening);
        // Ocean takes the floor.
        apply_call_event(&state, CallEvent::CallAgentSpoke { text: "Hello.".into() });
        assert_eq!(state.phase.get_untracked(), CallPhase::OceanSpeaking);
        // Caller talks again (no wake word) → floor returns to listening.
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "thanks".into(),
                start_ms: 20,
                is_final: true,
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::Listening);
        apply_call_event(
            &state,
            CallEvent::CallEnded {
                call_id: "c1".into(),
                duration_ms: 1,
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::Ended);
    }

    /// The marquee barge-in beat (OCEAN-243): a wake trigger that lands *while
    /// Ocean is speaking* is a real interruption — it bumps `barge_pulse` and
    /// flips the phase to `Interrupted`. A wake while merely listening is NOT a
    /// barge-in: it pulses the orb but leaves `barge_pulse` and the phase alone.
    #[test]
    fn wake_while_speaking_is_a_barge_in() {
        let state = CallState::new();
        // Ocean has the floor.
        apply_call_event(&state, CallEvent::CallAgentSpoke { text: "Let me…".into() });
        assert_eq!(state.phase.get_untracked(), CallPhase::OceanSpeaking);
        let barge_before = state.barge_pulse.get_untracked();
        // Human cuts in.
        apply_call_event(
            &state,
            CallEvent::CallWakeTriggered {
                utterance: "stop".into(),
            },
        );
        assert_eq!(
            state.barge_pulse.get_untracked(),
            barge_before + 1,
            "wake-during-speech bumps the barge flash",
        );
        assert_eq!(
            state.phase.get_untracked(),
            CallPhase::Interrupted,
            "barge-in snaps the pill to Interrupted",
        );
    }

    /// A wake word while the human already has the floor is a normal wake, not a
    /// barge-in: the orb pulse fires (wake_pulse) but the barge flash does not,
    /// and the phase stays Listening — so the "interrupted" cue is reserved for
    /// genuine interruptions of Ocean.
    #[test]
    fn wake_while_listening_is_not_a_barge_in() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallTranscriptSegment {
                speaker: "caller".into(),
                text: "ok".into(),
                start_ms: 1,
                is_final: true,
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::Listening);
        let wake_before = state.wake_pulse.get_untracked();
        let barge_before = state.barge_pulse.get_untracked();
        apply_call_event(
            &state,
            CallEvent::CallWakeTriggered {
                utterance: "hey Ocean".into(),
            },
        );
        assert_eq!(
            state.wake_pulse.get_untracked(),
            wake_before + 1,
            "the orb still pulses on any wake",
        );
        assert_eq!(
            state.barge_pulse.get_untracked(),
            barge_before,
            "no barge flash when nothing was interrupted",
        );
        assert_eq!(
            state.phase.get_untracked(),
            CallPhase::Listening,
            "stays listening — not an interruption",
        );
    }

    /// Each `call_summary_updated` bumps `summary_rev` so the strip can flash its
    /// "updated" beat, and the first summary also clears the connecting state.
    #[test]
    fn summary_update_bumps_rev_and_clears_connecting() {
        let state = CallState::new();
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c1".into(),
                room_id: "r1".into(),
                participants: vec![],
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::Connecting);
        let rev0 = state.summary_rev.get_untracked();
        apply_call_event(
            &state,
            CallEvent::CallSummaryUpdated {
                summary: "Caller wants a refund.".into(),
                as_of_ms: 100,
            },
        );
        assert_eq!(state.summary_rev.get_untracked(), rev0 + 1, "rev bumps");
        assert_eq!(
            state.phase.get_untracked(),
            CallPhase::Listening,
            "a summary is live signal too",
        );
        apply_call_event(
            &state,
            CallEvent::CallSummaryUpdated {
                summary: "Caller wants a refund on order 12.".into(),
                as_of_ms: 200,
            },
        );
        assert_eq!(state.summary_rev.get_untracked(), rev0 + 2, "bumps each update");
    }

    /// Speaker labels humanize raw STT/diarization tokens for the transcript:
    /// Ocean's replies are always "Ocean"; caller tokens get underscores/dashes
    /// turned to spaces and title-cased; an empty speaker falls back to "Caller".
    #[test]
    fn speaker_label_humanizes_tokens() {
        let line = |speaker: &str, is_agent: bool| TranscriptLine {
            speaker: speaker.into(),
            text: "x".into(),
            start_ms: 0,
            is_final: true,
            is_agent,
        };
        assert_eq!(speaker_label(&line("caller", false)), "Caller");
        assert_eq!(speaker_label(&line("agent_human", false)), "Agent Human");
        assert_eq!(speaker_label(&line("speaker-1", false)), "Speaker 1");
        assert_eq!(speaker_label(&line("", false)), "Caller", "empty → Caller");
        // Agent lines are always "Ocean" regardless of the stored speaker.
        assert_eq!(speaker_label(&line("whatever", true)), "Ocean");
    }

    /// A fresh `call_started` resets the phase back to Connecting and the summary
    /// rev to 0 so a second call doesn't inherit the prior call's state pill.
    #[test]
    fn second_call_resets_phase_and_summary_rev() {
        let state = CallState::new();
        apply_call_event(&state, CallEvent::CallAgentSpoke { text: "hi".into() });
        apply_call_event(
            &state,
            CallEvent::CallSummaryUpdated {
                summary: "first".into(),
                as_of_ms: 1,
            },
        );
        assert_eq!(state.phase.get_untracked(), CallPhase::OceanSpeaking);
        assert!(state.summary_rev.get_untracked() > 0);
        apply_call_event(
            &state,
            CallEvent::CallStarted {
                call_id: "c2".into(),
                room_id: "r2".into(),
                participants: vec![],
            },
        );
        assert_eq!(
            state.phase.get_untracked(),
            CallPhase::Connecting,
            "new call → connecting",
        );
        assert_eq!(state.summary_rev.get_untracked(), 0, "summary rev reset");
    }
}
