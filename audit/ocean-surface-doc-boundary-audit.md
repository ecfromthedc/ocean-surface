# Ocean Surface documentation boundary audit

Scope: documentation/config audit only. I inspected the top-level README/CLAUDE/AGENTS/handoff, `docs/`, extension manifest/docs, visible Markdown files, and Cargo/TOML crate maps. No source/docs were edited; this file is the only audit artifact.

Intended boundary under audit:

- `ocean-surface` / `ocean-surfaces` owns all user-facing surfaces: GUI, TUI, PWA/web, voice, extension, and Ocean Browser as another surface/product.
- `ocean-os` owns daemon/runtime/agent brain/tools/sessions/protocol/providers/backend only.

## Findings

### F-01 — Root agent docs assign TUI ownership to `ocean-os`

- Paths / conflicting claims:
  - `CLAUDE.md:7-8` and `AGENTS.md:7-8`:
    > `ocean-surface` ... `The client face: one Rust + Leptos app shipped as browser PWA + voice (later Tauri native macOS/iOS). A thin steering shell.`
    > `ocean-os` ... `The runtime + daemon + TUI. Owns the agent loop, tools, provider calls, sessions, permissions, events.`
  - `CLAUDE.md:22` and `AGENTS.md:22`:
    > `... it hits the TUI too — not a surface bug.`
- Severity: **High**
- Why it conflicts: the intended boundary says TUI is a user-facing surface owned by `ocean-surface` / `ocean-surfaces`, not by `ocean-os`.
- Recommended wording:
  > `ocean-surface` / `ocean-surfaces`: the user-facing surfaces workspace: GUI, TUI, PWA/web, voice, browser extension, and Ocean Browser. These are steering clients over Ocean Current.
  >
  > `ocean-os`: daemon/runtime/backend: agent loop, tools, sessions, protocol, providers, permissions, event streams, and backend services.
  >
  > Sessions live in the daemon. A session bug can affect any surface (GUI/TUI/web/voice/extension), but TUI ownership belongs with the surfaces workspace.
- Move/rename action: if TUI code still physically lives in `ocean-os`, document it as transitional and plan a move/rename into the surfaces workspace.

### F-02 — Top-level README scopes this repo as only one Leptos app shipped three ways

- Paths / conflicting claims:
  - `README.md:3-9`:
    > `One Rust + Leptos app, built once, shipped three ways:`
    > `Browser PWA` / `Native macOS` / `Native iOS`
  - `README.md:20-25` workspace table lists only `ocean-surface-ui`, `ocean-surface-proxy`, `ocean-surface-app`, and `legacy-voice`.
- Severity: **High**
- Why it conflicts: it omits the intended surface scope: GUI, TUI, extension, and Ocean Browser. It also conflicts with the actual workspace including `crates/ocean-gui`.
- Recommended wording:
  > Ocean Surface(s) is the user-facing surfaces workspace for Ocean: web/PWA, native GUI, TUI, voice, browser extension, and Ocean Browser. These surfaces do not own agent brain/runtime state; they render, capture input, and steer `ocean-os` over the product protocol.
- Recommended workspace table additions:
  > `crates/ocean-gui/` — native desktop GUI surface.
  >
  > `extension/` — browser extension side-panel surface.
  >
  > `legacy-voice/` or successor voice crate — voice surface / migration reference.
  >
  > `crates/ocean-tui/` — planned/owned TUI surface, or note if transitional elsewhere.
  >
  > `crates/ocean-browser/` / `ocean-browser/` — planned Ocean Browser product surface, if/when created.

### F-03 — README and root agent docs put provider credentials/provider backend in `ocean-surface-proxy`

- Paths / conflicting claims:
  - `README.md:11`:
    > `None hold agent logic, provider credentials, or sessions.`
  - `README.md:23`:
    > `crates/ocean-surface-proxy/ — axum service: holds xAI key for STT/TTS, serves the WASM bundle.`
  - `README.md:34-38`:
    > `Preconfigure voice once — drop your xAI key here ... ~/.config/ocean-surface/xai.key`
    > `Build the wasm bundle + serve it and the xAI proxy from one binary`
  - `README.md:66-70`:
    > `The proxy holds the xAI key server-side ... resolves it in order: env XAI_API_KEY ... ~/.config/ocean-surface/xai.key ... ~/.pi/agent/settings.json`
  - `CLAUDE.md:29`, `AGENTS.md:29`:
    > `crates/ocean-surface-proxy/ — axum service: holds the xAI key for STT/TTS, serves the WASM bundle`
