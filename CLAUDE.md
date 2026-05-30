# Ocean Surface — read this first

**This repo is one half of a two-repo system. The other half is `ocean-os`.**

| Repo | What it is | Where |
|---|---|---|
| **ocean-surface** (you are here) | The **client face**: one Rust + Leptos app shipped as browser PWA + voice (later Tauri native macOS/iOS). A thin steering shell. | `../ocean-surface` |
| **ocean-os** | The **runtime + daemon + TUI**. Owns the agent loop, tools, provider calls, **sessions**, permissions, events. The brain this repo talks to. | `../ocean-os` (also cloned locally) |

This repo holds **no agent logic, no provider credentials, no sessions.** It is a face over `ocean-daemon` (which lives in `ocean-os`). If something about agent behavior, sessions, or tools is broken, the cause is very often **over in `ocean-os`**, not here — go read `../ocean-os` before concluding the bug is surface-side.

## How it talks to the daemon

```
POST {daemon}/v1/agent/turns    { prompt, cwd, session_id? }
GET  {daemon}/v1/agent/events   (SSE stream of AgentTurnEvent)
POST {daemon}/v1/component/event { session_id, component_id, event }
```

Daemon URL defaults to `http://127.0.0.1:4780` (override `OCEAN_DAEMON_URL`). The daemon must be running — start it in `../ocean-os`: `cargo run -p ocean-daemon --release`.

**Sessions live in the daemon, not here.** This surface only carries a `session_id` string: it adopts the id from the daemon's `SessionCreated` SSE event and replays it on every turn. So "lost session / chat reset mid-conversation" is almost always a **daemon-side** session bug in `ocean-os` (`crates/ocean-agent`), and it hits the TUI too — not a surface bug.

## Workspace

| Path | Role |
|---|---|
| `crates/ocean-surface-ui/` | Leptos UI (CSR/WASM). Same code runs in browser and Tauri WebView. Session/transcript rendering: `src/daemon.rs`, `src/transcript.rs`, `src/components.rs` |
| `crates/ocean-surface-proxy/` | axum service: holds the xAI key for STT/TTS, serves the WASM bundle |
| `crates/ocean-surface-app/` | Tauri shell (Phase 4, not yet added) |
| `legacy-voice/` | Reference JS voice client; deleted once ported |

## Build / run

The UI crate is **wasm-only** — `cargo build` (no `-p`) builds only the native proxy by design. To build the UI:

```bash
cargo build -p ocean-surface-ui --target wasm32-unknown-unknown
./run-surface.sh        # builds wasm + serves UI and xAI proxy on :8790
./smoke.sh              # health + chat round-trip + voice; 5/5 green = all paths work
trunk serve --open      # live-reload dev (UI only; voice needs run-surface.sh)
```

## Don't kill a running daemon

The daemon (in `ocean-os`) is often live while the operator works. **Do not restart or kill it** unless told to — a surprise restart drops the in-flight session.

## More context

See `README.md` for the full dev loop, auth resolution order, and roadmap.
