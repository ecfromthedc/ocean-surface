//! Renders the conversation. Layout mirrors the TUI's PM panel:
//! "you ▸" / "ocean ▸" headers, a single header per assistant turn even
//! when thinking + tools + text interleave, collapsed Thinking pills,
//! tool chips with status color.
//!
//! Everything derives from the `turns` signal so streaming deltas reflect
//! live. Turns are keyed by index for stable DOM; within a turn the block
//! list is rebuilt on each change (cheap for chat-sized content, and avoids
//! stale snapshots that would freeze streaming text).

use std::collections::HashSet;

use leptos::prelude::*;

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
    view! {
        <div class="transcript">
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
fn AssistantTurn(idx: usize, turns: RwSignal<Vec<crate::model::Turn>>, daemon: Daemon) -> impl IntoView {
    // Which tool groups (keyed by their first block index) are expanded.
    // Tool groups collapse by default so a turn with many tool calls doesn't
    // flood the transcript.
    let open_groups = RwSignal::new(HashSet::<usize>::new());

    // Recompute the render-item list whenever the block set changes. Reading
    // the blocks here also subscribes to tool-status changes (a clone snapshot),
    // so the group summary updates as calls finish.
    let items = move || turns.with(|t| t.get(idx).map(|turn| render_items(&turn.blocks)).unwrap_or_default());

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
                            RenderItem::ToolGroup(block_idxs) => view! {
                                <ToolGroupView
                                    turn_idx=idx
                                    block_idxs=block_idxs
                                    turns=turns
                                    daemon=daemon
                                    open_groups=open_groups
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

/// A stacked, collapsed-by-default group of consecutive tool calls. Shows one
/// compact summary row ("⚙ N tools · status"); clicking expands to the
/// individual tool drawers (each still independently expandable for output).
#[component]
fn ToolGroupView(
    turn_idx: usize,
    block_idxs: Vec<usize>,
    turns: RwSignal<Vec<crate::model::Turn>>,
    daemon: Daemon,
    open_groups: RwSignal<HashSet<usize>>,
) -> impl IntoView {
    let group_key = *block_idxs.first().unwrap_or(&0);
    let count = block_idxs.len();
    let is_open = move || open_groups.with(|g| g.contains(&group_key));
    let toggle = move |_| {
        open_groups.update(|g| {
            if !g.remove(&group_key) {
                g.insert(group_key);
            }
        });
    };

    // Aggregate status across the group's tool calls, read reactively. A Memo
    // is Copy, so it can be shared by the summary text, the status class, etc.
    let summary = {
        let block_idxs = block_idxs.clone();
        Memo::new(move |_| {
            turns.with(|t| {
                let turn = match t.get(turn_idx) {
                    Some(turn) => turn,
                    None => return (0usize, 0usize, 0usize),
                };
                let mut running = 0;
                let mut err = 0;
                let mut done = 0;
                for &bi in &block_idxs {
                    if let Some(Block::ToolCall { status, .. }) = turn.blocks.get(bi) {
                        match status {
                            ToolStatus::Running => running += 1,
                            ToolStatus::Err => err += 1,
                            ToolStatus::Ok => done += 1,
                        }
                    }
                }
                (running, done, err)
            })
        })
    };

    let summary_text = move || {
        let (running, done, err) = summary.get();
        let mut parts = Vec::new();
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if done > 0 {
            parts.push(format!("{done} done"));
        }
        if err > 0 {
            parts.push(format!("{err} error"));
        }
        if parts.is_empty() {
            format!("{count} tools")
        } else {
            format!("{count} tools · {}", parts.join(", "))
        }
    };

    let group_class = move || {
        let (running, _done, err) = summary.get();
        if err > 0 {
            "is-err"
        } else if running > 0 {
            "is-running"
        } else {
            "is-ok"
        }
    };

    view! {
        <div class=move || format!("tool-group {}", group_class()) class:is-open=is_open>
            <button class="tool-group__head" on:click=toggle>
                <span class="tool-group__tick">{move || if is_open() { "▾" } else { "▸" }}</span>
                <span class="tool-group__icon">"⚙"</span>
                <span class="tool-group__label">{summary_text}</span>
            </button>
            <Show when=is_open>
                <ToolGroupBody
                    turn_idx=turn_idx
                    block_idxs=block_idxs.clone()
                    turns=turns
                    daemon=daemon.clone()
                />
            </Show>
        </div>
    }
}

/// The expanded contents of a tool group: each tool call rendered as its own
/// drawer. Split into its own component so prop ownership is clean (avoids the
/// Fn/FnOnce capture gymnastics of inlining the For inside `<Show>`).
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
            }) => {
                view! {
                    <ComponentView component_id kind kind_props=props daemon />
                }
                .into_any()
            }

            None => ().into_any(),
        }
    }
}
