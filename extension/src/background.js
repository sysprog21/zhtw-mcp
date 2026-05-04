import { scanText } from "./scanner.js";

const latestResults = new Map();
const DEFAULT_OPTIONS = { profile: "base", relaxed: false };
const MAX_STORED_ISSUES = 200;

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message?.type === "RUN_SCAN_ACTIVE_TAB") {
    runScanForActiveTab(message.options || DEFAULT_OPTIONS)
      .then(sendResponse)
      .catch((error) => sendResponse({ ok: false, error: error.message }));
    return true;
  }

  if (message?.type === "GET_ACTIVE_TAB_RESULT") {
    getActiveTab()
      .then((tab) => getStoredResult(tab?.id))
      .then((result) => sendResponse({ ok: true, result }))
      .catch((error) => sendResponse({ ok: false, error: error.message }));
    return true;
  }

  if (message?.type === "CLEAR_ACTIVE_TAB") {
    getActiveTab()
      .then((tab) => clearTabResult(tab?.id))
      .then(() => sendResponse({ ok: true }))
      .catch((error) => sendResponse({ ok: false, error: error.message }));
    return true;
  }

  return false;
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (changeInfo.status === "loading") {
    void clearTabResult(tabId).catch(reportAsyncError);
  }
});

chrome.tabs.onRemoved.addListener((tabId) => {
  void clearTabResult(tabId, { clearBadge: false }).catch(reportAsyncError);
});

async function runScanForActiveTab(options) {
  const tab = await getActiveTab();
  if (!tab?.id) {
    throw new Error("No active tab is available.");
  }
  if (!isInspectableUrl(tab.url)) {
    throw new Error("Chrome does not allow extensions to inspect this page.");
  }

  await ensureContentScript(tab.id);
  const collected = await sendTabMessage(tab.id, { type: "COLLECT_TEXT" });
  if (!collected?.ok) {
    throw new Error(collected?.error || "Could not collect visible page text.");
  }

  const scanResult = await scanText(collected.text, options);
  const highlighted = await sendTabMessage(tab.id, {
    type: "HIGHLIGHT_ISSUES",
    issues: scanResult.issues,
  });
  if (!highlighted?.ok) {
    throw new Error(highlighted?.error || "Could not highlight scan results.");
  }

  const badgeCount = scanResult.badge_count ?? countBadgeIssues(scanResult.issues);
  await setBadge(tab.id, badgeCount);

  const result = {
    ...scanResult,
    badge_count: badgeCount,
    highlighted_count: highlighted.highlighted_count,
    skipped_count: highlighted.skipped_count,
    options,
    page_title: tab.title || "",
    page_url: tab.url || "",
    scanned_at: new Date().toISOString(),
  };
  await storeResult(tab.id, result);
  return { ok: true, result };
}

async function ensureContentScript(tabId) {
  const target = { tabId };
  await chrome.scripting.insertCSS({ target, files: ["styles/content.css"] });
  await chrome.scripting.executeScript({
    target,
    files: ["src/shared.js", "src/content.js"],
  });
}

async function getActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

function isInspectableUrl(url = "") {
  return /^(https?|file):/i.test(url);
}

function sendTabMessage(tabId, message) {
  return new Promise((resolve, reject) => {
    chrome.tabs.sendMessage(tabId, message, (response) => {
      const error = chrome.runtime.lastError;
      if (error) {
        reject(new Error(error.message));
        return;
      }
      resolve(response);
    });
  });
}

async function setBadge(tabId, count) {
  const text = count > 99 ? "99+" : count > 0 ? String(count) : "";
  await chrome.action.setBadgeText({ tabId, text });
  await chrome.action.setBadgeBackgroundColor({
    tabId,
    color: count > 0 ? "#b3261e" : "#40614f",
  });
}

async function clearTabResult(tabId, { clearBadge = true } = {}) {
  if (!tabId) {
    return;
  }
  latestResults.delete(tabId);
  if (clearBadge) {
    await chrome.action.setBadgeText({ tabId, text: "" });
  }
  if (chrome.storage?.session) {
    await chrome.storage.session.remove(storageKey(tabId));
  }
}

async function storeResult(tabId, result) {
  latestResults.set(tabId, result);
  if (chrome.storage?.session) {
    try {
      await chrome.storage.session.set({ [storageKey(tabId)]: storageResult(result) });
    } catch (error) {
      console.warn("Could not persist scan result in session storage.", error);
    }
  }
}

async function getStoredResult(tabId) {
  if (!tabId) {
    return null;
  }
  if (latestResults.has(tabId)) {
    return latestResults.get(tabId);
  }
  if (chrome.storage?.session) {
    const stored = await chrome.storage.session.get(storageKey(tabId));
    return stored[storageKey(tabId)] || null;
  }
  return null;
}

function storageKey(tabId) {
  return `tab:${tabId}:scan`;
}

function storageResult(result) {
  const issues = Array.isArray(result.issues) ? result.issues : [];
  return {
    ...result,
    issues: issues.slice(0, MAX_STORED_ISSUES),
    stored_issue_count: Math.min(issues.length, MAX_STORED_ISSUES),
    total_issue_count: issues.length,
    storage_truncated: issues.length > MAX_STORED_ISSUES,
  };
}

function reportAsyncError(error) {
  console.warn("Background task failed.", error);
}

function countBadgeIssues(issues = []) {
  return issues.filter(
    (issue) => issue.severity === "warning" || issue.severity === "error",
  ).length;
}
