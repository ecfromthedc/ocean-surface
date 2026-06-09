mod agent;
mod assets;
mod canvas;
mod commands;
mod daemon;
mod editor_buffer;
mod editor_layout;
mod gui_control;
mod icons;
mod model;
mod rooms;
mod surface;
mod surface_host;
mod surface_livekit;
mod surface_livekit_client;
// The native libwebrtc-backed room session is only compiled with the `livekit`
// feature; the always-on `surface_livekit_client` facade provides a stub spawn
// otherwise. See crates/ocean-gui/Cargo.toml.
#[cfg(feature = "livekit")]
mod surface_livekit_session;
mod surface_livekit_video;
mod theme;
mod tldraw_adapter;
mod vault_index;
mod view;
mod watcher;

pub use assets::ShellAssets;
pub use model::ShellState;
pub use view::OceanGuiShell;
