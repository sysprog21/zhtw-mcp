# CLI usage

The `<!-- cli:<name> -->` … `<!-- cli:end -->` blocks below are the `--help`
texts: `build.rs` extracts them and embeds them into the binary verbatim, so
what `zhtw-mcp <name> --help` prints is exactly what this page shows. Edit the
help here; a missing or malformed block fails the build.

<!-- cli:global -->
```text
zhtw-mcp - Traditional Chinese (zh-TW) text linter and MCP server

Usage:
  zhtw-mcp [GLOBAL OPTIONS]                Run the MCP server over stdio
  zhtw-mcp [GLOBAL OPTIONS] <COMMAND>

Commands:
  lint <files|-->     Lint files, directories, or stdin for zh-CN wording
  convert [files|--]  Convert Simplified Chinese to Traditional and normalize
  setup <host>        Print MCP integration config for an editor/agent host
  pack <subcommand>   Manage rule packs (import|export|validate|list)
  tm <subcommand>     Manage translation memory (list|export|import|clear|record)
  cache clear         Clear the LLM judgment cache

Global options:
  --overrides <path>     Custom overrides JSON path (alias: --db)
  --suppressions <path>  Custom suppressions JSON path
  --pack <name>          Activate a rule pack (repeatable)
  --packs-dir <path>     Directory holding installed rule packs
  --config <path>        Explicit .zhtw-mcp.toml path
  --verbose              Log at info level
  --debug                Log at debug level
  -h, --help             Show this help

Run 'zhtw-mcp <command> --help' for details on a command.
```
<!-- cli:end -->

## Linting files

<!-- cli:lint -->
```text
zhtw-mcp lint - lint files, directories, or stdin for zh-CN wording

Usage:
  zhtw-mcp lint <files|dirs...>            Lint files (directories recurse)
  zhtw-mcp lint -- < input.txt             Lint stdin

Options:
  --format <fmt>            Output format: human (default), json, compact,
                            tabular, sarif
  --fix[=<mode>]            Apply fixes in place: lexical_safe (default),
                            orthographic, lexical_contextual
  --dry-run                 Preview fixes without writing
  --explain                 Attach cultural/linguistic annotations
  --profile <p>             Rule profile: base or strict
  --content-type <ct>       plain, markdown, markdown-scan-code, or yaml
  --exclude <pattern>       Skip matching paths (repeatable)
  --max-errors <n>          Fail when errors exceed n
  --max-warnings <n>        Fail when warnings exceed n
  --relaxed                 Relax colon and other UI-string-level rules
  --exempt-blockquotes      Skip Markdown blockquotes
  --consistency             Report mixed regional usage
  --baseline <file>         Suppress known issues, fail only on new ones
  --update-baseline         Rewrite the baseline file from this run
  --diff-from <ref>         Lint only files changed since a git ref
  --detect-ai [level]       AI-filler detection; optional low|medium|high
  --detect-translationese   Translationese scoring
  --translationese-domain <d>  general|technical|literary|news
  --detect-style [level]    Both detectors plus a composite scorecard
                            (requires --format json)
  --verify                  Confirm ambiguous substitutions via online
                            translation (sends text to the network,
                            requires the translate feature)
  --telemetry               Print stderr summary counters after the run
  --document-genre <g>      casual|technical|financial; with --detect-ai
  -h, --help                Show this help

```
<!-- cli:end -->

### Example

```bash
# Single file
zhtw-mcp lint README.md

# Multiple files and directories (recursive)
zhtw-mcp lint docs/ src/locales/ README.md

# Stdin
zhtw-mcp lint -- < input.txt

# With options
zhtw-mcp lint file.md --format json --profile strict --max-errors 5
zhtw-mcp lint file.md --telemetry           # print stderr summary counters
zhtw-mcp lint file.md --format tabular              # aligned columns
zhtw-mcp lint docs/ --exclude "vendor/**"
zhtw-mcp lint -- --content-type markdown < input.md
zhtw-mcp lint -- --content-type markdown-scan-code < input.md  # also lint inside code blocks
```

