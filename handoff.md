# Ocean — session handoff (2026-05-31)

## Current overlay (2026-06-04)

The active native surface is `crates/ocean-gui`, not a Tauri shell. The GPUI
collaboration spec is `docs/OCEAN_GPUI_CANVAS_LIVEKIT_SPEC.md`.

Session semantics changed after this handoff was written:

- Product surfaces create or choose a session before posting a turn.
- Product surfaces subscribe to `GET /v1/agent/events?session_id=<id>`.
- Product surfaces submit turns with `session_id`.
- Global SSE session adoption is legacy/debug behavior only and must not drive
  GPUI/web/extension transcript state.

The previous "SSE session-id filter + adoption" note below is historical. Do
not reimplement adoption.

Read this first, then `ocean-surface/CLAUDE.md` and `ocean-os/CLAUDE.md`.

## The system in one paragraph
Ocean is **two repos, one system**. `ocean-os` (`../ocean-os`) is the **brain**: a Rust
daemon that owns the agent loop, tools, provider calls, **sessions**, permissions, and the
event bus. `ocean-surface` (here) is the **client face**: a Rust+Leptos CSR/WASM app served
as a PWA, plus a small axum proxy that holds the xAI/Maps keys and reverse-proxies the
daemon. Clients are disposable; sessions live in the daemon only. They talk over
`POST /v1/agent/turns` + `GET /v1/agent/events` (SSE) + `/v1/model[s]` + `/v1/requests/{id}/cancel`.

## Current run state (as of this handoff)
- **Live stack** (public, behind Cloudflare tunnel `ocean.agentsworld.org`):
  - daemon on `127.0.0.1:4780`, currently model **gpt-5.5 (Codex/OAuth)**
  - proxy on `0.0.0.0:8790`, **HTTP Basic auth ON** (`smathdaddy` / `***REMOVED-CREDENTIAL***`)
  - tunnel: cloudflared config `~/.cloudflared/config.yml`, hostname `ocean.agentsworld.org` → `:8790`
- **Isolated TEST stack** (for dev, never tunneled): daemon `4781` (own config dir `/tmp/ocean-test`), proxy `8791` **auth OFF**. Use this for browser testing so you never touch the live one.
- Restart pattern (load keys first): `set -a; source ~/.config/ocean-rs/tools.env; set +a` then launch.
- **Do not** broad-`pkill ocean-surface-proxy` — it kills BOTH proxies. Target by port: `kill $(lsof -nP -iTCP:8791 -sTCP:LISTEN -t)`.

## Credentials
All agent/tool keys are in **`~/.config/ocean-rs/tools.env`** (mode 600, gitignored): Google Maps + API, Brave Search, Linear, Replicate, Slack bot+channel, Cloudflare. Provider model keys (DeepSeek, Codex OAuth, Kimi) are in **`~/.config/ocean-rs/auth.json`**. ⚠️ **DeepSeek balance is exhausted** (402 Insufficient Balance) — that's why we run gpt-5.5/Kimi. Top up or stay off DeepSeek.

## What shipped this session (all committed + pushed to `main` on both repos)

**ocean-os** (latest → older): `9c02772` docs: all 17 component kinds · `434df08` per-session lock + strict resume-vs-create · `0ba8044` map + video kinds · `e4ce352` /v1/models + model-on-TurnStarted + cancellable turns · `9d0adfa` session_id on all events · `df87bb0` real token usage on TurnFinished · `6d87aef` 9 rich component kinds · `31ecc89` Codex provider · (earlier) burn-fix + MiniMax/Kimi.

**ocean-surface** (latest → older): `6c858e4` auto-recover stale session · `6f11756` map+video components (Ocean-skinned) · `e1ffa81`/`9c2453e` SSE session-id filter + adoption · `3ca33bc`/`2002570` PWA service-worker fixes · `f995563` model picker + halt button.

Highlights:
- **18 agent-renderable components** now (was 6 working). New this session: `map` (live Google Maps + Places UI Kit, custom Map ID `75cd6c60a814ddab5a970623`, marker/place/search modes) and `video` (TikTok/IG/YouTube/Vimeo/direct-file embeds). All documented in `ocean-os/docs/AGENT_RENDER_PROTOCOL.md`.
- **Token-burn root cause fixed**: quadratic context replay (full transcript resent every round + reloaded every turn, uncapped tool output). Three caps in `ocean-runtime/src/agent_loop.rs` + `ocean-agent` (trim-to-window, 32KB tool-output cap, 200-msg session cap). This drained the DeepSeek balance before the fix.
- **Session foundation hardened** (per the Goose audit): per-session turn lock (no concurrent-turn corruption) + strict resume-vs-create (`create_if_missing` flag; unknown session id errors instead of silently forking) + surface auto-recovery (clears stale id, retries fresh, invisible to user). Tested + verified live.
- **UI**: token meter in header, model dropdown (hot-swap, no restart), halt/Stop button (cancels in-flight turn), stacked/collapsing tool calls.

## Uncommitted / in-flight work — NOT mine, leave alone unless asked
- `ocean-surface/index.html` — the **agent's own** map-marker fix (uses `innerMap` + `PinElement` numbered pins). Looks correct; uncommitted.
- `ocean-surface/crates/ocean-surface-ui/src/{tts.rs,voice.rs}` — a mobile-Safari **voice/TTS prime() fix** (primes audio on tap so iOS allows playback). Compiles; uncommitted; good change.
- `ocean-surface/AGENTS.md`, `ocean-os/{CLAUDE.md,README.md,ROADMAP.md,docs/OCEAN_SELF_IMPROVEMENT_PLAN.md,handoff.md}` — docs/scratch, someone else's WIP. Untracked/uncommitted.
- Screenshots (`*.png`) in surface root are gitignored test artifacts.

## The reference doc you'll want
**`ocean-os/docs/GOOSE_COMPARISON_AND_EXTENSIONS_GUIDANCE.md`** — a detailed audit of Ocean vs Goose with the extensions roadmap. It's the basis for what's next.

## What's next (in priority order, per the audit + operator)
1. **Extensions / MCP layer** — the big unlock. Build Ocean as an **MCP client** + a `CapabilityRegistry` so tools/skills load dynamically, instead of hardcoding each. Then the keys in `tools.env` (Brave, Slack, Replicate, Linear, Cloudflare) plug in as MCP servers. The agent loop should consume `registry.tools_for_session(...)` not a fixed `default_tools()`. **Do NOT wire those keys as one-off native Rust tools** — the audit explicitly warns that's throwaway.
2. **Pinned widgets** — a docked/persistent widget zone in the surface chrome (map/player/metrics that stay up across turns, outside the chat scroll). Add a `placement: "inline" | "pinned"` concept to ComponentRender + a pinned registry rendered in a side rail/dock. Independent UI track.
3. More component kinds as wanted (audio player, sortable table, creator card, calendar).
4. (Optional, longer) ACP adapter as a second front door; subagent/delegation.

## Foundation is solid
The Goose audit's session-foundation priorities are all closed (compile-green, request registration, cancellation, permission wiring, per-session lock, strict semantics). Build extensions on top with confidence.

## Build/run quick ref
- Surface UI is wasm-only: `cargo build -p ocean-surface-ui --target wasm32-unknown-unknown`; bundle: `trunk build --release`.
- Daemon: `cargo build -p ocean-daemon --release`; tests: `cargo test -p ocean-agent`.
- The agent only knows new capabilities after the **daemon is rebuilt AND restarted** — stale binary = stale tool list (this caused a "only knows 6 components" scare; it was just an un-restarted daemon).
