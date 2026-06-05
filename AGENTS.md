# Ocean Surface Agent Guide

Ocean Surface is the product-surface repo. It contains the GPUI desktop app,
Leptos web/PWA, Chrome extension surface, local proxy, voice UI, and canvas
experiments.

The sibling repo `../ocean-os` owns runtime authority: daemon, agent loop,
tools, providers, permissions, projects, workspaces, sessions, and events.

Do not put provider calls, agent reasoning, session storage, permission policy,
or tool execution authority in this repo. Surface code should render state,
collect intent, attach to sessions, and call the daemon.

## Current Build Focus

The active native app is:

```sh
cargo run -p ocean-gui --bin ocean-gui
```

The GPUI app is the native desktop surface. It is not a Tauri wrapper and it is
not a Papyrus note app. Old Papyrus/vault/editor work may be reused only as
local UI/editor source material.

The important GPUI direction is the real collaboration surface:

- GPUI native chrome and agent transcript.
- wry-hosted tldraw canvas pane for the multiplayer canvas.
- LiveKit Rust client for audio/video/data/RPC participation.
- Ocean daemon as the session/runtime authority.
- Canvas ledger/state injected into turns as surface context.

Use `docs/OCEAN_GPUI_CANVAS_LIVEKIT_SPEC.md` as the GPUI collaboration anchor.

## Session Contract

The ecosystem invariant is:

```text
Project -> Workspace -> Session -> Turns -> Events
Surface -> Session
```

First-party surfaces must create or choose a session before posting a turn:

```text
POST /v1/agent/sessions
GET  /v1/agent/events?session_id=<id>
POST /v1/agent/turns { session_id, prompt, cwd, project_id?, client_type }
```

Rules:

- Never open a product transcript on the global `/v1/agent/events` stream.
- Never adopt the active session from `SessionCreated` or `TurnStarted` on a
  global stream.
- Cross-surface sharing is explicit: two surfaces attach to the same
  `session_id`.
- Different sessions on different surfaces must not blend, switch each other,
  or race to become the active transcript.
- `client_type` only describes the surface medium (`surface-gpui`,
  `surface-web`, `surface-extension`). It is not a session id or workspace id.

## Workspace Map

| Path | Role |
|---|---|
| `crates/ocean-gui/` | GPUI native desktop app and tldraw canvas host |
| `crates/ocean-gui/canvas-web/` | web bundle loaded into the GPUI canvas webview |
| `crates/ocean-surface-ui/` | Leptos WASM web/PWA/extension UI |
| `crates/ocean-surface-proxy/` | axum proxy for web bundle, STT/TTS, config, daemon reverse proxy |
| `extension/` | Chrome extension wrapper around the Leptos surface |
| `legacy-voice/` | reference voice code only; do not build new architecture here |

## Build / Check

```sh
cargo check -p ocean-gui
cargo test -p ocean-gui
cargo check -p ocean-surface-ui --target wasm32-unknown-unknown
cargo check -p ocean-surface-proxy
```

For local web/proxy work:

```sh
./run-surface.sh
trunk serve --open
```

The daemon must be running from `../ocean-os` for live agent behavior.