### Auto-fix

```bash
zhtw-mcp lint file.md --fix                        # lexical_safe (default)
zhtw-mcp lint file.md --fix=orthographic           # punctuation/spacing/case/variant/grammar only
zhtw-mcp lint file.md --fix=lexical_contextual     # context-clue-gated and low-confidence rules too
zhtw-mcp lint file.md --fix --dry-run       # preview without writing
```

`lexical_safe` declines rules the ruleset annotates `editorial_confidence: low`,
where the flagged form is valid zh-TW and the suggestion is a register
preference rather than a correction. They are still reported; only
`lexical_contextual` rewrites them.

Declined fixes are counted in both output modes: human output appends
`N declined` to the fix line (and prints one even when nothing was applied),
and `--format json` reports `fixes_declined`. A decline means the fixer weighed
the issue and passed on it, for any of several reasons beyond editorial
confidence: multiple candidate suggestions, anchor rejection under `--verify`,
a clue gate the segmenter ran and did not confirm, or tier-2 suppression. A
higher tier will not necessarily apply them.

`fixes_skipped` is the wider JSON count and always includes `fixes_declined`.
It also counts issues that were never weighed: anything out of tier, issues
overlapping an earlier fix, and issues inside an excluded region. Out of tier
covers lexical issues under `--fix=orthographic` and clue-gated rules below
`lexical_contextual`, where the segmenter never runs. A rule that carries both
`context_clues` and `editorial_confidence: low` counts as out of tier at
`lexical_safe`, not as a decline, because the tier stopped it before the
annotation could. Read `fixes_skipped` for "how many issues did `--fix` leave
alone", `fixes_declined` for "how many did it turn down".

Reading from stdin with `--fix` makes the command a filter: the document goes
to stdout whether or not anything changed, and every status line goes to
stderr. Simplified input goes to stdout the same way even without `--fix`,
because the S2T conversion rewrites the document exactly as a fix does and
stdin has no copy on disk to recover it from. Traditional input without `--fix`
is unchanged, so nothing is emitted. `--dry-run` writes nothing to stdout, as
it does for a file.

That holds for the default human format only. `--format json`, `sarif`,
`compact`, and `tabular` put their report on stdout instead, so combining one
with a stdin rewrite discards the rewritten text. The command says so on stderr
rather than exiting quietly, because `compact` and `tabular` print nothing for
a clean document, and an empty stdout with exit 0 is indistinguishable from
success. Process a file if you need both the report and the text.

`zhtw-mcp convert` always fixes at `lexical_contextual`, so it rewrites
`editorial_confidence: low` terms unattended regardless of any `--fix` setting.
Conversion is a whole-document rewrite of Simplified input, where leaving the
judgment calls half-converted would be the worse outcome.

### Explaining flagged terms

```bash
zhtw-mcp lint file.md --explain
```

Each issue includes a cultural/linguistic annotation and its English anchor term.

### Scan caching

In lint-only mode (no `--fix`), the CLI automatically caches scan results keyed by file content hash (BLAKE3) and scan parameters. Unchanged files are skipped on subsequent runs. The cache lives at the platform default cache directory (`~/.cache/zhtw-mcp/` on Linux, `~/Library/Caches/zhtw-mcp/` on macOS) with 24-hour TTL and a 2000-entry cap. Caching is disabled when `--fix`, `--verify`, or stdin mode is active.

### Telemetry

Use `--telemetry` with `lint` to print a compact stderr summary after the run:

```bash
zhtw-mcp lint docs/ --telemetry
```

This reports processed file count plus total error/warning counts. It does not change stdout formatting or exit-code behavior.

### Network access and `ZHTW_NO_NETWORK`

