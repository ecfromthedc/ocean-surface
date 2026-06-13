//! Floating liquid-metal chat corridor — wired to `ocean-daemon`.
//!
//! Activate with `?float=1` (or `#float`) on the surface URL. Uses the same
//! session-first contract as the main cockpit:
//!
//!   POST /v1/agent/sessions → GET /v1/agent/events?session_id=… → POST /v1/agent/turns
//!
//! Tool calls, permission prompts, halt, and streaming deltas are handled by
//! the shared [`Daemon`] client in `daemon.rs`.

use leptos::ev::{Event, SubmitEvent};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::PermissionPrompts;
use crate::daemon::{daemon_url_from_env, Daemon};
use crate::markdown::render as render_md;
use crate::model::{Block, Role, ToolStatus, Turn};

#[component]
pub fn FloatingApp() -> impl IntoView {
    let daemon = Daemon::new(daemon_url_from_env());
    daemon.bootstrap_then_connect();

    let input = RwSignal::new(String::new());
    let status = daemon.status;
    let streaming = daemon.streaming;
    let turns = daemon.turns;

    // Tag body for transparent / overlay styling.
    Effect::new(move |_| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.body().map(|b| {
                let el: &web_sys::Element = b.as_ref();
                let _ = el.class_list().add_1("ocean-float-mode");
            });
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
        }
    };

    let daemon_halt = StoredValue::new(daemon.clone());
    let daemon_perms = StoredValue::new(daemon.clone());

    view! {
        <div class="ocean-float">
            <PermissionPrompts daemon=daemon_perms.get_value() />

            <div class="ocean-float__corridor" id="ocean-float-corridor">
                <FloatStream turns=turns />
                <Show when=move || streaming.get()>
                    <div class="ocean-float__typing" aria-hidden="false">
                        <div class="ocean-float__typing-bead">
                            <i></i><i></i><i></i>
                        </div>
                    </div>
                </Show>
                <form class="ocean-float__composer" on:submit=submit>
                    <input
                        type="text"
                        class="ocean-float__input"
                        placeholder="Message your agent…"
                        prop:value=move || input.get()
                        on:input=move |ev| input.set(event_target_value(&ev))
                        disabled=move || streaming.get()
                    />
                    <Show
                        when=move || streaming.get()
                        fallback=move || view! {
                            <button type="submit" class="ocean-float__send" aria-label="Send">
                                <svg viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
                            </button>
                        }
                    >
                        <button
                            type="button"
                            class="ocean-float__halt"
                            aria-label="Halt"
                            title="Halt turn"
                            on:click=move |_| daemon_halt.with_value(|d| d.halt())
                        >
                            "■"
                        </button>
                    </Show>
                </form>
            </div>

            <Show when=move || !status.get().is_empty()>
                <div class="ocean-float__status">{move || status.get()}</div>
            </Show>
        </div>
    }
}

#[component]
fn FloatStream(turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let stream: NodeRef<leptos::html::Div> = NodeRef::new();
    let pinned = RwSignal::new(true);
    const STICK: f64 = 80.0;

    let on_scroll = move |_: Event| {
        if let Some(el) = stream.get() {
            let el: &web_sys::Element = el.as_ref();
            let dist =
                el.scroll_height() as f64 - el.scroll_top() as f64 - el.client_height() as f64;
            pinned.set(dist <= STICK);
            apply_depth(el);
        }
    };

    Effect::new(move |_| {
        turns.with(|t| t.len());
        if pinned.get_untracked() {
            if let Some(el) = stream.get() {
                let el: web_sys::Element = el.unchecked_into();
                let scroll = move || {
                    el.set_scroll_top(el.scroll_height());
                    apply_depth(&el);
                };
                request_animation_frame(scroll);
            }
        }
    });

    let indices = move || (0..turns.with(Vec::len)).collect::<Vec<_>>();

    view! {
        <div class="ocean-float__stream" node_ref=stream on:scroll=on_scroll>
            <div class="ocean-float__stream-inner">
                <For
                    each=indices
                    key=|i| *i
                    children=move |idx| view! { <FloatTurn idx=idx turns=turns /> }
                />
            </div>
        </div>
    }
}

