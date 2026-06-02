use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Div, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, FontStyle, FontWeight,
    GlobalElementId, Hsla, InteractiveElement, IntoElement, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render, ScrollWheelEvent,
    ScrollHandle, ShapedLine, SharedString, StatefulInteractiveElement, Style, Styled, Task,
    TextRun, Timer, UTF16Selection, UnderlineStyle, Window, div, fill, font, point, px, relative,
    size, svg,
};

use super::agent::{AgentBlock, AgentEvent, AgentRole, AgentState, ToolStatus};
use super::commands::{CommandSpec, ShellCommand, filtered_commands};
use super::daemon::{
    AgentTurnRequest, AgentTurnResponse, DaemonClient, DaemonHealth, NativeDaemonState,
};
use super::editor_buffer::EditorCursor;
use super::editor_layout::{
    EDITOR_FALLBACK_WRAP_WIDTH_PX, EDITOR_LINE_HEIGHT_PX, EditorLineStyle, EditorRenderLine,
    EditorViewport, EditorVisualLayout, EditorVisualLine, byte_offset_for_char_column,
    char_column_for_byte_index,
};
use super::icons::ShellIcon;
use super::model::{EditorTab, FileEntry, FileKind, NoteSearchResult, OutlineItem, ShellState};
use super::theme;
use super::vault_index::Backlink;
use super::watcher::{VaultWatchEvent, VaultWatcher};

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(160);
const WATCH_EVENT_BATCH_LIMIT: usize = 128;
const DAEMON_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(120);
const AGENT_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const AGENT_EVENT_BATCH_LIMIT: usize = 128;
const AGENT_STICKY_BOTTOM_THRESHOLD_PX: f32 = 48.0;
const VISUAL_CURSOR_SCROLL_MARGIN: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceTab {
    Agent,
    Vault,
}