`zhtw-mcp` is local-only except for `--verify`, which sends the sentence around
each finding over HTTPS to Google Translate to confirm a flagged term carries
the meaning its rule claims. Set `ZHTW_NO_NETWORK` to any value other than
empty or `0` to refuse it:

```bash
ZHTW_NO_NETWORK=1 zhtw-mcp lint --verify README.md   # exits non-zero, naming the flag
ZHTW_NO_NETWORK=1 zhtw-mcp lint README.md            # unaffected
```

The run fails rather than quietly linting without the verification that was
asked for. The switch covers `lint --verify` and `convert --verify`; ordinary
linting, fixing and converting never touch the network.

### Output formats

| Format | Flag | Description |
|--------|------|-------------|
| `human` | _(default)_ | Colored, multi-line output for terminals |
| `json` | `--format json` | Machine-readable JSON array |
| `compact` | `--format compact` | One line per issue |
| `tabular` | `--format tabular` | Aligned columns for quick scanning |
| `sarif` | `--format sarif` | SARIF v2.1.0 for GitHub Code Scanning |

## Inline suppression

A pragma is `zhtw:` plus a keyword, placed behind whatever comment opener the file already uses: `<!--` (Markdown, HTML), `//` (C-family), or `#` (YAML, TOML, Python, shell, locale files). Every keyword works behind every opener.

Two limits on `#`: it is ignored under `--content-type markdown`, where `#` starts a heading rather than a comment, and it must begin its own token, so `key: value# zhtw:ignore` is data rather than a pragma.

| Keyword | Effect |
|---|---|
| `zhtw:ignore`, `zhtw:ignore-line` | suppress the line the pragma sits on |
| `zhtw:ignore-next-line`, `zhtw:ignore-next` | suppress the following line |
| `zhtw:ignore-block` ... `zhtw:end-ignore` | suppress everything between the two markers |

Every keyword has a `disable` spelling (`zhtw:disable-next-line`, `zhtw:disable-block`), and `zhtw:enable` is accepted as a block end, for users coming from linters that pair disable with enable. A bare `zhtw:disable` suppresses one line rather than opening a block; use `zhtw:disable-block` to fence off a region.

```yaml
title: 用戶手冊  # zhtw:ignore

# zhtw:disable-block
mainland_samples:
  - 用戶
  - 數據庫
# zhtw:enable
```

```markdown
<!-- zhtw:ignore-next-line -->
這行不會被檢查。
```

An unclosed block runs to the end of the file. An unrecognized keyword suppresses nothing, so a typo fails loudly (the issue still fires) rather than silently muting a region.

## Declared languages

Under `--content-type markdown` and `--content-type markdown-scan-code`, an HTML tag carrying a `lang` attribute scopes the prose it wraps. A run marked as something other than Chinese is not linted; nothing else about it is guessed.

```markdown
<div lang="en">

We ship 軟件, and it works.

</div>

他說<span lang="en">we ship 軟件, 但</span>結束。
```

Neither `軟件` above is reported, though the same word outside the marked runs
is. The rules:

