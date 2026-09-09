# zhtw-mcp

A linguistic linter for Traditional Chinese (zh-TW) that enforces Taiwan Ministry of Education (MoE) standards on vocabulary, punctuation, and character shapes. It plugs into AI coding assistants through the [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) and catches Mainland Chinese (zh-CN) regional drift before it reaches the user.

The tool enforces three official Taiwan standards:

- [Revised Handbook of Punctuation](https://language.moe.gov.tw/001/upload/files/site_content/m0001/hau/c2.htm) (《重訂標點符號手冊》修訂版) -- punctuation marks
- [Standard Form of National Characters](https://language.moe.gov.tw/001/Upload/files/SITE_CONTENT/M0001/STD/F4.HTML) (《國字標準字體》) -- character shapes
- Cross-strait vocabulary normalization, grounded in [OpenCC](https://github.com/BYVoid/OpenCC)'s TWPhrases/TWVariants datasets -- word choices

Over 1100 vocabulary rules and 15 casing rules are compiled into the binary. For ambiguous terms, the server asks the AI assistant it runs inside for help deciding -- no extra API keys required.

## Why this exists

### Modern Chinese is an inadequately standardized language

In the late Qing dynasty, scholars had to express Western concepts in a writing system with no native vocabulary for them. Whether coining new words or importing translations via Japanese (和製漢語), they assembled a literary system under enormous time pressure. Many translated terms were inconsistent, ambiguous, or contradictory. The Chinese-speaking world has lived with these deficiencies for over a century.

### Simplified Chinese made it worse

The PRC simplification effort reduced not just stroke counts but vocabulary precision. Terms that should vary by domain got flattened into single catch-all translations. Many PRC translations were coined hastily: if a term worked in one context, it spread uncritically to others.

### AI models amplify the problem

AI language models learn from web text where Simplified Chinese vastly outweighs Traditional Chinese (roughly 2.6:1 in [CC-100](https://data.statmt.org/cc-100/)). Major datasets like [CulturaX](https://huggingface.co/datasets/uonlp/CulturaX) do not even track Traditional Chinese separately. A [FAccT 2025 study](https://arxiv.org/abs/2505.22645) confirmed that most models favor zh-CN terminology when asked to write zh-TW. The output looks plausible but is not how people in Taiwan actually write.

This goes beyond character conversion. The same word often means different things across the strait:

| English | zh-CN | zh-TW | Why it matters |
|---------|-------|-------|----------------|
| concurrency | 並發 | 並行 | In zh-CN, 並行 means "parallel" -- a different concept entirely |
| parallel | 並行 | 平行 | zh-CN 並行 = "parallel"; in Taiwan, 並行 = "concurrent" |
| process (OS) | 進程 | 行程 | 進程 in Taiwan means "progress," not an OS process |
| file / document | 文件 / 文檔 | 檔案 / 文件 | 文件 in China = "file"; in Taiwan = "document" |
| render | 渲染 | 算繪 | 渲染 in Taiwan = "exaggerate" (a painting technique) |
| traverse | 遍歷 | 走訪 | 遍歷 in Taiwan is reserved for Ergodic theory (遍歷理論) |

### What this project does

Automatically check and correct zh-TW text produced by AI, catching cross-strait terminology leaks:

- Half-width punctuation (`,` `.` `:`) that should be full-width (`，` `。` `：`)
- Mainland-style `""` curly quotes replaced with Taiwan-style `「」` corner brackets
- Missing or extra CJK-Latin/digit spacing when the project's spacing policy requires it
- Mainland vocabulary -- 軟件→軟體, 內存→記憶體, 默認→預設, etc.
- Non-standard character variants -- 裏→裡, 着→著 per MoE standard forms
- Politically colored terms -- 祖國, 內地
- Casing -- JavaScript, GitHub, macOS

These standards are enforced through two profiles on the strictness axis, plus orthogonal capability flags:

| Profile | Purpose |
|---------|---------|
| `base` | Cross-strait vocabulary, punctuation, casing, grammar, politically colored terms |
| `strict` | Full MoE enforcement: character variants (裏→裡), grammar (臺/台), all punctuation |

| Flag | Purpose |
|------|---------|
| `relaxed` | Relaxed for software UI: disables colon/dunhao enforcement and grammar checks; uses en-dash for ranges |
| `detect_ai` | AI writing review: filler phrase detection, semantic safety words, copula/passive voice checks, density-based pattern detection |
| `spacing` | CJK boundary policy, `--spacing require\|strip`: `require` stores spaces (default); `strip` leaves the gap to a controlled HTML renderer. The one flag here that takes a value, and a different thing from `--off spacing`, which turns the whole family off |

For unsupported authority attributions, select `document_genre` in MCP or
`--document-genre casual|technical|financial` in the CLI. The check runs only
with AI detection on (`--detect-ai` / `detect_ai`), and never suggests an edit
in any genre: deleting an attribution changes what the sentence claims, so the
genre selects the advice rather than a rewrite. Casual prose is told to name
the source or drop the appeal; technical and financial prose are told the
claim needs a citation.

Profiles control how strict the zh-TW norm enforcement is. Flags are orthogonal -- `detect_ai` works with either profile, `relaxed` can combine with `strict` if you want variant normalization but lenient punctuation.

`spacing=require` is the default because source text also appears outside CSS-capable renderers. Projects that control their HTML can select `strip` and use [`text-autospace`](https://developer.mozilla.org/en-US/docs/Web/CSS/text-autospace), a Baseline newly-available property since November 2025. [UTR #59](https://www.unicode.org/reports/tr59/) is a draft for layout-time autospacing, not a requirement to remove source-text spaces. Either way the policy governs the U+0020 space, and only where CJK meets an ASCII letter or an ASCII digit.

See [docs/rules.md](docs/rules.md) for the full rule reference.

## Naming convention: cn and tw

This project follows [BCP 47](https://www.rfc-editor.org/info/bcp47). The region subtag comes from [ISO 3166-1 alpha-2](https://www.iso.org/iso-3166-country-codes.html), where "region" can denote a sovereign state, territory, or economic area -- not necessarily a "country."

- `zh-CN`: Chinese as written in the CN region (Simplified)
- `zh-TW`: Chinese as written in the TW region (Traditional)

Throughout the codebase, `cn` and `tw` denote regional writing conventions, not a political statement.

## Getting started

### Pre-built binaries

Every successful push to `main` refreshes the rolling [`latest`](https://github.com/sysprog21/zhtw-mcp/releases/tag/latest) release, which is what GitHub reports as the latest release. No version tag is involved. Each archive holds the binary, `LICENSE`, and `README.md`, and `SHA256SUMS` ships next to them.

| Platform | Asset |
| --- | --- |
| Linux x86_64 (glibc 2.39 or newer) | `zhtw-mcp-x86_64-unknown-linux-gnu.tar.gz` |
| Linux arm64 (glibc 2.39 or newer) | `zhtw-mcp-aarch64-unknown-linux-gnu.tar.gz` |
| macOS arm64 | `zhtw-mcp-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `zhtw-mcp-x86_64-pc-windows-msvc.tar.gz` |

On any other platform, use Nix below or build from source.

The browser extension is packaged onto the same release as
`zhtw-mcp-extension.zip`.  Unpack it and load it through `chrome://extensions`
with developer mode on.

#### macOS / Linux

```bash
base=https://github.com/sysprog21/zhtw-mcp/releases/download/latest
case "$(uname -sm)" in
  "Darwin arm64")  asset=zhtw-mcp-aarch64-apple-darwin.tar.gz ;;
  "Linux x86_64")  asset=zhtw-mcp-x86_64-unknown-linux-gnu.tar.gz ;;
  "Linux aarch64") asset=zhtw-mcp-aarch64-unknown-linux-gnu.tar.gz ;;
  *) asset=""; echo "no pre-built binary for $(uname -sm)" >&2 ;;
esac
[ -n "$asset" ] &&
  curl -fsSLO "$base/$asset" -O "$base/SHA256SUMS" &&
  shasum -a 256 --ignore-missing -c SHA256SUMS &&
  tar -xzf "$asset" zhtw-mcp
```

On Linux without `shasum`, use `sha256sum --ignore-missing -c SHA256SUMS` instead.

#### Windows (PowerShell)

```powershell
$base = "https://github.com/sysprog21/zhtw-mcp/releases/download/latest"
$asset = "zhtw-mcp-x86_64-pc-windows-msvc.tar.gz"
irm "$base/$asset" -OutFile $asset
irm "$base/SHA256SUMS" -OutFile SHA256SUMS
$want = ((Select-String -Path SHA256SUMS -SimpleMatch $asset).Line -split '\s+')[0]
if ((Get-FileHash -Algorithm SHA256 $asset).Hash -ine $want) { throw "checksum mismatch" }
tar -xzf $asset zhtw-mcp.exe
```

Both snippets leave the binary in the current directory; move it somewhere on your `PATH` to run it by name.

### Nix

On any system with Nix and flakes enabled:

```bash
# builds and drops into a temporary shell with `zhtw-mcp` on `$PATH`
nix shell "github:sysprog21/zhtw-mcp"

# builds and runs zhtw-mcp (you can use this command to register with an MCP
# client as shown in the Installing section)
nix run "github:sysprog21/zhtw-mcp"
```

> **Note:** The first run compiles the project from source, which can take
several minutes. Subsequent runs reuse the Nix store cache and start instantly.
To speed up the initial build, run `nix build --cores 0 "github:sysprog21/zhtw-mcp"`
first. And `--cores 0` tells Nix to use all available CPU cores.

### Building from source

Requires stable Rust 1.91+.

```bash
make
```

The binary is at `target/release/zhtw-mcp`.

Python 3 is a build requirement, not just a test requirement: the OpenCC
conversion tables are generated rather than committed.

### Working on the code

```bash
make check           # the gate CI runs: tests, clippy, formatting, hooks
make indent          # run the formatters; the gate checks their result
make hooks           # install the git hooks; uninstall-hooks removes them
make corpus          # precision, recall and false-positive metrics
```

`scripts/indent.sh` holds the formatter chain: comment reflow with
`commentflow`, then `cargo fmt`, `black`, `shfmt`, and the `assets/ruleset.json`
normalization that `scripts/check-ruleset.py` owns. `make indent` runs it with
`--write` and the gate runs it with `--check`, against a copy of the tree so a
check never rewrites what it is judging. The chain runs to a fixed point, since
reindenting a block can invalidate the wrap of a comment inside it.

No formatter here is passed a style flag. `shfmt` takes its settings from
`.editorconfig` and `commentflow` takes its column limit from `.clang-format`,
which exists only for that number: comments wrap at 80 while `rustfmt` allows
code 100.

Every lane reports that it skipped rather than failing when its tool is missing,
which is why a green local run is weaker evidence than a green CI run. CI
installs `commentflow`, `shfmt` and `shellcheck` on the Linux leg and sets
`ZHTW_REQUIRE_TOOLS=1` there, which turns a skip into a failure.

Any `cargo build` installs the git hooks, through `build.rs`, and `make hooks`
does it on its own. A configured `core.hooksPath` is left untouched, since it
may be shared by unrelated repositories. The pre-commit hook runs `rustfmt`, `black`, `shellcheck`,
`shfmt`, `commentflow` and the ruleset checks over a checkout of the index, so
an unstaged edit neither fails a commit nor rides along in one. The commit-msg
hook holds the subject to 50 columns and the body to 72, imperative and free of
em dashes, counting a CJK character as the two columns a terminal spends on
it. The pre-push hook replays
those rules over commits a rebase or an amend rewrote, and CI runs the same
script over a pull request's own commits, so the rules bind a contributor who
never installed the hooks as well.

`scripts/check-comments.sh` holds source comments to the two prose rules no
formatter knows about: no em dash, and no backtick outside a `///` or `//!` doc
comment, where backticks are rustdoc markup rather than prose. It runs in the
gate and in the pre-commit hook.

`scripts/test-git-hooks.sh` drives all four hooks against a scratch repository
and runs in the gate, so a hook that stops rejecting fails there rather than on
somebody's next commit.

### Installing

The quickest way to build, install to `$XDG_BIN_HOME` (or `~/.local/bin`),
stop older server processes, and register with detected MCP clients
(Claude Code and/or Codex):

```bash
make install      # build release, install binary, register detected MCP clients
make uninstall    # remove binary and detected MCP registrations
make status       # check binary freshness, process, and registration state
```

For manual setup or other MCP clients:

```bash
# Claude Code
claude mcp add zhtw-mcp -- /path/to/zhtw-mcp

# Codex CLI
codex mcp add zhtw -- /path/to/zhtw-mcp

# OpenCode
opencode mcp add zhtw-mcp /path/to/zhtw-mcp
```

Other MCP clients may use `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "zhtw-mcp": {
      "command": "/path/to/zhtw-mcp",
      "args": []
    }
  }
}
```

Replace `/path/to/zhtw-mcp` with the actual binary path (e.g., `target/release/zhtw-mcp`).

### CLI quick start

```bash
zhtw-mcp lint README.md                 # lint a file
zhtw-mcp lint file.md --fix             # auto-fix in place
zhtw-mcp lint file.md --fix --dry-run   # preview fixes
zhtw-mcp lint file.md --telemetry       # print stderr summary counters
zhtw-mcp cache clear                    # clear persistent judgment cache
```

See [docs/cli.md](docs/cli.md) for the full CLI reference and [docs/mcp.md](docs/mcp.md) for MCP tool/resource/prompt details.

### Common prompts

When running as an MCP server, you interact through natural language. The assistant translates your intent into `zhtw` tool calls:

| Intent | Say | Maps to | What happens |
|--------|-----|---------|--------------|
| Lint text | *"Check this paragraph for mainland terms"* | `zhtw({ "text": "..." })` | Returns issues with line/column, suggestions, and rule type |
| Auto-fix | *"Fix the zh-TW issues in this document"* | `zhtw({ "text": "...", "fix_mode": "lexical_safe" })` | Deterministic fixes applied; corrected text returned |
| Quality gate | *"Reject if more than 3 zh-TW errors"* | `zhtw({ "text": "...", "max_errors": 3 })` | `accepted: true/false` verdict based on error count |
| Strict MoE | *"Check this with strict MoE rules"* | `zhtw({ "text": "...", "profile": "strict" })` | Adds character variant (裏→裡) and full punctuation enforcement |
| UI strings | *"Lint this UI string, skip grammar"* | `zhtw({ "text": "...", "relaxed": true })` | Disables colon/dunhao/grammar enforcement; uses en-dash for ranges |
| AI writing review | *"Review this for AI writing artifacts"* | `zhtw({ "text": "...", "detect_ai": true })` | Flags filler phrases, semantic safety words, copula/passive overuse |
| Markdown-aware | *"Lint this markdown, skip code blocks"* | `zhtw({ "text": "...", "content_type": "markdown" })` | Fenced code, inline code, and HTML blocks excluded from scanning |
| Cost telemetry | *"Lint this and include telemetry"* | `zhtw({ "text": "...", "include_telemetry": true })` | Returns estimated token/caching metrics for the call |

Each `zhtw` call is stateless -- parameters like `profile` are per-call, not session state. Omitting `profile` defaults to `base`.

The server also exposes two read-only resources for assistants to consult: `zh-tw://style-guide/moe` (MoE standards) and `zh-tw://dictionary/ambiguous` (cross-strait term disambiguation). See [docs/mcp.md](docs/mcp.md) for the full prompt catalog.

## Further reading

- [docs/cli.md](docs/cli.md) -- full CLI reference, config files, CI/CD integration, S2T conversion
- [docs/mcp.md](docs/mcp.md) -- MCP tool parameters, resources, prompts, sampling, usage examples
- [docs/internals.md](docs/internals.md) -- processing pipeline, script detection, design decisions, testing
- [docs/rules.md](docs/rules.md) -- rule type reference, extending the ruleset, runtime overrides

## License

`zhtw-mcp` is available under a permissive MIT-style license.
Use of this source code is governed by a MIT license that can be found in the [LICENSE](LICENSE) file.
