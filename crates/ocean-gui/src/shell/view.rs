use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Div,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, FontStyle,
    FontWeight, GlobalElementId, Hsla, InteractiveElement, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement, Pixels, Point,
    RenderImage, Render, ScrollHandle, ScrollWheelEvent, ShapedLine, SharedString, Stateful,
    StatefulInteractiveElement, Style, Styled, StyledImage, Task, TextRun, Timer, UTF16Selection,
    UnderlineStyle, Window, div, fill, font, img, point, px, relative, size, svg,
};
use image::{Frame, RgbaImage};

use super::agent::{AgentBlock, AgentEvent, AgentRole, AgentState, AgentTurn, ToolStatus};
use super::canvas::{
    prompt_with_canvas_context, ActorRef as CanvasActorRef, CanvasId, CanvasLedger, CanvasMode,
    LedgerSource, OceanCanvasView, SurfacePatchEnvelope,
};
use super::commands::{CommandSpec, ShellCommand, filtered_commands};
use super::daemon::{
    AgentSessionCreateRequest, AgentTurnRequest, AgentTurnResponse, ComponentEventRequest,
    ComponentEventResponse, ControlEvent, CreateRoomRequest, DaemonClient, DaemonHealth,
    LiveKitTokenResponse, ModelInfo, ModelsResponse, NativeDaemonState, PermissionControlResponse,
    PermissionDecisionRequest, PermissionStatus, PermissionsResponse, ProjectInfo, ProjectsResponse,
    RequestControlResponse, Room, RoomGetResponse, RoomJoinRequest, RoomMessage, RoomMutateResponse,
    RoomParticipant, RoomParticipantKind, RoomPostMessageRequest, RoomTranscriptResponse,
    RoomsListResponse, SessionDetail, SessionSummary, SessionsResponse,
};
use super::editor_buffer::EditorCursor;
use super::editor_layout::{
    EDITOR_FALLBACK_WRAP_WIDTH_PX, EDITOR_LINE_HEIGHT_PX, EditorLineStyle, EditorRenderLine,
    EditorViewport, EditorVisualLayout, EditorVisualLine, byte_offset_for_char_column,
    char_column_for_byte_index,
};
use super::gui_control::{
    ComponentId, GuiCommand, GuiControlEvent, GuiControlState, REGION_CHAT_INLINE, RegionId, RoomId,
};
use super::icons::ShellIcon;
use super::model::{EditorTab, FileEntry, FileKind, NoteSearchResult, OutlineItem, ShellState};
use super::rooms::{
    RoomFocus, RoomsState, author_label, participant_count_label, short_time as room_short_time,
    slugify,
};
use super::surface::{
    DEFAULT_CANVAS_ID, LedgerComponent, PaneDock, SurfaceIpcCommand, SurfaceIpcEvent,
    SurfaceLedger, SurfaceMode, SurfacePane, SurfacePaneKind, SurfaceState, canvas_web_url,
    prompt_with_surface_context,
};
use super::surface_host::{CanvasHostState, CanvasHostTarget, CanvasWebViewHost, HostBounds};
use super::surface_livekit::{SurfaceLiveKitJoinState, SurfaceLiveKitState};
use super::surface_livekit_client::{
    SurfaceLiveKitClientEvent, SurfaceLiveKitClientHandle, SurfaceLiveKitJoinRequest,
    SurfaceLiveKitSurfaceUpdate, spawn_surface_livekit_client,
};
use super::surface_livekit_video::SurfaceVideoFrame;
use super::theme;
use super::vault_index::Backlink;
use super::watcher::{VaultWatchEvent, VaultWatcher};

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(160);
const WATCH_EVENT_BATCH_LIMIT: usize = 128;
const DAEMON_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(120);
const AGENT_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const AGENT_EVENT_BATCH_LIMIT: usize = 128;
/// How often the open room's transcript is re-tailed (`after_seq`) while a room
/// is open. The daemon's `room_trigger` frame is unscoped (council-wide) and so
/// never reaches the GPUI shell's session-scoped streams, so — like the web
/// surface (OCEAN-108) — the live transcript is kept fresh by this poll. Cheap:
/// each request returns only entries past the highest seq we already hold.
const ROOM_TRANSCRIPT_POLL_INTERVAL: Duration = Duration::from_millis(2_500);
const AGENT_STICKY_BOTTOM_THRESHOLD_PX: f32 = 48.0;
const VISUAL_CURSOR_SCROLL_MARGIN: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceTab {
    Surface,
    Agent,
    Vault,
}