- Severity: **High**
- Why it conflicts: provider credentials/provider calls/backend should be `ocean-os` responsibility. The docs also contradict themselves: line 11 says surfaces hold no provider credentials, later lines say the surface proxy holds xAI credentials.
- Recommended wording:
  > `crates/ocean-surface-proxy/` — temporary/dev web server for serving the built surface bundle and forwarding browser-safe requests. It must not be the canonical owner of provider credentials or provider calls.
  >
  > Speech/Maps/model provider credentials and backend calls are resolved by `ocean-os`. Surface calls daemon/backend protocol endpoints and never stores provider keys.
- Move/rename action: move canonical STT/TTS/Maps provider-key resolution docs to `ocean-os`; if the proxy remains, rename/docs-scope it as `ocean-surface-dev-server` or `surface-web-server` rather than a provider proxy.

### F-04 — README roadmap canonizes an xAI STT/TTS proxy as a surface phase

- Path / conflicting claim:
  - `README.md:80`:
    > `Phase 5 — xAI STT/TTS proxy (/api/stt, /api/tts, Leo voice)`
- Severity: **High**
- Why it conflicts: voice UX belongs to surface, but provider-backed STT/TTS service ownership belongs to `ocean-os` under the intended boundary.
- Recommended wording:
  > Phase 5 — Voice surface: mic capture, push-to-talk UX, playback, and calls to daemon-owned speech endpoints.
- Move/rename action: relocate `/api/stt` and `/api/tts` provider service documentation to `ocean-os`, or mark current surface proxy implementation as transitional only.

### F-05 — `handoff.md` places xAI/Maps keys and reverse-proxy/backend duties in surface, and includes live credentials

- Paths / conflicting claims:
  - `handoff.md:6-10`:
    > `ocean-surface ... is the client face: a Rust+Leptos CSR/WASM app served as a PWA, plus a small axum proxy that holds the xAI/Maps keys and reverse-proxies the daemon.`
  - `handoff.md:15-18`:
    > `proxy on 0.0.0.0:8790, HTTP Basic auth ON ... tunnel ... hostname ocean.agentsworld.org → :8790`
  - `handoff.md:23`:
    > `All agent/tool keys are in ~/.config/ocean-rs/tools.env ... Google Maps + API, Brave Search, Linear, Replicate, Slack bot+channel, Cloudflare. Provider model keys ...`
- Severity: **High**
- Why it conflicts: xAI/Maps/provider/backend/reverse-proxy duties are documented as part of the surface repo, not `ocean-os`. The file also exposes concrete Basic auth credentials, which should not live in repo docs regardless of boundary.
- Recommended wording:
  > `ocean-surface` serves user-facing UI bundles and surfaces. `ocean-os` owns provider/tool credentials, backend provider calls, protocol, daemon runtime, and secure deployment/backend configuration. Any web edge/proxy in this repo is temporary/dev-only unless explicitly approved.
- Move/rename action: move run-state/credential/deployment notes into a private ops runbook; replace secrets with placeholders; document the surface proxy as temporary if kept.

### F-06 — Legacy voice docs contradict provider-credential ownership

- Paths / conflicting claims:
  - `legacy-voice/README.md:7-9`:
    > `ocean-voice owns no agent logic, no provider credentials, and no sessions. The Ocean runtime owns all of that.`
  - `legacy-voice/README.md:40`:
    > `src/web-server.mjs — Web/PWA connector (:8790): serves the PWA, proxies xAI STT/TTS, forwards prompts.`
  - `legacy-voice/README.md:54-55`:
    > `xAI API key (XAI_API_KEY) for speech: browser STT, web TTS, and desktop TTS all use xAI Grok.`
  - `legacy-voice/README.md:97-99`:
    > `Credentials come from the environment (or, for the desktop daemon's xAI key, an optional XAI_SETTINGS_FILE). The Ocean runtime holds the model/provider keys.`
  - `legacy-voice/README.md:106-107`:
    > `xAI Grok is the only TTS engine ...`
- Severity: **High**
- Why it conflicts: the doc says voice owns no provider credentials, but then documents xAI key/config and xAI provider proxying inside the voice package.
- Recommended wording:
  > The voice surface owns microphone capture, voice UX, playback, and spoken-status presentation. STT/TTS provider credentials and provider calls are daemon/backend responsibilities in `ocean-os`; this surface calls backend speech endpoints. Any local xAI adapter here is legacy/transitional and not canonical.
