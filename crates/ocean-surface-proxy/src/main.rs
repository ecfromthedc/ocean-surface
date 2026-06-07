//! Ocean Surface — proxy.
//!
//! Two jobs:
//!
//! 1. Serve the compiled WASM bundle (Trunk's `dist/` directory) so a phone
//!    on the same network can load the app over HTTP without needing trunk
//!    serve running. Production deployment runs *only* this binary.
//!
//! 2. Hold the xAI API key and proxy STT + TTS requests so the WASM client
//!    never sees the secret. The browser fetches `/api/config` on load for
//!    zero-config bootstrap, then talks to `/api/stt` and `/api/tts`.
//!
//! Run: `cargo run -p ocean-surface-proxy -- --dist ./dist --bind 0.0.0.0:8790`
//! Then point a browser at http://<host>:8790/.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    body::Bytes,
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

const XAI_STT_URL: &str = "https://api.x.ai/v1/stt";
const XAI_TTS_URL: &str = "https://api.x.ai/v1/tts";
const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4780";
const DEFAULT_LIVEKIT_ROOM_ID: &str = "project:surface-main";
const DEFAULT_VOICE_PROFILE: &str = "leo";
// Default Google Maps JS API key (browser key, referrer-restricted in GCP).
// Override at runtime with GOOGLE_MAPS_API_KEY.
const DEFAULT_MAPS_KEY: &str = "AIzaSyCmUHR3JD9AZfw9DiRvvSSZsRitdGuunPs";

/// Shared state: an HTTP client plus the resolved xAI key + voice config.
struct AppState {
    http: reqwest::Client,
    /// The resolved xAI API key, if one could be found at startup.
    xai_key: Option<String>,
    voice_profile: String,
    daemon_url: String,
    default_livekit_room_id: String,
    tldraw_sync_uri: Option<String>,
    /// Google Maps JS API key, handed to the client via /api/config so the map
    /// component can load the Maps script. Maps browser keys are referrer-
    /// restricted (not secret), so client-side exposure is the intended model.
    maps_key: Option<String>,
    /// Map ID for the map's visual style (DEMO_MAP_ID by default).
    maps_map_id: String,
    /// Optional HTTP Basic auth. `Some((user, pass))` gates every route
    /// except /health. `None` = open (local dev). Set via OCEAN_SURFACE_USER
    /// + OCEAN_SURFACE_PASS.
    basic_auth: Option<(String, String)>,
}

