//! Renders live UI components that the agent emits via `component_render`.
//!
//! Each component kind maps to a Leptos view that reads the `props` JSON
//! and renders interactively. User interactions (button clicks, form submits,
//! card drags) are sent back to the daemon via `Daemon::send_component_event`,
//! which the agent's `component_wait` tool picks up.

use leptos::prelude::*;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

use crate::daemon::Daemon;
use crate::model::{Block, Role, ToolStatus, Turn};

#[wasm_bindgen]
extern "C" {
    /// Defined in index.html. Loads the Google Maps JS API + Places UI Kit
    /// (once, idempotent) using `key`, then renders/updates the map for
    /// component `container_id` from `props_json`. `map_id` selects the visual
    /// style. `on_event` is invoked with (event_name, json_payload) for
    /// marker/place selections, to relay back to the agent.
    #[wasm_bindgen(js_name = oceanRenderMap)]
    fn ocean_render_map(
        container_id: &str,
        key: &str,
        map_id: &str,
        props_json: &str,
        on_event: &JsValue,
    );

    /// Defined in index.html. Injects a TikTok/Instagram embed blockquote into
    /// `container_id` and loads/refreshes the platform embed script so it
    /// renders. `platform` is "tiktok" | "instagram".
    #[wasm_bindgen(js_name = oceanRenderSocialVideo)]
    fn ocean_render_social_video(container_id: &str, platform: &str, url: &str);
}

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
        "chart" => view! {
            <ChartView kind_props />
        }
        .into_any(),
        "timeline" => view! {
            <TimelineView kind_props />
        }
        .into_any(),
        "stat" => view! {
            <StatView kind_props />
        }
        .into_any(),
        "file_tree" => view! {
            <FileTreeView component_id kind_props daemon />
        }
        .into_any(),
        "diff" => view! {
            <DiffView kind_props />
        }
        .into_any(),
        "code" => view! {
            <CodeView kind_props />
        }
        .into_any(),
        "callout" => view! {
            <CalloutView kind_props />
        }
        .into_any(),
        "gallery" => view! {
            <GalleryView kind_props />
        }
        .into_any(),
        "confirm" => view! {
            <ConfirmView component_id kind_props daemon />
        }
        .into_any(),
        "map" => view! {
            <MapView component_id kind_props daemon />
        }
        .into_any(),
        "video" => view! {
            <VideoView component_id kind_props />
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
fn KanbanView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
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
fn FormView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
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
fn TableView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
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
            let w = c
                .get("width")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                .max(1.0);
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
// Chart — bar / line / sparkline from numeric series
// ---------------------------------------------------------------------------

/// A lightweight inline chart. Props shape:
/// ```json
/// { "title": "Plays", "type": "bar",
///   "series": [{ "label": "Mon", "value": 12 }, { "label": "Tue", "value": 30 }] }
/// ```
/// `type` is "bar" | "line" (line renders an SVG polyline). Pure CSS/SVG, no deps.
#[component]
fn ChartView(kind_props: Value) -> impl IntoView {
    let title = kind_props
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chart_type = kind_props
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("bar")
        .to_string();
    let series = kind_props
        .get("series")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let points: Vec<(String, f64)> = series
        .iter()
        .map(|p| {
            let label = p
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let value = p.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            (label, value)
        })
        .collect();
    let max = points
        .iter()
        .map(|(_, v)| *v)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    let body = if chart_type == "line" {
        let n = points.len().max(1);
        let coords: String = points
            .iter()
            .enumerate()
            .map(|(i, (_, v))| {
                let x = if n > 1 {
                    i as f64 / (n - 1) as f64 * 100.0
                } else {
                    0.0
                };
                let y = 100.0 - (v / max * 100.0);
                format!("{x:.2},{y:.2}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        view! {
            <svg class="chart-line" viewBox="0 0 100 100" preserveAspectRatio="none">
                <polyline points=coords fill="none" />
            </svg>
        }
        .into_any()
    } else {
        view! {
            <div class="chart-bars">
                {points.iter().map(|(label, v)| {
                    let h = (v / max * 100.0).round();
                    let label = label.clone();
                    let val = *v;
                    view! {
                        <div class="chart-bar">
                            <div class="chart-bar__track">
                                <div class="chart-bar__fill" style=format!("height: {h}%")>
                                    <span class="chart-bar__val">{val.to_string()}</span>
                                </div>
                            </div>
                            <span class="chart-bar__label">{label}</span>
                        </div>
                    }
                }).collect::<Vec<_>>()}
            </div>
        }
        .into_any()
    };

    view! {
        <div class="component-chart">
            {(!title.is_empty()).then(|| view! { <div class="component-chart__title">{title.clone()}</div> })}
            {body}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Timeline — ordered steps with status
// ---------------------------------------------------------------------------

/// A vertical timeline of steps. Props shape:
/// ```json
/// { "steps": [{ "label": "Plan", "status": "done", "detail": "approved" },
///             { "label": "Build", "status": "active" },
///             { "label": "Ship", "status": "pending" }] }
/// ```
/// status is "done" | "active" | "pending" | "error".
#[component]
fn TimelineView(kind_props: Value) -> impl IntoView {
    let steps = kind_props
        .get("steps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    view! {
        <div class="component-timeline">
            {steps.into_iter().map(|step| {
                let label = step.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let detail = step.get("detail").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let status = step.get("status").and_then(|v| v.as_str()).unwrap_or("pending").to_string();
                let dot = match status.as_str() {
                    "done" => "✓",
                    "active" => "◉",
                    "error" => "✗",
                    _ => "○",
                };
                view! {
                    <div class=format!("timeline-step timeline-step--{status}")>
                        <div class="timeline-step__rail">
                            <span class="timeline-step__dot">{dot}</span>
                        </div>
                        <div class="timeline-step__body">
                            <div class="timeline-step__label">{label}</div>
                            {(!detail.is_empty()).then(|| view! { <div class="timeline-step__detail">{detail.clone()}</div> })}
                        </div>
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Stat — row of KPI cards
// ---------------------------------------------------------------------------

/// A row of stat / KPI cards. Props shape:
/// ```json
/// { "stats": [{ "label": "Views", "value": "1.2M", "delta": "+12%", "trend": "up" }] }
/// ```
/// trend is "up" | "down" | "flat" (colors the delta).
#[component]
fn StatView(kind_props: Value) -> impl IntoView {
    let stats = kind_props
        .get("stats")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    view! {
        <div class="component-stats">
            {stats.into_iter().map(|s| {
                let label = s.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let value = cell_text(s.get("value").unwrap_or(&Value::Null));
                let delta = s.get("delta").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let trend = s.get("trend").and_then(|v| v.as_str()).unwrap_or("flat").to_string();
                view! {
                    <div class="stat-card">
                        <div class="stat-card__value">{value}</div>
                        <div class="stat-card__label">{label}</div>
                        {(!delta.is_empty()).then(|| view! {
                            <div class=format!("stat-card__delta stat-card__delta--{trend}")>{delta.clone()}</div>
                        })}
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

// ---------------------------------------------------------------------------
// File tree — collapsible directory tree, files emit clicks
// ---------------------------------------------------------------------------

/// A collapsible file/dir tree. Props shape:
/// ```json
/// { "root": "src/", "entries": [
///     { "name": "main.rs", "type": "file" },
///     { "name": "tools", "type": "dir", "children": [{ "name": "mod.rs", "type": "file" }] } ] }
/// ```
/// Clicking a file emits file_clicked { path }.
#[component]
fn FileTreeView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
    let root = kind_props
        .get("root")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entries = kind_props
        .get("entries")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    view! {
        <div class="component-filetree">
            {(!root.is_empty()).then(|| view! { <div class="filetree__root">{root.clone()}</div> })}
            <ul class="filetree__list">
                {entries.into_iter().map(|e| {
                    view! { <FileTreeNode entry=e depth=0 component_id=component_id.clone() daemon=daemon.clone() /> }
                }).collect::<Vec<_>>()}
            </ul>
        </div>
    }
}

#[component]
fn FileTreeNode(entry: Value, depth: usize, component_id: String, daemon: Daemon) -> impl IntoView {
    let name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_dir = entry.get("type").and_then(|v| v.as_str()) == Some("dir");
    let children = entry
        .get("children")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let indent = format!("padding-left: {}px", depth * 14 + 4);

    if is_dir {
        let open = RwSignal::new(depth == 0);
        let name_c = name.clone();
        let arrow = move || if open.get() { "▾" } else { "▸" };
        view! {
            <li class="filetree__node filetree__node--dir">
                <button class="filetree__row filetree__row--dir" type="button" style=indent
                    on:click=move |_| open.update(|v| *v = !*v)>
                    <span class="filetree__arrow">{arrow}</span>
                    <span class="filetree__icon">"📁"</span>
                    <span class="filetree__name">{name_c}</span>
                </button>
                <Show when=move || open.get()>
                    <ul class="filetree__list">
                        {children.clone().into_iter().map(|c| {
                            view! { <FileTreeNode entry=c depth=depth+1 component_id=component_id.clone() daemon=daemon.clone() /> }
                        }).collect::<Vec<_>>()}
                    </ul>
                </Show>
            </li>
        }
        .into_any()
    } else {
        let path = entry
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(&name)
            .to_string();
        let on_click = {
            let component_id = component_id.clone();
            let daemon = daemon.clone();
            let path = path.clone();
            move |_| {
                daemon.send_component_event(
                    component_id.clone(),
                    serde_json::json!({ "type": "file_clicked", "payload": { "path": path } }),
                );
            }
        };
        view! {
            <li class="filetree__node">
                <button class="filetree__row" type="button" style=indent on:click=on_click>
                    <span class="filetree__icon">"📄"</span>
                    <span class="filetree__name">{name}</span>
                </button>
            </li>
        }
        .into_any()
    }
}

// ---------------------------------------------------------------------------
// Diff — unified diff with +/- line coloring
// ---------------------------------------------------------------------------

/// A unified diff view. Props shape:
/// ```json
/// { "filename": "src/lib.rs",
///   "lines": [{ "kind": "ctx", "text": "fn main() {" },
///             { "kind": "del", "text": "  old();" },
///             { "kind": "add", "text": "  new();" }] }
/// ```
/// kind is "add" | "del" | "ctx". Alternatively pass `unified: "@@ ...\n+foo\n-bar"`.
#[component]
fn DiffView(kind_props: Value) -> impl IntoView {
    let filename = kind_props
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Either structured `lines`, or a raw `unified` string we parse by prefix.
    let lines: Vec<(String, String)> =
        if let Some(arr) = kind_props.get("lines").and_then(|v| v.as_array()) {
            arr.iter()
                .map(|l| {
                    let kind = l
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ctx")
                        .to_string();
                    let text = l
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    (kind, text)
                })
                .collect()
        } else if let Some(raw) = kind_props.get("unified").and_then(|v| v.as_str()) {
            raw.lines()
                .map(|l| {
                    let kind = match l.chars().next() {
                        Some('+') => "add",
                        Some('-') => "del",
                        Some('@') => "hunk",
                        _ => "ctx",
                    };
                    (kind.to_string(), l.to_string())
                })
                .collect()
        } else {
            Vec::new()
        };

    view! {
        <div class="component-diff">
            {(!filename.is_empty()).then(|| view! { <div class="diff__filename">{filename.clone()}</div> })}
            <pre class="diff__body">
                {lines.into_iter().map(|(kind, text)| {
                    let sym = match kind.as_str() { "add" => "+", "del" => "-", _ => " " };
                    view! {
                        <div class=format!("diff__line diff__line--{kind}")>
                            <span class="diff__gutter">{sym}</span>
                            <span class="diff__text">{text}</span>
                        </div>
                    }
                }).collect::<Vec<_>>()}
            </pre>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Code — syntax block with header + copy button
// ---------------------------------------------------------------------------

/// A code block with a language tag and copy-to-clipboard. Props shape:
/// ```json
/// { "language": "rust", "filename": "main.rs", "code": "fn main() {}" }
/// ```
#[component]
fn CodeView(kind_props: Value) -> impl IntoView {
    let language = kind_props
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let filename = kind_props
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let code = kind_props
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let header = if !filename.is_empty() {
        filename.clone()
    } else {
        language.clone()
    };
    let copied = RwSignal::new(false);
    let code_for_copy = code.clone();
    let on_copy = move |_| {
        if let Some(win) = web_sys::window() {
            let clip = win.navigator().clipboard();
            let _ = clip.write_text(&code_for_copy);
            copied.set(true);
        }
    };
    let copy_label = move || if copied.get() { "copied" } else { "copy" };

    view! {
        <div class="component-code">
            <div class="code__head">
                <span class="code__lang">{header}</span>
                <button class="code__copy" type="button" on:click=on_copy>{copy_label}</button>
            </div>
            <pre class="code__body"><code>{code}</code></pre>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Callout — colored info/warn/success/error banner
// ---------------------------------------------------------------------------

/// A colored callout banner. Props shape:
/// ```json
/// { "variant": "warn", "title": "Heads up", "body": "This is destructive." }
/// ```
/// variant is "info" | "success" | "warn" | "error".
#[component]
fn CalloutView(kind_props: Value) -> impl IntoView {
    let variant = kind_props
        .get("variant")
        .and_then(|v| v.as_str())
        .unwrap_or("info")
        .to_string();
    let title = kind_props
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = kind_props
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let icon = match variant.as_str() {
        "success" => "✓",
        "warn" => "⚠",
        "error" => "✗",
        _ => "ℹ",
    };

    view! {
        <div class=format!("component-callout component-callout--{variant}")>
            <span class="callout__icon">{icon}</span>
            <div class="callout__body">
                {(!title.is_empty()).then(|| view! { <div class="callout__title">{title.clone()}</div> })}
                {(!body.is_empty()).then(|| view! { <div class="callout__text" inner_html=crate::markdown::render(&body)></div> })}
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Gallery — image grid
// ---------------------------------------------------------------------------

/// An image gallery grid. Props shape:
/// ```json
/// { "images": [{ "src": "https://... or data:image/png;base64,..", "caption": "before" }] }
/// ```
#[component]
fn GalleryView(kind_props: Value) -> impl IntoView {
    let images = kind_props
        .get("images")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    view! {
        <div class="component-gallery">
            {images.into_iter().map(|img| {
                let src = img.get("src").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let caption = img.get("caption").and_then(|v| v.as_str()).unwrap_or("").to_string();
                view! {
                    <figure class="gallery__item">
                        <img class="gallery__img" src=src loading="lazy" />
                        {(!caption.is_empty()).then(|| view! { <figcaption class="gallery__cap">{caption.clone()}</figcaption> })}
                    </figure>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Confirm — yes/no prompt, emits the choice
// ---------------------------------------------------------------------------

/// A confirm prompt with two buttons. Props shape:
/// ```json
/// { "title": "Delete 10 files?", "body": "This cannot be undone.",
///   "confirm_label": "Delete", "cancel_label": "Cancel", "variant": "error" }
/// ```
/// Emits confirm_response { confirmed: bool }. variant colors the confirm button.
#[component]
fn ConfirmView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
    let title = kind_props
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Confirm")
        .to_string();
    let body = kind_props
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let confirm_label = kind_props
        .get("confirm_label")
        .and_then(|v| v.as_str())
        .unwrap_or("Confirm")
        .to_string();
    let cancel_label = kind_props
        .get("cancel_label")
        .and_then(|v| v.as_str())
        .unwrap_or("Cancel")
        .to_string();
    let variant = kind_props
        .get("variant")
        .and_then(|v| v.as_str())
        .unwrap_or("info")
        .to_string();

    let answered = RwSignal::new(false);
    let send = {
        let component_id = component_id.clone();
        let daemon = daemon.clone();
        move |confirmed: bool| {
            answered.set(true);
            daemon.send_component_event(
                component_id.clone(),
                serde_json::json!({ "type": "confirm_response", "payload": { "confirmed": confirmed } }),
            );
        }
    };
    let send_yes = send.clone();
    let send_no = send.clone();

    view! {
        <div class="component-confirm">
            <div class="confirm__title">{title}</div>
            {(!body.is_empty()).then(|| view! { <div class="confirm__body">{body.clone()}</div> })}
            <div class="confirm__actions">
                <button class="confirm__btn confirm__btn--cancel" type="button"
                    prop:disabled=move || answered.get()
                    on:click=move |_| send_no(false)>{cancel_label}</button>
                <button class=format!("confirm__btn confirm__btn--{variant}") type="button"
                    prop:disabled=move || answered.get()
                    on:click=move |_| send_yes(true)>{confirm_label}</button>
            </div>
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
pub fn ToolDrawer(turns: RwSignal<Vec<Turn>>, open: RwSignal<bool>) -> impl IntoView {
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

// ---------------------------------------------------------------------------
// Map — a live Google Map / Places UI Kit surface. Props:
//   { mode?: "markers"|"place"|"search",        // default inferred from fields
//     center?: {lat,lng}, zoom?,
//     markers?: [{lat,lng,title?}],              // markers mode
//     place_id?: "ChIJ...",                      // place mode (details card)
//     query?: "coffee in Austin",                // search mode (text search)
//     nearby?: {lat,lng,radius?,type?},          // search mode (nearby)
//     fit_markers? }
// Relays marker_clicked / place_selected back to the agent.
// ---------------------------------------------------------------------------
#[component]
fn MapView(component_id: String, kind_props: Value, daemon: Daemon) -> impl IntoView {
    let dom_id = format!("ocean-map-{}", sanitize_id(&component_id));
    let maps_key = daemon.maps_key;
    let maps_map_id = daemon.maps_map_id;

    // Selection callback → component event back to the agent. JS calls it with
    // (event_name: String, payload_json: String).
    let cid = component_id.clone();
    let daemon_cb = daemon.clone();
    let on_event =
        Closure::<dyn FnMut(String, String)>::new(move |event: String, payload: String| {
            let data = serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({}));
            daemon_cb.send_component_event(cid.clone(), json!({ "event": event, "data": data }));
        });
    // Leak so it stays callable from JS for the life of the map (maps are few
    // and long-lived; a small per-render leak is acceptable here).
    let on_event_js: JsValue = on_event.into_js_value();

    let props_str = kind_props.to_string();
    let dom_id_eff = dom_id.clone();
    Effect::new(move |_| {
        let key = maps_key.get();
        let map_id = maps_map_id.get();
        if key.trim().is_empty() {
            return; // config not loaded yet — effect re-runs when the key lands
        }
        let id = dom_id_eff.clone();
        let props = props_str.clone();
        let cb = on_event_js.clone();
        let mid = if map_id.trim().is_empty() {
            "DEMO_MAP_ID".to_string()
        } else {
            map_id
        };
        // Defer a frame so the container div exists in the DOM.
        request_animation_frame(move || {
            ocean_render_map(&id, &key, &mid, &props, &cb);
        });
    });

    view! {
        <div class="block block--map">
            <div id=dom_id class="ocean-map">
                <div class="ocean-map__loading">"loading map…"</div>
            </div>
        </div>
    }
}

/// Keep only chars safe for a DOM id.
fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Video — embed a clip inline. Props:
//   { url, title?, autoplay?, start? }
// `url` may be a TikTok / Instagram Reel / YouTube / Vimeo link, or a direct
// .mp4/.webm/.m3u8 file. The right embed is chosen from the URL.
// ---------------------------------------------------------------------------
#[derive(Clone)]
enum VideoKind {
    /// Plain iframe embed (YouTube, Vimeo) at this src.
    Iframe(String),
    /// Direct media file → <video> element.
    File(String),
    /// Social embed (TikTok / Instagram) needing the platform embed script.
    /// Carries (platform, canonical_url).
    Social(&'static str, String),
    /// Couldn't classify — show the raw link.
    Unknown(String),
}

fn classify_video(url: &str, start: i64) -> VideoKind {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();

    // Direct media files.
    if lower.ends_with(".mp4")
        || lower.ends_with(".webm")
        || lower.ends_with(".mov")
        || lower.ends_with(".m3u8")
        || lower.ends_with(".ogg")
    {
        return VideoKind::File(u.to_string());
    }

    // YouTube → privacy-friendly nocookie embed.
    if let Some(id) = youtube_id(&lower, u) {
        let mut src = format!("https://www.youtube-nocookie.com/embed/{id}");
        if start > 0 {
            src.push_str(&format!("?start={start}"));
        }
        return VideoKind::Iframe(src);
    }

    // Vimeo → player.vimeo.com/video/<id>.
    if lower.contains("vimeo.com") {
        if let Some(id) = u
            .rsplit('/')
            .find(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
        {
            return VideoKind::Iframe(format!("https://player.vimeo.com/video/{id}"));
        }
    }

    // TikTok / Instagram → social embed via their script.
    if lower.contains("tiktok.com") {
        return VideoKind::Social("tiktok", u.to_string());
    }
    if lower.contains("instagram.com") {
        return VideoKind::Social("instagram", u.to_string());
    }

    VideoKind::Unknown(u.to_string())
}

/// Pull a YouTube video id from common URL shapes.
fn youtube_id(lower: &str, raw: &str) -> Option<String> {
    if lower.contains("youtu.be/") {
        return raw
            .split("youtu.be/")
            .nth(1)
            .map(|s| s.split(['?', '&', '/']).next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty());
    }
    if lower.contains("youtube.com") {
        // watch?v=ID
        if let Some(rest) = raw.split("v=").nth(1) {
            let id = rest.split('&').next().unwrap_or("").to_string();
            if !id.is_empty() {
                return Some(id);
            }
        }
        // /embed/ID or /shorts/ID
        for marker in ["/embed/", "/shorts/"] {
            if let Some(rest) = raw.split(marker).nth(1) {
                let id = rest.split(['?', '&', '/']).next().unwrap_or("").to_string();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

#[component]
fn VideoView(component_id: String, kind_props: Value) -> impl IntoView {
    let url = kind_props
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = kind_props
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let autoplay = kind_props
        .get("autoplay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let start = kind_props
        .get("start")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if url.trim().is_empty() {
        return view! { <div class="block block--video"><div class="video-empty">"(no video url)"</div></div> }.into_any();
    }

    let kind = classify_video(&url, start);

    // Social embeds (TikTok/IG) are injected + processed by their script via JS glue.
    if let VideoKind::Social(platform, canon) = &kind {
        let dom_id = format!("ocean-video-{}", sanitize_id(&component_id));
        let platform = *platform;
        let canon = canon.clone();
        let dom_id_eff = dom_id.clone();
        Effect::new(move |_| {
            let id = dom_id_eff.clone();
            let p = platform.to_string();
            let c = canon.clone();
            request_animation_frame(move || {
                ocean_render_social_video(&id, &p, &c);
            });
        });
        return view! {
            <div class="block block--video">
                {(!title.is_empty()).then(|| view!{ <div class="video__title">{title.clone()}</div> })}
                <div id=dom_id class="video-embed video-embed--social">
                    <div class="video-embed__loading">"loading video…"</div>
                </div>
            </div>
        }.into_any();
    }

    let body = match kind {
        VideoKind::Iframe(src) => {
            // Build the iframe as raw HTML to sidestep macro attr limitations
            // (frameborder/allowfullscreen/allow). src is provider-derived, not
            // user free-text, but escape quotes defensively.
            let safe = src.replace('"', "%22");
            let html = format!(
                "<iframe src=\"{safe}\" frameborder=\"0\" allowfullscreen \
                 allow=\"accelerometer; autoplay; clipboard-write; encrypted-media; \
                 gyroscope; picture-in-picture; web-share\"></iframe>"
            );
            view! { <div class="video-embed video-embed--16x9" inner_html=html></div> }.into_any()
        }
        VideoKind::File(src) => view! {
            <div class="video-embed">
                <video
                    src=src
                    controls=true
                    autoplay=autoplay
                    playsinline=true
                    class="video-file"
                ></video>
            </div>
        }
        .into_any(),
        VideoKind::Unknown(u) => {
            let href = u.clone();
            view! {
                <div class="video-embed video-embed--unknown">
                    <a href=href target="_blank" rel="noopener">{u}</a>
                </div>
            }
            .into_any()
        }
        VideoKind::Social(_, _) => unreachable!(),
    };

    view! {
        <div class="block block--video">
            {(!title.is_empty()).then(|| view!{ <div class="video__title">{title.clone()}</div> })}
            {body}
        </div>
    }
    .into_any()
}

/// Permission-approval overlay (OCEAN-64).
///
/// When the daemon runs with permission-gating on, a mutating tool call
/// (write / edit / bash) BLOCKS until the operator posts a decision. The daemon
/// emits a `permission_request` on the control stream; `Daemon` collects them in
/// `pending_permissions`. This renders one prominent card per pending request —
/// stacked, oldest first — each with Approve / Deny. Clicking POSTs the decision
/// and clears the card; a decision made elsewhere (e.g. the TUI) clears it via
/// the `permission_decision` frame. A pending request blocks the turn, so the
/// stack is fixed at the bottom of the viewport above the composer and can't be
/// scrolled away.
#[component]
pub fn PermissionPrompts(daemon: Daemon) -> impl IntoView {
    let pending = daemon.pending_permissions;
    let daemon = StoredValue::new(daemon);

    view! {
        <Show when=move || !pending.get().is_empty()>
            <div class="ocean-perms" role="region" aria-label="permission requests">
                <For
                    each=move || pending.get()
                    key=|p| p.permission_id.clone()
                    children=move |p| {
                        let allow_id = p.permission_id.clone();
                        let deny_id = p.permission_id.clone();
                        let deciding = p.deciding;
                        let has_args = !p.args_summary.trim().is_empty();
                        let tool = p.tool.clone();
                        let reason = p.reason.clone();
                        let args_summary = p.args_summary.clone();
                        view! {
                            <div class="ocean-perm" class:is-deciding=move || deciding>
                                <div class="ocean-perm__head">
                                    <span class="ocean-perm__badge">"permission"</span>
                                    <span class="ocean-perm__tool">{tool}</span>
                                </div>
                                <div class="ocean-perm__reason">{reason}</div>
                                {has_args.then(|| view! {
                                    <pre class="ocean-perm__args">{args_summary.clone()}</pre>
                                })}
                                <div class="ocean-perm__actions">
                                    <button
                                        class="ocean-perm__deny"
                                        type="button"
                                        disabled=deciding
                                        on:click=move |_| daemon.with_value(|d| {
                                            d.decide_permission(deny_id.clone(), false)
                                        })
                                    >
                                        "Deny"
                                    </button>
                                    <button
                                        class="ocean-perm__approve"
                                        type="button"
                                        disabled=deciding
                                        on:click=move |_| daemon.with_value(|d| {
                                            d.decide_permission(allow_id.clone(), true)
                                        })
                                    >
                                        {move || if deciding { "…" } else { "Approve" }}
                                    </button>
                                </div>
                            </div>
                        }
                    }
                />
            </div>
        </Show>
    }
}