impl SurfaceTab {
    fn label(self) -> &'static str {
        match self {
            SurfaceTab::Agent => "Agent",
            SurfaceTab::Vault => "Vault",
        }
    }

    fn id(self) -> usize {
        match self {
            SurfaceTab::Agent => 0,
            SurfaceTab::Vault => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisualRowBoundary {
    Start,
    End,
}

#[derive(Clone, Debug)]
enum AgentStreamMessage {
    Event(AgentEvent),
    Error(String),
}

#[derive(Clone, Debug)]
enum AgentSubmitMessage {
    Response(AgentTurnResponse),
    Error(String),
}

pub struct OceanGuiShell {
    active_surface: SurfaceTab,
    state: ShellState,
    agent: AgentState,
    daemon: NativeDaemonState,
    agent_focus: FocusHandle,
    agent_scroll: ScrollHandle,
    editor_focus: FocusHandle,
    editor_bounds: Option<Bounds<Pixels>>,
    editor_visual_scroll_row: usize,
    editor_scroll_path: Option<PathBuf>,
    editor_layout_cache: EditorLayoutCache,
    editor_shape_cache: EditorShapeCache,
    command_palette: Option<CommandPaletteState>,
    watcher: Option<VaultWatcher>,
    watch_task: Option<Task<()>>,
    daemon_health_task: Option<Task<()>>,
    agent_event_task: Option<Task<()>>,
    agent_submit_task: Option<Task<()>>,
}

impl OceanGuiShell {
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor_focus = cx.focus_handle().tab_stop(true);
        let agent_focus = cx.focus_handle().tab_stop(true);
        window.focus(&agent_focus);

        let mut shell = Self {
            active_surface: SurfaceTab::Agent,
            state: ShellState::seed(),
            agent: AgentState::default(),
            daemon: NativeDaemonState::from_env(),
            agent_focus,
            agent_scroll: ScrollHandle::new(),
            editor_focus,
            editor_bounds: None,
            editor_visual_scroll_row: 0,
            editor_scroll_path: None,
            editor_layout_cache: EditorLayoutCache::default(),
            editor_shape_cache: EditorShapeCache::default(),
            command_palette: None,
            watcher: None,
            watch_task: None,
            daemon_health_task: None,
            agent_event_task: None,
            agent_submit_task: None,
        };
        shell.restart_watcher(cx);
        shell.refresh_daemon_health(cx);
        shell.connect_agent_events(cx);
        shell
    }

    fn icon(&self, icon: ShellIcon, color: Hsla, size: f32) -> impl IntoElement {
        svg().path(icon.path()).size(px(size)).text_color(color)
    }

    fn copper_rule(&self) -> Div {
        div().h(px(2.0)).bg(theme::accent())
    }

    fn agent_status_dot(&self) -> Div {
        div()
            .w(px(7.0))
            .h(px(7.0))
            .bg(if self.agent.streaming {
                theme::user()
            } else if matches!(&self.daemon.health, DaemonHealth::Ready(health) if health.ok) {
                theme::accent()
            } else {
                theme::danger()
            })
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_label = match self.active_surface {
            SurfaceTab::Agent => self.agent.status.clone(),
            SurfaceTab::Vault => self.state.active_label(),
        };

        div()
            .flex()
            .flex_col()
            .bg(theme::frame())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(52.0))
                    .px_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::accent_dark())
                                    .child(self.icon(ShellIcon::Editor, theme::accent(), 14.0))
                                    .child("Ocean"),
                            )
                            .child(self.render_surface_tabs(cx))
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(active_label),
                            ),
                    )
                    .child(self.render_top_toolbar(cx)),
            )
            .child(self.copper_rule())
    }

    fn render_surface_tabs(&self, cx: &mut Context<Self>) -> Div {
        [SurfaceTab::Agent, SurfaceTab::Vault]
            .into_iter()
            .fold(div().flex().items_center().gap_3(), |tabs, surface| {
                tabs.child(self.render_surface_tab(surface, cx))
            })
    }

    fn render_surface_tab(&self, surface: SurfaceTab, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.active_surface == surface;
        div()
            .id(("surface-tab", surface.id()))
            .h(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .bg(theme::frame())
            .border_b(px(2.0))
            .border_color(if selected {
                theme::accent()
            } else {
                theme::frame()
            })
            .font_family(theme::MONO_FONT)
            .text_xs()
            .font_weight(if selected {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .text_color(if selected {
                theme::accent_dark()
            } else {
                theme::muted()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, window, cx| {
                shell.active_surface = surface;
                if surface == SurfaceTab::Agent {
                    shell.command_palette = None;
                    window.focus(&shell.agent_focus);
                } else {
                    window.focus(&shell.editor_focus);
                }
                cx.notify();
            }))
            .child(surface.label())
    }

    fn render_top_toolbar(&self, cx: &mut Context<Self>) -> Div {
        match self.active_surface {
            SurfaceTab::Agent => self.render_agent_toolbar(cx),
            SurfaceTab::Vault => self.render_vault_toolbar(cx),
        }
    }

    fn render_agent_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.toolbar_button("Health", cx, |shell, cx| {
                shell.refresh_daemon_health(cx);
                cx.notify();
            }))
            .child(self.toolbar_button("Stream", cx, |shell, cx| {
                shell.connect_agent_events(cx);
                cx.notify();
            }))
            .child(self.health_dot())
    }

    fn render_vault_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.toolbar_button("Cmd", cx, |shell, cx| {
                shell.open_command_palette();
                cx.notify();
            }))
            .child(self.toolbar_button("Open", cx, |shell, cx| {
                shell.open_workspace_with_dialog(cx);
                cx.notify();
            }))
            .child(self.toolbar_button("New", cx, |shell, cx| {
                shell.state.create_note();
                shell.reset_editor_scroll();
                cx.notify();
            }))
            .child(self.toolbar_button("Rename", cx, |shell, cx| {
                shell.rename_selected_with_dialog();
                cx.notify();
            }))
            .child(self.toolbar_button("Del", cx, |shell, cx| {
                shell.delete_selected_with_confirmation();
                cx.notify();
            }))
            .child(self.toolbar_button("Reveal", cx, |shell, cx| {
                shell.state.reveal_selected();
                cx.notify();
            }))
            .child(self.toolbar_button("Refresh", cx, |shell, cx| {
                shell.state.refresh_files();
                cx.notify();
            }))
            .child(self.toolbar_button("Edit", cx, |shell, cx| {
                shell.state.open_active_external();
                cx.notify();
            }))
            .child(self.toolbar_button("Reload", cx, |shell, cx| {
                shell.state.reload_active();
                shell.reset_editor_scroll();
                cx.notify();
            }))
            .child(self.toolbar_button("Save", cx, |shell, cx| {
                shell.state.save_active();
                cx.notify();
            }))
            .child(div().w(px(7.0)).h(px(7.0)).bg(theme::green()))
    }

    fn health_dot(&self) -> Div {
        let color = match self.daemon.health {
            DaemonHealth::Checking => theme::rule(),
            DaemonHealth::Ready(_) => theme::green(),
            DaemonHealth::Offline(_) => theme::danger(),
        };

        div().w(px(7.0)).h(px(7.0)).bg(color)
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        match self.active_surface {
            SurfaceTab::Agent => self.render_agent_workspace(window, cx),
            SurfaceTab::Vault => self.render_vault_workspace(window, cx),
        }
    }

    fn render_vault_workspace(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .child(self.render_file_tree(cx))
            .child(self.render_editor(window, cx))
            .child(self.render_inspector(cx))
    }

    fn render_agent_workspace(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::background())
            .child(self.render_agent_sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .bg(theme::paper())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(46.0))
                            .px_4()
                            .bg(theme::frame())
                            .border_b(px(1.0))
                            .border_color(theme::rule())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::accent_dark())
                                    .child(self.agent_status_dot())
                                    .child("Agent"),
                            )
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(self.daemon.url.clone()),
                            ),
                    )
                    .child(self.render_agent_transcript())
                    .child(self.render_agent_composer(window, cx)),
            )
    }

    fn render_agent_sidebar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .w(px(224.0))
            .flex_shrink_0()
            .h_full()
            .bg(theme::panel())
            .border_r(px(1.0))
            .border_color(theme::rule())
            .child(self.panel_header(ShellIcon::Vault, "Ocean"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .child(self.agent_metric_row("health", self.daemon.status_label()))
                    .child(self.agent_metric_row("backend", self.daemon.backend_label()))
                    .child(self.agent_metric_row(
                        "session",
                        self.agent
                            .session_id
                            .clone()
                            .unwrap_or_else(|| "new".to_string()),
                    ))
                    .child(self.agent_metric_row(
                        "model",
                        self.agent
                            .model
                            .clone()
                            .unwrap_or_else(|| "pending".to_string()),
                    ))
                    .child(self.agent_metric_row("vault", self.state.root_label())),
            )
            .child(self.panel_header(ShellIcon::Report, "Surfaces"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(self.surface_row(SurfaceTab::Agent, cx))
                    .child(self.surface_row(SurfaceTab::Vault, cx)),
            )
    }

    fn surface_row(&self, surface: SurfaceTab, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.active_surface == surface;
        div()
            .id(("surface-row", surface.id()))
            .flex()
            .items_center()
            .justify_between()
            .h(px(30.0))
            .px_2()
            .bg(if selected {
                theme::paper()
            } else {
                theme::panel()
            })
            .border_1()
            .border_color(if selected {
                theme::rule_strong()
            } else {
                theme::panel()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, window, cx| {
                shell.active_surface = surface;
                if surface == SurfaceTab::Agent {
                    shell.command_palette = None;
                    window.focus(&shell.agent_focus);
                } else {
                    window.focus(&shell.editor_focus);
                }
                cx.notify();
            }))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if selected {
                        theme::accent_dark()
                    } else {
                        theme::ink()
                    })
                    .child(surface.label()),
            )
            .child(if selected {
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::accent())
                    .child("*")
            } else {
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("")
            })
    }

    fn agent_metric_row(&self, label: &'static str, value: impl Into<String>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .min_h(px(28.0))
            .border_b(px(1.0))
            .border_color(theme::rule().opacity(0.42))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(label),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::ink())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(value.into()),
            )
    }

    fn render_agent_transcript(&self) -> Div {
        let mut transcript = div()
            .id("agent-transcript-scroll")
            .flex()
            .flex_col()
            .gap_0()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .px_6()
            .py_3()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .scrollbar_width(px(6.0))
            .track_scroll(&self.agent_scroll);

        if self.agent.turns.is_empty() {
            transcript = transcript
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(34.0))
                        .border_b(px(1.0))
                        .border_color(theme::rule().opacity(0.42))
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .text_color(theme::muted())
                        .child("daemon")
                        .child(self.daemon.status_label()),
                );
        } else {
            for (turn_index, turn) in self.agent.turns.iter().enumerate() {
                transcript = transcript.child(self.render_agent_turn(turn_index, turn));
            }
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .overflow_hidden()
            .bg(theme::paper())
            .child(transcript)
    }

    fn render_agent_turn(
        &self,
        index: usize,
        turn: &super::agent::AgentTurn,
    ) -> impl IntoElement {
        let (label, color) = match turn.role {
            AgentRole::User => ("USER", theme::user()),
            AgentRole::Assistant => ("OCEAN", theme::accent()),
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w(px(0.0))
            .overflow_x_hidden();
        for (block_index, block) in turn.blocks.iter().enumerate() {
            body = body.child(self.render_agent_block(index, block_index, block));
        }

        div()
            .id(("agent-turn", index))
            .flex()
            .gap_4()
            .min_w(px(0.0))
            .w_full()
            .overflow_x_hidden()
            .py_4()
            .border_b(px(1.0))
            .border_color(theme::rule().opacity(0.42))
            .child(
                div()
                    .w(px(58.0))
                    .flex_shrink_0()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(label),
            )
            .child(body)
    }

    fn render_agent_block(
        &self,
        turn_index: usize,
        block_index: usize,
        block: &AgentBlock,
    ) -> impl IntoElement {
        let block_dom_id = turn_index.saturating_mul(1000).saturating_add(block_index);
        match block {
            AgentBlock::Text(text) => div()
                .id(("agent-text", block_dom_id))
                .w_full()
                .min_w(px(0.0))
                .overflow_x_hidden()
                .whitespace_normal()
                .line_height(px(22.0))
                .font_family(theme::UI_FONT)
                .text_size(px(14.5))
                .text_color(theme::ink())
                .child(text.clone()),
            AgentBlock::Thinking { content, .. } => div()
                .id(("agent-thinking", block_dom_id))
                .w_full()
                .min_w(px(0.0))
                .overflow_x_hidden()
                .whitespace_normal()
                .line_height(px(18.0))
                .pl_3()
                .py_1()
                .border_l(px(2.0))
                .border_color(theme::rule())
                .font_family(theme::MONO_FONT)
                .text_xs()
                .text_color(theme::thinking())
                .child(content.clone()),
            AgentBlock::ToolCall {
                name,
                output,
                status,
                ..
            } => {
                let (status_label, color) = match status {
                    ToolStatus::Running => ("running", theme::user()),
                    ToolStatus::Ok => ("ok", theme::green()),
                    ToolStatus::Err => ("err", theme::danger()),
                };
                div()
                    .id(("agent-tool", block_dom_id))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w_full()
                    .min_w(px(0.0))
                    .overflow_x_hidden()
                    .pl_3()
                    .py_2()
                    .border_l(px(2.0))
                    .border_color(theme::rule())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .child(div().text_color(theme::ink()).child(name.clone()))
                            .child(div().text_color(color).child(status_label)),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .w_full()
                            .min_w(px(0.0))
                            .overflow_x_hidden()
                            .whitespace_normal()
                            .line_height(px(18.0))
                            .child(if output.is_empty() {
                                String::from("waiting for output")
                            } else {
                                output.clone()
                            }),
                    )
            }
            AgentBlock::Component {
                component_id, kind, ..
            } => div()
                .id(("agent-component", block_dom_id))
                .w_full()
                .min_w(px(0.0))
                .overflow_x_hidden()
                .whitespace_normal()
                .pl_3()
                .py_2()
                .border_l(px(2.0))
                .border_color(theme::rule())
                .font_family(theme::MONO_FONT)
                .text_xs()
                .text_color(theme::accent())
                .child(format!("{kind} {component_id}")),
        }
    }

    fn render_agent_composer(&self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let focused = self.agent_focus.is_focused(_window);
        let prompt = if self.agent.composer_text.is_empty() {
            "ask Ocean".to_string()
        } else {
            self.agent.composer_text.clone()
        };

        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(58.0))
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_t(px(1.0))
            .border_color(theme::rule())
            .track_focus(&self.agent_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event: &MouseDownEvent, window, cx| {
                    window.focus(&shell.agent_focus);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::on_agent_composer_key_down))
            .child(
                div()
                    .flex_1()
                    .min_h(px(36.0))
                    .px_3()
                    .py_2()
                    .bg(theme::background())
                    .border_1()
                    .border_color(if focused {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(if self.agent.composer_text.is_empty() {
                        theme::muted()
                    } else {
                        theme::ink()
                    })
                    .child(prompt),
            )
            .child(
                div()
                    .id("agent-send")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(64.0))
                    .h(px(36.0))
                    .bg(if self.agent.can_submit() {
                        theme::accent()
                    } else {
                        theme::panel()
                    })
                    .border_1()
                    .border_color(if self.agent.can_submit() {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if self.agent.can_submit() {
                        theme::background()
                    } else {
                        theme::muted()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.submit_agent_prompt(cx);
                        cx.notify();
                    }))
                    .child(if self.agent.streaming { "..." } else { "Send" }),
            )
    }

    fn render_file_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = div().flex().flex_col().gap_1().p_2();

        for file in &self.state.files {
            rows = rows.child(self.render_file_row(file, cx));
        }

        div()
            .flex()
            .flex_col()
            .w(px(240.0))
            .h_full()
            .bg(theme::panel())
            .border_r(px(1.0))
            .border_color(theme::rule())
            .child(self.panel_header(ShellIcon::Files, &self.state.root_label()))
            .child(rows)
    }

    fn render_file_row(&self, file: &FileEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.selected_path.as_ref() == Some(&file.path);
        let color = if selected {
            theme::accent_dark()
        } else {
            theme::ink()
        };
        let icon = match file.kind {
            FileKind::Folder => ShellIcon::Files,
            FileKind::Markdown => ShellIcon::Editor,
        };
        let disclosure = match file.kind {
            FileKind::Folder if file.has_children && file.expanded => "v",
            FileKind::Folder if file.has_children => ">",
            FileKind::Folder | FileKind::Markdown => " ",
        };
        let file_id = file.id;

        div()
            .id(("file", file.id))
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .px_2()
            .bg(if selected {
                theme::paper()
            } else {
                theme::panel()
            })
            .border_1()
            .border_color(if selected {
                theme::rule_strong()
            } else {
                theme::panel()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.state.set_active_file(file_id);
                shell.sync_editor_scroll_path();
                cx.notify();
            }))
            .child(div().w(px(file.depth as f32 * 14.0)))
            .child(
                div()
                    .w(px(10.0))
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if file.kind == FileKind::Folder {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .child(disclosure),
            )
            .child(self.icon(icon, color, 14.0))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(color)
                    .child(file.label.clone()),
            )
    }

    fn render_editor(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .bg(theme::background())
            .child(self.render_tabs(cx))
            .child(self.render_buffer(window, cx))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div()
            .flex()
            .items_end()
            .gap_1()
            .h(px(44.0))
            .px_3()
            .pt_2()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule());

        for (index, tab) in self.state.tabs.iter().enumerate() {
            tabs = tabs.child(self.render_tab(index, tab, cx));
        }

        tabs
    }

    fn render_tab(
        &self,
        tab_index: usize,
        tab: &EditorTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.state.active_path.as_ref() == Some(&tab.path);

        let mut tab_view = div()
            .id(("tab", tab_index))
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.0))
            .px_3()
            .bg(if selected {
                theme::paper()
            } else {
                theme::panel()
            })
            .border_1()
            .border_color(if selected {
                theme::rule_strong()
            } else {
                theme::rule()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.state.set_active_tab(tab_index);
                shell.sync_editor_scroll_path();
                cx.notify();
            }))
            .child(self.icon(ShellIcon::Editor, theme::accent(), 13.0))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if selected {
                        theme::ink()
                    } else {
                        theme::muted()
                    })
                    .child(tab.label.clone()),
            );

        if tab.dirty {
            tab_view = tab_view.child(div().w(px(6.0)).h(px(6.0)).bg(theme::accent()));
        }

        tab_view.child(
            div()
                .id(("close-tab", tab_index))
                .px_1()
                .font_family(theme::MONO_FONT)
                .text_xs()
                .text_color(theme::muted())
                .hover(|style| style.bg(theme::background()).cursor_pointer())
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.state.close_tab(tab_index);
                    cx.notify();
                }))
                .child("x"),
        )
    }

    fn render_buffer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.active_path.is_none() {
            return div()
                .id("empty-editor-buffer")
                .flex()
                .flex_col()
                .flex_1()
                .mx_3()
                .mb_3()
                .items_center()
                .justify_center()
                .bg(theme::paper())
                .border_1()
                .border_color(theme::rule_strong())
                .child(
                    div()
                        .font_family(theme::SERIF_FONT)
                        .text_size(px(28.0))
                        .text_color(theme::accent_dark())
                        .child("No file open"),
                )
                .child(
                    div()
                        .mt_2()
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .text_color(theme::muted())
                        .child("Open or create a markdown note"),
                );
        }

        let cursor = self.state.cursor_position();
        let editor_focused = self.editor_focus.is_focused(window);
        let has_selection = self.state.selection_range().is_some();
        let lines = self.visible_render_lines();

        div()
            .id("editor-buffer")
            .flex()
            .flex_col()
            .flex_1()
            .mx_3()
            .mb_3()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule_strong())
            .overflow_hidden()
            .key_context("MarkdownEditor")
            .track_focus(&self.editor_focus)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, event: &MouseDownEvent, window, cx| {
                    window.focus(&shell.editor_focus);
                    let (line, column) =
                        shell.line_column_from_editor_point(event.position, window);
                    if event.click_count == 1
                        && event.modifiers.platform
                        && shell.state.open_wikilink_at_line_column(line, column)
                    {
                        shell.reset_editor_scroll();
                    } else if event.click_count >= 2 {
                        shell.state.select_word_at_line_column(line, column);
                    } else {
                        shell.state.move_cursor_to_line_column(line, column);
                    }
                    shell.reveal_editor_cursor(window);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(Self::on_editor_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_editor_scroll_wheel))
            .on_key_down(cx.listener(Self::on_editor_key_down))
            .child(EditorSurfaceElement {
                shell: cx.entity(),
                lines,
                cursor,
                visual_scroll_row: self.editor_visual_scroll_row,
                show_cursor: editor_focused && !has_selection,
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(EditorInputElement {
                        shell: cx.entity(),
                        focus_handle: self.editor_focus.clone(),
                    }),
            )
    }

    fn on_editor_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() || !self.editor_focus.is_focused(window) {
            return;
        }

        let (line, column) = self.line_column_from_editor_point(event.position, window);
        self.state.extend_cursor_to_line_column(line, column);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_editor_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pixel_delta = event.delta.pixel_delta(px(EDITOR_LINE_HEIGHT_PX));
        let line_delta = scroll_line_delta_from_pixels(pixel_delta.y / px(1.0));

        if line_delta != 0 && self.scroll_editor_by_visual_rows(line_delta, window) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn line_column_from_editor_point(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
    ) -> (usize, usize) {
        let bounds = self
            .editor_bounds
            .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(0.0))));
        let viewport = EditorViewport::from_surface(bounds);
        let position = viewport.clamp_to_text(position);
        let layout = self.visible_editor_layout(viewport.wrap_width(), window);
        let visible_capacity = viewport.visible_row_capacity();
        let scroll_row = layout.clamp_scroll_row(self.editor_visual_scroll_row, visible_capacity);
        self.editor_visual_scroll_row = scroll_row;
        let row = scroll_row + viewport.row_for_point(position);
        let Some(visual_line) = layout
            .visual_line_at_row(row)
            .or_else(|| layout.lines.last())
        else {
            return (self.state.document_start_line, 0);
        };
        let x = viewport.x_in_text(position);
        let relative_column = if visual_line.text.is_empty() || x <= px(0.0) {
            0
        } else {
            let key = EditorShapeKey::visual_line(visual_line);
            let shaped = self.editor_shape_cache.shape_line(key, window);
            char_column_for_byte_index(&visual_line.text, shaped.closest_index_for_x(x))
        };
        let column = visual_line.source_columns.start
            + relative_column
                .min(visual_line.source_columns.end - visual_line.source_columns.start);

        (visual_line.document_line_index, column)
    }

    fn on_editor_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;

        if self.command_palette.is_some() {
            self.handle_command_palette_key(event, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }

        if modifiers.secondary() && !modifiers.alt && keystroke.key.as_str() == "p" {
            self.open_command_palette();
            cx.stop_propagation();
            cx.notify();
            return;
        }

        if modifiers.secondary() && !modifiers.alt {
            let handled = match keystroke.key.as_str() {
                "s" => {
                    self.state.save_active();
                    true
                }
                "a" => {
                    self.state.select_all();
                    true
                }
                "c" => {
                    if let Some(selected) = self.state.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected));
                        self.state.status_message = String::from("Copied selection");
                    }
                    true
                }
                "x" => {
                    if let Some(selected) = self.state.take_selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected));
                    }
                    true
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.state.insert_text(&text);
                    }
                    true
                }
                "z" => {
                    if modifiers.shift {
                        self.state.redo();
                    } else {
                        self.state.undo();
                    }
                    true
                }
                "y" => {
                    self.state.redo();
                    true
                }
                _ => false,
            };

            if handled {
                self.reveal_editor_cursor(window);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

        if modifiers.secondary() && !modifiers.alt {
            let handled = match keystroke.key.as_str() {
                "n" => {
                    self.state.create_note();
                    self.reset_editor_scroll();
                    true
                }
                "o" => {
                    self.open_workspace_with_dialog(cx);
                    true
                }
                "r" => {
                    if modifiers.shift {
                        self.state.refresh_files();
                    } else {
                        self.state.reload_active();
                        self.reset_editor_scroll();
                    }
                    true
                }
                "backspace" | "delete" => {
                    self.delete_selected_with_confirmation();
                    true
                }
                _ => false,
            };

            if handled {
                self.reveal_editor_cursor(window);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

        let handled = match keystroke.key.as_str() {
            "backspace" => {
                self.state.delete_backward();
                true
            }
            "delete" => {
                self.state.delete_forward();
                true
            }
            "enter" => {
                self.state.insert_newline();
                true
            }
            "tab" => {
                self.state.insert_tab();
                true
            }
            "left" => {
                if modifiers.shift {
                    self.state.extend_cursor_left();
                } else {
                    self.state.move_cursor_left();
                }
                true
            }
            "right" => {
                if modifiers.shift {
                    self.state.extend_cursor_right();
                } else {
                    self.state.move_cursor_right();
                }
                true
            }
            "up" => {
                if modifiers.shift {
                    self.move_cursor_by_visual_row(-1, true, window);
                } else {
                    self.move_cursor_by_visual_row(-1, false, window);
                }
                true
            }
            "down" => {
                if modifiers.shift {
                    self.move_cursor_by_visual_row(1, true, window);
                } else {
                    self.move_cursor_by_visual_row(1, false, window);
                }
                true
            }
            "home" => {
                self.move_cursor_to_visual_row_boundary(
                    VisualRowBoundary::Start,
                    modifiers.shift,
                    window,
                );
                true
            }
            "end" => {
                self.move_cursor_to_visual_row_boundary(
                    VisualRowBoundary::End,
                    modifiers.shift,
                    window,
                );
                true
            }
            _ => false,
        };

        if handled {
            self.reveal_editor_cursor(window);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn open_workspace_with_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = rfd::FileDialog::new().pick_folder() {
            self.state.set_workspace_root(root);
            self.reset_editor_scroll();
            self.restart_watcher(cx);
        }
    }

    fn open_command_palette(&mut self) {
        self.command_palette = Some(CommandPaletteState::default());
    }

    fn on_agent_composer_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.secondary() && !modifiers.alt && key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.agent.insert_composer_text(&text);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let handled = match key {
            "enter" if !modifiers.shift => {
                self.submit_agent_prompt(cx);
                true
            }
            "enter" => {
                self.agent.insert_composer_text("\n");
                true
            }
            "backspace" | "delete" => {
                self.agent.delete_composer_backward();
                true
            }
            _ => {
                if let Some(text) = command_palette_text(event) {
                    self.agent.insert_composer_text(&text);
                    true
                } else {
                    false
                }
            }
        };

        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn submit_agent_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.agent.take_prompt_for_submit() else {
            return;
        };

        let request = AgentTurnRequest {
            prompt,
            cwd: self.state.root.display().to_string(),
            session_id: self.agent.session_id.clone(),
            client_type: Some("surface-gpui".to_string()),
        };
        self.agent_scroll.scroll_to_bottom();
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.submit_turn(&url, &request))
                .map(AgentSubmitMessage::Response)
                .unwrap_or_else(AgentSubmitMessage::Error);
            let _ = sender.send(result);
        });

        self.agent_submit_task = Some(spawn_agent_submit_task(receiver, cx));
    }

    fn connect_agent_events(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::sync_channel(512);
        self.agent.status = "connecting stream".to_string();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| {
                client.stream_agent_events(&url, |event| {
                    sender
                        .send(AgentStreamMessage::Event(event))
                        .map_err(|error| error.to_string())
                })
            });

            if let Err(error) = result {
                let _ = sender.send(AgentStreamMessage::Error(error));
            }
        });

        self.agent_event_task = Some(spawn_agent_event_task(receiver, cx));
    }

    fn apply_agent_stream_messages(&mut self, messages: Vec<AgentStreamMessage>) {
        let should_stick_to_bottom = self.should_stick_agent_transcript_to_bottom();
        let mut accepted_event = false;

        for message in messages {
            match message {
                AgentStreamMessage::Event(event) => {
                    accepted_event |= self.apply_agent_event(event);
                }
                AgentStreamMessage::Error(error) => {
                    self.agent.status = format!("stream error: {error}");
                }
            }
        }

        if accepted_event && should_stick_to_bottom {
            self.agent_scroll.scroll_to_bottom();
        }
    }

    fn should_stick_agent_transcript_to_bottom(&self) -> bool {
        should_stick_to_bottom(
            self.agent_scroll.max_offset().height,
            self.agent_scroll.offset().y,
        )
    }

    fn apply_agent_submit_message(&mut self, message: AgentSubmitMessage) {
        match message {
            AgentSubmitMessage::Response(response) if response.ok => {
                if self.agent.session_id.is_none() {
                    self.agent.session_id = Some(response.session_id);
                }
                self.agent.status = response.status;
            }
            AgentSubmitMessage::Response(response) => {
                self.agent.mark_post_error(
                    response
                        .error
                        .unwrap_or_else(|| format!("turn {}", response.status)),
                );
            }
            AgentSubmitMessage::Error(error) => self.agent.mark_post_error(error),
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) -> bool {
        let event_session_id = event.session_id().map(str::to_string);
        let adoption_event = matches!(
            event,
            AgentEvent::SessionCreated { .. } | AgentEvent::TurnStarted { .. }
        );

        if let Some(event_session_id) = event_session_id.as_deref() {
            match self.agent.session_id.as_deref() {
                Some(current) if current != event_session_id => {
                    if !(adoption_event && self.agent.streaming) {
                        return false;
                    }
                }
                None => {
                    if !(adoption_event && self.agent.streaming) {
                        return false;
                    }
                }
                _ => {}
            }
        }

        self.agent.apply_event(event);
        true
    }

    fn handle_command_palette_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let key = keystroke.key.as_str();
        let mut entry_to_run = None;

        if modifiers.secondary() && !modifiers.alt && key == "p" {
            self.command_palette = None;
            return;
        }

        match key {
            "escape" => {
                self.command_palette = None;
            }
            "enter" => {
                entry_to_run = self
                    .command_palette
                    .as_ref()
                    .and_then(|palette| palette.selected_entry(&self.state));
                self.command_palette = None;
            }
            "up" => {
                let entry_count = self
                    .command_palette
                    .as_ref()
                    .map(|palette| palette.entry_count(&self.state))
                    .unwrap_or(0);
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.move_selection(-1, entry_count);
                }
            }
            "down" => {
                let entry_count = self
                    .command_palette
                    .as_ref()
                    .map(|palette| palette.entry_count(&self.state))
                    .unwrap_or(0);
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.move_selection(1, entry_count);
                }
            }
            "backspace" => {
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.delete_backward();
                }
            }
            "delete" => {
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.clear();
                }
            }
            _ => {
                if let Some(text) = command_palette_text(event)
                    && let Some(palette) = self.command_palette.as_mut()
                {
                    palette.insert_text(&text);
                }
            }
        }

        if let Some(entry) = entry_to_run {
            self.execute_palette_entry(entry, cx);
        }
    }

    fn execute_palette_entry(&mut self, entry: PaletteEntry, cx: &mut Context<Self>) {
        match entry {
            PaletteEntry::Command(command) => self.execute_command(command.kind, cx),
            PaletteEntry::Note(note) => {
                self.state.open_note_path(note.path);
                self.sync_editor_scroll_path();
            }
        }
    }

    fn execute_command(&mut self, command: ShellCommand, cx: &mut Context<Self>) {
        match command {
            ShellCommand::OpenVault => self.open_workspace_with_dialog(cx),
            ShellCommand::NewNote => {
                self.state.create_note();
                self.reset_editor_scroll();
            }
            ShellCommand::RenameNote => self.rename_selected_with_dialog(),
            ShellCommand::DeleteNote => self.delete_selected_with_confirmation(),
            ShellCommand::RevealNote => self.state.reveal_selected(),
            ShellCommand::RefreshVault => self.state.refresh_files(),
            ShellCommand::EditExternal => self.state.open_active_external(),
            ShellCommand::ReloadNote => {
                self.state.reload_active();
                self.reset_editor_scroll();
            }
            ShellCommand::SaveNote => self.state.save_active(),
        }
    }

    fn restart_watcher(&mut self, cx: &mut Context<Self>) {
        self.watch_task = None;
        self.watcher = None;

        match VaultWatcher::start(&self.state.root) {
            Ok((watcher, receiver)) => {
                self.watcher = Some(watcher);
                self.watch_task = Some(spawn_watch_task(receiver, cx));
            }
            Err(error) => {
                self.state.status_message = format!("Watcher unavailable: {error}");
            }
        }
    }

    fn refresh_daemon_health(&mut self, cx: &mut Context<Self>) {
        self.daemon.mark_checking();

        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let health = DaemonClient::new()
                .map(|client| client.health(&url))
                .unwrap_or_else(DaemonHealth::Offline);
            let _ = sender.send(health);
        });

        self.daemon_health_task = Some(spawn_daemon_health_task(receiver, cx));
    }

    fn apply_daemon_health(&mut self, health: DaemonHealth) {
        self.daemon.apply_health(health);
    }

    fn apply_watch_events(&mut self, events: Vec<VaultWatchEvent>) {
        let mut paths = Vec::new();
        for event in events {
            for path in event.paths {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }

        self.state.apply_external_vault_change(&paths);
    }

    fn rename_selected_with_dialog(&mut self) {
        let Some(source) = self.state.selected_note_path() else {
            self.state.status_message = String::from("Select a note to rename");
            return;
        };
        let Some(parent) = source.parent() else {
            self.state.status_message = String::from("Cannot rename this note");
            return;
        };
        let file_name = source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| String::from("untitled.md"));

        if let Some(target) = rfd::FileDialog::new()
            .set_directory(parent)
            .set_file_name(file_name)
            .save_file()
        {
            self.state.rename_selected_to(target);
        }
    }

    fn delete_selected_with_confirmation(&mut self) {
        let Some(source) = self.state.selected_note_path() else {
            self.state.status_message = String::from("Select a note to delete");
            return;
        };
        let file_name = source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| String::from("selected note"));

        let result = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Delete note")
            .set_description(format!("Delete {file_name}?"))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();

        if matches!(result, rfd::MessageDialogResult::Yes) {
            self.state.delete_selected_note();
        }
    }

    fn render_command_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(palette) = self.command_palette.as_ref() else {
            return div();
        };
        let entries = palette.entries(&self.state);
        let mut list = div().flex().flex_col().gap_1().p_2();

        if entries.is_empty() {
            list = list.child(
                div()
                    .h(px(34.0))
                    .px_2()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("No result"),
            );
        } else {
            for (index, entry) in entries.iter().cloned().enumerate() {
                list = list.child(self.render_palette_row(index, entry, palette.selected, cx));
            }
        }

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .bg(theme::ink().opacity(0.16))
            .child(
                div()
                    .absolute()
                    .top(px(96.0))
                    .left(px(360.0))
                    .right(px(360.0))
                    .bg(theme::paper())
                    .border_1()
                    .border_color(theme::rule_strong())
                    .child(self.copper_rule())
                    .child(
                        div()
                            .h(px(48.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b(px(1.0))
                            .border_color(theme::rule())
                            .font_family(theme::MONO_FONT)
                            .text_color(theme::ink())
                            .child(
                                div()
                                    .text_color(theme::accent_dark())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(">"),
                            )
                            .child(if palette.query.is_empty() {
                                div().text_color(theme::muted()).child("find")
                            } else {
                                div().text_color(theme::ink()).child(palette.query.clone())
                            })
                            .child(div().w(px(7.0)).h(px(18.0)).bg(theme::accent())),
                    )
                    .child(list),
            )
    }

    fn render_palette_row(
        &self,
        index: usize,
        entry: PaletteEntry,
        selected_index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = index == selected_index;
        let entry_to_run = entry.clone();
        let (icon, label, right_label, color) = match entry {
            PaletteEntry::Command(command) => (
                ShellIcon::Report,
                command.label.to_string(),
                command.shortcut.to_string(),
                theme::ink(),
            ),
            PaletteEntry::Note(note) => (
                ShellIcon::Editor,
                note.label,
                note.parent_label,
                theme::accent_dark(),
            ),
        };

        div()
            .id(("palette-entry", index))
            .flex()
            .items_center()
            .justify_between()
            .h(px(34.0))
            .px_2()
            .bg(if selected {
                theme::panel_raised()
            } else {
                theme::paper()
            })
            .border_1()
            .border_color(if selected {
                theme::rule_strong()
            } else {
                theme::paper()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.command_palette = None;
                shell.execute_palette_entry(entry_to_run.clone(), cx);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if selected {
                        theme::accent_dark()
                    } else {
                        color
                    })
                    .child(self.icon(icon, theme::accent(), 13.0))
                    .child(label),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(right_label),
            )
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut outline = div().flex().flex_col().gap_1().p_3();
        for (index, item) in self.state.outline.iter().enumerate() {
            outline = outline.child(self.render_outline_item(index, item, cx));
        }

        let mut links = div().flex().flex_col().gap_1().p_3();
        for (index, link) in self.state.links.iter().enumerate() {
            links = links.child(self.render_link_row(index, link, cx));
        }

        let mut backlinks = div().flex().flex_col().gap_1().p_3();
        for (index, backlink) in self.state.backlinks.iter().enumerate() {
            backlinks = backlinks.child(self.render_backlink_row(index, backlink, cx));
        }

        div()
            .flex()
            .flex_col()
            .w(px(280.0))
            .h_full()
            .bg(theme::panel())
            .border_l(px(1.0))
            .border_color(theme::rule())
            .child(self.panel_header(ShellIcon::Inspector, "Outline"))
            .child(outline)
            .child(self.panel_header(ShellIcon::Vault, "Links"))
            .child(links)
            .child(self.panel_header(ShellIcon::Vault, "Backlinks"))
            .child(backlinks)
            .child(self.panel_header(ShellIcon::Report, "Properties"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .child(self.stat_row("words", self.state.status.words))
                    .child(self.stat_row("lines", self.state.status.lines))
                    .child(self.stat_row("links", self.state.status.links))
                    .child(self.stat_row("refs", self.state.status.backlinks)),
            )
    }

    fn render_outline_item(
        &self,
        index: usize,
        item: &OutlineItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(("outline", index))
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.0))
            .px_2()
            .bg(theme::panel())
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, window, cx| {
                if shell.state.jump_to_outline_item(index) {
                    window.focus(&shell.editor_focus);
                }
                cx.notify();
            }))
            .child(div().w(px(f32::from(item.level.saturating_sub(1)) * 14.0)))
            .child(div().w(px(7.0)).h(px(7.0)).bg(theme::accent()))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::ink())
                    .child(format!("{}  {}", item.label, item.line_number)),
            )
    }

    fn render_link_row(
        &self,
        index: usize,
        link: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let link = link.to_string();
        let link_to_open = link.clone();
        div()
            .id(("link", index))
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.0))
            .px_2()
            .bg(theme::panel())
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.state.open_or_create_wikilink(&link_to_open);
                cx.notify();
            }))
            .child(self.icon(ShellIcon::Editor, theme::accent(), 12.0))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::ink())
                    .child(link),
            )
    }

    fn render_backlink_row(
        &self,
        index: usize,
        backlink: &Backlink,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = backlink.path.clone();
        div()
            .id(("backlink", index))
            .flex()
            .flex_col()
            .gap_2()
            .min_h(px(48.0))
            .px_2()
            .py_2()
            .bg(theme::panel())
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.state.open_note_path(path.clone());
                shell.sync_editor_scroll_path();
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::ink())
                            .child(self.icon(ShellIcon::Editor, theme::accent(), 12.0))
                            .child(backlink.label.clone()),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!("L{}", backlink.line_number)),
                    ),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(backlink.snippet.clone()),
            )
    }

    fn stat_row(&self, label: &'static str, value: usize) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_2()
            .bg(theme::panel())
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(label),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::ink())
                    .child(value.to_string()),
            )
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let right_label = match self.active_surface {
            SurfaceTab::Agent => format!(
                "daemon {}  backend {}  session {}",
                self.daemon.status_label(),
                self.daemon.backend_label(),
                self.agent
                    .session_id
                    .as_deref()
                    .unwrap_or("new")
            ),
            SurfaceTab::Vault => {
                let status = &self.state.status;
                format!(
                    "{} words  {} lines  {} links  {} refs  {} rendered",
                    status.words,
                    status.lines,
                    status.links,
                    status.backlinks,
                    status.rendered_lines
                )
            }
        };
        let left_label = match self.active_surface {
            SurfaceTab::Agent => {
                if self.agent.streaming {
                    "streaming".to_string()
                } else {
                    self.agent.status.clone()
                }
            }
            SurfaceTab::Vault => self.state.status_message.clone(),
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_3()
            .bg(theme::frame())
            .border_t(px(1.0))
            .border_color(theme::rule())
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::muted())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(left_label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(right_label),
            )
    }

    fn toolbar_button(
        &self,
        label: &'static str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut OceanGuiShell, &mut Context<OceanGuiShell>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(label)
            .px_2()
            .py_1()
            .bg(theme::frame())
            .border_1()
            .border_color(theme::frame())
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::accent_dark())
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| handler(shell, cx)))
            .child(label)
    }

    fn panel_header(&self, icon: ShellIcon, title: &str) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .px_3()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule())
            .child(self.icon(icon, theme::accent(), 14.0))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent_dark())
                    .child(title.to_string()),
            )
    }

    fn visible_render_lines(&self) -> Vec<EditorRenderLine> {
        self.state
            .document_lines
            .iter()
            .take(self.state.status.rendered_lines)
            .enumerate()
            .map(|(index, line)| {
                let document_line_index = self.state.document_start_line + index;
                EditorRenderLine {
                    document_line_index,
                    text: line.clone(),
                    selected_columns: self.state.selected_columns_for_line(document_line_index),
                    style: EditorLineStyle::for_text(line),
                }
            })
            .collect()
    }

    fn visible_editor_layout(&mut self, wrap_width: Pixels, window: &Window) -> EditorVisualLayout {
        let lines = self.visible_render_lines();
        self.editor_layout_cache
            .layout_for_lines(&lines, wrap_width, window)
    }

    fn move_cursor_by_visual_row(
        &mut self,
        row_delta: isize,
        extend_selection: bool,
        window: &Window,
    ) {
        let cursor = self.state.cursor_position();
        let wrap_width = self.current_wrap_width();
        let layout = self.visible_editor_layout(wrap_width, window);

        if let Some(target) = layout.cursor_after_visual_delta(cursor, row_delta) {
            if extend_selection {
                self.state
                    .extend_cursor_to_line_column(target.line, target.column);
            } else {
                self.state
                    .move_cursor_to_line_column(target.line, target.column);
            }
            return;
        }

        match (row_delta, extend_selection) {
            (-1, true) => self.state.extend_cursor_up(),
            (-1, false) => self.state.move_cursor_up(),
            (1, true) => self.state.extend_cursor_down(),
            (1, false) => self.state.move_cursor_down(),
            _ => {}
        }
    }

    fn move_cursor_to_visual_row_boundary(
        &mut self,
        boundary: VisualRowBoundary,
        extend_selection: bool,
        window: &Window,
    ) {
        let cursor = self.state.cursor_position();
        let wrap_width = self.current_wrap_width();
        let layout = self.visible_editor_layout(wrap_width, window);
        let target = match boundary {
            VisualRowBoundary::Start => layout.visual_row_start_for_cursor(cursor),
            VisualRowBoundary::End => layout.visual_row_end_for_cursor(cursor),
        };

        if let Some(target) = target {
            if extend_selection {
                self.state
                    .extend_cursor_to_line_column(target.line, target.column);
            } else {
                self.state
                    .move_cursor_to_line_column(target.line, target.column);
            }
            return;
        }

        match boundary {
            VisualRowBoundary::Start if !extend_selection => self.state.move_cursor_to_start(),
            VisualRowBoundary::End if !extend_selection => self.state.move_cursor_to_end(),
            VisualRowBoundary::Start | VisualRowBoundary::End => {}
        }
    }

    fn sync_editor_scroll_path(&mut self) {
        if self.editor_scroll_path != self.state.active_path {
            self.reset_editor_scroll();
        }
    }

    fn reset_editor_scroll(&mut self) {
        self.editor_scroll_path = self.state.active_path.clone();
        self.editor_visual_scroll_row = 0;
    }

    fn scroll_editor_by_visual_rows(&mut self, row_delta: isize, window: &mut Window) -> bool {
        let step_count = row_delta.unsigned_abs().min(240);
        let direction = if row_delta.is_negative() { -1 } else { 1 };
        let mut changed = false;

        for _ in 0..step_count {
            if !self.scroll_editor_visual_row_once(direction, window) {
                break;
            }
            changed = true;
        }

        changed
    }

    fn scroll_editor_visual_row_once(&mut self, direction: isize, window: &Window) -> bool {
        let viewport = self.current_editor_viewport();
        let layout = self.visible_editor_layout(viewport.wrap_width(), window);
        let visible_capacity = viewport.visible_row_capacity();
        let current_row = layout.clamp_scroll_row(self.editor_visual_scroll_row, visible_capacity);
        self.editor_visual_scroll_row = current_row;

        if direction.is_positive() {
            let max_row = layout.max_scroll_row(visible_capacity);
            if current_row < max_row {
                return self.set_editor_top_visual_row(&layout, current_row + 1);
            }

            let next_document_line = self.state.document_start_line.saturating_add(1);
            if self.state.set_document_start_line(next_document_line) {
                self.editor_visual_scroll_row = 0;
                return true;
            }

            return false;
        }

        if current_row > 0 {
            return self.set_editor_top_visual_row(&layout, current_row - 1);
        }

        let Some(previous_document_line) = self.state.document_start_line.checked_sub(1) else {
            return false;
        };

        if !self.state.set_document_start_line(previous_document_line) {
            return false;
        }

        let layout = self.visible_editor_layout(viewport.wrap_width(), window);
        let last_row = layout
            .last_visual_row_for_document_line(self.state.document_start_line)
            .unwrap_or(0);
        self.editor_visual_scroll_row = layout.clamp_scroll_row(last_row, visible_capacity);
        true
    }

    fn reveal_editor_cursor(&mut self, window: &Window) -> bool {
        let viewport = self.current_editor_viewport();
        let layout = self.visible_editor_layout(viewport.wrap_width(), window);
        let visible_capacity = viewport.visible_row_capacity();
        let current_row = layout.clamp_scroll_row(self.editor_visual_scroll_row, visible_capacity);
        self.editor_visual_scroll_row = current_row;

        let Some((cursor_row, _)) = layout.visual_line_for_cursor(self.state.cursor_position())
        else {
            return false;
        };

        let next_row = layout.scroll_row_to_reveal_row(
            cursor_row,
            current_row,
            visible_capacity,
            VISUAL_CURSOR_SCROLL_MARGIN,
        );

        if next_row == current_row {
            return false;
        }

        self.set_editor_top_visual_row(&layout, next_row)
    }

    fn set_editor_top_visual_row(&mut self, layout: &EditorVisualLayout, row: usize) -> bool {
        let Some(anchor) = layout.anchor_for_visual_row(row) else {
            self.editor_visual_scroll_row = 0;
            return false;
        };

        let changed_document_line = self
            .state
            .set_document_start_line(anchor.document_line_index);
        let changed_visual_row = self.editor_visual_scroll_row != anchor.local_visual_row;
        self.editor_visual_scroll_row = anchor.local_visual_row;

        changed_document_line || changed_visual_row
    }

    fn current_editor_viewport(&self) -> EditorViewport {
        let bounds = self
            .editor_bounds
            .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(0.0))));
        EditorViewport::from_surface(bounds)
    }

    fn current_wrap_width(&self) -> Pixels {
        self.editor_bounds
            .map(EditorViewport::from_surface)
            .map(|viewport| viewport.wrap_width())
            .unwrap_or_else(|| px(EDITOR_FALLBACK_WRAP_WIDTH_PX))
    }
}

