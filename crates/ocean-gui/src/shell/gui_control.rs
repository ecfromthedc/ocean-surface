use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const REGION_CHAT_INLINE: &str = "chat.inline";
pub const REGION_MAIN_CANVAS: &str = "main.canvas";
pub const REGION_SIDEBAR_LEFT: &str = "sidebar.left";
pub const REGION_SIDEBAR_RIGHT: &str = "sidebar.right";
pub const REGION_STATUS_BAR: &str = "status.bar";
pub const REGION_MODAL: &str = "modal";
pub const REGION_DRAWER_BOTTOM: &str = "drawer.bottom";

const RECENT_EVENT_LIMIT: usize = 64;
const KNOWN_REGIONS: &[&str] = &[
    REGION_CHAT_INLINE,
    REGION_MAIN_CANVAS,
    REGION_SIDEBAR_LEFT,
    REGION_SIDEBAR_RIGHT,
    REGION_STATUS_BAR,
    REGION_MODAL,
    REGION_DRAWER_BOTTOM,
];

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RegionId(String);

impl RegionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RegionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for RegionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PaneId(String);

impl PaneId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PaneId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for PaneId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ComponentId(String);

impl ComponentId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ComponentId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ComponentId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RoomId(String);

impl RoomId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RoomId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for RoomId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct CanvasId(String);

