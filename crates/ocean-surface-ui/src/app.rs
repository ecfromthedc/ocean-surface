//! Top-level app shell. Owns the Daemon, mounts the transcript + composer.

use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::daemon::{Daemon, DEFAULT_DAEMON_URL};
use crate::transcript::Transcript;

#[component]
pub fn App() -> impl IntoView {
    let daemon = Daemon::new(daemon_url_from_env());
    // Subscribe to /v1/agent/events for the lifetime of the page.
    daemon.connect();

    let input = RwSignal::new(String::new());
    let textarea_ref: NodeRef<leptos::html::Textarea> = NodeRef::new();

    // Daemon holds only Copy signal handles, so cloning per-closure is cheap
    // and avoids fighting the borrow checker over a single moved value.
    let status = daemon.status;

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

    view! {
        <main class="ocean-surface">
            <header class="ocean-header">
                <div class="ocean-brand">
                    <span class="ocean-brand__dot"></span>
                    <span class="ocean-brand__name">"Ocean"</span>
                </div>
                <div class="ocean-status">{move || status.get()}</div>
            </header>

            <Transcript daemon=daemon.clone() />

            <form class="ocean-composer" on:submit=submit>
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