#[component]
fn FloatTurn(idx: usize, turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let role = move || turns.with(|t| t.get(idx).map(|turn| turn.role));

    view! {
        {move || match role() {
            Some(Role::User) => view! { <FloatUser idx=idx turns=turns /> }.into_any(),
            Some(Role::Assistant) => view! { <FloatAssistant idx=idx turns=turns /> }.into_any(),
            None => ().into_any(),
        }}
    }
}

#[component]
fn FloatUser(idx: usize, turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let text = move || turn_text(&turns.with(|t| t.get(idx).cloned()));
    view! {
        <div class="ocean-float__msg ocean-float__msg--user">
            <div class="ocean-float__bubble">
                <span>{text}</span>
            </div>
        </div>
    }
}

#[component]
fn FloatAssistant(idx: usize, turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let text = move || {
        turns.with(|t| {
            t.get(idx)
                .map(|turn| {
                    turn.blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::Text(s) if !s.is_empty() => Some(s.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        })
    };

    let tools = move || {
        turns.with(|t| {
            t.get(idx)
                .map(|turn| {
                    turn.blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::ToolCall {
                                name,
                                status,
                                output,
                                ..
                            } => Some((name.clone(), *status, output.clone())),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };

    let has_text = move || !text().trim().is_empty();

    view! {
        <div class="ocean-float__msg ocean-float__msg--agent">
            <Show when=has_text>
                <div class="ocean-float__bubble">
                    <span inner_html=move || render_md(&text())></span>
                </div>
            </Show>
            <For
                each=tools
                key=|(name, _, _)| name.clone()
                children=move |(name, status, output)| {
                    let status_class = match status {
                        ToolStatus::Running => "running",
                        ToolStatus::Ok => "ok",
                        ToolStatus::Err => "err",
                    };
                    let label = match status {
                        ToolStatus::Running => format!("{name}…"),
                        ToolStatus::Ok => name.clone(),
                        ToolStatus::Err => format!("{name} ✗"),
                    };
                    view! {
                        <div class=format!("ocean-float__tool ocean-float__tool--{status_class}") title=output>
                            {label}
                        </div>
                    }
                }
            />
        </div>
    }
}

fn turn_text(turn: &Option<Turn>) -> String {
    turn.as_ref()
        .map(|t| {
            t.blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Recession into the invisible corridor vanishing point (top fade only).
fn apply_depth(stream_el: &web_sys::Element) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(corridor) = doc.get_element_by_id("ocean-float-corridor") else {
        return;
    };
    let _corridor = corridor;
    let stream_rect = stream_el.get_bounding_client_rect();
    let fade_end = stream_rect.top() + stream_rect.height() * 0.42;
    let fade_start = stream_rect.top() + stream_rect.height() * 0.08;
    let span = fade_end - fade_start;
    if span <= 0.0 {
        return;
    }

    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    if let Ok(msgs) = doc.query_selector_all("#ocean-float-corridor .ocean-float__msg") {
        for i in 0..msgs.length() {
            let Some(node) = msgs.item(i) else { continue };
            let Ok(el) = node.dyn_into::<web_sys::HtmlElement>() else {
                continue;
            };
            let rect = el.get_bounding_client_rect();
            let mid = rect.top() + rect.height() / 2.0;

            if mid >= fade_end {
                let _ = el.style().set_property("opacity", "");
                let _ = el.style().set_property("transform", "");
                let _ = el.style().set_property("filter", "");
                continue;
            }

            let t = if mid <= fade_start {
                1.0
            } else {
                (fade_end - mid) / span
            };
            let scale = 1.0 - t * 0.14;
            let opacity = 1.0 - t * t * 0.8;
            let blur = t * 2.0;

            let _ = el.style().set_property("opacity", &format!("{opacity:.3}"));
            let _ = el
                .style()
                .set_property("transform", &format!("scale({scale:.3})"));
            let filter = if blur > 0.15 {
                format!("blur({blur:.1}px)")
            } else {
                String::new()
            };
            let _ = el.style().set_property("filter", &filter);
        }
    }
}

pub fn float_mode_active() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let search = window.location().search().unwrap_or_default();
    if search.contains("float=1") || search.contains("mode=float") {
        return true;
    }
    let hash = window.location().hash().unwrap_or_default();
    hash.contains("float")
}
