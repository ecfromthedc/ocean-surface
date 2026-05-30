//! Renders live UI components that the agent emits via `component_render`.
//!
//! Each component kind maps to a Leptos view that reads the `props` JSON
//! and renders interactively. User interactions (button clicks, form submits,
//! card drags) are sent back to the daemon via `Daemon::send_component_event`,
//! which the agent's `component_wait` tool picks up.

use leptos::prelude::*;
use serde_json::Value;

use crate::daemon::Daemon;

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
                                            view! { <option value=val.clone()>{val}</option> }
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

    view! {
        <div class="component-table">
            <table class="data-table">
                <thead>
                    <tr>
                        {columns.iter().map(|col| {
                            let col = col.as_str().unwrap_or("");
                            view! { <th>{col.to_string()}</th> }
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
                                    let text = cell.as_str().unwrap_or("");
                                    view! { <td>{text.to_string()}</td> }
                                }).collect::<Vec<_>>()}
                            </tr>
                        }
                    }).collect::<Vec<_>>()}
                </tbody>
            </table>
        </div>
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
        (value / max * 100.0).round() as i32
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

    // Calculate grid template columns from widths.
    let total_width: f64 = children
        .iter()
        .map(|c| c.get("width").and_then(|v| v.as_f64()).unwrap_or(1.0))
        .sum();
    let total_width = total_width.max(1.0);

    view! {
        <div
            class="component-dashboard"
            style=format!(
                "display: grid; grid-template-columns: repeat(auto-fit, minmax(250px, 1fr)); gap: 12px;"
            )
        >
            {children.iter().map(|child| {
                let _child_id = child.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                // Dashboard children are rendered in-place by their component_id
                // elsewhere in the block tree. This container just provides layout.
                // In a richer implementation we'd mount sub-ComponentViews here.
                view! { <div class="dashboard-cell"></div> }
            }).collect::<Vec<_>>()}
        </div>
    }
}