impl AppState {
    fn has_auth(&self) -> bool {
        self.xai_key.is_some()
    }
}

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

    let xai_key = match resolve_xai_key() {
        Ok(key) => key,
        Err(err) => {
            tracing::warn!(error = %err, "failed to resolve xAI key; STT/TTS will be disabled");
            None
        }
    };
    if xai_key.is_some() {
        tracing::info!("xAI key resolved; STT/TTS enabled");
    } else {
        tracing::warn!("no xAI key found (env XAI_API_KEY, ~/.config/ocean-surface/xai.key, or ~/.pi/agent/settings.json); STT/TTS disabled. Drop your key in ~/.config/ocean-surface/xai.key to preconfigure voice.");
    }

    let voice_profile =
        std::env::var("OCEAN_VOICE_PROFILE").unwrap_or_else(|_| DEFAULT_VOICE_PROFILE.into());
    let daemon_url =
        std::env::var("OCEAN_DAEMON_URL").unwrap_or_else(|_| DEFAULT_DAEMON_URL.into());
    let default_livekit_room_id = std::env::var("OCEAN_LIVEKIT_ROOM_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LIVEKIT_ROOM_ID.to_string());
    let tldraw_sync_uri = std::env::var("OCEAN_TLDRAW_SYNC_URI")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    // Google Maps JS API key for the map component. Env override, else the
    // configured default. Empty disables the map (component renders a notice).
    let maps_key = std::env::var("GOOGLE_MAPS_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(DEFAULT_MAPS_KEY.to_string()))
        .filter(|s| !s.is_empty());
    if maps_key.is_some() {
        tracing::info!("Google Maps key resolved; map component enabled");
    }
    // Map ID controls the map's visual style. Defaults to DEMO_MAP_ID (works
    // with advanced markers + Places UI Kit out of the box). Set
    // GOOGLE_MAPS_MAP_ID to your custom styled map id to skin it.
    let maps_map_id = std::env::var("GOOGLE_MAPS_MAP_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "DEMO_MAP_ID".to_string());

    // HTTP Basic auth. Enabled by default with the operator creds; set
    // OCEAN_SURFACE_AUTH=off to disable entirely (e.g. trusted localhost).
    let basic_auth = if std::env::var("OCEAN_SURFACE_AUTH").as_deref() == Ok("off") {
        tracing::warn!("HTTP Basic auth DISABLED (OCEAN_SURFACE_AUTH=off)");
        None
    } else {
        let user = std::env::var("OCEAN_SURFACE_USER").unwrap_or_else(|_| "smathdaddy".into());
        let pass = std::env::var("OCEAN_SURFACE_PASS").unwrap_or_else(|_| "***REMOVED-CREDENTIAL***".into());
        tracing::info!(user = %user, "HTTP Basic auth enabled");
        Some((user, pass))
    };

    let state = Arc::new(AppState {
        http: reqwest::Client::new(),
        xai_key,
        voice_profile,
        daemon_url,
        default_livekit_room_id,
        tldraw_sync_uri,
        basic_auth,
        maps_key,
        maps_map_id,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/config", get(config))
        .route("/api/stt", post(stt))
        .route("/api/tts", post(tts))
        // Reverse-proxy the daemon's agent API so a remote browser (phone via
        // the tunnel) talks to the daemon through this same origin — no
        // hardcoded localhost, no mixed-content. The daemon stays bound to
        // 127.0.0.1 and is never exposed directly.
        .route("/v1/agent/turns", post(proxy_turns))
        .route("/v1/agent/events", get(proxy_events))
        // Control stream + permission decision (OCEAN-135/136). The web UI opens
        // GET /v1/events (the CONTROL stream that carries permission_request
        // cards) and answers a prompt by POSTing
        // /v1/permissions/{id}/decision. Without these reverse-proxy routes both
        // fell through to ServeDir → 404, so on the phone/tunnel surface a
        // permission-gated tool call hung the turn forever (the card never
        // arrived) and Allow/Deny never reached the daemon. /v1/events streams
        // like /v1/agent/events; the decision route forwards body + the {id}.
        .route("/v1/events", get(proxy_control_events))
        .route(
            "/v1/permissions/{id}/decision",
            post(proxy_permission_decision),
        )
        .route(
            "/v1/agent/sessions",
            get(proxy_sessions).post(proxy_sessions_post),
        )
        .route("/v1/sessions/{id}", get(proxy_session_detail))
        .route("/v1/agent/sessions/{id}", get(proxy_agent_session_detail))
        // Model picker + halt button reach the daemon through this origin too.
        .route("/v1/models", get(proxy_models))
        .route("/v1/model", get(proxy_model_get).post(proxy_model_set))
        .route(
            "/v1/projects",
            get(proxy_projects_list).post(proxy_projects_create),
        )
        .route(
            "/v1/projects/{id}",
            get(proxy_project_get)
                .patch(proxy_project_patch)
                .delete(proxy_project_delete),
        )
        // Persistent Rooms lifecycle (OCEAN-65/107/120). The Rooms UI talks to
        // these same-origin; without these reverse-proxy routes every rooms
        // request fell through to ServeDir → 404 (empty body) → the UI's
        // resp.json() choked with "EOF while parsing a value" and the whole
        // Rooms feature was dead on web. The `/persistent` literal route covers
        // list (GET) + create (POST); the wildcard catch-all covers every
        // sub-path (room get, participants join/leave, messages, transcript)
        // forwarding method + body + query (the transcript tail uses
        // ?after_seq=). Declared BEFORE the livekit-token route so the
        // `persistent` segment is matched as a literal, never swallowed by the
        // `{room_id}` capture — though the two are distinct subtrees either way
        // (`/v1/rooms/persistent/...` vs `/v1/rooms/{id}/livekit-token`).
        .route(
            "/v1/rooms/persistent",
            get(proxy_rooms_persistent).post(proxy_rooms_persistent),
        )
        .route(
            "/v1/rooms/persistent/{*rest}",
            get(proxy_rooms_persistent)
                .post(proxy_rooms_persistent)
                .delete(proxy_rooms_persistent),
        )
        .route(
            "/v1/rooms/{room_id}/livekit-token",
            post(proxy_livekit_token),
        )
        .route("/v1/requests/{id}/cancel", post(proxy_cancel))
        // Component interaction events (kanban click / form submit) flow from a
        // remote surface back to the daemon through this origin too, so a phone
        // via the tunnel can drive interactive components (OCEAN-62c).
        .route("/v1/component/event", post(proxy_component_event))
        // Longhouse council control (convene / demo) reaches the daemon through
        // this origin, so the Game Boy deck served from dist/ can fire a demo
        // (POST /v1/longhouse/demo) and trigger real councils same-origin.
        .route("/v1/longhouse/{*rest}", get(proxy_longhouse).post(proxy_longhouse))
        // Quorum/Council observability deck (OCEAN-96). The Game Boy longhouse
        // viewer is embedded in the binary and exposed at a clean path so the
        // web header's "Council" tab can open it. `/longhouse.html` kept as a
        // legacy alias.
        .route("/ui/council", get(council_deck))
        .route("/longhouse.html", get(council_deck))
        .fallback_service(ServeDir::new(&dist).append_index_html_on_directories(true))
        // Fix the headers on the compiled `.wasm` asset (the blank-page bug).
        // ServeDir guesses `application/wasm` itself, but two things still broke
        // the deployed page: (1) Cloudflare's tunnel re-compressed the wasm with
        // `content-encoding: zstd`, which — combined with the Trunk SRI integrity
        // on the wasm preload — made Chrome ABORT the wasm fetch, so `init()`
        // rejected and the app never mounted; (2) the immutable hashed asset
        // wasn't marked cacheable. This post-response layer forces
        // `Content-Type: application/wasm` and `Cache-Control:
        // public, max-age=31536000, immutable, no-transform`. `no-transform` is
        // the directive Cloudflare honors to skip compression/minification, so
        // the wasm is served byte-for-byte uncompressed and the fetch no longer
        // aborts. Runs AFTER routing/ServeDir so it only touches the actual file
        // response; non-wasm paths pass through untouched.
        .layer(middleware::from_fn(wasm_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            basic_auth_gate,
        ))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!(?bind, dist = %dist.display(), "ocean-surface-proxy listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Resolve the xAI API key, in priority order:
///   1. env `XAI_API_KEY`
///   2. the dedicated key file `~/.config/ocean-surface/xai.key` (override
///      path via `OCEAN_SURFACE_KEY_FILE`) — the canonical "preconfigured"
///      location: drop the key there once and every launch picks it up with
///      no env-setting.
///   3. JSON path `.xai.apiKey` inside `~/.pi/agent/settings.json`.
/// Returns `Ok(None)` when no key is configured (absent files are not errors).
fn resolve_xai_key() -> anyhow::Result<Option<String>> {
    // 1. Environment.
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(Some(key));
        }
    }

    // 2. Dedicated persistent key file — the preconfigured source of truth.
    if let Some(key) = read_key_file()? {
        return Ok(Some(key));
    }

    // 3. Legacy fallback: the pi agent settings file.
    let settings_path = match std::env::var("XAI_SETTINGS_FILE") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let home = std::env::var("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".pi/agent/settings.json")
        }
    };

    if !settings_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&settings_path)
        .with_context(|| format!("reading {}", settings_path.display()))?;
    let settings: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {} as JSON", settings_path.display()))?;

    let key = settings
        .get("xai")
        .and_then(|x| x.get("apiKey"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(key)
}

/// Read the dedicated key file. Default path `~/.config/ocean-surface/xai.key`,
/// overridable via `OCEAN_SURFACE_KEY_FILE`. Whole-file contents, trimmed.
/// Absent file is not an error (returns Ok(None)).
fn read_key_file() -> anyhow::Result<Option<String>> {
    let path = match std::env::var("OCEAN_SURFACE_KEY_FILE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            let home = std::env::var("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".config/ocean-surface/xai.key")
        }
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let key = raw.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}

/// HTTP Basic auth gate. When creds are configured, every request except
/// `/health` must carry a matching `Authorization: Basic` header; otherwise
/// we return 401 with a WWW-Authenticate challenge (the browser's native
/// login popup). No cookies, no sessions — nothing to expire or lock you out.
async fn basic_auth_gate(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let Some((want_user, want_pass)) = state.basic_auth.as_ref() else {
        return next.run(req).await; // auth disabled
    };
    // Let health through unauthenticated so tunnels/monitors can probe.
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());

    if let Some(creds) = provided {
        if let Some((u, p)) = creds.split_once(':') {
            if u == want_user && p == want_pass {
                return next.run(req).await;
            }
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"Ocean Surface\"")],
        "authentication required",
    )
        .into_response()
}

/// Header value: long-lived, immutable, and — critically — `no-transform` so
/// Cloudflare won't zstd/gzip-compress the wasm in front of us. Without
/// `no-transform`, the tunnel served the wasm with `content-encoding: zstd`,
/// which (with the Trunk SRI preload) made Chrome abort the fetch → blank page.
const WASM_CACHE_CONTROL: &str = "public, max-age=31536000, immutable, no-transform";

/// Response post-processor: for any request whose path ends in `.wasm`, force
/// the correct MIME (`application/wasm`, required for `instantiateStreaming`)
/// and a `no-transform` cache policy so the proxy ships the wasm uncompressed.
/// Everything else passes through unchanged. This is the primary fix for the
/// blank deployed page — see the layer registration in `main` for the full
/// root-cause writeup.
async fn wasm_headers(req: Request, next: Next) -> Response {
    let is_wasm = req.uri().path().ends_with(".wasm");
    let mut resp = next.run(req).await;
    if is_wasm && resp.status().is_success() {
        let headers = resp.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/wasm"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(WASM_CACHE_CONTROL),
        );
    }
    resp
}

