import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { chromium } from "@playwright/test";

const extensionPath = path.resolve(import.meta.dirname, "..");

// A cold, contended CI runner registers the MV3 service worker far slower than
// a warm local one, and the budget here is only a ceiling on a hang.
async function extensionWorker(context) {
  return context.serviceWorkers()[0] ?? context.waitForEvent("serviceworker", { timeout: 30_000 });
}

test("headless extension bundle loads its popup and initializes WASM", { timeout: 60_000 }, async () => {
  const profile = await mkdtemp(path.join(tmpdir(), "zhtw-extension-"));
  let context;

  try {
    context = await chromium.launchPersistentContext(profile, {
      channel: "chromium",
      headless: true,
      args: [
        `--disable-extensions-except=${extensionPath}`,
        `--load-extension=${extensionPath}`,
      ],
    });
    const worker = await extensionWorker(context);
    const extensionId = new URL(worker.url()).host;
    const popup = await context.newPage();
    await popup.goto(`chrome-extension://${extensionId}/popup.html`);
    await popup.locator("#scan-button").waitFor();

    const scanExport = await popup.evaluate(async () => {
      const wasm = await import(chrome.runtime.getURL("dist/zhtw_mcp_wasm.js"));
      await wasm.default({
        module_or_path: chrome.runtime.getURL("dist/zhtw_mcp_wasm_bg.wasm"),
      });
      return typeof wasm.scan_text;
    });

    assert.equal(scanExport, "function");
  } finally {
    await context?.close();
    await rm(profile, { recursive: true, force: true });
  }
});
