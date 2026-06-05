# Spec: model switching in the GPUI app

For the GPUI agent. The native app currently has **no** model-switching code —
it only calls `/health`, `/v1/agent/turns`, `/v1/agent/events`, and the "model"
shown in the shell is a **read-only** metric row (`view.rs`, `agent_metric_row("model", …)`).
The web surface can hot-swap models live; this app can't. Build that here.

The daemon + proxy already fully support it — verified live against
`ocean.agentsworld.org`. You only need the client + UI.

## The daemon contract (already works — don't change the daemon)

- `GET /v1/models` → catalogue + current:
  ```json
  {"ok":true,
   "current":{"model":"deepseek-chat","provider":"deepseek"},
   "models":[{"id":"deepseek-v4-pro","label":"DeepSeek V4 Pro","provider":"deepseek"},
             {"id":"kimi-k2.6","label":"Kimi K2.6","provider":"kimi"}, …11 total]}
  ```
- `POST /v1/model` with `{"model":"<id>"}` → hot-swaps the daemon's active model
  globally (not per-turn), returns `{"ok":true,"model":"<id>","provider":"<p>"}`.
- `GET /v1/model` → current selection only.

This is a **daemon-global** swap: one POST changes the model for all subsequent
turns. Mirror the web surface (`ocean-surface-ui/src/daemon.rs::set_model` /
`fetch_models`) — same endpoints, same JSON.

## Build these three pieces, in the app's existing idiom

### 1. `src/shell/daemon.rs` — two client methods (match the existing style)
Add alongside `health` / `submit_turn`, using the same `reqwest::blocking`
pattern and a `models_url` / `model_url` helper next to the other `*_url` fns:

```rust
pub fn fetch_models(&self, base_url: &str) -> Result<ModelsResponse, String> {
    self.http.get(models_url(base_url)).timeout(HEALTH_TIMEOUT).send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json::<ModelsResponse>().map_err(|e| e.to_string())
}

pub fn set_model(&self, base_url: &str, id: &str) -> Result<(), String> {
    self.http.post(model_url(base_url)).timeout(HEALTH_TIMEOUT)
        .json(&serde_json::json!({ "model": id })).send()
        .and_then(|r| r.error_for_status())
        .map(|_| ()).map_err(|e| e.to_string())
}
```
with `ModelsResponse { ok, current: Option<CurrentModel>, models: Vec<ModelInfo> }`,
`ModelInfo { id, label, provider }`, `CurrentModel { model, provider }`
(`#[derive(Deserialize)]`).

### 2. `src/shell/model.rs` (ShellState) — hold the catalogue + current
Add `models: Vec<ModelInfo>` and `current_model: Option<String>`. Populate
`current_model` from BOTH the `/v1/models` `current` field AND the turn-stream
model event you already track in `agent.rs` (whichever is fresher).

### 3. `src/shell/view.rs` — replace the read-only row with a dropdown
The `agent_metric_row("model", …)` becomes a clickable control that lists
`state.models` and calls `set_model` on pick, then re-runs `fetch_models` to
confirm (optimistic update is fine — set local current immediately, reconcile on
the confirm read). Use the app's existing dropdown/menu pattern (you already
have menu UI elsewhere in the shell — reuse it, don't invent a new widget).

## When to fetch — DO NOT repeat the web bug we just fixed

The web surface had a startup race: it fetched the catalogue **before** it knew
the daemon URL, so it worked on localhost and showed an **empty picker** from
`ocean.agentsworld.org`. Don't recreate it here:

- Call `fetch_models` **after** the app has resolved its `base_url` (the same
  place/after you already do the first `health` check), not before.
- The native app reads `DEFAULT_DAEMON_URL` / its configured base_url
  synchronously, so it's less race-prone than the web bootstrap — but still
  fetch the catalogue on the same path that establishes the connection, and
  re-fetch after a successful `set_model`.

## Done =

From the running native app you can open the model control, see all ~11 models,
pick one, and the next turn uses it — verified against `ocean.agentsworld.org`,
not just localhost. Keep it GPUI-native (no web widgets / component_render —
this is the surface-gpui client).
