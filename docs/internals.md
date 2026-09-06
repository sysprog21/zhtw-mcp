# Internals

## Script detection

The scanner detects Traditional vs. Simplified Chinese by counting exclusive characters. Variant rules (裏→裡, 着→著) are skipped for Simplified input. When detection is `Unknown`, variant rules still fire (conservative default).

## Processing pipeline

1. NFC normalization with byte-offset mapping
2. Content-type dispatch: Markdown (pulldown-cmark), YAML (key token exclusion), plain text (regex exclusion). `MarkdownScanCode` variant also lints inside fenced code blocks.
3. Inline suppression markers (`zhtw:ignore`, `zhtw:ignore-next-line`, `zhtw:ignore-block`/`zhtw:end-ignore`), recognized behind any of the `<!--`, `//`, and `#` comment openers; see `docs/cli.md`
4. Spelling pass: dual Aho-Corasick automata (leftmost-longest for spelling, case-insensitive for case rules); context-clue AC pre-scan for rules with `context_clues` or `negative_context_clues`
5. Punctuation pass: full-width conversion, CN curly quotes, enumeration comma, quote hierarchy, CJK spacing
6. Variant pass: character variant normalization with exception phrase checking
7. Overlap resolution: longer match wins, higher severity on tie
8. Profile filtering (e.g., `臺`/`台` only in `strict`)
9. Tier 2 local disambiguation: collocations, context clue density, profile priors. Issues scoring >= 0.6 resolve locally, < 0.3 suppressed as likely FP, [0.3, 0.6) forwarded to Tier 3
10. Tier 3 sampling (optional): gray-zone terms escalated to host LLM, results cached in persistent judgment cache

## Design decisions

- The MCP server runs on the official RMCP SDK over a single-threaded Tokio runtime, gated behind the `native` feature so the wasm and library builds pull in neither. RMCP owns method routing and server-to-client requests; `mcp/transport.rs` keeps the JSON-RPC framing contract in front of it (4 MiB line bound, -32700 for unparsable input, -32600 with the id echoed, pre-initialize rejection); the lint pipeline stays synchronous and runs on the blocking pool.
- Pure Rust, no C/C++ dependencies. MMSEG segmenter builds its dictionary from ruleset vocabulary at construction time.
- Byte-safe edits: positions from pulldown-cmark event ranges map back to original byte offsets.
- JSON ruleset (`assets/ruleset.json`) embedded via `include_str!`. Runtime overrides in platform config directory.
- SHA-256 trace IDs for reproducibility. The `uuid` crate arrives transitively through RMCP and is not used for trace IDs.
- Release binary about 9 MB on aarch64-apple-darwin (LTO + strip), against the 20 MiB `make check-size` gate.
- Sampling (step 10) only activates when running as an MCP server inside an AI assistant. The standalone CLI runs Tier 2 disambiguation but skips Tier 3 sampling, keeping gray-zone issues at their original severity.
- Persistent judgment cache (`~/.config/zhtw-mcp/judgment_cache.json`) stores LLM disambiguation results keyed on a 9-field blake3-hashed composite (ruleset_hash, prompt/disambig versions, profile, content type, normalized context, term, candidate set hash, english anchor). 30-day TTL, 10000-entry cap, atomic writes (tempfile + rename), schema-versioned with backup-and-reset. Eliminates repeated LLM calls across sessions.
- Incremental scan cache (BLAKE3-keyed, 24h TTL, 2000-entry cap) skips re-scanning unchanged files in lint-only CLI mode. Disabled for `--fix`, `--verify`, and stdin. MCP path does not use the cache (stateless by design).
- Built-in SC→TC converter (`s2t.rs` + `s2t_data.rs`) eliminates the OpenCC runtime dependency for the `convert` subcommand.
- The rhythm (氣口) axis is a capability flag on `ProfileConfig`, not a profile: `--rhythm` composes with `base` or `strict` instead of replacing one. Its findings carry `IssueType::Translationese` with a rhythm `PhaseFamily`, which is what keeps them out of the translationese score's issue-density signal and out of every fix tier. An opt-in taste flag must not move a calibrated number or rewrite a document.
- Register (`register.rs`) is resolved once per document and decides which detectors stay quiet, never what to rewrite. `RegisterMode` on `ProfileConfig` is the policy a caller sets and a batch-wide config can carry; `Register` is the answer for one text, because a config that served a whole batch could not hold it. Detection errs toward casual on purpose: a missed 公文 leaves the linter where it was, while a false formal reading silently drops real findings. Anchors such as 謹啟 and 此致 need a phrase boundary in front of them, or 台端 matches inside 平台端 and turns a technical document formal.
- Anchor calibration (`translate.rs`) annotates ambiguous issues with `anchor_match: Option<bool>` (confirmed/unconfirmed/no-signal) via synonym table and LCP stem matching. Fails open on API error (severity preserved).

## Corpus evaluation

Synthetic corpus fixtures in `tests/corpus/` drive aggregate quality metrics.

- `ai-generated.json`: zh-TW technical prose with LLM-style filler and zh-CN drift.
- `native-zh-tw.json`: clean native-style zh-TW technical prose used for false-positive checks.
- `cn-to-tw-conversion.json`: zh-CN technical prose evaluated after built-in SC->TC conversion.
- `deterministic.json`: Tier 1 regression corpus (fully solvable via rigid rules, no LLM needed).
- `ambiguous.json`: polysemous terms for Tier 2/3 disambiguation validation.
- `editorial.json`: AI filler and hedging language (density detection).
- `mixed-content.json`: markdown tables, code blocks, CJK-Latin interleaving (structural integrity).

The corpora are synthetic and repeat short seed documents enough times to exceed 50 KiB per corpus during evaluation. The test harness (`tests/corpus-evaluation.rs`) treats each seed as an independent document, weighted by its `repeat` count.

Metric definitions:

- `precision`: true-positive issue matches / all reported issues on corpora with gold positives.
- `recall`: true-positive issue matches / all gold issues on corpora with gold positives.
- `false_positive_rate`: fraction of native zh-TW documents that produced one or more issues.
- `safe_fix_success_rate`: fraction of documents whose `lexical_safe` output exactly matches the expected fixed text.

Gate thresholds: precision >= 90%, false-positive rate <= 5% on native zh-TW, safe-fix success >= 85% on AI-generated corpus.

`expected_issues` and `expected_fixed` are intentionally independent: `expected_issues` lists all scanner detections (precision/recall), while `expected_fixed` reflects `LexicalSafe` fixer output (safe-fix rate). Confusable rules and clue-gated cross_strait rules are flagged by the scanner but skipped by the fixer, so some issues appear in `expected_issues` without a corresponding replacement in `expected_fixed`.

Run `make corpus` to print the metrics table locally.

## Testing

```bash
cargo test                             # all tests
cargo test engine::scan                # specific module
cargo test --test scanner-integration  # integration tests (scanner behavior)
cargo test --test e2e-mcp              # E2E: JSON-RPC round-trip
cargo test --test vocabulary-expansion # political nouns, IT terms, context clues
cargo test --test cli-lint             # CLI: exit codes, formats, fix, SARIF, baseline
cargo test --test anchor-benchmark -- --ignored  # anchor calibration (requires network)
cargo test --test fix-tier-benchmark   # fix tier coverage
cargo test --test regression           # regression corpus (4 datasets)
cargo test --test evaluate-tier         # tier distribution + LLM avoidance metrics
cargo test corpus -- --nocapture       # corpus evaluation suite
cargo clippy                           # must be warning-free
cargo fmt --check
```
