// External module loader (extension CSP forbids inline <script>).
//
// Before booting the WASM cockpit we publish a few browser-control snapshots
// onto `window` for the WASM app (which can't call chrome.* APIs itself) to
// read at turn-send time:
//
//   window.__ocean_active_tab  → { url, title } of the active tab (OCEAN-70)
//   window.__ocean_open_tabs   → [{ url, title, active }] for the current
//                                window (OCEAN-92) — lets the agent see/refer
//                                to the user's open tabs, not just the focused
//                                one. Active window only, kept current via the
//                                same activation / URL-change / focus listeners.
//   window.__ocean_capture_visible_tab → a function the WASM app (or a UI
//                                button) can call to capture the visible tab as
//                                a PNG data URL (OCEAN-92). User-initiated only.
//
// Privacy: only the current window, only tabs the user already has open; the
// agent only sees the snapshots on a turn the user initiates, never a passive
// background scrape.

function publishActiveTab() {
  try {
    chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => {
      if (chrome.runtime.lastError) return;
      const tab = tabs && tabs[0];
      if (!tab || !tab.url) {
        window.__ocean_active_tab = null;
        return;
      }
      window.__ocean_active_tab = { url: tab.url, title: tab.title || "" };
    });
  } catch (_e) {
    // `tabs` permission missing or API unavailable — degrade to no context.
    window.__ocean_active_tab = null;
  }
}

// Publish the full open-tab list for the current window (OCEAN-92). The WASM
// app reads this snapshot at send time and folds it into turn guidance so the
// agent can reason about / reference the tabs the user has open. We cap the
// list so a user with hundreds of tabs can't bloat the guidance block.
const MAX_OPEN_TABS = 24;
function publishOpenTabs() {
  try {
    chrome.tabs.query({ currentWindow: true }, (tabs) => {
      if (chrome.runtime.lastError) return;
      if (!Array.isArray(tabs)) {
        window.__ocean_open_tabs = [];
        return;
      }
      window.__ocean_open_tabs = tabs
        // Drop tabs with no URL and the extension's own panel page so we never
        // leak the cockpit itself into context.
        .filter((t) => t && t.url && !t.url.startsWith("chrome-extension://"))
        .slice(0, MAX_OPEN_TABS)
        .map((t) => ({
          url: t.url,
          title: t.title || "",
          active: !!t.active,
        }));
    });
  } catch (_e) {
    window.__ocean_open_tabs = [];
  }
}

// Capture the visible area of the active tab as a PNG data URL (OCEAN-92).
// Returns a Promise so the WASM app / a UI button can await it. User-initiated
// only — we never capture on a timer or in the background.
//
// NOTE (daemon follow-up): the daemon's POST /v1/agent/turns currently accepts
// no image/attachment field (AgentTurnRequest has prompt/cwd/guidance/... but
// no images), so we cannot yet hand this capture to the agent for visual
// reasoning. This function surfaces the capture (download / preview) on the
// extension side; wiring it into a turn needs a daemon-side image field first.
function captureVisibleTab() {
  return new Promise((resolve, reject) => {
    try {
      chrome.tabs.captureVisibleTab({ format: "png" }, (dataUrl) => {
        if (chrome.runtime.lastError) {
          reject(new Error(chrome.runtime.lastError.message));
          return;
        }
        resolve(dataUrl || null);
      });
    } catch (e) {
      reject(e);
    }
  });
}
window.__ocean_capture_visible_tab = captureVisibleTab;

// Convenience wrapper the WASM cockpit's screenshot button calls: capture the
// visible tab and save it as a PNG download (the only thing we can do with it
// today, since the daemon turn API has no image field yet — see note above).
// Returns true on success so the button can reflect state. User-initiated only.
async function captureAndSave() {
  const dataUrl = await captureVisibleTab();
  if (!dataUrl) return false;
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const a = document.createElement("a");
  a.href = dataUrl;
  a.download = `ocean-tab-${stamp}.png`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  return true;
}
window.__ocean_capture_and_save = captureAndSave;

function publishSnapshots() {
  publishActiveTab();
  publishOpenTabs();
}

// Seed now, then keep fresh as the user moves around the browser.
publishSnapshots();
if (chrome.tabs) {
  chrome.tabs.onActivated.addListener(publishSnapshots);
  chrome.tabs.onUpdated.addListener((_id, info) => {
    // Re-query on navigations that change the URL or finish loading.
    if (info.url || info.status === "complete") publishSnapshots();
  });
  // Opening/closing a tab changes the open-tab list even without activation.
  if (chrome.tabs.onCreated) chrome.tabs.onCreated.addListener(publishOpenTabs);
  if (chrome.tabs.onRemoved) chrome.tabs.onRemoved.addListener(publishOpenTabs);
}
if (chrome.windows) {
  chrome.windows.onFocusChanged.addListener(publishSnapshots);
}

import init from "./dist/ocean-surface-ui.js";
init("./dist/ocean-surface-ui_bg.wasm");
