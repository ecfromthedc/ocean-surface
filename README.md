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

Browser, talking to a local daemon:

```sh
# In ../ocean-os:
cargo run -p ocean-daemon --release

# In this repo:
trunk serve --open
# → http://localhost:8080
```

Same browser, talking to a daemon on another tailnet host:

```sh
OCEAN_DAEMON_URL=http://mac-mini.tailnet:4780 trunk serve --open
```

Production-ish (one binary, serves the bundle + will proxy STT/TTS):

```sh
trunk build --release
cargo run -p ocean-surface-proxy --release
# → http://0.0.0.0:8790
```

## Roadmap

- ✅ Phase 1 — Workspace scaffold (Leptos UI crate + proxy crate, Trunk config)
- ⬜ Phase 2 — SSE wired end-to-end (you can see assistant streaming in browser)
- ⬜ Phase 3 — Chat surface polish: markdown, thinking pills, tool chips, mobile-first style
- ⬜ Phase 4 — Tauri shell for macOS + iOS
- ⬜ Phase 5 — xAI STT/TTS proxy (Leo voice profile)
- ⬜ Phase 6 — Voice surface: push-to-talk orb, mic capture, audio playback

## Provenance

The voice work in `legacy-voice/` was originally proposed as PR #22 in `ocean-os`. Extracted here so the runtime repo stays Rust-only.
