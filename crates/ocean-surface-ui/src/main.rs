//! Ocean Surface — Leptos entry.
//!
//! Mounts the App component to <body>. Both the browser PWA build (via
//! Trunk) and the future Tauri shell load this same binary; what changes
//! between them is just the host (browser vs WKWebView).

use leptos::prelude::*;

mod app;
mod daemon;
mod markdown;
mod model;
mod transcript;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(log::Level::Info);
    mount_to_body(App);
}
