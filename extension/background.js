// Open the Ocean side panel when the toolbar icon is clicked, and make that
// the default behavior so the panel is one click away in any tab.
chrome.action.onClicked.addListener(async (tab) => {
  await chrome.sidePanel.open({ tabId: tab.id });
});
chrome.runtime.onInstalled.addListener(() => {
  chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true });
});