impl EntityInputHandler for OceanGuiShell {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        Some(self.state.text_for_utf16_range(range, adjusted_range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (range, reversed) = self.state.selected_utf16_range();
        Some(UTF16Selection { range, reversed })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.state.marked_utf16_range()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.unmark_text();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_some() {
            cx.notify();
            return;
        }

        if self.state.replace_text_in_utf16_range(range, text) {
            self.reveal_editor_cursor(window);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_some() {
            cx.notify();
            return;
        }

        if self
            .state
            .replace_and_mark_text_in_utf16_range(range, new_text, new_selected_range)
        {
            self.reveal_editor_cursor(window);
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (start, end) = self.state.editor_cursors_for_utf16_range(range_utf16);
        let viewport = EditorViewport::from_surface(element_bounds);
        let layout = self.visible_editor_layout(viewport.wrap_width(), window);
        let visible_capacity = viewport.visible_row_capacity();
        let scroll_row = layout.clamp_scroll_row(self.editor_visual_scroll_row, visible_capacity);
        self.editor_visual_scroll_row = scroll_row;
        let (cursor_row, visual_line) = layout.visual_line_for_cursor(start)?;
        if cursor_row < scroll_row || cursor_row >= scroll_row.saturating_add(visible_capacity) {
            return None;
        }

        let visible_row = cursor_row - scroll_row;
        let y = viewport.line_y(visible_row);
        let shaped = self
            .editor_shape_cache
            .shape_line(EditorShapeKey::visual_line(visual_line), window);
        let start_column = visual_line.relative_column_for_source_column(start.column);
        let start_x = viewport.clamp_text_x(
            viewport.text_origin().x + x_for_char_column(&shaped, &visual_line.text, start_column),
        );
        let end_x = if start.line == end.line && visual_line.contains_source_column(end.column) {
            let end_column = visual_line.relative_column_for_source_column(end.column);
            viewport.clamp_text_x(
                viewport.text_origin().x
                    + x_for_char_column(&shaped, &visual_line.text, end_column),
            )
        } else {
            start_x + px(2.0)
        };
        let width = (end_x - start_x).max(px(2.0));

        Some(Bounds::new(
            point(start_x, y + px(2.0)),
            size(width, px(EDITOR_LINE_HEIGHT_PX - 4.0)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let (line, column) = self.line_column_from_editor_point(point, window);
        Some(self.state.utf16_index_for_line_column(line, column))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkdownRunKind {
    Plain,
    Link,
    WikiLink,
    Code,
    Bold,
    Italic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MarkdownRun {
    range: Range<usize>,
    kind: MarkdownRunKind,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EditorShapeKey {
    line_index: usize,
    text: String,
    style: EditorLineStyle,
}

impl EditorShapeKey {
    fn visual_line(line: &EditorVisualLine) -> Self {
        Self {
            line_index: line.document_line_index,
            text: line.text.clone(),
            style: line.style,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditorLayoutCacheKey {
    wrap_width_px: u32,
    lines: Vec<EditorLayoutLineKey>,
}

impl EditorLayoutCacheKey {
    fn new(lines: &[EditorRenderLine], wrap_width: Pixels) -> Self {
        Self {
            wrap_width_px: pixel_cache_key(wrap_width),
            lines: lines.iter().map(EditorLayoutLineKey::render_line).collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditorLayoutLineKey {
    document_line_index: usize,
    text: String,
    selected_columns: Option<Range<usize>>,
    style: EditorLineStyle,
}

impl EditorLayoutLineKey {
    fn render_line(line: &EditorRenderLine) -> Self {
        Self {
            document_line_index: line.document_line_index,
            text: line.text.clone(),
            selected_columns: line.selected_columns.clone(),
            style: line.style,
        }
    }
}

#[derive(Default)]
struct EditorLayoutCache {
    key: Option<EditorLayoutCacheKey>,
    layout: Option<EditorVisualLayout>,
}

impl EditorLayoutCache {
    fn layout_for_lines(
        &mut self,
        lines: &[EditorRenderLine],
        wrap_width: Pixels,
        window: &Window,
    ) -> EditorVisualLayout {
        let key = EditorLayoutCacheKey::new(lines, wrap_width);
        if self.key.as_ref() == Some(&key)
            && let Some(layout) = &self.layout
        {
            return layout.clone();
        }

        let layout = EditorVisualLayout::from_render_lines(lines, wrap_width, window);
        self.key = Some(key);
        self.layout = Some(layout.clone());
        layout
    }
}

fn pixel_cache_key(width: Pixels) -> u32 {
    let width_px = width / px(1.0);
    if width_px.is_finite() && width_px > 0.0 {
        width_px.round() as u32
    } else {
        0
    }
}

#[derive(Default)]
struct EditorShapeCache {
    lines: HashMap<EditorShapeKey, ShapedLine>,
}

impl EditorShapeCache {
    fn shape_line(&mut self, key: EditorShapeKey, window: &Window) -> ShapedLine {
        if let Some(shaped) = self.lines.get(&key) {
            return shaped.clone();
        }

        let shaped = shape_editor_text_line(&key.text, key.style, window);
        self.lines.insert(key, shaped.clone());
        shaped
    }

    fn prune_visible(&mut self, visible_keys: &[EditorShapeKey]) {
        self.lines.retain(|key, _| visible_keys.contains(key));
    }
}

struct EditorSurfaceElement {
    shell: Entity<OceanGuiShell>,
    lines: Vec<EditorRenderLine>,
    cursor: EditorCursor,
    visual_scroll_row: usize,
    show_cursor: bool,
}

impl IntoElement for EditorSurfaceElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorSurfaceElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let viewport = EditorViewport::from_surface(bounds);
        let line_height = px(EDITOR_LINE_HEIGHT_PX);
        let visible_capacity = viewport.visible_row_capacity();
        let (visible_lines, shaped_lines) = self.shell.update(cx, |shell, _| {
            let layout = shell.editor_layout_cache.layout_for_lines(
                &self.lines,
                viewport.wrap_width(),
                window,
            );
            let visual_scroll_row =
                layout.clamp_scroll_row(self.visual_scroll_row, visible_capacity);
            let visible_lines = layout
                .visible_lines_from(visual_scroll_row, visible_capacity)
                .to_vec();
            let visible_keys = visible_lines
                .iter()
                .map(EditorShapeKey::visual_line)
                .collect::<Vec<_>>();
            shell.editor_shape_cache.prune_visible(&visible_keys);

            let shaped_lines = visible_keys
                .into_iter()
                .map(|key| shell.editor_shape_cache.shape_line(key, window))
                .collect::<Vec<_>>();
            (visible_lines, shaped_lines)
        });

        window.with_content_mask(
            Some(ContentMask {
                bounds: viewport.surface_bounds,
            }),
            |window| {
                window.paint_layer(bounds, |window| {
                    for (row_index, (line, shaped)) in
                        visible_lines.iter().zip(shaped_lines.iter()).enumerate()
                    {
                        let y = viewport.line_y(row_index);
                        if line.source_columns.start == 0 {
                            paint_editor_line_number(
                                line.document_line_index + 1,
                                viewport.gutter_x,
                                y,
                                window,
                                cx,
                            );
                        } else {
                            paint_editor_continuation_marker(viewport.gutter_x, y, window, cx);
                        }

                        window.with_content_mask(
                            Some(ContentMask {
                                bounds: viewport.text_bounds,
                            }),
                            |window| {
                                if let Some(selection) = &line.selected_columns {
                                    let x = viewport.text_origin().x
                                        + x_for_char_column(shaped, &line.text, selection.start);
                                    let end_x = viewport.text_origin().x
                                        + x_for_char_column(shaped, &line.text, selection.end);
                                    let width = (end_x - x).max(px(2.0));
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(x, y + px(2.0)),
                                            size(width, px(EDITOR_LINE_HEIGHT_PX - 4.0)),
                                        ),
                                        theme::accent().opacity(0.22),
                                    ));
                                }

                                if self.show_cursor
                                    && self.cursor.line == line.document_line_index
                                    && line.contains_cursor(self.cursor)
                                {
                                    let cursor_column =
                                        line.relative_column_for_source_column(self.cursor.column);
                                    let x = viewport.text_origin().x
                                        + x_for_char_column(shaped, &line.text, cursor_column);
                                    window.paint_quad(fill(
                                        Bounds::new(point(x, y + px(3.0)), size(px(2.0), px(20.0))),
                                        theme::accent_dark(),
                                    ));
                                }

                                let _ = shaped.paint(
                                    point(viewport.text_origin().x, y + px(2.0)),
                                    line_height,
                                    window,
                                    cx,
                                );
                            },
                        );
                    }
                });
            },
        );
    }
}

fn paint_editor_line_number(
    line_number: usize,
    x: Pixels,
    y: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    paint_editor_gutter_label(format!("{line_number:>3}"), x, y, window, cx);
}

fn paint_editor_continuation_marker(x: Pixels, y: Pixels, window: &mut Window, cx: &mut App) {
    paint_editor_gutter_label(String::from("  |"), x, y, window, cx);
}

fn paint_editor_gutter_label(
    label: String,
    x: Pixels,
    y: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let run = editor_text_run(
        label.len(),
        theme::MONO_FONT,
        FontWeight::NORMAL,
        FontStyle::Normal,
        theme::rule(),
        None,
        None,
    );
    let text_system = window.text_system().clone();
    let shaped = text_system.shape_line(SharedString::from(label), px(11.0), &[run], None);
    let _ = shaped.paint(point(x, y + px(2.0)), px(EDITOR_LINE_HEIGHT_PX), window, cx);
}

fn shape_editor_text_line(text: &str, style: EditorLineStyle, window: &Window) -> ShapedLine {
    let runs = markdown_text_runs(text, style);
    let text_system = window.text_system().clone();
    text_system.shape_line(
        SharedString::from(text.to_string()),
        style.font_size(),
        &runs,
        None,
    )
}

fn markdown_text_runs(text: &str, line_style: EditorLineStyle) -> Vec<TextRun> {
    markdown_runs(text)
        .into_iter()
        .map(|run| {
            let style = text_run_style(line_style, run.kind);
            editor_text_run(
                run.range.end - run.range.start,
                style.family,
                style.weight,
                style.font_style,
                style.color,
                style.background,
                style.underline,
            )
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct EditorTextRunStyle {
    family: &'static str,
    weight: FontWeight,
    font_style: FontStyle,
    color: Hsla,
    background: Option<Hsla>,
    underline: Option<UnderlineStyle>,
}

fn text_run_style(line_style: EditorLineStyle, kind: MarkdownRunKind) -> EditorTextRunStyle {
    let mut style = base_text_run_style(line_style);

    match kind {
        MarkdownRunKind::Plain => {}
        MarkdownRunKind::Link => {
            style.color = theme::accent_dark();
            style.underline = Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(theme::accent()),
                wavy: false,
            });
        }
        MarkdownRunKind::WikiLink => {
            style.color = theme::accent();
            style.weight = FontWeight::SEMIBOLD;
            style.background = Some(theme::accent().opacity(0.10));
        }
        MarkdownRunKind::Code => {
            style.family = theme::MONO_FONT;
            style.weight = FontWeight::MEDIUM;
            style.color = theme::accent_dark();
            style.background = Some(theme::rule().opacity(0.18));
        }
        MarkdownRunKind::Bold => {
            style.weight = FontWeight::BOLD;
            style.color = theme::accent_dark();
        }
        MarkdownRunKind::Italic => {
            style.font_style = FontStyle::Italic;
            style.color = theme::muted();
        }
    }

    style
}

fn base_text_run_style(line_style: EditorLineStyle) -> EditorTextRunStyle {
    if line_style == EditorLineStyle::Heading {
        EditorTextRunStyle {
            family: theme::SERIF_FONT,
            weight: FontWeight::BOLD,
            font_style: FontStyle::Normal,
            color: theme::accent_dark(),
            background: None,
            underline: None,
        }
    } else {
        EditorTextRunStyle {
            family: theme::MONO_FONT,
            weight: FontWeight::NORMAL,
            font_style: FontStyle::Normal,
            color: theme::ink(),
            background: None,
            underline: None,
        }
    }
}

fn editor_text_run(
    len: usize,
    family: &str,
    weight: FontWeight,
    font_style: FontStyle,
    color: Hsla,
    background_color: Option<Hsla>,
    underline: Option<UnderlineStyle>,
) -> TextRun {
    let mut font = font(family.to_string());
    font.weight = weight;
    font.style = font_style;

    TextRun {
        len,
        font,
        color,
        background_color,
        underline,
        strikethrough: None,
    }
}

fn markdown_runs(text: &str) -> Vec<MarkdownRun> {
    let mut runs = Vec::new();
    let mut plain_start = 0;
    let mut cursor = 0;

    while cursor < text.len() {
        if let Some((end, kind)) = markdown_token_at(text, cursor) {
            if plain_start < cursor {
                runs.push(MarkdownRun {
                    range: plain_start..cursor,
                    kind: MarkdownRunKind::Plain,
                });
            }

            runs.push(MarkdownRun {
                range: cursor..end,
                kind,
            });
            cursor = end;
            plain_start = cursor;
        } else {
            cursor = next_char_boundary(text, cursor);
        }
    }

    if plain_start < text.len() {
        runs.push(MarkdownRun {
            range: plain_start..text.len(),
            kind: MarkdownRunKind::Plain,
        });
    }

    if runs.is_empty() {
        runs.push(MarkdownRun {
            range: 0..0,
            kind: MarkdownRunKind::Plain,
        });
    }

    runs
}

fn markdown_token_at(text: &str, start: usize) -> Option<(usize, MarkdownRunKind)> {
    let rest = &text[start..];

    if rest.starts_with('`') {
        return delimited_token(text, start, "`", "`", MarkdownRunKind::Code);
    }

    if rest.starts_with("[[") {
        return delimited_token(text, start, "[[", "]]", MarkdownRunKind::WikiLink);
    }

    if rest.starts_with('[')
        && let Some(close_label) = rest.find("](")
    {
        let url_start = start + close_label + 2;
        if let Some(close_url) = text[url_start..].find(')') {
            return Some((url_start + close_url + 1, MarkdownRunKind::Link));
        }
    }

    if rest.starts_with("**") {
        return delimited_token(text, start, "**", "**", MarkdownRunKind::Bold);
    }

    if rest.starts_with('*') && !rest.starts_with("**") {
        return delimited_token(text, start, "*", "*", MarkdownRunKind::Italic);
    }

    None
}

fn delimited_token(
    text: &str,
    start: usize,
    opener: &str,
    closer: &str,
    kind: MarkdownRunKind,
) -> Option<(usize, MarkdownRunKind)> {
    let body_start = start + opener.len();
    let close_offset = text[body_start..].find(closer)?;
    if close_offset == 0 {
        return None;
    }

    Some((body_start + close_offset + closer.len(), kind))
}

fn next_char_boundary(text: &str, start: usize) -> usize {
    text[start..]
        .chars()
        .next()
        .map(|character| start + character.len_utf8())
        .unwrap_or(text.len())
}

struct EditorInputElement {
    shell: Entity<OceanGuiShell>,
    focus_handle: FocusHandle,
}

impl IntoElement for EditorInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorInputElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.shell.update(cx, |shell, _| {
            shell.editor_bounds = Some(bounds);
        });
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.shell.clone()),
            cx,
        );
    }
}

impl Render for OceanGuiShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active_surface == SurfaceTab::Vault {
            self.sync_editor_scroll_path();
        }

        let mut shell = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::UI_FONT)
            .text_color(theme::ink())
            .child(self.render_top_bar(cx))
            .child(self.render_body(window, cx))
            .child(self.render_status_bar());

        if self.active_surface == SurfaceTab::Vault && self.command_palette.is_some() {
            shell = shell.child(self.render_command_palette(cx));
        }

        shell
    }
}

#[derive(Clone, Debug, Default)]
struct CommandPaletteState {
    query: String,
    selected: usize,
}

#[derive(Clone, Debug)]
enum PaletteEntry {
    Command(CommandSpec),
    Note(NoteSearchResult),
}

impl CommandPaletteState {
    fn entries(&self, state: &ShellState) -> Vec<PaletteEntry> {
        let query = self.query.trim();
        let mut entries = Vec::new();

        if query.is_empty() {
            entries.extend(filtered_commands("").into_iter().map(PaletteEntry::Command));
            entries.extend(
                state
                    .searchable_notes("", 8)
                    .into_iter()
                    .map(PaletteEntry::Note),
            );
            return entries;
        }

        entries.extend(
            state
                .searchable_notes(query, 18)
                .into_iter()
                .map(PaletteEntry::Note),
        );
        entries.extend(
            filtered_commands(query)
                .into_iter()
                .take(8)
                .map(PaletteEntry::Command),
        );
        entries
    }

    fn entry_count(&self, state: &ShellState) -> usize {
        self.entries(state).len()
    }

    fn selected_entry(&self, state: &ShellState) -> Option<PaletteEntry> {
        self.entries(state).get(self.selected).cloned()
    }

    fn insert_text(&mut self, text: &str) {
        self.query.push_str(text);
        self.selected = 0;
    }

    fn delete_backward(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    fn clear(&mut self) {
        self.query.clear();
        self.selected = 0;
    }

    fn move_selection(&mut self, delta: isize, entry_count: usize) {
        if entry_count == 0 {
            self.selected = 0;
            return;
        }

        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected
                .saturating_add(delta as usize)
                .min(entry_count.saturating_sub(1))
        };
    }
}

fn command_palette_text(event: &KeyDownEvent) -> Option<String> {
    let modifiers = event.keystroke.modifiers;
    if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
        return None;
    }

    match event.keystroke.key.as_str() {
        "space" => Some(String::from(" ")),
        key if key.chars().count() == 1 => Some(key.to_string()),
        _ => None,
    }
}

enum WatchDrain {
    Empty,
    Disconnected,
    Events(Vec<VaultWatchEvent>),
}

fn drain_watch_events(receiver: &Receiver<VaultWatchEvent>) -> WatchDrain {
    let mut events = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(event) => {
                events.push(event);
                if events.len() >= WATCH_EVENT_BATCH_LIMIT {
                    break;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                return if events.is_empty() {
                    WatchDrain::Disconnected
                } else {
                    WatchDrain::Events(events)
                };
            }
        }
    }

    if events.is_empty() {
        WatchDrain::Empty
    } else {
        WatchDrain::Events(events)
    }
}

fn spawn_watch_task(
    receiver: Receiver<VaultWatchEvent>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(WATCH_POLL_INTERVAL).await;
            let events = match drain_watch_events(&receiver) {
                WatchDrain::Empty => continue,
                WatchDrain::Disconnected => break,
                WatchDrain::Events(events) => events,
            };

            if shell
                .update(cx, |shell, cx| {
                    shell.apply_watch_events(events);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    })
}

fn spawn_daemon_health_task(
    receiver: Receiver<DaemonHealth>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(DAEMON_HEALTH_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(health) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_daemon_health(health);
                        cx.notify();
                    });
                    break;
                }
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    })
}

