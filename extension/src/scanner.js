import init, { scan_text } from "../dist/zhtw_mcp_wasm.js";

let initPromise;

async function loadWasmModule() {
  if (!initPromise) {
    initPromise = (async () => {
      const wasmUrl = chrome.runtime.getURL("dist/zhtw_mcp_wasm_bg.wasm");
      await init({ module_or_path: wasmUrl });
    })();
  }
  try {
    await initPromise;
  } catch (error) {
    initPromise = undefined;
    throw error;
  }
}

export async function scanText(text, options = {}) {
  try {
    await loadWasmModule();
  } catch (error) {
    throw new Error(
      `WASM scanner is not built. Run "sh extension/build-wasm.sh" before loading the extension. ${error.message}`,
    );
  }

  const resultJson = scan_text(text, JSON.stringify(options));
  return JSON.parse(resultJson);
}
