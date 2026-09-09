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

The popup selects the profile, the CJK boundary spacing policy (the same choice `--spacing` makes on the CLI), the UI-string relaxation, and the rule families to turn off.

The extension uses `activeTab`, so it reads the page only after a user gesture and only for the current active tab. Badge counts include warning and error issues; info-level findings appear in the popup but do not increase the badge.

After a scan, the extension watches editable fields for runs of three or more
of the same printable ASCII symbol. Warn is the default and shows a floating notice under
the focused field; Remove deletes the run the current edit produced, in text
inputs, textareas and contenteditable regions alike; Off does neither. The
choice is remembered across popup sessions.

A run made of a markup character (`#`, `:`, `-`, `~`, `*`, `_`, `=`, `+`,
`` ` ``, `>`, `|`) is left alone when it opens its line, so a Markdown heading,
thematic break, fenced block or fenced note survives both modes, including
`### Heading` and a fence carrying a language tag. The same characters inside a
sentence, with prose in front of them, are treated as repeated punctuation.
Removal takes only a run the current edit produced, so typing in one part of a
field never rewrites another.

In Warn and Remove the scan also leaves the page watching its editable fields,
so what you type afterwards is checked as you type. Nothing leaves the page and
nothing is stored: the check runs locally and password fields are never read.
Scanning with Off removes those observers outright, so no scan buys a lasting
view of your typing. The setting is remembered, but it reaches the page on the
next scan, so changing it does not disturb a tab you are not scanning.

Removal goes through the browser's own editing command, so it lands on the undo
stack and reaches a framework listening on the field. Undo puts the run back and
leaves it there.

On the page itself, only those markers leave the scan, plus a standalone `...`
that no Chinese prose sits against. A line of repeated punctuation such as `!!!`
stays in, because the punctuation rules are what report it.

## Test JavaScript helpers

```sh
npm test --prefix extension
```

## Test the unpacked extension headlessly

The browser test loads the real Manifest V3 extension into Playwright's bundled
Chromium, opens its popup page, and initializes the WASM module. Build the WASM
bundle and install the pinned test dependency first:

```sh
sh extension/build-wasm.sh
npm ci --prefix extension
npx --prefix extension playwright install chromium
npm run test:all --prefix extension
```
