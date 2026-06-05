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
use wasm_bindgen::JsCast;

use crate::components::ComponentView;
use crate::daemon::Daemon;
use crate::markdown::render as render_md;
use crate::model::{Block, Role, ToolStatus};

/// One renderable item in an assistant turn: either a single block (text,
/// thinking, component, or a lone tool call) or a run of ≥2 consecutive tool
/// calls that collapse into one stacked group.
#[derive(Clone, PartialEq)]
enum RenderItem {
    Single(usize),
    ToolGroup(Vec<usize>),
}

/// Collapse consecutive `ToolCall` blocks into `ToolGroup`s; everything else is
/// a `Single`. A run of exactly one tool call stays a `Single` (no point
/// wrapping a lone call in a group header).
fn render_items(blocks: &[Block]) -> Vec<RenderItem> {
    let mut items = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let flush = |run: &mut Vec<usize>, items: &mut Vec<RenderItem>| {
        match run.len() {
            0 => {}
            1 => items.push(RenderItem::Single(run[0])),
            _ => items.push(RenderItem::ToolGroup(std::mem::take(run))),
        }
        run.clear();
    };
    for (i, block) in blocks.iter().enumerate() {
        if matches!(block, Block::ToolCall { .. }) {
            run.push(i);
        } else {
            flush(&mut run, &mut items);
            items.push(RenderItem::Single(i));
        }
    }
    flush(&mut run, &mut items);
    items
}

#[component]
pub fn Transcript(daemon: Daemon) -> impl IntoView {
    let turns = daemon.turns;
    // Key by turn index. New turns append; existing ones mutate in place and
    // their child views read the signal reactively, so re-keying isn't needed
    // mid-stream.
    let indices = move || (0..turns.with(Vec::len)).collect::<Vec<_>>();

    // Auto-scroll: keep the viewport pinned to the latest output as turns
    // append and streaming deltas grow existing turns — but only when the user
    // is already at (or near) the bottom. If they've scrolled up to read
    // history, we leave them be. "Near bottom" is sampled continuously from the
    // scroll handler so the effect can decide *before* the DOM grows.
    let container: NodeRef<leptos::html::Div> = NodeRef::new();
    let pinned = RwSignal::new(true);

    // px from the bottom within which we still consider the user "pinned".
    // Generous enough to survive a streaming delta landing between frames.
    const STICK_THRESHOLD: f64 = 80.0;

    let on_scroll = move |_| {
        if let Some(el) = container.get() {
            let el: &web_sys::Element = el.as_ref();
            let distance =
                el.scroll_height() as f64 - el.scroll_top() as f64 - el.client_height() as f64;
            pinned.set(distance <= STICK_THRESHOLD);
        }
    };

    Effect::new(move |_| {
        // Track every mutation of the turns signal: new turns AND in-place
        // block growth mid-stream both flow through this one signal, so reading
        // it here subscribes the effect to every streaming delta.
        turns.with(|t| {
            let _total_blocks: usize = t.iter().map(|turn| turn.blocks.len()).sum();
        });
        if pinned.get_untracked() {
            if let Some(el) = container.get() {
                let el: web_sys::Element = el.unchecked_into();
                // Defer to next frame so the just-appended DOM has laid out and
                // scroll_height reflects the new content before we jump.
                let scroll = move || el.set_scroll_top(el.scroll_height());
                request_animation_frame(scroll);
            }
        }
    });

    view! {
        <div class="transcript" node_ref=container on:scroll=on_scroll>
            <For
                each=indices
                key=|i| *i
                children=move |idx| view! { <TurnView idx=idx turns=turns daemon=daemon.clone() /> }
            />
        </div>
    }
}