/// Health check — reports STT/TTS readiness, which is simply whether a key
/// resolved at startup.
async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let ready = state.has_auth();
    Json(json!({
        "ok": true,
        "service": "ocean-surface-proxy",
        "stt": ready,
        "tts": ready,
    }))
}

/// Zero-config bootstrap the UI fetches on load. `daemon_url` is empty so the
/// client talks to the daemon through THIS origin (the /v1/agent/* reverse
/// proxy below) — works identically on localhost and through the tunnel, with
/// no mixed-content or hardcoded host.
async fn config(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(config_payload(&state))
}

fn config_payload(state: &AppState) -> Value {
    json!({
        "daemon_url": "",
        "has_auth": state.has_auth(),
        "voice_profile": state.voice_profile,
        "maps_key": state.maps_key.clone().unwrap_or_default(),
        "maps_map_id": state.maps_map_id.clone(),
        "livekit_room_id": state.default_livekit_room_id,
        "livekit_token_path": livekit_token_daemon_path(&state.default_livekit_room_id),
        "tldraw_sync_uri": state.tldraw_sync_uri.clone().unwrap_or_default(),
        "surface": {
            "livekit_room_id": state.default_livekit_room_id,
            "livekit_token_path": livekit_token_daemon_path(&state.default_livekit_room_id),
            "tldraw_sync_uri": state.tldraw_sync_uri.clone().unwrap_or_default(),
        }
    })
}

