use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_SURFACE_SESSION_ID: &str = "surface:main";
pub const DEFAULT_CANVAS_ID: &str = "canvas:main";
pub const DEFAULT_TLDRAW_ROOM_ID: &str = "ocean-surface-main";
pub const DEFAULT_LIVEKIT_ROOM_ID: &str = "project:surface-main";

const DEFAULT_COMPONENT_WIDTH: f32 = 240.0;
const DEFAULT_COMPONENT_HEIGHT: f32 = 160.0;
const SLOT_ORIGIN_X: f32 = 40.0;
const SLOT_ORIGIN_Y: f32 = 40.0;
const SLOT_GAP: f32 = 24.0;
const SLOT_SCAN_COLUMNS: usize = 6;
const SLOT_SCAN_ROWS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfacePaneKind {
    TldrawCanvas,
    AgentTranscript,
    Notes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMode {
    General,
    WorkflowBuilder,
    Storyboard,
    CampaignBoard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneDock {
    Full,
    Left,
    Right,
    Detached,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceState {
    session_id: String,
    active_pane_id: String,
    panes: Vec<SurfacePane>,
    canvases: BTreeMap<String, SurfaceLedger>,
    next_pane_ordinal: u32,
    next_canvas_ordinal: u32,
}

impl Default for SurfaceState {
    fn default() -> Self {
        let mut canvases = BTreeMap::new();
        canvases.insert(
            DEFAULT_CANVAS_ID.to_string(),
            SurfaceLedger::new(
                DEFAULT_CANVAS_ID,
                DEFAULT_TLDRAW_ROOM_ID,
                SurfaceMode::General,
            ),
        );

        Self {
            session_id: DEFAULT_SURFACE_SESSION_ID.to_string(),
            active_pane_id: "pane:1".to_string(),
            panes: vec![SurfacePane::tldraw_canvas(
                "pane:1",
                "Main Canvas",
                DEFAULT_CANVAS_ID,
                DEFAULT_TLDRAW_ROOM_ID,
                PaneDock::Full,
            )],
            canvases,
            next_pane_ordinal: 2,
            next_canvas_ordinal: 2,
        }
    }
}

impl SurfaceState {
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn active_pane_id(&self) -> &str {
        &self.active_pane_id
    }

    #[must_use]
    pub fn panes(&self) -> &[SurfacePane] {
        &self.panes
    }

    #[must_use]
    pub fn canvas(&self, canvas_id: &str) -> Option<&SurfaceLedger> {
        self.canvases.get(canvas_id)
    }

    pub fn open_canvas_pane(&mut self, title: impl Into<String>, mode: SurfaceMode) -> String {
        let pane_id = format!("pane:{}", self.next_pane_ordinal);
        self.next_pane_ordinal += 1;
        let canvas_id = format!("canvas:{}", self.next_canvas_ordinal);
        let tldraw_room_id = format!("ocean-surface-{}", self.next_canvas_ordinal);
        self.next_canvas_ordinal += 1;
        let title = title.into();

        self.canvases.insert(
            canvas_id.clone(),
            SurfaceLedger::new(&canvas_id, &tldraw_room_id, mode),
        );
        self.panes.push(SurfacePane::tldraw_canvas(
            pane_id.clone(),
            if title.trim().is_empty() {
                "Canvas".to_string()
            } else {
                title
            },
            canvas_id.clone(),
            tldraw_room_id,
            PaneDock::Right,
        ));
        self.active_pane_id = pane_id;
        canvas_id
    }

    pub fn set_active_pane(&mut self, pane_id: &str) -> bool {
        if !self
            .panes
            .iter()
            .any(|pane| pane.pane_id == pane_id && pane.attached)
        {
            return false;
        }

        self.active_pane_id = pane_id.to_string();
        true
    }

    #[must_use]
    pub fn active_canvas_id(&self) -> Option<&str> {
        self.panes
            .iter()
            .find(|pane| pane.pane_id == self.active_pane_id && pane.attached)
            .and_then(|pane| pane.canvas_id.as_deref())
    }

    pub fn detach_pane(&mut self, pane_id: &str) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.pane_id == pane_id) else {
            return false;
        };

        pane.attached = false;
        pane.dock = PaneDock::Detached;
        true
    }

    pub fn attach_pane(&mut self, pane_id: &str, dock: PaneDock) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.pane_id == pane_id) else {
            return false;
        };

        pane.attached = true;
        pane.dock = dock;
        true
    }

    pub fn upsert_component(&mut self, canvas_id: &str, component: LedgerComponent) -> bool {
        let Some(canvas) = self.canvases.get_mut(canvas_id) else {
            return false;
        };

        canvas.upsert_component(component);
        true
    }

    /// Remove a component from a canvas ledger by id. Returns `true` when a
    /// component was actually removed (so the caller can sync state outward).
    pub fn remove_component(&mut self, canvas_id: &str, component_id: &str) -> bool {
        let Some(canvas) = self.canvases.get_mut(canvas_id) else {
            return false;
        };

        if canvas.components.remove(component_id).is_some() {
            canvas.revision += 1;
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn next_slot(&self, canvas_id: &str, width: f32, height: f32) -> Option<ComponentRect> {
        let canvas = self.canvases.get(canvas_id)?;
        canvas.next_slot(width, height)
    }

    #[must_use]
    pub fn turn_context(&self) -> SurfaceTurnContext {
        let panes = self
            .panes
            .iter()
            .filter(|pane| pane.attached)
            .map(SurfacePaneContext::from)
            .collect::<Vec<_>>();
        let canvases = self
            .canvases
            .values()
            .map(SurfaceCanvasContext::from)
            .collect::<Vec<_>>();

        SurfaceTurnContext {
            session_id: self.session_id.clone(),
            active_pane_id: self.active_pane_id.clone(),
            panes,
            canvases,
        }
    }

    pub fn apply_ipc_event(&mut self, event: SurfaceIpcEvent) -> bool {
        match event {
            SurfaceIpcEvent::CanvasReady {
                pane_id,
                canvas_id,
                tldraw_room_id,
            } => {
                let Some(pane) = self.panes.iter_mut().find(|pane| pane.pane_id == pane_id) else {
                    return false;
                };
                pane.canvas_id = Some(canvas_id.clone());
                pane.tldraw_room_id = Some(tldraw_room_id.clone());
                self.canvases
                    .entry(canvas_id.clone())
                    .or_insert_with(|| {
                        SurfaceLedger::new(&canvas_id, &tldraw_room_id, SurfaceMode::General)
                    })
                    .tldraw_room_id = tldraw_room_id.clone();
                true
            }
            SurfaceIpcEvent::LedgerSnapshot {
                canvas_id,
                revision,
                components,
            } => {
                let canvas = self.canvases.entry(canvas_id.clone()).or_insert_with(|| {
                    SurfaceLedger::new(&canvas_id, &canvas_id, SurfaceMode::General)
                });
                canvas.revision = revision;
                canvas.components = components
                    .into_iter()
                    .map(|component| (component.id.clone(), component))
                    .collect();
                true
            }
            SurfaceIpcEvent::SelectionChanged {
                canvas_id,
                selected_ids,
            } => {
                let canvas = self.canvases.entry(canvas_id.clone()).or_insert_with(|| {
                    SurfaceLedger::new(&canvas_id, &canvas_id, SurfaceMode::General)
                });
                canvas.selection = selected_ids;
                canvas.revision += 1;
                true
            }
            SurfaceIpcEvent::CanvasError { .. } => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfacePane {
    pub pane_id: String,
    pub title: String,
    pub kind: SurfacePaneKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tldraw_room_id: Option<String>,
    pub attached: bool,
    pub dock: PaneDock,
}

impl SurfacePane {
    #[must_use]
    pub fn tldraw_canvas(
        pane_id: impl Into<String>,
        title: impl Into<String>,
        canvas_id: impl Into<String>,
        tldraw_room_id: impl Into<String>,
        dock: PaneDock,
    ) -> Self {
        Self {
            pane_id: pane_id.into(),
            title: title.into(),
            kind: SurfacePaneKind::TldrawCanvas,
            canvas_id: Some(canvas_id.into()),
            tldraw_room_id: Some(tldraw_room_id.into()),
            attached: true,
            dock,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLedger {
    pub canvas_id: String,
    pub tldraw_room_id: String,
    pub mode: SurfaceMode,
    pub revision: u64,
    pub components: BTreeMap<String, LedgerComponent>,
    pub selection: Vec<String>,
    pub metadata: Value,
}

impl SurfaceLedger {
    #[must_use]
    pub fn new(
        canvas_id: impl Into<String>,
        tldraw_room_id: impl Into<String>,
        mode: SurfaceMode,
    ) -> Self {
        Self {
            canvas_id: canvas_id.into(),
            tldraw_room_id: tldraw_room_id.into(),
            mode,
            revision: 0,
            components: BTreeMap::new(),
            selection: Vec::new(),
            metadata: json!({
                "grid_size": 24,
                "snap_to": "grid"
            }),
        }
    }

    pub fn upsert_component(&mut self, component: LedgerComponent) {
        self.components.insert(component.id.clone(), component);
        self.revision += 1;
    }

    #[must_use]
    pub fn next_slot(&self, width: f32, height: f32) -> Option<ComponentRect> {
        for row in 0..SLOT_SCAN_ROWS {
            for column in 0..SLOT_SCAN_COLUMNS {
                let x = SLOT_ORIGIN_X + column as f32 * (width + SLOT_GAP);
                let y = SLOT_ORIGIN_Y + row as f32 * (height + SLOT_GAP);
                let candidate = ComponentRect {
                    x,
                    y,
                    width,
                    height,
                };

                if self
                    .components
                    .values()
                    .all(|component| !component.rect().intersects(&candidate))
                {
                    return Some(candidate);
                }
            }
        }

        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ComponentRect {
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerComponent {
    pub id: String,
    pub component_type: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub content: Value,
    pub metadata: Value,
    pub connections: Vec<String>,
}

impl LedgerComponent {
    #[must_use]
    pub fn markdown_card(id: impl Into<String>, x: f32, y: f32, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            component_type: "markdown_card".to_string(),
            x,
            y,
            width: DEFAULT_COMPONENT_WIDTH,
            height: DEFAULT_COMPONENT_HEIGHT,
            content: json!({ "text": text.into() }),
            metadata: json!({}),
            connections: Vec::new(),
        }
    }

    #[must_use]
    pub fn rect(&self) -> ComponentRect {
        ComponentRect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceTurnContext {
    pub session_id: String,
    pub active_pane_id: String,
    pub panes: Vec<SurfacePaneContext>,
    pub canvases: Vec<SurfaceCanvasContext>,
}

// OCEAN-168 / Slice 9 — tldraw adapter demotion: the legacy SurfaceLedger
// prompt-injection path (`SurfaceTurnContext::to_prompt_injection`,
// `SURFACE_PROMPT_CONTRACT`, and `prompt_with_surface_context`) was removed.
// Turn-context now flows from the native CanvasLedger ONLY (see
// `view::build_submit_prompt` + `canvas::prompt_with_canvas_context`); shipping
// both blocks fed the agent two overlapping canvas descriptions. The
// `SurfaceTurnContext` type itself is retained — it still backs the LiveKit
// compact metadata and the tldraw projection pane — just not the prompt.

#[must_use]
pub fn canvas_web_url(
    index_path: &Path,
    session_id: &str,
    pane: &SurfacePane,
    sync_uri: Option<&str>,
) -> Option<String> {
    let canvas_id = pane.canvas_id.as_deref()?;
    let tldraw_room_id = pane.tldraw_room_id.as_deref()?;
    let mut url = file_url(index_path);
    url.push_str("?session_id=");
    url.push_str(&percent_encode_query_value(session_id));
    url.push_str("&pane_id=");
    url.push_str(&percent_encode_query_value(&pane.pane_id));
    url.push_str("&canvas_id=");
    url.push_str(&percent_encode_query_value(canvas_id));
    url.push_str("&tldraw_room_id=");
    url.push_str(&percent_encode_query_value(tldraw_room_id));
    if let Some(sync_uri) = sync_uri.filter(|value| !value.trim().is_empty()) {
        url.push_str("&sync_uri=");
        url.push_str(&percent_encode_query_value(sync_uri));
    }
    Some(url)
}

fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut url = String::from("file://");
    if !path.starts_with('/') {
        url.push('/');
    }

    for byte in path.bytes() {
        match byte {
            b'/' => url.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                url.push(byte as char);
            }
            _ => push_percent_encoded(&mut url, byte),
        }
    }

    url
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => push_percent_encoded(&mut encoded, byte),
        }
    }
    encoded
}

fn push_percent_encoded(output: &mut String, byte: u8) {
    use std::fmt::Write as _;
    write!(output, "%{byte:02X}").expect("writing to string should not fail");
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfacePaneContext {
    pub pane_id: String,
    pub title: String,
    pub kind: SurfacePaneKind,
    pub canvas_id: Option<String>,
    pub dock: PaneDock,
}

impl From<&SurfacePane> for SurfacePaneContext {
    fn from(pane: &SurfacePane) -> Self {
        Self {
            pane_id: pane.pane_id.clone(),
            title: pane.title.clone(),
            kind: pane.kind,
            canvas_id: pane.canvas_id.clone(),
            dock: pane.dock,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceCanvasContext {
    pub canvas_id: String,
    pub tldraw_room_id: String,
    pub mode: SurfaceMode,
    pub revision: u64,
    pub components: Vec<LedgerComponent>,
    pub selection: Vec<String>,
    pub metadata: Value,
}

impl From<&SurfaceLedger> for SurfaceCanvasContext {
    fn from(canvas: &SurfaceLedger) -> Self {
        Self {
            canvas_id: canvas.canvas_id.clone(),
            tldraw_room_id: canvas.tldraw_room_id.clone(),
            mode: canvas.mode,
            revision: canvas.revision,
            components: canvas.components.values().cloned().collect(),
            selection: canvas.selection.clone(),
            metadata: canvas.metadata.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceIpcEvent {
    CanvasReady {
        pane_id: String,
        canvas_id: String,
        tldraw_room_id: String,
    },
    LedgerSnapshot {
        canvas_id: String,
        revision: u64,
        components: Vec<LedgerComponent>,
    },
    SelectionChanged {
        canvas_id: String,
        selected_ids: Vec<String>,
    },
    CanvasError {
        pane_id: Option<String>,
        canvas_id: Option<String>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceIpcCommand {
    LoadCanvas {
        pane_id: String,
        canvas_id: String,
        tldraw_room_id: String,
    },
    UpsertComponent {
        canvas_id: String,
        component: LedgerComponent,
    },
    FocusComponent {
        canvas_id: String,
        component_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_surface_state_starts_with_one_tldraw_canvas_pane() {
        let state = SurfaceState::default();
        let context = state.turn_context();

        assert_eq!(context.session_id, "surface:main");
        assert_eq!(context.panes.len(), 1);
        assert_eq!(context.canvases.len(), 1);
        assert_eq!(context.panes[0].kind, SurfacePaneKind::TldrawCanvas);
        assert_eq!(context.canvases[0].canvas_id, "canvas:main");
        assert_eq!(context.canvases[0].tldraw_room_id, "ocean-surface-main");
    }

    #[test]
    fn surface_context_is_session_rooted_and_includes_all_canvas_ledgers() {
        let mut state = SurfaceState::default();
        let second_canvas = state.open_canvas_pane("Storyboard", SurfaceMode::Storyboard);
        state.upsert_component(
            "canvas:main",
            LedgerComponent::markdown_card("brief-1", 40.0, 40.0, "Sales brief"),
        );
        state.upsert_component(
            &second_canvas,
            LedgerComponent::markdown_card("frame-1", 80.0, 80.0, "Opening frame"),
        );

        let context = state.turn_context();

        assert_eq!(context.session_id, "surface:main");
        assert_eq!(context.panes.len(), 2);
        assert_eq!(context.canvases.len(), 2);
        assert!(context.canvases.iter().any(|canvas| {
            canvas
                .components
                .iter()
                .any(|component| component.id == "brief-1")
        }));
        assert!(context.canvases.iter().any(|canvas| {
            canvas
                .components
                .iter()
                .any(|component| component.id == "frame-1")
        }));
    }

    #[test]
    fn upsert_then_remove_component_keys_by_id_and_bumps_revision() {
        let mut state = SurfaceState::default();
        state.upsert_component(
            "canvas:main",
            LedgerComponent::markdown_card("brief-1", 40.0, 40.0, "v1"),
        );
        // Re-render with the same id upserts in place rather than duplicating.
        state.upsert_component(
            "canvas:main",
            LedgerComponent::markdown_card("brief-1", 40.0, 40.0, "v2"),
        );
        let canvas = state.canvas("canvas:main").expect("canvas");
        assert_eq!(canvas.components.len(), 1);
        assert_eq!(canvas.components["brief-1"].content, json!({ "text": "v2" }));

        let rev_before = state.canvas("canvas:main").unwrap().revision;
        assert!(state.remove_component("canvas:main", "brief-1"));
        let canvas = state.canvas("canvas:main").expect("canvas");
        assert!(canvas.components.is_empty());
        assert_eq!(canvas.revision, rev_before + 1);

        // Removing a missing component is a no-op.
        assert!(!state.remove_component("canvas:main", "brief-1"));
        assert!(!state.remove_component("canvas:missing", "brief-1"));
    }

    #[test]
    fn next_slot_avoids_existing_ledger_components() {
        let mut state = SurfaceState::default();
        state.upsert_component(
            "canvas:main",
            LedgerComponent::markdown_card("brief-1", 40.0, 40.0, "Sales brief"),
        );
        let slot = state.next_slot("canvas:main", 240.0, 160.0).expect("slot");

        assert_ne!((slot.x, slot.y), (40.0, 40.0));
        assert_eq!(slot.width, 240.0);
        assert_eq!(slot.height, 160.0);
    }

    #[test]
    fn ipc_events_decode_canvas_ready_and_ledger_snapshots() {
        let ready: SurfaceIpcEvent = serde_json::from_value(serde_json::json!({
            "type": "canvas_ready",
            "pane_id": "pane:1",
            "canvas_id": "canvas:main",
            "tldraw_room_id": "ocean-surface-main"
        }))
        .expect("canvas ready event");
        assert!(matches!(ready, SurfaceIpcEvent::CanvasReady { .. }));

        let snapshot: SurfaceIpcEvent = serde_json::from_value(serde_json::json!({
            "type": "ledger_snapshot",
            "canvas_id": "canvas:main",
            "revision": 2,
            "components": [
                {
                    "id": "brief-1",
                    "component_type": "markdown_card",
                    "x": 40.0,
                    "y": 40.0,
                    "width": 240.0,
                    "height": 160.0,
                    "content": { "text": "Sales brief" },
                    "metadata": {},
                    "connections": []
                }
            ]
        }))
        .expect("ledger snapshot event");
        assert!(matches!(snapshot, SurfaceIpcEvent::LedgerSnapshot { .. }));
    }

    #[test]
    fn ledger_snapshot_creates_missing_canvas_from_webview_state() {
        let mut state = SurfaceState::default();
        assert!(state.canvas("canvas:webview").is_none());

        let applied = state.apply_ipc_event(SurfaceIpcEvent::LedgerSnapshot {
            canvas_id: "canvas:webview".to_string(),
            revision: 7,
            components: vec![LedgerComponent::markdown_card(
                "brief-1",
                40.0,
                40.0,
                "Sales brief",
            )],
        });

        assert!(applied);
        let canvas = state.canvas("canvas:webview").expect("created canvas");
        assert_eq!(canvas.revision, 7);
        assert_eq!(canvas.components.len(), 1);
        assert_eq!(
            canvas.components["brief-1"].content,
            serde_json::json!({ "text": "Sales brief" })
        );
    }

    #[test]
    fn canvas_error_event_decodes_from_bridge() {
        let event: SurfaceIpcEvent = serde_json::from_value(serde_json::json!({
            "type": "canvas_error",
            "pane_id": "pane:1",
            "canvas_id": "canvas:main",
            "message": "bad command"
        }))
        .expect("canvas error event");

        assert!(matches!(
            event,
            SurfaceIpcEvent::CanvasError { message, .. } if message == "bad command"
        ));
    }

    #[test]
    fn turn_context_carries_surface_topology_without_binding_to_pane() {
        // OCEAN-168 / Slice 9: the SurfaceLedger no longer feeds the agent prompt
        // (`prompt_with_surface_context` was removed; the native CanvasLedger is
        // the single turn-context source). The topology view is RETAINED for the
        // LiveKit compact metadata and the tldraw projection pane, and must still
        // report panes/canvases independent of the active pane focus.
        let mut state = SurfaceState::default();
        state.open_canvas_pane("Workflow", SurfaceMode::WorkflowBuilder);

        let context = state.turn_context();
        assert_eq!(context.session_id, "surface:main");
        // Both the default canvas pane and the newly opened workflow pane appear.
        assert!(context.panes.len() >= 2);
        assert!(context
            .canvases
            .iter()
            .any(|c| c.tldraw_room_id == "ocean-surface-main"));
    }

    #[test]
    fn canvas_web_url_carries_session_pane_canvas_and_sync_uri() {
        let state = SurfaceState::default();
        let pane = &state.panes()[0];
        let url = canvas_web_url(
            Path::new("/tmp/Ocean GUI/canvas-web/index.html"),
            state.session_id(),
            pane,
            Some("http://127.0.0.1:5858/connect"),
        )
        .expect("canvas url");

        assert_eq!(
            url,
            "file:///tmp/Ocean%20GUI/canvas-web/index.html?session_id=surface%3Amain&pane_id=pane%3A1&canvas_id=canvas%3Amain&tldraw_room_id=ocean-surface-main&sync_uri=http%3A%2F%2F127.0.0.1%3A5858%2Fconnect"
        );
    }
}