impl CanvasId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CanvasId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for CanvasId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GuiCommand {
    FocusRegion {
        region: RegionId,
    },
    OpenSession {
        session_id: String,
    },
    SwitchSession {
        session_id: String,
    },
    SwitchRoom {
        room_id: RoomId,
    },
    MountComponent {
        region: RegionId,
        component_id: ComponentId,
        kind: String,
        props: Value,
        replace: bool,
    },
    UpdateComponent {
        component_id: ComponentId,
        props: Value,
    },
    UnmountComponent {
        component_id: ComponentId,
    },
    PatchCanvas {
        canvas_id: CanvasId,
        patch: Value,
    },
    SetStatus {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GuiControlEvent {
    Focused {
        region: RegionId,
    },
    SessionOpened {
        session_id: String,
    },
    SessionSwitched {
        session_id: String,
    },
    RoomSwitched {
        room_id: RoomId,
    },
    ComponentMounted {
        component_id: ComponentId,
        region: RegionId,
        revision: u64,
    },
    ComponentUpdated {
        component_id: ComponentId,
        revision: u64,
    },
    ComponentUnmounted {
        component_id: ComponentId,
    },
    CanvasPatched {
        canvas_id: CanvasId,
        revision: u64,
    },
    StatusChanged {
        text: String,
    },
    Rejected {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MountedComponent {
    pub component_id: ComponentId,
    pub region: RegionId,
    pub kind: String,
    pub props: Value,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasState {
    pub canvas_id: CanvasId,
    pub patches: Vec<Value>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuiControlState {
    active_region: RegionId,
    active_pane: Option<PaneId>,
    active_room: Option<RoomId>,
    active_session_id: Option<String>,
    components: HashMap<ComponentId, MountedComponent>,
    canvases: HashMap<CanvasId, CanvasState>,
    status: String,
    recent_events: VecDeque<GuiControlEvent>,
}

impl Default for GuiControlState {
    fn default() -> Self {
        Self {
            active_region: RegionId::from(REGION_CHAT_INLINE),
            active_pane: None,
            active_room: None,
            active_session_id: None,
            components: HashMap::new(),
            canvases: HashMap::new(),
            status: "idle".to_string(),
            recent_events: VecDeque::new(),
        }
    }
}

impl GuiControlState {
    #[must_use]
    pub fn active_region(&self) -> &RegionId {
        &self.active_region
    }

    #[must_use]
    pub fn active_region_label(&self) -> &str {
        self.active_region.as_str()
    }

    #[must_use]
    pub fn active_pane(&self) -> Option<&PaneId> {
        self.active_pane.as_ref()
    }

    #[must_use]
    pub fn active_room(&self) -> Option<&RoomId> {
        self.active_room.as_ref()
    }

    #[must_use]
    pub fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }

    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[must_use]
    pub fn component(&self, component_id: &ComponentId) -> Option<&MountedComponent> {
        self.components.get(component_id)
    }

    #[must_use]
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    #[must_use]
    pub fn component_count_in_region(&self, region: &RegionId) -> usize {
        self.components
            .values()
            .filter(|component| &component.region == region)
            .count()
    }

    #[must_use]
    pub fn last_event(&self) -> Option<&GuiControlEvent> {
        self.recent_events.back()
    }

    pub fn apply(&mut self, command: GuiCommand) -> GuiControlEvent {
        let event = match command {
            GuiCommand::FocusRegion { region } => self.focus_region(region),
            GuiCommand::OpenSession { session_id } => {
                self.active_session_id = Some(session_id.clone());
                GuiControlEvent::SessionOpened { session_id }
            }
            GuiCommand::SwitchSession { session_id } => {
                self.active_session_id = Some(session_id.clone());
                GuiControlEvent::SessionSwitched { session_id }
            }
            GuiCommand::SwitchRoom { room_id } => {
                self.active_room = Some(room_id.clone());
                GuiControlEvent::RoomSwitched { room_id }
            }
            GuiCommand::MountComponent {
                region,
                component_id,
                kind,
                props,
                replace,
            } => self.mount_component(region, component_id, kind, props, replace),
            GuiCommand::UpdateComponent {
                component_id,
                props,
            } => self.update_component(component_id, props),
            GuiCommand::UnmountComponent { component_id } => self.unmount_component(component_id),
            GuiCommand::PatchCanvas { canvas_id, patch } => {
                let revision = self
                    .canvases
                    .entry(canvas_id.clone())
                    .and_modify(|canvas| {
                        canvas.revision += 1;
                        canvas.patches.push(patch.clone());
                    })
                    .or_insert_with(|| CanvasState {
                        canvas_id: canvas_id.clone(),
                        patches: vec![patch],
                        revision: 1,
                    })
                    .revision;
                GuiControlEvent::CanvasPatched {
                    canvas_id,
                    revision,
                }
            }
            GuiCommand::SetStatus { text } => {
                self.status = text.clone();
                GuiControlEvent::StatusChanged { text }
            }
        };

        self.record(event)
    }

    fn focus_region(&mut self, region: RegionId) -> GuiControlEvent {
        if !is_known_region(&region) {
            return GuiControlEvent::Rejected {
                reason: "unknown region".to_string(),
            };
        }

        self.active_region = region.clone();
        GuiControlEvent::Focused { region }
    }

    fn mount_component(
        &mut self,
        region: RegionId,
        component_id: ComponentId,
        kind: String,
        props: Value,
        replace: bool,
    ) -> GuiControlEvent {
        if !is_known_region(&region) {
            return GuiControlEvent::Rejected {
                reason: "unknown region".to_string(),
            };
        }

        if let Some(component) = self.components.get_mut(&component_id) {
            if !replace {
                return GuiControlEvent::Rejected {
                    reason: "component already mounted".to_string(),
                };
            }

            component.region = region.clone();
            component.kind = kind;
            component.props = props;
            component.revision += 1;
            return GuiControlEvent::ComponentMounted {
                component_id,
                region,
                revision: component.revision,
            };
        }

        self.components.insert(
            component_id.clone(),
            MountedComponent {
                component_id: component_id.clone(),
                region: region.clone(),
                kind,
                props,
                revision: 1,
            },
        );

        GuiControlEvent::ComponentMounted {
            component_id,
            region,
            revision: 1,
        }
    }

    fn update_component(&mut self, component_id: ComponentId, props: Value) -> GuiControlEvent {
        let Some(component) = self.components.get_mut(&component_id) else {
            return GuiControlEvent::Rejected {
                reason: "component not mounted".to_string(),
            };
        };

        component.props = props;
        component.revision += 1;
        GuiControlEvent::ComponentUpdated {
            component_id,
            revision: component.revision,
        }
    }

    fn unmount_component(&mut self, component_id: ComponentId) -> GuiControlEvent {
        if self.components.remove(&component_id).is_none() {
            return GuiControlEvent::Rejected {
                reason: "component not mounted".to_string(),
            };
        }

        GuiControlEvent::ComponentUnmounted { component_id }
    }

    fn record(&mut self, event: GuiControlEvent) -> GuiControlEvent {
        if self.recent_events.len() == RECENT_EVENT_LIMIT {
            self.recent_events.pop_front();
        }
        self.recent_events.push_back(event.clone());
        event
    }
}

fn is_known_region(region: &RegionId) -> bool {
    KNOWN_REGIONS.contains(&region.as_str())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ComponentId, GuiCommand, GuiControlEvent, GuiControlState, REGION_CHAT_INLINE,
        REGION_MAIN_CANVAS, RegionId, RoomId,
    };

    #[test]
    fn focuses_known_regions_and_records_event() {
        let mut state = GuiControlState::default();

        let event = state.apply(GuiCommand::FocusRegion {
            region: RegionId::from(REGION_MAIN_CANVAS),
        });

        assert_eq!(
            event,
            GuiControlEvent::Focused {
                region: RegionId::from(REGION_MAIN_CANVAS)
            }
        );
        assert_eq!(state.active_region().as_str(), REGION_MAIN_CANVAS);
        assert_eq!(state.last_event(), Some(&event));
    }

    #[test]
    fn mounts_updates_and_unmounts_components_by_region() {
        let mut state = GuiControlState::default();
        let component_id = ComponentId::from("approval-1");

        let mounted = state.apply(GuiCommand::MountComponent {
            region: RegionId::from(REGION_CHAT_INLINE),
            component_id: component_id.clone(),
            kind: "confirm".to_string(),
            props: json!({ "title": "Restart daemon" }),
            replace: false,
        });

        assert_eq!(
            mounted,
            GuiControlEvent::ComponentMounted {
                component_id: component_id.clone(),
                region: RegionId::from(REGION_CHAT_INLINE),
                revision: 1,
            }
        );
        assert_eq!(state.component_count(), 1);
        assert_eq!(
            state
                .component(&component_id)
                .expect("mounted component")
                .props,
            json!({ "title": "Restart daemon" })
        );

        let updated = state.apply(GuiCommand::UpdateComponent {
            component_id: component_id.clone(),
            props: json!({ "title": "Restart daemon", "state": "running" }),
        });

        assert_eq!(
            updated,
            GuiControlEvent::ComponentUpdated {
                component_id: component_id.clone(),
                revision: 2,
            }
        );
        assert_eq!(
            state
                .component(&component_id)
                .expect("updated component")
                .props,
            json!({ "title": "Restart daemon", "state": "running" })
        );

        let unmounted = state.apply(GuiCommand::UnmountComponent {
            component_id: component_id.clone(),
        });

        assert_eq!(
            unmounted,
            GuiControlEvent::ComponentUnmounted { component_id }
        );
        assert_eq!(state.component_count(), 0);
    }

    #[test]
    fn rejects_update_for_unknown_component() {
        let mut state = GuiControlState::default();

        let event = state.apply(GuiCommand::UpdateComponent {
            component_id: ComponentId::from("missing"),
            props: json!({ "state": "ignored" }),
        });

        assert_eq!(
            event,
            GuiControlEvent::Rejected {
                reason: "component not mounted".to_string()
            }
        );
        assert_eq!(state.component_count(), 0);
    }

    #[test]
    fn switches_room_and_session_independently() {
        let mut state = GuiControlState::default();

        state.apply(GuiCommand::SwitchRoom {
            room_id: RoomId::from("daily-standup"),
        });
        state.apply(GuiCommand::SwitchSession {
            session_id: "sess-42".to_string(),
        });

        assert_eq!(
            state.active_room().map(RoomId::as_str),
            Some("daily-standup")
        );
        assert_eq!(state.active_session_id(), Some("sess-42"));
    }
}