/// Reverse-proxy POST /v1/agent/turns to the local daemon.
async fn proxy_turns(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    let url = format!("{}/v1/agent/turns", state.daemon_url.trim_end_matches('/'));
    match state
        .http
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Reverse-proxy POST /v1/agent/sessions to the local daemon.
async fn proxy_sessions_post(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    let url = format!(
        "{}/v1/agent/sessions",
        state.daemon_url.trim_end_matches('/')
    );
    match state
        .http
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Reverse-proxy GET /v1/agent/sessions to the local daemon.
async fn proxy_sessions(State(state): State<Arc<AppState>>, req: Request) -> impl IntoResponse {
    let q = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!(
        "{}/v1/agent/sessions{q}",
        state.daemon_url.trim_end_matches('/')
    );
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Single-session detail passthrough. The chat app loads a session's transcript
/// via GET /v1/sessions/{id} (and the /v1/agent/sessions/{id} variant). Without
/// these the proxy 404'd that path → the app parsed an empty body → "EOF while
/// parsing a value" → blank chat history on session switch.
async fn proxy_session_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    proxy_get_json(&state, &format!("/v1/sessions/{id}")).await
}

async fn proxy_agent_session_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    proxy_get_json(&state, &format!("/v1/agent/sessions/{id}")).await
}

/// JSON GET passthrough helper for small daemon endpoints.
async fn proxy_get_json(state: &AppState, path: &str) -> Response {
    let url = format!("{}{path}", state.daemon_url.trim_end_matches('/'));
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// JSON POST passthrough helper for small daemon endpoints.
async fn proxy_post_json(state: &AppState, path: &str, body: Bytes) -> Response {
    let url = format!("{}{path}", state.daemon_url.trim_end_matches('/'));
    match state
        .http
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Reverse-proxy GET /v1/models (model picker catalogue).
async fn proxy_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    proxy_get_json(&state, "/v1/models").await
}

/// Reverse-proxy GET /v1/model (current selection).
async fn proxy_model_get(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    proxy_get_json(&state, "/v1/model").await
}

/// Reverse-proxy POST /v1/model (hot-swap the model).
async fn proxy_model_set(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    proxy_post_json(&state, "/v1/model", body).await
}

/// Reverse-proxy GET /v1/projects (project list for the picker).
async fn proxy_projects_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    proxy_get_json(&state, "/v1/projects").await
}

/// Reverse-proxy POST /v1/projects (create a project).
async fn proxy_projects_create(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> impl IntoResponse {
    proxy_post_json(&state, "/v1/projects", body).await
}

/// Reverse-proxy GET /v1/projects/{id} (project + its sessions).
async fn proxy_project_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    proxy_get_json(&state, &format!("/v1/projects/{id}")).await
}

/// Reverse-proxy PATCH /v1/projects/{id} (update name/config).
async fn proxy_project_patch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    proxy_method_json(
        &state,
        reqwest::Method::PATCH,
        &format!("/v1/projects/{id}"),
        body,
    )
    .await
}

