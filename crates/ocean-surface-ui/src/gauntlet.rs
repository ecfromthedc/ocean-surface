//! Leptos Gauntlet — an exhaustive demo of every renderable component kind.
//!
//! This component mounts inline so the user can verify every block type,
//! component kind, and lifecycle event works without an agent running.
//! It directly turns data into the same Turn/Block model the SSE pipeline
//! produces, exercising the exact same Transcript rendering path.
//!
//! Access by clicking the "🧪" button in the header, or by setting a flag.

use leptos::prelude::*;
use serde_json::json;

use crate::components::ComponentView;
use crate::daemon::Daemon;
use crate::markdown::render as render_md;
use crate::model::{Block, Role, ToolStatus, Turn};

/// Renders a full gauntlet of inline component + block examples.
/// Uses only the public rendering pathway (`Transcript`-style layout,
/// `ComponentView` dispatch) so it's a faithful test of the real pipeline.
#[component]
pub fn Gauntlet() -> impl IntoView {
    view! {
        <div class="gauntlet">
            <h1>"🧪 Leptos Gauntlet"</h1>
            <p class="gauntlet__intro">
                "Every block type, component kind, and layout the agent can render. "
                "If all of these look right, the rendering pipeline is sound."
            </p>

            <hr/>
            <h2>"1. Text blocks"</h2>
            <GauntletText/>

            <hr/>
            <h2>"2. Thinking blocks"</h2>
            <GauntletThinking/>

            <hr/>
            <h2>"3. Tool call blocks"</h2>
            <GauntletToolCalls/>

            <hr/>
            <h2>"4. Markdown rendering"</h2>
            <GauntletMarkdown/>

            <hr/>
            <h2>"5. Kanban component"</h2>
            <GauntletKanban/>

            <hr/>
            <h2>"6. Form component"</h2>
            <GauntletForm/>

            <hr/>
            <h2>"7. Table component"</h2>
            <GauntletTable/>

            <hr/>
            <h2>"8. Progress component"</h2>
            <GauntletProgress/>

            <hr/>
            <h2>"9. Dashboard component (grid of children)"</h2>
            <GauntletDashboard/>

            <hr/>
            <h2>"10. Mixed turn — everything stacked"</h2>
            <GauntletMixedTurn/>

            <hr/>
            <h2>"11. Mixed component — kanban inside form inside dashboard"</h2>
            <GauntletNestedComponents/>
        </div>
    }
}

// ---------------------------------------------------------------------------
// 1. Text blocks
// ---------------------------------------------------------------------------

#[component]
fn GauntletText() -> impl IntoView {
    let turns = RwSignal::new(vec![
        Turn {
            turn_id: Some("gauntlet-text-1".into()),
            role: Role::Assistant,
            blocks: vec![
                Block::Text("This is a plain text block. It should appear as normal prose.".into()),
                Block::Text(
                    "This is a second text block in the same turn. They stack vertically.".into(),
                ),
            ],
        },
        Turn {
            turn_id: Some("gauntlet-text-2".into()),
            role: Role::User,
            blocks: vec![Block::Text(
                "User turns render with the \"you ▸\" header.".into(),
            )],
        },
    ]);

    view! {
        <TurnList turns=turns />
    }
}

// ---------------------------------------------------------------------------
// 2. Thinking blocks
// ---------------------------------------------------------------------------

#[component]
fn GauntletThinking() -> impl IntoView {
    let turns = RwSignal::new(vec![
        Turn {
            turn_id: Some("gauntlet-think-1".into()),
            role: Role::Assistant,
            blocks: vec![
                Block::Thinking {
                    content: "Hmm, let me think about this step by step.\n\nFirst, I need to understand what the user is asking. They want to see a thinking block rendered inline. The block should be collapsible — clicking the pill expands or collapses the content.\n\nSecond, I should verify that the counter shows the correct character count.\n\nThird, this reasoning should be clearly separated from the final answer.".into(),
                    expanded: false,
                },
                Block::Text("Okay, here's my answer after thinking it through.".into()),
            ],
        },
        Turn {
            turn_id: Some("gauntlet-think-2".into()),
            role: Role::Assistant,
            blocks: vec![
                Block::Thinking {
                    content: "A short thought that starts expanded.".into(),
                    expanded: true,
                },
            ],
        },
    ]);

    view! {
        <TurnList turns=turns />
    }
}

// ---------------------------------------------------------------------------
// 3. Tool call blocks
// ---------------------------------------------------------------------------

