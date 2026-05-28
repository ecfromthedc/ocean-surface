# Ocean Surface

Client surfaces for the [Ocean OS](https://github.com/Risingtides-dev/ocean-os) runtime.

Ocean OS owns the brain — the Rust `ocean-daemon` runs the agent loop, talks to providers, executes tools, persists sessions. Ocean Surface is everything you *touch* it through that isn't the TUI: voice, web, mobile.

Every surface is a **thin steering client**. None of them hold agent logic, provider credentials, or sessions. They all speak the same product-shaped agent API:

```
POST /v1/agent/turns   { prompt, cwd, session_id?, guidance? }
GET  /v1/agent/events  (SSE stream of AgentTurnEvent)
```

## Packages

| Path     | What                                                                                       | Status      |
|----------|--------------------------------------------------------------------------------------------|-------------|
| `voice/` | **ocean-voice** — web PWA + desktop daemon + CLI. xAI Grok for STT/TTS (voice profile: Leo). | Imported    |
| `webui/` | **ocean-webui** — Leptos/WASM browser client. Sibling to voice, same daemon API.           | Planned     |

## Why a separate repo

`ocean-os` is a pure Rust monorepo. The surfaces are a mix of JavaScript (`voice/`) and WASM/Leptos (planned `webui/`), so they live here instead of polluting the runtime tree. They consume `ocean-os` as a deployed service over HTTP/SSE — no source-level coupling.

## Running against a local daemon

Start the daemon from the `ocean-os` repo:

```sh
cd ../ocean-os
cargo run -p ocean-daemon --release
```

Then run any surface against it (see each package's README for details).

## Provenance

`voice/` was originally proposed as `feat/ocean-voice` (PR #22) in `ocean-os`. Extracted here to keep the runtime repo Rust-only.