/// Reverse-proxy DELETE /v1/projects/{id}.
async fn proxy_project_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    proxy_method_json(
        &state,
        reqwest::Method::DELETE,
        &format!("/v1/projects/{id}"),
        Bytes::new(),
    )
    .await
}

/// Reverse-proxy POST /v1/component/event (component interaction → daemon).
async fn proxy_component_event(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> impl IntoResponse {
    proxy_post_json(&state, "/v1/component/event", body).await
}

/// Reverse-proxy POST /v1/rooms/{room_id}/livekit-token.
async fn proxy_livekit_token(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    proxy_post_json(&state, &livekit_token_daemon_path(&room_id), body).await
}

/// JSON passthrough for an arbitrary method (PATCH/DELETE), mirroring
/// proxy_post_json but with the verb supplied.
async fn proxy_method_json(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    body: Bytes,
) -> Response {
    let url = format!("{}{path}", state.daemon_url.trim_end_matches('/'));
    match state
        .http
        .request(method, &url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Reverse-proxy POST /v1/requests/{id}/cancel (halt a running turn).
async fn proxy_cancel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    proxy_post_json(&state, &format!("/v1/requests/{id}/cancel"), Bytes::new()).await
}

fn livekit_token_daemon_path(room_id: &str) -> String {
    format!(
        "/v1/rooms/{}/livekit-token",
        percent_encode_path_segment(room_id)
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                write!(encoded, "%{byte:02X}").expect("writing to string should not fail");
            }
        }
    }
    encoded
}

