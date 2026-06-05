# Ocean Chrome Extension Context Handoff

## Current state

The Ocean Chrome extension lives in `../ocean-surface/extension` and is an MV3 side-panel wrapper around the same Leptos/WASM Surface app used by the web PWA.

Entry points:

- `extension/manifest.json` — declares the MV3 extension, side panel, storage permission, and daemon host permission for `http://127.0.0.1:4780/*`.
- `extension/background.js` — opens the side panel from the toolbar action and sets side-panel behavior on install.
- `extension/sidepanel.html` — minimal HTML shell loading the packaged app.
- `extension/sidepanel.js` — imports `dist/ocean-surface-ui.js` and initializes `dist/ocean-surface-ui_bg.wasm`.
- `crates/ocean-surface-ui/src/daemon.rs` — detects `chrome-extension://` and connects directly to the local daemon instead of using `/api/config`.

Today the extension lets the user chat with Ocean from a Chrome side panel. It
does identify itself as `surface-extension`; it does **not** yet pass rich
browser-tab context into an agent turn.

## What the agent currently receives

`crates/ocean-surface-ui/src/daemon.rs` sends turns with roughly:

```json
{
  "prompt": "...",
  "cwd": "...",
  "session_id": "...",
  "project_id": "...",
  "client_type": "surface-extension"
}
```

In extension mode the UI connects directly to `http://127.0.0.1:4780` and sends
`client_type: "surface-extension"`, but the request does not yet tell the
daemon/agent:

- what tab the side panel is attached to,
- the active tab URL/title,
- selected text on the page,
- any page snapshot or DOM context.

So the agent can infer the surface type, but cannot yet see the active page
without the user describing it.

## Desired behavior

When the app is running as `chrome-extension://...`, every turn should carry explicit browser-extension context.

Minimum useful payload:

```json
{
  "client_type": "surface-extension",
  "guidance": "Client: Ocean Chrome extension side panel\nActive tab: <title> — <url>\nSelected text: <selection if any>"
}
```

Better long-term shape:

```json
{
  "client_type": "surface-extension",
  "client_context": {
    "surface": "chrome_extension_side_panel",
    "active_tab": {
      "id": 123,
      "url": "https://example.com/page",
      "title": "Example Page"
    },
    "selection": "selected text, if any",
    "page_excerpt": "optional visible text excerpt"
  }
}
```

## Implementation plan

### 1. Detect extension mode in Surface UI

Add a small helper in `crates/ocean-surface-ui/src/daemon.rs` or a new browser/extension module:

```rust
fn is_chrome_extension() -> bool {
    web_sys::window()
        .and_then(|w| w.location().protocol().ok())
        .map(|p| p.starts_with("chrome-extension"))
        .unwrap_or(false)
}
```

This logic already exists inside `bootstrap_then_connect`; extract/reuse it.

### 2. Keep the correct `client_type`

`crates/ocean-surface-ui/src/daemon.rs` already sends `surface-extension` when
running under `chrome-extension://` and `surface-web` otherwise. Preserve that
behavior while adding richer tab context.

### 3. Add extension permissions for tab context

Update `extension/manifest.json` permissions if active-tab metadata is needed:

```json
"permissions": ["sidePanel", "storage", "tabs", "activeTab", "scripting"]
```

Notes:

- `tabs` lets the extension query tab title/url depending on host access.
- `activeTab` grants temporary access after user invocation.
- `scripting` is useful for reading selection or injecting a tiny content script.

### 4. Bridge tab context from JS to WASM

The Rust/WASM UI cannot directly use `chrome.*` unless we bind it. Simplest path:

- add a JS helper loaded in the extension page, e.g. `extension/ocean_extension_context.js`, exposing a global async function:

```js
window.__oceanExtensionContext = async function () {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });

  let selection = "";
  try {
    const results = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => window.getSelection()?.toString() || "",
    });
    selection = results?.[0]?.result || "";
  } catch (_) {}

  return {
    surface: "chrome_extension_side_panel",
    active_tab: {
      id: tab?.id,
      url: tab?.url || "",
      title: tab?.title || "",
    },
    selection,
  };
};
```

Then call it from Rust with `wasm_bindgen`/`js_sys::Reflect` before sending the turn.

### 5. Decide transport field

There are two reasonable options:

1. **Fast path:** put a formatted text block into existing `guidance` if the daemon request already supports `guidance`.
2. **Typed path:** add `client_context: Option<serde_json::Value>` to the shared request type in `ocean-core`, plumb it through `ocean-daemon`/`ocean-agent`, and incorporate it into the agent turn/system context.

Fast path is less invasive. Typed path is cleaner.

## Why this matters

Once this is wired, the agent can automatically answer/act with awareness like:

- “You’re in the Ocean Chrome extension side panel.”
- “The active tab is GitHub PR #123.”
- “You selected this error text; I’ll use it as context.”
- “I can use browser tools against the same Chrome session.”

Without this, the side panel is visually in-browser, but the agent turn is indistinguishable from normal Ocean Surface web chat.

## Related existing browser plumbing in ocean-os

The runtime already has browser-driving tools:

- `crates/ocean-browser` — CDP wrapper and Chrome launch/attach logic.
- `crates/ocean-runtime/src/tools/browser` — agent-facing tools:
  - `browser_navigate`
  - `browser_read_page`
  - `browser_screenshot`
  - `browser_click`
  - `browser_type`
  - `browser_key`
  - `browser_scroll`
  - `browser_eval_js`
  - `browser_console`
  - `browser_network`

Browser tools emit `BrowserActivity { active: true }`, which the daemon relays over SSE. Surface receives this and focuses the side panel when browser work is active.

That means most browser automation plumbing exists already. The missing piece is **extension-origin client context in the turn request**.
