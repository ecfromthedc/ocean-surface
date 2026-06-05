# Ocean Surface

The client face of [Ocean OS](https://github.com/Risingtides-dev/ocean-os).
This repo holds the GPUI desktop app, Leptos web/PWA, Chrome extension surface,
local proxy, voice UI, and canvas work.

| Target | How | Why |
|---|---|---|
| GPUI desktop | `cargo run -p ocean-gui --bin ocean-gui` | native desktop collaboration surface, agent transcript, tldraw canvas host, LiveKit controls |
| Browser PWA | `trunk serve` or `./run-surface.sh` | desktop/mobile browser access over the daemon API |
| Chrome extension | `extension/` wrapper | browser-side panel with explicit `surface-extension` context |
| Web proxy | `cargo run -p ocean-surface-proxy` | serves web bundle, config, STT/TTS, and daemon reverse proxy |

All targets are thin clients over `ocean-daemon`. None hold agent logic,
provider credentials, or session authority. They speak the daemon's product
agent API:

```
POST /v1/agent/sessions
GET  /v1/agent/events?session_id=<id>
POST /v1/agent/turns   { prompt, cwd, session_id, project_id?, client_type }
```

Surfaces create or choose a session before posting turns. They do not adopt a
session from global SSE. Cross-surface sharing is explicit: attach both
surfaces to the same `session_id`.

## Workspace

| Path                            | Role                                                                 |
| ------------------------------- | -------------------------------------------------------------------- |
| `crates/ocean-gui/`             | GPUI native desktop app and tldraw canvas host.                      |
| `crates/ocean-gui/canvas-web/`  | tldraw/web bundle loaded by the GPUI canvas host.                    |
| `crates/ocean-surface-ui/`      | Leptos UI (CSR/WASM) for web/PWA/extension.                          |
| `crates/ocean-surface-proxy/`   | axum service: holds xAI key for STT/TTS, serves the WASM bundle.     |
| `extension/`                    | Chrome extension wrapper around the Leptos surface.                  |
| `legacy-voice/`                 | Reference: the JS voice client (PR #22). Deleted once ported.        |

## Dev loop

### One command (recommended)

The daemon must be running (in `../ocean-os`: `cargo run -p ocean-daemon --release`). Then:

```sh
# Preconfigure voice once — drop your xAI key here (gitignored):
mkdir -p ~/.config/ocean-surface && printf '%s' "sk-YOUR-XAI-KEY" > ~/.config/ocean-surface/xai.key

# Build the wasm bundle + serve it and the xAI proxy from one binary:
./run-surface.sh
# → open http://<this-host>:8790  (works on a phone via the tailnet IP)
```

`run-surface.sh` binds `0.0.0.0:8790` by default. Override with
`OCEAN_SURFACE_BIND`, `OCEAN_DAEMON_URL`, `OCEAN_VOICE_PROFILE`.

### Verify before you open the browser

```sh
./smoke.sh        # health, /api/config, chat round-trip, + live STT/TTS if a key is set
```

5/5 green means every wired path works; then the browser check is just UI/mic confirmation.

### GPUI desktop work

```sh
cargo run -p ocean-gui --bin ocean-gui
cargo check -p ocean-gui
```

The GPUI collaboration direction is documented in
[`docs/OCEAN_GPUI_CANVAS_LIVEKIT_SPEC.md`](docs/OCEAN_GPUI_CANVAS_LIVEKIT_SPEC.md).

### Live-reload web dev

```sh
trunk serve --open                                    # → http://localhost:8080
OCEAN_DAEMON_URL=http://mac-mini.tailnet:4780 trunk serve --open   # remote daemon
```

Note: `trunk serve` serves the UI but NOT the proxy, so voice (`/api/stt`,
`/api/tts`) and `/api/config` need `run-surface.sh`. Text chat works under
both.

## Auth — preconfigured

The proxy holds the xAI key server-side (the browser never sees it) and resolves it in order:

1. env `XAI_API_KEY`
2. `~/.config/ocean-surface/xai.key` (override: `OCEAN_SURFACE_KEY_FILE`) — set once, every launch picks it up
3. `~/.pi/agent/settings.json` → `.xai.apiKey` (legacy fallback)

`GET /api/config` reports `has_auth`; the UI fetches it on boot so no URL or credential is ever typed in the browser.

## Roadmap

- Done: web/PWA chat, SSE transcript, model picker, session picker, proxy,
  voice STT/TTS, Chrome extension bootstrap.
- In progress: GPUI native app, explicit session scoping, tldraw canvas host,
  canvas ledger, LiveKit presence controls.
- Next: reliable GPUI canvas IPC, tldraw render commands, LiveKit mic/camera
  participation, and surface-state injection into agent turns.

## Provenance

The voice work in `legacy-voice/` was originally proposed as PR #22 in `ocean-os`. Extracted here so the runtime repo stays Rust-only.
