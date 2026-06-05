//! Top-level app shell. Owns the Daemon, mounts the transcript + composer.

use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::ToolDrawer;
use crate::daemon::{Daemon, DEFAULT_DAEMON_URL};
use crate::gauntlet::Gauntlet;
use crate::icons::{SoundOff, SoundOn, WaveLogo};
use crate::model::{Block, Role, Turn};
use crate::sessions::SessionsPanel;
use crate::transcript::Transcript;
use crate::voice::VoiceOrb;

#[component]
pub fn App() -> impl IntoView {
    let daemon = Daemon::new(daemon_url_from_env());
    // Zero-config boot: fetch /api/config from the same-origin proxy to learn
    // the daemon URL + confirm auth is preconfigured, THEN connect AND fetch the
    // model catalogue — in that order, inside bootstrap. Falls back to
    // daemon_url_from_env() if no proxy answers.
    //
    // Do NOT add an eager daemon.fetch_models() (or any url-dependent call)
    // here: it would run before bootstrap learns the real origin, succeed by
    // luck on localhost, and silently fail from ocean.risingtidesviral.com
    // (wrong URL → empty model picker). Any startup fetch that needs the daemon
    // URL belongs INSIDE bootstrap_then_connect, after url.set().
    daemon.bootstrap_then_connect();

    let input = RwSignal::new(String::new());
    let textarea_ref: NodeRef<leptos::html::Textarea> = NodeRef::new();

    // Daemon holds only Copy signal handles, so cloning per-closure is cheap
    // and avoids fighting the borrow checker over a single moved value.
    let status = daemon.status;
    let turns = daemon.turns;
    let streaming = daemon.streaming;
    let voice_ready = daemon.voice_ready;
    let last_turn_tokens = daemon.last_turn_tokens;
    let session_tokens = daemon.session_tokens;
    let model = daemon.model;
    let models = daemon.models;
    let project = daemon.project;
    let projects = daemon.projects;
    // Active session identity, shown in the header so the user always knows
    // which session is live and where it's anchored.
    let session_id = daemon.session_id;
    let session_title = daemon.session_title;
    let cwd = daemon.cwd;
    let has_session = move || session_id.get().is_some();
    let active_session_label = move || {
        let title = session_title.get();
        if title.trim().is_empty() {
            "untitled session".to_string()
        } else {
            title
        }
    };
    // Predicates pulled out of the view! macro: a bare `>` inside an attribute
    // expression would be parsed as the element's closing bracket.
    let has_tokens = move || session_tokens.get().total() > 0;
    let has_rate = move || {
        last_turn_tokens
            .get()
            .map(|t| t.tokens_per_second > 0.0)
            .unwrap_or(false)
    };

    // Show the Leptos gauntlet instead of the chat surface.
    let show_gauntlet = RwSignal::new(false);
    // Sessions panel overlay.
    let show_sessions = RwSignal::new(false);

    // TTS: speak the assistant's final text each time a turn finishes
    // (streaming flips true→false). Gated by `muted`. We track the previous
    // streaming value so we only fire on the falling edge, and remember the
    // last spoken turn so re-renders don't double-speak.
    let muted = RwSignal::new(false);
    let prev_streaming = RwSignal::new(false);
    let last_spoken: RwSignal<Option<String>> = RwSignal::new(None);
    Effect::new(move |_| {
        let now = streaming.get();
        let was = prev_streaming.get_untracked();
        prev_streaming.set(now);
        // Falling edge = a turn just completed.
        if was && !now {
            if let Some((id, text)) = latest_assistant_text(&turns.get_untracked()) {
                if last_spoken.get_untracked().as_deref() != Some(id.as_str()) {
                    last_spoken.set(Some(id));
                    crate::tts::speak(text, muted);
                }
            }
        }
    });

    let submit = {
        let daemon = daemon.clone();
        move |ev: SubmitEvent| {
            ev.prevent_default();
            let text = input.get_untracked();
            if text.trim().is_empty() {
                return;
            }
            input.set(String::new());
            daemon.send_prompt(text);
            // Refocus the textarea so successive prompts feel snappy.
            if let Some(el) = textarea_ref.get_untracked() {
                let _ = el.focus();
            }
        }
    };

    // Wrap submit in a StoredValue so it can be shared across closures
    // without being consumed (the gauntlet <Show> fallback closure needs it).
    let submit = StoredValue::new(submit);

    // Tool drawer: concealed strip that drops down to show recent tool calls.
    let tool_drawer_open = RwSignal::new(false);

    // Clone reserved for the SessionsPanel (the gauntlet <Show> fallback
    // moves the main `daemon` into its closure).
    let daemon_for_panel = daemon.clone();

    // Voice → text: drop the transcript into the composer and submit it,
    // reusing the exact same send path as typing.
    let on_transcript = {
        let daemon = daemon.clone();
        Callback::new(move |text: String| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            input.set(text.clone());
            daemon.send_prompt(text);
            input.set(String::new());
        })
    };
    let on_voice_status = Callback::new(move |msg: String| status.set(msg));

    // Clone for the header model picker's on:change.
    let daemon_model = daemon.clone();
    // Clone for the header project picker's on:change.
    let daemon_project = daemon.clone();
    // StoredValue is Copy, so the halt button's closure (inside the chat-branch
    // <Show> fallback, which must be Fn) can grab the daemon without the
    // fallback moving a plain clone out of its environment.
    let daemon_halt = StoredValue::new(daemon.clone());

    // In the Chrome side panel the cockpit lives in a ~360px-wide column. Tag
    // the root so the shared stylesheet's compact `.ocean-surface--extension`
    // rules apply, without forking the layout for the full-width web app.
    let root_class = if crate::daemon::running_as_extension() {
        "ocean-surface ocean-surface--extension"
    } else {
        "ocean-surface"
    };

    view! {
        <main class=root_class>
            <header class="ocean-header">
                <div class="ocean-brand">
                    <span class="ocean-brand__logo"><WaveLogo /></span>
                    <span class="ocean-brand__name">"Ocean"</span>
                </div>
                <div class="ocean-header__right">
                    // Project picker: selects which project (directory-bound
                    // workspace) turns run in. Purely client-side — the choice
                    // rides on every turn's project_id so the daemon binds to
                    // that project's workspace_root instead of its launch dir.
                    <select
                        class="ocean-project"
                        aria-label="project"
                        title="Project"
                        prop:value=move || project.get().unwrap_or_default()
                        on:change=move |ev| {
                            let id = event_target_value(&ev);
                            daemon_project.set_project((!id.is_empty()).then_some(id));
                        }
                    >
                        <option prop:value="" prop:selected=move || project.get().is_none()>
                            "no project"
                        </option>
                        <For
                            each=move || projects.get()
                            key=|p| p.id.clone()
                            children=move |p| {
                                let id = p.id.clone();
                                let id_sel = p.id.clone();
                                let label = if p.name.is_empty() { p.id.clone() } else { p.name.clone() };
                                view! {
                                    <option
                                        prop:value=id.clone()
                                        prop:selected=move || project.get().as_deref() == Some(id_sel.as_str())
                                    >
                                        {label}
                                    </option>
                                }
                            }
                        />
                    </select>
                    // Model picker: shows the live model and hot-swaps it on the
                    // daemon (POST /v1/model). Reflects mid-session swaps via the
                    // model signal (set from TurnStarted).
                    <select
                        class="ocean-model"
                        aria-label="model"
                        title="Model"
                        prop:value=move || model.get().unwrap_or_default()
                        on:change=move |ev| {
                            let id = event_target_value(&ev);
                            if !id.is_empty() {
                                daemon_model.set_model(id);
                            }
                        }
                    >
                        // If the current model isn't in the catalogue yet, still
                        // show it as the selected option.
                        <Show when=move || {
                            let cur = model.get();
                            cur.is_some()
                                && !models.get().iter().any(|m| Some(&m.id) == cur.as_ref())
                        }>
                            <option prop:value=move || model.get().unwrap_or_default() prop:selected=true>
                                {move || model.get().unwrap_or_default()}
                            </option>
                        </Show>
                        <For
                            each=move || models.get()
                            key=|m| m.id.clone()
                            children=move |m| {
                                let id = m.id.clone();
                                let id_sel = m.id.clone();
                                let label = if m.label.is_empty() { m.id.clone() } else { m.label.clone() };
                                view! {
                                    <option
                                        prop:value=id.clone()
                                        prop:selected=move || model.get().as_deref() == Some(id_sel.as_str())
                                    >
                                        {label}
                                    </option>
                                }
                            }
                        />
                    </select>
                    // Active session identity — title + workspace anchor. Click
                    // to open the sessions panel. Hidden until a session exists
                    // (lazy default flow shows nothing until the first prompt).
                    <Show when=has_session>
                        <button
                            class="ocean-active-session"
                            type="button"
                            aria-label="active session"
                            title=move || format!("Active session — {} · {}", active_session_label(), cwd.get())
                            on:click=move |_| show_sessions.update(|v| *v = !*v)
                        >
                            <span class="ocean-active-session__title">{active_session_label}</span>
                            <span class="ocean-active-session__cwd">{move || cwd.get()}</span>
                        </button>
                    </Show>
                    <button
                        class="ocean-sessions-btn"
                        type="button"
                        aria-label="sessions"
                        title="Sessions"
                        on:click=move |_| show_sessions.update(|v| *v = !*v)
                    >
                        "☰"
                    </button>
                    <button
                        class="ocean-gauntlet-btn"
                        type="button"
                        aria-label="open component gauntlet"
                        title="🧪 Leptos Gauntlet"
                        on:click=move |_| show_gauntlet.update(|v| *v = !*v)
                    >
                        {move || if show_gauntlet.get() { "✕" } else { "🧪" }}
                    </button>
                    // Token usage: session total, with a per-turn + cache
                    // breakdown on hover. Hidden until the first turn finishes.
                    <Show when=has_tokens>
                        <div
                            class="ocean-tokens"
                            title=move || {
                                let s = session_tokens.get();
                                let last = last_turn_tokens.get().unwrap_or_default();
                                format!(
                                    "Session — in {} · out {} · cache {} · total {}\nLast turn — in {} · out {} · {:.1} tok/s",
                                    s.input, s.output, s.cache_read, s.total(),
                                    last.input, last.output, last.tokens_per_second,
                                )
                            }
                        >
                            <span class="ocean-tokens__io">
                                {move || {
                                    let s = session_tokens.get();
                                    format!("↑{} ↓{}", fmt_tokens(s.input), fmt_tokens(s.output))
                                }}
                            </span>
                            <Show when=has_rate>
                                <span class="ocean-tokens__rate">
                                    {move || format!("{:.0} t/s", last_turn_tokens.get().unwrap_or_default().tokens_per_second)}
                                </span>
                            </Show>
                        </div>
                    </Show>
                    <div class="ocean-status">{move || status.get()}</div>
                    // Mute toggle only matters when TTS is available.
                    <Show when=move || voice_ready.get()>
                        <button
                            class="ocean-mute"
                            type="button"
                            aria-label="toggle speech"
                            class:is-muted=move || muted.get()
                            on:click=move |_| muted.update(|m| *m = !*m)
                        >
                                {move || if muted.get() {
                                view! { <SoundOff /> }.into_any()
                            } else {
                                view! { <SoundOn /> }.into_any()
                            }}
                        </button>
                    </Show>
                </div>
            </header>

            // Toggle between the gauntlet and normal chat surface.
            // Using <Show> so closures don't fight over ownership.
            <Show
                when=move || show_gauntlet.get()
                fallback=move || view! {
                    <>
                        <Transcript daemon=daemon.clone() />

                        <ToolDrawer turns=turns open=tool_drawer_open />

                        <form class="ocean-composer" on:submit=move |ev| submit.with_value(|s| s(ev))>
                            // Push-to-talk only when the proxy has a usable xAI key;
                            // otherwise a dim, disabled placeholder explains why.
                            <Show
                                when=move || voice_ready.get()
                                fallback=|| view! {
                                    <div class="voice-wrap">
                                        <button class="voice-orb is-disabled" type="button" disabled=true
                                                title="voice off — set xAI key in ~/.config/ocean-surface/xai.key">
                                            <span class="voice-orb__glyph"><crate::icons::Amplitude /></span>
                                        </button>
                                        <span class="voice-hint">"voice off"</span>
                                    </div>
                                }
                            >
                                <VoiceOrb on_transcript=on_transcript on_status=on_voice_status />
                            </Show>
                            <textarea
                                class="ocean-composer__input"
                                placeholder="message Ocean…"
                                rows="2"
                                node_ref=textarea_ref
                                prop:value=move || input.get()
                                on:input=move |ev| input.set(event_target_value(&ev))
                                on:keydown=move |ev| {
                                    // Enter to submit, Shift+Enter for newline.
                                    if ev.key() == "Enter" && !ev.shift_key() {
                                        ev.prevent_default();
                                        if let Some(target) = ev.target() {
                                            if let Ok(el) = target.dyn_into::<web_sys::HtmlElement>() {
                                                if let Ok(Some(form)) = el.closest("form") {
                                                    if let Ok(form) = form.dyn_into::<web_sys::HtmlFormElement>()
                                                    {
                                                        let _ = form.request_submit();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            />
                            // Halt the in-flight turn. Only shown while streaming.
                            <Show when=move || streaming.get()>
                                <button
                                    class="ocean-composer__halt"
                                    type="button"
                                    aria-label="stop"
                                    title="Stop the running turn"
                                    on:click=move |_| daemon_halt.with_value(|d| d.halt())
                                >
                                    "■ Stop"
                                </button>
                            </Show>
                            <button
                                class="ocean-composer__send"
                                type="submit"
                                disabled=move || input.get().trim().is_empty()
                            >
                                "Send"
                            </button>
                        </form>
                    </>
                }
            >
                <Gauntlet />
            </Show>

            <SessionsPanel daemon=daemon_for_panel open=show_sessions />
        </main>
    }
}

/// Pull the most recent assistant turn's concatenated text blocks, paired
/// with its turn id (used to dedupe TTS). Skips thinking + tool output.
fn latest_assistant_text(turns: &[Turn]) -> Option<(String, String)> {
    let turn = turns.iter().rev().find(|t| t.role == Role::Assistant)?;
    let id = turn.turn_id.clone()?;
    let mut text = String::new();
    for block in &turn.blocks {
        if let Block::Text(buf) = block {
            text.push_str(buf);
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some((id, text))
    }
}

/// Resolve the daemon URL. Browser defaults to the same host on :4780;
/// the Tauri shell can override via env at build time.
fn daemon_url_from_env() -> String {
    // Compile-time override (Tauri builds can set OCEAN_DAEMON_URL).
    if let Some(url) = option_env!("OCEAN_DAEMON_URL") {
        return url.to_string();
    }
    // Runtime: same host as the page, port 4780.
    if let Some(window) = web_sys::window() {
        if let Ok(host) = window.location().host() {
            if !host.is_empty() {
                let host_only = host.split(':').next().unwrap_or(&host);
                return format!("http://{host_only}:4780");
            }
        }
    }
    DEFAULT_DAEMON_URL.into()
}

/// Humanize a token count for the header chip: 942 → "942", 12_345 → "12.3k",
/// 1_580_000 → "1.6M". Keeps the readout compact.
fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}
