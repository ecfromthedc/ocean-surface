//! Top-level app shell. Owns the Daemon, mounts the transcript + composer.

use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::ToolDrawer;
use crate::daemon::{Daemon, DEFAULT_DAEMON_URL};
use crate::model::{Block, Role, Turn};
use crate::transcript::Transcript;
use crate::voice::VoiceOrb;

#[component]
pub fn App() -> impl IntoView {
    let daemon = Daemon::new(daemon_url_from_env());
    // Zero-config boot: fetch /api/config from the same-origin proxy to learn
    // the daemon URL + confirm auth is preconfigured, then open the SSE stream.
    // Falls back to daemon_url_from_env() if no proxy answers.
    daemon.bootstrap_then_connect();

    let input = RwSignal::new(String::new());
    let textarea_ref: NodeRef<leptos::html::Textarea> = NodeRef::new();

    // Daemon holds only Copy signal handles, so cloning per-closure is cheap
    // and avoids fighting the borrow checker over a single moved value.
    let status = daemon.status;
    let turns = daemon.turns;
    let streaming = daemon.streaming;
    let voice_ready = daemon.voice_ready;

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

    // Tool drawer: concealed strip that drops down to show recent tool calls.
    let tool_drawer_open = RwSignal::new(false);

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

    view! {
        <main class="ocean-surface">
            <header class="ocean-header">
                <div class="ocean-brand">
                    <span class="ocean-brand__dot"></span>
                    <span class="ocean-brand__name">"Ocean"</span>
                </div>
                <div class="ocean-header__right">
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
                            {move || if muted.get() { "🔇" } else { "🔊" }}
                        </button>
                    </Show>
                </div>
            </header>

            <Transcript daemon=daemon.clone() />

            <ToolDrawer turns=turns open=tool_drawer_open />

            <form class="ocean-composer" on:submit=submit>
                // Push-to-talk only when the proxy has a usable xAI key;
                // otherwise a dim, disabled placeholder explains why.
                <Show
                    when=move || voice_ready.get()
                    fallback=|| view! {
                        <div class="voice-wrap">
                            <button class="voice-orb is-disabled" type="button" disabled=true
                                    title="voice off — set xAI key in ~/.config/ocean-surface/xai.key">
                                <span class="voice-orb__glyph">"🎙"</span>
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
                <button
                    class="ocean-composer__send"
                    type="submit"
                    disabled=move || input.get().trim().is_empty()
                >
                    "Send"
                </button>
            </form>
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