fn spawn_agent_event_task(
    receiver: Receiver<AgentStreamMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            let mut messages = Vec::new();
            loop {
                match receiver.try_recv() {
                    Ok(message) => {
                        messages.push(message);
                        if messages.len() >= AGENT_EVENT_BATCH_LIMIT {
                            break;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            if messages.is_empty() {
                continue;
            }

            if shell
                .update(cx, |shell, cx| {
                    shell.apply_agent_stream_messages(messages);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
        }
    })
}

fn spawn_agent_submit_task(
    receiver: Receiver<AgentSubmitMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_submit_message(message);
                        cx.notify();
                    });
                    break;
                }
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    })
}

fn x_for_char_column(shaped: &ShapedLine, text: &str, column: usize) -> Pixels {
    shaped.x_for_index(byte_offset_for_char_column(text, column))
}

fn scroll_line_delta_from_pixels(delta_y: f32) -> isize {
    let lines = delta_y / EDITOR_LINE_HEIGHT_PX;
    if lines > 0.0 {
        lines.ceil() as isize
    } else if lines < 0.0 {
        lines.floor() as isize
    } else {
        0
    }
}

fn should_stick_to_bottom(max_offset_y: Pixels, offset_y: Pixels) -> bool {
    if max_offset_y <= px(AGENT_STICKY_BOTTOM_THRESHOLD_PX) {
        return true;
    }

    (max_offset_y + offset_y).abs() <= px(AGENT_STICKY_BOTTOM_THRESHOLD_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_cache_keys_include_line_text_and_style() {
        let heading = test_shape_key(1, "# Title");
        let body_same_line = test_shape_key(1, "Title");
        let body_other_line = test_shape_key(2, "Title");

        assert_ne!(heading, body_same_line);
        assert_ne!(body_same_line, body_other_line);
        assert_eq!(heading.style, EditorLineStyle::Heading);
        assert_eq!(body_same_line.style, EditorLineStyle::Body);
    }

    #[test]
    fn layout_cache_keys_include_wrap_width() {
        let lines = vec![test_render_line(7, "Copper wrapped text", None)];

        let narrow = EditorLayoutCacheKey::new(&lines, px(240.0));
        let wide = EditorLayoutCacheKey::new(&lines, px(360.0));

        assert_ne!(narrow, wide);
    }

    #[test]
    fn layout_cache_keys_include_line_text() {
        let first = vec![test_render_line(7, "Copper wrapped text", None)];
        let second = vec![test_render_line(7, "Paper wrapped text", None)];

        assert_ne!(
            EditorLayoutCacheKey::new(&first, px(240.0)),
            EditorLayoutCacheKey::new(&second, px(240.0))
        );
    }

    #[test]
    fn layout_cache_keys_include_selection_ranges() {
        let unselected = vec![test_render_line(7, "Copper wrapped text", None)];
        let selected = vec![test_render_line(7, "Copper wrapped text", Some(0..6))];

        assert_ne!(
            EditorLayoutCacheKey::new(&unselected, px(240.0)),
            EditorLayoutCacheKey::new(&selected, px(240.0))
        );
    }

    #[test]
    fn layout_cache_keys_are_stable_for_same_visible_lines() {
        let first = vec![
            test_render_line(7, "Copper wrapped text", Some(0..6)),
            test_render_line(8, "## Heading", None),
        ];
        let second = first.clone();

        assert_eq!(
            EditorLayoutCacheKey::new(&first, px(240.4)),
            EditorLayoutCacheKey::new(&second, px(240.49))
        );
    }

    #[test]
    fn transcript_sticks_to_bottom_only_near_bottom() {
        assert!(should_stick_to_bottom(px(240.0), px(-240.0)));
        assert!(should_stick_to_bottom(px(240.0), px(-205.0)));
        assert!(!should_stick_to_bottom(px(240.0), px(-120.0)));
    }

    #[test]
    fn transcript_sticks_when_content_barely_overflows() {
        assert!(should_stick_to_bottom(px(20.0), px(0.0)));
    }

    #[test]
    fn markdown_runs_detect_inline_primitives() {
        let runs = markdown_runs("See [[Note]] and [site](https://example.com) with `code`.");

        assert_eq!(
            run_kinds(&runs),
            vec![
                MarkdownRunKind::Plain,
                MarkdownRunKind::WikiLink,
                MarkdownRunKind::Plain,
                MarkdownRunKind::Link,
                MarkdownRunKind::Plain,
                MarkdownRunKind::Code,
                MarkdownRunKind::Plain,
            ]
        );
    }

    #[test]
    fn markdown_runs_keep_code_span_precedence() {
        let runs = markdown_runs("`[[not a link]] **not bold**` **bold** *italic*");

        assert_eq!(
            run_kinds(&runs),
            vec![
                MarkdownRunKind::Code,
                MarkdownRunKind::Plain,
                MarkdownRunKind::Bold,
                MarkdownRunKind::Plain,
                MarkdownRunKind::Italic,
            ]
        );
    }

    #[test]
    fn markdown_text_runs_cover_utf8_bytes() {
        let text = "alpha **bé🙂ta** [[delta]]";
        let runs = markdown_text_runs(text, EditorLineStyle::Body);

        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
        assert!(runs.iter().any(|run| run.font.weight == FontWeight::BOLD));
        assert!(runs.iter().any(|run| run.background_color.is_some()));
    }

    fn run_kinds(runs: &[MarkdownRun]) -> Vec<MarkdownRunKind> {
        runs.iter().map(|run| run.kind).collect()
    }

    fn test_shape_key(line_index: usize, text: &str) -> EditorShapeKey {
        EditorShapeKey {
            line_index,
            text: text.to_string(),
            style: EditorLineStyle::for_text(text),
        }
    }

    fn test_render_line(
        document_line_index: usize,
        text: &str,
        selected_columns: Option<Range<usize>>,
    ) -> EditorRenderLine {
        EditorRenderLine {
            document_line_index,
            text: text.to_string(),
            selected_columns,
            style: EditorLineStyle::for_text(text),
        }
    }
}
