use std::collections::VecDeque;
use std::sync::mpsc::Sender;

use super::surface::{SurfaceIpcCommand, SurfaceIpcEvent};
use gpui::{Bounds, Pixels};

#[derive(Clone, Debug, PartialEq)]
pub struct CanvasHostTarget {
    pub pane_id: String,
    pub url: String,
    pub bounds: HostBounds,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl HostBounds {
    #[must_use]
    pub fn from_gpui(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: f64::from(bounds.origin.x),
            y: f64::from(bounds.origin.y),
            width: f64::from(bounds.size.width).max(0.0),
            height: f64::from(bounds.size.height).max(0.0),
        }
    }

    #[must_use]
    pub fn visible(self) -> bool {
        self.width >= 1.0 && self.height >= 1.0
    }

    #[cfg(target_os = "macos")]
    fn to_wry_rect(self) -> wry::Rect {
        wry::Rect {
            position: wry::dpi::LogicalPosition::new(self.x, self.y).into(),
            size: wry::dpi::LogicalSize::new(self.width, self.height).into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanvasHostAction {
    Mount(CanvasHostTarget),
    Navigate { pane_id: String, url: String },
    Resize { pane_id: String, bounds: HostBounds },
    Eval { pane_id: String, script: String },
    Hide { pane_id: String },
}

#[derive(Debug, Default)]
pub struct CanvasHostState {
    active: Option<CanvasHostTarget>,
    last_command_json: Option<String>,
    pending: VecDeque<CanvasHostAction>,
    inbound: VecDeque<SurfaceIpcEvent>,
}

impl CanvasHostState {
    pub fn sync_target(&mut self, target: Option<CanvasHostTarget>) {
        match (self.active.as_mut(), target) {
            (None, Some(target)) if target.bounds.visible() => {
                self.pending
                    .push_back(CanvasHostAction::Mount(target.clone()));
                self.active = Some(target);
                self.last_command_json = None;
            }
            (Some(active), Some(target)) if active.pane_id != target.pane_id => {
                self.pending.push_back(CanvasHostAction::Hide {
                    pane_id: active.pane_id.clone(),
                });
                if target.bounds.visible() {
                    self.pending
                        .push_back(CanvasHostAction::Mount(target.clone()));
                    self.active = Some(target);
                } else {
                    self.active = None;
                }
                self.last_command_json = None;
            }
            (Some(active), Some(target)) if active.url != target.url => {
                active.url = target.url.clone();
                active.bounds = target.bounds;
                self.pending.push_back(CanvasHostAction::Navigate {
                    pane_id: active.pane_id.clone(),
                    url: target.url,
                });
                self.last_command_json = None;
            }
            (Some(active), Some(target)) if active.bounds != target.bounds => {
                active.bounds = target.bounds;
                self.pending.push_back(CanvasHostAction::Resize {
                    pane_id: active.pane_id.clone(),
                    bounds: target.bounds,
                });
            }
            (Some(active), None) => {
                self.pending.push_back(CanvasHostAction::Hide {
                    pane_id: active.pane_id.clone(),
                });
                self.active = None;
                self.last_command_json = None;
            }
            _ => {}
        }
    }

    pub fn sync_command(&mut self, command: &SurfaceIpcCommand) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let Ok(payload) = serde_json::to_string(command) else {
            return;
        };
        if self.last_command_json.as_deref() == Some(payload.as_str()) {
            return;
        }

        let script = format!("window.oceanSurfaceApplyCommand?.({payload});");
        self.last_command_json = Some(payload);
        self.pending.push_back(CanvasHostAction::Eval {
            pane_id: active.pane_id.clone(),
            script,
        });
    }

    pub fn push_event_json(&mut self, payload: &str) -> Result<(), serde_json::Error> {
        let event = serde_json::from_str(payload)?;
        self.inbound.push_back(event);
        Ok(())
    }

    pub fn pop_event(&mut self) -> Option<SurfaceIpcEvent> {
        self.inbound.pop_front()
    }

    pub fn pop_action(&mut self) -> Option<CanvasHostAction> {
        self.pending.pop_front()
    }
}

pub struct CanvasWebViewHost {
    #[cfg(target_os = "macos")]
    webview: Option<wry::WebView>,
    #[cfg(target_os = "macos")]
    pane_id: Option<String>,
    #[cfg(target_os = "macos")]
    ipc_sender: Sender<String>,
}

#[cfg(target_os = "macos")]
impl CanvasWebViewHost {
    #[must_use]
    pub fn new(ipc_sender: Sender<String>) -> Self {
        Self {
            webview: None,
            pane_id: None,
            ipc_sender,
        }
    }

    pub fn apply_action<W>(&mut self, parent: &W, action: CanvasHostAction) -> Result<(), String>
    where
        W: wry::raw_window_handle::HasWindowHandle,
    {
        match action {
            CanvasHostAction::Mount(target) => self.mount(parent, target),
            CanvasHostAction::Navigate { pane_id, url } => {
                if self.pane_id.as_deref() == Some(pane_id.as_str())
                    && let Some(webview) = self.webview.as_ref()
                {
                    webview.load_url(&url).map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            CanvasHostAction::Resize { pane_id, bounds } => {
                if self.pane_id.as_deref() == Some(pane_id.as_str())
                    && let Some(webview) = self.webview.as_ref()
                {
                    webview
                        .set_bounds(bounds.to_wry_rect())
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            CanvasHostAction::Eval { pane_id, script } => {
                if self.pane_id.as_deref() == Some(pane_id.as_str())
                    && let Some(webview) = self.webview.as_ref()
                {
                    webview
                        .evaluate_script(&script)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            CanvasHostAction::Hide { pane_id } => {
                if self.pane_id.as_deref() == Some(pane_id.as_str()) {
                    if let Some(webview) = self.webview.as_ref() {
                        let _ = webview.set_visible(false);
                    }
                    self.webview = None;
                    self.pane_id = None;
                }
                Ok(())
            }
        }
    }

    fn mount<W>(&mut self, parent: &W, target: CanvasHostTarget) -> Result<(), String>
    where
        W: wry::raw_window_handle::HasWindowHandle,
    {
        let sender = self.ipc_sender.clone();
        let webview = wry::WebViewBuilder::new()
            .with_url(&target.url)
            .with_bounds(target.bounds.to_wry_rect())
            .with_ipc_handler(move |request| {
                let _ = sender.send(request.body().clone());
            })
            .build_as_child(parent)
            .map_err(|error| error.to_string())?;

        self.pane_id = Some(target.pane_id);
        self.webview = Some(webview);
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
impl CanvasWebViewHost {
    #[must_use]
    pub fn new(_ipc_sender: Sender<String>) -> Self {
        Self {}
    }

    pub fn apply_action<W>(
        &mut self,
        _parent: &W,
        _action: CanvasHostAction,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::shell::surface::LedgerComponent;

    #[test]
    fn host_mounts_resizes_navigates_and_hides_active_canvas() {
        let mut host = CanvasHostState::default();
        let first = CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas-a.html".to_string(),
            bounds: HostBounds {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 200.0,
            },
        };

        host.sync_target(Some(first.clone()));
        assert_eq!(host.pop_action(), Some(CanvasHostAction::Mount(first)));

        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas-a.html".to_string(),
            bounds: HostBounds {
                x: 10.0,
                y: 20.0,
                width: 400.0,
                height: 240.0,
            },
        }));
        assert!(matches!(
            host.pop_action(),
            Some(CanvasHostAction::Resize { pane_id, .. }) if pane_id == "pane:1"
        ));

        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas-b.html".to_string(),
            bounds: HostBounds {
                x: 10.0,
                y: 20.0,
                width: 400.0,
                height: 240.0,
            },
        }));
        assert_eq!(
            host.pop_action(),
            Some(CanvasHostAction::Navigate {
                pane_id: "pane:1".to_string(),
                url: "file:///canvas-b.html".to_string(),
            })
        );

        host.sync_target(None);
        assert_eq!(
            host.pop_action(),
            Some(CanvasHostAction::Hide {
                pane_id: "pane:1".to_string(),
            })
        );
    }

    #[test]
    fn host_serializes_surface_commands_for_webview_bridge() {
        let mut host = CanvasHostState::default();
        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas.html".to_string(),
            bounds: HostBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        }));
        let _ = host.pop_action();

        host.sync_command(&SurfaceIpcCommand::UpsertComponent {
            canvas_id: "canvas:main".to_string(),
            component: LedgerComponent {
                id: "brief-1".to_string(),
                component_type: "markdown_card".to_string(),
                x: 40.0,
                y: 40.0,
                width: 240.0,
                height: 160.0,
                content: serde_json::json!({ "text": "Brief" }),
                metadata: serde_json::json!({}),
                connections: Vec::new(),
            },
        });

        let Some(CanvasHostAction::Eval { script, .. }) = host.pop_action() else {
            panic!("expected eval action");
        };
        assert!(script.contains("window.oceanSurfaceApplyCommand?.("));
        assert!(script.contains("\"type\":\"upsert_component\""));
        assert!(script.contains("\"canvas_id\":\"canvas:main\""));
    }

    #[test]
    fn host_dedupes_repeated_bridge_commands_until_target_changes() {
        let mut host = CanvasHostState::default();
        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas-a.html".to_string(),
            bounds: HostBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        }));
        let _ = host.pop_action();
        let command = SurfaceIpcCommand::LoadCanvas {
            pane_id: "pane:1".to_string(),
            canvas_id: "canvas:main".to_string(),
            tldraw_room_id: "ocean-surface-main".to_string(),
        };

        host.sync_command(&command);
        assert!(matches!(
            host.pop_action(),
            Some(CanvasHostAction::Eval { .. })
        ));
        host.sync_command(&command);
        assert!(host.pop_action().is_none());

        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas-b.html".to_string(),
            bounds: HostBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        }));
        let _ = host.pop_action();
        host.sync_command(&command);
        assert!(matches!(
            host.pop_action(),
            Some(CanvasHostAction::Eval { .. })
        ));
    }

    #[test]
    fn host_decodes_canvas_events_from_ipc_json() {
        let mut host = CanvasHostState::default();
        host.push_event_json(
            r#"{
                "type": "selection_changed",
                "canvas_id": "canvas:main",
                "selected_ids": ["brief-1"]
            }"#,
        )
        .expect("event json");

        assert!(matches!(
            host.pop_event(),
            Some(SurfaceIpcEvent::SelectionChanged { selected_ids, .. })
                if selected_ids == vec!["brief-1".to_string()]
        ));
    }

    #[test]
    fn zero_sized_target_does_not_mount() {
        let mut host = CanvasHostState::default();
        host.sync_target(Some(CanvasHostTarget {
            pane_id: "pane:1".to_string(),
            url: "file:///canvas.html".to_string(),
            bounds: HostBounds::default(),
        }));

        assert!(host.active.is_none());
        assert!(host.pop_action().is_none());
    }

    #[test]
    fn host_bounds_preserve_gpui_geometry() {
        let bounds = Bounds::new(
            gpui::point(gpui::px(12.5), gpui::px(24.0)),
            gpui::size(gpui::px(640.0), gpui::px(360.0)),
        );

        assert_eq!(
            HostBounds::from_gpui(bounds),
            HostBounds {
                x: 12.5,
                y: 24.0,
                width: 640.0,
                height: 360.0,
            }
        );
    }
}