#[component]
fn GauntletToolCalls() -> impl IntoView {
    let turns = RwSignal::new(vec![Turn {
        turn_id: Some("gauntlet-tool-1".into()),
        role: Role::Assistant,
        blocks: vec![
            Block::ToolCall {
                call_id: "tc-ok-1".into(),
                name: "read".into(),
                args_preview: r#"{"path": "src/main.rs"}"#.into(),
                output: "fn main() {\n    println!(\"Hello, world!\");\n}".into(),
                status: ToolStatus::Ok,
                expanded: false,
            },
            Block::ToolCall {
                call_id: "tc-err-1".into(),
                name: "bash".into(),
                args_preview: r#"{"command": "rm -rf /"}"#.into(),
                output: "permission denied: cannot delete root".into(),
                status: ToolStatus::Err,
                expanded: true,
            },
            Block::ToolCall {
                call_id: "tc-running-1".into(),
                name: "grep".into(),
                args_preview: r#"{"pattern": "TODO"}"#.into(),
                output: "".into(),
                status: ToolStatus::Running,
                expanded: false,
            },
        ],
    }]);

    view! {
        <TurnList turns=turns />
    }
}

// ---------------------------------------------------------------------------
// 4. Markdown rendering
// ---------------------------------------------------------------------------

#[component]
fn GauntletMarkdown() -> impl IntoView {
    let md = "\
# Heading 1
## Heading 2
### Heading 3

**Bold text**, *italic text*, and `inline code`.

> Blockquote with a citation.

- List item one
- List item two
  - Nested item

1. Ordered first
2. Ordered second

```rust
fn hello() -> &'static str {
    \"Hello, Gauntlet!\"
}
```

[Link to Leptos](https://leptos.dev)

| Column A | Column B |
|----------|----------|
| Cell 1   | Cell 2   |
| Cell 3   | Cell 4   |

---

Horizontal rule above.";
    let turns = RwSignal::new(vec![Turn {
        turn_id: Some("gauntlet-md-1".into()),
        role: Role::Assistant,
        blocks: vec![Block::Text(md.into())],
    }]);

    view! {
        <TurnList turns=turns />
    }
}

// ---------------------------------------------------------------------------
// 5. Kanban component
// ---------------------------------------------------------------------------

#[component]
fn GauntletKanban() -> impl IntoView {
    let props = json!({
        "columns": [
            { "id": "backlog", "title": "Backlog" },
            { "id": "in-progress", "title": "In Progress" },
            { "id": "done", "title": "Done" },
        ],
        "cards": [
            { "id": "card-1", "column": "backlog", "title": "Fix login bug", "description": "Users can't log in with SSO" },
            { "id": "card-2", "column": "backlog", "title": "Add dark mode" },
            { "id": "card-3", "column": "in-progress", "title": "Refactor auth", "description": "Move to JWT" },
            { "id": "card-4", "column": "done", "title": "Ship v1.0", "description": "Initial release" },
            { "id": "card-5", "column": "done", "title": "Write tests" },
        ],
    });

    view! {
        <div class="gauntlet__component-wrapper">
            <ComponentView component_id="gauntlet-kanban".into() kind="kanban".into() kind_props=props daemon=Daemon::dummy() />
        </div>
    }
}

// ---------------------------------------------------------------------------
// 6. Form component
// ---------------------------------------------------------------------------

#[component]
fn GauntletForm() -> impl IntoView {
    let props = json!({
        "title": "Report a Bug",
        "fields": [
            { "name": "title", "label": "Title", "type": "text", "required": true },
            { "name": "severity", "label": "Severity", "type": "select", "required": true, "options": ["low", "medium", "high", "critical"] },
            { "name": "description", "label": "Description", "type": "textarea", "required": false },
            { "name": "email", "label": "Email", "type": "email", "required": true },
        ],
        "submit_label": "Submit Bug Report",
    });

    view! {
        <div class="gauntlet__component-wrapper">
            <ComponentView component_id="gauntlet-form".into() kind="form".into() kind_props=props daemon=Daemon::dummy() />
        </div>
    }
}

// ---------------------------------------------------------------------------
// 7. Table component
// ---------------------------------------------------------------------------

#[component]
fn GauntletTable() -> impl IntoView {
    let props = json!({
        "columns": ["Name", "Status", "Priority", "Assigned To"],
        "rows": [
            ["Fix login bug", "open", "high", "alice"],
            ["Add dark mode", "in-progress", "medium", "bob"],
            ["Refactor auth", "open", "critical", "carol"],
            ["Write docs", "done", "low", "dave"],
            ["Ship v1.0", "done", "high", "everyone"],
        ],
    });

    view! {
        <div class="gauntlet__component-wrapper">
            <ComponentView component_id="gauntlet-table".into() kind="table".into() kind_props=props daemon=Daemon::dummy() />
        </div>
    }
}

// ---------------------------------------------------------------------------
// 8. Progress component
// ---------------------------------------------------------------------------

#[component]
fn GauntletProgress() -> impl IntoView {
    view! {
        <div class="gauntlet__component-wrapper gauntlet__component-wrapper--column">
            <h4>"Determinate (45%)"</h4>
            <ComponentView component_id="gauntlet-progress-1".into() kind="progress".into()
                kind_props=json!({ "label": "Building...", "value": 0.45, "max": 1.0, "indeterminate": false })
                daemon=Daemon::dummy()
            />

            <h4>"Determinate (100%)"</h4>
            <ComponentView component_id="gauntlet-progress-2".into() kind="progress".into()
                kind_props=json!({ "label": "Done!", "value": 1.0, "max": 1.0, "indeterminate": false })
                daemon=Daemon::dummy()
            />

            <h4>"Indeterminate (spinner)"</h4>
            <ComponentView component_id="gauntlet-progress-3".into() kind="progress".into()
                kind_props=json!({ "label": "Working...", "value": 0.0, "max": 1.0, "indeterminate": true })
                daemon=Daemon::dummy()
            />
        </div>
    }
}

// ---------------------------------------------------------------------------
// 9. Dashboard component
// ---------------------------------------------------------------------------

#[component]
fn GauntletDashboard() -> impl IntoView {
    // A dashboard is a CSS grid of children. Each child can be a literal
    // component (kind + props inline) or a reference by id.
    let props = json!({
        "children": [
            {
                "id": "dash-kanban",
                "kind": "kanban",
                "props": {
                    "columns": [
                        { "id": "todo", "title": "To Do" },
                        { "id": "done", "title": "Done" },
                    ],
                    "cards": [
                        { "id": "dc-1", "column": "todo", "title": "Task A" },
                        { "id": "dc-2", "column": "done", "title": "Task B" },
                    ],
                },
            },
            {
                "id": "dash-table",
                "kind": "table",
                "props": {
                    "columns": ["Metric", "Value"],
                    "rows": [["Uptime", "99.9%"], ["Users", "1,234"]],
                },
            },
            {
                "id": "dash-progress",
                "kind": "progress",
                "props": {
                    "label": "Sprint completion",
                    "value": 7.0,
                    "max": 10.0,
                    "indeterminate": false,
                },
            },
        ],
    });

    view! {
        <div class="gauntlet__component-wrapper">
            <ComponentView component_id="gauntlet-dashboard".into() kind="dashboard".into() kind_props=props daemon=Daemon::dummy() />
        </div>
    }
}

// ---------------------------------------------------------------------------
// 10. Mixed turn — everything in one assistant turn
// ---------------------------------------------------------------------------

#[component]
fn GauntletMixedTurn() -> impl IntoView {
    let turns = RwSignal::new(vec![
        Turn {
            turn_id: Some("gauntlet-mixed-1".into()),
            role: Role::Assistant,
            blocks: vec![
                Block::Thinking {
                    content: "The user wants a comprehensive demo. Let me render a thinking block, then a tool call, then some text, then a component.".into(),
                    expanded: false,
                },
                Block::ToolCall {
                    call_id: "mixed-tc-1".into(),
                    name: "glob".into(),
                    args_preview: r#"{"pattern": "src/**/*.rs"}"#.into(),
                    output: "src/main.rs\nsrc/lib.rs\nsrc/components.rs".into(),
                    status: ToolStatus::Ok,
                    expanded: false,
                },
                Block::Text("**Here are the source files I found.** Now let me show you the task board:".into()),
                Block::Component {
                    component_id: "mixed-kanban".into(),
                    kind: "kanban".into(),
                    props: json!({
                        "columns": [
                            { "id": "todo", "title": "To Do" },
                            { "id": "done", "title": "Done" },
                        ],
                        "cards": [
                            { "id": "mc-1", "column": "todo", "title": "Review PR" },
                            { "id": "mc-2", "column": "done", "title": "Fix typo" },
                        ],
                    }),
                },
                Block::Text("And here's a quick data table:".into()),
                Block::Component {
                    component_id: "mixed-table".into(),
                    kind: "table".into(),
                    props: json!({
                        "columns": ["File", "Lines"],
                        "rows": [["main.rs", "42"], ["lib.rs", "128"]],
                    }),
                },
            ],
        },
    ]);

    view! {
        <TurnList turns=turns />
    }
}

// ---------------------------------------------------------------------------
// 11. Nested components — kanban inside dashboard
// ---------------------------------------------------------------------------

#[component]
fn GauntletNestedComponents() -> impl IntoView {
    let props = json!({
        "children": [
            {
                "id": "nested-form",
                "kind": "form",
                "props": {
                    "title": "Task Details",
                    "fields": [
                        { "name": "name", "label": "Name", "type": "text", "required": true },
                        { "name": "status", "label": "Status", "type": "select", "required": true,
                          "options": ["open", "in-progress", "done"] },
                    ],
                    "submit_label": "Save",
                },
            },
            {
                "id": "nested-progress",
                "kind": "progress",
                "props": {
                    "label": "Task progress",
                    "value": 0.7,
                    "max": 1.0,
                    "indeterminate": false,
                },
            },
        ],
    });

    view! {
        <div class="gauntlet__component-wrapper">
            <ComponentView component_id="gauntlet-nested".into() kind="dashboard".into() kind_props=props daemon=Daemon::dummy() />
        </div>
    }
}

// ---------------------------------------------------------------------------
// TurnList — reuses the same pattern as Transcript but with a local turns vec
// ---------------------------------------------------------------------------

/// Renders a list of turns exactly the way Transcript does.
#[component]
fn TurnList(turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let indices = move || (0..turns.with(Vec::len)).collect::<Vec<_>>();
    view! {
        <div class="transcript gauntlet__transcript">
            <For
                each=indices
                key=|i| *i
                children=move |idx| view! { <GauntletTurn idx=idx turns=turns /> }
            />
        </div>
    }
}

#[component]
fn GauntletTurn(idx: usize, turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let role = move || turns.with(|t| t.get(idx).map(|turn| turn.role));

    view! {
        <div class="turn">
            {move || match role() {
                Some(Role::User) => {
                    let text = move || turns.with(|t| {
                        t.get(idx).map(|turn| turn.blocks.iter()
                            .filter_map(|b| match b { Block::Text(s) => Some(s.clone()), _ => None })
                            .collect::<Vec<_>>().join("\n"))
                        .unwrap_or_default()
                    });
                    view! {
                        <div class="turn--user">
                            <div class="turn__header">"you ▸"</div>
                            <div class="turn__body">{text}</div>
                        </div>
                    }.into_any()
                }
                Some(Role::Assistant) => {
                    let block_indices = move || turns.with(|t| {
                        (0..t.get(idx).map(|turn| turn.blocks.len()).unwrap_or(0)).collect::<Vec<_>>()
                    });
                    view! {
                        <div class="turn--assistant">
                            <div class="turn__header">"ocean ▸"</div>
                            <div class="turn__body">
                                <For
                                    each=block_indices
                                    key=|i| *i
                                    children=move |block_idx| view! { <GauntletBlock idx=idx block_idx=block_idx turns=turns /> }
                                />
                            </div>
                        </div>
                    }.into_any()
                }
                None => ().into_any(),
            }}
        </div>
    }
}

#[component]
fn GauntletBlock(idx: usize, block_idx: usize, turns: RwSignal<Vec<Turn>>) -> impl IntoView {
    let block = move || {
        turns.with(|t| {
            t.get(idx)
                .and_then(|turn| turn.blocks.get(block_idx).cloned())
        })
    };

    let toggle = move || {
        turns.update(|t| {
            if let Some(turn) = t.get_mut(idx) {
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
            let label = format!("{name}({args_preview})");
            let body = if output.trim().is_empty() {
                "(no output yet)".to_string()
            } else {
                output.clone()
            };
            view! {
                <div class=format!("block block--tool drawer {status_class}") class:is-open=move || expanded>
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
            }.into_any()
        }

        Some(Block::Component {
            component_id,
            kind,
            props,
        }) => view! {
            <ComponentView component_id kind kind_props=props daemon=Daemon::dummy() />
        }
        .into_any(),

        None => ().into_any(),
    }
}