/// The Quorum/Council observability deck (the Game Boy "longhouse" viewer).
/// Embedded into the binary so it's always reachable, independent of whether
/// the build step copied it into `dist/`. Served at `/ui/council` (canonical)
/// and `/longhouse.html` (legacy). The page connects to the existing
/// `/v1/agent/events` SSE stream and drives councils via `/v1/longhouse/*`,
/// both of which this proxy already exposes on the same origin.
const COUNCIL_DECK_HTML: &str = include_str!("../static/longhouse.html");

/// Serve the council deck page.
async fn council_deck() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        COUNCIL_DECK_HTML,
    )
}

/// Reverse-proxy the daemon's `/v1/longhouse/*` control endpoints (e.g.
/// `demo`, `convene`). Forwards method, path tail, query, and body so the
/// deck can drive a council through this same origin. The resulting council
/// events arrive on the existing `/v1/agent/events` SSE stream.
async fn proxy_longhouse(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
    req: Request,
) -> impl IntoResponse {
    let method = req.method().clone();
    let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!(
        "{}/v1/longhouse/{rest}{q}",
        state.daemon_url.trim_end_matches('/')
    );
    // buffer the (small) body so we can forward it on POST
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let builder = if method == axum::http::Method::POST {
        state
            .http
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
    } else {
        state.http.get(&url)
    };
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (StatusCode::BAD_GATEWAY, format!("daemon unreachable: {err}")).into_response(),
    }
}

