const form = document.querySelector("#scan-form");
const statusNode = document.querySelector("#status");
const countNode = document.querySelector("#count");
const metaNode = document.querySelector("#meta");
const issueListNode = document.querySelector("#issue-list");
const scanButton = document.querySelector("#scan-button");
const profileInput = document.querySelector("#profile");
const relaxedInput = document.querySelector("#relaxed");

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  await runScan();
});

document.addEventListener("DOMContentLoaded", loadPreviousResult);

async function loadPreviousResult() {
  const response = await sendMessage({ type: "GET_ACTIVE_TAB_RESULT" });
  if (response.ok && response.result) {
    renderResult(response.result);
    return;
  }
  renderEmpty();
}

async function runScan() {
  setBusy(true);
  renderStatus("正在掃描目前分頁…", "busy");
  try {
    const response = await sendMessage({
      type: "RUN_SCAN_ACTIVE_TAB",
      options: {
        profile: profileInput.value,
        relaxed: relaxedInput.checked,
      },
    });
    if (!response.ok) {
      throw new Error(response.error || "掃描失敗");
    }
    renderResult(response.result);
  } catch (error) {
    renderStatus(error.message, "error");
  } finally {
    setBusy(false);
  }
}

function sendMessage(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage(message, (response) => {
      const error = chrome.runtime.lastError;
      if (error) {
        resolve({ ok: false, error: error.message });
        return;
      }
      resolve(response || { ok: false, error: "No response from background." });
    });
  });
}

function renderEmpty() {
  countNode.textContent = "—";
  metaNode.textContent = "按下檢查後，只會讀取目前分頁可見文字。";
  issueListNode.innerHTML = "";
  renderStatus("尚未掃描", "idle");
}

function renderResult(result) {
  const count = result.badge_count || 0;
  countNode.textContent = String(count);
  metaNode.textContent = `${result.page_title || "目前分頁"} · ${formatBreakdown(result)}`;
  renderStatus(count ? `找到 ${count} 個需留意的用語` : "沒有警告或錯誤", count ? "warn" : "ok");
  renderIssues(result.issues || []);
}

function renderIssues(issues) {
  issueListNode.innerHTML = "";
  if (!issues.length) {
    const empty = document.createElement("li");
    empty.className = "issue issue--empty";
    empty.textContent = "沒有可列出的結果。";
    issueListNode.append(empty);
    return;
  }

  for (const issue of issues.slice(0, 30)) {
    const item = document.createElement("li");
    item.className = `issue issue--${issue.severity}`;

    const found = document.createElement("span");
    found.className = "issue__found";
    found.textContent = issue.found;

    const detail = document.createElement("span");
    detail.className = "issue__detail";
    const suggestion = issue.suggestions?.length
      ? ` → ${issue.suggestions.join("、")}`
      : "";
    detail.textContent = `${issue.rule_type}${suggestion}`;

    item.append(found, detail);
    issueListNode.append(item);
  }
}

function formatBreakdown(result) {
  const counts = result.severity_counts || {};
  return `錯誤 ${counts.error || 0}，警告 ${counts.warning || 0}，資訊 ${counts.info || 0}`;
}

function renderStatus(text, tone) {
  statusNode.textContent = text;
  statusNode.dataset.tone = tone;
}

function setBusy(isBusy) {
  scanButton.disabled = isBusy;
  scanButton.textContent = isBusy ? "檢查中…" : "檢查目前分頁";
}
