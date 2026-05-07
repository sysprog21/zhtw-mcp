# zhtw-mcp Chrome extension

This Manifest V3 extension checks the visible text in the active tab for non-standard Traditional Chinese usage, highlights findings in the page, and shows the warning/error count in the extension badge.

## Build the scanner WASM

Install the browser Rust target and `wasm-pack`, then build the scanner glue:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
sh extension/build-wasm.sh
```

The generated files are written to `extension/dist/`.

## Load in Chrome

1. Open `chrome://extensions`.
2. Enable Developer mode.
3. Choose **Load unpacked** and select the `extension/` directory.
4. Open a page containing text such as `這個軟件使用了遞歸算法來遍歷鏈表`.
5. Click the extension icon, then **檢查目前分頁**.

The extension uses `activeTab`, so it scans only after a user gesture and only for the current active tab. Badge counts include warning and error issues; info-level findings appear in the popup but do not increase the badge.

The extension applies browser-only result filtering for common page markup patterns such as `...` and `:::`. These filters do not change the Rust CLI, MCP server, or WASM scanner rules.

## Test JavaScript helpers

```sh
npm test --prefix extension
```
