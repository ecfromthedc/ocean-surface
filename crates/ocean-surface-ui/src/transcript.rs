//! Renders the conversation. Layout mirrors the TUI's PM panel:
//! "you ▸" / "ocean ▸" headers, a single header per assistant turn even
//! when thinking + tools + text interleave, collapsed Thinking pills,
//! tool chips with status color.
//!
//! Everything derives from the `turns` signal so streaming deltas reflect
//! live. Turns are keyed by index for stable DOM; within a turn the block
//! list is rebuilt on each change (cheap for chat-sized content, and avoids
//! stale snapshots that would freeze streaming text).

use leptos::prelude::*;

use crate::daemon::Daemon;
use crate::markdown::render as render_md;
use crate::model::{Block, Role, ToolStatus};

#[component]
pub fn Transcript(daemon: Daemon) -> impl IntoView {
    let turns = daemon.turns;
    // Key by turn index. New turns append; existing ones mutate in place and
    // their child views read the signal reactively, so re-keying isn't needed
    // mid-stream.
    let indices = move || (0..turns.with(Vec::len)).collect::<Vec<_>>();
    view! {
        <div class="transcript">
            <For
                each=indices
                key=|i| *i
                children=move |idx| view! { <TurnView idx=idx turns=turns /> }
            />
        </div>
    }
}

#[component]
fn TurnView(idx: usize, turns: RwSignal<Vec<crate::model::Turn>>) -> impl IntoView {
    // Role is stable for the life of a turn, so read it once reactively to
    // pick the layout, then let the body derive from the signal.
    let role = move || turns.with(|t| t.get(idx).map(|turn| turn.role));

    view! {
        <div class="turn">
            {move || match role() {
                Some(Role::User) => view! { <UserTurn idx=idx turns=turns /> }.into_any(),
                Some(Role::Assistant) => view! { <AssistantTurn idx=idx turns=turns /> }.into_any(),
                None => ().into_any(),
            }}
        </div>
    }
}

#[component]
fn UserTurn(idx: usize, turns: RwSignal<Vec<crate::model::Turn>>) -> impl IntoView {
    let text = move || {
        turns.with(|t| {
            t.get(idx)
                .map(|turn| {
                    turn.blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::Text(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        })
    };
    view! {
        <div class="turn--user">
            <div class="turn__header">"you ▸"</div>
            <div class="turn__body">{text}</div>
        </div>
    }
}

#[component]
fn AssistantTurn(idx: usize, turns: RwSignal<Vec<crate::model::Turn>>) -> impl IntoView {
    // How many blocks does this turn currently have? Re-render the body when
    // that count changes (new block appended). Within each block, content is
    // read reactively too, so streaming deltas show.
    let block_indices =
        move || turns.with(|t| (0..t.get(idx).map(|turn| turn.blocks.len()).unwrap_or(0)).collect::<Vec<_>>());

    view! {
        <div class="turn--assistant">
            <div class="turn__header">"ocean ▸"</div>
            <div class="turn__body">
                <For
                    each=block_indices
                    key=|i| *i
                    children=move |block_idx| view! {
                        <BlockView turn_idx=idx block_idx=block_idx turns=turns />
                    }
                />
            </div>
        </div>
    }
}

#[component]
fn BlockView(
    turn_idx: usize,
    block_idx: usize,
    turns: RwSignal<Vec<crate::model::Turn>>,
) -> impl IntoView {
    // Snapshot of this block, recomputed whenever turns changes.
    let block = move || turns.with(|t| t.get(turn_idx).and_then(|turn| turn.blocks.get(block_idx).cloned()));

    let toggle = move || {
        turns.update(|t| {
            if let Some(turn) = t.get_mut(turn_idx) {
                if let Some(b) = turn.blocks.get_mut(block_idx) {
                    match b {
                        Block::Thinking { expanded, .. } => *expanded = !*expanded,
                        Block::ToolCall { expanded, .. } => *expanded = !*expanded,
                        _ => {}
                    }
                }
            }
        });
    };

    move || match block() {
        Some(Block::Text(text)) => view! {
            <div class="block block--text" inner_html=render_md(&text)></div>
        }
        .into_any(),

        Some(Block::Thinking { content, expanded }) => {
            let count = content.chars().count();
            let glyph = if expanded { "▾" } else { "▸" };
            view! {
                <div class="block block--thinking">
                    <button class="block__pill" on:click=move |_| toggle()>
                        {format!("{glyph} thinking… ({count} chars)")}
                    </button>
                    <Show when=move || expanded>
                        <pre class="block__thinking-body">{content.clone()}</pre>
                    </Show>
                </div>
            }
            .into_any()
        }

        Some(Block::ToolCall {
            name,
            args_preview,
            output,
            status,
            expanded,
            ..
        }) => {
            let status_class = match status {
                ToolStatus::Running => "is-running",
                ToolStatus::Ok => "is-ok",
                ToolStatus::Err => "is-err",
            };
            let status_label = match status {
                ToolStatus::Running => "running",
                ToolStatus::Ok => "done",
                ToolStatus::Err => "error",
            };
            let glyph = if expanded { "▾" } else { "▸" };
            let header = format!("{glyph} tool · {name}({args_preview}) · {status_label}");
            let body = if output.trim().is_empty() {
                "(no output yet)".to_string()
            } else {
                output.clone()
            };
            view! {
                <div class=format!("block block--tool {status_class}")>
                    <button class="block__pill" on:click=move |_| toggle()>
                        {header}
                    </button>
                    <Show when=move || expanded>
                        <pre class="block__tool-output">{body.clone()}</pre>
                    </Show>
                </div>
            }
            .into_any()
        }

        None => ().into_any(),
    }
}
