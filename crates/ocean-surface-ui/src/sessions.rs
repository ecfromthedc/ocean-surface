//! Sessions panel — list, switch, and manage conversations.
//!
//! Fetches `GET /v1/agent/sessions` from the daemon and renders a compact
//! side panel or overlay. Click any session to resume it; click "New Session"
//! to start fresh.

use leptos::prelude::*;

use crate::daemon::{Daemon, SessionSummary};

/// Sessions panel that slides in from the right when open.
#[component]
pub fn SessionsPanel(daemon: Daemon, open: RwSignal<bool>) -> impl IntoView {
    let session_list = daemon.session_list;
    let current_id = daemon.session_id;

    // Fetch sessions whenever the panel opens.
    let fetch = {
        let daemon = daemon.clone();
        move || daemon.fetch_sessions()
    };
    Effect::new(move |_| {
        if open.get() {
            fetch();
        }
    });

    let is_open = move || open.get();

    view! {
        <div
            class="sessions-overlay"
            class:sessions-overlay--open=is_open
            on:click=move |ev| {
                // Close when clicking the backdrop, not the panel itself.
                let target = event_target::<web_sys::HtmlElement>(&ev);
                if target.class_list().contains("sessions-overlay") {
                    open.set(false);
                }
            }
        >
            <div class="sessions-panel">
                <div class="sessions-panel__head">
                    <h2 class="sessions-panel__title">"Sessions"</h2>
                    <button
                        class="sessions-panel__close"
                        type="button"
                        aria-label="close sessions panel"
                        on:click=move |_| open.set(false)
                    >
                        "✕"
                    </button>
                </div>

                <div class="sessions-panel__actions">
                    <button
                        class="sessions-panel__new-btn"
                        type="button"
                        on:click={
                            let daemon = daemon.clone();
                            // Eagerly create the session on the daemon and switch
                            // to it (re-scoping SSE). Keep the panel open so the
                            // freshly created session shows up active in the list.
                            move |_| daemon.create_session()
                        }
                    >
                        "+ New Session"
                    </button>
                </div>

                <div class="sessions-panel__list">
                    <For
                        each=move || session_list.get()
                        key=|s| s.id.clone()
                        children=move |session: SessionSummary| {
                            let daemon = daemon.clone();
                            let session_id = session.id.clone();
                            let session_title = session.title.clone();
                            let is_current = {
                                let session_id = session_id.clone();
                                move || current_id.get().as_deref() == Some(session_id.as_str())
                            };

                            view! {
                                <button
                                    class="sessions-item"
                                    class:sessions-item--active=is_current
                                    type="button"
                                    on:click={
                                        let daemon = daemon.clone();
                                        let id = session_id.clone();
                                        let title = session_title.clone();
                                        move |_| {
                                            daemon.switch_session(id.clone(), title.clone());
                                            open.set(false);
                                        }
                                    }
                                >
                                    <div class="sessions-item__title">
                                        {if session_title.is_empty() {
                                            "(untitled)".to_string()
                                        } else {
                                            session_title.clone()
                                        }}
                                    </div>
                                    <div class="sessions-item__meta">
                                        <span class="sessions-item__turns">
                                            {format!("{} turn{}", session.turn_count, if session.turn_count == 1 { "" } else { "s" })}
                                        </span>
                                        <span class="sessions-item__cwd">
                                            {session.cwd.clone()}
                                        </span>
                                    </div>
                                </button>
                            }
                        }
                    />
                </div>

                <Show when=move || session_list.get().is_empty()>
                    <div class="sessions-panel__empty">
                        "No sessions yet. Send a message to start one."
                    </div>
                </Show>
            </div>
        </div>
    }
}