impl SurfaceTab {
    fn label(self) -> &'static str {
        match self {
            SurfaceTab::Surface => "Surface",
            SurfaceTab::Agent => "Agent",
            SurfaceTab::Vault => "Vault",
        }
    }

    fn id(self) -> usize {
        match self {
            SurfaceTab::Surface => 0,
            SurfaceTab::Agent => 1,
            SurfaceTab::Vault => 2,
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

/// Frames forwarded off the `/v1/events` control stream (OCEAN-75). Only the
/// two permission frames are modelled; everything else decodes to
/// `ControlEvent::Other` upstream and never reaches here as a real message.
#[derive(Clone, Debug)]
enum AgentControlStreamMessage {
    Event(ControlEvent),
    Error(String),
}

#[derive(Clone, Debug)]
enum AgentSubmitMessage {
    SessionReady {
        session_id: String,
        title: Option<String>,
        request: AgentTurnRequest,
    },
    Response(AgentTurnResponse),
    Error(String),
}

#[derive(Clone, Debug)]
enum AgentModelsMessage {
    Refreshed(Result<ModelsResponse, String>),
    Swapped(Result<ModelsResponse, String>),
}

#[derive(Clone, Debug)]
enum AgentProjectsMessage {
    Refreshed(Result<ProjectsResponse, String>),
}

#[derive(Clone, Debug)]
enum SurfaceLiveKitMessage {
    Token(Result<LiveKitTokenResponse, String>),
    Client(SurfaceLiveKitClientEvent),
}

#[derive(Clone, Debug)]
enum AgentSessionsMessage {
    Refreshed(Result<SessionsResponse, String>),
}

#[derive(Clone, Debug)]
enum AgentSessionLoadMessage {
    Loaded {
        session_id: String,
        result: Result<SessionDetail, String>,
    },
}

#[derive(Clone, Debug)]
enum AgentPermissionsMessage {
    Refreshed(Result<PermissionsResponse, String>),
}

#[derive(Clone, Debug)]
enum AgentControlMessage {
    Cancelled(Result<RequestControlResponse, String>),
    PermissionDecided(Result<PermissionControlResponse, String>),
    ComponentEventSent(Result<ComponentEventResponse, String>),
}

/// Results of one-shot rooms requests (list/create/load/join/leave/post),
/// forwarded from a background thread to the main thread by the rooms message
/// pump — the same pattern the agent catalogues use (OCEAN-109).
#[derive(Clone, Debug)]
enum RoomsMessage {
    Listed(Result<RoomsListResponse, String>),
    /// A room was created; carries its slug key so we can open it next.
    Created {
        key: String,
        result: Result<RoomMutateResponse, String>,
    },
    /// A room record + transcript loaded; tagged with the generation it was
    /// requested under so a stale load is dropped.
    Loaded {
        generation: u64,
        key: String,
        result: Result<RoomGetResponse, String>,
    },
    /// A join/leave mutation landed for `key`.
    Mutated {
        key: String,
        result: Result<RoomMutateResponse, String>,
    },
    /// An agent participant was added to `key` (OCEAN-119). Carries the agent id
    /// so the success status can point the operator at `@id`.
    AgentAdded {
        key: String,
        agent_id: String,
        result: Result<RoomMutateResponse, String>,
    },
    /// A posted message landed for `key` (re-tail to pick it up + any trigger).
    Posted {
        key: String,
        result: Result<RoomMutateResponse, String>,
    },
    /// A transcript tail poll for `key` returned (only new seqs are appended).
    TranscriptTail {
        key: String,
        result: Result<RoomTranscriptResponse, String>,
    },
}

/// A live remote video tile rendered in the LiveKit presence panel.
///
/// `image` is the latest decoded frame wrapped as a `gpui::RenderImage` (BGRA
/// byte order — see `surface_livekit_video`). It is `None` between
/// `RemoteVideoSubscribed` and the first decoded frame, so the tile shows a
/// placeholder until pixels land.
struct SurfaceVideoTile {
    participant_identity: String,
    width: u32,
    height: u32,
    image: Option<Arc<RenderImage>>,
}

pub struct OceanGuiShell {
    active_surface: SurfaceTab,
    state: ShellState,
    agent: AgentState,
    surface: SurfaceState,
    surface_host: CanvasHostState,
    surface_webview_host: CanvasWebViewHost,
    surface_ipc_receiver: Receiver<String>,
    surface_livekit: SurfaceLiveKitState,
    surface_livekit_client: Option<SurfaceLiveKitClientHandle>,
    /// Live remote video tiles, keyed by track sid. Each holds the latest
    /// decoded frame as a `gpui::RenderImage` plus tile metadata. Populated from
    /// `RemoteVideoSubscribed` / `RemoteVideoFrame` / `RemoteVideoRemoved`
    /// client events (OCEAN-97).
    surface_video_tiles: HashMap<String, SurfaceVideoTile>,
    /// The native, agent-owned [`CanvasLedger`] for the active session (Slice 4
    /// data layer). The native [`OceanCanvasView`] (Slice 5) renders from this;
    /// it stays `None` until a canvas is active.
    ///
    /// Held behind an `Arc<Mutex<…>>` shared cell so the view's [`LedgerSource`]
    /// (a plain `Fn() -> Option<CanvasLedger>` with no GPUI context) can read the
    /// latest ledger each frame without needing an `App`/entity borrow. The shell
    /// writes through [`Self::set_canvas_ledger`]; the view reads through the
    /// source. This keeps the ledger single-sourced (one cell) while crossing the
    /// context-free render boundary.
    canvas_ledger: Arc<Mutex<Option<CanvasLedger>>>,
    /// The native [`OceanCanvasView`] entity (Slice 5), mounted as a child of the
    /// surface pane. It renders from the shared `canvas_ledger` cell above via the
    /// [`LedgerSource`] closure installed at construction. Held as a GPUI
    /// [`Entity`] so the shell can call `cx.notify()` on it directly when a patch
    /// arrives (the canvas is its own entity; notifying the shell does not repaint
    /// it). This is the wiring that makes agent-driven canvas mutations *visible*
    /// (OCEAN-156) — without it `ocean_canvas_view()` had zero call sites.
    canvas_view: Entity<OceanCanvasView>,
    /// When `true` the surface pane renders the legacy tldraw webview projection
    /// (markers over a webview host); when `false` (the default) it renders the
    /// native [`OceanCanvasView`]. The native canvas is the default agent-render
    /// surface (gpui_masterbuild.md §9 / Gate D); the tldraw path is kept intact
    /// behind the existing toolbar toggle (full demotion is a later slice).
    surface_use_tldraw: bool,
    /// Monotonic count of native-canvas repaint requests issued from
    /// [`Self::apply_surface_patch_event`]. The real repaint is `cx.notify()` on
    /// the `canvas_view` entity, which is not observable without a window; this
    /// counter makes the §16 hot path ("patch arrives -> canvas repaints")
    /// assertable in headless tests. Bumped in lockstep with the entity notify.
    canvas_repaint_requests: Arc<AtomicU64>,
    gui_control: GuiControlState,
    daemon: NativeDaemonState,
    model_catalog: Vec<ModelInfo>,
    project_catalog: Vec<ProjectInfo>,
    /// Selected project id. When set, turns send it as `project_id` with an empty
    /// cwd so the daemon binds to the project's workspace_root.
    current_project: Option<String>,
    session_catalog: Vec<SessionSummary>,
    pending_permissions: Vec<PermissionStatus>,
    /// Native persistent-rooms panel state (OCEAN-109).
    rooms: RoomsState,
    model_picker_open: bool,
    project_picker_open: bool,
    session_picker_open: bool,
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
    /// Listener for the `/v1/events` control stream, which carries permission
    /// frames (OCEAN-75). Re-spawned alongside the agent event listener and
    /// gated by the SAME `agent_event_generation`, so a session switch retires
    /// the old control listener too.
    agent_control_stream_task: Option<Task<()>>,
    /// Monotonic generation for the agent SSE listener. Bumped on every
    /// (re)connect; the spawned reader thread captures its own generation and
    /// stops forwarding events once a newer connection supersedes it. Without
    /// this, starting a new session spawns a fresh listener while the old
    /// thread keeps feeding the previous session's events into the same state —
    /// the cross-surface / new-session bleed.
    agent_event_generation: Arc<AtomicU64>,
    agent_submit_task: Option<Task<()>>,
    agent_models_task: Option<Task<()>>,
    agent_projects_task: Option<Task<()>>,
    surface_livekit_task: Option<Task<()>>,
    agent_sessions_task: Option<Task<()>>,
    agent_session_load_task: Option<Task<()>>,
    agent_permissions_task: Option<Task<()>>,
    agent_control_task: Option<Task<()>>,
    /// One-shot rooms request pump (list/create/load/join/leave/post).
    rooms_task: Option<Task<()>>,
    /// The live transcript-tail poll loop for the open room (OCEAN-109).
    rooms_poll_task: Option<Task<()>>,
}

impl OceanGuiShell {
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor_focus = cx.focus_handle().tab_stop(true);
        let agent_focus = cx.focus_handle().tab_stop(true);
        let (surface_ipc_sender, surface_ipc_receiver) = mpsc::channel();
        window.focus(&agent_focus);

        // Build the shared ledger cell first so the native canvas view's
        // [`LedgerSource`] reads the *same* cell the shell writes through
        // `set_canvas_ledger`. One cell, single source of truth (Slice 4/6).
        let canvas_ledger: Arc<Mutex<Option<CanvasLedger>>> = Arc::new(Mutex::new(None));
        let canvas_view = {
            let cell = Arc::clone(&canvas_ledger);
            let source: LedgerSource =
                Arc::new(move || cell.lock().ok().and_then(|g| g.clone()));
            cx.new(|_| OceanCanvasView::new(source))
        };

        let mut shell = Self {
            active_surface: SurfaceTab::Surface,
            state: ShellState::seed(),
            agent: AgentState::default(),
            surface: SurfaceState::default(),
            surface_host: CanvasHostState::default(),
            surface_webview_host: CanvasWebViewHost::new(surface_ipc_sender),
            surface_ipc_receiver,
            surface_livekit: SurfaceLiveKitState::default(),
            surface_livekit_client: None,
            surface_video_tiles: HashMap::new(),
            canvas_ledger,
            canvas_view,
            surface_use_tldraw: false,
            canvas_repaint_requests: Arc::new(AtomicU64::new(0)),
            gui_control: GuiControlState::default(),
            daemon: NativeDaemonState::from_env(),
            model_catalog: Vec::new(),
            project_catalog: Vec::new(),
            current_project: None,
            session_catalog: Vec::new(),
            pending_permissions: Vec::new(),
            rooms: RoomsState::default(),
            model_picker_open: false,
            project_picker_open: false,
            session_picker_open: false,
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
            agent_control_stream_task: None,
            agent_event_generation: Arc::new(AtomicU64::new(0)),
            agent_submit_task: None,
            agent_models_task: None,
            agent_projects_task: None,
            surface_livekit_task: None,
            agent_sessions_task: None,
            agent_session_load_task: None,
            agent_permissions_task: None,
            agent_control_task: None,
            rooms_task: None,
            rooms_poll_task: None,
        };
        shell.restart_watcher(cx);
        shell.refresh_daemon_health(cx);
        shell.connect_agent_events(cx);
        shell.refresh_agent_catalogs(cx);
        shell
    }

    /// Snapshot the active native canvas ledger, if any.
    pub fn canvas_ledger(&self) -> Option<CanvasLedger> {
        self.canvas_ledger.lock().ok().and_then(|g| g.clone())
    }

    /// Replace the active native canvas ledger (driven by patch events in a later
    /// slice). Setting it makes the native [`OceanCanvasView`] render content on
    /// its next frame.
    pub fn set_canvas_ledger(&mut self, ledger: Option<CanvasLedger>) {
        if let Ok(mut guard) = self.canvas_ledger.lock() {
            *guard = ledger;
        }
    }

    /// The mounted native [`OceanCanvasView`] entity, whose ledger source reads
    /// this shell's active `canvas_ledger` cell. The surface pane renders this
    /// entity (see [`Self::render_surface_canvas_region`]); the shell repaints it
    /// on each patch via [`Self::apply_surface_patch_event`]. Construction happens
    /// once in [`Self::new`]; this just hands out a cheap entity-handle clone.
    pub fn ocean_canvas_view(&self) -> Entity<OceanCanvasView> {
        self.canvas_view.clone()
    }

    /// Whether the surface pane currently renders the legacy tldraw projection
    /// instead of the native canvas. Default is `false` (native).
    pub fn surface_uses_tldraw(&self) -> bool {
        self.surface_use_tldraw
    }

    /// Toggle the surface pane between the native [`OceanCanvasView`] (default)
    /// and the legacy tldraw webview projection. Wired to the existing
    /// "Open canvas" toolbar button so the tldraw path stays reachable.
    pub fn toggle_surface_tldraw(&mut self) {
        self.surface_use_tldraw = !self.surface_use_tldraw;
    }

    fn icon(&self, icon: ShellIcon, color: Hsla, size: f32) -> impl IntoElement {
        svg().path(icon.path()).size(px(size)).text_color(color)
    }

    fn copper_rule(&self) -> Div {
        div().h(px(2.0)).bg(theme::accent())
    }

    fn agent_status_dot(&self) -> Div {
        div().w(px(7.0)).h(px(7.0)).bg(if self.agent.streaming {
            theme::user()
        } else if matches!(&self.daemon.health, DaemonHealth::Ready(health) if health.ok) {
            theme::accent()
        } else {
            theme::danger()
        })
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_label = match self.active_surface {
            SurfaceTab::Surface => self.surface.session_id().to_string(),
            SurfaceTab::Agent => self.agent.status.clone(),
            SurfaceTab::Vault => self.state.active_label(),
        };

        let mut bar = div().flex().flex_col().bg(theme::frame()).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(44.0))
                .px_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
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
        );

        if let Some(picker_bar) = self.render_agent_picker_bar(cx) {
            bar = bar.child(picker_bar);
        }

        bar.child(self.copper_rule())
    }

    fn render_surface_tabs(&self, cx: &mut Context<Self>) -> Div {
        [SurfaceTab::Surface, SurfaceTab::Agent, SurfaceTab::Vault]
            .into_iter()
            .fold(div().flex().items_center().gap_1(), |tabs, surface| {
                tabs.child(self.render_surface_tab(surface, cx))
            })
    }

    fn render_surface_tab(&self, surface: SurfaceTab, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.active_surface == surface;
        div()
            .id(("surface-tab", surface.id()))
            .h(px(26.0))
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
                if matches!(surface, SurfaceTab::Agent | SurfaceTab::Surface) {
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
            SurfaceTab::Surface => self.render_surface_toolbar(cx),
            SurfaceTab::Agent => self.render_agent_toolbar(cx),
            SurfaceTab::Vault => self.render_vault_toolbar(cx),
        }
    }

    fn render_surface_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.toolbar_icon_button(
                "toolbar-surface-new-canvas",
                ShellIcon::Blocks,
                "New canvas pane",
                cx,
                |shell, cx| {
                    shell.open_surface_canvas("Canvas", SurfaceMode::General);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-workflow",
                ShellIcon::Code,
                "New workflow canvas",
                cx,
                |shell, cx| {
                    shell.open_surface_canvas("Workflow", SurfaceMode::WorkflowBuilder);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-storyboard",
                ShellIcon::FileText,
                "New storyboard canvas",
                cx,
                |shell, cx| {
                    shell.open_surface_canvas("Storyboard", SurfaceMode::Storyboard);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-add-card",
                ShellIcon::Check,
                "Drop markdown card",
                cx,
                |shell, cx| {
                    shell.drop_surface_markdown_card();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-open-tldraw",
                ShellIcon::Files,
                "Toggle tldraw / native canvas",
                cx,
                |shell, cx| {
                    // Flip the in-pane surface between the native OceanCanvasView
                    // (default) and the legacy tldraw projection, keeping the
                    // tldraw path reachable (OCEAN-156). When switching *into*
                    // tldraw, also open the external webview canvas as before.
                    shell.toggle_surface_tldraw();
                    if shell.surface_uses_tldraw() {
                        shell.open_surface_canvas_preview();
                    } else {
                        shell.agent.status = "native canvas".to_string();
                    }
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-detach-pane",
                ShellIcon::Diff,
                "Detach active pane",
                cx,
                |shell, cx| {
                    shell.detach_active_surface_pane();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-attach-pane",
                ShellIcon::Files,
                "Attach active pane",
                cx,
                |shell, cx| {
                    shell.attach_active_surface_pane();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-reconnect",
                ShellIcon::Server,
                "Reconnect agent stream",
                cx,
                |shell, cx| {
                    shell.connect_agent_events(cx);
                    shell.refresh_daemon_health(cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-mic",
                if self.surface_livekit.mic_enabled() {
                    ShellIcon::Chat
                } else {
                    ShellIcon::Diff
                },
                "Toggle mic intent",
                cx,
                |shell, cx| {
                    shell.toggle_surface_mic();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-camera",
                if self.surface_livekit.camera_enabled() {
                    ShellIcon::Check
                } else {
                    ShellIcon::Files
                },
                "Toggle camera intent",
                cx,
                |shell, cx| {
                    shell.toggle_surface_camera();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-surface-livekit-token",
                ShellIcon::Chat,
                "Join or leave hangout",
                cx,
                |shell, cx| {
                    shell.request_surface_livekit_token(cx);
                    cx.notify();
                },
            ))
            .child(self.health_dot())
    }

    fn render_agent_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.agent_toolbar_picker_button(
                "toolbar-project-picker",
                &current_project_toolbar_label(&self.current_project, &self.project_catalog),
                self.project_picker_open,
                "Select project",
                cx,
                |shell, cx| {
                    shell.project_picker_open = !shell.project_picker_open;
                    shell.model_picker_open = false;
                    shell.session_picker_open = false;
                    if shell.project_picker_open {
                        shell.refresh_agent_projects(cx);
                    }
                    cx.notify();
                },
            ))
            .child(self.agent_toolbar_picker_button(
                "toolbar-model-picker",
                &current_model_toolbar_label(&self.agent.model, &self.model_catalog),
                self.model_picker_open,
                "Select model",
                cx,
                |shell, cx| {
                    shell.model_picker_open = !shell.model_picker_open;
                    shell.project_picker_open = false;
                    shell.session_picker_open = false;
                    if shell.model_picker_open {
                        shell.refresh_agent_models(cx);
                    }
                    cx.notify();
                },
            ))
            .child(self.agent_toolbar_picker_button(
                "toolbar-session-picker",
                &current_session_toolbar_label(&self.agent),
                self.session_picker_open,
                "Switch session",
                cx,
                |shell, cx| {
                    shell.session_picker_open = !shell.session_picker_open;
                    shell.model_picker_open = false;
                    shell.project_picker_open = false;
                    if shell.session_picker_open {
                        shell.refresh_agent_sessions(cx);
                    }
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-health",
                ShellIcon::Server,
                "Check daemon health",
                cx,
                |shell, cx| {
                    shell.refresh_daemon_health(cx);
                    shell.refresh_agent_catalogs(cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-stream",
                ShellIcon::Chat,
                "Reconnect agent stream",
                cx,
                |shell, cx| {
                    shell.connect_agent_events(cx);
                    shell.refresh_agent_sessions(cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-cancel-request",
                ShellIcon::Blocks,
                "Cancel active request",
                cx,
                |shell, cx| {
                    shell.cancel_active_request(cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-approve-permission",
                ShellIcon::Check,
                "Approve latest permission",
                cx,
                |shell, cx| {
                    shell.decide_latest_permission(true, cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-deny-permission",
                ShellIcon::Diff,
                "Deny latest permission",
                cx,
                |shell, cx| {
                    shell.decide_latest_permission(false, cx);
                    cx.notify();
                },
            ))
            .child(self.agent_toolbar_picker_button(
                "toolbar-rooms",
                &rooms_toolbar_label(&self.rooms),
                self.rooms.panel_open,
                "Persistent rooms",
                cx,
                |shell, cx| {
                    shell.toggle_rooms_panel(cx);
                },
            ))
            .child(self.health_dot())
    }

    fn render_agent_picker_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.active_surface != SurfaceTab::Agent {
            return None;
        }

        if self.project_picker_open {
            return Some(
                div()
                    .px_3()
                    .pb_2()
                    .child(self.render_project_picker_panel(cx)),
            );
        }

        if self.model_picker_open {
            return Some(
                div()
                    .px_3()
                    .pb_2()
                    .child(self.render_model_picker_panel(cx)),
            );
        }

        if self.session_picker_open {
            return Some(
                div()
                    .px_3()
                    .pb_2()
                    .child(self.render_session_picker_panel(cx)),
            );
        }

        None
    }

    fn render_model_picker_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let current_model = self.agent.model.as_deref();
        let mut panel = div()
            .id("model-picker-panel")
            .flex()
            .flex_col()
            .w(px(360.0))
            .ml_auto()
            .h(px(260.0))
            .overflow_y_scroll()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule_strong());

        if self.model_catalog.is_empty() {
            panel = panel.child(self.picker_placeholder_row("No models loaded"));
        } else {
            for (index, model) in self.model_catalog.iter().enumerate() {
                let selected = current_model == Some(model.id.as_str());
                let model_id = model.id.clone();
                panel = panel.child(self.picker_row(
                    ("model-picker-row", index),
                    selected,
                    if model.label.is_empty() {
                        model.id.clone()
                    } else {
                        model.label.clone()
                    },
                    model.provider.clone(),
                    cx,
                    move |shell, cx| {
                        shell.select_agent_model(model_id.clone(), cx);
                        cx.notify();
                    },
                ));
            }
        }

        panel
    }

    fn render_project_picker_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let current = self.current_project.as_deref();
        let mut panel = div()
            .id("project-picker-panel")
            .flex()
            .flex_col()
            .w(px(360.0))
            .ml_auto()
            .h(px(260.0))
            .overflow_y_scroll()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule_strong());

        // A "no project" row first — clears the selection (turns fall back to
        // the GUI's own root dir).
        panel = panel.child(self.picker_row(
            ("project-picker-row", 0usize),
            current.is_none(),
            "no project".to_string(),
            "use the app's folder".to_string(),
            cx,
            move |shell, cx| {
                shell.current_project = None;
                shell.project_picker_open = false;
                cx.notify();
            },
        ));

        if self.project_catalog.is_empty() {
            panel = panel.child(self.picker_placeholder_row("No projects loaded"));
        } else {
            for (index, project) in self.project_catalog.iter().enumerate() {
                let selected = current == Some(project.id.as_str());
                let project_id = project.id.clone();
                let title = if project.name.is_empty() {
                    project.id.clone()
                } else {
                    project.name.clone()
                };
                panel = panel.child(self.picker_row(
                    // +1 so it never collides with the "no project" row id.
                    ("project-picker-row", index + 1),
                    selected,
                    title,
                    project.workspace_root.clone(),
                    cx,
                    move |shell, cx| {
                        shell.current_project = Some(project_id.clone());
                        shell.project_picker_open = false;
                        cx.notify();
                    },
                ));
            }
        }

        panel
    }

    fn render_session_picker_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let current_session = self.agent.session_id.as_deref();
        let mut panel = div()
            .id("session-picker-panel")
            .flex()
            .flex_col()
            .w(px(380.0))
            .ml_auto()
            .h(px(280.0))
            .overflow_y_scroll()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule_strong())
            .child(
                self.picker_action_row("New session", "fresh", cx, |shell, cx| {
                    shell.start_new_agent_session(cx);
                    cx.notify();
                }),
            );

        if self.session_catalog.is_empty() {
            panel = panel.child(self.picker_placeholder_row("No sessions loaded"));
        } else {
            for (index, session) in self.session_catalog.iter().enumerate() {
                let selected = current_session == Some(session.id.as_str());
                let session_id = session.id.clone();
                let session_title = session.title.clone();
                panel = panel.child(self.picker_row(
                    ("session-picker-row", index),
                    selected,
                    compact_session_title(session),
                    format!("{} turns", session.turn_count),
                    cx,
                    move |shell, cx| {
                        shell.switch_agent_session(session_id.clone(), session_title.clone(), cx);
                        cx.notify();
                    },
                ));
            }
        }

        panel
    }

    fn render_vault_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.toolbar_icon_button(
                "toolbar-command-palette",
                ShellIcon::Search,
                "Command palette",
                cx,
                |shell, cx| {
                    shell.open_command_palette();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-open-workspace",
                ShellIcon::Files,
                "Open workspace",
                cx,
                |shell, cx| {
                    shell.open_workspace_with_dialog(cx);
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-new-note",
                ShellIcon::FileText,
                "New note",
                cx,
                |shell, cx| {
                    shell.state.create_note();
                    shell.reset_editor_scroll();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-rename-note",
                ShellIcon::Editor,
                "Rename note",
                cx,
                |shell, cx| {
                    shell.rename_selected_with_dialog();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-delete-note",
                ShellIcon::Blocks,
                "Delete note",
                cx,
                |shell, cx| {
                    shell.delete_selected_with_confirmation();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-reveal-note",
                ShellIcon::Diff,
                "Reveal note",
                cx,
                |shell, cx| {
                    shell.state.reveal_selected();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-refresh-vault",
                ShellIcon::Server,
                "Refresh vault",
                cx,
                |shell, cx| {
                    shell.state.refresh_files();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-edit-external",
                ShellIcon::Code,
                "Edit externally",
                cx,
                |shell, cx| {
                    shell.state.open_active_external();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-reload-note",
                ShellIcon::Diff,
                "Reload note",
                cx,
                |shell, cx| {
                    shell.state.reload_active();
                    shell.reset_editor_scroll();
                    cx.notify();
                },
            ))
            .child(self.toolbar_icon_button(
                "toolbar-save-note",
                ShellIcon::Check,
                "Save note",
                cx,
                |shell, cx| {
                    shell.state.save_active();
                    cx.notify();
                },
            ))
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
            SurfaceTab::Surface => self.render_surface_workspace(window, cx),
            SurfaceTab::Agent => self.render_agent_workspace(window, cx),
            SurfaceTab::Vault => self.render_vault_workspace(window, cx),
        }
    }

    fn render_surface_workspace(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::background())
            .child(self.render_surface_sidebar(cx))
            .child(self.render_surface_canvas_region(cx))
            .child(self.render_surface_agent_rail(window, cx))
    }

    fn render_surface_agent_rail(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .w(px(430.0))
            .flex_shrink_0()
            .h_full()
            .min_h(px(0.0))
            .bg(theme::paper())
            .border_l(px(1.0))
            .border_color(theme::rule())
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(current_session_toolbar_label(&self.agent)),
                    ),
            )
            .child(self.render_agent_transcript(cx))
            .child(self.render_agent_composer(window, cx))
    }

    fn render_surface_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut panes = div().flex().flex_col().gap_1().p_2();
        for (index, pane) in self.surface.panes().iter().enumerate() {
            panes = panes.child(self.surface_pane_row(index, pane, cx));
        }

        div()
            .flex()
            .flex_col()
            .w(px(236.0))
            .flex_shrink_0()
            .h_full()
            .bg(theme::panel())
            .border_r(px(1.0))
            .border_color(theme::rule())
            .child(self.panel_header(ShellIcon::Blocks, "Surface"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .child(self.agent_metric_row("session", self.surface.session_id()))
                    .child(self.agent_metric_row("active", self.surface.active_pane_id()))
                    .child(
                        self.agent_metric_row("canvases", self.surface_canvas_count().to_string()),
                    )
                    .child(self.agent_metric_row("livekit", self.surface_livekit.status_label()))
                    .child(self.agent_metric_row("surface", self.surface_livekit.surface_id()))
                    .child(self.agent_metric_row("room", self.surface_livekit.room_id()))
                    .child(self.agent_metric_row("who", self.surface_livekit.participant_id()))
                    .child(self.agent_metric_row("name", self.surface_livekit.display_name()))
                    .child(self.agent_metric_row(
                        "mic",
                        if self.surface_livekit.mic_enabled() {
                            "on"
                        } else {
                            "off"
                        },
                    ))
                    .child(self.agent_metric_row(
                        "cam",
                        if self.surface_livekit.camera_enabled() {
                            "on"
                        } else {
                            "off"
                        },
                    ))
                    .child(self.agent_metric_row(
                        "present",
                        self.surface_livekit_roster_summary(),
                    ))
                    .children(self.surface_livekit_roster_rows())
                    .children(self.surface_livekit_video_tiles())
                    .child(self.agent_metric_row("daemon", self.daemon.status_label()))
                    .child(
                        self.agent_metric_row("agent", current_session_toolbar_label(&self.agent)),
                    ),
            )
            .child(self.panel_header(ShellIcon::Report, "Panes"))
            .child(panes)
            .child(self.panel_header(ShellIcon::Chat, "Surfaces"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(self.surface_row(SurfaceTab::Surface, cx))
                    .child(self.surface_row(SurfaceTab::Agent, cx))
                    .child(self.surface_row(SurfaceTab::Vault, cx)),
            )
    }

    fn surface_pane_row(
        &self,
        index: usize,
        pane: &SurfacePane,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = pane.pane_id == self.surface.active_pane_id();
        let pane_id = pane.pane_id.clone();
        let detail = match pane.kind {
            SurfacePaneKind::TldrawCanvas => pane
                .canvas_id
                .as_deref()
                .unwrap_or(DEFAULT_CANVAS_ID)
                .to_string(),
            SurfacePaneKind::AgentTranscript => "agent".to_string(),
            SurfacePaneKind::Notes => "notes".to_string(),
        };

        div()
            .id(("surface-pane-row", index))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .h(px(30.0))
            .px_2()
            .bg(if selected {
                theme::paper()
            } else {
                theme::panel()
            })
            .border_l(px(2.0))
            .border_color(if selected {
                theme::accent()
            } else {
                theme::panel()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.surface.set_active_pane(&pane_id);
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
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
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(pane.title.clone()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(detail),
            )
    }

    fn render_surface_canvas_region(&self, cx: &mut Context<Self>) -> Div {
        let canvas_id = self.active_surface_canvas_id();
        let ledger = canvas_id
            .as_deref()
            .and_then(|canvas_id| self.surface.canvas(canvas_id));

        // The native canvas is the default agent-render surface; the legacy
        // tldraw projection is shown only when the operator toggles into it
        // (OCEAN-156, gpui_masterbuild.md §9 / Gate D).
        let target = surface_render_target(self.surface_use_tldraw);
        let use_tldraw = target == SurfaceRenderTarget::Tldraw;

        // Header title/subtitle reflect the active render mode.
        let title = if use_tldraw {
            ledger
                .map(|ledger| ledger.canvas_id.clone())
                .unwrap_or_else(|| DEFAULT_CANVAS_ID.to_string())
        } else {
            self.canvas_ledger()
                .map(|l| l.canvas_id.to_string())
                .unwrap_or_else(|| DEFAULT_CANVAS_ID.to_string())
        };
        let subtitle = if use_tldraw {
            ledger
                .map(|ledger| ledger.tldraw_room_id.clone())
                .unwrap_or_else(|| "tldraw pending".to_string())
        } else {
            "native canvas".to_string()
        };

        let mut region = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
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
                            .child(self.icon(ShellIcon::Blocks, theme::accent(), 14.0))
                            .child(title),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(subtitle),
                    ),
            );

        if use_tldraw {
            // Legacy path: tldraw webview host + ledger markers (kept intact
            // behind the toggle).
            region = region.child(self.render_tldraw_host_placeholder(ledger, cx));
        } else {
            // Native path: mount the OceanCanvasView entity. It draws the active
            // CanvasLedger (the agent-render surface) with GPUI primitives.
            region = region.child(
                div()
                    .id("surface-native-canvas-host")
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .m_3()
                    .border_1()
                    .border_color(theme::rule())
                    .overflow_hidden()
                    .child(self.canvas_view.clone()),
            );
        }

        region.child(self.render_surface_ledger_strip(ledger))
    }

    fn render_tldraw_host_placeholder(
        &self,
        ledger: Option<&SurfaceLedger>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let component_count = ledger.map(|ledger| ledger.components.len()).unwrap_or(0);
        div()
            .id("surface-tldraw-host")
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .m_3()
            .bg(theme::background())
            .border_1()
            .border_color(theme::rule())
            .overflow_hidden()
            .child(SurfaceCanvasHostElement { shell: cx.entity() })
            .child(
                div()
                    .absolute()
                    .top(px(12.0))
                    .left(px(12.0))
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(format!(
                        "Live canvas · {} ledger components",
                        component_count
                    )),
            )
            .child(self.render_surface_component_markers(ledger))
    }

    fn render_surface_component_markers(&self, ledger: Option<&SurfaceLedger>) -> Div {
        let mut layer = div().absolute().top_0().left_0().right_0().bottom_0();
        if let Some(ledger) = ledger {
            for component in ledger.components.values() {
                layer = layer.child(self.surface_component_marker(component));
            }
        }
        layer
    }

    fn surface_component_marker(&self, component: &LedgerComponent) -> Div {
        div()
            .absolute()
            .left(px(component.x))
            .top(px(component.y))
            .w(px(component.width))
            .h(px(component.height))
            .bg(theme::panel_raised())
            .border_1()
            .border_color(theme::accent())
            .p_2()
            .overflow_hidden()
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent_dark())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(component.id.clone()),
            )
            .child(
                div()
                    .pt_1()
                    .font_family(theme::UI_FONT)
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .text_color(theme::ink())
                    .whitespace_normal()
                    .child(component_text(component)),
            )
    }

    fn render_surface_ledger_strip(&self, ledger: Option<&SurfaceLedger>) -> Div {
        let mut strip = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.0))
            .px_3()
            .bg(theme::frame())
            .border_t(px(1.0))
            .border_color(theme::rule())
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::muted());

        if let Some(ledger) = ledger {
            strip = strip
                .child(format!("mode {:?}", ledger.mode))
                .child(format!("revision {}", ledger.revision))
                .child(format!("selection {}", ledger.selection.len()))
                .child(format!("components {}", ledger.components.len()));
        } else {
            strip = strip.child("no active canvas");
        }

        strip
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
                    .children(self.render_permission_banner(cx))
                    .children(if self.rooms.panel_open {
                        Some(self.render_rooms_panel(window, cx))
                    } else {
                        None
                    })
                    .children(if self.rooms.panel_open {
                        None
                    } else {
                        Some(self.render_agent_transcript(cx))
                    })
                    .children(if self.rooms.panel_open {
                        None
                    } else {
                        Some(self.render_agent_composer(window, cx))
                    }),
            )
    }

    /// A prominent approve/deny banner for permission requests blocked on the
    /// daemon (OCEAN-75). Renders one card per pending permission, oldest first,
    /// stacked between the Agent header and transcript so a gated mutating tool
    /// call can't silently hang. Returns `None` when nothing is pending so the
    /// layout is untouched on the ungated path. This is the GPUI counterpart to
    /// the web surface's approval cards (OCEAN-64).
    fn render_permission_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.pending_permissions.is_empty() {
            return None;
        }

        let mut banner = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule_strong());

        let count = self.pending_permissions.len();
        banner = banner.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .font_family(theme::MONO_FONT)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::accent_dark())
                .child(if count == 1 {
                    "Approval required".to_string()
                } else {
                    format!("Approval required · {count} pending")
                }),
        );

        for permission in &self.pending_permissions {
            banner = banner.child(self.render_permission_card(permission, cx));
        }

        Some(banner)
    }

    fn render_permission_card(
        &self,
        permission: &PermissionStatus,
        cx: &mut Context<Self>,
    ) -> Div {
        let permission_id = permission.permission_id.clone();
        let allow_id: ElementId =
            SharedString::from(format!("permission-allow-{permission_id}")).into();
        let deny_id: ElementId =
            SharedString::from(format!("permission-deny-{permission_id}")).into();
        let allow_permission = permission_id.clone();
        let deny_permission = permission_id.clone();

        let args_summary = permission_args_summary(&permission.args);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule_strong())
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::ink())
                    .child(permission.tool.clone()),
            );

        if !permission.reason.trim().is_empty() {
            card = card.child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(permission.reason.clone()),
            );
        }

        if !args_summary.is_empty() {
            card = card.child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::thinking())
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(args_summary),
            );
        }

        card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id(allow_id)
                        .px_3()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::green())
                        .border_1()
                        .border_color(theme::green())
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.decide_permission_by_id(allow_permission.clone(), true, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::background())
                                .child("Approve"),
                        ),
                )
                .child(
                    div()
                        .id(deny_id)
                        .px_3()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::frame())
                        .border_1()
                        .border_color(theme::danger())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::panel_raised()))
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.decide_permission_by_id(deny_permission.clone(), false, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::danger())
                                .child("Deny"),
                        ),
                ),
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
                    .child(
                        self.agent_metric_row(
                            "session",
                            current_session_toolbar_label(&self.agent),
                        ),
                    )
                    .child(self.agent_metric_row(
                        "model",
                        current_model_toolbar_label(&self.agent.model, &self.model_catalog),
                    ))
                    .child(
                        self.agent_metric_row("region", self.gui_control.active_region().as_str()),
                    )
                    .child(
                        self.agent_metric_row(
                            "room",
                            self.gui_control
                                .active_room()
                                .map(|room| room.as_str())
                                .unwrap_or("none"),
                        ),
                    )
                    .child(
                        self.agent_metric_row(
                            "ctl sess",
                            self.gui_control
                                .active_session_id()
                                .map(short_session_label)
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                    )
                    .child(
                        self.agent_metric_row(
                            "pane",
                            self.gui_control
                                .active_pane()
                                .map(|pane| pane.as_str())
                                .unwrap_or("none"),
                        ),
                    )
                    .child(self.agent_metric_row(
                        "components",
                        self.gui_control.component_count().to_string(),
                    ))
                    .child(
                        self.agent_metric_row(
                            "approvals",
                            self.pending_permissions.len().to_string(),
                        ),
                    )
                    .child(self.agent_metric_row(
                        "latest",
                        permission_summary_label(self.pending_permissions.first()),
                    ))
                    .child(self.agent_metric_row("control", self.gui_control_event_label()))
                    .child(self.agent_metric_row("vault", self.state.root_label())),
            )
            .child(self.panel_header(ShellIcon::Report, "Surfaces"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(self.surface_row(SurfaceTab::Surface, cx))
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
                if matches!(surface, SurfaceTab::Agent | SurfaceTab::Surface) {
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

    /// Summary line for the LiveKit presence panel: total participants and how
    /// many are remote. Empty roster reads "0" so the row is always meaningful.
    fn surface_livekit_roster_summary(&self) -> String {
        let roster = self.surface_livekit.roster();
        if roster.is_empty() {
            return "0".to_string();
        }
        let total = roster.len();
        let remote = roster.iter().filter(|p| !p.local).count();
        format!("{total} ({remote} remote)")
    }

    /// One metric row per LiveKit participant, mirroring the web surface's
    /// roster: name (with a `you` marker for the local row) and live
    /// mic/camera/speaking flags derived from track publications.
    fn surface_livekit_roster_rows(&self) -> Vec<Div> {
        self.surface_livekit
            .roster()
            .iter()
            .map(|participant| {
                let mut flags: Vec<&str> = Vec::new();
                if participant.local {
                    flags.push("you");
                }
                if participant.speaking {
                    flags.push("speaking");
                }
                if participant.mic {
                    flags.push("mic");
                }
                if participant.camera {
                    flags.push("cam");
                }
                let value = if flags.is_empty() {
                    "idle".to_string()
                } else {
                    flags.join(" · ")
                };
                let label = if participant.name.trim().is_empty() {
                    participant.identity.clone()
                } else {
                    participant.name.clone()
                };
                self.roster_metric_row(label, value)
            })
            .collect()
    }

    /// Render the live remote video tiles (OCEAN-97). Each subscribed remote
    /// video track gets a 16:9 tile: once frames arrive it shows the decoded
    /// `RenderImage`; before the first frame (or for an undecodable buffer
    /// layout) it shows a labelled placeholder so presence is still legible.
    fn surface_livekit_video_tiles(&self) -> Vec<Div> {
        if self.surface_video_tiles.is_empty() {
            return Vec::new();
        }
        let mut tiles: Vec<&SurfaceVideoTile> = self.surface_video_tiles.values().collect();
        tiles.sort_by(|a, b| a.participant_identity.cmp(&b.participant_identity));

        tiles
            .into_iter()
            .map(|tile| {
                let frame: Div = div()
                    .w_full()
                    .h(px(96.0))
                    .rounded_md()
                    .overflow_hidden()
                    .bg(theme::panel_raised())
                    .border_1()
                    .border_color(theme::rule().opacity(0.42))
                    .flex()
                    .items_center()
                    .justify_center();

                let frame = if let Some(image) = tile.image.clone() {
                    frame.child(
                        img(image)
                            .object_fit(ObjectFit::Contain)
                            .w_full()
                            .h_full(),
                    )
                } else {
                    frame.child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .child("connecting video…"),
                    )
                };

                let dims = if tile.width > 0 && tile.height > 0 {
                    format!("{}×{}", tile.width, tile.height)
                } else {
                    "live".to_string()
                };

                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pt_1()
                    .child(frame)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(tile.participant_identity.clone()),
                            )
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT)
                                    .text_xs()
                                    .text_color(theme::ink())
                                    .child(dims),
                            ),
                    )
            })
            .collect()
    }

    /// Like `agent_metric_row` but for a dynamic (owned) label, used by the
    /// LiveKit roster where the label is a participant name.
    fn roster_metric_row(&self, label: String, value: String) -> Div {
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
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::ink())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(value),
            )
    }

    fn render_agent_transcript(&self, cx: &mut Context<Self>) -> Div {
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
            transcript = transcript.child(
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
                transcript = transcript.child(self.render_agent_turn(turn_index, turn, cx));
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
        cx: &mut Context<Self>,
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
            body = body.child(self.render_agent_block(index, block_index, block, cx));
        }

        let mut role_column = div()
            .w(px(58.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .font_family(theme::MONO_FONT)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .child(label);
        if turn.role == AgentRole::Assistant {
            role_column = role_column.child(
                div()
                    .id(("agent-turn-pin", index))
                    .px_1()
                    .py_1()
                    .border_1()
                    .border_color(theme::rule().opacity(0.42))
                    .bg(theme::frame())
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()).border_color(theme::rule()))
                    .tooltip(|_, cx| {
                        cx.new(|_| ToolbarTooltip {
                            label: "Pin turn to canvas",
                        })
                        .into()
                    })
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.pin_agent_turn_to_canvas(index);
                        cx.notify();
                    }))
                    .child("PIN"),
            );
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
            .child(role_column)
            .child(body)
    }

    fn render_agent_block(
        &self,
        turn_index: usize,
        block_index: usize,
        block: &AgentBlock,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let block_dom_id = turn_index.saturating_mul(1000).saturating_add(block_index);
        match block {
            AgentBlock::Text(text) => div()
                .id(("agent-text", block_dom_id))
                .w_full()
                .min_w(px(0.0))
                .overflow_x_hidden()
                .whitespace_normal()
                .line_height(px(23.0))
                .font_family(theme::UI_FONT)
                .text_size(px(15.0))
                .text_color(theme::ink())
                .child(text.clone())
                .into_any_element(),
            AgentBlock::Thinking { content, expanded } => {
                let mut block = self.collapsible_agent_block(
                    ("agent-thinking", block_dom_id),
                    *expanded,
                    "thinking",
                    compact_text_stat(content),
                    theme::thinking(),
                    turn_index,
                    block_index,
                    cx,
                );

                if *expanded {
                    block =
                        block.child(self.agent_block_detail(content.clone(), theme::thinking()));
                }

                block.into_any_element()
            }
            AgentBlock::ToolCall {
                name,
                args_preview,
                output,
                status,
                expanded,
                ..
            } => {
                let (status_label, color) = match status {
                    ToolStatus::Running => ("running", theme::user()),
                    ToolStatus::Ok => ("ok", theme::green()),
                    ToolStatus::Err => ("err", theme::danger()),
                };
                let show_detail = *expanded || matches!(*status, ToolStatus::Err);
                let mut block = self.collapsible_agent_block(
                    ("agent-tool", block_dom_id),
                    *expanded,
                    format!("{name} · {status_label}"),
                    tool_call_summary(args_preview, output, *status),
                    color,
                    turn_index,
                    block_index,
                    cx,
                );

                if show_detail {
                    let detail = if output.is_empty() {
                        String::from("waiting for output")
                    } else {
                        output.clone()
                    };
                    block = block.child(self.agent_block_detail(detail, theme::muted()));
                }

                block.into_any_element()
            }
            AgentBlock::Component {
                component_id, kind, ..
            } => {
                let session_id = self.agent.session_id.clone();
                let component_id_for_click = component_id.clone();

                div()
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
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised().opacity(0.62)))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        if let Some(session_id) = session_id.clone() {
                            shell.send_component_event(
                                session_id,
                                component_id_for_click.clone(),
                                serde_json::json!({ "type": "click" }),
                                cx,
                            );
                        } else {
                            shell.agent.status = "component event needs session".to_string();
                        }
                        cx.notify();
                    }))
                    .child(format!("{kind} {component_id}"))
                    .into_any_element()
            }
        }
    }

    fn collapsible_agent_block(
        &self,
        id: impl Into<ElementId>,
        expanded: bool,
        title: impl Into<String>,
        summary: impl Into<String>,
        color: Hsla,
        turn_index: usize,
        block_index: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w(px(0.0))
            .overflow_x_hidden()
            .bg(theme::frame().opacity(0.62))
            .border_1()
            .border_color(theme::rule().opacity(0.42))
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised().opacity(0.72)))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.agent.toggle_block_expanded(turn_index, block_index);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(28.0))
                    .px_2()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .child(
                        div()
                            .w(px(10.0))
                            .text_color(theme::muted())
                            .child(if expanded { "v" } else { ">" }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(title.into()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(theme::muted())
                            .child(summary.into()),
                    ),
            )
    }

    fn agent_block_detail(&self, content: String, color: Hsla) -> Div {
        div()
            .w_full()
            .min_w(px(0.0))
            .overflow_x_hidden()
            .px_3()
            .pb_2()
            .font_family(theme::MONO_FONT)
            .text_xs()
            .line_height(px(18.0))
            .whitespace_normal()
            .text_color(color)
            .child(content)
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
                    .font_family(theme::UI_FONT)
                    .text_size(px(14.5))
                    .line_height(px(21.0))
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

    // ---- Rooms panel (OCEAN-109) ---------------------------------------------

    /// The persistent-rooms panel, shown in the agent area in place of the
    /// transcript/composer while open. Shows the room list (with a create row)
    /// until a room is opened, then the room's roster, transcript, and a message
    /// composer. The native counterpart to the web rooms UI (OCEAN-108).
    fn render_rooms_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::paper())
            .child(self.render_rooms_panel_head(cx));

        if self.rooms.open_key.is_some() {
            panel = panel.child(self.render_rooms_roster(cx));
            panel = panel.child(self.render_rooms_add_agent(window, cx));
            if let Some(summary) = self.rooms.trigger_policy_summary() {
                panel = panel.child(self.render_rooms_policy_summary(summary));
            }
            panel = panel.child(self.render_rooms_transcript(cx));
            if let Some(hint) = self.render_rooms_mention_hint(cx) {
                panel = panel.child(hint);
            }
            panel = panel.child(self.render_rooms_composer(window, cx));
        } else {
            panel = panel
                .child(self.render_rooms_create_row(window, cx))
                .child(self.render_rooms_policy_toggles(cx))
                .child(self.render_rooms_list(cx));
        }

        if !self.rooms.status.is_empty() {
            panel = panel.child(
                div()
                    .px_4()
                    .py_2()
                    .bg(theme::frame())
                    .border_t(px(1.0))
                    .border_color(theme::rule())
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.rooms.status.clone()),
            );
        }

        panel
    }

    fn render_rooms_panel_head(&self, cx: &mut Context<Self>) -> Div {
        let mut head = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(40.0))
            .px_4()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule_strong());

        let mut left = div()
            .flex()
            .items_center()
            .gap_2()
            .font_family(theme::MONO_FONT)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::accent_dark());

        if self.rooms.open_key.is_some() {
            left = left.child(
                div()
                    .id("rooms-back")
                    .px_2()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::rule())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.close_room(cx);
                    }))
                    .child("< Rooms"),
            );
        }
        left = left.child(self.rooms.header_title());

        head = head.child(left).child(
            div()
                .id("rooms-close")
                .w(px(24.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::panel())
                .border_1()
                .border_color(theme::rule())
                .cursor_pointer()
                .hover(|style| style.bg(theme::panel_raised()))
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.rooms.panel_open = false;
                    cx.notify();
                }))
                .child(
                    div()
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .text_color(theme::muted())
                        .child("x"),
                ),
        );

        head
    }

    fn render_rooms_create_row(&self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let draft = self.rooms.new_room_draft.clone();
        let placeholder = draft.is_empty();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule())
            .track_focus(&self.agent_focus)
            .on_key_down(cx.listener(Self::on_rooms_key_down))
            .child(
                div()
                    .id("rooms-create-input")
                    .flex_1()
                    .min_h(px(30.0))
                    .px_3()
                    .py_1()
                    .bg(theme::background())
                    .border_1()
                    .border_color(if self.rooms.focus == RoomFocus::NewRoomName {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::UI_FONT)
                    .text_size(px(13.5))
                    .text_color(if placeholder {
                        theme::muted()
                    } else {
                        theme::ink()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.rooms.focus = RoomFocus::NewRoomName;
                        window.focus(&shell.agent_focus);
                        cx.notify();
                    }))
                    .child(if placeholder {
                        "New room name...".to_string()
                    } else {
                        draft
                    }),
            )
            .child(
                div()
                    .id("rooms-create-btn")
                    .px_3()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::accent())
                    .border_1()
                    .border_color(theme::accent())
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.create_room_from_draft(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::background())
                            .child("+ Create"),
                    ),
            )
    }

    fn render_rooms_list(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut list = div()
            .id("rooms-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll();

        if self.rooms.list.is_empty() {
            list = list.child(
                div()
                    .px_4()
                    .py_4()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("No rooms yet. Create one above to start collaborating."),
            );
            return list;
        }

        for (index, room) in self.rooms.list.iter().enumerate() {
            list = list.child(self.render_rooms_list_row(index, room, cx));
        }
        list
    }

    fn render_rooms_list_row(
        &self,
        index: usize,
        room: &Room,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = room.id.clone();
        let name = room.name.clone();
        let count = participant_count_label(room.participants.len());
        let last = room_short_time(&room.updated_at);

        let mut meta = div()
            .flex()
            .items_center()
            .gap_3()
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::muted())
            .child(count);
        if !last.is_empty() {
            meta = meta.child(div().text_color(theme::thinking()).child(last));
        }

        div()
            .id(("rooms-list-row", index))
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_2()
            .bg(theme::paper())
            .border_b(px(1.0))
            .border_color(theme::rule().opacity(0.32))
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| {
                shell.open_room(key.clone(), cx);
                cx.notify();
            }))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::ink())
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name),
            )
            .child(meta)
    }

    fn render_rooms_roster(&self, cx: &mut Context<Self>) -> Div {
        let mut roster = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule());

        let participants: Vec<RoomParticipant> = self
            .rooms
            .open_room
            .as_ref()
            .map(|room| room.participants.clone())
            .unwrap_or_default();

        for participant in &participants {
            let is_agent = participant.kind == RoomParticipantKind::Agent;
            // Roster chip: kind glyph (🤖 for agents) + display name + a muted
            // "human/agent/…" kind label so it's obvious who's auto-convene-able
            // (OCEAN-119, matching the web roster in OCEAN-117). Agent chips get an
            // accent border to stand out.
            roster = roster.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .h(px(22.0))
                    .bg(theme::panel())
                    .border_1()
                    .border_color(if is_agent {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::ink())
                    .child(format!(
                        "{} {}",
                        participant.kind.glyph(),
                        participant.display_name
                    ))
                    .child(
                        div()
                            .text_color(theme::muted())
                            .child(participant.kind.label()),
                    ),
            );
        }

        // Join / leave toggle.
        let joined = self.rooms.joined_open();
        roster = roster.child(
            div()
                .id("rooms-join-toggle")
                .px_3()
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(if joined {
                    theme::frame()
                } else {
                    theme::accent()
                })
                .border_1()
                .border_color(if joined {
                    theme::danger()
                } else {
                    theme::accent()
                })
                .cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .on_click(cx.listener(move |shell, _, _, cx| {
                    if joined {
                        shell.leave_open_room(cx);
                    } else {
                        shell.join_open_room(cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if joined {
                            theme::danger()
                        } else {
                            theme::background()
                        })
                        .child(if joined { "Leave" } else { "Join" }),
                ),
        );

        roster
    }

    /// Trigger-policy toggles applied at room creation (OCEAN-119). These wire
    /// into the daemon's `room_create` body; there is no room-update route yet,
    /// so policy is set once at create time (matching OCEAN-117). Each row is a
    /// clickable `[x]`/`[ ]` toggle; the cron row shows the schedule draft.
    fn render_rooms_policy_toggles(&self, cx: &mut Context<Self>) -> Div {
        let mut section = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule())
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent_dark())
                    .child("Auto-convene triggers"),
            );

        let toggle_row =
            |id: &'static str,
             label: &'static str,
             hint: Option<&'static str>,
             checked: bool,
             cx: &mut Context<Self>,
             on_toggle: fn(&mut Self)|
             -> Stateful<Div> {
                let mut row = div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(22.0))
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        on_toggle(shell);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(if checked {
                                theme::accent_dark()
                            } else {
                                theme::muted()
                            })
                            .child(if checked { "[x]" } else { "[ ]" }),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::ink())
                            .child(label),
                    );
                if let Some(hint) = hint {
                    row = row.child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .child(hint),
                    );
                }
                row
            };

        section = section
            .child(toggle_row(
                "rooms-policy-mention",
                "On @mention",
                Some("wake a mentioned agent"),
                self.rooms.policy_on_mention,
                cx,
                |shell| shell.rooms.policy_on_mention = !shell.rooms.policy_on_mention,
            ))
            .child(toggle_row(
                "rooms-policy-thread",
                "On thread reply",
                None,
                self.rooms.policy_on_thread_reply,
                cx,
                |shell| shell.rooms.policy_on_thread_reply = !shell.rooms.policy_on_thread_reply,
            ))
            .child(toggle_row(
                "rooms-policy-component",
                "On component event",
                None,
                self.rooms.policy_on_component_event,
                cx,
                |shell| {
                    shell.rooms.policy_on_component_event =
                        !shell.rooms.policy_on_component_event
                },
            ));

        // Cron schedule row — free-form text routed by the rooms key handler when
        // its input is focused. Click to focus it.
        let cron = self.rooms.policy_on_schedule_draft.clone();
        let cron_placeholder = cron.is_empty();
        let cron_focused = self.rooms.focus == RoomFocus::ScheduleCron;
        section = section.child(
            div()
                .id("rooms-policy-cron")
                .flex()
                .items_center()
                .gap_2()
                .mt_1()
                .child(
                    div()
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .text_color(theme::ink())
                        .child("On schedule (cron)"),
                )
                .child(
                    div()
                        .id("rooms-policy-cron-input")
                        .flex_1()
                        .min_h(px(24.0))
                        .px_2()
                        .py_1()
                        .bg(theme::background())
                        .border_1()
                        .border_color(if cron_focused {
                            theme::accent()
                        } else {
                            theme::rule()
                        })
                        .font_family(theme::MONO_FONT)
                        .text_xs()
                        .text_color(if cron_placeholder {
                            theme::muted()
                        } else {
                            theme::ink()
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.rooms.focus = RoomFocus::ScheduleCron;
                            window.focus(&shell.agent_focus);
                            cx.notify();
                        }))
                        .child(if cron_placeholder {
                            "e.g. 0 9 * * *".to_string()
                        } else {
                            cron
                        }),
                ),
        );

        section
    }

    /// Read-only summary of the open room's auto-convene triggers (OCEAN-119).
    /// Shown only when the room carries a policy.
    fn render_rooms_policy_summary(&self, summary: String) -> Div {
        div()
            .px_4()
            .py_1()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule().opacity(0.4))
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::thinking())
            .child(summary)
    }

    /// Add-agent control: add a participant with `kind = Agent` so it can be
    /// @mentioned + auto-convened (OCEAN-119 / OCEAN-111). Two text rows (agent id
    /// + optional display name) routed by the rooms key handler, plus a button.
    fn render_rooms_add_agent(&self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let id_draft = self.rooms.agent_id_draft.clone();
        let id_placeholder = id_draft.is_empty();
        let name_draft = self.rooms.agent_name_draft.clone();
        let name_placeholder = name_draft.is_empty();
        let can_add = self.rooms.can_add_agent();
        let id_focused = self.rooms.focus == RoomFocus::AgentId;
        let name_focused = self.rooms.focus == RoomFocus::AgentName;

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_b(px(1.0))
            .border_color(theme::rule())
            .track_focus(&self.agent_focus)
            .on_key_down(cx.listener(Self::on_rooms_key_down))
            .child(
                div()
                    .id("rooms-addagent-id")
                    .flex_1()
                    .min_h(px(28.0))
                    .px_2()
                    .py_1()
                    .bg(theme::background())
                    .border_1()
                    .border_color(if id_focused {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if id_placeholder {
                        theme::muted()
                    } else {
                        theme::ink()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.rooms.focus = RoomFocus::AgentId;
                        window.focus(&shell.agent_focus);
                        cx.notify();
                    }))
                    .child(if id_placeholder {
                        "agent id (e.g. flux)".to_string()
                    } else {
                        id_draft
                    }),
            )
            .child(
                div()
                    .id("rooms-addagent-name")
                    .flex_1()
                    .min_h(px(28.0))
                    .px_2()
                    .py_1()
                    .bg(theme::background())
                    .border_1()
                    .border_color(if name_focused {
                        theme::accent()
                    } else {
                        theme::rule()
                    })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if name_placeholder {
                        theme::muted()
                    } else {
                        theme::ink()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.rooms.focus = RoomFocus::AgentName;
                        window.focus(&shell.agent_focus);
                        cx.notify();
                    }))
                    .child(if name_placeholder {
                        "display name (optional)".to_string()
                    } else {
                        name_draft
                    }),
            )
            .child(
                div()
                    .id("rooms-addagent-btn")
                    .px_2()
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if can_add {
                        theme::accent()
                    } else {
                        theme::panel()
                    })
                    .border_1()
                    .border_color(if can_add { theme::accent() } else { theme::rule() })
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.add_agent_from_draft(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if can_add {
                                theme::background()
                            } else {
                                theme::muted()
                            })
                            .child("🤖 Add agent"),
                    ),
            )
    }

    /// @mention discoverability: list the open room's agent ids so a human knows
    /// who they can mention to auto-convene. Clicking a chip inserts `@id ` into
    /// the composer (OCEAN-119, matching OCEAN-117). `None` when no agents.
    fn render_rooms_mention_hint(&self, cx: &mut Context<Self>) -> Option<Div> {
        let agent_ids = self.rooms.agent_ids();
        if agent_ids.is_empty() {
            return None;
        }
        let mut hint = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .bg(theme::frame())
            .border_t(px(1.0))
            .border_color(theme::rule().opacity(0.4))
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("@agents:"),
            );

        for (index, id) in agent_ids.into_iter().enumerate() {
            let insert = id.clone();
            hint = hint.child(
                div()
                    .id(("rooms-mention-chip", index))
                    .px_2()
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::accent())
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::accent_dark())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.rooms.insert_mention(&insert);
                        cx.notify();
                    }))
                    .child(format!("@{id}")),
            );
        }
        Some(hint)
    }

    fn render_rooms_transcript(&self, _cx: &mut Context<Self>) -> Stateful<Div> {
        let mut transcript = div()
            .id("rooms-transcript")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h(px(0.0))
            .px_4()
            .py_3()
            .overflow_y_scroll();

        if self.rooms.transcript.is_empty() {
            transcript = transcript.child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child("No messages yet. Say something — use @id to convene an agent."),
            );
            return transcript;
        }

        for message in &self.rooms.transcript {
            transcript = transcript.child(self.render_rooms_message(message));
        }
        transcript
    }

    fn render_rooms_message(&self, message: &RoomMessage) -> Div {
        let is_system = message.kind.is_system();
        let mut row = div().flex().flex_col().gap_1();

        if is_system {
            return row.child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(message.body.clone()),
            );
        }

        row = row.child(
            div()
                .font_family(theme::MONO_FONT)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::accent_dark())
                .child(author_label(&message.author_id, message.author_kind)),
        );
        row.child(
            div()
                .font_family(theme::UI_FONT)
                .text_size(px(13.5))
                .line_height(px(20.0))
                .text_color(theme::ink())
                .child(message.body.clone()),
        )
    }

    fn render_rooms_composer(&self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let draft = self.rooms.composer_draft.clone();
        let placeholder = draft.is_empty();
        let can_send = self.rooms.can_send();
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(54.0))
            .px_4()
            .py_2()
            .bg(theme::frame())
            .border_t(px(1.0))
            .border_color(theme::rule())
            .track_focus(&self.agent_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event: &MouseDownEvent, window, cx| {
                    shell.rooms.focus = RoomFocus::Composer;
                    window.focus(&shell.agent_focus);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::on_rooms_key_down))
            .child(
                div()
                    .flex_1()
                    .min_h(px(34.0))
                    .px_3()
                    .py_2()
                    .bg(theme::background())
                    .border_1()
                    .border_color(theme::rule())
                    .font_family(theme::UI_FONT)
                    .text_size(px(13.5))
                    .text_color(if placeholder {
                        theme::muted()
                    } else {
                        theme::ink()
                    })
                    .child(if placeholder {
                        "Message... (@id to mention)".to_string()
                    } else {
                        draft
                    }),
            )
            .child(
                div()
                    .id("rooms-send")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(64.0))
                    .h(px(34.0))
                    .bg(if can_send {
                        theme::accent()
                    } else {
                        theme::panel()
                    })
                    .border_1()
                    .border_color(if can_send { theme::accent() } else { theme::rule() })
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if can_send {
                        theme::background()
                    } else {
                        theme::muted()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.post_room_message(cx);
                        cx.notify();
                    }))
                    .child("Send"),
            )
    }

    /// Key handling for the rooms panel inputs. Routes typed text to the open
    /// room's composer draft, or to the new-room name draft when no room is open.
    /// Enter creates/sends; backspace deletes; Escape closes the open room (or
    /// the panel).
    fn on_rooms_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let in_room = self.rooms.open_key.is_some();

        // Typed text routes to whichever rooms input is focused (OCEAN-119): the
        // composer / add-agent inputs in a room, or the new-room name / cron input
        // in the list view. `push_typed`/`pop_typed` resolve the live draft.
        let focus = self.rooms.focus;

        if modifiers.secondary() && !modifiers.alt && key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.rooms.push_typed(&text);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let handled = match key {
            "escape" => {
                if in_room {
                    self.close_room(cx);
                } else {
                    self.rooms.panel_open = false;
                }
                true
            }
            "enter" => {
                // Enter acts on the focused input: add an agent from the add-agent
                // inputs, otherwise post (room) / create (list).
                match focus {
                    RoomFocus::AgentId | RoomFocus::AgentName if in_room => {
                        self.add_agent_from_draft(cx);
                    }
                    _ if in_room => self.post_room_message(cx),
                    _ => self.create_room_from_draft(cx),
                }
                true
            }
            "backspace" | "delete" => {
                self.rooms.pop_typed();
                true
            }
            _ => {
                if let Some(text) = command_palette_text(event) {
                    self.rooms.push_typed(&text);
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

    fn open_surface_canvas(&mut self, title: &str, mode: SurfaceMode) {
        let canvas_id = self.surface.open_canvas_pane(title, mode);
        let tldraw_room_id = self
            .surface
            .canvas(&canvas_id)
            .map(|ledger| ledger.tldraw_room_id.clone());
        if let Some(tldraw_room_id) = tldraw_room_id {
            let _ = self.surface.apply_ipc_event(SurfaceIpcEvent::CanvasReady {
                pane_id: self.surface.active_pane_id().to_string(),
                canvas_id: canvas_id.clone(),
                tldraw_room_id,
            });
        }
        self.agent.status = format!("opened {canvas_id}");
        self.sync_surface_livekit_update();
    }

    fn detach_active_surface_pane(&mut self) {
        let pane_id = self.surface.active_pane_id().to_string();
        if self.surface.detach_pane(&pane_id) {
            self.agent.status = format!("detached {pane_id}");
            self.sync_surface_livekit_update();
        }
    }

    fn attach_active_surface_pane(&mut self) {
        let pane_id = self.surface.active_pane_id().to_string();
        if self.surface.attach_pane(&pane_id, PaneDock::Right) {
            self.agent.status = format!("attached {pane_id}");
            self.sync_surface_livekit_update();
        }
    }

    fn request_surface_livekit_token(&mut self, cx: &mut Context<Self>) {
        match self.surface_livekit.join_state() {
            SurfaceLiveKitJoinState::RequestingToken | SurfaceLiveKitJoinState::Joining => {
                self.agent.status = self.surface_livekit.status_label();
                return;
            }
            SurfaceLiveKitJoinState::Joined => {
                self.disconnect_surface_livekit();
                return;
            }
            SurfaceLiveKitJoinState::TokenReady => {
                self.start_surface_livekit_join(cx);
                return;
            }
            SurfaceLiveKitJoinState::NotJoined | SurfaceLiveKitJoinState::Failed => {}
        }

        let url = self.daemon.url.clone();
        let room_id = self.surface_livekit.room_id().to_string();
        let request = self.surface_livekit.begin_token_request();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.livekit_token(&url, &room_id, &request));
            let _ = sender.send(SurfaceLiveKitMessage::Token(result));
        });

        self.surface_livekit_task = Some(spawn_surface_livekit_task(receiver, cx));
    }

    fn disconnect_surface_livekit(&mut self) {
        let Some(client) = self.surface_livekit_client.clone() else {
            self.surface_livekit.mark_disconnected("not connected");
            self.agent.status = self.surface_livekit.status_label();
            return;
        };

        match client.try_disconnect() {
            Ok(()) => {
                self.surface_livekit_client = None;
                self.surface_livekit.mark_disconnected("leaving room");
                self.agent.status = "leaving hangout".to_string();
            }
            Err(error) => {
                self.agent.status = error.to_string();
            }
        }
    }

    fn start_surface_livekit_join(&mut self, cx: &mut Context<Self>) {
        let Some(credentials) = self.surface_livekit.credentials().cloned() else {
            self.surface_livekit
                .mark_failed("missing LiveKit credentials after token response");
            self.agent.status = self.surface_livekit.status_label();
            return;
        };

        let room_metadata = match self
            .surface_livekit
            .room_metadata_json(&self.surface, self.agent.session_id.as_deref())
        {
            Ok(metadata) => metadata,
            Err(error) => {
                self.surface_livekit
                    .mark_failed(format!("metadata encode error: {error}"));
                self.agent.status = self.surface_livekit.status_label();
                return;
            }
        };
        let participant_attributes = self
            .surface_livekit
            .participant_attributes(self.surface.session_id());
        let request = SurfaceLiveKitJoinRequest::new(
            credentials,
            room_metadata,
            participant_attributes,
            self.surface_livekit.mic_enabled(),
            self.surface_livekit.camera_enabled(),
        );
        let (sender, receiver) = mpsc::channel();
        let (client_sender, client_receiver) = mpsc::channel();

        self.surface_livekit_client = Some(spawn_surface_livekit_client(request, client_sender));
        thread::spawn(move || {
            for event in client_receiver {
                let _ = sender.send(SurfaceLiveKitMessage::Client(event));
            }
        });

        self.surface_livekit.mark_joining();
        self.agent.status = self.surface_livekit.status_label();
        self.surface_livekit_task = Some(spawn_surface_livekit_task(receiver, cx));
    }

    fn apply_surface_livekit_message(
        &mut self,
        message: SurfaceLiveKitMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            SurfaceLiveKitMessage::Token(Ok(response)) if response.ok => {
                if self.surface_livekit.apply_token_response(response).is_ok() {
                    let room_id = RoomId::from(self.surface_livekit.room_id().to_string());
                    self.gui_control.apply(GuiCommand::SwitchRoom { room_id });
                    self.agent.status = self.surface_livekit.status_label();
                    self.start_surface_livekit_join(cx);
                }
            }
            SurfaceLiveKitMessage::Token(Ok(response)) => {
                let error = response.error.unwrap_or_else(|| "token denied".to_string());
                self.surface_livekit.mark_failed(error.clone());
                self.agent.status = error;
            }
            SurfaceLiveKitMessage::Token(Err(error)) => {
                self.surface_livekit.mark_failed(error.clone());
                self.agent.status = format!("token error: {error}");
            }
            SurfaceLiveKitMessage::Client(event) => self.apply_surface_livekit_client_event(event),
        }
    }

    fn apply_surface_livekit_client_event(&mut self, event: SurfaceLiveKitClientEvent) {
        match event {
            SurfaceLiveKitClientEvent::Joining { room } => {
                self.surface_livekit.mark_joining();
                self.agent.status = format!("joining {room}");
            }
            SurfaceLiveKitClientEvent::Joined { room, participant } => {
                self.surface_livekit.mark_joined();
                self.agent.status = format!("joined {room} as {participant}");
            }
            SurfaceLiveKitClientEvent::MetadataPublished { room } => {
                self.agent.status = format!("published surface state to {room}");
            }
            SurfaceLiveKitClientEvent::SurfaceStatePublished { room } => {
                self.agent.status = format!("synced surface state to {room}");
            }
            SurfaceLiveKitClientEvent::SurfaceStateFailed { room, error } => {
                self.agent.status = format!("{room} surface sync failed: {error}");
            }
            SurfaceLiveKitClientEvent::MicrophonePublished { room, track_sid } => {
                self.agent.status = format!("{room} microphone live {track_sid}");
            }
            SurfaceLiveKitClientEvent::MicrophoneUnpublished { room } => {
                self.agent.status = format!("{room} microphone off");
            }
            SurfaceLiveKitClientEvent::MicrophoneFailed { room, error } => {
                self.surface_livekit.set_mic_enabled(false);
                self.agent.status = format!("{room} microphone failed: {error}");
                self.sync_surface_livekit_update();
            }
            SurfaceLiveKitClientEvent::CameraPublished { room, track_sid } => {
                self.agent.status = format!("{room} camera live {track_sid}");
            }
            SurfaceLiveKitClientEvent::CameraUnpublished { room } => {
                self.agent.status = format!("{room} camera off");
            }
            SurfaceLiveKitClientEvent::CameraFailed { room, error } => {
                self.surface_livekit.toggle_camera();
                self.agent.status = format!("{room} camera failed: {error}");
                self.sync_surface_livekit_update();
            }
            SurfaceLiveKitClientEvent::RemoteVideoSubscribed {
                room,
                participant_identity,
                track_sid,
            } => {
                self.surface_video_tiles.insert(
                    track_sid,
                    SurfaceVideoTile {
                        participant_identity: participant_identity.clone(),
                        width: 0,
                        height: 0,
                        image: None,
                    },
                );
                self.agent.status = format!("{room} video tile {participant_identity}");
            }
            SurfaceLiveKitClientEvent::RemoteVideoRemoved {
                room, track_sid, ..
            } => {
                self.surface_video_tiles.remove(&track_sid);
                self.agent.status = format!("{room} video tile removed");
            }
            SurfaceLiveKitClientEvent::RemoteVideoFrame { room: _, frame } => {
                self.apply_remote_video_frame(frame);
            }
            SurfaceLiveKitClientEvent::MediaFailed { room, error } => {
                self.agent.status = format!("{room} media failed: {error}");
            }
            SurfaceLiveKitClientEvent::ConnectionState { room, state } => {
                self.agent.status = format!("{room} {state}");
            }
            SurfaceLiveKitClientEvent::RosterUpdated { room, participants } => {
                let remote = participants.iter().filter(|p| !p.local).count();
                self.surface_livekit.set_roster(participants);
                self.agent.status = format!("{room} roster: {remote} remote");
            }
            SurfaceLiveKitClientEvent::Disconnected { room, reason } => {
                self.surface_livekit
                    .mark_disconnected(format!("{room} disconnected: {reason}"));
                self.surface_livekit_client = None;
                self.surface_video_tiles.clear();
                self.agent.status = format!("{room} disconnected: {reason}");
            }
            SurfaceLiveKitClientEvent::Failed { room, error } => {
                self.surface_livekit.mark_failed(error.clone());
                self.surface_livekit_client = None;
                self.surface_video_tiles.clear();
                self.agent.status = format!("{room} failed: {error}");
            }
        }
    }

    /// Store the latest decoded frame for a remote video tile.
    ///
    /// The decoded BGRA bytes are wrapped in a `gpui::RenderImage` (a
    /// main-thread-only type, which is why construction happens here in the view
    /// rather than on the LiveKit worker thread). Frames arrive on a one-deep
    /// queue, so this is naturally latest-wins under load. Frames for tracks we
    /// have no tile for (e.g. a frame that races ahead of `RemoteVideoSubscribed`)
    /// lazily create the tile.
    fn apply_remote_video_frame(&mut self, frame: SurfaceVideoFrame) {
        if !frame.is_renderable() {
            return;
        }
        let image = render_image_from_bgra(frame.width, frame.height, &frame.bgra);
        let tile = self
            .surface_video_tiles
            .entry(frame.track_sid.clone())
            .or_insert_with(|| SurfaceVideoTile {
                participant_identity: frame.participant_identity.clone(),
                width: frame.width,
                height: frame.height,
                image: None,
            });
        tile.participant_identity = frame.participant_identity;
        tile.width = frame.width;
        tile.height = frame.height;
        tile.image = Some(image);
    }

    fn current_surface_livekit_update(&self) -> Result<SurfaceLiveKitSurfaceUpdate, String> {
        let room_metadata = self
            .surface_livekit
            .room_metadata_json(&self.surface, self.agent.session_id.as_deref())
            .map_err(|error| format!("metadata encode error: {error}"))?;
        let participant_attributes = self
            .surface_livekit
            .participant_attributes(self.surface.session_id());
        Ok(SurfaceLiveKitSurfaceUpdate::new(
            room_metadata,
            participant_attributes,
            self.surface_livekit.mic_enabled(),
            self.surface_livekit.camera_enabled(),
        ))
    }

    fn sync_surface_livekit_update(&mut self) {
        let Some(client) = self.surface_livekit_client.clone() else {
            return;
        };
        let update = match self.current_surface_livekit_update() {
            Ok(update) => update,
            Err(error) => {
                self.agent.status = error;
                return;
            }
        };
        if let Err(error) = client.try_update_surface(update) {
            self.agent.status = error.to_string();
        }
    }

    fn toggle_surface_mic(&mut self) {
        let enabled = self.surface_livekit.toggle_mic();
        self.agent.status = if enabled {
            "surface mic intent on".to_string()
        } else {
            "surface mic intent off".to_string()
        };
        self.sync_surface_livekit_update();
    }

    fn toggle_surface_camera(&mut self) {
        let enabled = self.surface_livekit.toggle_camera();
        self.agent.status = if enabled {
            "surface camera intent on".to_string()
        } else {
            "surface camera intent off".to_string()
        };
        self.sync_surface_livekit_update();
    }

    fn sync_surface_canvas_host(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        self.drain_surface_canvas_ipc();

        let target = self
            .active_surface_canvas_web_url()
            .map(|url| CanvasHostTarget {
                pane_id: self.surface.active_pane_id().to_string(),
                url,
                bounds: HostBounds::from_gpui(bounds),
            });
        self.surface_host.sync_target(target);

        if let Some(command) = self.active_surface_load_command() {
            self.surface_host.sync_command(&command);
        }

        self.flush_surface_host_actions(window);
    }

    fn hide_surface_canvas_host(&mut self, window: &mut Window) {
        self.surface_host.sync_target(None);
        self.flush_surface_host_actions(window);
    }

    fn drain_surface_canvas_ipc(&mut self) {
        loop {
            match self.surface_ipc_receiver.try_recv() {
                Ok(payload) => {
                    if let Err(error) = self.surface_host.push_event_json(&payload) {
                        self.agent.status = format!("canvas ipc error: {error}");
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.agent.status = "canvas ipc disconnected".to_string();
                    break;
                }
            }
        }

        while let Some(event) = self.surface_host.pop_event() {
            match event {
                SurfaceIpcEvent::CanvasError {
                    pane_id,
                    canvas_id,
                    message,
                } => {
                    let where_text = canvas_id
                        .or(pane_id)
                        .unwrap_or_else(|| "canvas".to_string());
                    self.agent.status = format!("{where_text} error: {message}");
                }
                event => {
                    if self.surface.apply_ipc_event(event) {
                        self.agent.status = "canvas state updated".to_string();
                        self.sync_surface_livekit_update();
                    }
                }
            }
        }
    }

    fn flush_surface_host_actions(&mut self, window: &mut Window) {
        while let Some(action) = self.surface_host.pop_action() {
            if let Err(error) = self.surface_webview_host.apply_action(window, action) {
                self.agent.status = format!("canvas host error: {error}");
            }
        }
    }

    fn drop_surface_markdown_card(&mut self) {
        let Some(canvas_id) = self.active_surface_canvas_id() else {
            self.agent.status = "no active canvas".to_string();
            return;
        };
        self.upsert_surface_markdown_card(
            canvas_id,
            "Draft card from Ocean Surface".to_string(),
            "card".to_string(),
        );
    }

    fn pin_agent_turn_to_canvas(&mut self, turn_index: usize) {
        let Some(text) = self.agent_turn_canvas_text(turn_index) else {
            self.agent.status = "nothing to pin".to_string();
            return;
        };
        let Some(canvas_id) = self.active_surface_canvas_id() else {
            self.agent.status = "no active canvas".to_string();
            return;
        };
        self.upsert_surface_markdown_card(canvas_id, text, format!("turn-{turn_index}"));
    }

    fn upsert_surface_markdown_card(&mut self, canvas_id: String, text: String, id_prefix: String) {
        let Some(slot) = self.surface.next_slot(&canvas_id, 240.0, 160.0) else {
            self.agent.status = "canvas has no free slot".to_string();
            return;
        };
        let component_count = self
            .surface
            .canvas(&canvas_id)
            .map(|ledger| ledger.components.len())
            .unwrap_or(0);
        let component_id = format!("{id_prefix}-{}", component_count + 1);
        let component = LedgerComponent::markdown_card(component_id.clone(), slot.x, slot.y, text);
        if self.surface.upsert_component(&canvas_id, component.clone()) {
            self.surface_host
                .sync_command(&SurfaceIpcCommand::UpsertComponent {
                    canvas_id,
                    component,
                });
            self.agent.status = format!("pinned {component_id}");
            self.sync_surface_livekit_update();
        }
    }

    fn agent_turn_canvas_text(&self, turn_index: usize) -> Option<String> {
        let turn = self.agent.turns.get(turn_index)?;
        let mut text = String::new();
        for block in &turn.blocks {
            match block {
                AgentBlock::Text(value) => {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(value.trim());
                }
                AgentBlock::Component {
                    component_id, kind, ..
                } => {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&format!("[{kind} component: {component_id}]"));
                }
                AgentBlock::ToolCall { name, status, .. } => {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&format!("[tool: {name} · {status:?}]"));
                }
                AgentBlock::Thinking { .. } => {}
            }
        }
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn open_surface_canvas_preview(&mut self) {
        let Some(url) = self.active_surface_canvas_web_url() else {
            self.agent.status = "canvas web assets missing".to_string();
            return;
        };

        match Command::new("open").arg(&url).spawn() {
            Ok(_) => {
                self.agent.status = "opened canvas".to_string();
            }
            Err(error) => {
                self.agent.status = format!("canvas open failed: {error}");
            }
        }
    }

    fn active_surface_canvas_id(&self) -> Option<String> {
        self.surface
            .active_canvas_id()
            .map(str::to_string)
            .or_else(|| Some(DEFAULT_CANVAS_ID.to_string()))
    }

    fn active_surface_canvas_web_url(&self) -> Option<String> {
        let index_path = canvas_web_index_path()?;
        let pane = self
            .surface
            .panes()
            .iter()
            .find(|pane| pane.pane_id == self.surface.active_pane_id())?;
        let sync_uri = std::env::var("OCEAN_TLDRAW_SYNC_URI").ok();
        canvas_web_url(
            &index_path,
            self.surface.session_id(),
            pane,
            sync_uri.as_deref(),
        )
    }

    fn active_surface_load_command(&self) -> Option<SurfaceIpcCommand> {
        let pane = self
            .surface
            .panes()
            .iter()
            .find(|pane| pane.pane_id == self.surface.active_pane_id())?;
        let canvas_id = pane.canvas_id.clone()?;
        let tldraw_room_id = pane.tldraw_room_id.clone()?;
        Some(SurfaceIpcCommand::LoadCanvas {
            pane_id: pane.pane_id.clone(),
            canvas_id,
            tldraw_room_id,
        })
    }

    fn surface_canvas_count(&self) -> usize {
        self.surface.turn_context().canvases.len()
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
        let Some(mut prompt) = self.agent.take_prompt_for_submit() else {
            return;
        };
        if self.active_surface == SurfaceTab::Surface {
            prompt = prompt_with_surface_context(&prompt, &self.surface.turn_context());
        }
        // OCEAN-154 / Slice 7: fold the native CanvasLedger's compact context into
        // the outgoing prompt so the model sees the authoritative canvas state
        // (ids/kinds/rects/edges/selection/mode/viewport) each turn and can drive
        // it with `surface_patch`. This rides on the PROMPT field, not the
        // discarded `guidance` field (OCEAN-143), so it actually reaches the model.
        prompt = prompt_with_canvas_context(&prompt, self.canvas_ledger().as_ref());

        // With a project selected, send an empty cwd so the daemon binds to the
        // project's workspace_root (a non-empty cwd would win). Otherwise fall
        // back to the GUI's own root directory as before.
        let cwd = if self.current_project.is_some() {
            String::new()
        } else {
            self.state.root.display().to_string()
        };
        self.agent_scroll.scroll_to_bottom();

        let project_id = self.current_project.clone();
        let client_type = "surface-gpui".to_string();
        match self.agent.session_id.clone() {
            Some(session_id) => {
                let request = AgentTurnRequest {
                    prompt,
                    cwd,
                    session_id: Some(session_id),
                    project_id,
                    client_type: Some(client_type),
                    // Per-turn overrides aren't surfaced in the GPUI shell yet;
                    // send None so the daemon applies its global defaults. The
                    // fields exist so the request matches the daemon's current
                    // AgentTurnRequest wire shape (OCEAN-61).
                    guidance: None,
                    room_id: None,
                    thinking_level: None,
                    model_id: None,
                };
                self.spawn_agent_turn_submit(request, cx);
            }
            None => {
                self.spawn_agent_session_prepare(prompt, cwd, project_id, client_type, cx);
            }
        }
    }

    fn spawn_agent_session_prepare(
        &mut self,
        prompt: String,
        cwd: String,
        project_id: Option<String>,
        client_type: String,
        cx: &mut Context<Self>,
    ) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();
        self.agent.status = "creating session".to_string();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| {
                let create = AgentSessionCreateRequest {
                    title: session_title_hint(&prompt),
                    workspace_root: cwd.clone(),
                    project_id: project_id.clone(),
                    client_type: Some(client_type.clone()),
                };
                let response = client.create_session(&url, &create)?;
                if !response.ok {
                    return Err(response
                        .error
                        .unwrap_or_else(|| "session creation failed".to_string()));
                }
                let session_id = response
                    .session_id
                    .ok_or_else(|| "session creation returned no session id".to_string())?;
                let request = AgentTurnRequest {
                    prompt,
                    cwd,
                    session_id: Some(session_id.clone()),
                    project_id,
                    client_type: Some(client_type),
                    // See note above: per-turn overrides not yet surfaced in the
                    // GPUI shell; fields present for wire parity (OCEAN-61).
                    guidance: None,
                    room_id: None,
                    thinking_level: None,
                    model_id: None,
                };
                Ok(AgentSubmitMessage::SessionReady {
                    session_id,
                    title: response.title,
                    request,
                })
            });

            let result = result.unwrap_or_else(AgentSubmitMessage::Error);
            let _ = sender.send(result);
        });

        self.agent_submit_task = Some(spawn_agent_submit_task(receiver, cx));
    }

    fn spawn_agent_turn_submit(&mut self, request: AgentTurnRequest, cx: &mut Context<Self>) {
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

        // Bump the generation and capture it for this listener. Any older
        // listener thread still running will see the shared counter advance and
        // stop forwarding — so a "new session" can't be polluted by the prior
        // session's in-flight events.
        let generation = self.agent_event_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(session_id) = self.agent.session_id.clone() else {
            self.agent.status = "new session".to_string();
            self.agent_event_task = None;
            return;
        };

        self.agent.status = "connecting stream".to_string();
        let active_generation = Arc::clone(&self.agent_event_generation);
        // Scope the SSE subscription to the session we're on, so the daemon
        // only ships this session's events down this connection.

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| {
                client.stream_agent_events(&url, Some(session_id.as_str()), |event| {
                    // Superseded by a newer connect → stop this stale listener.
                    if active_generation.load(Ordering::SeqCst) != generation {
                        return Err("superseded by newer agent stream".to_string());
                    }
                    sender
                        .send(AgentStreamMessage::Event(event))
                        .map_err(|error| error.to_string())
                })
            });

            // Only surface an error if we're still the active listener; a
            // superseded thread exiting is expected, not a failure to show.
            if let Err(error) = result {
                if active_generation.load(Ordering::SeqCst) == generation {
                    let _ = sender.send(AgentStreamMessage::Error(error));
                }
            }
        });

        self.agent_event_task = Some(spawn_agent_event_task(receiver, cx));

        // Permission requests ride a SEPARATE stream. The product event stream
        // `/v1/agent/events` only carries `AgentTurnEvent` types and serializes
        // the inner event (no `permission_id`). The daemon emits
        // `OceanEvent::PermissionRequest` — WITH the envelope's `permission_id`
        // — onto the control stream `/v1/events`. Open that too, gated on the
        // same generation, so a gated mutating turn surfaces an approval banner
        // live instead of waiting on the catalogue poll. When gating is off the
        // daemon never emits these frames, so this stream sits idle.
        //
        // Session filtering happens in `apply_control_event` against the live
        // `self.agent.session_id`, so the listener itself doesn't need the id.
        self.connect_control_events(generation, cx);
    }

    fn connect_control_events(&mut self, generation: u64, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::sync_channel(256);
        let active_generation = Arc::clone(&self.agent_event_generation);

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| {
                client.stream_control_events(&url, |event| {
                    if active_generation.load(Ordering::SeqCst) != generation {
                        return Err("superseded by newer control stream".to_string());
                    }
                    sender
                        .send(AgentControlStreamMessage::Event(event))
                        .map_err(|error| error.to_string())
                })
            });

            // A superseded thread exiting is expected; only the active listener
            // reports a real failure.
            if let Err(error) = result
                && active_generation.load(Ordering::SeqCst) == generation
            {
                let _ = sender.send(AgentControlStreamMessage::Error(error));
            }
        });

        self.agent_control_stream_task = Some(spawn_agent_control_stream_task(receiver, cx));
    }

    fn apply_agent_control_stream_messages(&mut self, messages: Vec<AgentControlStreamMessage>) {
        for message in messages {
            match message {
                AgentControlStreamMessage::Event(event) => self.apply_control_event(event),
                AgentControlStreamMessage::Error(error) => {
                    // Don't clobber the agent status with control-stream noise;
                    // log-level visibility is enough for an idle/ungated path.
                    self.agent.status = format!("control stream: {error}");
                }
            }
        }
    }

    /// Apply one control-stream frame to the pending-permission queue, scoped to
    /// the active session. A `permission_request` enqueues a card (deduped by
    /// id); a `permission_decision` removes the matching card (the daemon
    /// decided it, possibly from another surface like the TUI or web). Frames
    /// for another session, or without a `permission_id`, are dropped. Mirrors
    /// the web surface's `apply_control_event` (OCEAN-64).
    fn apply_control_event(&mut self, event: ControlEvent) {
        let active = self.agent.session_id.clone();
        match event {
            ControlEvent::PermissionRequest {
                permission_id,
                session_id,
                tool,
                reason,
                args,
            } => {
                // Hard session isolation, matching the agent stream: a frame
                // must carry exactly the active session id, else drop it.
                if active.is_none() || session_id.as_deref() != active.as_deref() {
                    return;
                }
                let Some(permission_id) = permission_id else {
                    return;
                };
                // Dedupe: the daemon reuses one PermissionId for an identical
                // tool+args retry within a turn, so a replayed frame must not
                // stack a second banner.
                if self
                    .pending_permissions
                    .iter()
                    .any(|permission| permission.permission_id == permission_id)
                {
                    return;
                }
                self.pending_permissions.push(PermissionStatus {
                    permission_id,
                    request_id: String::new(),
                    session_id,
                    tool,
                    reason,
                    args,
                    created_at: String::new(),
                });
                self.agent.status = "permission requested".to_string();
            }
            ControlEvent::PermissionDecision {
                permission_id,
                session_id,
            } => {
                if session_id.as_deref() != active.as_deref() {
                    return;
                }
                if let Some(permission_id) = permission_id {
                    self.pending_permissions
                        .retain(|permission| permission.permission_id != permission_id);
                }
            }
            ControlEvent::Other => {}
        }
    }

    fn apply_agent_stream_messages(
        &mut self,
        messages: Vec<AgentStreamMessage>,
        cx: &mut Context<Self>,
    ) {
        let should_stick_to_bottom = self.should_stick_agent_transcript_to_bottom();
        let mut accepted_event = false;

        for message in messages {
            match message {
                AgentStreamMessage::Event(event) => {
                    accepted_event |= self.apply_agent_event(event, cx);
                }
                AgentStreamMessage::Error(error) => {
                    self.agent.status = format!("stream error: {error}");
                    self.gui_control.apply(GuiCommand::SetStatus {
                        text: "stream error".to_string(),
                    });
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

    fn apply_agent_submit_message(&mut self, message: AgentSubmitMessage, cx: &mut Context<Self>) {
        match message {
            AgentSubmitMessage::SessionReady {
                session_id,
                title,
                request,
            } => {
                self.gui_control.apply(GuiCommand::OpenSession {
                    session_id: session_id.clone(),
                });
                self.agent.session_id = Some(session_id);
                if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
                    self.agent.session_title = title;
                }
                self.agent.status = "session ready".to_string();
                self.connect_agent_events(cx);
                self.spawn_agent_turn_submit(request, cx);
            }
            AgentSubmitMessage::Response(response) if response.ok => {
                if self.agent.session_id.is_none() {
                    self.gui_control.apply(GuiCommand::OpenSession {
                        session_id: response.session_id.clone(),
                    });
                    self.agent.session_id = Some(response.session_id);
                }
                self.agent.status = response.status;
                self.refresh_agent_sessions(cx);
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

    fn apply_agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) -> bool {
        let event_session_id = event.session_id().map(str::to_string);

        if let Some(event_session_id) = event_session_id.as_deref() {
            match self.agent.session_id.as_deref() {
                Some(current) if current != event_session_id => {
                    return false;
                }
                None => return false,
                _ => {}
            }
        }

        let component_already_mounted = match &event {
            AgentEvent::ComponentRender { component_id, .. } => self
                .gui_control
                .component(&ComponentId::from(component_id.as_str()))
                .is_some(),
            _ => false,
        };

        if let Some(command) = gui_command_for_agent_event(&event, component_already_mounted) {
            self.gui_control.apply(command);
        }

        // Translate agent render/component commands into tldraw canvas
        // upserts so they actually paint shapes on the active canvas, keyed by
        // component_id (OCEAN-78). Re-renders upsert in place rather than
        // duplicating, and the ledger persists the component across turns.
        match &event {
            AgentEvent::ComponentRender {
                component_id,
                kind,
                props,
                ..
            } => self.render_agent_component_to_canvas(component_id, kind, props),
            AgentEvent::ComponentUnmount { component_id, .. } => {
                self.unmount_agent_component_from_canvas(component_id);
            }
            // GPUI Masterbuild Slice 6: apply daemon surface patches to the
            // native ledger so agent-driven canvas mutations paint without a
            // chat/tldraw fallback.
            AgentEvent::SurfacePatch {
                session_id,
                canvas_id,
                patches,
                ..
            } => {
                self.apply_surface_patch_event(
                    session_id.clone(),
                    canvas_id.clone(),
                    patches.clone(),
                    cx,
                );
            }
            _ => {}
        }

        self.agent.apply_event(event);
        true
    }

    /// Apply a daemon `surface_patch` event (Slice 6) to the active session's
    /// native [`CanvasLedger`]. Each [`SurfacePatchEnvelope`] is replayed through
    /// `CanvasLedger::apply_patch`, which bumps the ledger revision, records the
    /// patch in its log, and allocates placement for any component that omits a
    /// rect. The ledger is lazily created (keyed on the event's `canvas_id`) the
    /// first time a patch arrives, then the updated ledger is written back to the
    /// shared cell so the native [`OceanCanvasView`] repaints on its next frame.
    ///
    /// Each envelope carries its own actor/timestamp, so we preserve them rather
    /// than re-stamping — this keeps the agent attribution and ordering the
    /// daemon emitted. Repaint is driven by the caller's `cx.notify()` after the
    /// stream batch is applied; mutating the shared `canvas_ledger` cell is what
    /// the view reads on its next render.
    fn apply_surface_patch_event(
        &mut self,
        session_id: String,
        canvas_id: CanvasId,
        patches: Vec<SurfacePatchEnvelope>,
        cx: &mut Context<Self>,
    ) {
        let Some(ledger) =
            apply_patches_to_ledger(self.canvas_ledger(), session_id, canvas_id, patches)
        else {
            return;
        };

        self.set_canvas_ledger(Some(ledger));

        // §16 hot path: the canvas is its own GPUI entity reading the shared
        // ledger cell through its LedgerSource, so writing the cell is not enough
        // — the entity must be told to repaint. Notify it directly so the new
        // component paints the instant the patch arrives (notifying the shell
        // alone would not invalidate the child canvas entity).
        self.request_canvas_repaint(cx);
    }

    /// Mark the native canvas entity dirty so it repaints on the next frame, and
    /// bump the headless-observable repaint counter. This is the single repaint
    /// entry point for agent-driven canvas mutations (OCEAN-156).
    fn request_canvas_repaint(&self, cx: &mut Context<Self>) {
        self.canvas_repaint_requests.fetch_add(1, Ordering::SeqCst);
        self.canvas_view.update(cx, |_view, cx| cx.notify());
    }

    /// Number of native-canvas repaint requests issued so far (test/observation
    /// hook for the §16 patch hot path).
    pub fn canvas_repaint_request_count(&self) -> u64 {
        self.canvas_repaint_requests.load(Ordering::SeqCst)
    }

    /// Translate an agent `ComponentRender` into a tldraw shape upsert on the
    /// active canvas. Upserts by `component_id`: an existing component keeps its
    /// placement/size and is updated in place; a new one is allotted a free
    /// slot. The Rust-side surface ledger persists the component so it survives
    /// across turns and so re-renders dedupe rather than duplicate (OCEAN-78).
    fn render_agent_component_to_canvas(
        &mut self,
        component_id: &str,
        kind: &str,
        props: &serde_json::Value,
    ) {
        let Some(canvas_id) = self.active_surface_canvas_id() else {
            return;
        };

        let existing = self
            .surface
            .canvas(&canvas_id)
            .and_then(|ledger| ledger.components.get(component_id))
            .cloned();

        let (x, y, width, height) = if let Some(existing) = existing.as_ref() {
            // Preserve placement/size on re-render so the shape updates in place.
            (existing.x, existing.y, existing.width, existing.height)
        } else {
            let width = 240.0;
            let height = 160.0;
            let slot = self.surface.next_slot(&canvas_id, width, height);
            let (x, y) = slot.map_or((40.0, 40.0), |slot| (slot.x, slot.y));
            (x, y, width, height)
        };

        let component = LedgerComponent {
            id: component_id.to_string(),
            component_type: kind.to_string(),
            x,
            y,
            width,
            height,
            content: ledger_content_from_props(kind, props),
            metadata: serde_json::json!({ "source": "agent_render" }),
            connections: existing.map(|c| c.connections).unwrap_or_default(),
        };

        if self.surface.upsert_component(&canvas_id, component.clone()) {
            self.surface_host
                .sync_command(&SurfaceIpcCommand::UpsertComponent {
                    canvas_id,
                    component,
                });
            self.agent.status = format!("rendered {component_id} to canvas");
            self.sync_surface_livekit_update();
        }
    }

    /// Remove an agent-rendered component from the active canvas ledger and
    /// instruct the canvas-web bridge to focus-clear it. We only own removal of
    /// the ledger entry surface-side; the tldraw shape is dropped on the next
    /// load_canvas / ledger reconciliation (no dedicated delete command exists
    /// in the bridge yet — see PR follow-ups).
    fn unmount_agent_component_from_canvas(&mut self, component_id: &str) {
        let Some(canvas_id) = self.active_surface_canvas_id() else {
            return;
        };
        if self.surface.remove_component(&canvas_id, component_id) {
            self.agent.status = format!("unmounted {component_id} from canvas");
            self.sync_surface_livekit_update();
        }
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

    fn refresh_agent_catalogs(&mut self, cx: &mut Context<Self>) {
        self.refresh_agent_models(cx);
        self.refresh_agent_projects(cx);
        self.refresh_agent_sessions(cx);
        self.refresh_agent_permissions(cx);
    }

    fn refresh_agent_models(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_models(&url));
            let message = AgentModelsMessage::Refreshed(result);
            let _ = sender.send(message);
        });

        self.agent_models_task = Some(spawn_agent_models_task(receiver, cx));
    }

    fn refresh_agent_projects(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_projects(&url));
            let _ = sender.send(AgentProjectsMessage::Refreshed(result));
        });

        self.agent_projects_task = Some(spawn_agent_projects_task(receiver, cx));
    }

    fn apply_agent_projects_message(&mut self, message: AgentProjectsMessage) {
        match message {
            AgentProjectsMessage::Refreshed(Ok(response)) => {
                // Drop a stale selection that no longer exists in the catalogue.
                if let Some(sel) = &self.current_project {
                    if !response.projects.iter().any(|p| &p.id == sel) {
                        self.current_project = None;
                    }
                }
                self.project_catalog = response.projects;
            }
            AgentProjectsMessage::Refreshed(Err(error)) => {
                self.agent.status = format!("projects error: {error}");
            }
        }
    }

    fn select_agent_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        self.agent.model = Some(model_id.clone());
        self.agent.status = "switching model".to_string();
        self.model_picker_open = false;

        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| {
                client.set_model(&url, &model_id)?;
                client.fetch_models(&url)
            });
            let message = AgentModelsMessage::Swapped(result);
            let _ = sender.send(message);
        });

        self.agent_models_task = Some(spawn_agent_models_task(receiver, cx));
    }

    fn apply_agent_models_message(&mut self, message: AgentModelsMessage) {
        match message {
            AgentModelsMessage::Refreshed(Ok(response)) => {
                self.model_catalog = response.models;
                if let Some(current) = response.current {
                    if !current.model.is_empty() {
                        self.agent.model = Some(current.model);
                    }
                }
            }
            AgentModelsMessage::Swapped(Ok(response)) => {
                self.model_catalog = response.models;
                if let Some(current) = response.current {
                    if !current.model.is_empty() {
                        self.agent.model = Some(current.model);
                    }
                }
                if !self.agent.streaming {
                    self.agent.status = "model ready".to_string();
                }
            }
            AgentModelsMessage::Refreshed(Err(_)) => {}
            AgentModelsMessage::Swapped(Err(error)) => {
                self.agent.status = format!("model swap error: {error}");
            }
        }
    }

    fn refresh_agent_sessions(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_sessions(&url));
            let message = AgentSessionsMessage::Refreshed(result);
            let _ = sender.send(message);
        });

        self.agent_sessions_task = Some(spawn_agent_sessions_task(receiver, cx));
    }

    fn apply_agent_sessions_message(&mut self, message: AgentSessionsMessage) {
        match message {
            AgentSessionsMessage::Refreshed(Ok(response)) => {
                self.session_catalog = response.sessions;
            }
            AgentSessionsMessage::Refreshed(Err(_)) => {}
        }
    }

    fn refresh_agent_permissions(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_permissions(&url));
            let _ = sender.send(AgentPermissionsMessage::Refreshed(result));
        });

        self.agent_permissions_task = Some(spawn_agent_permissions_task(receiver, cx));
    }

    fn apply_agent_permissions_message(&mut self, message: AgentPermissionsMessage) {
        match message {
            AgentPermissionsMessage::Refreshed(Ok(response)) if response.ok => {
                // The `/v1/permissions` snapshot is daemon-wide. Keep only the
                // active session's requests so the banner stays session-scoped,
                // matching the control-stream path (OCEAN-75). A permission with
                // no session id is kept (older daemons), since we can't prove it
                // belongs to another session.
                let active = self.agent.session_id.clone();
                self.pending_permissions = response
                    .permissions
                    .into_iter()
                    .filter(|permission| match (&permission.session_id, &active) {
                        (Some(permission_session), Some(active_session)) => {
                            permission_session == active_session
                        }
                        (Some(_), None) => false,
                        (None, _) => true,
                    })
                    .collect();
            }
            AgentPermissionsMessage::Refreshed(Ok(response)) => {
                self.agent.status = format!(
                    "permissions error: {}",
                    response.error.unwrap_or_else(|| "unknown".to_string())
                );
            }
            AgentPermissionsMessage::Refreshed(Err(error)) => {
                self.agent.status = format!("permissions request error: {error}");
            }
        }
    }

    fn cancel_active_request(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.agent.active_turn_id.clone() else {
            self.agent.status = "no active request".to_string();
            return;
        };

        self.agent.status = "cancelling request".to_string();
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result =
                DaemonClient::new().and_then(|client| client.cancel_request(&url, &request_id));
            let _ = sender.send(AgentControlMessage::Cancelled(result));
        });

        self.agent_control_task = Some(spawn_agent_control_task(receiver, cx));
    }

    fn decide_latest_permission(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Some(permission_id) = self
            .pending_permissions
            .first()
            .map(|permission| permission.permission_id.clone())
        else {
            self.agent.status = "no pending permission".to_string();
            return;
        };
        self.decide_permission_by_id(permission_id, allow, cx);
    }

    /// POST an allow/deny decision for a specific permission to
    /// `/v1/permissions/{id}/decision` (OCEAN-75). Used by the per-card banner
    /// buttons so each pending request can be decided independently. The card is
    /// removed optimistically; the daemon also broadcasts a `permission_decision`
    /// frame on the control stream which prunes it on every attached surface
    /// (whichever lands first).
    fn decide_permission_by_id(
        &mut self,
        permission_id: String,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        if !self
            .pending_permissions
            .iter()
            .any(|permission| permission.permission_id == permission_id)
        {
            self.agent.status = "no pending permission".to_string();
            return;
        }

        let request = if allow {
            PermissionDecisionRequest::allow(permission_id.clone())
        } else {
            PermissionDecisionRequest::deny(permission_id.clone(), "denied from Ocean GUI")
        };

        // Optimistically clear the card so the banner reacts immediately; the
        // control-stream broadcast / decision response confirm the removal.
        self.pending_permissions
            .retain(|permission| permission.permission_id != permission_id);

        self.agent.status = "sending permission decision".to_string();
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result =
                DaemonClient::new().and_then(|client| client.decide_permission(&url, &request));
            let _ = sender.send(AgentControlMessage::PermissionDecided(result));
        });

        self.agent_control_task = Some(spawn_agent_control_task(receiver, cx));
    }

    // ---- Persistent rooms (OCEAN-109) ----------------------------------------

    /// Toggle the rooms panel. Opening it refreshes the room list.
    fn toggle_rooms_panel(&mut self, cx: &mut Context<Self>) {
        self.rooms.panel_open = !self.rooms.panel_open;
        if self.rooms.panel_open {
            // Closing the pickers keeps the agent surface uncluttered.
            self.model_picker_open = false;
            self.project_picker_open = false;
            self.session_picker_open = false;
            // Default typing target depends on whether a room is open.
            self.rooms.focus = if self.rooms.open_key.is_some() {
                RoomFocus::Composer
            } else {
                RoomFocus::NewRoomName
            };
            self.refresh_rooms(cx);
        }
        cx.notify();
    }

    /// Fetch the room list (`GET /v1/rooms/persistent`).
    fn refresh_rooms(&mut self, cx: &mut Context<Self>) {
        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_rooms(&url));
            let _ = sender.send(RoomsMessage::Listed(result));
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Create a room from the new-room draft (`POST /v1/rooms/persistent`), then
    /// open it. Keys are slugified from the name (matching the web surface) so a
    /// room created from either surface keys the same.
    fn create_room_from_draft(&mut self, cx: &mut Context<Self>) {
        let name = self.rooms.new_room_draft.trim().to_string();
        if name.is_empty() {
            return;
        }
        let key = slugify(&name);
        if key.is_empty() {
            self.rooms.status = "room name needs a letter or number".to_string();
            return;
        }
        // Snapshot the trigger-policy toggles into the create body before we
        // clear the draft (OCEAN-119). `None` when all-off so the daemon stores
        // no policy.
        let trigger_policy = self.rooms.collect_trigger_policy();
        self.rooms.new_room_draft.clear();
        self.rooms.status = format!("creating '{name}'...");

        let url = self.daemon.url.clone();
        let request = CreateRoomRequest {
            key: key.clone(),
            name,
            trigger_policy,
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.create_room(&url, &request));
            let _ = sender.send(RoomsMessage::Created { key, result });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Open a room: load its record + full transcript under a fresh generation,
    /// then start the live transcript-tail poll.
    fn open_room(&mut self, key: String, cx: &mut Context<Self>) {
        // Entering a room → typing targets the composer by default.
        self.rooms.focus = RoomFocus::Composer;
        let generation = self.rooms.begin_open(key.clone());
        let url = self.daemon.url.clone();
        let load_key = key.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = DaemonClient::new().and_then(|client| client.fetch_room(&url, &load_key));
            let _ = sender.send(RoomsMessage::Loaded {
                generation,
                key: load_key,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
        self.start_room_transcript_poll(key, generation, cx);
    }

    /// Close the open room (back to the list) and retire its poll loop.
    fn close_room(&mut self, cx: &mut Context<Self>) {
        self.rooms.close_room();
        // Back in the list → typing targets the new-room name input by default.
        self.rooms.focus = RoomFocus::NewRoomName;
        self.rooms_poll_task = None;
        cx.notify();
    }

    /// Join the open room as this surface's identity
    /// (`POST .../participants`).
    fn join_open_room(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.rooms.open_key.clone() else {
            return;
        };
        let url = self.daemon.url.clone();
        let request = RoomJoinRequest {
            id: self.rooms.identity.id.clone(),
            display_name: self.rooms.identity.display_name.clone(),
            kind: RoomParticipantKind::Human,
        };
        self.rooms.status = "joining...".to_string();
        let (sender, receiver) = mpsc::channel();
        let mutate_key = key.clone();
        thread::spawn(move || {
            let result =
                DaemonClient::new().and_then(|client| client.join_room(&url, &mutate_key, &request));
            let _ = sender.send(RoomsMessage::Mutated {
                key: mutate_key,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Leave the open room (`DELETE .../participants/{id}`).
    fn leave_open_room(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.rooms.open_key.clone() else {
            return;
        };
        let url = self.daemon.url.clone();
        let participant_id = self.rooms.identity.id.clone();
        self.rooms.status = "leaving...".to_string();
        let (sender, receiver) = mpsc::channel();
        let mutate_key = key.clone();
        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.leave_room(&url, &mutate_key, &participant_id));
            let _ = sender.send(RoomsMessage::Mutated {
                key: mutate_key,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Add an **agent** participant to the open room from the add-agent drafts
    /// (`POST .../participants` with `kind = agent`). Once present, the agent's id
    /// is mentionable (`@id`) and — if the room's trigger policy has `on_mention`
    /// — auto-convenes when mentioned (OCEAN-119 / OCEAN-111). The daemon's join
    /// route accepts the `kind` field directly, so this needs no daemon change.
    fn add_agent_from_draft(&mut self, cx: &mut Context<Self>) {
        let agent_id = self.rooms.agent_id_draft.trim().to_string();
        if agent_id.is_empty() {
            self.rooms.status = "agent id required".to_string();
            cx.notify();
            return;
        }
        let Some(key) = self.rooms.open_key.clone() else {
            return;
        };
        let display_name = {
            let trimmed = self.rooms.agent_name_draft.trim();
            if trimmed.is_empty() {
                agent_id.clone()
            } else {
                trimmed.to_string()
            }
        };
        self.rooms.agent_id_draft.clear();
        self.rooms.agent_name_draft.clear();
        self.rooms.focus = RoomFocus::Composer;
        self.rooms.status = format!("adding agent '{agent_id}'...");

        let url = self.daemon.url.clone();
        let request = RoomJoinRequest {
            id: agent_id.clone(),
            display_name,
            kind: RoomParticipantKind::Agent,
        };
        let (sender, receiver) = mpsc::channel();
        let mutate_key = key.clone();
        let status_id = agent_id.clone();
        thread::spawn(move || {
            let result =
                DaemonClient::new().and_then(|client| client.join_room(&url, &mutate_key, &request));
            let _ = sender.send(RoomsMessage::AgentAdded {
                key: mutate_key,
                agent_id: status_id,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Post the composer draft to the open room (`POST .../messages`). `@id`
    /// mentions in the body drive the daemon's auto-convene trigger policy.
    fn post_room_message(&mut self, cx: &mut Context<Self>) {
        let body = self.rooms.composer_draft.trim().to_string();
        if body.is_empty() {
            return;
        }
        let Some(key) = self.rooms.open_key.clone() else {
            return;
        };
        self.rooms.composer_draft.clear();

        let url = self.daemon.url.clone();
        let request = RoomPostMessageRequest {
            author_id: self.rooms.identity.id.clone(),
            author_kind: RoomParticipantKind::Human,
            body,
        };
        let (sender, receiver) = mpsc::channel();
        let post_key = key.clone();
        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.post_room_message(&url, &post_key, &request));
            let _ = sender.send(RoomsMessage::Posted {
                key: post_key,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Re-tail the open room's transcript after our highest held seq, appending
    /// only new entries. Used after our own writes and by the poll loop.
    fn refresh_room_transcript(&mut self, key: String, cx: &mut Context<Self>) {
        // Only tail if this is still the open room.
        if self.rooms.open_key.as_deref() != Some(key.as_str()) {
            return;
        }
        let url = self.daemon.url.clone();
        let after_seq = self.rooms.highest_seq();
        let (sender, receiver) = mpsc::channel();
        let tail_key = key.clone();
        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.fetch_room_transcript(&url, &tail_key, after_seq));
            let _ = sender.send(RoomsMessage::TranscriptTail {
                key: tail_key,
                result,
            });
        });
        self.rooms_task = Some(spawn_rooms_task(receiver, cx));
    }

    /// Start (or replace) the live transcript-tail poll loop for `key` at
    /// `generation`. The loop retires itself once the rooms generation advances
    /// (room change / panel close) — so a stale poll never writes into the wrong
    /// room. This is the GPUI analogue of the web surface's `start_live_tail`
    /// transcript poll (OCEAN-108); `room_trigger` is unscoped and so can't be
    /// relied on over the shell's session-scoped streams.
    fn start_room_transcript_poll(
        &mut self,
        key: String,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.rooms_poll_task = Some(cx.spawn(async move |shell, cx| {
            loop {
                Timer::after(ROOM_TRANSCRIPT_POLL_INTERVAL).await;
                let still_active = shell
                    .update(cx, |shell, cx| {
                        if shell.rooms.generation != generation
                            || shell.rooms.open_key.as_deref() != Some(key.as_str())
                        {
                            return false;
                        }
                        shell.refresh_room_transcript(key.clone(), cx);
                        true
                    })
                    .unwrap_or(false);
                if !still_active {
                    return;
                }
            }
        }));
    }

    fn apply_rooms_message(&mut self, message: RoomsMessage, cx: &mut Context<Self>) {
        match message {
            RoomsMessage::Listed(Ok(response)) if response.ok => {
                self.rooms.set_list(response.rooms);
            }
            RoomsMessage::Listed(Ok(response)) => {
                self.rooms.status = format!(
                    "rooms list failed: {}",
                    response.error.unwrap_or_else(|| "unknown error".to_string())
                );
            }
            RoomsMessage::Listed(Err(error)) => {
                self.rooms.status = format!("rooms fetch error: {error}");
            }
            RoomsMessage::Created { key, result } => match result {
                Ok(response) if response.ok => {
                    self.rooms.status = "room created".to_string();
                    self.refresh_rooms(cx);
                    self.open_room(key, cx);
                }
                Ok(response) => {
                    self.rooms.status = format!(
                        "create failed: {}",
                        response.error.unwrap_or_else(|| "unknown error".to_string())
                    );
                }
                Err(error) => self.rooms.status = format!("create error: {error}"),
            },
            RoomsMessage::Loaded {
                generation,
                key,
                result,
            } => match result {
                Ok(response) if response.ok => {
                    if self
                        .rooms
                        .apply_loaded(generation, response.room, response.transcript)
                    {
                        // Stale loads are dropped inside apply_loaded; on a fresh
                        // landing make sure the poll is anchored to this room.
                        self.start_room_transcript_poll(key, generation, cx);
                    }
                }
                Ok(response) => {
                    self.rooms.status = format!(
                        "room load failed: {}",
                        response.error.unwrap_or_else(|| "unknown error".to_string())
                    );
                }
                Err(error) => self.rooms.status = format!("room load error: {error}"),
            },
            RoomsMessage::Mutated { key, result } => match result {
                Ok(response) if response.ok => {
                    self.rooms.set_open_room(response.room);
                    self.rooms.status.clear();
                    self.refresh_room_transcript(key, cx);
                    self.refresh_rooms(cx);
                }
                Ok(response) => {
                    self.rooms.status = format!(
                        "room update failed: {}",
                        response.error.unwrap_or_else(|| "unknown error".to_string())
                    );
                }
                Err(error) => self.rooms.status = format!("room update error: {error}"),
            },
            RoomsMessage::AgentAdded {
                key,
                agent_id,
                result,
            } => match result {
                Ok(response) if response.ok => {
                    self.rooms.set_open_room(response.room);
                    self.rooms.status = format!("agent '{agent_id}' added — mention @{agent_id}");
                    self.refresh_room_transcript(key, cx);
                    self.refresh_rooms(cx);
                }
                Ok(response) => {
                    self.rooms.status = format!(
                        "add agent failed: {}",
                        response.error.unwrap_or_else(|| "unknown error".to_string())
                    );
                }
                Err(error) => self.rooms.status = format!("add agent error: {error}"),
            },
            RoomsMessage::Posted { key, result } => match result {
                Ok(response) if response.ok => {
                    // The daemon may append a System line on auto-convene; re-tail
                    // to pick up our message + any trigger notice.
                    self.refresh_room_transcript(key, cx);
                }
                Ok(response) => {
                    self.rooms.status = format!(
                        "message failed: {}",
                        response.error.unwrap_or_else(|| "unknown error".to_string())
                    );
                }
                Err(error) => self.rooms.status = format!("message error: {error}"),
            },
            RoomsMessage::TranscriptTail { key, result } => match result {
                Ok(response) if response.ok => {
                    self.rooms.append_transcript_tail(&key, response.transcript);
                }
                Ok(_) => {}
                Err(error) => self.rooms.status = format!("transcript error: {error}"),
            },
        }
    }

    fn send_component_event(
        &mut self,
        session_id: String,
        component_id: String,
        event: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        self.agent.status = "sending component event".to_string();
        let url = self.daemon.url.clone();
        let request = ComponentEventRequest {
            session_id,
            component_id,
            event,
        };
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let result =
                DaemonClient::new().and_then(|client| client.send_component_event(&url, &request));
            let _ = sender.send(AgentControlMessage::ComponentEventSent(result));
        });

        self.agent_control_task = Some(spawn_agent_control_task(receiver, cx));
    }

    fn apply_agent_control_message(
        &mut self,
        message: AgentControlMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            AgentControlMessage::Cancelled(Ok(response)) => {
                self.agent.status = response.message;
            }
            AgentControlMessage::Cancelled(Err(error)) => {
                self.agent.status = format!("cancel request error: {error}");
            }
            AgentControlMessage::PermissionDecided(Ok(response)) => {
                self.pending_permissions
                    .retain(|permission| permission.permission_id != response.permission_id);
                self.agent.status = response.message;
                self.refresh_agent_permissions(cx);
            }
            AgentControlMessage::PermissionDecided(Err(error)) => {
                self.agent.status = format!("permission decision error: {error}");
            }
            AgentControlMessage::ComponentEventSent(Ok(response)) => {
                self.agent.status = response
                    .status
                    .unwrap_or_else(|| "component event sent".into());
            }
            AgentControlMessage::ComponentEventSent(Err(error)) => {
                self.agent.status = format!("component event error: {error}");
            }
        }
    }

    fn start_new_agent_session(&mut self, cx: &mut Context<Self>) {
        self.agent = AgentState::default();
        self.pending_permissions.clear();
        self.gui_control = GuiControlState::default();
        self.gui_control.apply(GuiCommand::SetStatus {
            text: "new session".to_string(),
        });
        self.model_picker_open = false;
        self.session_picker_open = false;
        self.agent.status = "new session".to_string();
        self.agent_scroll.scroll_to_top_of_item(0);
        self.connect_agent_events(cx);
    }

    fn switch_agent_session(
        &mut self,
        session_id: String,
        session_title: String,
        cx: &mut Context<Self>,
    ) {
        self.model_picker_open = false;
        self.session_picker_open = false;
        self.gui_control = GuiControlState::default();
        self.gui_control.apply(GuiCommand::SwitchSession {
            session_id: session_id.clone(),
        });
        self.gui_control.apply(GuiCommand::SetStatus {
            text: "loading session".to_string(),
        });
        self.agent.turns.clear();
        self.pending_permissions.clear();
        self.agent.session_id = Some(session_id.clone());
        self.agent.session_title = session_title;
        self.agent.active_turn_id = None;
        self.agent.streaming = false;
        self.agent.last_turn_tokens = None;
        self.agent.session_tokens = Default::default();
        self.agent.status = "loading session".to_string();
        self.agent_scroll.scroll_to_top_of_item(0);
        self.connect_agent_events(cx);

        let url = self.daemon.url.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = DaemonClient::new()
                .and_then(|client| client.fetch_session(&url, &session_id))
                .and_then(|response| {
                    response
                        .session
                        .ok_or_else(|| "session snapshot missing".to_string())
                });
            let _ = sender.send(AgentSessionLoadMessage::Loaded { session_id, result });
        });

        self.agent_session_load_task = Some(spawn_agent_session_load_task(receiver, cx));
    }

    fn apply_agent_session_load_message(&mut self, message: AgentSessionLoadMessage) {
        match message {
            AgentSessionLoadMessage::Loaded { session_id, result } => {
                if self.agent.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(detail) => {
                        self.agent.session_title = detail.title.clone();
                        if !detail.model.is_empty() {
                            self.agent.model = Some(detail.model);
                        }
                        self.agent.turns = turns_from_session_transcript(detail.transcript);
                        self.agent.status = "session loaded".to_string();
                    }
                    Err(error) => {
                        self.agent.status = format!("session load error: {error}");
                    }
                }
            }
        }
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
            SurfaceTab::Surface => {
                let context = self.surface.turn_context();
                format!(
                    "surface {}  panes {}  canvases {}  active {}",
                    context.session_id,
                    context.panes.len(),
                    context.canvases.len(),
                    context.active_pane_id
                )
            }
            SurfaceTab::Agent => format!(
                "daemon {}  backend {}  session {}  region {}  components {}",
                self.daemon.status_label(),
                self.daemon.backend_label(),
                self.agent.session_id.as_deref().unwrap_or("new"),
                self.gui_control.active_region_label(),
                self.gui_control
                    .component_count_in_region(&RegionId::from(REGION_CHAT_INLINE))
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
            SurfaceTab::Surface => format!(
                "canvas {}",
                self.active_surface_canvas_id()
                    .unwrap_or_else(|| "none".to_string())
            ),
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

    fn toolbar_icon_button(
        &self,
        id: &'static str,
        icon: ShellIcon,
        tooltip: &'static str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut OceanGuiShell, &mut Context<OceanGuiShell>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .w(px(26.0))
            .h(px(26.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::frame())
            .border_1()
            .border_color(theme::frame())
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()).border_color(theme::rule()))
            .tooltip(move |_, cx| cx.new(|_| ToolbarTooltip { label: tooltip }).into())
            .on_click(cx.listener(move |shell, _, _, cx| handler(shell, cx)))
            .child(self.icon(icon, theme::muted(), 14.0))
    }

    fn agent_toolbar_picker_button(
        &self,
        id: &'static str,
        label: &str,
        open: bool,
        tooltip: &'static str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut OceanGuiShell, &mut Context<OceanGuiShell>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(26.0))
            .max_w(px(180.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(if open { theme::paper() } else { theme::frame() })
            .border_1()
            .border_color(if open {
                theme::rule_strong()
            } else {
                theme::rule().opacity(0.38)
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()).border_color(theme::rule()))
            .tooltip(move |_, cx| cx.new(|_| ToolbarTooltip { label: tooltip }).into())
            .on_click(cx.listener(move |shell, _, _, cx| handler(shell, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(if open {
                        theme::accent_dark()
                    } else {
                        theme::ink()
                    })
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.to_string()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(if open { "v" } else { ">" }),
            )
    }

    fn picker_row(
        &self,
        id: impl Into<ElementId>,
        selected: bool,
        title: impl Into<String>,
        detail: impl Into<String>,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut OceanGuiShell, &mut Context<OceanGuiShell>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .min_h(px(30.0))
            .px_3()
            .bg(if selected {
                theme::frame()
            } else {
                theme::paper()
            })
            .border_b(px(1.0))
            .border_color(theme::rule().opacity(0.32))
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_raised()))
            .on_click(cx.listener(move |shell, _, _, cx| handler(shell, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
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
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title.into()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .whitespace_nowrap()
                    .child(detail.into()),
            )
    }

    fn picker_action_row(
        &self,
        title: &str,
        detail: &str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut OceanGuiShell, &mut Context<OceanGuiShell>) + 'static,
    ) -> impl IntoElement {
        self.picker_row("picker-action", false, title, detail, cx, handler)
    }

    fn picker_placeholder_row(&self, label: &str) -> Div {
        div()
            .min_h(px(34.0))
            .px_3()
            .flex()
            .items_center()
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::muted())
            .child(label.to_string())
    }

    fn gui_control_event_label(&self) -> String {
        match self.gui_control.last_event() {
            Some(GuiControlEvent::Focused { region }) => format!("focus {}", region.as_str()),
            Some(GuiControlEvent::SessionOpened { .. }) => "session open".to_string(),
            Some(GuiControlEvent::SessionSwitched { .. }) => "session switch".to_string(),
            Some(GuiControlEvent::RoomSwitched { room_id }) => {
                format!("room {}", room_id.as_str())
            }
            Some(GuiControlEvent::ComponentMounted { component_id, .. }) => {
                format!("mount {}", component_id.as_str())
            }
            Some(GuiControlEvent::ComponentUpdated { component_id, .. }) => {
                format!("update {}", component_id.as_str())
            }
            Some(GuiControlEvent::ComponentUnmounted { component_id }) => {
                format!("unmount {}", component_id.as_str())
            }
            Some(GuiControlEvent::CanvasPatched { canvas_id, .. }) => {
                format!("canvas {}", canvas_id.as_str())
            }
            Some(GuiControlEvent::StatusChanged { text }) => text.clone(),
            Some(GuiControlEvent::Rejected { reason }) => format!("reject {reason}"),
            None => self.gui_control.status().to_string(),
        }
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

struct SurfaceCanvasHostElement {
    shell: Entity<OceanGuiShell>,
}

impl IntoElement for SurfaceCanvasHostElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SurfaceCanvasHostElement {
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
            shell.sync_surface_canvas_host(bounds, window);
        });
    }
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
        if self.active_surface != SurfaceTab::Surface {
            self.hide_surface_canvas_host(window);
        } else {
            self.drain_surface_canvas_ipc();
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

struct ToolbarTooltip {
    label: &'static str,
}

impl Render for ToolbarTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(theme::paper())
            .border_1()
            .border_color(theme::rule())
            .font_family(theme::MONO_FONT)
            .text_xs()
            .text_color(theme::ink())
            .child(self.label)
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

fn compact_text_stat(text: &str) -> String {
    match text.chars().count() {
        0 => "waiting".to_string(),
        1 => "1 char".to_string(),
        count => format!("{count} chars"),
    }
}

/// Compact label for the rooms toolbar toggle button. Shows the open room's
/// name when one is open, else a room count.
fn rooms_toolbar_label(rooms: &RoomsState) -> String {
    if let Some(room) = rooms.open_room.as_ref() {
        format!("Room: {}", room.name)
    } else if rooms.open_key.is_some() {
        "Room".to_string()
    } else {
        match rooms.list.len() {
            0 => "Rooms".to_string(),
            count => format!("Rooms ({count})"),
        }
    }
}

fn permission_summary_label(permission: Option<&PermissionStatus>) -> String {
    let Some(permission) = permission else {
        return "none".to_string();
    };

    if permission.reason.trim().is_empty() {
        permission.tool.clone()
    } else {
        format!("{} · {}", permission.tool, permission.reason)
    }
}

/// Render a permission request's args JSON into a compact, single-line summary
/// for the approval banner. Objects render as `key: value` pairs; everything
/// else is shown inline. Mirrors the web surface's `summarize_args` (OCEAN-64),
/// kept short so the card stays scannable.
fn permission_args_summary(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| {
                let rendered = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                format!("{key}: {rendered}")
            })
            .collect::<Vec<_>>()
            .join(" · "),
        other => other.to_string(),
    }
}

fn tool_call_summary(args_preview: &str, output: &str, status: ToolStatus) -> String {
    let output_stat = if output.is_empty() {
        match status {
            ToolStatus::Running => "waiting".to_string(),
            ToolStatus::Ok => "no output".to_string(),
            ToolStatus::Err => "error output pending".to_string(),
        }
    } else {
        compact_text_stat(output)
    };

    if args_preview.trim().is_empty() || args_preview == "{}" {
        output_stat
    } else {
        format!("{args_preview} · {output_stat}")
    }
}

fn component_text(component: &LedgerComponent) -> String {
    component
        .content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&component.component_type)
        .to_string()
}

/// Which surface the canvas pane paints (OCEAN-156). The native Ocean canvas is
/// the default agent-render target; the legacy tldraw projection is only shown
/// when the operator toggles into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRenderTarget {
    /// The native [`OceanCanvasView`] entity, drawing the native [`CanvasLedger`].
    Native,
    /// The legacy tldraw webview projection + [`SurfaceLedger`] markers.
    Tldraw,
}

/// Pure render-target selector for the surface pane. Native unless the tldraw
/// toggle is set. Kept window-free so the §9 / Gate D default ("native canvas,
/// not the legacy SurfaceLedger markers") is assertable headlessly.
fn surface_render_target(use_tldraw: bool) -> SurfaceRenderTarget {
    if use_tldraw {
        SurfaceRenderTarget::Tldraw
    } else {
        SurfaceRenderTarget::Native
    }
}

/// Apply a batch of daemon [`SurfacePatchEnvelope`]s to the native
/// [`CanvasLedger`], window-free.
///
/// Reuses `existing` when it already targets `canvas_id`; otherwise starts a
/// fresh ledger keyed on the event's canvas/session (CanvasMode defaults to
/// freeform — Slice 7 injects mode from the prompt contract). Each envelope's
/// daemon-stamped actor/timestamp is preserved so attribution survives the wire.
///
/// Returns `None` for an empty batch (nothing to apply, no repaint), or the
/// mutated ledger to write back to the shared cell. Splitting this out of
/// [`OceanGuiShell::apply_surface_patch_event`] lets the §16 hot path
/// (patch -> ledger component) be tested without a window.
fn apply_patches_to_ledger(
    existing: Option<CanvasLedger>,
    session_id: String,
    canvas_id: CanvasId,
    patches: Vec<SurfacePatchEnvelope>,
) -> Option<CanvasLedger> {
    if patches.is_empty() {
        return None;
    }

    let mut ledger = match existing {
        Some(ledger) if ledger.canvas_id == canvas_id => ledger,
        _ => CanvasLedger::new(canvas_id, session_id, CanvasMode::default()),
    };

    for envelope in patches {
        let actor: CanvasActorRef = envelope.actor;
        ledger.apply_patch(envelope.patch, actor, envelope.created_at_ms);
    }

    Some(ledger)
}

fn canvas_web_index_path() -> Option<PathBuf> {
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(contents_dir) = exe_path.parent().and_then(|macos_dir| macos_dir.parent())
    {
        let bundled_root = contents_dir.join("Resources").join("canvas-web");
        for file_name in ["inline.html", "index.html"] {
            let bundled = bundled_root.join(file_name);
            if bundled.exists() {
                return Some(bundled);
            }
        }
    }

    let dev_dist_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("canvas-web")
        .join("dist");
    for file_name in ["inline.html", "index.html"] {
        let dev_dist = dev_dist_root.join(file_name);
        if dev_dist.exists() {
            return Some(dev_dist);
        }
    }

    None
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
                    shell.apply_agent_stream_messages(messages, cx);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
        }
    })
}

fn spawn_agent_control_stream_task(
    receiver: Receiver<AgentControlStreamMessage>,
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
                    shell.apply_agent_control_stream_messages(messages);
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
                        shell.apply_agent_submit_message(message, cx);
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

fn spawn_agent_models_task(
    receiver: Receiver<AgentModelsMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_models_message(message);
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

fn spawn_agent_projects_task(
    receiver: Receiver<AgentProjectsMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_projects_message(message);
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

/// Wrap a packed BGRA frame buffer in a `gpui::RenderImage`.
///
/// GPUI stores `RenderImage` frames in BGRA byte order (its own decoder swaps
/// R<->B before constructing one), and the LiveKit decode path already produces
/// BGRA (see `surface_livekit_video`), so the bytes are handed through verbatim.
/// The `image::RgbaImage` is just the byte container; the channel *labels* are
/// irrelevant to the renderer, only the B,G,R,A byte order matters.
fn render_image_from_bgra(width: u32, height: u32, bgra: &[u8]) -> Arc<RenderImage> {
    let buffer = RgbaImage::from_raw(width, height, bgra.to_vec())
        .unwrap_or_else(|| RgbaImage::new(width.max(1), height.max(1)));
    Arc::new(RenderImage::new(vec![Frame::new(buffer)]))
}

fn spawn_surface_livekit_task(
    receiver: Receiver<SurfaceLiveKitMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_surface_livekit_message(message, cx);
                        cx.notify();
                    });
                }
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    })
}

fn spawn_agent_sessions_task(
    receiver: Receiver<AgentSessionsMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_sessions_message(message);
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

fn spawn_agent_session_load_task(
    receiver: Receiver<AgentSessionLoadMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_session_load_message(message);
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

fn spawn_agent_permissions_task(
    receiver: Receiver<AgentPermissionsMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_permissions_message(message);
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

fn spawn_rooms_task(
    receiver: Receiver<RoomsMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_rooms_message(message, cx);
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

fn spawn_agent_control_task(
    receiver: Receiver<AgentControlMessage>,
    cx: &mut Context<OceanGuiShell>,
) -> Task<()> {
    cx.spawn(async move |shell, cx| {
        loop {
            Timer::after(AGENT_EVENT_POLL_INTERVAL).await;
            match receiver.try_recv() {
                Ok(message) => {
                    let _ = shell.update(cx, |shell, cx| {
                        shell.apply_agent_control_message(message, cx);
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

fn current_model_toolbar_label(current: &Option<String>, models: &[ModelInfo]) -> String {
    let Some(current) = current.as_deref() else {
        return "pending".to_string();
    };

    models
        .iter()
        .find(|model| model.id == current)
        .map(|model| {
            if model.label.is_empty() {
                model.id.clone()
            } else {
                model.label.clone()
            }
        })
        .unwrap_or_else(|| current.to_string())
}

fn current_project_toolbar_label(current: &Option<String>, projects: &[ProjectInfo]) -> String {
    let Some(current) = current.as_deref() else {
        return "no project".to_string();
    };
    projects
        .iter()
        .find(|p| p.id == current)
        .map(|p| {
            if p.name.is_empty() {
                p.id.clone()
            } else {
                p.name.clone()
            }
        })
        .unwrap_or_else(|| current.to_string())
}

fn current_session_toolbar_label(agent: &AgentState) -> String {
    if !agent.session_title.trim().is_empty() {
        return agent.session_title.clone();
    }

    agent
        .session_id
        .as_deref()
        .map(short_session_label)
        .unwrap_or_else(|| "new session".to_string())
}

fn session_title_hint(prompt: &str) -> Option<String> {
    let title = prompt.trim().chars().take(60).collect::<String>();
    (!title.is_empty()).then_some(title)
}

fn short_session_label(session_id: &str) -> String {
    let short = session_id.chars().take(8).collect::<String>();
    format!("session {short}")
}

fn compact_session_title(session: &SessionSummary) -> String {
    if !session.title.trim().is_empty() {
        session.title.clone()
    } else {
        short_session_label(&session.id)
    }
}

fn gui_command_for_agent_event(
    event: &AgentEvent,
    component_already_mounted: bool,
) -> Option<GuiCommand> {
    match event {
        AgentEvent::SessionCreated { session_id, .. } => Some(GuiCommand::OpenSession {
            session_id: session_id.clone(),
        }),
        AgentEvent::TurnStarted { session_id, .. } => Some(GuiCommand::SwitchSession {
            session_id: session_id.clone(),
        }),
        AgentEvent::ComponentRender {
            component_id,
            kind,
            props,
            replace,
            ..
        } => Some(GuiCommand::MountComponent {
            region: RegionId::from(REGION_CHAT_INLINE),
            component_id: ComponentId::from(component_id.as_str()),
            kind: kind.clone(),
            props: props.clone(),
            replace: *replace || component_already_mounted,
        }),
        AgentEvent::ComponentUnmount { component_id, .. } => Some(GuiCommand::UnmountComponent {
            component_id: ComponentId::from(component_id.as_str()),
        }),
        // Surface patches drive the native canvas ledger directly (Slice 6),
        // not the gui_control component tree — no GuiCommand to emit.
        AgentEvent::SurfacePatch { .. }
        | AgentEvent::Other
        | AgentEvent::Extension { .. }
        | AgentEvent::BrowserActivity { .. }
        | AgentEvent::AssistantTextDelta { .. }
        | AgentEvent::ThinkingDelta { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallChunk { .. }
        | AgentEvent::ToolCallFinished { .. }
        | AgentEvent::TurnFinished { .. } => None,
    }
}

/// Build a tldraw ledger component `content` payload from an agent component
/// render's `props`. The canvas-web bridge reads `content.text` for the card
/// label, so we surface a human-readable string there while preserving the full
/// agent props under `content.props` and the original `kind` so nothing is lost
/// on round-trip (OCEAN-78).
fn ledger_content_from_props(kind: &str, props: &serde_json::Value) -> serde_json::Value {
    let text = render_props_text(props).unwrap_or_else(|| kind.to_string());
    serde_json::json!({
        "text": text,
        "kind": kind,
        "props": props.clone(),
    })
}

/// Pull a readable label out of agent component props, checking the common
/// content-bearing keys in priority order before falling back to a scalar
/// value or `None`.
fn render_props_text(props: &serde_json::Value) -> Option<String> {
    if let Some(object) = props.as_object() {
        for key in [
            "text", "markdown", "title", "label", "body", "content", "heading", "summary",
        ] {
            if let Some(text) = object.get(key).and_then(serde_json::Value::as_str) {
                if !text.trim().is_empty() {
                    return Some(text.to_string());
                }
            }
        }
        return None;
    }

    match props {
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn turns_from_session_transcript(
    entries: Vec<super::daemon::SessionTranscriptEntry>,
) -> Vec<AgentTurn> {
    let mut turns = Vec::new();

    for entry in entries {
        if entry.text.trim().is_empty() && entry.tool_name.is_none() {
            continue;
        }

        match entry.role.as_str() {
            "user" => turns.push(AgentTurn::user(entry.text)),
            "assistant" => {
                let mut turn = AgentTurn::assistant(format!("snapshot-{}", turns.len()));
                if entry.is_error.unwrap_or(false) {
                    turn.blocks.push(AgentBlock::ToolCall {
                        call_id: format!("snapshot-error-{}", turns.len()),
                        name: "assistant_error".to_string(),
                        args_preview: String::new(),
                        output: entry.text,
                        status: ToolStatus::Err,
                        expanded: true,
                    });
                } else {
                    turn.blocks.push(AgentBlock::Text(entry.text));
                }
                turns.push(turn);
            }
            "tool" => {
                let mut turn = AgentTurn::assistant(format!("snapshot-tool-{}", turns.len()));
                turn.blocks.push(AgentBlock::ToolCall {
                    call_id: format!("snapshot-tool-{}", turns.len()),
                    name: entry.tool_name.unwrap_or_else(|| "tool".to_string()),
                    args_preview: String::new(),
                    output: entry.text,
                    status: if entry.is_error.unwrap_or(false) {
                        ToolStatus::Err
                    } else {
                        ToolStatus::Ok
                    },
                    expanded: false,
                });
                turns.push(turn);
            }
            _ => {}
        }
    }

    turns
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
    fn compact_stats_summarize_hidden_agent_blocks() {
        assert_eq!(compact_text_stat(""), "waiting");
        assert_eq!(compact_text_stat("a"), "1 char");
        assert_eq!(compact_text_stat("abc"), "3 chars");
        assert_eq!(
            tool_call_summary("{\"cmd\":\"pwd\"}", "/repo", ToolStatus::Ok),
            "{\"cmd\":\"pwd\"} · 5 chars"
        );
        assert_eq!(tool_call_summary("{}", "", ToolStatus::Running), "waiting");
    }

    #[test]
    fn toolbar_labels_prefer_catalogue_and_titles() {
        let mut agent = AgentState::default();
        agent.model = Some("gpt-5.5".to_string());
        agent.session_title = "Daily Room".to_string();

        assert_eq!(
            current_model_toolbar_label(
                &agent.model,
                &[ModelInfo {
                    id: "gpt-5.5".to_string(),
                    provider: "openai-codex".to_string(),
                    label: "GPT-5.5 (Codex)".to_string(),
                }]
            ),
            "GPT-5.5 (Codex)"
        );
        assert_eq!(current_session_toolbar_label(&agent), "Daily Room");
    }

    #[test]
    fn session_snapshot_transcript_becomes_agent_turns() {
        let turns = turns_from_session_transcript(vec![
            super::super::daemon::SessionTranscriptEntry {
                role: "user".to_string(),
                text: "hello".to_string(),
                tool_name: None,
                is_error: None,
            },
            super::super::daemon::SessionTranscriptEntry {
                role: "tool".to_string(),
                text: "/repo".to_string(),
                tool_name: Some("pwd".to_string()),
                is_error: Some(false),
            },
        ]);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, AgentRole::User);
        assert_eq!(turns[1].role, AgentRole::Assistant);
    }

    #[test]
    fn render_props_text_prefers_content_bearing_keys() {
        assert_eq!(
            render_props_text(&serde_json::json!({ "title": "Sales brief" })).as_deref(),
            Some("Sales brief")
        );
        // `text` wins over `title` when both are present.
        assert_eq!(
            render_props_text(&serde_json::json!({ "title": "T", "text": "Body" })).as_deref(),
            Some("Body")
        );
        // Blank strings are skipped in favor of the next candidate key.
        assert_eq!(
            render_props_text(&serde_json::json!({ "text": "  ", "label": "Fallback" })).as_deref(),
            Some("Fallback")
        );
        // A bare string payload is used directly.
        assert_eq!(
            render_props_text(&serde_json::json!("just text")).as_deref(),
            Some("just text")
        );
        // Nothing readable yields None.
        assert_eq!(render_props_text(&serde_json::json!({ "count": 3 })), None);
        assert_eq!(render_props_text(&serde_json::Value::Null), None);
    }

    #[test]
    fn ledger_content_from_props_round_trips_kind_and_props() {
        let content = ledger_content_from_props(
            "markdown_card",
            &serde_json::json!({ "title": "Campaign plan", "rows": [1, 2] }),
        );

        assert_eq!(content["text"], serde_json::json!("Campaign plan"));
        assert_eq!(content["kind"], serde_json::json!("markdown_card"));
        assert_eq!(
            content["props"],
            serde_json::json!({ "title": "Campaign plan", "rows": [1, 2] })
        );
    }

    #[test]
    fn ledger_content_falls_back_to_kind_when_props_have_no_text() {
        let content = ledger_content_from_props("status_badge", &serde_json::json!({ "n": 1 }));
        assert_eq!(content["text"], serde_json::json!("status_badge"));
    }

    #[test]
    fn component_render_event_becomes_gui_mount_command() {
        let command = gui_command_for_agent_event(
            &AgentEvent::ComponentRender {
                session_id: "s1".to_string(),
                component_id: "approval-1".to_string(),
                kind: "confirm".to_string(),
                props: serde_json::json!({ "title": "Restart daemon" }),
                replace: false,
            },
            false,
        )
        .expect("component render should map to a gui command");

        assert_eq!(
            command,
            super::super::gui_control::GuiCommand::MountComponent {
                region: super::super::gui_control::RegionId::from(
                    super::super::gui_control::REGION_CHAT_INLINE
                ),
                component_id: super::super::gui_control::ComponentId::from("approval-1"),
                kind: "confirm".to_string(),
                props: serde_json::json!({ "title": "Restart daemon" }),
                replace: false,
            }
        );
    }

    #[test]
    fn permission_summary_label_compacts_latest_permission() {
        let permission = super::super::daemon::PermissionStatus {
            permission_id: "perm-1".to_string(),
            request_id: "req-1".to_string(),
            session_id: Some("session-12345678".to_string()),
            tool: "bash".to_string(),
            reason: "permission required for bash".to_string(),
            args: serde_json::json!({ "cmd": "cargo check" }),
            created_at: "2026-06-03T00:00:00Z".to_string(),
        };

        assert_eq!(
            permission_summary_label(Some(&permission)),
            "bash · permission required for bash"
        );
        assert_eq!(permission_summary_label(None), "none");
    }

    #[test]
    fn permission_args_summary_renders_object_as_inline_pairs() {
        let summary = permission_args_summary(&serde_json::json!({ "cmd": "cargo check" }));
        assert_eq!(summary, "cmd: cargo check");
    }

    #[test]
    fn permission_args_summary_handles_null_and_scalars() {
        assert_eq!(permission_args_summary(&serde_json::Value::Null), "");
        assert_eq!(
            permission_args_summary(&serde_json::json!("rm -rf /tmp/x")),
            "\"rm -rf /tmp/x\""
        );
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

    // ---- OCEAN-156: native canvas mount + repaint-on-patch -----------------

    use super::super::canvas::{
        CanvasComponentPatch, ComponentId, PatchId, Rect, SurfaceId, SurfacePatch,
    };

    /// A `surface_patch` upsert envelope, as the daemon would emit it.
    fn upsert_envelope(id: &str, title: &str) -> SurfacePatchEnvelope {
        SurfacePatchEnvelope {
            patch_id: PatchId::new("p1"),
            session_id: "sess-1".to_string(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: CanvasId::new("canvas:main"),
            actor: CanvasActorRef::agent(Some("agent-1".to_string())),
            created_at_ms: 0,
            patch: SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new(id),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(420.0, 120.0, 320.0, 220.0)),
                    z_index: None,
                    content: serde_json::json!({ "title": title }),
                    metadata: serde_json::Value::Null,
                },
            },
        }
    }

    #[test]
    fn surface_pane_selects_native_canvas_by_default_not_tldraw() {
        // Default (no toggle) must paint the native OceanCanvasView / CanvasLedger
        // source, NOT the legacy SurfaceLedger markers (§9 / Gate D).
        assert_eq!(
            surface_render_target(false),
            SurfaceRenderTarget::Native,
            "default surface render target must be the native canvas"
        );
        // The legacy tldraw projection is still reachable behind the toggle.
        assert_eq!(surface_render_target(true), SurfaceRenderTarget::Tldraw);
    }

    #[test]
    fn applying_a_patch_yields_a_native_ledger_component() {
        // A patch event populates the native CanvasLedger (the source the native
        // pane renders), proving the §19 acceptance object ("a card appears on
        // the native canvas") lands in the ledger the mounted view draws.
        let ledger = apply_patches_to_ledger(
            None,
            "sess-1".to_string(),
            CanvasId::new("canvas:main"),
            vec![upsert_envelope("hello", "hello from the agent")],
        )
        .expect("non-empty patch batch yields a ledger");

        assert_eq!(ledger.canvas_id, CanvasId::new("canvas:main"));
        let component = ledger
            .component(&ComponentId::new("hello"))
            .expect("upserted component is present in the native ledger");
        assert_eq!(
            component.content.get("title").and_then(|v| v.as_str()),
            Some("hello from the agent")
        );
        // Revision advanced — the view's next frame sees a changed ledger.
        assert!(ledger.revision > 0, "apply_patch must bump the revision");
    }

    #[test]
    fn patch_reuses_active_ledger_for_same_canvas() {
        // A second patch to the same canvas must extend the existing ledger, not
        // reset it — so successive agent turns accumulate on one native surface.
        let first = apply_patches_to_ledger(
            None,
            "sess-1".to_string(),
            CanvasId::new("canvas:main"),
            vec![upsert_envelope("a", "first")],
        )
        .unwrap();

        let second = apply_patches_to_ledger(
            Some(first),
            "sess-1".to_string(),
            CanvasId::new("canvas:main"),
            vec![upsert_envelope("b", "second")],
        )
        .unwrap();

        assert!(second.component(&ComponentId::new("a")).is_some());
        assert!(second.component(&ComponentId::new("b")).is_some());
    }

    #[test]
    fn empty_patch_batch_is_a_noop_and_signals_no_repaint() {
        // An empty batch yields no ledger -> apply_surface_patch_event returns
        // early and never requests a canvas repaint (the §16 hot path only fires
        // on real mutations).
        assert!(apply_patches_to_ledger(
            None,
            "sess-1".to_string(),
            CanvasId::new("canvas:main"),
            Vec::new(),
        )
        .is_none());
    }
}