- A tag whose `lang` names a Chinese variety is scanned as usual. That covers `zh`, `zh-TW`, `zh-Hant`, `zh-CN`, `zh-Hans` and the ISO 639-3 varieties of the Chinese macrolanguage (`cmn`, `yue`, `nan`, `hak`, `wuu`, `gan`, `hsn`, `lzh`, `cdo`, `cjy`, `cnp`, `cpx`, `csp`, `czh`, `czo`, `mnp`). Only the primary subtag is read, and case does not matter. `zh-CN` is deliberately in that list: mainland vocabulary is what this linter exists to rewrite.
- Any other non-empty value, `en` or `ja-JP` or `fr`, takes the run out of the scan.
- `lang=""` means "language unknown" in HTML, so it is not a declaration that the run is not Chinese. It leaves the run scanned, and it cancels an outer declaration for the text it wraps.
- Nesting is honored: a `lang="zh-TW"` span inside a `lang="en"` block is scanned again, and a same-name tag nested inside a scope does not close it early.
- A void element (`<br lang="en">`) or a self-closed one wraps nothing, so its `lang` scopes nothing.
- Elements whose end tag is optional close on their next sibling, as they do in a browser. `<p lang="en">English<p lang="zh-TW">中文` is two paragraphs, not one inside the other, so the second is scanned. The same holds for `li`, `dt`, `dd`, `tr`, `td`, `th`, `option`, `optgroup`, `rt` and `rp`. An element in between is looked past where HTML looks past it: a `<span>` or a `<div>` does not stop the close, a `<section>` or a nested list does.
- What a `script`, `style`, `textarea`, `title`, `xmp` or `iframe` element holds is text, not markup, so a tag written inside one is a string and scopes nothing. The element's own `lang` still applies to its contents.
- A tag left unclosed scopes to the end of the document, the way an unclosed element in HTML is closed by whatever contains it.

The browser extension honors `lang` the same way, reading it from the nearest ancestor of each text node it collects.

## Translation memory

The `tm` subcommand manages the translation memory, which records
disambiguation decisions (the flagged term, the suggestion, and what was
chosen) so a call made once is reused afterwards.

<!-- cli:tm -->
```text
zhtw-mcp tm - manage translation memory

Usage:
  zhtw-mcp tm list                List recorded entries as JSON
  zhtw-mcp tm export <file>       Export entries to a file
  zhtw-mcp tm import <file>       Import entries from a file
  zhtw-mcp tm clear               Remove all entries
  zhtw-mcp tm record --found <term> --suggested <term> --chose <term>
                     [--context <text>]
                                  Record a disambiguation decision

Options:
  -h, --help            Show this help

```
<!-- cli:end -->

## Judgment cache

LLM-backed disambiguation decisions are also persisted in a separate judgment cache. To clear it:

```bash
zhtw-mcp cache clear
```

<!-- cli:cache -->
```text
zhtw-mcp cache - manage the LLM judgment cache

Usage:
  zhtw-mcp cache clear            Remove all cached disambiguation decisions

Options:
  -h, --help            Show this help

```
<!-- cli:end -->

## CI/CD integration

```bash
# SARIF output for GitHub Code Scanning
zhtw-mcp lint docs/ --format sarif > results.sarif

# Baseline mode: suppress known issues, fail only on new ones
zhtw-mcp lint docs/ --baseline baseline.json

# Lint only files changed since a branch
zhtw-mcp lint --diff-from main
```

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Linting completed and the text stayed within `--max-errors` and `--max-warnings`. |
| 1 | Linting completed and the text failed a gate. The findings are on stdout. |
| 2 | The run failed: bad arguments, unreadable config, or a file that could not be processed. Any findings printed are incomplete. |

A file that cannot be read (not UTF-8, over the 16 MiB limit, permission denied)
is reported on stderr and skipped; the rest of the batch is still linted and
still reported. Because the gate was then computed over an incomplete set, the
run exits 2 rather than 0 or 1, so a green build cannot come from a file nobody
managed to read.

## Project config file

Create `.zhtw-mcp.toml` at your project root for team-wide settings:

```toml
profile = "strict"
max_errors = 0
max_warnings = 10
exclude = ["vendor/**", "*.bak"]
packs = ["medical"]
```

Discovered by walking from cwd upward to the `.git` root. CLI flags override config values. Supported fields: `profile`, `relaxed`, `content_type`, `max_errors`, `max_warnings`, `ignore_terms`, `exclude`, `overrides`, `suppressions`, `packs`, `translation_memory`, plus the `[markdown]` and `[glossary]` sections.

`ignore_terms` keeps matching issues in the output but drops them to `info`, so they count against neither `max_errors` nor `max_warnings`. `overrides`, `suppressions` and `translation_memory` name the store files; all three are also read in server mode, so an MCP client needs no flags and the server answers from the same stores `lint` reads.