#[component]
fn TurnView(idx: usize, turns: RwSignal<Vec<crate::model::Turn>>, daemon: Daemon) -> impl IntoView {
    // Role is stable for the life of a turn, so read it once reactively to
    // pick the layout, then let the body derive from the signal.
    let role = move || turns.with(|t| t.get(idx).map(|turn| turn.role));

    view! {
        <div class="turn">
            {move || match role() {
                Some(Role::User) => view! { <UserTurn idx=idx turns=turns /> }.into_any(),
                Some(Role::Assistant) => view! { <AssistantTurn idx=idx turns=turns daemon=daemon.clone() /> }.into_any(),
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
fn AssistantTurn(
    idx: usize,
    turns: RwSignal<Vec<crate::model::Turn>>,
    daemon: Daemon,
) -> impl IntoView {
    // Recompute the render-item list whenever the block set changes. Reading
    // the blocks here also subscribes to tool-status changes (a clone snapshot),
    // so the group summary updates as calls finish.
    let items = move || {
        turns.with(|t| {
            t.get(idx)
                .map(|turn| render_items(&turn.blocks))
                .unwrap_or_default()
        })
    };

    view! {
        <div class="turn--assistant">
            <div class="turn__header">"ocean ▸"</div>
            <div class="turn__body">
                <For
                    each=items
                    key=|item| match item {
                        RenderItem::Single(i) => (0u8, *i),
                        RenderItem::ToolGroup(ix) => (1u8, *ix.first().unwrap_or(&0)),
                    }
                    children=move |item| {
                        let daemon = daemon.clone();
                        match item {
                            RenderItem::Single(block_idx) => view! {
                                <BlockView turn_idx=idx block_idx=block_idx turns=turns daemon=daemon />
                            }
                            .into_any(),
                            // No group wrapper/pill: consecutive tool calls just
                            // render as their own bare, individually-expandable
                            // drawer lines, flush in the turn body.
                            RenderItem::ToolGroup(block_idxs) => view! {
                                <ToolGroupBody
                                    turn_idx=idx
                                    block_idxs=block_idxs
                                    turns=turns
                                    daemon=daemon
                                />
                            }
                            .into_any(),
                        }
                    }
                />
            </div>
        </div>
    }
}

/// A run of consecutive tool calls, each rendered as its own bare,
/// individually-expandable drawer line (no group header/pill). Split into its
/// own component so prop ownership stays clean.
#[component]
fn ToolGroupBody(
    turn_idx: usize,
    block_idxs: Vec<usize>,
    turns: RwSignal<Vec<crate::model::Turn>>,
    daemon: Daemon,
) -> impl IntoView {
    view! {
        <div class="tool-group__body">
            <For
                each=move || block_idxs.clone()
                key=|bi| *bi
                children=move |bi| {
                    let daemon = daemon.clone();
                    view! {
                        <BlockView turn_idx=turn_idx block_idx=bi turns=turns daemon=daemon />
                    }
                }
            />
        </div>
    }
}

#[component]
fn BlockView(
    turn_idx: usize,
    block_idx: usize,
    turns: RwSignal<Vec<crate::model::Turn>>,
    daemon: Daemon,
) -> impl IntoView {
    // Snapshot of this block, recomputed whenever turns changes.
    let block = move || {
        turns.with(|t| {
            t.get(turn_idx)
                .and_then(|turn| turn.blocks.get(block_idx).cloned())
        })
    };

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

    move || {
        let daemon = daemon.clone();
        match block() {
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
                let label = format!("{name}({args_preview})");
                let body = if output.trim().is_empty() {
                    "(no output yet)".to_string()
                } else {
                    output.clone()
                };
                view! {
                    <div class=format!("block block--tool drawer {status_class}")
                        class:is-open=move || expanded>
                        <button class="drawer__head" on:click=move |_| toggle()>
                            <span class="drawer__tick">{glyph}</span>
                            <span class="drawer__dot"></span>
                            <span class="drawer__label">{label}</span>
                            <span class="drawer__status">{status_label}</span>
                        </button>
                        <Show when=move || expanded>
                            <pre class="drawer__body">{body.clone()}</pre>
                        </Show>
                    </div>
                }
                .into_any()
            }

            Some(Block::Component {
                component_id,
                kind,
                props,
            }) => view! {
                <ComponentView component_id kind kind_props=props daemon />
            }
            .into_any(),

            None => ().into_any(),
        }
    }
}
