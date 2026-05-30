//! Renders live UI components that the agent emits via `component_render`.
//!
//! Each component kind maps to a Leptos view that reads the `props` JSON
//! and renders interactively. User interactions (button clicks, form submits,
//! card drags) are sent back to the daemon via `Daemon::send_component_event`,
//! which the agent's `component_wait` tool picks up.

use leptos::prelude::*;
use serde_json::Value;

use crate::daemon::Daemon;
use crate::model::{Block, Role, ToolStatus, Turn};

/// Dispatch to the right component renderer based on `kind`.
#[component]
pub fn ComponentView(
    component_id: String,
    kind: String,
    kind_props: Value,
    daemon: Daemon,
) -> impl IntoView {
    match kind.as_str() {
        "kanban" => view! {
            <KanbanView component_id kind_props daemon />
        }
        .into_any(),
        "form" => view! {
            <FormView component_id kind_props daemon />
        }
        .into_any(),
        "table" => view! {
            <TableView component_id kind_props daemon />
        }
        .into_any(),
        "progress" => view! {
            <ProgressView kind_props />
        }
        .into_any(),
        "markdown" => view! {
            <MarkdownView kind_props />
        }
        .into_any(),
        "dashboard" => view! {
            <DashboardView kind_props daemon />
        }
        .into_any(),
        other => view! {
            <div class="block block--component-unknown">
                <span class="block__pill">
                    {format!("unknown component kind: {other}")}
                </span>
            </div>
        }
        .into_any(),
    }
}

// ---------------------------------------------------------------------------
// Kanban
// ---------------------------------------------------------------------------