- Move/rename action: migrate provider-key sections to `ocean-os` docs; rename `src/daemon.mjs` language to `voice helper` or `voice service` if it remains to avoid confusing it with Ocean Current.

### F-07 — Legacy voice docs position TUI/CLI as external peer clients without current repo ownership clarity

- Paths / conflicting claims:
  - `legacy-voice/README.md:3-5`:
    > `A voice surface ... in the same spirit as ocean-cli and ocean-tui.`
  - `legacy-voice/README.md:13-28` architecture diagram includes:
    > `Terminal ... cli.mjs ... ocean-daemon owns sessions, tools, provider calls, permissions`
- Severity: **Medium**
- Why it conflicts: the intended boundary says TUI is a user-facing surface owned by the surface(s) repo. The doc uses older product names and does not identify where TUI belongs now. CLI ownership is also undefined.
- Recommended wording:
  > Voice is one of the Ocean Surfaces alongside GUI, TUI, web/PWA, extension, and Ocean Browser. All are steering surfaces over Ocean Current. If a CLI remains, classify it explicitly as a developer/admin client or include it in the surfaces ownership matrix.
- Move/rename action: add/update a surfaces ownership matrix; document TUI migration if it remains in `ocean-os` code today.

### F-08 — Extension docs treat browser product scope as mostly `ocean-os` browser tooling, not a surface product

- Paths / conflicting claims:
  - `docs/ocean-extension-context.md:176-195`:
    > `Related existing browser plumbing in ocean-os`
    > `crates/ocean-browser — CDP wrapper and Chrome launch/attach logic.`
    > `crates/ocean-runtime/src/tools/browser — agent-facing tools ...`
    > `most browser automation plumbing exists already`
  - `docs/ocean-extension-manifesto.html:512`:
    > `Browser tools operate against the same Chrome session the user sees.`
  - `docs/ocean-extension-manifesto.html:570`:
    > `ocean-runtime — Agent brain + tools ... browser CDP driver, sessions, provider loop.`
- Severity: **Medium**
- Why it conflicts: `ocean-os` can own browser automation tools/protocol, but the intended boundary says Ocean Browser itself is a user-facing surface/product owned by surface(s). The docs do not distinguish runtime browser tools from the Ocean Browser product.
- Recommended wording:
  > `ocean-os` owns browser automation tools/adapters exposed to the agent. `ocean-surface` / `ocean-surfaces` owns browser-facing products: the Chrome extension and future Ocean Browser/Chromium surface. Runtime CDP drivers should be described as backend tooling, not the Ocean Browser product.
- Move/rename action: if `crates/ocean-browser` remains in `ocean-os`, consider renaming/docs-labeling it as `ocean-browser-tools` / CDP adapter to reserve `Ocean Browser` for the surface product.

### F-09 — Extension docs say Surface renders “sessions” without clarifying daemon ownership

- Path / conflicting claim:
  - `docs/ocean-extension-manifesto.html:568`:
    > `Leptos/WASM Surface ... Renders transcript, components, voice, sessions, and sends turns.`
- Severity: **Low**
- Why it conflicts: session UI can be surface-owned, but authoritative sessions are `ocean-os` backend/runtime. The wording can be read as surface-owned sessions.
- Recommended wording:
  > Renders transcript, components, voice controls, and session selectors/views; daemon-owned sessions remain in `ocean-os`.

### F-10 — Cargo workspace map includes GUI but top-level docs omit it; default workflow centers the proxy

- Paths / conflicting claims:
  - `Cargo.toml:3-6`:
    > workspace members include `crates/ocean-gui`
  - `Cargo.toml:8-15`:
    > `cargo build (no -p) builds only the native proxy ... default-members = ["crates/ocean-surface-proxy"]`
    > `crates/ocean-gui is the native GPUI desktop face. It is intentionally not a default member so cargo build keeps the existing proxy-focused workflow.`
- Severity: **Medium**
- Why it conflicts: the config confirms GUI belongs in this repo, but README/AGENTS/CLAUDE do not reflect that. The default build/workflow centers a proxy that currently has backend/provider responsibilities.
- Recommended wording:
  > `crates/ocean-gui` is a first-class native GUI surface in the surfaces workspace.
  >
  > `cargo build` defaults to the dev web server only for legacy workflow reasons; user-facing targets are built explicitly (`ocean-gui`, `ocean-surface-ui`, extension bundle, etc.). Provider/backend duties remain in `ocean-os`.