/// Reverse-proxy the daemon's persistent-rooms API (`/v1/rooms/persistent`
/// and everything under it). Mirrors `proxy_longhouse` — forwards the method,
/// full path, query string, and body — but also handles DELETE (leave a room:
/// `DELETE /v1/rooms/persistent/{key}/participants/{id}`). One handler serves
/// the whole subtree: list (GET) + create (POST) on the bare path, plus room
/// get (GET), join (POST), leave (DELETE), post-message (POST) and transcript
/// (GET, `?after_seq=`) on the wildcard paths. We reconstruct the daemon path
/// from the incoming request URI rather than a captured tail so the same
/// handler covers both the literal and the wildcard routes verbatim.
async fn proxy_rooms_persistent(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> impl IntoResponse {
    let method = req.method().clone();
    // The path is always under /v1/rooms/persistent (the only routes wired to
    // this handler); forward it unchanged, with the query string preserved so
    // the transcript tail's ?after_seq= reaches the daemon.
    let path = req.uri().path().to_string();
    let q = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("{}{path}{q}", state.daemon_url.trim_end_matches('/'));
    // buffer the (small) body so we can forward it on POST/DELETE
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let builder = if method == axum::http::Method::GET {
        state.http.get(&url)
    } else {
        state
            .http
            .request(method, &url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
    };
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(err) => (StatusCode::BAD_GATEWAY, format!("daemon unreachable: {err}")).into_response(),
    }
}

/// Reverse-proxy the daemon's CONTROL stream `GET /v1/events` (OCEAN-135).
/// This is the SSE channel that carries `permission_request` cards. Mirrors
/// `proxy_events` exactly — streams the upstream body straight through and
/// forwards the full query string (e.g. ?session_id=) — but for the
/// control-plane path. Without it the web UI's control subscription 404'd and
/// permission cards never reached the phone, hanging every gated turn.
async fn proxy_control_events(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> impl IntoResponse {
    let q = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("{}/v1/events{q}", state.daemon_url.trim_end_matches('/'));
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let stream = resp.bytes_stream();
            let body = axum::body::Body::from_stream(stream);
            (
                status,
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                body,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// Reverse-proxy POST /v1/permissions/{id}/decision (OCEAN-136). The web UI
/// answers a permission prompt here (Allow/Deny); the body matches the
/// daemon's decision payload. Forwards the {id} + body so the decision reaches
/// the daemon — without it Allow/Deny 404'd and the gated turn stayed stuck.
async fn proxy_permission_decision(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    proxy_post_json(&state, &format!("/v1/permissions/{id}/decision"), body).await
}

/// Reverse-proxy the daemon's SSE event stream. We stream the upstream body
/// straight through so deltas arrive in real time.
async fn proxy_events(State(state): State<Arc<AppState>>, req: Request) -> impl IntoResponse {
    // Preserve ?session_id= query string — scopes SSE to one session per
    // OCEAN_ECOSYSTEM_CONTRACT.md. Do not strip. The full upstream query is
    // forwarded verbatim so session_id (and any other params like ?all=1)
    // reaches the daemon and the stream stays scoped to the caller's session.
    let q = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!(
        "{}/v1/agent/events{q}",
        state.daemon_url.trim_end_matches('/')
    );
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            // Pipe the upstream byte stream into the response body unchanged.
            let stream = resp.bytes_stream();
            let body = axum::body::Body::from_stream(stream);
            (
                status,
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                body,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("daemon unreachable: {err}"),
        )
            .into_response(),
    }
}

/// POST /api/stt — forward raw audio bytes to xAI as multipart, return `{ok, text}`.
async fn stt(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    let Some(key) = state.xai_key.as_deref() else {
        return Json(json!({ "ok": false, "error": "xAI key not configured" })).into_response();
    };

    if body.is_empty() {
        return Json(json!({ "ok": false, "error": "empty audio body" })).into_response();
    }

    let part = match reqwest::multipart::Part::bytes(body.to_vec())
        .file_name("clip.webm")
        .mime_str("application/octet-stream")
    {
        Ok(part) => part,
        Err(err) => {
            return Json(json!({ "ok": false, "error": format!("multipart: {err}") }))
                .into_response();
        }
    };

    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", "grok-stt")
        .text("language", "en")
        .text("response_format", "json");

    let resp = state
        .http
        .post(XAI_STT_URL)
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await;

    let resp = match resp {
        Ok(resp) => resp,
        Err(err) => {
            tracing::error!(error = %err, "stt request failed");
            return Json(json!({ "ok": false, "error": format!("stt request failed: {err}") }))
                .into_response();
        }
    };

    let status = resp.status();
    let payload: Value = match resp.json().await {
        Ok(payload) => payload,
        Err(err) => {
            return Json(json!({ "ok": false, "error": format!("stt decode failed: {err}") }))
                .into_response();
        }
    };

    if !status.is_success() {
        tracing::error!(%status, ?payload, "stt upstream error");
        return Json(json!({ "ok": false, "error": "stt_failed", "detail": payload }))
            .into_response();
    }

    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    Json(json!({ "ok": true, "text": text })).into_response()
}

#[derive(Deserialize)]
struct TtsRequest {
    text: String,
}

/// POST /api/tts — forward `{text}` to xAI, stream mp3 bytes back to the browser.
async fn tts(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TtsRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let key = state.xai_key.as_deref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "xAI key not configured".to_string(),
    ))?;

    let text = req.text.trim();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "text required".to_string()));
    }

    let resp = state
        .http
        .post(XAI_TTS_URL)
        .bearer_auth(key)
        .json(&json!({
            "model": "grok-tts",
            "text": text,
            "voice": state.voice_profile,
            "language": "en",
            "response_format": "mp3",
        }))
        .send()
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "tts request failed");
            (
                StatusCode::BAD_GATEWAY,
                format!("tts request failed: {err}"),
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        tracing::error!(%status, %detail, "tts upstream error");
        let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return Err((code, format!("tts_failed: {detail}")));
    }

    let audio = resp
        .bytes()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, format!("tts read failed: {err}")))?;

    Ok(([(header::CONTENT_TYPE, "audio/mpeg")], audio))
}

