//! Ocean Surface — proxy.
//!
//! Two jobs:
//!
//! 1. Serve the compiled WASM bundle (Trunk's `dist/` directory) so a phone
//!    on the same network can load the app over HTTP without needing trunk
//!    serve running. Production deployment runs *only* this binary.
//!
//! 2. Hold the xAI API key and proxy STT + TTS requests so the WASM client
//!    never sees the secret. Routes are added in Phase 5; for now we expose
//!    `/health` and the static file server only.
//!
//! Run: `cargo run -p ocean-surface-proxy -- --dist ./dist --bind 0.0.0.0:8790`
//! Then point a browser at http://<host>:8790/.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::{routing::get, Json, Router};
use serde_json::json;
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "ocean_surface_proxy=info".into()),
        )
        .init();

    let bind: SocketAddr = std::env::var("OCEAN_SURFACE_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8790".into())
        .parse()
        .context("OCEAN_SURFACE_BIND must be host:port")?;

    let dist = std::env::var("OCEAN_SURFACE_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("dist"));

    let app = Router::new()
        .route("/health", get(health))
        .fallback_service(ServeDir::new(&dist).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    tracing::info!(?bind, dist = %dist.display(), "ocean-surface-proxy listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "service": "ocean-surface-proxy",
        "stt": "not_wired",
        "tts": "not_wired",
    }))
}