- Move/rename action: once backend/provider proxy duties move out, rename/re-scope `ocean-surface-proxy` and consider changing `default-members` or comments so proxy is not presented as the repo’s central target.

### F-11 — Legacy voice system prompt mentions local voice “sessions/turns/runtime snapshots” as surface-owned storage

- Paths / conflicting claims:
  - `legacy-voice/config/voice-agent-instructions.md:47-56`:
    > `The voice home is the visible place for user-facing voice-agent notes, docs, presentations, scripts, state, sessions, turns, voices, and workspace files.`
    > `The voice state folder stores latest transcript, latest response, spoken summary, voice choice, and runtime snapshots.`
  - Same file `:74-85` correctly says runtime owns requests, sessions, event streams, etc.
- Severity: **Medium**
- Why it conflicts: it blurs UI scratch/transcript storage with authoritative runtime sessions/snapshots, which should be `ocean-os` owned.
- Recommended wording:
  > The voice home stores user-visible notes, docs, transcripts, UI scratch artifacts, and per-turn display files. Authoritative sessions, runtime state, event storage, and backend snapshots live in Ocean Current / `ocean-os`.

## Missing docs / gaps

1. **No canonical ownership matrix.** Add a top-level matrix explicitly splitting:
   - `ocean-surface(s)`: GUI, TUI, web/PWA, voice, extension, Ocean Browser, UI rendering, input capture, local shell UX.
   - `ocean-os`: daemon/runtime, sessions, protocol types, providers, tools, permissions, backend services, event streams.
2. **TUI surface docs are missing.** The root docs currently assign TUI to `ocean-os`, and this repo has no visible TUI crate/doc/roadmap. Add a TUI entry or transitional migration note.
3. **Ocean Browser docs are missing.** Extension docs discuss browser tools, but there is no product doc for Ocean Browser as a surface/product or standalone Chromium fork.
4. **GUI is under-documented.** `crates/ocean-gui` exists in `Cargo.toml` and has `MODEL_PICKER_SPEC.md`, but README/AGENTS/CLAUDE omit it from the workspace map.
5. **Extension is missing from top-level repo map.** `extension/` and `docs/ocean-extension-context.md` exist, but README/AGENTS/CLAUDE do not list extension as a first-class surface.
6. **Voice is still documented as `legacy-voice`.** Intended boundary makes voice a first-class surface; add current voice-surface docs and mark exactly what is legacy vs canonical.
7. **Provider/backend migration doc is missing.** Because several docs currently route xAI/STT/TTS/Maps through surface proxy, add a short migration plan: provider credentials and `/api/stt`/`/api/tts`/Maps backend move to `ocean-os`, surface calls backend protocol only.
8. **Plural naming is unresolved.** The task names `ocean-surface/ocean-surfaces`; docs only use singular `ocean-surface`. Decide whether the repo/product should be renamed to `ocean-surfaces` or keep repo singular with docs explaining it is the surfaces workspace.
9. **Generated HTML docs should be regenerated after Markdown/source updates.** `docs/ocean-extension-context.html` and `docs/ocean-extension-manifesto.html` duplicate extension boundary language; update their source or regenerate after correcting ownership wording.
10. **No explicit “surface sessions vs daemon sessions” terminology.** Add wording distinguishing session selectors/transcript rendering/session_id caching (surface-owned UI state) from authoritative sessions/storage/locks/history (daemon-owned runtime state).

## Files inspected with no material boundary conflict found

- `crates/ocean-surface-ui/Cargo.toml` — dependency/features only; comments align with WASM UI surface.
- `crates/ocean-surface-proxy/Cargo.toml` — package/dependency metadata only; the name reinforces proxy scope but the conflicting claims are in docs above.
- `crates/ocean-gui/Cargo.toml` — confirms GUI crate exists; no bad description, but lacks package description.
- `Trunk.toml` — web serving config only.
- `extension/manifest.json` — describes the extension as an Ocean cockpit and grants daemon host permission; no direct ownership conflict, but extension should be added to top-level docs.
- `extension/background.js`, `extension/sidepanel.html`, `extension/sidepanel.js` — implementation comments only; no boundary conflict found.
- `crates/ocean-gui/assets/icons/ocean-gui/README.md` — icon provenance only.