#[cfg(test)]
mod tests {
    use super::{
        config_payload, livekit_token_daemon_path, percent_encode_path_segment, wasm_headers,
        AppState, WASM_CACHE_CONTROL,
    };
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt; // for `oneshot`

    /// Build a router that returns a tiny body for any path, wrapped in the
    /// `wasm_headers` layer, so we can assert the layer's header rewriting per
    /// request path without touching the real ServeDir.
    fn wasm_test_router() -> Router {
        Router::new()
            .fallback(get(|| async {
                // Simulate ServeDir's default: a 200 with some other content-type.
                (
                    [(header::CONTENT_TYPE, "application/octet-stream")],
                    Body::from(vec![0x00, 0x61, 0x73, 0x6d]),
                )
            }))
            .layer(middleware::from_fn(wasm_headers))
    }

    #[tokio::test]
    async fn wasm_response_gets_application_wasm_and_no_transform() {
        let resp = wasm_test_router()
            .oneshot(
                Request::builder()
                    .uri("/ocean-surface-ui-abc123.wasm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/wasm"
        );
        let cc = resp.headers().get(header::CACHE_CONTROL).unwrap();
        assert_eq!(cc, WASM_CACHE_CONTROL);
        // The Cloudflare-compression escape hatch must be present.
        assert!(cc.to_str().unwrap().contains("no-transform"));
    }

    #[tokio::test]
    async fn non_wasm_response_is_left_untouched() {
        let resp = wasm_test_router()
            .oneshot(
                Request::builder()
                    .uri("/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        // Untouched: the layer must not rewrite non-wasm content types or add
        // the immutable cache policy to e.g. index.html.
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        assert!(resp.headers().get(header::CACHE_CONTROL).is_none());
    }

    #[test]
    fn livekit_token_proxy_path_preserves_room_id_as_single_segment() {
        assert_eq!(
            livekit_token_daemon_path("project:surface-demo"),
            "/v1/rooms/project%3Asurface-demo/livekit-token"
        );
        assert_eq!(
            livekit_token_daemon_path("project/surface demo"),
            "/v1/rooms/project%2Fsurface%20demo/livekit-token"
        );
    }

    #[test]
    fn path_segment_encoder_leaves_safe_url_bytes_unescaped() {
        assert_eq!(
            percent_encode_path_segment("abc-XYZ_123.~"),
            "abc-XYZ_123.~"
        );
    }

    #[test]
    fn config_payload_includes_surface_collaboration_defaults() {
        let state = AppState {
            http: reqwest::Client::new(),
            xai_key: Some("configured".to_string()),
            voice_profile: "leo".to_string(),
            daemon_url: "http://127.0.0.1:4780".to_string(),
            default_livekit_room_id: "project/surface demo".to_string(),
            tldraw_sync_uri: Some("http://127.0.0.1:5858/connect".to_string()),
            maps_key: Some("maps".to_string()),
            maps_map_id: "DEMO_MAP_ID".to_string(),
            basic_auth: None,
        };

        let payload = config_payload(&state);

        assert_eq!(payload["daemon_url"], "");
        assert_eq!(payload["has_auth"], true);
        assert_eq!(payload["livekit_room_id"], "project/surface demo");
        assert_eq!(
            payload["livekit_token_path"],
            "/v1/rooms/project%2Fsurface%20demo/livekit-token"
        );
        assert_eq!(payload["tldraw_sync_uri"], "http://127.0.0.1:5858/connect");
        assert_eq!(
            payload["surface"]["livekit_token_path"],
            "/v1/rooms/project%2Fsurface%20demo/livekit-token"
        );
    }
}
