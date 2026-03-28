# CHANGELOG

Tracks completed items from the technical roadmap (TODO.md).

## Completed

### Spec Compliance & Safety

#### 25.4 Structured error `data` field for parameter validation ([#10](https://github.com/sysprog21/zhtw-mcp/issues/10))
- Parameter validation errors in `tools/call` now return JSON-RPC
  `INVALID_PARAMS (-32602)` with structured `data` instead of tool-level
  `CallToolResult` errors
- `data` carries `{field, value, accepted}` for enum params (e.g. invalid
  profile), `{field, expected_type, actual_type}` for type mismatch, and
  `{field}` for missing required params — clients render actionable
  diagnostics without parsing error message strings
- `tool_check()` returns `Result<CallToolResult, JsonRpcResponse>`:
  validation failures short-circuit as JSON-RPC errors before tool logic
- All parse functions reject wrong JSON types via `optional_str_validated`
  helper; non-object `arguments` rejected up front with structured type info
- Scope: `mcp/tools.rs` (276 LOC refactored), `tests/e2e_mcp.rs` (97 LOC,
  E2E test verifies `data.field` on invalid profile)

#### 31.5 Reject unknown parameters in tools/call ([#15](https://github.com/sysprog21/zhtw-mcp/issues/15))
- `handle_tools_call` now validates all keys in `params.arguments` against a
  known-params list before processing; unrecognized keys (e.g. `max_error`
  instead of `max_errors`) return `INVALID_PARAMS (-32602)` with
  `data: {"unexpected": ["<key>", ...]}` immediately
- Known-params list is `cfg`-gated: `verify` only accepted when the `translate`
  feature is enabled, matching the tool schema exactly
- Scope: `mcp/tools.rs` (77 LOC), `mcp/types.rs` (13 LOC), `tests/e2e_mcp.rs`
  (80 LOC, E2E tests for unknown key rejection)

#### 31.4 Handle MCP shutdown and exit lifecycle methods ([#14](https://github.com/sysprog21/zhtw-mcp/issues/14))
- `shutdown` (request): responds `{}`, sets `shutting_down` flag, rejects subsequent
  requests with `INVALID_REQUEST (-32600)`
- `exit` (notification): terminates with code 0 after prior shutdown, code 1 without
- Both handled as early guards in `dispatch_preinit` before the method match, so no
  match arm ordering can leak post-shutdown requests through
- Scope: `mcp/tools.rs` (~39 LOC), `tests/e2e_mcp.rs` (195 LOC, 4 E2E tests)

#### 39.1 Sampling bridge input sanitization
- `SamplingBridge` wraps user-controlled text (context window, matched term) in
  randomized per-request delimiter tags (e.g. `<text_fragment_8f2b4c>...</text_fragment_8f2b4c>`)
  before sending to host LLM via `sampling/createMessage`
- `systemPrompt` declares delimited content as inert data — host LLMs obey system-level
  instructions more reliably against indirect prompt injection
- NFC-normalizes context windows before inclusion; uses OS-seeded `RandomState` for
  unpredictable hex nonces
- Split shared system prompt's response-format instruction into a per-method parameter:
  disambiguation expects a bare term, bulk confirmation expects a JSON map
- Scope: `mcp/sampling.rs` (~262 LOC net change, tests verify parameterized format
  reaches the wire)

#### 25.3 Sampling budget exhaustion observable in tool response ([#8](https://github.com/sysprog21/zhtw-mcp/issues/8))
- When sampling budget (default 5) is exhausted, ambiguous issues previously returned
  unrefined with no signal to the client
- `refine_issues_with_sampling` now returns `SamplingStats { used, skipped }` instead of
  void; skipped count includes both issues collected but not sampled (budget ran out
  mid-loop) and eligible issues beyond 10x-budget collection cap (`uncollected_skipped`)
- `sampling_used` and `sampling_skipped` fields added to `IssueSummary` in tool response
  JSON; `skip_serializing_if` adds zero tokens when sampling is inactive
- All output modes (full, compact, tabular, summary) inherit the fields through
  `IssueSummary`
- Unit tests: zero-sampling omission (no fields in JSON), active-sampling presence
  (fields with correct counts), stat propagation through `build_check_output`
- Scope: `mcp/sampling.rs` (`SamplingStats` struct, `refine_issues_with_sampling` return
  type), `mcp/tools.rs` (`IssueSummary` fields, plumbing through all output paths)

### Performance

#### 39.2 Incremental scan caching for repeated file analysis
- Two-tier scan cache (`src/cache.rs`) skips the entire scan pipeline for
  unchanged files in CLI `lint` and directory scans:
  1. Fast path: `stat(file)` checks `(path, mtime, size, params)` — no file read
  2. Slow path: BLAKE3 content hash for mtime-miss files (content unchanged after `touch`)
- Cache key: `(file_path, ruleset_hash, profile, content_type, fix_mode)` hashed via BLAKE3
- Storage: bincode binary format at platform cache dir (e.g. `~/Library/Caches/zhtw-mcp/scan-cache.bin`
  on macOS, `~/.cache/zhtw-mcp/scan-cache.bin` on Linux). TTL 24h, MAX_ENTRIES 2000 cap
- Atomic writes via `tempfile` fd + `persist()` rename; `fs2` exclusive flock on `.lock` sidecar
  prevents concurrent CLI processes from clobbering each other
- TOCTOU-safe: `File::open` then `.metadata()` on the fd, read from same fd
- Cache disabled when `--fix` active or `--verify` active (both need fresh scan results)
- `CacheHit` preserves `input_was_sc` flag so SC files fall through to S2T write-back
  instead of returning empty text on fast-path hit
- Rayon `par_iter` for multi-file parallel scanning with `Mutex<ScanCache>` shared across threads
- Eager text drop after scan when text not needed (guards: `input_was_sc`, `fix_mode`, `verify`)
  to prevent OOM on large parallel scans
- Spelling scan: lazy `clue_hits_cache` defers O(n) clue AC pass until a rule actually needs
  context-clue checking (~7% of rules); paragraph break search clamped to `CONTEXT_WINDOW_CHARS * 4`
  bytes to avoid O(N) per-match scans
- Clue index: `u16::try_from().expect()` instead of `debug_assert` (catches overflow in release builds)
- 5 unit tests (fast-path hit/miss, slow-path content check, persistence, TTL expiry, overflow eviction)
- New deps: `blake3`, `bincode`, `rayon`, `fs2`

#### 43.8 Per-match hot-path optimizations
- Spelling AC scan dominates total scan time (~100% of `full_default`
  at 11.7ms/100KB); context-clue text runs 1.6-1.9x slower per byte.
  Three changes reduce per-match overhead to near-zero:
- **NFC normalization fast-path**: `is_nfc_quick()` 3-way dispatch
  (`Yes`/`Maybe`/`No`) avoids the O(n) `is_nfc()` walk for definitive
  results. Common zh-TW input (already NFC) takes the `Yes` path with
  no allocation; `No` skips straight to normalize without a redundant
  exact check
- **Windowed clue AC scan**: replaced full-document `build_clue_hits()`
  pre-scan (O(n) + sort + `Vec` alloc) with `scan_clues_in_window()`
  that runs the clue AC only over the bounded ~160-byte context window
  per match. Zero allocation; early exit on negative-clue veto
- **AC absorption for exceptions/superstrings**: 52 exception phrases
  and superstring `to` forms injected into the `LeftmostLongest`
  spelling AC at `Scanner::new()` time. The longer absorber pattern
  prevents the shorter `from` match from ever being reported,
  converting per-match O(E) exception iteration and O(T) superstring
  checks into a single build-time cost. Two-part overlap-collision
  filter (containment + right-boundary suffix-prefix check) rejects
  absorbers that would shadow unrelated rules