/// A kanban board. Props shape:
/// ```json
/// { "columns": [{ "id": "todo", "title": "To Do" }],
///   "cards": [{ "id": "card-1", "column": "todo", "title": "Fix bug" }] }
/// ```
#[component]
fn KanbanView(
    component_id: String,
    kind_props: Value,
    daemon: Daemon,
) -> impl IntoView {
    let columns = kind_props
        .get("columns")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cards = kind_props
        .get("cards")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let on_card_click = {
        let component_id = component_id.clone();
        let daemon = daemon.clone();
        move |card_id: &str| {
            let payload = serde_json::json!({
                "type": "card_clicked",
                "payload": { "card_id": card_id }
            });
            daemon.send_component_event(component_id.clone(), payload);
        }
    };

    view! {
        <div class="component-kanban">
            <div class="kanban-columns">
                {columns.into_iter().map(|col| {
                    let col_id = col.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let col_title = col.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let col_cards: Vec<&Value> = cards.iter().filter(|c| {
                        c.get("column").and_then(|v| v.as_str()) == Some(&col_id)
                    }).collect();
                    let on_click = on_card_click.clone();

                    view! {
                        <div class="kanban-column">
                            <div class="kanban-column__header">{col_title.clone()}</div>
                            <div class="kanban-column__cards">
                                {col_cards.into_iter().map(move |card| {
                                    let card_id = card.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let card_title = card.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let card_desc = card.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                    let oc = on_click.clone();
                                    let cid = card_id.clone();
                                    view! {
                                        <button
                                            class="kanban-card"
                                            type="button"
                                            on:click=move |_| oc(&cid)
                                        >
                                            <div class="kanban-card__title">{card_title.clone()}</div>
                                            {if !card_desc.is_empty() {
                                                view! { <div class="kanban-card__desc">{card_desc.to_string()}</div> }.into_any()
                                            } else {
                                                ().into_any()
                                            }}
                                        </button>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        </div>
                    }
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------

/// A simple input form. Props shape:
/// ```json
/// { "title": "Report a bug",
///   "fields": [{ "name": "title", "label": "Title", "type": "text", "required": true }],
///   "submit_label": "Submit" }
/// ```
#[component]
fn FormView(
    component_id: String,
    kind_props: Value,
    daemon: Daemon,
) -> impl IntoView {
    let title = kind_props
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Form");
    let fields = kind_props
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let submit_label = kind_props
        .get("submit_label")
        .and_then(|v| v.as_str())
        .unwrap_or("Submit");

    // Store form values reactively by field name.
    let values: Vec<(String, RwSignal<String>)> = fields
        .iter()
        .filter_map(|f| {
            let name = f.get("name").and_then(|v| v.as_str())?.to_string();
            Some((name, RwSignal::new(String::new())))
        })
        .collect();

    let on_submit = {
        let component_id = component_id.clone();
        let daemon = daemon.clone();
        let values = values.clone();
        move |ev: leptos::ev::SubmitEvent| {
            ev.prevent_default();
            let mut payload = serde_json::Map::new();
            for (name, signal) in &values {
                payload.insert(name.clone(), Value::String(signal.get_untracked()));
            }
            daemon.send_component_event(
                component_id.clone(),
                serde_json::json!({
                    "type": "form_submit",
                    "payload": Value::Object(payload),
                }),
            );
        }
    };

    view! {
        <div class="component-form">
            <form on:submit=on_submit>
                <h4 class="component-form__title">{title.to_string()}</h4>
                {fields.clone().into_iter().map(|field| {
                    let name = field.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let label = field.get("label").and_then(|v| v.as_str()).unwrap_or(&name).to_string();
                    let field_type = field.get("type").and_then(|v| v.as_str()).unwrap_or("text").to_string();
                    let required = field.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
                    let signal = values.iter().find(|(n, _)| *n == name).map(|(_, s)| *s);

                    match field_type.as_str() {
                        "textarea" => {
                            let label_c = label.clone();
                            view! {
                                <label class="form-field">
                                    <span class="form-field__label">{label_c}{if required { "*" } else { "" }}</span>
                                    <textarea
                                        class="form-field__input"
                                        prop:value=move || signal.map(|s| s.get()).unwrap_or_default()
                                        on:input=move |ev| { if let Some(s) = signal { s.set(event_target_value(&ev)) } }
                                        rows="3"
                                    />
                                </label>
                            }.into_any()
                        }
                        "select" => {
                            let options = field.get("options").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                            let label_c = label.clone();
                            view! {
                                <label class="form-field">
                                    <span class="form-field__label">{label_c}{if required { "*" } else { "" }}</span>
                                    <select
                                        class="form-field__input"
                                        on:change=move |ev| { if let Some(s) = signal { s.set(event_target_value(&ev)) } }
                                    >
                                        <option value="" disabled selected>"—"</option>
                                        {options.into_iter().map(|opt| {
                                            let val = opt.as_str().unwrap_or("").to_string();
                                            let val2 = val.clone();
                                            view! { <option value=val2>{val}</option> }
                                        }).collect::<Vec<_>>()}
                                    </select>
                                </label>
                            }.into_any()
                        }
                        _ => {
                            let label_c = label.clone();
                            view! {
                                <label class="form-field">
                                    <span class="form-field__label">{label_c}{if required { "*" } else { "" }}</span>
                                    <input
                                        class="form-field__input"
                                        type=field_type
                                        prop:value=move || signal.map(|s| s.get()).unwrap_or_default()
                                        on:input=move |ev| { if let Some(s) = signal { s.set(event_target_value(&ev)) } }
                                    />
                                </label>
                            }.into_any()
                        }
                    }
                }).collect::<Vec<_>>()}
                <button class="form-submit" type="submit">{submit_label.to_string()}</button>
            </form>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------

/// A simple data table. Props shape:
/// ```json
/// { "columns": ["Name", "Status"],
///   "rows": [["Fix bug", "open"], ["Add tests", "done"]] }
/// ```
#[component]
fn TableView(
    component_id: String,
    kind_props: Value,
    daemon: Daemon,
) -> impl IntoView {
    let columns = kind_props
        .get("columns")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let rows = kind_props
        .get("rows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let on_row_click = {
        let component_id = component_id.clone();
        let daemon = daemon.clone();
        move |row_index: usize| {
            daemon.send_component_event(
                component_id.clone(),
                serde_json::json!({
                    "type": "row_clicked",
                    "payload": { "row_index": row_index },
                }),
            );
        }
    };

    let col_count = columns.len().max(1);
    let row_count = rows.len();
    let is_empty = rows.is_empty();

    view! {
        <div class="component-table">
            <table class="data-table">
                <thead>
                    <tr>
                        {columns.iter().map(|col| {
                            view! { <th>{cell_text(col)}</th> }
                        }).collect::<Vec<_>>()}
                    </tr>
                </thead>
                <tbody>
                    {rows.iter().enumerate().map(|(i, row)| {
                        let cells = row.as_array().cloned().unwrap_or_default();
                        let oc = on_row_click.clone();
                        view! {
                            <tr on:click=move |_| oc(i) class="data-table__row">
                                {cells.iter().map(|cell| {
                                    view! { <td>{cell_text(cell)}</td> }
                                }).collect::<Vec<_>>()}
                            </tr>
                        }
                    }).collect::<Vec<_>>()}
                    {is_empty.then(|| view! {
                        <tr><td class="data-table__empty" colspan=col_count.to_string()>"no rows"</td></tr>
                    })}
                </tbody>
            </table>
            {(!is_empty).then(|| view! {
                <div class="data-table__footer">{format!("{row_count} row{}", if row_count == 1 { "" } else { "s" })}</div>
            })}
        </div>
    }
}

/// Coerce any JSON value to display text. Strings render as-is; numbers and
/// bools render their literal form (a numeric cell like `73` must not vanish);
/// null/objects render empty.
fn cell_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// A progress bar. Props shape:
/// ```json
/// { "label": "Building...", "value": 0.6, "max": 1.0, "indeterminate": false }
/// ```
#[component]
fn ProgressView(kind_props: Value) -> impl IntoView {
    let label = kind_props
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let value = kind_props
        .get("value")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let max = kind_props
        .get("max")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let indeterminate = kind_props
        .get("indeterminate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pct = if max > 0.0 {
        (value / max * 100.0).round().clamp(0.0, 100.0) as i32
    } else {
        0
    };

    view! {
        <div class="component-progress">
            {if !label.is_empty() {
                view! { <div class="progress-label">{label.to_string()}</div> }.into_any()
            } else {
                ().into_any()
            }}
            <div class="progress-bar">
                <div
                    class:progress-bar__fill=true
                    class:is-indeterminate=indeterminate
                    style=format!("width: {pct}%")
                >
                    {if !indeterminate {
                        view! { <span class="progress-pct">{format!("{pct}%")}</span> }.into_any()
                    } else {
                        ().into_any()
                    }}
                </div>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// Renders markdown content as embedded HTML.
/// Props: `{ "content": "## Heading\n\nParagraph." }`
#[component]
fn MarkdownView(kind_props: Value) -> impl IntoView {
    let content = kind_props
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    view! {
        <div class="component-markdown" inner_html=crate::markdown::render(content)></div>
    }
}

// ---------------------------------------------------------------------------
// Dashboard (layout container)
// ---------------------------------------------------------------------------

/// A grid container for child components. Props shape:
/// ```json
/// { "children": [{ "id": "kanban-1", "width": 2 }, { "id": "progress-1", "width": 1 }] }
/// ```
/// Children are referenced by their component_id (rendered elsewhere in the
/// turn's block list). This view places them in a CSS grid.
#[component]
fn DashboardView(kind_props: Value, daemon: Daemon) -> impl IntoView {
    let children = kind_props
        .get("children")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Column track widths come from each child's `width` (CSS grid `fr` units,
    // per the render protocol). Default 1fr when omitted.
    let columns = children
        .iter()
        .map(|c| {
            let w = c.get("width").and_then(|v| v.as_f64()).unwrap_or(1.0).max(1.0);
            format!("{w}fr")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let columns = if columns.is_empty() {
        "1fr".to_string()
    } else {
        columns
    };

    view! {
        <div
            class="component-dashboard"
            style=format!(
                "display: grid; grid-template-columns: {columns}; gap: 12px;"
            )
        >
            {children.into_iter().map(|child| {
                let child_id = child.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                // A child may carry its own component spec inline (kind + props),
                // in which case we mount it directly. Otherwise it's a bare layout
                // placeholder referenced by id and rendered elsewhere.
                match child.get("kind").and_then(|v| v.as_str()) {
                    Some(kind) => {
                        let kind = kind.to_string();
                        let props = child.get("props").cloned().unwrap_or(Value::Null);
                        view! {
                            <div class="dashboard-cell dashboard-cell--filled">
                                <ComponentView
                                    component_id=child_id
                                    kind=kind
                                    kind_props=props
                                    daemon=daemon.clone()
                                />
                            </div>
                        }
                        .into_any()
                    }
                    None => view! {
                        <div class="dashboard-cell">{child_id}</div>
                    }
                    .into_any(),
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Tool Drawer — concealed strip that drops down to show recent tool activity
// ---------------------------------------------------------------------------

/// A tiny concealed drawer pinned to the bottom of the transcript. Shows a
/// small `▸` arrow tab; when clicked it slides open to reveal compact chips
/// for every tool call and thinking block in the current turn.
///
/// Props:
/// - `turns`: the turns signal (reads the latest assistant turn's blocks)
/// - `open`: RwSignal<bool> controlling open/closed state
#[component]
pub fn ToolDrawer(
    turns: RwSignal<Vec<Turn>>,
    open: RwSignal<bool>,
) -> impl IntoView {
    // Derive chips from the latest assistant turn's blocks.
    let chips = move || {
        turns.with(|t| {
            let mut out: Vec<(String, String, String)> = Vec::new(); // (icon, label, status)
            for turn in t.iter().rev() {
                if turn.role != Role::Assistant {
                    continue;
                }
                for block in &turn.blocks {
                    match block {
                        Block::Thinking { content, .. } => {
                            let chars = content.chars().count();
                            out.push((
                                "🧠".into(),
                                format!("thinking ({chars} chars)"),
                                "dim".into(),
                            ));
                        }
                        Block::ToolCall {
                            name,
                            status,
                            output,
                            ..
                        } => {
                            let icon = match status {
                                ToolStatus::Running => "◉",
                                ToolStatus::Ok => "✓",
                                ToolStatus::Err => "✗",
                            };
                            let status_class = match status {
                                ToolStatus::Running => "running",
                                ToolStatus::Ok => "ok",
                                ToolStatus::Err => "err",
                            };
                            let preview: String = output.chars().take(24).collect();
                            let label = if preview.is_empty() {
                                name.clone()
                            } else {
                                format!("{} ({})", name, preview.trim())
                            };
                            out.push((icon.into(), label, status_class.into()));
                        }
                        _ => {}
                    }
                }
                // Only the latest turn.
                if !out.is_empty() {
                    break;
                }
            }
            out
        })
    };

    let arrow = move || if open.get() { "▾" } else { "▸" };
    let count = move || chips().len();
    let is_open = move || open.get();
    let has_chips = move || !chips().is_empty();

    view! {
        <div class="tool-drawer" class:tool-drawer--open=is_open>
            <button
                class="tool-drawer__tab"
                on:click=move |_| open.update(|v| *v = !*v)
                title="toggle tool drawer"
            >
                <span class="tool-drawer__arrow">{arrow}</span>
                <span class="tool-drawer__label">"tools"</span>
                <span class="tool-drawer__count">{move || format!("({})", count())}</span>
            </button>

            <div class="tool-drawer__body">
                <Show when=is_open>
                    <div class="tool-drawer__chips">
                        {move || chips().into_iter().map(|(icon, label, status_class)| {
                            let cls = format!("tool-chip tool-chip--{status_class}");
                            view! {
                                <span class={cls}>
                                    <span class="tool-chip__icon">{icon}</span>
                                    <span class="tool-chip__label">{label}</span>
                                </span>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                    <Show when=move || !has_chips()>
                        <div class="tool-drawer__empty">"no tool calls yet"</div>
                    </Show>
                </Show>
            </div>
        </div>
    }
}