## Converting Simplified Chinese to Traditional

The `convert` subcommand converts Simplified Chinese (zh-CN) text to Traditional Chinese (zh-TW) and then applies the full lint/fix pipeline to normalize vocabulary:

<!-- cli:convert -->
```text
zhtw-mcp convert - convert Simplified Chinese (zh-CN) to Traditional (zh-TW)

Converts characters and phrases, then runs the lint/fix pipeline to
normalize vocabulary.  Corrected output goes to stdout.

Usage:
  zhtw-mcp convert <file>                  Convert a file
  zhtw-mcp convert [--] < input.txt        Convert stdin (the default)

Options:
  --content-type <ct>   plain, markdown, markdown-scan-code, or yaml
  --verify              Confirm ambiguous substitutions via online
                        translation (sends text to the network,
                        requires the translate feature)
  -h, --help            Show this help

```
<!-- cli:end -->

```bash
# Convert a file (writes corrected output to stdout)
zhtw-mcp convert file.md

# Convert from stdin
zhtw-mcp convert -- < input.txt

# Specify content type explicitly
zhtw-mcp convert file.md --content-type markdown

# Rule packs apply here too, same as they do to lint
zhtw-mcp --pack medical convert file.md
```

This is a two-stage pipeline: first a built-in character/phrase converter (SC→TC), then iterative vocabulary normalization via the standard scanner.

### `--verify` sends text off the machine

When the `translate` feature is enabled (it is, by default), `lint` and `convert` both accept `--verify` to confirm ambiguous substitutions against English anchor terms. This is the only part of either subcommand that touches the network: it sends the sentence around each unresolved issue, up to 4 KB per run, to `translate.googleapis.com`. Nothing else in the tool leaves the machine.

It is off unless you pass the flag. Build with `--no-default-features` (plus the features you want) to remove the capability entirely; `--verify` then fails with an explanatory error rather than silently doing nothing.

## Editor integration setup

<!-- cli:setup -->
```text
zhtw-mcp setup - print MCP integration config for an editor/agent host

Usage:
  zhtw-mcp setup <host>

Hosts:
  claude_code, codex, opencode, copilot, cursor, windsurf, cline, continue, generic
  translation-guide     Print the translation style guide instead

Options:
  -h, --help            Show this help

```
<!-- cli:end -->

Generate configuration snippets for MCP-capable editors:

```bash
zhtw-mcp setup claude-code
zhtw-mcp setup codex
```

Prints JSON configuration for the specified host. Available hosts depend on the build.

## Pre-commit hook

Add to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/<org>/zhtw-mcp
  hooks:
    - id: zhtw-mcp
```

The hook runs `zhtw-mcp lint` on staged Markdown, YAML, and text files.

## Rule packs

<!-- cli:pack -->
```text
zhtw-mcp pack - manage domain-specific rule packs

Usage:
  zhtw-mcp pack import <file>     Install a pack from a JSON file
  zhtw-mcp pack export <name>     Export an installed pack to <name>.json
  zhtw-mcp pack validate <file>   Validate schema and check for issues
  zhtw-mcp pack list              List installed packs

Activate packs for a run with the global flag: zhtw-mcp --pack <name> lint ...

Options:
  -h, --help            Show this help

```
<!-- cli:end -->

Domain-specific rule overlays stored as JSON files in the `packs/` subdirectory. Same schema as `overrides.json`. Layered on top of the base ruleset in `--pack` flag order.

```bash
zhtw-mcp pack import medical.json   # install a pack
zhtw-mcp pack export medical         # export a pack to medical.json
zhtw-mcp pack validate medical.json  # validate schema and check for issues
zhtw-mcp pack list                   # list installed packs
zhtw-mcp --pack medical lint file.md # activate pack for a lint run
zhtw-mcp --pack medical --pack legal # multiple packs
```