- **Word-boundary straddle memoization** (PR #44): `boundary_cache`
  (`HashMap<usize, bool>`) deduplicates MMSEG trie traversals across
  clustered AC matches sharing the same byte offset
- Scope: `engine/normalize.rs`, `engine/scan/mod.rs`,
  `engine/scan/spelling.rs`; `force_bytewise()` includes absorber
  patterns for charwise/bytewise test parity

#### 20.2 Profile-guided dict construction cost
- Measured `Scanner::new()` construction time: ~2.0ms (target threshold: 5ms)
- Breakdown via `construction_breakdown` criterion bench group:
  - Spelling Aho-Corasick (776 patterns): ~1.5ms (73%)
  - `Segmenter::from_rules()` (~2312 dict entries): ~0.23ms (11%)
  - Case Aho-Corasick (15 patterns): ~0.05ms (2%)
  - Filtering/collect overhead: ~0.17ms (8%)
- Criterion baseline saved (`--save-baseline main`) for regression tracking
- Verdict: no optimization needed at current scale. `OnceLock<AhoCorasick>` and `phf`
  deferred indefinitely — construction is a one-shot cost at server startup

#### 45.2 Specialized rule filter fast paths
- Two-step optimization inspired by regex VM instruction sets (Cox,
  MoonBit, Odin): compile rule-specific execution paths at build time
  to eliminate dead branches at match time
- **Step 1 — per-rule bitflags** (PR #49): six `FILTER_*` constants in a
  `u8` bitmask gate optional filter stages (superstring, exceptions,
  context clues, positional clues, deletion).  79% of rules have
  flags=0, bypassing all guarded blocks.  Criterion A/B on 100KB:
  spelling_only -49%, full_default -36%
- **Step 2 — monomorphic fast paths**: const-generic
  `process_match_dispatch<CLASS>` monomorphized into three variants
  (CLASS_SIMPLE / CLASS_CLUED / CLASS_FULL).  Rule classes precomputed
  from filter flags at `Scanner::new()` time, stored in parallel
  `rule_classes: Vec<u8>`.  Distribution: 83% simple, 17% clued, 0%
  full.  The compiler dead-code-eliminates context-clue window
  computation and positional-clue checks for simpler rule classes.
  Absorption sentinels filtered before dispatch (continue vs
  return-from-callee).  Criterion A/B on 100KB:
  spelling_only -21% (30.2 -> 23.8 ms), full_default -29%
  (37.4 -> 26.6 ms)
- **Combined improvement** vs pre-bitflags baseline: ~60%
  spelling_only, ~55% full_default
- Step 3 (bytecode micro-VM) deferred: no evidence of further gains
- Scope: `engine/scan/spelling.rs`, `engine/scan/mod.rs`
- Gate: 965 tests pass, clippy clean, identical scan output

#### 30.5 Avoid redundant exclusion rebuilds in fix path ([#4](https://github.com/sysprog21/zhtw-mcp/issues/4))
- MCP fix path re-scanned post-fix text via `scan_for_content_type_with_config()`,
  which rebuilt all exclusion zones (URLs, code blocks, paths, suppression markers)
  from scratch on every fix cycle
- Added `remap_exclusions()` in `fixer.rs`: O(E + F) merge-style forward pass
  that shifts exclusion zone boundaries by accumulated fix deltas instead of
  re-parsing the full document
- Re-scan now calls `scan_with_prebuilt_excluded_config()` with remapped zones;
  NFC fallback in `scan_nfc_with_content_type()` still discards prebuilt zones
  when normalization changes byte offsets, preserving correctness
- Boundary condition for zero-length insertions (spacing fixes with `old_len == 0`)
  uses `fix_end = offset + old_len` with `saturating_add`, ensuring insertions at
  an exclusion boundary shift the zone right
- Scope: `fixer.rs`, `mcp/tools.rs`, `tests/exclusion-remap.rs`
- Gate: 50 KB markdown, 408 fixes, 100 iterations -- OLD (rebuild) 6921 us,
  NEW (remap) 4532 us, 34.5% speedup. 975 tests pass, identical scan output

#### 48.x sub-tasks 2-6: AC rule IR refactor
- Replaced monomorphized `process_match_dispatch<CLASS>` with a data-driven
  IR: `MatchPredicate` enum with 10 variants, `CompiledSpellingDb` extracts
  AC automata + compiled rules from Scanner, `compile_rule_predicates()`
  builds ordered predicate chains per rule
- `eval_simple` inlined fast path for CLASS_SIMPLE (82% of rules): no Vec
  iteration, no enum matching, cached `rule_type` and `filter_flags` in
  `CompiledRule` to eliminate pointer chase through `spelling_rules`
- CLASS_TRULY_SIMPLE (no superstring/exception/deletion) inlined directly
  in the AC loop: no MatchContext construction, no function call overhead
- Predicate reordering: clue checks before boundary straddle (cheap reject
  before expensive segmenter)
- Fused pos+neg clue into single `CheckClues` predicate (one window, one
  AC pass instead of two separate computations)
- Profile fast-reject at AC loop level before MatchContext construction
  (eliminates ~5% of hits under default profile)
- Precomputed exception/superstring substring offsets at compile time
  (eliminates per-hit `match_indices` calls)
- 18 differential parity tests in `tests/ir-parity.rs`
- Scope: new `src/engine/scan/rule_ir.rs` (1040 LOC), `spelling.rs`,
  `mod.rs`, `tests/ir-parity.rs`

#### 48.x sub-task 8: Internal scratch space
- `ScratchSpace` struct with reusable buffers: `issues: Vec<Issue>`,
  `clue_index: Vec<(usize, u16)>`, `overlap_order/keep/accepted`
- `scan_with_config_into()` reuses caller-provided scratch; existing
  `scan_with_config()` wraps with fresh scratch for API compat
- `resolve_overlaps_with_scratch()` reuses sort/keep/accepted buffers
- `build_clue_index_into()` appends into caller buffer instead of allocating
- MCP hot path benefit: single ScratchSpace reused across requests
- Scope: `mod.rs`, `overlap.rs`, `rule_ir.rs`

#### 49.1 Typed JSON/SARIF output
- Replaced `serde_json::Value` tree construction with typed output structs
  deriving `Serialize` for direct serialization
- Scope: `src/main.rs`

#### 49.2 Build-time postcard ruleset
- `build.rs` deserializes `assets/ruleset.json` and serializes to postcard
  binary format via `include_bytes!`
- Runtime: `postcard::from_bytes` (~10x faster than `serde_json::from_str`)
- Full field-by-field parity test: all SpellingRule and CaseRule fields
  compared between JSON and postcard deserialization
- Scope: new `build.rs`, `src/rules/loader.rs`
- New dep: `postcard`

#### 49.3 Lazy scan cache loading
- Cache entries loaded on first lookup instead of eagerly at `open()`
- Scope: `src/cache.rs`

#### 45.1 Grammar AC prefilter
- 35 trigger patterns from 8 grammar scanners collected into a single
  `aho-corasick` automaton, replacing 8 sequential O(P*N) `str::find()`
  loops with one O(N) AC pass + per-type validator dispatch
- Grammar scan time: 1.01ms -> 0.62ms (-38% on 100KB)
- 11 differential tests verify AC path matches legacy scanners
- Old scanner functions gated behind `#[cfg(test)]` for differential testing
- Scope: `src/engine/scan/grammar.rs`

#### Document-wide boundary crossing bitmap
- `BoundaryBitmap` precomputes which byte positions have non-rule dictionary
  words crossing them, with `min_cross_start` for exact end-boundary
  resolution
- Start boundary: bitmap is authoritative (O(1) array lookup)
- End boundary: `min_cross_start[end] <= match_start` answers the
  `no_walk_after` question without segmenter fallback
- Eliminates ~100% of per-hit segmenter calls on 100KB+ documents
- Lazy threshold: bitmap only built for `text.len() > 4096`; small files
  use per-hit segmenter directly
- Scope: `src/engine/segment.rs` (`BoundaryBitmap`), `rule_ir.rs` (eval)

#### Document-wide clue pre-scan
- Single clue AC pass over full text at scan start, producing sorted
  `Vec<(byte_offset, clue_id)>` index
- Per-hit clue check replaced with binary search into precomputed index
- context_clues/100KB: 36.1ms -> 17.9ms (-50%)
- Scope: `rule_ir.rs` (`build_clue_index_into`, `lookup_clues_in_window`)

#### Lazy issue construction
- Spelling eval defers `found`/`suggestions`/`context`/`english`/
  `context_clues` allocation until after overlap resolution discards ~20%
  of hits
- `inflate_spelling_issues()` fills surviving issues from compiled DB
- Deletion rules: `found` shows `rule.from` (the phrase to delete), not
  the extended span with absorbed trailing punctuation
- Scope: `rule_ir.rs`, `mod.rs`, `ruleset.rs` (`spelling_rule_idx` field)

#### Fused detect + LineIndex + BoundaryBitmap pass
- `detect_type_lineindex_and_bitmap()` shares one `char_indices()` iteration
  for SC/TC detection, newline recording, and boundary bitmap construction
- Eliminates two redundant O(N) passes over the text
- Scope: `mod.rs`

#### Incremental column counting
- `fill_line_col_sorted()` maintains `(cursor_byte, cursor_col)` across
  sorted issues; resumes UTF-16 counting from last position instead of
  re-scanning from line start
- Line/col fill moved after final sort to fix grammar issue coordinates
- Scope: `src/engine/lineindex.rs`, `mod.rs`

#### Post-scan pipeline optimizations
- Removed `boundary_cache` HashMap (leftmost-longest AC makes start
  position caching useless overhead)
- Combined boundary helper (`match_straddles_word_boundary`) replaces two
  separate calls
- Cursor-aware deletion span extension (reuses exclusion cursor, avoids
  `is_excluded` binary search)

#### Cumulative measurements (node1, 100KB, vs 44.2 baseline)
- scan/100KB:          16.2ms -> 11.3ms  (-30%)
- spelling_only/100KB: 13.6ms ->  9.7ms  (-29%)
- full_default/100KB:  15.4ms -> 11.0ms  (-29%)
- context_clues/100KB: 36.1ms -> 17.9ms  (-50%)
- grammar_only/100KB:   1.0ms ->  0.6ms  (-38%)
- ac_traversal_only:      N/A ->  175us  (DAAC floor reference)
- construction:        8.65ms -> 8.78ms  (+1.5%, acceptable)

### Improvements (audit-identified)

#### I.1 Input size limit for `tool_check`
- `MAX_TEXT_BYTES = 256 * 1024` constant on `Server`; `tool_check` rejects requests exceeding 256 KiB of UTF-8 `text` with a structured error before any processing begins
- Prevents CPU/memory amplification from large single requests through the NFC + Aho-Corasick + Markdown + segmentation pipeline
- E2E tests: boundary at exactly 256 KiB passes, 256 KiB + 1 byte rejected

#### I.2 Error propagation in `OverrideStore::open`
- `create_dir_all` and `fs::write` errors now propagated with path and operation context (was silently swallowed via `let _ = ...`)
- Matches `SuppressionStore::open` error handling pattern
- Common `write_default_json()` helper extracted for both stores (code-simplifier)

#### I.3 Test coverage: CLI lint, markdown E2E, strict_moe variants
- New `tests/cli_lint.rs`: 8 integration tests covering exit codes, human/JSON formats, profile selection, `--max-errors` gate, `--content-type markdown/plain`, `.md` auto-detection
- E2E MCP tests expanded: `content_type: "markdown"` (code block exclusion), `profile: "strict_moe"` (variant rules fire), gate rejection (`accepted: false`, `isError: true`), `fix_mode: "aggressive"`, boundary-condition size limit, missing `text` field, invalid `content_type` rejected
- 395 tests (296 unit + 8 CLI + 62 integration + 1 E2E + 28 vocab)

#### I.4 CLI lint `--content-type markdown` support
- New `--content-type plain|markdown` flag for `lint` subcommand
- Auto-detection from `.md`/`.markdown` extension (case-insensitive); explicit flag overrides
- `--content-type` rejected outside `lint` subcommand (Codex review finding)
- Markdown mode uses `scan_markdown_profiled` (pulldown-cmark) instead of regex-only exclusion

#### I.5 Aggressive mode context-clue bypass fix
- When `segmenter` is `None` in aggressive mode, rules with `context_clues` are now skipped (was falling through to unconditional application)
- Prevents false positive corrections when direct callers of `apply_fixes` (CLI, library consumers) don't provide a segmenter
- Fixer context-clue logic flattened from 4-level nesting to 2-level (code-simplifier)

#### I.6 `punct_issue` length fix
- `punct_issue` now uses `found.len()` instead of hardcoded `length: 1`
- Correct for all current callers (ASCII punctuation) and future-proof for multi-byte found strings
- Unit test: verifies `length` matches for both single-byte and multi-byte inputs

#### I.7 `scan_ellipsis` single-pass optimization
- Merged 3 separate linear scans (ASCII dots, circle periods, single ellipsis) into 1 byte-level pass
- O(3n) -> O(n); improved cache locality for 100KB+ documents
- Byte-level pattern matching: `'.'` (0x2E), `'。'` (E3 80 82), `'…'` (E2 80 A6)
- `ellipsis_issue` inner function eliminated; uses module-level `punct_issue_sev` (code-simplifier)

#### Review refinements (Gemini + Codex consensus)
- `parse_content_type` changed from silent default to `Result<ContentType, CallToolResult>`: unknown values (e.g. `"html"`) return explicit error instead of silently defaulting to plain
- `.md` extension detection made case-insensitive (`to_ascii_lowercase`)
- `--content-type` flag rejected outside `lint` subcommand
- Oversized request test uses boundary conditions (exactly 256 KiB passes, +1 byte rejected)

#### Code-simplifier refinements
- `tools.rs`: extracted `scan_for_content_type()` method and `build_check_output()` function, eliminating ~30 lines of duplication between lint-only and fix paths
- `store.rs`: extracted `write_default_json()` and `backup_file()` helpers, shared between `OverrideStore` and `SuppressionStore`
- `fixer.rs`: flattened context-clue check from 4-level nesting (25 lines) to flat 2-level structure (15 lines) using `is_some_and` chains
- `scan.rs`: eliminated `ellipsis_issue` inner function; `punct_issue_sev` as single constructor with `punct_issue` as convenience wrapper
- `tests/cli_lint.rs`: extracted `run_lint_stdin()` helper for the 6 tests sharing the spawn/pipe/wait pattern

### Tier 1: Structural Foundations

#### 1.1 Line/column position reporting
- `Issue` struct gains `line` (1-based) and `col` (1-based, UTF-16 code units) fields
- `LineIndex` helper pre-computes newline byte positions for O(log n) offset-to-position conversion
- UTF-16 column encoding matches LSP default; UTF-32 mode available via `ColumnEncoding` enum
- Tests: multi-line input with mixed ASCII, CJK, and emoji verify correct column values

#### 1.7 Deterministic output contract
- Issues sorted by: byte offset ascending, severity descending, rule_type discriminant
- `issue_type_ord()` assigns stable numeric ordering to each `IssueType` variant
- Ordering contract documented in `zh_lint` tool schema description
- Tests: mixed-type issues assert exact output order

#### 1.2 Unicode normalization policy
- NFC normalization applied before scanning via `normalize.rs`
- Fast path: `is_nfc()` check skips allocation when input is already NFC (common for CJK text)
- Byte-offset mapping tracks how each normalized byte relates to the original input
- Robust combining mark handling: correctly skips absorbed/reordered marks during mapping
- Issue offsets mapped back to original text positions after scanning
- Tests: composed vs decomposed input, surrounding text, CJK text, mixed content

#### 1.5 Inline suppression mechanism
- Suppression markers: `<!-- zhtw:ignore-next-line -->`, `<!-- zhtw:ignore-block -->` / `<!-- zhtw:end-ignore -->`, `// zhtw:ignore`
- Suppression ranges merged into excluded ranges before scanning
- Unclosed blocks suppress from marker to end of text
- Tests: next-line, block, unclosed block, inline code, multiple suppressions, empty text

#### 1.6 Minimal profile/config framework
- `Profile` enum: `Default`, `StrictMoe`, `UiStrings` with `name()`, `description()`, `from_str_lossy()`
- `zh_lint` accepts optional `profile` parameter; `Default` behaves identically to prior behavior
- `zh_profile_list` returns real `Profile::ALL` enum values instead of hardcoded JSON
- Profile-conditional rule behavior deferred to later batches (2.2, 2.5, 3.2)

#### 1.8 CI / pre-commit integration mode
- `zhtw-mcp lint <file|-->`: reads file or stdin, runs scanner with embedded ruleset (no sled DB)
- Human format (default): one diagnostic per line to stderr (`line:col: severity [type] 'found' -> suggestions`)
- JSON format (`--format json`): structured output to stdout for machine consumption
- `--max-errors N`: exit 0 when issues <= N, exit 1 otherwise (default: 0)
- `--profile P`: select linting profile (default, strict_moe, ui_strings)

#### 1.3 Markdown-aware text extraction
- `pulldown-cmark` 0.12 integration via `into_offset_iter()` for structural Markdown parsing
- New `markdown.rs` module: `build_markdown_excluded_ranges()` excludes fenced/indented code blocks, inline code, HTML blocks/tags, and YAML frontmatter
- YAML frontmatter pre-pass: detects leading `---` fences, marks range as excluded before pulldown-cmark parsing
- `Scanner::scan_markdown()` method combines non-backtick regex exclusions (URLs, paths, mentions) with pulldown-cmark structural exclusions and inline suppression markers
- `content_type` parameter added to `zh_lint`, `zh_apply_fixes`, and `zh_finalize` tool schemas (values: `plain`, `markdown`; default: `plain`)
- `build_exclusions_for_content_type()` centralizes exclusion logic for tool handlers, ensuring consistent suppression handling across both plain and markdown modes
- Tests: fenced code blocks, inline code, YAML frontmatter, HTML blocks, indented code blocks, suppression in markdown mode, mixed exclusions, URLs in markdown mode

#### 1.4 Deprecate regex-based backtick exclusion
- `build_excluded_ranges_no_backticks()` in `excluded.rs` provides URL/path/mention exclusion without backtick regex patterns
- When `content_type=markdown`, pulldown-cmark handles all code block/inline code exclusion, making regex backtick patterns redundant
- Regex backtick patterns preserved for `content_type=plain` (backward compatibility)
- All 62 determiner_compat integration tests pass under both paths

### Tier 2: Punctuation Coverage

#### 2.1 Remaining full-width marks (!, ?, ;, (, ))
- `scan_punctuation()` extended to handle `!` → `！`, `?` → `？`, `；` → `；` with same CJK-adjacency heuristic (one CJK side sufficient)
- `(` → `（`, `)` → `）` use stricter `immediate_cjk()` (no whitespace skip, CJK on both sides required) to avoid flagging markdown links, function calls, spaced notes
- `immediate_cjk()` helper added alongside `adjacent_cjk()`, sharing `adjacent_cjk_inner()` with configurable whitespace skip
- Tests: 7 tests covering CJK-adjacent, ASCII-only, URL-excluded, markdown-like, function call contexts

#### 2.2 Full-width colon
- Half-width `:` flagged adjacent to CJK → `：` suggestion
- Exception: `Profile::UiStrings` allows half-width `:` (common in UI label:value patterns)
- Guards: skip `:` in time formats (digit:digit), protocol patterns (://)
- Tests: CJK context, time format, URL, UiStrings profile, Default profile

#### 2.3 Enumeration comma (dunhao)
- `scan_dunhao()` detects runs of short items (≤4 chars) separated by full-width `，`
- Requires 3+ items (2+ commas in a run) to trigger suggestion: `，` → `、`
- Severity: `Info` (advisory, not Error) — heuristic may have false positives
- Tests: short item list, long clauses, two-item non-trigger

#### 2.4 Quotation mark hierarchy
- `fix_quote_pairing()` enhanced with depth-based nesting: even depth → `「」` (primary), odd depth → `『』` (secondary)
- Smart dual-mode: trial run checks if character-based open/close never underflows; falls back to alternation for ambiguous quotes
- Paragraph break (`\n\n`) resets depth and position counter, preventing unclosed quotes from corrupting subsequent paragraphs
- Tests: basic conversion, nested depth, paragraph break reset

#### 2.5 Range indicator normalization
- `scan_range_indicators()` detects `~` and `-` in CJK context
- `~`: flagged when both sides are digit/CJK with at least one CJK character
- `-`: flagged only when both adjacent characters are CJK (very conservative, avoids dates/negative numbers)
- Profile-dependent suggestion: `Profile::UiStrings` → `–` (en dash), others → `～` (wave dash)
- Tests: CJK context, pure ASCII, UiStrings en dash, CJK hyphen, digit-only, URL exclusion

#### Profile threading
- `scan_with_excluded()` gains `profile: Profile` parameter
- New `scan_profiled()` and `scan_markdown_profiled()` methods; `scan()`/`scan_markdown()` delegate with `Profile::Default`
- `parse_profile()` helper in `tools.rs` extracts profile from MCP tool arguments
- Profile threaded through `zh_lint`, `zh_apply_fixes`, `zh_finalize` tool handlers and CLI lint mode
- Backward compatible: existing callers (112+ unit/integration tests) unchanged

### Tier 3: Character Variant Normalization (異體字)

#### 3.1 Variant normalization engine
- `RuleType::Variant` and `IssueType::Variant` added to type system
- Variant rules reuse existing Aho-Corasick spelling pipeline (no separate engine)
- Profile gating: variant rules only fire under `Profile::StrictMoe`; `Default` and `UiStrings` skip them
- `traditional_only: true` ensures variants don't fire on Simplified Chinese text
- `exceptions` field added to `SpellingRule` (`Option<Vec<String>>`, serde default)
- Exception phrase checking: if a variant match falls inside a known exception phrase, skip it
- 39 single-character pairs curated from OpenCC `TWVariants.txt` (ground truth for MoE standard forms)
- `issue_type_ord()` assigns Variant ordinal 6 for deterministic output ordering
- Tests: fires under StrictMoe, silent under Default/UiStrings, multiple variants, exception skipping, traditional_only gating

#### 3.2 The `臺`/`台` question
- Phrase-level matching (not blanket character replacement): `台灣`→`臺灣`, `台北`→`臺北`, `台中`→`臺中`, `台南`→`臺南`, `台東`→`臺東`, `台大`→`臺大`
- Only active under `Profile::StrictMoe`; `Default` profile does not flag `台`
- Exception phrases (平台, 月台, 舞台, etc.) naturally excluded by phrase-level matching — no pattern matches them
- Tests: fires under StrictMoe, silent under Default, 平台 not flagged

#### 3.3 Variant rule representation in ruleset.json
- Variants folded into `spelling_rules` with `"type": "variant"` (no separate array)
- Format: `{ "from": "裏", "to": ["裡"], "type": "variant", "traditional_only": true, "context": "MoE 標準字體" }`
- `exceptions` field: list of phrases where the non-standard form is correct (new `SpellingRule` field)
- `check-ruleset.py --lint` validates variant rules: `traditional_only` must be true, exactly 1 tw entry
- Ruleset: 423 spelling rules (378 original + 39 character variants + 6 臺/台 phrases) + 15 case rules
- 284 tests (221 unit + 62 integration + 1 E2E), zero clippy warnings

#### 3.4 sled schema versioning
- `SCHEMA_VERSION: u32 = 2` constant in `store.rs`; bumped from implicit 1 (original layout) to 2 (`SpellingRule` gained `exceptions` field)
- `import_ruleset()` writes `schema_version` to the `meta` tree alongside `source_hash`
- `read_schema_version()` reads stored version; returns 1 (implicit) for pre-3.4 databases
- `schema_version_matches()` compares stored version against `SCHEMA_VERSION`
- Proactive check in `main.rs`: on startup, if DB exists but version mismatches, re-import embedded ruleset and clear stale overrides before attempting to load
- Reactive fallback preserved: if `Server::new()` still fails after version check, re-imports as last resort
- Tests: version written on import, legacy DB returns implicit 1, version mismatch triggers re-import
- 287 tests (224 unit + 62 integration + 1 E2E), zero clippy warnings

### Tier 5: MCP Protocol Expansion

#### 5.4 Transport and capability prerequisites
- `ClientCapabilitiesRaw` / `ClientCapabilities` structs parse client capabilities from `initialize` handshake
- `ServerCapabilities` expanded to declare `resources`, `prompts`, and `tools`
- `handle_initialize` stores parsed client capabilities for downstream use (sampling gating)
- New types: `ResourceCapability`, `PromptCapability`, `ResourceDef`, `ResourcesListResult`, `ResourceReadParams`, `ResourceContent`, `ResourceReadResult`, `PromptDef`, `PromptArgDef`, `PromptGetParams`, `PromptMessage`, `PromptContent`, `PromptGetResult`
- Transport dispatch routes `resources/list`, `resources/read`, `prompts/list`, `prompts/get` to Server handlers

#### 5.1 MCP Resources
- New `src/mcp/resources.rs` module: `list_resources()` and `read_resource()`
- `zh-tw://style-guide/moe`: Markdown summary of MoE punctuation, variant, and vocabulary standards
- `zh-tw://dictionary/ambiguous`: JSON array of terms with `english` field for LLM disambiguation
- Content generated from embedded ruleset at request time (no caching needed)
- Tests: 4 unit tests (list, style guide read, ambiguous dict filtering, unknown URI)
- E2E: `resources/list` returns 2 resources; `resources/read` returns style guide content

#### 5.2 MCP Prompts
- New `src/mcp/prompts.rs` module: `list_prompts()` and `get_prompt()`
- `normalize_tone` prompt: system prompt instructing LLM to write in Taipei professional tone with MoE conventions
- Prompt references `zh-tw://style-guide/moe` for hosts supporting resource injection
- Tests: 3 unit tests (list, get, unknown name)
- E2E: `prompts/list` returns normalize_tone; `prompts/get` returns messages with Traditional Chinese content

#### 5.3 explain_variant tool
- New `zh_explain_variant` MCP tool: explain why MoE prefers a standard character form
- Input: `{ "char": "裏" }` or `{ "pair": "着/著" }`
- Output: structured JSON with `non_standard_form`, `standard_form`, `explanation`, `context`, `exceptions`
- Error path: caps hint list to 20 entries (prevents DoS from dumping all variant rules)
- E2E tests: successful lookup, unknown character with capped hints

### Tier 6: Profile System

#### 6.1 Profile definitions
- `ProfileConfig` struct with 9 boolean flags controlling processing chain stages
- `Profile::config()` method returns the configuration for each profile
- `scan_with_excluded()` refactored: uses `ProfileConfig` instead of ad-hoc profile matching
- `scan_spelling`, `scan_punctuation`, `scan_range_indicators` signatures updated to accept `&ProfileConfig`
- Tests: 6 profile chain tests (StrictMoe catches variants, UiStrings skips dunhao/colon, en dash, wave dash, config consistency)

#### 6.2 Political stance profiles
- `PoliticalStance` enum: `RocCentric`, `International`, `Neutral` — orthogonal to main `Profile` enum
- `PoliticalStance::allows_rule(from)`: identity-loaded terms (內地, 大陸同胞, 祖國) suppressed under `International`; all political_coloring rules suppressed under `Neutral`
- `ProfileConfig::political_stance` field + `with_stance()` builder method
- `scan_with_config()` method accepts `ProfileConfig` directly, filtering political_coloring rules at scan time
- `filter_by_stance()` in tools.rs for post-scan filtering in MCP tool handlers
- `political_stance` parameter added to `zh_lint`, `zh_apply_fixes`, `zh_finalize` tool schemas
- `zh_profile_list` returns available stances alongside profiles
- 7 integration tests: RocCentric flags all, International skips identity terms, Neutral suppresses all political, unit logic tests

### Tier 4: Vocabulary Expansion & Segmentation

#### 4.1 IT/software terminology audit
- Audited existing rules against spec term table and OpenCC STPhrases
- Added `概率`→`機率` (probability) rule
- Most terms from the spec already present (373 cross_strait rules)

#### 4.2 Political/regional proper nouns
- 19 country name rules as `cross_strait` (老撾→寮國, 沙特→沙烏地, 新西蘭→紐西蘭, 意大利→義大利, etc.)
- 2 international body rules as `political_coloring` (東盟→東協, 英聯邦→大英國協)
- All rules include `english` field for disambiguation
- 16 integration tests in `tests/vocabulary_expansion.rs`
- Ruleset expanded to 445 spelling + 15 case rules

#### 4.3 Ambiguity-aware rules with context clues
- `context_clues` field added to `SpellingRule`: list of surrounding words suggesting intended meaning
- `context_clues` propagated to `Issue` struct for fixer access
- `apply_fixes_with_context()`: new fixer function accepting optional `Segmenter`
  - Safe mode: refuses rules with `context_clues` (ambiguous)
  - Aggressive mode: checks if 2+ clue words found in surrounding text window; applies only when context is clear
- `surrounding_window()`: extracts 40-char context window around match for clue scanning
- `Segmenter::from_rules()` now includes context_clue words in dictionary
- Server wired: `Segmenter` built in `Server::new()`, passed to `apply_fixes_with_context` in tool handlers
- Rules with context_clues: 程序 (program), 質量 (quality), 接口 (interface), 並行 (parallelism)
- 6 new fixer unit tests + 5 integration tests for context-aware disambiguation

#### 4.4 Lightweight segmentation engine
- `src/engine/segment.rs`: Forward Maximum Matching (FMM) word segmenter
- `Segmenter` with `HashSet<String>` dictionary, `Token` struct with text/offset/in_dict
- `from_rules()` builds dict from spelling rule vocabulary (from + to + context_clues) + ~100 stop words
- `segment()`, `word_count()`, `has_context_clue()` methods
- O(n * max_word_len) where max_word_len=6; pure Rust, zero C dependencies
- 13 unit tests: segmentation, longest match, byte offsets, empty input, ASCII, word count, context clues

### Housekeeping

#### Review refinements (Gemini feedback)
- Eliminated redundant `spelling_rules.clone()` in `Server::new`: `Scanner` now exposes `spelling_rules()` accessor, Server delegates instead of keeping a separate copy
- Capped `explain_variant` error response: `known_variants_sample` limited to 20 entries with `total_variant_rules` count
- Added E2E tests for `zh_explain_variant`: successful lookup + unknown character error path

#### 5.5 Sampling protocol support
- New `src/mcp/sampling.rs` module: `SamplingBridge`, `refine_issues_with_sampling`, `is_sampling_eligible`
- `SamplingBridge` wraps transport IO channels (writer + mpsc receiver) for server→client `sampling/createMessage` requests
- Thread-based stdin reader in `transport.rs`: background thread reads bounded lines into `mpsc::channel`, enabling `recv_timeout` for sampling responses
- `StdinMsg` enum: `Line(String)` and `TooLong` variants for reader→main thread communication
- `sample_disambiguation()`: sends structured prompt (context window + ambiguous term + suggestions) and parses `CreateMessageResult`
- `recv_response_text()`: timeout-bounded loop that skips notifications, TooLong events, and ID-mismatched responses
- `refine_issues_with_sampling()`: iterates eligible issues, sends sampling requests, promotes confirmed suggestions to front, downgrades severity to `Info` on timeout
- `snap_to_char_boundary()`: safe UTF-8 byte-index snapping for context window extraction
- Transport `dispatch()` creates `SamplingBridge` for `tools/call` when client declared sampling support
- `Server::supports_sampling()` accessor for capability check
- 14 unit tests covering: eligibility, bridge send/parse, timeout, budget exhaustion, error response, ID mismatch, notification skipping, TooLong event skipping, char boundary snapping, issue refinement, timeout downgrade

#### 5.6 Ambiguity escalation policy
- `sampling_enabled` boolean parameter added to `zh_lint` and `zh_finalize` tool schemas (default: false)
- `is_sampling_eligible()`: rules with `english` field AND (multiple suggestions OR context_clues) are sampling candidates
- Per-invocation budget: `DEFAULT_SAMPLING_BUDGET = 5` limits sampling calls to prevent runaway latency
- `tool_lint`: sampling refinement runs after scan + stance filtering, before returning issues
- `tool_finalize`: sampling refinement runs after scan + stance filtering, before fix pass (promoted suggestions influence the fixer)
- Budget exhaustion: excess ambiguous items fall back to deterministic `Info`-severity diagnostics
- Sampling-disabled mode: identical to prior behavior (bridge is None or sampling_enabled is false)
- E2E tests: schema verification (sampling_enabled in zh_lint/zh_finalize), parameter acceptance

### Code Simplification Pass

#### Code-simplifier refinements (Gemini + Codex reviewed, approved)
- `audit.rs`: `sha256_hex` reduced from manual byte iteration to `format!("{:x}", Sha256::digest(data))`.
- `fixer.rs`: `surrounding_window` rewritten to use `floor_char_boundary`/`ceil_char_boundary` instead of collecting all char_indices into a Vec (zero allocation, O(CONTEXT_WINDOW_CHARS) instead of O(n)).
- `engine/scan.rs`: `remap_issues_to_original` merged two loops (offset remap + line/col computation) into a single pass; removed redundant emptiness guards before `extend` calls on suppression ranges.
- `engine/zhtype.rs`: removed redundant `text.is_empty()` check before `text.trim().is_empty()`.
- `engine/segment.rs`: `has_context_clue` replaced imperative for-loop with `.any()` iterator chain.
- `mcp/tools.rs`: `tool_explain_variant` eliminated intermediate Option variables; `filter_by_stance` simplified retain to single boolean expression; removed redundant suppression guards; sampling bridge pattern simplified from nested ifs to tuple destructure.
- 377 tests (286 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings.

### Housekeeping

#### Review refinements (Gemini batch 3 feedback)
- Fixed `surrounding_window` panic on empty text input (blocker: out-of-bounds index on empty chars vec)
- Fixed docstring: window includes the match span, not excludes it
- Optimized context-clue checking: `count_context_clues()` segments window once instead of re-segmenting per clue word
- Added edge-case tests: empty text, boundary spans, empty context_clues vec

#### Review refinements (Codex sampling review feedback)
- Fixed CRITICAL message-eating bug: `recv_response_text` consumed non-matching messages (notifications, mismatched IDs, other requests) from shared `mpsc::Receiver`, losing them permanently. Added `spillover: Vec<StdinMsg>` buffer to `SamplingBridge`; non-matching messages are stashed and returned via `into_spillover()` for transport re-processing.
- Transport main loop uses `VecDeque<StdinMsg>` pending buffer: drains spillover before blocking on channel, preserving message ordering.
- `dispatch()` return type changed to `(Option<JsonRpcResponse>, Vec<StdinMsg>)` to propagate spillover from `tools/call` arm.
- Fixed `find_matching_suggestion` order dependence: now prefers exact match first, then falls back to longest substring match (was first-hit which picked shorter substrings when listed first).
- Eliminated ID collision risk: sampling request IDs changed from integer (`next_id` starting at 10000) to string format `"zhtw-sampling-N"` (N starting at 0). JSON-RPC 2.0 allows string IDs; clients echo them back unchanged.
- Fixed potential 32-bit overflow: context window calculation `issue.offset + issue.length + 120` now uses `saturating_add` chain.
- Rejected Bug 5 (use std `floor_char_boundary`/`ceil_char_boundary`): requires Rust 1.91, project MSRV is 1.80. Custom `snap_to_char_boundary` retained.
- Updated 16 sampling unit tests: string IDs, spillover assertions on stash/notification/TooLong tests, 2 new tests for suggestion matching (exact preference, longest substring).
- 366 tests (275 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings.

#### Review refinements (second-round Gemini + Codex consensus)
- Process-global `AtomicU64` counter for sampling request IDs: replaced per-bridge `next_id: u64` field with `static SAMPLING_ID: AtomicU64`. Prevents stale response collisions when a timed-out bridge's response arrives during a later bridge's lifetime.
- Empty string filter in `find_matching_suggestion`: substring branch now skips empty strings (`!s.is_empty()`) to prevent vacuous `contains("")` matches.
- Response-shaped message discard in transport: main loop pre-checks raw JSON for `method` field; messages without `method` (stale sampling responses) are silently discarded instead of generating spurious PARSE_ERROR.
- Test serialization: `Mutex<()>` guard serializes sampling bridge tests that depend on the global counter, preventing race conditions with parallel test execution.
- 367 tests (276 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings.

#### 5.7 Agentic Editor integration patterns
- New `src/mcp/setup.rs` module: host-specific configuration generators for Claude Code, OpenCode, and GitHub Copilot
- `zh_setup` MCP tool (tool 8): generates integration configuration content for a specified host editor
  - `claude_code`: CLAUDE.md section with tool references, workflow, conventions, and profile guide
  - `opencode`: Skill definition YAML with lint → fix → finalize pipeline and resource/prompt references
  - `copilot`: copilot-instructions.md content + VS Code settings.json MCP server registration snippet
  - No host parameter: lists all available hosts
- `zhtw-mcp setup <host>` CLI subcommand: same content generation, prints JSON to stdout
- 6 unit tests: content verification for each host, host parsing, all-hosts generation
- E2E tests: `zh_setup` with `claude_code` host + host listing
- 373 tests (282 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings

#### Review refinements (Codex sampling review: std char boundary methods)
- Replaced hand-rolled `snap_to_char_boundary` with `str::floor_char_boundary`/`str::ceil_char_boundary` (stable since Rust 1.91).
- Bumped MSRV from 1.80 to 1.91 in Cargo.toml. Unlocked `is_multiple_of` clippy suggestion in quote pairing logic.

#### Review refinements (Gemini batch 3 findings)
- Removed dead `quote_hierarchy` field from `ProfileConfig`: defined in all three profiles but never checked by the scanner. YAGNI — will re-add if quote hierarchy gating is implemented.
- Fixed hardcoded byte offset `21` in `safe_mode_skips_issues_with_context_clues` test: now uses `text.find("程序").unwrap()` like other tests, preventing silent breakage on test string changes.
- Other Gemini findings (empty string panic, docstring, performance, boundary tests) were already addressed in prior iterations.

#### Review refinements (third-round Codex findings)
- `recv_response_text` no longer drops messages when ID matches but payload shape is unexpected: stashes the original line in spillover instead of silently returning `None` via `?` chains.
- `recv_response_text` now rejects blank (whitespace-only) LLM responses, returning `None` instead of `Some("")`.
- `find_matching_suggestion` both paths (exact and substring) filter empty and whitespace-only strings via `!s.trim().is_empty()`.
- Test lock poisoning prevention: `TEST_LOCK.lock().unwrap()` replaced with `.unwrap_or_else(|e| e.into_inner())` to avoid cascading failures when a test panics.
- 4 new tests: `bridge_stashes_malformed_payload_on_id_match`, `find_matching_exact_ignores_empty_suggestion`, `find_matching_ignores_whitespace_only_suggestion`, `bridge_returns_none_on_blank_response`.
- 377 tests (286 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings.

#### Rename SpellingRule `cn`/`tw` to `from`/`to`
- `SpellingRule.cn` renamed to `from`, `SpellingRule.tw` renamed to `to` across entire codebase
- Direction-neutral naming: `from` = term to match, `to` = replacement suggestions — correct for all rule types (cross-strait, variant, typo, confusable)
- JSON ruleset: all 423 spelling rules updated (`"cn"` key -> `"from"`, `"tw"` key -> `"to"`); context string values using "tw"/"cn" as region abbreviations left unchanged
- MCP tool schema: `zh_override_rule` description updated (`{from, to, ...}`)
- Python script: `check-ruleset.py` updated for new field names
- No schema version bump needed (postcard binary format is positional)
- No backward-compat aliases (project is pre-release)

### Tier 7: Hardening & Production Readiness

#### 7.3 Replace sled with JSON file storage
- Removed `sled` (0.34.7) and `postcard` (1.1.3) dependencies entirely — eliminates ~15 transitive crates
- New `OverrideStore` struct in `store.rs`: JSON-file-backed override storage at `~/.config/zhtw-mcp/overrides.json`
- `Overrides` struct: `{ schema_version, spelling: Vec<SpellingRule>, case: Vec<CaseRule> }` — human-readable, diffable, git-friendly
- Atomic writes: `flush_pending()` uses `tempfile::NamedTempFile` + `persist()` (rename) to prevent corruption on crash/power-loss
- Persist-before-mutate: all mutation methods clone state, flush the pending copy, then update in-memory on success (prevents memory/disk divergence on flush failure)
- Corrupt JSON recovery: `open()` backs up corrupt files to `.corrupt.bak` and starts fresh instead of hard-failing startup
- Schema version mismatch: backs up old file to `.vN.bak` before resetting
- `Server` struct refactored: holds `OverrideStore` + `Ruleset` instead of `sled::Db` + `PathBuf`
- `import` subcommand removed entirely (embedded ruleset is always the base; overrides are user-only)
- `--overrides` CLI flag replaces `--db` (alias kept for convenience)
- Best-effort `warn_legacy_sled()`: detects legacy sled DB, warns user to re-apply overrides (cannot read sled without dep)
- `SCHEMA_VERSION` bumped from 2 to 3
- Gemini + Codex review: fixed non-atomic writes, mutate-before-persist ordering, corrupt JSON recovery, stale comment, unused return value
- 378 tests (287 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings

#### 7.4 Performance benchmarks (criterion)
- `benches/scanner.rs`: 6 benchmark groups using criterion 0.5 for statistically rigorous micro-benchmarks
- Scanner construction: Aho-Corasick automaton build from full embedded ruleset (~810us)
- `scan()` on 1KB / 10KB / 100KB mixed CJK+ASCII text (~41us / 450us / 4.7ms, linear scaling)
- `scan_profiled()` with StrictMoe profile at same 3 sizes (variant + extended punctuation passes)
- `apply_fixes_with_context()` with 50 concurrent issues (~1.5us)
- Markdown exclusion pass at 3 sizes (~4us / 44us / 410us)
- FMM segmenter on 100-char CJK text (~10us, well under the 1ms contract from Tier 4.4)
- Test data: repeating paragraphs with consistent hit ratio for stable benchmarking
- `cargo bench --bench scanner` runs all groups; baseline recorded for regression detection

#### 7.1 MCP SDK evaluation
- Evaluated 4 Rust MCP SDKs: rmcp (official, 0.15.0), rust-mcp-sdk (0.8.3), mcpkit (0.5.0), mcp-server (abandoned)
- Verdict: Do Not Migrate. Stay hand-rolled.
  - All SDKs require tokio -- zhtw-mcp has zero async dependencies; adding tokio means +60-70% dep tree growth and +800KB-1.5MB binary size for zero functional benefit
  - rmcp is pre-1.0 with breaking changes every ~8 weeks (22 releases in 11 months)
  - The SamplingBridge spillover-stashing pattern has no equivalent in any SDK
  - Hand-rolled protocol layer is ~580 lines (types+transport); maintenance is cheaper than SDK dependency churn
  - Missing SDK features (streamable HTTP, OAuth, tasks, elicitation) are irrelevant for a stdio lint server
- Re-evaluation gate: quarterly, or when rmcp reaches 1.0, or when HTTP transport is needed
- Current footprint: 2.5 MB binary, 13 direct deps, 141 total crate nodes

#### 7.5 Startup latency measurement
- Cold start + initialize response: 16ms (binary execution to JSON-RPC response)
- Scanner construction (criterion): ~810us
- JSON ruleset parse + automaton build + server init: well under 100ms decision gate
- No optimization needed: lazy construction (OnceLock), pre-serialized automaton, and build.rs const generation are all unnecessary at this latency
- Decision: no changes applied; re-profile if ruleset grows beyond 1000 rules

#### 7.x Code review refinements (Gemini + Codex consensus)
- Deleted dead code: `parse_ruleset()` in `loader.rs` (unused, both reviewers flagged)
- Renamed `migrate_from_sled()` to `warn_legacy_sled()`: honest name, returns `()` instead of misleading `bool`
- `OverrideStore::open()` now writes default overrides file on first creation (prevents perpetual legacy sled warning on every startup)
- Benchmark: swapped `Segmenter`/`Scanner` build order in `bench_apply_fixes` to eliminate unnecessary `.clone()` on `spelling_rules`
- E2E test: replaced fixed temp dir name with `tempfile::tempdir()` (prevents parallel test interference, auto-cleanup on drop)
- Code-simplifier pass: `tool_explain_variant` deduplicates variant rule collection and `standard` extraction; benchmark generators use `str::repeat()`

### Tier 8: Distribution & Adoption

#### 8.2 Binary size budget
- Release binary: 2.5MB (LTO + strip, aarch64-darwin). Budget: 5MB max.
- `make check-size` CI gate: fails if release binary exceeds 5,242,880 bytes
- Formalized as a Makefile target for integration into CI pipelines

#### 8.3 User dictionary / feedback loop
- `SuppressionStore` in `store.rs`: JSON-file-backed per-user suppression list at `~/.config/zhtw-mcp/suppressions.json`
- `zh_suppress_rule` tool (9th MCP tool): add/remove/list/clear suppressed terms
- Suppressed terms still appear in `zh_lint` and `zh_finalize` output but with severity downgraded to `Info`
- `zh_finalize` gate excludes Info-severity issues from error count (suppressed terms pass the gate)
- Distinct from `zh_override_rule`: suppression is per-user preference (soft downgrade), override is a rule modification (hard change)
- Atomic writes via `tempfile::NamedTempFile` + `persist()` (same pattern as `OverrideStore`)
- Mutate-after-flush pattern: in-memory state committed only after successful disk write, with rollback on failure
- Corrupt/schema-mismatch file backed up to `.bak` before reset (matching `OverrideStore` behavior)
- `--suppressions` CLI flag for custom path (enables test isolation)
- Input terms trimmed before add/remove to prevent whitespace mismatches
- 4 unit tests + 4 E2E protocol tests (add, lint with downgrade, list, remove)

#### 8.1 Install script and editor registration
- `scripts/install.sh`: build from source (cargo), install to `~/.local/bin/`, optional editor registration
- `--prefix DIR` for custom install location, `--register claude|vscode|all` for editor setup
- Claude Desktop: auto-generates `claude_desktop_config.json` entry under `mcpServers`
- VS Code: generates `.vscode/mcp.json` for GitHub Copilot MCP integration
- PATH detection: warns if install directory is not in PATH

### Improvements (12.x)

#### 12.1 Stack-based quote hierarchy validation
- `validate_quote_hierarchy()` in `scan.rs`: per-paragraph stack-based validator for CJK bracket quotes
- Detects: mismatched close (`「...』`), secondary without primary (`『...』` at top level), unclosed quotes at paragraph boundaries, interleaved quotes (`「...『...」...』`)
- Validates `「」` (primary), `『』` (secondary), `《》` (book title marks)
- Operates per-paragraph (split on `\n\n` and `\r\n\r\n`) to prevent cascading across blocks
- Emits `IssueType::Punctuation` with `Severity::Warning` (non-blocking: never trips `max_errors` gate)
- 8 unit tests: balanced, unbalanced, interleaved, multi-depth, secondary at top level, book title, paragraph reset, code exclusion

#### 12.2 Plain-text backtick exclusion via pulldown-cmark
- Unified all input through pulldown-cmark for code block/inline code exclusion
- Removed 3 backtick regexes (`RE_TRIPLE_BACKTICK`, `RE_DOUBLE_BACKTICK`, `RE_SINGLE_BACKTICK`) from `excluded.rs`
- Renamed `build_excluded_ranges_no_backticks()` to `build_excluded_ranges()` (now content-pattern only: URLs, paths, @mentions)
- Added `scan_profiled_md(text, profile, use_markdown)` for explicit markdown toggle
- Production paths (MCP tools, CLI) use `scan_profiled_md()` with content-type-aware flag; plain text avoids false exclusion of 4-space indented paragraphs
- All 62 determiner_compat tests pass with pulldown-cmark on plain-text input

#### 12.3 Expand variant coverage via OpenCC dictionaries
- `scripts/import-opencc-phrases.py`: imports from `externals/OpenCC/data/dictionary/TWPhrases.txt`
- 293 new cross-strait phrase rules added (from 446 to 739 spelling rules after this step)
- Filters: skip single-character entries (MIN_FROM_CHARS=2), skip identity mappings, skip circular rules (`to_filtered = [t for t in to_options if t != frm]`), skip existing rules
- All imported as `type: "cross_strait"`, `traditional_only: true`
- Provenance pinned to OpenCC submodule version; Apache-2.0 license

#### 12.4 Cross-strait technical terminology expansion
- `scripts/import-crossstrait-terms.py`: imports from two sources
  - `externals/table.csv` (zhuohongwei/chinese-technical-terms): 3 new entries
  - `externals/invade/database/vocabs/*.yml`: 25 new entries (filtered to TECHNOLOGY, HARDWARE, MEDIA, VEHICLE, FINANCE categories)
- Blocklist of 31 context-dependent terms that would cause false positives (面向, 物理, 水平, 應用, etc.)
- Total: 28 new entries (767 spelling rules total)
- `check-ruleset.py --lint` passes; all 397 tests pass

#### Review refinements (Gemini + Codex)
- Fixed content_type being ignored after 12.2 unification: `scan_for_content_type()` and `build_exclusions_for_content_type()` restored content-type branching via `scan_profiled_md()`
- Fixed CRLF paragraph splitting in `validate_quote_hierarchy`: handles both `\n\n` and `\r\n\r\n` to match `fix_quote_pairing`

#### Code-simplifier refinements
- `scan_punctuation`: consolidated `b'!'`/`b'?'`/`b';'` match arms into single arm with data tuple selection (eliminated 14 lines of duplicated control flow)
- `scan_punctuation`: consolidated `b'('`/`b')'` arms similarly
- `fix_quote_pairing`: eliminated double negation (`all_same_char` → `char_based_ok` → `use_alternation`); computed directly as `char_based_ok` using short-circuit `!all_same && { trial... }`
- `validate_quote_hierarchy`: consolidated three closing-quote handlers into single `'」' | '』' | '》'` arm with data tuple and shared stack logic
- `import-crossstrait-terms.py`: extracted `filter_candidates()` to deduplicate CSV/vocab filter-skip-build loops
- 397 tests (298 unit + 8 CLI + 62 integration + 1 E2E + 28 vocab), zero clippy warnings

### Tier 9: Polish & Remaining Backlog

#### 9.1 Dictionary hot-reload
- `zh_reload` tool (10th MCP tool): rebuilds scanner from current overrides without server restart
- Leverages existing `rebuild_scanner()` method; returns new rule counts and ruleset hash
- Simpler alternative to filesystem-watching (per TODO.md recommendation)

#### 9.2 Async runtime evaluation
- Decision: synchronous stdio transport with thread-based stdin reader is adequate
- No async runtime justified for stdio-only workload; re-evaluate only if HTTP transport or SDK migration demands it
- Documented criteria for future re-evaluation in TODO.md

#### 9.3 Ellipsis normalization
- `scan_ellipsis()` free function in `scan.rs`: three detection patterns
  - ASCII `...` (3+ dots) adjacent to CJK → `……` (Warning)
  - Circle periods `。。。` (3+ consecutive) → `……` (Warning)
  - Single `…` (not followed by another) → `……` (Info)
- `ellipsis_normalization: bool` field in `ProfileConfig` (enabled for all profiles)
- Wired into `scan_with_config()` pipeline before overlap resolution
- 5 unit tests: ASCII dots, circle periods, single U+2026, correct double U+2026, dots without CJK

### Tier 10: Bug Fixes (audit-identified)

#### B.1 NFC normalization in fix path
- Fix path (`FixMode::Safe|Aggressive`) now uses `scan_profiled`/`scan_markdown_profiled` (same as lint path) instead of `scan_with_excluded`, ensuring NFC normalization is applied before scanning in both paths
- Exclusion ranges still built on original text for the fixer

#### B.2 CLI lint `--max-errors` severity alignment
- `main.rs` exit gate now counts only `Severity::Error` issues, matching MCP tool behavior
- JSON output includes both `total` and `errors` fields

#### B.3 Position-based sampling downgrade persistence
- `sampling_downgraded` changed from `Vec<String>` to `Vec<(String, usize)>`, keying by (term, original offset)
- After fix + re-scan, downgrades are re-applied using exact offset remapping via `remap_to_post_fix()` (accumulates byte deltas from `AppliedFix` records) instead of loose proximity heuristic
- Gemini + Codex reviewed: exact offset matching eliminates false positives from the previous `max_shift = applied * 20` heuristic

#### B.4 `scan_dunhao` minimum run length
- Added `run_len < 2` guard: requires at least 2 consecutive short segments (3 commas, 4 items with 2+ verified short) before flagging dunhao suggestions
- Prevents false positives on single isolated short items bounded by long clauses

#### B.5 CLI lint disabled rule filtering
- `run_lint` now filters `spelling_rules.retain(|r| !r.disabled)` and `case_rules.retain(|r| !r.disabled)` before constructing the scanner, matching MCP server behavior
- Intentionally disabled rules (e.g. `文件`) no longer fire in CLI lint mode

#### B.6 Position-based convergent chain suppression
- `FixResult` changed from `applied_replacements: Vec<String>` to `applied_fixes: Vec<AppliedFix>` recording `(offset, old_len, replacement)` for each fix
- `remap_to_post_fix()` helper computes exact post-fix byte offsets by accumulating deltas from preceding fixes
- Convergence suppression now uses span-vs-span overlap against fixer-written byte ranges instead of global text matching
- Prevents suppressing legitimate pre-existing issues that happen to share text with a fixer output
- `debug_assert!(result >= 0)` guards against negative offset remapping under invariant violations

#### B.7 Trace issue count consistency
- Fix path audit trace now records `remaining_issues.len()` (all severities) instead of `residual_errors` (error-only), matching the lint path's `issues.len()` metric
- Both paths now use comparable all-severity issue counts in trace records

#### Review refinements (Gemini + Codex consensus on B.3/B.6)
- B.3: replaced loose proximity heuristic (`max_shift = applied * 20`) with exact offset remapping via `AppliedFix` records
- B.6: replaced global `applied_replacements.contains(&issue.found)` with position-based byte range suppression; changed start-only overlap check to full span-vs-span overlap (`issue.offset < end && issue_end > start`)
- Added `debug_assert!` in `remap_to_post_fix` for defensive `isize` underflow detection
- 386 tests (295 unit + 62 integration + 1 E2E + 28 vocab), zero clippy warnings

### Improvements (12.x)

#### 12.1 Stack-based quote hierarchy validation
- `validate_quote_hierarchy()` in `scan.rs`: stack-based validator walks text per-paragraph, validates structural nesting of CJK quote marks: `「」` (primary), `『』` (secondary), `《》` (book title)
- Violations detected: mismatched close (e.g. `「...』`), secondary without primary (`『...』` at top level), unclosed quotes at paragraph boundaries, interleaved quotes (`「...『...」...』`)
- Operates per-paragraph (split on `\n\n` and `\r\n\r\n`) so one block's unclosed quote doesn't cascade
- Emits `IssueType::Punctuation` with `Severity::Warning` (non-blocking: `max_errors` gate counts only `Error`)
- Integrated into `scan_with_config()` as post-scan pass after `fix_quote_pairing`
- 11 unit tests: balanced primary, balanced nested, secondary at top level, unclosed, extra close, interleaved, multi-depth, paragraph reset, book title balanced/unmatched, code exclusion

#### 12.2 Plain-text backtick exclusion via pulldown-cmark
- Unified scan path: both plain text and Markdown use pulldown-cmark for code block/inline code exclusion
- Removed 3 backtick regex statics (`RE_TRIPLE_BACKTICK`, `RE_DOUBLE_BACKTICK`, `RE_SINGLE_BACKTICK`) from `excluded.rs`
- `build_excluded_ranges()` now handles only content patterns (URLs, paths, @mentions)
- `excluded_offset_pairs()` combines content-pattern exclusions with pulldown-cmark structural exclusions
- New `scan_profiled_md()` method with explicit `use_markdown: bool` parameter: Markdown mode uses pulldown-cmark, plain mode skips it to avoid 4-space-indented paragraphs being falsely excluded as code (Gemini review finding)
- `content_type` parameter restored to active use in `tools.rs` and `main.rs` (was ignored after initial unification; fixed per Gemini + Codex review)
- All 62 determiner_compat integration tests pass with the unified path

#### 12.3 Expand variant coverage via OpenCC dictionaries
- `scripts/import-opencc-phrases.py`: import script reads OpenCC `TWPhrases.txt`, filters identity mappings and circular rules, deduplicates against existing ruleset, adds new entries as `cross_strait` with `traditional_only: true`
- 293 new cross-strait phrase entries imported from OpenCC TWPhrases.txt (IT terms, country names, general terminology)
- TWVariants.txt: all 39 character-level entries already covered by existing 45 variant rules (no new additions needed)
- Provenance: OpenCC dictionary pinned to externals/OpenCC submodule, Apache-2.0 license

#### 12.4 Cross-strait technical terminology expansion
- `scripts/import-crossstrait-terms.py`: import script reads `externals/table.csv` (curated CN→TW tech mappings) and `externals/invade/database/vocabs/*.yml` (categorized vocabulary, filtered to TECHNOLOGY/HARDWARE/MEDIA/VEHICLE/FINANCE categories)
- Blocklist of 31 context-dependent terms that would cause false positives (e.g. `面向`, `物理`, `通過`, slang)
- 28 new entries: 3 from table.csv + 25 from invade vocabs (e.g. `智能→智慧`, `回滾→回溯/還原`, `反安裝→解除安裝`, `摳圖→去背`, `流媒體→串流媒體`)
- Ruleset expanded from 446 to 767 spelling rules (293 OpenCC + 28 cross-strait tech)

#### Review refinements (Gemini + Codex consensus on 12.x)
- Fixed content-type-aware exclusion: `scan_profiled_md()` with `use_markdown` flag; plain text skips pulldown-cmark to avoid false code block exclusion on indented paragraphs
- Fixed CRLF paragraph splitting in `validate_quote_hierarchy`: handles both `\n\n` and `\r\n\r\n` (Codex finding)
- Fixed import scripts: added `json.dump()` write-to-disk step before calling `check-ruleset.py` (entries were lost because scripts only modified in-memory dict)
- Fixed circular rule filter in OpenCC import: entries where `from` appears in `to` list are filtered out
- 397 tests (298 unit + 8 CLI + 62 integration + 1 E2E + 28 vocab), zero clippy warnings

### Improvements (13.x)

#### 13.1a Domain-specific rule packs
- `~/.config/zhtw-mcp/packs/` directory for domain-specific vocabularies (medical, legal, semiconductor, finance)
- New MCP tools: `zh_list_packs`, `zh_load_pack`, `zh_unload_pack` for runtime pack management
- `zh_check` gains `pack` parameter to activate packs alongside base ruleset
- CLI `--pack <name>` flag for batch lint mode
- Pack merge order: embedded ruleset → overrides.json → packs (lexicographic, last-wins on conflict)
- Pack discovery reports active conflicts when multiple packs define the same `from` key
- `zhtw-mcp pack import <file>` / `pack export <name>` for portable rule exchange
- Pack name validation: path traversal prevention (no `/`, `\`, `..`, null bytes), Windows reserved name blocking

#### 13.1b Portable rule exchange format
- Top-level `metadata` object: `name`, `version`, `author`, `description`, `license`, `source_url`
- `tags` field on SpellingRule for filtering and categorization
- `zhtw-mcp pack validate <file>` checks schema, deduplication, circular references, `@seealso` integrity

#### 13.1c Rule authoring via MCP tools
- `zh_add_rule` tool: add conversion rules without editing JSON; validates duplicates, circular refs, substring conflicts
- `zh_remove_rule` tool: remove user override rules by `from` key
- `zh_list_rules` tool: enumerate user overrides (not embedded ruleset)
- Scanner rebuilt after mutation via `rebuild_scanner()` method on `Server`
- Write target: always base `overrides.json` (packs are read-only at runtime)

#### 13.2a Agent integration recipes
- `zhtw-mcp setup` extended to 8 platforms: Claude Code, OpenCode, Copilot, Cursor, Windsurf, Cline, Continue.dev, Generic
- Cursor: `.cursor/rules` with zh-TW conventions and tool usage
- Windsurf: `.windsurfrules` integration
- Cline: `.clinerules` configuration
- Continue.dev: `config.json` MCP server registration
- Generic: platform-agnostic `.zhtw-mcp.md` instruction file

#### 13.2b Explanation mode
- `explain: true` parameter on `zh_check` attaches cultural/linguistic annotations to each issue
- Draws from `context`, `english`, and `context_clues` fields in SpellingRule
- Variant rules include MoE standard form reference
- Case rules include canonical casing rationale
- Explanation absent when `explain` is false/omitted (zero overhead, backward compatible)

#### 13.2c Token-efficient output
- `output` parameter: `"full"` (default) | `"compact"` on `zh_check`
- Compact mode: omits echoed `text` (unless fixes applied), omits `trace` metadata, omits byte `offset`/`length`
- Issue deduplication: same rule firing N times grouped into one entry with `count` + `locations: [{line, col}]`
- Terser tool schema: description strings cut to single-line essentials (~200 tokens saved per `tools/list`)
- Setup output trimmed: CLAUDE.md section from ~1200 chars to ~600 chars (pointer to style guide resource)

#### 13.2d Natural-language lint interface
- `lint_natural` MCP prompt: translates free-form instruction into `zh_check` parameters
- Accepts `instruction` and `text` arguments; returns structured prompt for host LLM to extract parameters
- Prompt-based routing: host LLM parses instruction, no sampling round-trip

#### 13.2e Multi-turn editorial workflow prompt
- `editorial_review` MCP prompt: multi-turn zh-TW editor persona
- Host LLM iteratively reviews text via `zh_check`, explains issues, applies fixes until `accepted: true`
- `max_iterations` parameter (default: 3) prevents infinite loops on ambiguous text
- References `zh-tw://style-guide/moe` resource for detailed conventions

#### 13.3a CLI batch lint mode
- `lint` subcommand accepts multiple file arguments
- Single scanner construction amortized across all files
- Per-file exit code: fail if any file exceeds `--max-errors`
- Aggregate summary printed to stderr

#### 13.3b Async transport behind cargo feature flag
- `--features async-transport` enables tokio-based stdio transport
- `src/mcp/transport_async.rs`: single-threaded tokio runtime via `Builder::new_current_thread().enable_all()`
- Server remains synchronous; async wraps transport I/O only
- Feature OFF: binary identical to current (~2.5MB, zero tokio in dep tree)
- Feature ON: adds tokio with minimal features (`rt`, `io-util`, `time`, `sync`, `macros`, `io-std`)
- Both sync and async builds pass full test suite; clippy clean under both configurations

#### 13.3c Scanner memory optimization
- Vec pre-allocation: `Vec::with_capacity((text.len() / 2048).max(8))` in scan_spelling
- `effective_suggestions()` helper: builds suggestion list from rule's `to`/`english` fields, reducing inline cloning
- Benchmark: 100KB scan 3.81ms (down from ~4.3ms baseline, ~11% improvement, meets >10% p95 threshold)
- Binary ruleset embedding (build.rs + postcard) evaluated and rejected: JSON parse is <1ms, complexity not justified

#### 13.5 Single-binary distribution
- Binary ruleset embedding: evaluated and rejected (cold-start JSON parse <1ms, not worth build.rs complexity)
- GitHub Actions CI workflow (`.github/workflows/ci.yml`): format, clippy (sync + async), tests, ruleset lint, binary size check
- GitHub Actions release workflow (`.github/workflows/release.yml`): on `v*` tag push, builds 5 platform targets:
  - `x86_64-unknown-linux-musl` (static linking)
  - `aarch64-unknown-linux-musl` (ARM servers)
  - `x86_64-apple-darwin` (Intel Mac)
  - `aarch64-apple-darwin` (Apple Silicon)
  - `x86_64-pc-windows-msvc` (Windows)
- Artifacts: `zhtw-mcp-<version>-<target>.tar.gz` (Unix) / `.zip` (Windows) with SHA256 checksum files
- `scripts/install.sh` rewritten: detects OS/arch, downloads pre-built binary from GitHub Releases, verifies SHA256 checksum, falls back to build-from-source if download fails or platform unsupported
- `--from-source` flag for explicit source build; `--register claude|vscode|all` for editor registration
- `Cargo.toml` polished for crates.io: `repository`, `homepage`, `categories`, `keywords`, `include` patterns
- 405 tests, zero clippy warnings

#### Review refinements (Codex + Gemini consensus on 13.x)
- release.yml: replaced fragile `gcc-aarch64-linux-gnu` linker with `cross` for aarch64-musl builds (correct musl toolchain)
- release.yml: per-job permissions (`contents: read` for build, `contents: write` for release only)
- release.yml: Windows checksum uses `Set-Content -NoNewline` to avoid CRLF mismatch
- release.yml: Unix checksum uses `sha256sum` with `shasum` fallback (idiomatic per platform)
- ci.yml: added `permissions: contents: read`, `concurrency` group, `timeout-minutes: 15`
- install.sh: single global `CLEANUP_DIRS` array with one `trap cleanup EXIT` (fixes temp dir leak from competing traps)
- install.sh: `build_from_source` uses subshell `(cd ... && cargo build)` to avoid cwd mutation
- install.sh: checksum verification fails closed (no sha256 tool = error, not silent bypass)
- install.sh: argument validation for `--prefix`/`--register` (guards against missing values)
- install.sh: `grep -qxF` for PATH check (fixed-string match avoids regex metacharacter issues)
- install.sh: `usage()` uses heredoc instead of `sed` on `$0` (works under `curl | bash`)
- Cargo.toml: `async-transport = ["dep:tokio"]` (prevents implicit `tokio` feature leak)
- Cargo.toml: added `readme = "README.md"` and `README.md` to `include` for crates.io
- scan.rs: `effective_suggestions()` uses `any()` check before allocation (avoids wasteful alloc-then-discard)
- scan.rs: early return before `LineIndex::new()` when issues is empty (skips O(n) scan for clean text)

#### Code-simplifier refinements (13.x)
- store.rs: extracted `build_merged_rules()` — DRY helper encapsulating pack-loading + rule-merging pipeline shared by MCP server and CLI batch linter
- tools.rs: `Server::build_scanner` reduced from 30 → 14 lines by delegating to `build_merged_rules`; removed unused `merge_spelling_rules`/`merge_case_rules` imports; `build_summary` rewritten with `fold`
- main.rs: `run_lint_batch` scanner setup reduced from 28 → 9 lines via `build_merged_rules`
- transport_async.rs: removed dead `VecDeque<String>` pending queue (never pushed to, always fell through to None); flattened read loop; collapsed `tools/call` dispatch to single expression
- scan.rs: `effective_suggestions` simplified to single-pass filter+collect

### Improvements (14.x)

#### 14.1 Context window boundary bleed in Markdown
- `surrounding_window_bounded()` added to `src/engine/scan.rs`: clamps the context window at excluded-range boundaries, preventing context clues from crossing structural Markdown boundaries (e.g. code blocks)
- Context-clue gate now calls the bounded variant instead of the unbounded `surrounding_window()`

#### 14.2 Suppression aliases
- `<!-- zhtw:disable-next-line -->` and `<!-- zhtw:disable-block -->` / `<!-- zhtw:end-disable -->` accepted as user-friendly aliases for the existing `ignore` suppression markers
- Helper predicates in `src/engine/suppression.rs` accept both spellings; inline `「」`/`''` exclusion remains deferred

#### 14.3 Negative context clues
- `negative_context_clues: Option<Vec<String>>` field added to `SpellingRule` in `src/rules/ruleset.rs`
- When any negative clue word appears in the bounded surrounding window, the rule is vetoed
- Segmenter dictionary updated in `src/engine/segment.rs`; veto gate added in `src/engine/scan.rs::scan_spelling()`

#### 14.4 Disabled `參數` → `引數` rule
- `"disabled": true` added to the `參數` (parameter) rule in `assets/ruleset.json`
- `Scanner::new()` filters disabled rules at construction time; flag honoured regardless of calling path

#### 14.5 `max_warnings` gate
- `max_warnings: Option<u64>` parameter added to `zh_check` tool in `src/mcp/tools.rs`
- Gate logic added to `build_check_output()`; gate JSON object now includes `max_warnings` / `residual_warnings`
- `--max-warnings N` CLI flag added to `lint` subcommand in `src/main.rs`

### Bug Fixes & Improvements (15.x)

#### 15.1 HackMD / Docusaurus container-block false positives
- Pre-pass added to `build_markdown_excluded_ranges()` in `src/engine/markdown.rs`: lines matching `/^\s*:{3,}/` (e.g. `:::warning`, `:::`) are pushed as excluded ranges
- Only fence lines excluded; prose content between fences remains scannable
- Tests: `container_fence_lines_excluded`, `container_fence_four_colons`

#### 15.2 ASCII-context comma false positives
- Revalidated on current HEAD: `adjacent_cjk()` already handles ASCII neighbors correctly — stops at first non-whitespace, so ASCII neighbors suppress the flag
- No code change required; confirmed correct behavior via `cargo run -- lint lkl.md`

#### 15.3 YAML content-type support
- Engine: `build_yaml_excluded_ranges()` added to `src/engine/markdown.rs`; `Scanner::scan_profiled_yaml()` added to `src/engine/scan.rs`; `scan_with_prebuilt_excluded()` extended with `use_yaml: bool`. YAML key tokens at line start excluded. Tests: `yaml_key_colon_excluded`, `yaml_key_with_spaces_before_colon`, `yaml_list_item_not_excluded`, `yaml_hyphenated_key_excluded`, `yaml_indented_key_excluded`
- MCP: `Yaml` variant added to `ContentType` in `src/mcp/tools.rs`; `parse_content_type` accepts `"yaml"`; dispatch routes to YAML path; JSON schema updated
- CLI: `yaml` added to `--content-type` flag in `src/main.rs`; `.yml`/`.yaml` extension auto-detected

#### 15.4 `卸載` false positive in filesystem contexts
- Added `negative_context_clues: ["掛載", "mount", "umount", "unmount", "檔案系統", "分割區"]` to the `卸載` rule in `assets/ruleset.json`
- Filesystem/mount contexts now veto the rule; standalone `卸載` in non-filesystem prose still fires

### Bug Fixes (17.x)

#### 17.1b Context clue absorption in segmenter
- Changed `has_context_clue()` to accept substring matches within segmented tokens
- Root cause: MMSEG Rule 1 (max total chars) prefers "下拉菜單"(4) as a single token over "下拉"(2)+"菜單"(2) because "下拉菜單" is itself a rule `from` term in the dict. The clue "下拉" never surfaces as a standalone token.
- Fix: after segmenting the window, check whether any clue is a substring of any matched token (not just exact match), aligned at character boundaries
- `has_context_clue("下拉菜單的操作", &["下拉"])` now returns true

### Essential Features (19.x)

#### 19.4 Multi-file and directory linting
- `zhtw-mcp lint src/ docs/ README.md` accepts directories and multiple file arguments
- Recursively discovers `.md`/`.yml`/`.yaml`/`.txt` files; skips hidden files/directories
- `--exclude <glob>` to skip additional patterns
- Deterministic lexicographic traversal order; path normalization and deduplication
- Aggregate exit code: fail if total issues across all files exceed `--max-errors`
- Human format: grouped by filename with per-file summary; JSON format: array of per-file results

#### 19.5 Project config file support
- `.zhtw-mcp.toml` for team-wide, version-controlled lint configuration
- Fields: `profile`, `content_type`, `max_errors`, `max_warnings`, `ignore_terms`, `exclude`, `overrides`, `suppressions`, `packs`
- Discovery: resolves from cwd upward, stopping at VCS root (`.git`) or filesystem root
- CLI flags override config values; absent config causes no error (silent fallback)
- Explicit `--config <path>` for non-standard locations

#### 19.1 CLI `--fix` mode for in-place file correction
- `--fix` flag on `lint` subcommand: read file -> scan -> apply fixes -> write back (atomic via `tempfile::NamedTempFile` + `persist()`)
- Default to safe mode; `--fix=aggressive` for context-clue-gated rules
- `--fix --dry-run` shows what would change without writing
- `--fix` with stdin (`--`): emits fixed text to stdout (pipe-friendly)
- Summary of applied fixes printed to stderr in human format

#### 19.6 SARIF output for CI/CD integration
- `--format sarif` emits SARIF v2.1.0 (Static Analysis Results Interchange Format)
- Maps `Severity` to SARIF `level` (error/warning/note), `IssueType`/`RuleType` to `rule.id`
- Includes `region` (line, column, charOffset) and `physicalLocation` per result
- No new dependencies: JSON schema emitted via `serde_json`
- Compatible with GitHub Code Scanning (`codeql-action/upload-sarif`)

#### 19.9 Baseline mode for incremental adoption
- `--baseline <file>` suppresses pre-existing issues; reports only new ones
- `--update-baseline` generates/refreshes baseline with SHA-256 fingerprints (rule_type + found + file; position-independent)
- Exit code reflects new issues only; baseline issues counted separately in summary
- Enables ratchet pattern for gradual adoption on large document sets

### Usability (19.x)

#### 19.7 Pre-commit hook integration
- `.pre-commit-hooks.yaml` added to repository root
- Hook entry: `zhtw-mcp lint`, `language: rust`, `types_or: [markdown, yaml, text]`

#### 19.8 Diff-only mode for PR workflows
- `--diff-from <git-ref>` lints only files changed since a given ref
- Resolves changed files via `git diff --name-only <ref>...HEAD`; filters to supported extensions
- Exit code reflects only issues in changed files

#### 19.3 Colored terminal output for human-format lint
- ANSI color on stderr: error=red, warning=yellow, info=cyan; rule type in dim
- Bold filenames and found terms; dim annotations and summary text
- Respects `NO_COLOR` env var (per no-color.org spec) and `std::io::IsTerminal`
- Zero-overhead const struct pattern: `COLORS_ON`/`COLORS_OFF` with empty strings for disabled

#### 19.2 CLI `--explain` flag for rule rationale
- `--explain` appends rule `context` and `english` fields below each issue in human format
- Shows cross-strait distinction, MoE standard reference, and domain disambiguation
- Works in both human and JSON formats

#### 17.2 General zh-TW vocabulary supplement for context window segmenter
- ~198 curated zh-TW prose words embedded in `segment.rs` alongside STOP_WORDS
- Categories: abstract nouns (55), time words (17), location words (15), common verbs (54), adjectives/adverbs (31), connectives/discourse markers (27)
- Three-tier frequency layering: rule terms (freq=1) < general vocab (freq=5) < stop words (freq=10)
- Inserted via `or_insert(5)` to avoid overriding existing rule terms
- Improves MMSEG segmenter context clue recall on natural prose
- 489 tests (364 unit + 62 integration + 1 E2E + 34 vocab + 28 CLI)

### Architecture (20.x)

#### 20.3 Decompose scan.rs into submodules
- Split `src/engine/scan.rs` (3853 lines) into focused submodules
  - `scan/spelling.rs` (201 lines) — spelling AC matching + context-clue resolution + segmentation cache
  - `scan/case_rule.rs` (66 lines) — case-sensitivity AC matching
  - `scan/punctuation.rs` (305 lines) — full-width punctuation, dunhao, range indicator checks
  - `scan/ellipsis.rs` (132 lines) — ellipsis normalization
  - `scan/quotes.rs` (247 lines) — quote pairing + hierarchy validation
  - `scan/overlap.rs` (56 lines) — `resolve_overlaps()` priority-based greedy algorithm
  - `scan/mod.rs` (736 lines code) — `Scanner` struct, public API, shared CJK/context helpers
  - All submodules ≤400 lines; no cross-submodule state sharing beyond `Scanner` fields

#### 20.4 Deduplicate CLI / MCP scan pipeline
- Extracted shared scan-dispatch logic from `main.rs` and `mcp/tools.rs`
  - `ContentType` enum moved to `engine::scan` (shared between CLI and MCP)
  - `Scanner::scan_for_content_type()` provides single entry point for content-type dispatch
  - `build_exclusions_for_content_type()` provides shared exclusion builder
  - CLI `main.rs` uses `scan_for_content_type()` instead of manual match
  - MCP `tools.rs` delegates to `Scanner::scan_for_content_type()`, removed local ContentType enum
  - Also integrated daachorse `CharwiseDoubleArrayAhoCorasick` for spelling AC (20.3 enhancement)

### Translation Confirmation (32.x)

#### 32.1 translate: fail-closed on API error silently drops valid issues
- Fixed via 32.4 (tri-state TranslateOutcome). Empty/error translations now produce `Unknown` outcome, severity preserved (fail-open)
- Gate: `check_anchor_unknown_on_empty_translation` test

#### 32.2 translate: Windows lock detection incomplete in TranslationCache::open_at
- Added `"Access is denied"` and `"sharing violation"` to lock detection strings in `open_at`
- Gate: `open_at_windows_lock_error_message` test

#### 32.3 translate: TranslationCache singleton in Server
- `TranslationCache` opened once in `Server::new()`, stored as `Option<TranslationCache>` field
- `confirm_issues` signature changed to accept `cache: Option<&TranslationCache>` (no Arc needed — single-threaded)
- `ConfirmConfig.use_cache` removed; all 4 call sites updated: tools.rs (2, uses `self.translation_cache.as_ref()`), main.rs (2, opens own cache via `TranslationCache::open().ok()`)

#### 32.4 translate: tri-state TranslateOutcome — Unknown ≠ Rejected
- `google_translate_raw` returns `Result<String, TranslateError>` where `TranslateError { Io, RateLimit(u16), Parse }`
- `check_anchor()` returns `TranslateOutcome { Confirmed, Rejected, Unknown }`
- `confirm_issues` maps: Confirmed → keep, Rejected → downgrade, Unknown → keep
- `ConfirmResult` gains `unknown` count field; all callers updated
- Gate: 7 new `check_anchor_*` unit tests covering all three outcome paths

#### 32.5 translate: fix double cache lookup and term dedup key
- Double lookup removed: `translate_cached` now returns `(Result<String, TranslateError>, CacheSource)` — single cache probe. `CacheSource { Hit, Network }` drives rate-limiting and `api_calls` count
- Dedup key changed from `found` to `(found, english)` — same surface form with different english anchors now translated separately
- Gate: `check_anchor_same_term_different_english` test

#### 32.6 translate: batched LLM anchor injection via sample_bulk_confirm
- `SamplingBridge::sample_bulk_confirm(&[BulkConfirmTerm])` sends a single `sampling/createMessage` with all terms as indexed JSON array
- LLM returns index-keyed `{"0": true, "1": false, ...}` map (avoids ambiguity when same `found` appears with different `english` anchors)
- Tolerates markdown code fences in LLM responses
- `confirm_issues_with_sampling()` in translate.rs collects unique (found, english) terms, calls bulk confirm, applies tri-state outcomes
- Wired into `tool_check()`: if bridge has budget, prefer sampling; if sampling returns all-unknown, fall back to Google Translate cache
- Uses same bridge as disambiguation (shares remaining budget)
- Gate: 7 new tests — `bulk_confirm_*` (5) + `confirm_with_sampling_*` (2)

#### 32.7 translate: unit tests for confirm_issues correctness
- 11 new unit tests added (19 total, was 8). Existing 8 unchanged
- `check_anchor_confirmed_when_english_present` — Confirmed path
- `check_anchor_confirmed_slash_variants` — slash-separated english
- `check_anchor_rejected_when_anchor_absent` — Rejected path
- `check_anchor_unknown_on_io_error` — Unknown (Io)
- `check_anchor_unknown_on_rate_limit` — Unknown (RateLimit)
- `check_anchor_unknown_on_parse_error` — Unknown (Parse)
- `check_anchor_unknown_on_empty_translation` — Unknown (empty, 32.1)
- `check_anchor_case_insensitive` — case-insensitive matching
- `check_anchor_skips_short_variants` — <3 char filter
- `open_at_windows_lock_error_message` — Windows lock strings (32.2)
- `check_anchor_same_term_different_english` — dedup key (32.5)
- 592 tests, zero clippy warnings

### Calibration (35.x)

#### 35.6 Translation as calibration signal (rewrite)
- Replaced over-engineered anchor confirmation (~1500 LOC) with lightweight calibration (~300 LOC)
- Previous implementation: `AnchorOutcome` with 6 resolution methods, synonym tables, LCP stem
  matching, sled cache, per-issue confidence scoring, sampling integration — benchmark showed
  1/20 FP rejection, 159% latency overhead, 50% NoSignal (API cap)
- New design: single `calibrate_issues(text, issues)` function, one Google Translate API call
  per lint invocation, tri-state `anchor_match: Option<bool>` annotation
- Sentinel-delimited payload (`###SEG0`, `###SEG1`) for reliable segment round-trip through
  Google Translate (newline-based splitting was unreliable)
- Stopword filtering (~80 English stopwords) prevents false matches on common words like
  "the", "is", "and" in anchor token matching
- Content-word anchor matching: tokenize `english` field, filter stopwords, ANY content word
  match → `Some(true)` (parenthesized qualifiers are human hints, not AND-gates)
- CJK sentence boundary detection (。！？；) for ±40-char context extraction per issue
- `MAX_PAYLOAD_BYTES = 4096` cap prevents unbounded GET URL
- Fail-open: API failure → all `anchor_match = None`, zero impact on scan results
- CLI: `--verify` flag, `[verified]`/`[unverified]` tags in human output, stats line
- MCP: `verify: true` parameter, `anchor_match` per issue in JSON response
- Calibration-sampling interaction: `Some(true)` skips sampling, `Some(false)` keeps eligible
  for LLM second opinion (critical fix identified by Gemini review)
- `海內存知己` exception added to 內存 rule (classical Chinese poetry false positive)
- Deleted: `assets/anchor_synonyms.json`, `sled`/`postcard` dependencies
- Deleted from Issue struct: `anchor_confidence`, `anchor_method`, `anchor_evidence`
- Added to Issue struct: `anchor_match: Option<bool>`
- 20 new unit tests in `engine/translate.rs`, updated E2E and benchmark tests
- Phase 4 (evaluate for default enablement) deferred: requires corpus data collection
- Supersedes: 32.8 (pre_warm) closed as won't-fix — no cache to warm

### Grammar Scanner (34.x)

#### 34.0 Grammar scanner: plumbing and pattern-based checks
- Phase 1 — plumbing:
  - `IssueType::Grammar` variant added to `ruleset.rs` with `issue_type_ord()` ordinal 7
  - `grammar.rs` submodule created in `src/engine/scan/`, wired into `scan_with_config()`
    pipeline behind `ProfileConfig::grammar_checks` flag
  - Output formatting in `tools.rs` (compact + full + explain) and `main.rs`
    (human + SARIF + JSON + compact) handles Grammar variant
  - `IssueType::Grammar` round-trips through serde; all output formats tested
- Phase 2a — interlingual transfer detection (3 specified patterns + 4 bonus):
  - `scan_he_connecting_clauses`: flags `和` between verb phrases (verb suffix
    `了`/`過`/`著`/`來`/`去`/`完`/`好`/`到` before `和` + pronoun after)
  - `scan_bare_shi_adjective`: flags `[pronoun]是[adjective]` without degree adverb
    (`很`/`非常`/`特別`/`太`/`真`/etc.); 46 curated adjectives, 13 pronouns
  - `scan_redundant_preposition`: flags transitive verb + spurious preposition
    (e.g. `討論…關於`, `強調…之上`, `考慮…到`)
  - Bonus: `scan_bureaucratic_nominalization` (進行/加以/予以 + verb → direct verb),
    `scan_verbose_action` (做出/作出 + verb-object → direct verb),
    `scan_dui_jinxing` (對…進行… → direct verb + object),
    `scan_double_attribution` (根據…顯示/指出 redundant double attribution)
- Phase 2b — A-not-A + 嗎 clash detection:
  - `scan_a_not_a_ma`: 14 A-not-A patterns (是不是, 有沒有, 能不能, 會不會,
    要不要, 好不好, 對不對, 行不行, 可不可以, 願不願意, 想不想, 知不知道,
    喜不喜歡, 認不認識, 做不做, 吃不吃, 去不去, 來不來, 看不看, 走不走)
  - Sentence boundary detection (。？！) prevents cross-sentence false positives
- Implementation: patterns hardcoded in Rust (not data-driven grammar pack as
  originally proposed in TODO) — pragmatic choice given the small, stable pattern
  set and the need for structural heuristics (verb suffix detection, pronoun
  matching) that don't fit the `SpellingRule` schema
- 1876 lines, 170+ unit tests in grammar.rs, 6 CLI integration tests
  (JSON, SARIF, compact, human, explain formats + spelling coexistence)

### Token Optimization (38.x)

#### 38.1 Tabular (TSV) output mode for LLM-facing responses
- `output: "tabular"` parameter on `zhtw` MCP tool; `--format tabular` CLI flag
- Header-once TSV format eliminates JSON syntax tax (repeated keys, braces, quotes)
- Abbreviated severity codes (E/W/I) and rule type codes (cs/cf/v/pol/typo/punc/case/gram)
- Zero-count meta field omission in `#ok=` header line
- Same-column location compression: `1,4,7:C` instead of `1:C,4:C,7:C`
- `group_issues()`, `shorten_severity()`, `shorten_type()`, `compress_locations()` shared
  between MCP tabular output and CLI tabular format
- Token reduction: 80-89% vs full JSON output (measured via `scripts/measure-tokens.py`)
- Scope: `tools.rs` (MCP output builder), `main.rs` (CLI formatter)

#### 38.2 Diff-based patch output for fix mode
- `fix_output` parameter: `"full"` (default) | `"search_replace"` | `"patch"`
- `search_replace`: LLM-friendly `<<<<<<< SEARCH` / `======= REPLACE` / `>>>>>>> END` blocks
- `patch`: JSON array with UTF-8 byte offsets, `found` safety guard, descending offset order
- Token reduction: 55-57% on typical lint-fix responses vs full-text mode
- Scope: `tools.rs` (`FixOutputMode` enum, patch builder, search/replace formatter)

#### 38.3 Sampling prompt optimization
- Reduced `maxTokens` budget (32 for disambiguation, 128 for bulk confirm)
- Compressed system directives: stripped politeness tokens and redundant framing
- Format-restricting instructions constrain LLM response to bare term or JSON line
- Token reduction: >=30% per sampling round-trip
- Scope: `mcp/sampling.rs` (prompt templates, maxTokens tuning)

#### 38.4 Semantic cache for sampling disambiguation
- `DisambiguationCache` struct: in-memory `HashMap` scoped to single `tools/call` invocation
- Length-prefixed key encoding: `(found_term, english, normalized_context)` with newline
  separators to avoid 3 String allocations per lookup
- `normalize_cache_context()`: NFC-normalized, punctuation-stripped context window
- Zero false-hit risk (exact normalized-context matching); no persistence across requests
- Cache hit avoids `createMessage` call entirely (does not consume sampling budget)
- Scope: `mcp/sampling.rs` (cache layer around `refine_issues_with_sampling`)
- Measurement script: `scripts/measure-tokens.py` (tiktoken cl100k_base proxy)

### Protocol Robustness (25.x)

#### 25.6 Declare sampling capability explicitly in ServerCapabilities ([#12](https://github.com/sysprog21/zhtw-mcp/issues/12))
- Closed: spec-invalid.  MCP 2024-11-05 schema defines `sampling` as a
  **ClientCapabilities** field, not ServerCapabilities.  The client declares
  `capabilities.sampling: {}` during initialize; the server reads it to
  decide whether `sampling/createMessage` requests are permitted.  The
  server never declares sampling in its own capabilities.
  ServerCapabilities fields per spec: `experimental`, `logging`, `prompts`,
  `resources`, `tools`.  The existing implementation already handles this
  correctly (`tools.rs:103-104` checks `client_capabilities.sampling`).

### Reliability (25.x)

#### 25.2 Reject invalid UTF-8 lines instead of lossy repair ([#7](https://github.com/sysprog21/zhtw-mcp/issues/7), PR [#19](https://github.com/sysprog21/zhtw-mcp/pull/19))
- `read_bounded_line()` in `transport.rs` changed from `from_utf8_lossy()` to
  `String::from_utf8()` with `ReadLine::InvalidUtf8` error variant
- Malformed UTF-8 input now returns `PARSE_ERROR (-32700)` instead of silently
  replacing invalid sequences with U+FFFD
- Scope: both `transport.rs` and `transport_async.rs` updated
- Contributed by ChAoSUnItY

### Calibration & Fix Policy (35.x)

#### 35.4 Confidence-based fix policy
- Four fix tiers: None < Orthographic < LexicalSafe < LexicalContextual.
  - `orthographic` — punctuation, spacing, character forms, case, variant, grammar.
  - `lexical_safe` — above + deterministic terms (single suggestion, no context_clues).
    When `--verify` calibration has run, `anchor_match == Some(false)` issues skipped;
    `anchor_match == None` applies unconditionally.
  - `lexical_contextual` — all above + context-clue-gated terms (segmenter-verified).
    Non-clue issues use same single-suggestion constraint as LexicalSafe.
    Anchor rejection respected for non-clue issues; overridden for clue-gated
    (segmenter provides independent confirmation).
  - `rewrite` — deferred (requires its own design).
  - Old `safe`/`aggressive` removed; all call sites, tests, docs, scripts updated.
  - Scope: `fixer.rs` (decision logic + 4 anchor_match tests), `tools.rs` (parsing),
    `main.rs` (CLI), `setup.rs`/`prompts.rs` (generated docs), `e2e_mcp.rs`,
    `vocabulary_expansion.rs`, `fix_tier_benchmark.rs` (new benchmark test).

### AI Writing Detection (40.x)

#### 40.1 AI translation artifact rules
- Per-occurrence pattern detection for AI writing artifacts.
  - **Filler phrase rules**: 22 `ai_filler` rules in `ruleset.json` — 值得注意的是,
    需要注意的是, 在某種程度上, etc.  AC pattern matching via spelling scanner,
    gated by `ProfileConfig::ai_filler_detection`.
  - **Semantic safety words**: 意味著 disambiguation in `grammar.rs` with
    sentence-level context (definition→表示, consequence→代表, explanation→也就是說).
  - **Copula avoidance**: 作為/標誌著/充當→是, 擁有/設有→有 in technical prose
    context.  Compound-word guards (有所作為, 擁有權) prevent false positives.
  - **Passive voice**: 被廣泛使用/採用/應用 → active form.  Only adverb+verb
    patterns where dropping 被 is universally safe.
  - **說教句式**: `scan_ai_didactic()` in grammar.rs.
  - **空泛誇張**: `scan_ai_vague_exaggeration()` in grammar.rs.
  - **更重要的是 / 深刻影響 / 不容忽視**: 6 ai_filler rules in `ruleset.json`.
  - Implementation: two-bucket split — fixed-string rules in `ruleset.json`
    (`RuleType::AiFiller`), syntactic rules in `grammar.rs` (`IssueType::AiStyle`).
    AiStyle excluded from orthographic fix tier in `fixer.rs`.

#### 40.2 Document-level AI signature scoring
- Density-based detection: count occurrences of tracked phrases, compute
  density Δ_p = (C_p / L_text) × 1000, flag when density exceeds threshold.
  Calibrated from x86.md field review.
- `AiSignatureReport`: aggregated post-scan summary with `score: f32` (0.0–1.0),
  `markers: Vec<AiMarker>`, `top_signals: Vec<String>`.
  Scoring model: weighted sum of normalized density ratios.  No ML model —
  deterministic, explainable, reproducible.
  Incorporates all six signals: phrase density, structural patterns, issue density,
  sentence variability (40.8), zero-width artifacts (40.9), punctuation density
  matrix (40.10).
- Summary output mode: `output: "summary"` returns issue counts + AI signature.
- Scope: `engine/ai_score.rs`, post-scan aggregation in `tools.rs` and `main.rs`.

#### 40.3 Structural AI pattern detection
- Formulaic structural patterns beyond individual word choice.
  - **Enumerated list density**: list-paragraph ratio flagged when >40%.
  - **Paragraph-ending pattern**: formulaic declarations (這個...證明...,
    這...成為...的基礎/基石/起點, 正是這個...讓...).
  - **Binary contrast density**: paired transitions (然而...卻, 不僅...更,
    雖然...但) flagged when >5次/千字.
  - **Dash overuse**: paragraphs with ≥3 em-dashes flagged.
  - **Formulaic section headings**: stereotyped patterns (挑戰與未來展望,
    結論與展望) in heading context.
  - **說教句式** and **空泛誇張**: implemented as `scan_ai_didactic()` and
    `scan_ai_vague_exaggeration()` in grammar.rs.
- Profile-gated: `editorial` profile only.
- Scope: `engine/scan/grammar.rs`, `engine/markdown.rs`.

#### 40.4 Editorial review profile
- All AI writing detection rules bundled under `editorial` profile.
  `ProfileConfig` flags: `ai_filler_detection`, `ai_semantic_safety`,
  `ai_density_detection`, `ai_structural_patterns`, `ai_threshold_multiplier`.
- Profile isolation: `default`, `strict_moe`, `relaxed` have all AI flags false.
- Fix policy: `AiStyle` excluded from orthographic fix tier.

#### 40.5 AI translation style guide for LLM prompt generation
- `zhtw-mcp setup translation-guide` → style guide as LLM system prompt,
  preventing AI artifacts at generation time.
- Content: cross-strait terminology, semantic safety alternatives, nominalization
  avoidance, filler prohibition, verb-driven syntax.
- Scope: `mcp/setup.rs`, `main.rs`.

#### 40.6 Density-based AI phrase detection
- Post-scan frequency pass: for each tracked phrase, compute
  `count / (text_len_chars / 1000)`.  Annotate occurrences with density
  context when density exceeds per-phrase threshold.
- Tracked phrases (from x86.md field data): 更重要的是, 值得注意的是,
  這意味著, 不容忽視, 深刻影響, 從某種意義上, 從某種程度上, 需要注意的是,
  在某種程度上, 在這個過程中.
- `ai_threshold_multiplier` in ProfileConfig scales thresholds:
  0.5 (sensitive) / 1.0 (balanced) / 1.5 (conservative).
- Scope: `engine/ai_score.rs`, `engine/scan/grammar.rs`, `engine/scan/mod.rs`.

#### 40.7 `detect_ai` parameter
- `detect_ai` is orthogonal to `profile`:
  MCP `"detect_ai": true, "ai_threshold": "medium"`,
  CLI `--detect-ai [low|medium|high]`.
- When enabled, runs density pass + structural patterns regardless of profile.
  Results appear in `issues` array with `rule_type: "ai_style"`.
- When `profile: "editorial"` is active, `detect_ai` is implicitly true at
  `medium` threshold.  Explicit `detect_ai` overrides.
- Scope: `tools.rs`, `main.rs`, `engine/scan/mod.rs`.

#### 40.8 Sentence length variability as UID proxy
- σ_l as Uniform Information Density proxy.
  `compute_sentence_variability()` in `ai_score.rs`: splits on terminal
  punctuation (。！？!?), filters fragments <4 chars, requires ≥10 sentences,
  f64 accumulation, returns f32.
- `sentence_variability: Option<f32>` in `AiSignatureReport`.
- Contributes up to 0.15 to composite score when σ_l < 5.0.
- Top-signal: `句長變異低 σ=X.X（疑似 AI 均質化）`.

#### 40.9 Zero-width tokenizer artifact detection
- `scan_ai_zero_width()` in `grammar.rs`: scans for U+200B (ZWSP), U+200C (ZWNJ),
  U+200D (ZWJ), U+FEFF (BOM mid-text), U+200E/F (LRM/RLM).
  Per-occurrence AiStyle issues with empty-string suggestion for auto-removal.
- `zero_width_count: usize` in `AiSignatureReport`.
- Contributes up to 0.2 to composite score (3+ chars = max).
- Auto-fix: orthographic-tier only when `found` is entirely zero-width codepoints.
- Orthogonal to all other detection — works on aggressively paraphrased text.

#### 40.10 Punctuation density matrix (rhythmic forensics)
- `PunctuationProfile` and `PunctuationStat` in `ai_score.rs`.
  Five tracked types: comma (，), period (。), semicolon (；), dunhao (、),
  dash (——).
- Per-type density τ_x and CV of inter-punctuation distances.
  Minimum N ≥ 10 per type; f64 accumulation for CV.
- Exclusion zones respected.  Aggregate CV weighted by occurrence count.
- Contributes up to 0.1 to composite score.
- Top-signal: `標點節奏過於均勻（疑似 AI 生成）`.

#### 40.11 Composite score rebalancing
- Rebalanced signal weights: phrase density 0.5→0.7, structural 0.75→0.4.
  Theoretical max ~1.85 before clamp.
- Multi-dimensional corroboration: no single signal exceeds 0.7.
  Reaching ≥0.8 requires ≥2 distinct signal dimensions.
- Threshold gating: ≥0.8 overwhelming evidence, 0.4–0.8 moderate, ≤0.3 clean.
- Phase 4 (cross-strait terminology drift as scoring input) deferred.

### Translation Memory (37.x)

#### 37.3 Translation memory: persistent correction tracking
- `TranslationMemoryStore` in `store.rs`: JSON-file-backed per-project correction
  history at `.zhtw-tm.json` (discovered from cwd→.git root; overridable via
  `.zhtw-mcp.toml` `translation_memory` field or `--config`)
- `TmEntry` records: `found`, `scanner_suggested`, `user_chose`, `context`
  (optional), `timestamp` (ISO 8601 date)
- Suppression logic: when the last entry for a term has `user_chose == found`
  (user kept the flagged term), severity downgraded to Info in future scans.
  Info-severity issues do not count toward error/warning gates
- Profile boundary: orthographic issue types (Punctuation, Case, Variant,
  Grammar, AiStyle) are immune to TM suppression — only lexical/contextual
  rules (CrossStrait, Confusable, Typo, AiFiller) can be suppressed
- Fix integration: TM-suppressed terms excluded from `--fix` application
  (terms the user deliberately rejected are never auto-corrected)
- Deduplication: keyed by `found` term, latest decision wins; cap at 10,000
  entries for new terms (updates always allowed)
- Atomic writes with fs2 advisory locks; rollback on write failure
- CLI subcommands:
  - `tm list` — show all entries as JSON
  - `tm record --found F --suggested S --chose C [--context ctx]` — record decision
  - `tm export <file>` — export to file
  - `tm import <file>` — merge from file (latest-wins dedup)
  - `tm clear` — reset all entries
- MCP integration: `summary.tm_suppressed` count in tool response (omitted when 0)
- 9 unit tests: suppression, acceptance, dedup, persistence, clear, export/import
  round-trip, schema mismatch reset, path discovery, hand-edited duplicates
- Note: confidence boosting (`user_chose == scanner_suggested` → higher confidence)
  not yet implemented; only suppression (rejection tracking) is active.
  Context-aware suppression (per-context instead of per-term) deferred.

### Dictionary Performance

#### 30.1 Criterion benchmark suite for scan pipeline
- `benches/scanner.rs` with criterion 0.5, 35 benchmarks across 10 groups.
  Covers scanner construction (iter_batched, clone-free), plain-text scan at
  1KB/10KB/100KB, StrictMoe profiled scan, fix-path (apply-only + end-to-end
  scan+fix on 10KB with ~50 issues), context-clue-heavy scan (asserted >= 20%
  clue-gated issues at setup), markdown exclusion, FMM segmenter, post-scan
  transforms, and per-stage CPU attribution on 100KB.
- Baseline numbers (100KB, median):
  - `spelling_only`: 11.7ms (dominates — confirms 30.7's 90%+ finding)
  - `full_default`: 10.8ms
  - `punctuation_spacing`: 1.34ms
  - `grammar_only`: 748us
  - `build_exclusions_plain`: 727us
  - `case_only`: 620us
  - `baseline_no_checks` (NFC + detect + alloc): 448us
  - `detect_chinese_type`: 198us
  - `lineindex_100kb`: 58us
  - Context-clue text is 1.6-1.9x slower per-byte than general text.
  - Scan scales linearly (O(n)) across all sizes.
  - Fix path negligible: fixer adds <1% to scan+fix end-to-end.
- Supersedes 20.1 (scanner-only benchmarks); keep scanner baselines as subset.
- Relationship to 44.2: criterion detects THAT a regression happened;
  44.2's flamegraph/Tracy tooling explains WHY.
- Deferred: MCP lifecycle benchmark (optional per spec), CI regression gate
  (requires stable runner + `critcmp` or equivalent).

#### 30.7 Per-stage CPU attribution on 100KB input
- Added `bench_cpu_attribution_100kb` to `benches/scanner.rs`: 10 criterion
  benchmarks isolating each scan stage using `ProfileConfig` flags.
- Results on 100KB mixed CJK+ASCII (median):
  - `detect_chinese_type`: 0.56ms (1.3%)
  - `build_exclusions_plain`: 1.99ms (4.6%)
  - `spelling_only`: 40.8ms (90.8%) — dominates
  - `punctuation_spacing`: 4.3ms (6.5%)
  - `grammar_only`: 3.6ms (4.9%)
  - `case_only`: 2.0ms (1.2%)
  - `lineindex_100kb`: 74µs (post-scan overhead negligible)
  - `build_exclusions_markdown`: 1.96ms (2x heavier than plain text)
- Closed `detect_chinese_type` bitmap optimization as won't-fix: 1.3% of CPU,
  below the 5% profiling gate.

#### 43.5 Starter bitmask pruning (won't-fix)
- Closed: bytewise AC fallback path does not execute (charwise daachorse
  succeeds for the embedded ruleset).  Clue AC pre-scan is lazy and amortized.
  Profiling shows per-match filtering is the bottleneck, not AC traversal.

#### 43.7 Delimiter-aware pre-segmentation (won't-fix)
- Closed: AC traversal is not the bottleneck.  Spelling per-match processing
  (exclusion checks, context-clue lookups, word-boundary checks) accounts for
  90.8% of CPU.  Pre-segmentation adds offset tracking complexity for no
  measurable gain.

#### Per-match filtering optimizations
- Four optimizations targeting the spelling per-match hot path (90.8% of CPU):
  1. Precomputed superstring flag (`spelling_has_superstring: Vec<bool>`) in
     `Scanner::new()`: skips `already_correct_form()` for ~93% of rules where
     `from` is not a substring of any `to` entry.
  2. Linear exclusion cursor in `scan_spelling`: amortized O(1) exclusion
     checks via advancing cursor, replacing O(log E) binary search per match.
     Relies on daachorse's monotonic start-order iteration guarantee.
  3. Fast CJK guard in `word_straddles_boundary()`: checks immediate neighbor
     chars before the backward-walk + dictionary-probe loop.  Non-CJK
     boundaries return false immediately.  `match_straddles_word_boundary()`
     combines both edge checks into one call.
  4. Single-pass `count_clues_in_window()`: O(C * W) nested iteration rewritten
     to O(W) single scan with `[bool; 32]` bitset for dedup.  Fixed latent bug:
     old 16-entry bitset was too small for 6 rules with 17-21 context_clues
     (e.g. "程序" has 21).  Startup assert validates capacity.
- Results (criterion, 100KB mixed CJK+ASCII):
  - scan/100KB full pipeline: 17.0ms → 14.2ms (-16%)
  - spelling_only isolated: 40.8ms → 10.8ms (-73%, 3.8x)
  - scan_context_clues/100KB: 26.6ms → 26.2ms (-1.5%)

### Housekeeping

#### Drop sha2 and bincode deps, unify hashing on blake3
- Removed `sha2` (0.10) and `bincode` (1) dependencies; ~8 fewer transitive crates
  (digest, block-buffer, generic-array, typenum, crypto-common, cpufeatures,
  version_check)
- All hashing unified on `blake3` (already a dependency for scan cache):
  - `audit.rs`: `sha256_hex()` → `hash_hex()` via `blake3::Hasher`
  - `baseline.rs`: issue fingerprints use `blake3::Hasher` + `.to_hex()`
  - `loader.rs`: ruleset hash computation via `hash_hex()`
- Scan cache serialization: `bincode::serialize/deserialize` → `serde_json::to_vec/from_slice`;
  cache file renamed `.bin` → `.json` (human-readable, debuggable with `jq`)
- Old `.bin` cache files silently ignored (cache miss, no data loss)
- Audit trace IDs remain 128-bit hex strings; baseline fingerprint format unchanged
- `pulldown-cmark` bumped 0.12 → 0.13

### Disambiguation (43.2)

#### Positional clue syntax for directional disambiguation (PR [#45](https://github.com/sysprog21/zhtw-mcp/pull/45))
- `positional_clues` field on `SpellingRule`: optional `Vec<String>`
  with directional constraints on where context terms must appear
  relative to the AC match
- Five operators: `before:TERM` (within 20 chars after match),
  `after:TERM` (within 20 chars before match), `adjacent:TERM`
  (immediately next, either side), `not_before:TERM`, `not_after:TERM`
- Narrower 20-char windows (vs 40-char context windows) express
  proximity, not just co-occurrence
- Windows respect paragraph breaks and excluded ranges (code spans,
  URLs, suppressions) — same discipline as `context_byte_window`
- AND semantics: all positive clues must match, any negative vetoes;
  when both `context_clues` and `positional_clues` present, both
  must pass
- Fail-open parsing: unrecognized syntax logged to stderr and skipped
- Pre-parsed at `Scanner::new()` into `PositionalClue` enum for
  zero per-match parsing cost
- 13 unit tests: all 5 operators, paragraph break boundaries, code
  span exclusion, AND-with-context_clues, multi-condition gate,
  regression without clues
- Scope: `rules/ruleset.rs` (field), `engine/scan/mod.rs`
  (`PositionalClue` enum + parsing + bounds + 486 LOC),
  `engine/scan/spelling.rs` (hot-path gate)
#### Positional clue rule migration (PR [#53](https://github.com/sysprog21/zhtw-mcp/pull/53))
- Migrated 7 high-FP rules to `positional_clues`:
  - `函式→函數` (confusable): `not_after:程式/呼叫/回呼/匿名` suppresses
    when programming compounds precede the match (clause boundary bleed)
  - `最核心`, `核心問題`, `核心邏輯` (confusable, zero-clue): added
    `context_clues` for tech terms + `not_after:kernel/Linux`; no longer
    fire unconditionally on non-technical text
  - `核心功能` (confusable): added `context_clues` + `positional_clues`
    (already had `negative_context_clues`)
  - `項目→專案` (cross_strait): `not_after:個/各/每` suppresses when
    preceded by quantifiers (item usage, not project)
  - `調用→呼叫` (cross_strait, zero-clue): added `context_clues` for
    programming terms + `not_after:法院/調閱` to suppress legal usage
- `scripts/check-ruleset.py`: added `positional_clues` to
  `KNOWN_SPELLING_FIELDS` and `SPELLING_FIELD_ORDER`; syntax validation
  rejects malformed operator:term entries; guards non-list and non-string
  values with schema warnings instead of crashes
- Fixed pre-existing `word_straddles_boundary` false suppression:
  dictionary words starting inside a match span (e.g. `目的` overlapping
  the end of `項目`) no longer veto the match.  New
  `word_straddles_boundary_with_limit(Option<usize>)` skips backward-walk
  positions strictly inside the match while still catching longer words
  that begin at match_start
- Added `差分`, `形式`, `排程` to `GENERAL_VOCAB` for word-boundary
  disambiguation in academic/technical prose
- Scope: `assets/ruleset.json` (7 rules), `scripts/check-ruleset.py`,
  `engine/scan/spelling.rs`, `engine/segment.rs` (new API + unit test)

### Corpus Evaluation (36.0)

#### Corpus-based evaluation suite
- Three synthetic evaluation corpora in `tests/corpus/`:
  `ai-generated.json` (16 cases, 51KB), `native-zh-tw.json`
  (18 cases, 53KB), `cn-to-tw-conversion.json` (12 cases, 55KB)
- AI-generated corpus covers cross_strait, confusable, ai_filler
  (fixable and unfixable), and punctuation rules; includes
  context-clue-gated fillers with `to: []` (flag-only) and
  `to: [""]` (delete)
- Native zh-TW corpus includes false-friend edge cases: 循環
  (economics, not loop), 地址 (postal, not memory), 日誌
  (literary, not server log), 驅動 (verb, not driver), 開源節流
  (finance, not open source), 函數 (math, not programming)
- CN-to-TW corpus validates built-in S2T character conversion
  followed by vocabulary normalization scan; spans cloud, VM,
  debugger, threading, recursion domains
- Metrics: precision, recall, false-positive rate, safe-fix success
  rate; `expected_issues` (scanner output) and `expected_fixed`
  (fixer output) are intentionally independent
- Gate thresholds: precision >= 90%, FP rate <= 5% on native zh-TW,
  safe-fix success >= 85% on AI-generated corpus
- Harness: `nth_offset` panics on `occurrence: 0` (1-based);
  `matches_expected` handles rules with empty suggestions (`to: []`)
- `make corpus` target for local metric inspection
- Scope: `tests/corpus-evaluation.rs`, `tests/corpus/`,
  `Makefile`, `docs/internals.md`

### Fixer Optimization (47.2)

#### Fix O(n^2) in suppress_convergent_issues
- `suppress_convergent_issues` called `remap_to_post_fix` (O(n)) once
  per applied fix to build post-fix byte ranges, giving O(n^2) total
- Replaced with a single forward-pass delta accumulator: since
  `applied_fixes` are sorted by offset ascending and non-overlapping,
  a running prefix sum produces identical remapped positions in O(n)
- Added equivalence test comparing O(n) output against O(n^2) reference
  across 10 cases (empty, same-length, expansion, contraction, deletion,
  multi-fix combinations), plus suppression behavior tests
- Scope: `src/fixer.rs`

### TM Store Optimization (47.1)

#### Fix O(N*M) TM import and simplify TM index (PR [#54](https://github.com/sysprog21/zhtw-mcp/pull/54))
- `TranslationMemoryStore.index` was `HashMap<String, Vec<usize>>` but
  only ever read `.last()`. Simplified to `HashMap<String, usize>` where
  `build_tm_index` naturally keeps the last occurrence via `insert()`
- `record()`/`import()`: O(1) `index.get()` replaces O(N) `rposition` scan
- `import()`: incremental index maintenance replaces post-loop rebuild
- `record()` rollback: single `index.remove()` replaces full rebuild
- `import()` at cap: `continue` (still apply updates) instead of `break`
- Scope: `src/rules/store.rs` (24 added, 33 removed)

### Scan Optimization (45.2)

#### Per-rule bitflags to gate spelling filter (PR [#49](https://github.com/sysprog21/zhtw-mcp/pull/49))
- Precomputed `u8` bitmask per spelling rule at `Scanner::new()` time,
  encoding which optional filter stages are active: superstring guard,
  exception check, positive context clues, negative context clues,
  positional clues, and deletion extension
- Six `FILTER_*` constants and `FILTER_HAS_ANY_CLUE` convenience mask
  in `spelling.rs`; `process_spelling_match` checks `flags & CONST != 0`
  before each optional stage, skipping data load and branch when clear
- 79% of rules in default ruleset have `flags == 0`, so all guarded
  blocks are bypassed for the dominant rule class
- `spelling_has_superstring: Vec<bool>` eliminated from `Scanner`;
  superstring check folded into `FILTER_HAS_SUPERSTRING` bit
- Negative context clues normalized at build time: empty vecs collapsed
  to `None` (matching positive clue treatment), enabling
  `scan_clues_in_window` early-exit when only positive clues present
- Benchmark (criterion, 100KB mixed CJK+ASCII, Apple M-series):
  spelling_only 34.4ms -> 17.7ms (-49%), full_default 36.0ms -> 22.9ms
  (-36%), context_clues 70.8ms -> 58.3ms (-18%)
- Construction overhead +5ms (per-rule flag computation loop, 1200 rules
  x 6 checks) -- one-shot startup cost, per-scan savings dominate
- Scope: `engine/scan/mod.rs`, `engine/scan/spelling.rs` (192 insertions,
  276 deletions net reduction)
