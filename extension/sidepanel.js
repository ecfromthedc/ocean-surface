// External module loader (extension CSP forbids inline <script>).
//
// Before booting the WASM cockpit we publish the active tab the side panel is
// docked in onto `window.__ocean_active_tab` ({ url, title }). The WASM app
// (which can't call chrome.* APIs itself) reads that snapshot at turn-send time
// and attaches it as guidance, so the agent knows what page the user is on.
// Privacy: only the single active tab in the current window, kept current via
// activation / URL-change / focus listeners — never the full tab list, and the
// agent only sees it on a turn the user initiates.

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

// Seed it now, then keep it fresh as the user moves around the browser.
publishActiveTab();
if (chrome.tabs) {
  chrome.tabs.onActivated.addListener(publishActiveTab);
  chrome.tabs.onUpdated.addListener((_id, info) => {
    // Only re-query on navigations that change the URL or finish loading.
    if (info.url || info.status === "complete") publishActiveTab();
  });
}
if (chrome.windows) {
  chrome.windows.onFocusChanged.addListener(publishActiveTab);
}

import init from "./dist/ocean-surface-ui.js";
init("./dist/ocean-surface-ui_bg.wasm");
