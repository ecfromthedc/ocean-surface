# Ocean Surface

The client face of [Ocean OS](https://github.com/Risingtides-dev/ocean-os). One Rust + Leptos app, built once, shipped three ways:

| Target          | How                                  | Why                                                        |
| --------------- | ------------------------------------ | ---------------------------------------------------------- |
| Browser PWA     | `trunk serve`                        | iPhone "Add to Home Screen", desktop browser, anywhere     |
| Native macOS    | `cargo tauri dev` (added in Phase 4) | Menubar icon, dock, system audio, push notifications       |
| Native iOS      | `cargo tauri ios dev` (Phase 4)      | TestFlight builds, real-device performance                 |

All three are thin clients over `ocean-daemon`. None hold agent logic, provider credentials, or sessions. They speak the daemon's product agent API:

```
POST /v1/agent/turns   { prompt, cwd, session_id?, guidance? }
GET  /v1/agent/events  (SSE stream of AgentTurnEvent)
```

## Workspace

| Path                            | Role                                                                 |
| ------------------------------- | -------------------------------------------------------------------- |
| `crates/ocean-surface-ui/`      | Leptos UI (CSR/WASM). Same code runs in browser and Tauri WebView.   |
| `crates/ocean-surface-proxy/`   | axum service: holds xAI key for STT/TTS, serves the WASM bundle.     |
| `crates/ocean-surface-app/`     | Tauri shell (added in Phase 4). Wraps the UI as a native .app.       |
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

### Live-reload dev (UI work)

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

- ✅ Phase 1 — Workspace scaffold (Leptos UI crate + proxy crate, Trunk config)
- ✅ Phase 2 — SSE wired end-to-end (assistant streams live in the browser)
- ✅ Phase 3 — Chat surface: markdown, thinking pills, tool chips, mobile-first style
- ⬜ Phase 4 — Tauri shell for macOS + iOS
- ✅ Phase 5 — xAI STT/TTS proxy (`/api/stt`, `/api/tts`, Leo voice)
- ✅ Phase 6 — Voice surface: push-to-talk orb, mic capture, mp3 playback

Phase 4 (native Tauri shell) is the only deferred phase; the browser PWA is feature-complete.

## Provenance

The voice work in `legacy-voice/` was originally proposed as PR #22 in `ocean-os`. Extracted here so the runtime repo stays Rust-only.
