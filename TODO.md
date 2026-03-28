# TODO.md

Technical roadmap. Completed items in CHANGELOG.md. Epic IDs (e.g. 20.x) persist
across sections, reflect origin not priority.

Ordered by priority tier: P0 (spec/safety) → P1 (performance &
instrumentation) → P2 (correctness & linting quality) → P3 (infrastructure) →
P4 (feature expansion) → P5 (ecosystem) → P6 (data-dependent/advanced) →
Deferred.

Within tiers: performance > correctness > linter quality > MCP robustness.

Status key: `[ ]` not started, `[~]` partial

---

## P1: Performance & Instrumentation

Scan latency, startup cost, and the measurement infrastructure that
validates future optimizations.

Ordering based on 44.2 CPU attribution (node1, 100KB, 2026-03-26):
spelling stage is 88% of scan time; clue-heavy text is 2.2x slower
(likely dominated by post-match context evaluation).  Everything else
is ~10% or less.  Two tracks: (a) instrument → attack the 88% spelling
hot path (48.x IR refactor), (b) reduce startup/output allocation
overhead (49.x, dhat-driven, parallelizable with 48.x).

Note: per-stage percentages are non-additive (sum to ~116%) because
each isolated benchmark includes shared overhead (detect_chinese_type,
alloc, sort, overlap resolution).  The percentages indicate relative
weight, not exclusive cost.

### 44.1 Structured instrumentation with `tracing`
- [ ] Replace ad-hoc `eprintln!` calls with the `tracing` crate's
      structured spans and events.  Prerequisite for measuring any
      optimization (45.x, 46.x, 48.x) and for 25.7 MCP LogMessage.
  - Phase 1 -- Subscriber + stderr output:
    - Add `tracing` + `tracing-subscriber` (`fmt` feature).
    - Wrap key operations in spans: `scan`, `fix`, `mcp_request`,
      `load_ruleset`, `build_ac`.
    - Record fields: `content_length`, `issue_count`, `fix_count`,
      `elapsed_ms`, `content_type`, `profile`.
    - Log level mapping: `error` fatal, `warn` config issues, `info`
      request lifecycle, `debug` per-rule match decisions, `trace`
      AC traversal.
    - CLI: `RUST_LOG` env var (default `warn`).  `--verbose` -> `info`,
      `--debug` -> `debug`.
  - Phase 2 -- MCP LogMessage bridge (subsumes 25.7):
    - Implement a `tracing::Layer` on the subscriber registry.  The MCP
      layer captures events at `info`+ and serializes as
      `notifications/message` onto the JSON-RPC stdout transport.
      The `fmt` layer writes to stderr.  No layer may write to stdout
      except through the MCP transport encoder.
    - Prerequisite for Phase 2 only: add `logging: Option<Value>` to
      `ClientCapabilitiesRaw` in `src/mcp/types.rs:163` and
      `logging: bool` to `ClientCapabilities` (~10 LOC).
  - Phase 3 (deferred) -- OpenTelemetry export: only if HTTP transport
    (7.2) lands.
  - Dependency budget: `tracing` + `tracing-subscriber` (~300-500KB).
    No async runtime.  Verify with `cargo tree -e features`.
  - Scope: `Cargo.toml` (2 new deps), `src/main.rs` (subscriber init),
    `src/mcp/transport.rs` (MCP log layer), `src/engine/scan/mod.rs`
    (scan span), `src/mcp/tools.rs` (request span).
  - Gate: `RUST_LOG=debug cargo run -- lint file.md` produces structured
    span output on stderr.  Default produces zero stderr on clean runs.

### 44.2 CPU profiling infrastructure for scan pipeline
- [~] Formalize repeatable profiling tooling.  Critical prerequisite for
      45.x (filter fast paths), 46.x (AC acceleration), and 48.x (IR
      refactor): all require measured baselines to justify complexity.
  - **Done:**
    - **Criterion benchmark suite** (`benches/scanner.rs`, 636 lines):
      construction breakdown, per-stage CPU attribution on 100KB,
      context-clue-heavy scanning, fix path, markdown exclusion, segmenter.
    - **`justfile`** with profiling recipes: `bench-node1`, `perf-stat`,
      `flamegraph`, `dhat`, `bench-cpu`.
    - **`dhat-rs`** allocation profiling: `cargo run --profile profiling
      --features dhat-heap -- lint <file>` produces `dhat-heap.json`.
      `[profile.profiling]` keeps debug symbols for readable stacks.
    - **`cargo flamegraph`** on node1: `just flamegraph` generates SVG.
    - **`perf stat`** on node1: `just perf-stat` reports cache/branch
      miss rates.
  - **Baseline data** (node1, x86_64 64-core, Rust 1.94.0, 2026-03-26):
    ```
    [Pre-optimization, 2026-03-26]
    Criterion per-stage (in-process, no startup):
    full_default          15.43 ms  (reference, 100%)
    spelling_only         13.58 ms  (88.0%)
    punctuation_spacing    1.58 ms  (10.3%)
    grammar_only           1.02 ms  ( 6.6%)
    case_only              0.69 ms  ( 4.5%)
    baseline_no_checks     0.51 ms  ( 3.3%)
    detect_chinese_type    0.32 ms  ( 2.1%)
    build_exclusions_plain 0.11 ms  ( 0.7%)
    lineindex_100kb        0.08 ms  ( 0.5%)
    scan_context_clues/100KB   36.1 ms  (2.2x vs scan/100KB)
    ```
    ```
    [Post-optimization, 2026-03-28]
    Criterion per-stage (in-process, no startup):
    full_default          11.53 ms  (reference, 100%)     -25%
    spelling_only          9.91 ms  (85.9%)               -27%
    punctuation_spacing    1.68 ms  (14.6%)               noise
    grammar_only           0.62 ms  ( 5.4%)               -39%
    case_only              0.79 ms  ( 6.9%)               noise
    baseline_no_checks     0.63 ms  ( 5.4%)               noise
    detect_chinese_type    0.35 ms  ( 3.0%)               noise
    ac_traversal_only      0.16 ms  ( 1.4%)               NEW
    build_exclusions_plain 0.10 ms  ( 0.9%)               noise
    lineindex_100kb        0.08 ms  ( 0.7%)               noise
    scan_context_clues/100KB  18.0 ms  (1.6x vs scan/100KB)  -50%

    End-to-end scan benchmarks:
    scan/1KB               126 us                         -14%
    scan/10KB             1.11 ms                         -30%
    scan/100KB           11.55 ms                         -29%
    strict_moe/100KB     11.77 ms                         -27%
    scan_and_fix/10KB     1.12 ms                         -29%
    scanner_construction  8.71 ms                         +1%
    ```
    ```
    CLI wall-clock (post-optimization):
    tiny file (startup)   ~140 ms   (S2TConverter still eager in CLI)
    100KB lint            ~150 ms   (dominated by startup, not scan)

    perf stat (100KB, post-optimization):
    IPC              1.27
    L1-dcache miss   3.19%
    branch miss      2.66%
    wall-clock       168 ms  (+/- 2%)

    Construction (one-time, criterion):
    scanner_construction       8.71 ms  (full Scanner::new)
    spelling_aho_corasick      1.92 ms  (bytewise AC, NOT charwise DAAC)
    segmenter_from_rules       0.56 ms
    case_aho_corasick          0.06 ms
    ```
  - **Key findings** (updated 2026-03-28):
    1. Spelling stage reduced from 88% to 86% of scan time (-27%).
       Boundary bitmap eliminated all per-hit segmenter calls.
    2. Clue-heavy text reduced from 2.2x to 1.6x vs base scan (-50%).
       Document-wide clue pre-scan + binary search.
    3. Grammar reduced 39% via AC prefilter (35 patterns, single pass).
    4. AC traversal floor measured at 0.16ms (1.4% of full scan).
       The remaining 9.8ms is post-match eval + post-scan pipeline.
    5. L1-dcache miss rate increased slightly to 3.19% (from 2.21%)
       due to boundary bitmap memory; still not a bottleneck.
    6. IPC decreased to 1.27 (from 1.73); more branch-heavy code from
       bitmap lookups and fused passes.  Trade-off: fewer total
       instructions executed.
  - **Remaining**:
    - **Tracy** (opt-in, stretch goal): `cfg(feature = "profile-tracy")`
      for phase-level timeline analysis.  Not blocking any work.
  - Scope: `Cargo.toml` (dhat dep, profiling profile), `justfile`,
    `src/main.rs` (dhat global allocator + lazy S2TConverter).
  - Gate: ✅ `just flamegraph` produces valid SVG.  ✅ `just perf-stat`
    reports cache miss rates.  ✅ `just dhat` produces dhat-heap.json.
    Tracy is optional stretch goal.

### 48.x sub-tasks 2-6: AC rule IR refactor ✅
Completed 2026-03-27.  See CHANGELOG.md for details.
Sub-task 7 (sentence boundary) is 37.1 (P3).
Sub-tasks 9-12 closed (superseded); see Deferred section.

### 49.1 Typed JSON/SARIF output ✅
### 49.2 Build-time postcard ruleset ✅
### 49.3 Lazy scan cache loading ✅
Completed 2026-03-27.  See CHANGELOG.md for details.

### 50.x Post-IR scan pipeline optimization

Promoted from Deferred based on post-optimization profiling (2026-03-28,
node1).  The 48.x IR refactor + boundary bitmap + grammar AC prefilter
achieved -30% scan, -50% clue-heavy, -38% grammar.  The remaining cost
is spread across bitmap construction, eval overhead, sort/overlap,
inflation, and post-scan passes -- no single dominant bottleneck.

Current measurements (node1, 100KB):
```
spelling_only:  9.7 ms  (AC floor: 0.175 ms)
full_default:  11.0 ms
context_clues: 17.9 ms
grammar_only:   0.6 ms
```

Remaining cost centers in spelling_only (9.7ms, ~3700 hits):
```
Bitmap construction:     ~1.5ms  (O(N*L^2) dict probes)
Per-hit eval overhead:   ~1.5ms  (3700x exclusion+bitmap+emit)
Sort + overlap:          ~1.0ms  (O(n log n), n=3700)
Issue inflation:         ~1.0ms  (~3000 survivors x 5 clones)
Line/col + clue + detect ~1.3ms  (post-scan passes)
AC traversal:            ~0.2ms  (DAAC floor)
Misc:                    ~3.2ms
```

#### 50.1 Trie-based segmenter dictionary
- [ ] Replace `HashMap<String, u32>` in Segmenter with a character-
      indexed trie (e.g. double-array trie or nested `HashMap<char, _>`).
      `build_boundary_bitmap_from_chars()` currently tries every 2..=L
      char substring and hashes it into the HashMap.  A trie reduces
      each prefix probe to a single-node traversal.
  - Estimated: -0.5 to -1.0ms on bitmap construction.
  - Scope: `src/engine/segment.rs`.
  - Complexity: M.  Risk: med (dict API change, must preserve MMSEG).
  - Gate: bitmap construction time measurably reduced.

#### 50.2 Profile-aware AC pattern filtering
- [ ] Under the default profile, 68 rules (47 variant + 16 ai_filler +
      5 political) are always rejected by config gates.  Remove them
      from the AC automaton entirely for default-profile scans.
  - Currently profile gates fire at the AC loop level (fast-reject
    before MatchContext), but the patterns still generate AC state
    transitions.  Removing them shrinks the DAAC by ~5%.
  - Two options: (a) build separate AC per profile at Scanner::new(),
    or (b) rebuild AC lazily on first scan when profile is known.
  - Scope: `src/engine/scan/rule_ir.rs`, `src/engine/scan/mod.rs`.
  - Complexity: M.  Risk: med (profile must be known at AC build time).
  - Gate: default-profile spelling_only measurably faster.

#### 50.3 Sort-free overlap for offset-ordered AC hits
- [ ] The AC produces hits in offset order.  After eval, issues are
      already nearly offset-sorted.  Replace the O(n log n) sort with a
      debug_assert verifying order, or an is_sorted() short-circuit.
  - Grammar issues are appended after overlap (separate pass).
  - Non-spelling scanners also produce offset-ordered output.
  - Scope: `src/engine/scan/mod.rs`.
  - Complexity: S.  Risk: low.
  - Gate: sort removed or short-circuited.  Identical output.

#### 50.4 Consolidate eval path duplication
- [ ] Three code paths duplicate exclusion, boundary bitmap, and issue
      construction: inlined CLASS_TRULY_SIMPLE, eval_simple, and
      eval_predicates.  Extract shared micro-helpers.
  - Scope: `rule_ir.rs`, `spelling.rs`.
  - Complexity: S.  Risk: low.
  - Gate: identical behavior, fewer code paths to maintain.

#### 50.5 Reduce issue inflation cost
- [ ] `inflate_spelling_issues` clones ~15K strings for ~3000 survivors.
      Use `Arc<[String]>` for suggestions in CompiledSpellingDb so
      inflation is an atomic increment, not a deep clone.
  - Estimated: -0.3 to -0.5ms.
  - Scope: `rule_ir.rs`, possibly `ruleset.rs`.
  - Complexity: M.  Risk: med (may touch public `Issue` type).
  - Gate: inflation cost measurably reduced on 100KB.

#### 50.6 Skip redundant post-scan passes
- [ ] Fuse inflation + line/col fill into one pass.  Short-circuit
      pre-overlap sort when issues are already offset-ordered (see 50.3).
      Extend `offset_only` mode to skip inflation fields not needed by
      MCP compact output.
  - Estimated: -0.3 to -0.5ms.
  - Scope: `mod.rs` (scan_with_config_into pipeline).
  - Complexity: S.  Risk: low.
  - Gate: fewer passes visible in profiling.  Identical output.

---

## P2: Linting Quality & Correctness

Items that directly reduce false positives, improve scan correctness,
or extend coverage to new content types.  Ordered: FP reduction >
correctness > coverage.

### 36.3 Source code content type with comment extraction
- [ ] When AI agents send source files (`.rs`, `.py`, `.ts`, `.c`) through
      the MCP tool, code syntax triggers massive false positives on
      identifiers, string literals, and keywords.  Current content types
      (`plain`, `markdown`, `yaml`) have no source code handling.
  - Add `ContentType::SourceCode(Lang)` with per-language comment
    extraction.  Only comments and doc-comments are scanned; everything
    else becomes an exclusion zone.
  - Phase 1: conservative line-prefix heuristic.  Match `^\s*//`,
    `^\s*#`, `^\s*--` at line start for line comments and simple
    `/* ... */` block comment matching.  Accept higher false-negative
    rate (missed inline comments) rather than false positives from `//`
    inside string literals.  Pure Rust, zero dependencies.
  - Phase 2 (deferred): tree-sitter for precise extraction.  Only if
    Phase 1 false-negative rate proves unacceptable in practice.
  - Languages: Rust, Python, C/C++, JavaScript/TypeScript, Go, Java,
    Shell.
  - Auto-detection: file extension -> language mapping in CLI
    (`resolve_file_args()` / per-file lint loop).  MCP:
    `content_type: "source_rust"` or
    `content_type: "source"` (auto-detect from shebang or heuristics).
  - Scope: `src/engine/excluded.rs` (comment boundary extraction),
    `src/engine/scan/mod.rs` (ContentType variant + pipeline wiring),
    `src/mcp/tools.rs` (content_type parsing), `src/main.rs` (ext map).
  - Gate: scanning a `.rs` file with Chinese comments flags issues in
    comments but not in string literals or identifiers.

### 42.5 Ruleset validation subcommand
- [ ] Add `zhtw-mcp ruleset validate` subcommand that catches latent
      FP bugs in the ruleset:
    1. Same-field conflict: `from` of rule A == `to` of rule B within the
       same rule type (potential over-conversion chain).
    2. Duplicate `from` entries across rules.
    3. Missing `context_clues` on cross-strait rules with ambiguous terms
       (the "latent false-positive bug" documented in CLAUDE.md).
    4. `@seealso` reference validation (currently in `check-ruleset.py`).
    5. `english` field non-empty for rules participating in consistency
       analysis (prerequisite check for 35.1).
  - `--strict` flag: also report identity-style rules (`from == to[0]`).
  - Output: issue list with severity (error/warning/info), exit code 1 on
    errors.  `--json` for CI integration.
  - Replaces `scripts/check-ruleset.py --lint` with a faster Rust
    implementation.  Migration checklist:
    1. Achieve command parity: every check in `check-ruleset.py --lint`
       (currently invoked by `make check`, Makefile:23) must have
       an equivalent in the Rust validator.
    2. Update `Makefile` to call `zhtw-mcp ruleset validate` instead of
       `python3 scripts/check-ruleset.py --lint`.
    3. Update `docs/rules.md:129` which references the Python script.
    4. Keep `check-ruleset.py --format` (sorting/formatting) as a
       separate concern -- only the `--lint` path is replaced.
  - Scope: `src/main.rs` (subcommand), new `src/rules/validate.rs`.
  - Gate: `zhtw-mcp ruleset validate` exits 0 on current `ruleset.json`.
    Catches all issues that `check-ruleset.py --lint` catches (run both,
    diff output).  CI uses the Rust validator.

### 35.9 Project glossary support
- [ ] Teams need project-local terminology overrides beyond `ignore_terms`.
      Support a `[glossary]` section in `.zhtw-mcp.toml`:
  ```toml
  [glossary]
  preferred = ["執行緒", "記憶體", "程式碼"]
  banned = ["線程", "內存", "代碼"]
  proper_nouns = ["TSMC", "MediaTek"]
  ```
  - `preferred`: terms that must be used (consistency check fires if
    alternatives appear).  Interacts with 35.1: glossary `preferred`
    overrides the default TW-preferred form when resolving consistency
    diagnostics.
  - `banned`: terms that always fire regardless of context clues.
  - `proper_nouns`: added to exclusion list (never flagged).
  - Override tracking: glossary entries in tool response show
    `source: "project_glossary"` vs `source: "ruleset"`.
  - Scope: `src/config.rs` (parse glossary section),
    `src/rules/store.rs` (merge glossary into override/suppression
    stores -- required to implement the precedence contract below),
    `src/mcp/tools.rs` (pass glossary to scan config),
    `src/engine/scan/mod.rs` (exclusion for `proper_nouns`).
    Response schema: add `source` field to issue JSON.
  - Precedence: glossary `banned` > TM (37.3, done) > glossary
    `preferred` > domain pack > embedded ruleset.
  - Gate: `.zhtw-mcp.toml` with `banned = ["線程"]` flags `線程` even
    without context clues.  `proper_nouns = ["TSMC"]` suppresses
    matches on `TSMC`.  Issue JSON includes `source: "project_glossary"`.

### 35.1 Document-wide terminology consistency report
- [ ] Same document using both zh-TW and zh-CN variants of the same concept
      (e.g. 程式/程序, 執行緒/線程, 記憶體/內存) is a higher-signal problem
      than any single occurrence.  Current scanner reports each instance
      independently; no cross-document aggregation.
  - Fix: add a consistency pass after the main scan, before fix
    application.  Group issues by the `english` field on the matched rule
    (natural equivalence class key).  If both TW-preferred and CN-preferred
    forms of the same concept appear in the same document, emit a
    `Consistency` diagnostic with all locations + suggested uniform term.
  - Stage: operates on raw scan output.  TM-suppressed issues (severity
    downgraded to Info) are excluded from consistency grouping to avoid
    flagging user-approved terms.  Runs before fix application.
  - Output: new `consistency` array in tool response (alongside `issues`),
    each entry: `{ term_group, preferred, occurrences: [{offset, found}] }`.
    CLI: append consistency block when `--consistency` flag is set.
    Output modes: included in full/compact; omitted in summary/tabular.
  - Note: 35.9 (project glossary) `preferred` list provides an explicit
    override for which term wins.  Without a glossary, the TW-preferred
    form from the rule's `to` field is used.
  - Prerequisite: depends on rules having non-empty `english` fields for
    equivalence grouping.  42.5 (ruleset validation) enforces this; 35.1
    degrades gracefully without it (rules lacking `english` are simply
    excluded from consistency analysis).
  - Scope: post-scan aggregation in `src/mcp/tools.rs` and `src/main.rs`.
    No scanner changes -- reuses existing issue data.
  - Gate: `cargo test consistency` passes.  Test fixture: document
    containing both `記憶體` and `內存` produces a `Consistency`
    diagnostic with `term_group: "memory"`.  Document with only TW forms
    produces zero consistency diagnostics.  Benchmark: <1ms overhead on
    100KB document with no mixed usage.

---

## P3: Infrastructure & Refactoring

Foundational work that unblocks P2/P4 items or reduces maintenance cost.

### 47.3 Remove dead public methods in scan module
- [ ] `scan_with_excluded` and `scan_profiled_yaml` in
      `src/engine/scan/mod.rs` have zero callers outside the file.
  - Delete both methods and any test-only wrappers that exist solely
    to exercise them.
  - Scope: `src/engine/scan/mod.rs`.
  - Gate: `cargo test` passes.

### 47.4 Extract per-format output functions from run_lint_batch
- [ ] `run_lint_batch` in `src/main.rs` is ~823 lines handling file
      resolution, scanning, fixing, and 5 output formats (human, json,
      sarif, compact, tabular).  Each format arm should be its own function.
      Note: 49.1 (streaming JSON/SARIF) should land first as the
      performance-motivated subset; 47.4 is the structural cleanup.
  - Extract: `format_human()`, `format_compact()`, `format_tabular()`,
    `format_sarif()`, `format_json()`.
  - Extract shared helpers: `display_path()` (duplicated between compact
    and tabular arms), `format_suggestions()` (duplicated 3 times across
    human/tabular/SARIF).
  - Also: deduplicate content-type resolution between lint path and
    convert path (they disagree on aliases like `"md"`, `"yml"`).
    Extract `fn resolve_content_type(override_str: Option<&str>,
    file_path: Option<&str>) -> ContentType`.
  - Absorbs former 33.1: ensure CLI compact and MCP compact produce
    identical one-line representations.  Verify token-reduction gate:
    >=40% vs. human default, >=60% vs. `--explain`.
  - Scope: `src/main.rs`.
  - Gate: `cargo test` passes.  No behavioral change.

### 47.5 Deduplicate issue-grouping logic in MCP tools
- [ ] `build_compact_groups` and `group_issues` in `src/mcp/tools.rs`
      both iterate issues, build a BTreeMap keyed by
      `(found, rule_type, suggestions, severity)`, accumulate count and
      locations.  Only the value type differs (`CompactGroup` vs
      `IssueGroup`).
  - Fix: extract a single grouping function returning a richer type
    that both consumers transform from.
  - Also: eliminate unnecessary `issues.clone()` when TM store is None
    in the fix path (pass `&issues` directly).
  - Scope: `src/mcp/tools.rs`.
  - Gate: `cargo test` passes.  Identical MCP output.

### 47.6 Deduplicate store open/schema-check pattern
- [ ] `OverrideStore::open`, `SuppressionStore::open`, and
      `TranslationMemoryStore::open` in `src/rules/store.rs` are
      structurally identical: read file, deserialize, check schema
      version, backup-and-reset on mismatch, backup-and-reset on
      corrupt JSON, create default if missing.
  - Fix: extract a generic helper:
    `fn load_or_reset<T: Default + DeserializeOwned + Serialize>(
        path: &Path, expected_version: u32,
        get_version: impl Fn(&T) -> u32,
    ) -> Result<T>`
  - The three `open()` methods become thin wrappers.
  - Also: `OverrideStore` clones entire `Overrides` for mutation
    rollback safety, while `SuppressionStore` mutates in place with
    rollback.  Pick one pattern (in-place + rollback is correct).
  - Scope: `src/rules/store.rs`.
  - Gate: `cargo test` passes.

### 47.7 Eliminate double merge in build_merged_rules
- [ ] `build_merged_rules` (`src/rules/store.rs`) calls
      `load_spelling_rules()` which already merges base+overrides,
      then feeds that merged result into another `merge_spelling_rules`
      call.  The base+override merge runs twice.
  - Fix: assemble all layers (base, overrides, packs) as slices and
    call `merge_spelling_rules` exactly once.
  - Also: `merge_spelling_rules` HashMap keys clone the `from` String
    on every insert.  Use `swap_remove(i)` instead of `remove(i)` in
    the Scanner::new dedup loop (O(1) vs O(n) per removal).
  - Scope: `src/rules/store.rs`, `src/engine/scan/mod.rs`.
  - Gate: `cargo test` passes.  Identical merged ruleset.

### 35.7 AST-aware Markdown linting
- [~] Markdown exclusion zones (code fences, HTML, frontmatter) already
      implemented in `src/engine/markdown.rs`.  Missing: structural weighting
      and per-element profile differentiation.
  - [ ] Heading text: higher severity weight.  Add `severity_boost` to
    `ProfileConfig`, apply +1 severity for issues inside heading nodes.
  - [ ] Table cell awareness: report column index for editor integration
    (useful for SARIF `region` output).
  - [ ] Front matter vs body: scan YAML front matter values with relaxed
    punctuation rules (half-width colon allowed).  Currently excluded
    entirely.  Hardcode a minimal `ProfileConfig` for front matter
    (half-width colons allowed, spacing relaxed).
  - Scope: `src/engine/markdown.rs`, `src/engine/scan/mod.rs`,
    `src/main.rs`.
  - Gate: heading-severity test shows boosted severity.  No performance
    regression on 100KB Markdown.

### 20.6 Tool edge-case test coverage ([#18](https://github.com/sysprog21/zhtw-mcp/issues/18))
- [ ] Add targeted tests for `handle_tools_call` in `src/mcp/tools.rs`.
  - Already covered (cli-lint.rs / e2e-mcp.rs): `max_errors` gate, profile
    selection, content_type markdown/plain, unknown params (31.5), size
    limit, structured error `data` field (25.4, done).
  - Still missing: empty text, `fix_mode`+`political_stance` interaction,
    `explain`+`compact` mutual behavior, `max_errors=0` as explicit
    pass-all.
  - Gate: ≥8 tests covering remaining parameter-combination edge cases.

### 37.1 Sentence / paragraph boundary index (shared infrastructure)
- [ ] Extract a reusable sentence and paragraph boundary index as shared
      infrastructure.  Grammar checks (34.0, done) define sentence
      boundaries ad-hoc; this replaces that.  Also enables 37.2.
  - For Chinese text: segment on `。`/`？`/`！`/`；` and `\n\n`.
    For mixed CJK/Latin: also split on `.`/`?`/`!` followed by
    whitespace + uppercase.
  - Output: `Vec<SentenceBound { byte_start, byte_end }>` computed once
    per scan, passed to grammar scanner.
  - Abbreviation deny-list for false splits (Mr., P.S.).
  - Scope: `src/engine/segment.rs` or new `sentence.rs`, wired into
    `scan/mod.rs`.
  - Gate: sentence boundaries on mixed CJK/Latin text match manually
    annotated gold set with >=95% F1.

### 25.5 Log MCP protocol version mismatch on initialize ([#11](https://github.com/sysprog21/zhtw-mcp/issues/11))
- [ ] Server does not inspect client `protocolVersion`.
  - Per MCP spec, server returns its version; client decides disconnect.
  - Fix: log warning on mismatch, no handshake behavior change.
  - Gate: E2E test verifies initialize succeeds with mismatched version.

---

## P4: Feature Expansion

New capabilities that extend the tool's reach.  Items with prerequisites
are listed after their dependencies.

### 35.2 Enriched explain schema with anchor provenance
- [~] Explain mode currently appends a prose `explanation` string per
      issue.  Anchor provenance (`anchor_en`, `anchor_match`) already
      serialized via `AnchorProvenance` struct in `src/mcp/tools.rs`.
      Remaining:
      upgrade to structured fields for teaching-quality review.
  - `rationale` — why this is flagged (rule context + MoE reference)
  - `domain` — which register/domain triggered (or "general")
  - `is_false_friend` — boolean, zh-CN and zh-TW share characters but
    different meanings (e.g. 文件: document vs. file)
  - `auto_fix_safe` — boolean, whether safe fix would apply this
  - `needs_review` — boolean, sampling or manual confirmation recommended
  - MCP: structured JSON object per issue (alongside current text).
    CLI: human-readable rendering of the same fields.
  - Core fields (`rationale`, `domain`, `auto_fix_safe`) can ship
    without 35.6 Phase 4.
  - Scope: `src/mcp/tools.rs` explain builder, `src/main.rs` explain
    formatter.
  - Gate: explain output for a context-clue-gated rule includes `domain`,
    `rationale`, and `auto_fix_safe`.  No extra tokens when `explain` is
    not requested.

### 37.4 HTML content type with text-node-only scanning
- [ ] Add `ContentType::Html` that extracts scannable text runs from HTML,
      treating tags, attributes, `<script>`, `<style>`, comments, and CDATA
      as exclusion zones.
  - Phase 1: use `html5ever` for correct HTML parsing (~500KB binary
    size).  If rejected: fall back to conservative state machine scanning
    text between `>` and `<` boundaries.
  - Auto-detection: `.html`, `.htm` extensions.  MCP:
    `content_type: "html"`.
  - Scope: `src/engine/excluded.rs`, `src/engine/scan/mod.rs`,
    `src/mcp/tools.rs`, `src/main.rs`.
  - Gate: scanning an HTML file with Chinese text in `<p>` tags flags
    issues but ignores tag attributes, script blocks, and CSS.

### 42.4 Pre-fix backup
- [ ] Add `--backup` flag to `lint --fix`: creates timestamped backup
      directory, copies each file that will be modified, then applies fixes.
  - Safety integration:
    1. Not in a git repo and `--backup` not set → warn.
    2. In a git repo but uncommitted changes → warn.
    Do NOT block — just warn.  `--yes` suppresses warnings.
  - Backup path: `.zhtw-backup/YYYYMMDD_HHMMSS/` relative to scan root.
  - Scope: `src/main.rs` (~50 LOC).
  - Gate: `--backup --fix` creates backup directory with correct copies;
    files modified only after backup completes.

### 42.3 Fix preview with diff output
- [ ] Add `--show-diff` flag to `lint --fix`: runs scan, prints unified
      diff (original vs. fixed) per file, then prompts `Apply? [y/N]`
      before writing.  `--yes` skips the prompt (CI mode).
  - Diff format: `--- a/file.md` / `+++ b/file.md` with line context,
    colored red/green on TTY.  Reuses `--fix --dry-run` scan result.
  - Relationship to `--dry-run`: `--dry-run` reports issues in lint format;
    `--show-diff` shows the actual text transformation as a diff.
    `--show-diff` implies `--fix`.
  - Scope: `src/main.rs` (diff formatter + confirmation prompt).  ~100 LOC.
  - Gate: `--show-diff` on a file with 3 issues produces a valid unified
    diff with exactly 3 changed hunks.

### 37.2 Sentence-level issue grouping in output
- [ ] Add sentence-level grouping as an output mode for documents with
      many issues.
  - CLI: `--group-by sentence` groups issues under their containing
    sentence.  `--group-by rule` groups by `(issue_type, found)`.
  - MCP: `group_by: "sentence"` parameter wraps issues in
    `{ sentence_text, sentence_offset, issues: [...] }` array.
  - Default: ungrouped (current behavior).
  - Prerequisite: 37.1 (sentence boundary index).
  - Scope: `src/mcp/tools.rs` (MCP output builder), `src/main.rs`
    (CLI formatter).
  - Gate: grouped output produces fewer groups than issues.  Test:
    document with 10 issues across 3 sentences produces exactly 3 groups.

### 45.1 Grammar pattern AC prefilter ✅
Completed 2026-03-27.  35-pattern AC, -38% grammar.  See CHANGELOG.md.

### 33.2 Per-rule hit count stats for lint runs
- [ ] Add per-rule hit counts to `ScanOutput` and CLI `--stats` flag.
  - CLI: `--stats` appends summary block + top-N rules by frequency.
  - Track per-rule hit counts using `HashMap<(IssueType, String), u32>`.
  - MCP: fold behind `include_stats: true` parameter.  Goal: 0 extra
    tokens by default.
  - Scope: `src/engine/scan/mod.rs` (ScanOutput), `src/mcp/tools.rs`
    (MCP), `src/main.rs` (CLI).
  - Gate: CLI stats block adds ≤3 lines.  No measurable latency impact.

### 36.2 Baseline management tooling
- [~] Baseline mode (`--baseline`) and `--update-baseline` exist for
      incremental adoption.  Missing: lifecycle management subcommands.
  - `zhtw-mcp baseline prune <file>` — remove stale fingerprints (issues
    whose source text no longer exists in the scanned files).
  - `zhtw-mcp baseline diff <file>` — show which baselined issues would
    fire if the baseline were removed (helps assess remaining debt).
  - Scope: `src/main.rs` (subcommands), `src/baseline.rs` (prune/diff
    logic).
  - Gate: `baseline prune` on a file with deleted rules produces a smaller
    baseline file.

### 42.9 Inline suppression directive aliases
- [~] Done (14.2): `<!-- zhtw:disable-next-line -->`,
      `<!-- zhtw:disable-block -->` / `<!-- zhtw:end-disable -->`.
      Also implemented: `// zhtw:disable` / `// zhtw:ignore` same-line
      suppression (all content types, `//` prefix only).  Remaining:
  - [ ] `<!-- zhtw:disable-line -->`: suppress all issues on the same
    line as the directive.  Markdown only (HTML comment syntax).
  - [ ] `<!-- zhtw:disable -->` / `<!-- zhtw:enable -->`: shorter
    aliases for `disable-block` / `end-disable`.
  - [ ] `# zhtw:disable-next-line` and `# zhtw:disable` for Python/
    Shell source comments.  Currently only `//` prefix is recognized.
    Guard with `ContentType::SourceCode` (36.3) to avoid matching `#`
    inside prose text.
  - Scope: `src/engine/suppression.rs` (~20 LOC).
  - Gate: test fixture with `<!-- zhtw:disable-line -->` on the same
    line as `線程` suppresses that issue.  `# zhtw:disable` in a `.py`
    file suppresses that line.

---

## P5: Ecosystem & Domain

Items that broaden domain coverage, improve workflows, or add UX polish.

### 33.3 Claude Code hook for automatic zh-TW linting
- [ ] Implement `PostToolUse` hook that auto-invokes `zhtw-mcp lint` on
      files matching `*.md`, `*.txt`, `*.yml` when the tool is `Write`
      or `Edit`.
  - No bash script.  Add `zhtw-mcp internal hook-callback` hidden
    subcommand that reads the Claude Code hook JSON payload from stdin,
    extracts `file_path`, checks extension, runs the lint, and writes
    the result JSON to stdout.  All logic in Rust -- cross-platform.
  - Add `zhtw-mcp hook install` subcommand that writes the hook entry
    to `~/.claude/settings.json`.
  - Token budget protection:
    1. Emit nothing (exit 0, empty stdout) when no issues found.
    2. Cache `(path, blake3(content)) -> issue_set` on disk.  Reuse
       existing `blake3` crate.  Suppress output when issue set is
       unchanged from the previous invocation on same file content.
    3. Cap output to summary line + first 3 issues + `"+N more"`.
  - Gate: hook adds <50ms to file-write operations.  Zero tokens
    emitted on clean files.
  - Scope: `src/main.rs` (`hook install` + `internal hook-callback`
    subcommands).

### 42.6 Ruleset info subcommand
- [ ] Add `zhtw-mcp ruleset info` subcommand:
    - Total rule counts by type (spelling, case, variant, ai_filler).
    - Rules with `context_clues` vs. without (coverage metric).
    - Rules with `english` field vs. without.
    - Override store: count of user overrides and suppressions.
    - Pack store: loaded packs and their rule counts.
  - `--json` flag for programmatic consumption.
  - Scope: `src/main.rs` (~60 LOC), reads from `src/rules/loader.rs`
    and `src/rules/store.rs`.
  - Gate: output matches manual count of `ruleset.json` entries.

### 35.5 Curated domain packs
- [~] `PackStore` infrastructure exists (import/export/validate/list).
      `SpellingRule.tags` field exists for categorization.  Packs are
      activatable via CLI/config/server merge paths.  Remaining: curated
      content and pack-level validation enforcement.
  - [ ] Create starter packs with domain-scoped rules.  Priority order:
    1. `cs_systems` — OS, process, thread, memory, filesystem terms
    2. `networking` — protocol, routing, DNS, socket terms
    3. `compiler` — AST, parser, lexer, codegen terms
    4. `ai_ml` — model, training, inference, dataset terms
    5. `semiconductor` — fab, lithography, CMOS terms
  - Key constraint: packs MUST have `context_clues` on every rule.
  - Data sources: existing tagged ruleset terms, Python `zhtw` domain
    dictionaries (14 JSON files, 3490+ terms), Microsoft Language Portal
    glossaries, MoE dictionary (`externals/moedict-data/`).
  - Terminology precedence: glossary `banned` > TM > glossary `preferred`
    > domain pack > embedded ruleset.
  - Scope: `assets/packs/` directory, validation in `src/rules/store.rs`.
  - Gate: `cs_systems` pack has ≥30 rules, <5% false positive rate.
    Rules without `context_clues` rejected by `pack validate`.

### 35.8 Profile expansion to register system
- [ ] Current profiles (`default`, `strict_moe`, `ui_strings`, `editorial`)
      are too coarse.  Expand to a register system controlling rule
      activation per text genre.
  - Includes renaming `ui_strings` → `relaxed` (absorbed from former 41.1).
    `relaxed` pairs with `strict_moe` and describes the enforcement policy.
  - Profiles: `academic_paper`, `technical_docs`, `classroom_material`,
    `newsroom`, `code_comment`.  Each is a composable `ProfileConfig`
    setting flags (`grammar_checks`, `strict_punctuation`, `variant_check`,
    `spacing_rules`, `allowed_packs`).
  - Scope: `src/config.rs`, `src/mcp/tools.rs`, `src/engine/scan/mod.rs`.
  - Gate: `academic_paper` profile rejects CN terms that `default` allows.

### 39.3 Agent skill packaging for progressive disclosure
- [~] OpenCode skill generator already exists (`src/mcp/setup.rs:opencode_skill`).
      `setup claude-code` emits CLAUDE.md.  Missing: Claude Code SKILL.md
      and Gemini CLI skill generators with progressive disclosure (metadata
      loads first, full instructions activate only when relevant).
  - `zhtw-mcp setup skill-claude` → generates
    `~/.claude/skills/zhtw-mcp/SKILL.md` with YAML frontmatter.
  - `zhtw-mcp setup skill-gemini` → generates
    `.gemini/skills/zhtw-lint.md`.
  - Keep existing `setup claude-code` (no deprecation).
  - Scope: `src/mcp/setup.rs`, `src/main.rs`.
  - Gate: generated SKILL.md loads with <30 tokens idle overhead.

### 36.1 CI/editor integration templates
- [~] Lower adoption barrier with ready-to-use integration configs.
      Pre-commit hook template already documented (`docs/cli.md:110`).
      Remaining:
  - [ ] GitHub Actions workflow with SARIF upload.
  - [ ] reviewdog configuration for PR comment annotations.
  - [ ] VS Code diagnostics bridge via MCP.
  - Scope: `examples/` or `docs/integrations/` directory.
  - Gate: GitHub Actions template runs successfully on a sample repo.

### 42.7 Term import and review pipeline
- [ ] Phase 1: `zhtw-mcp import <file>` — validate and queue external
      terms to `~/.config/zhtw-mcp/pending/`.
  - Phase 2: `zhtw-mcp review [--approve-all|--reject-all]` — interactive
    review.  Approved terms merge into `overrides.json`.
  - Phase 3 (deferred): LLM-assisted validation via Gemini API.
  - Precedence: imported terms become user overrides in `overrides.json`,
    which sit at the same level as the embedded ruleset in the terminology
    stack.  Glossary `banned` and TM still take precedence over imported
    overrides (same as over embedded rules).
  - Scope: `src/main.rs` (subcommands), `src/rules/store.rs` (pending
    storage).
  - Gate: import 50-term JSON → review → approved terms affect scans.

### 42.8 Progress display for directory scanning
- [ ] Add progress reporting to the directory scan loop in `src/main.rs`.
      File discovery starts at `resolve_file_args()` (line 1791) which
      calls `walk_directory()` (line 1825); progress display hooks into
      the per-file lint loop that iterates over the resolved file list.
  - TTY: `\r` overwrite with file count progress.
  - Non-TTY: percentage at 25%/50%/75%/100%.
  - Disabled when `--format json` or `--format sarif`.
  - Detection: `std::io::stderr().is_terminal()`.
  - Scope: `src/main.rs` (~40 LOC).
  - Gate: scanning a 100-file directory shows progress on TTY.

---

## P6: Data-Dependent & Advanced

Items blocked on corpus data collection, or requiring significant
architectural investment for narrow benefit.

### 35.6 Translation as calibration signal — Phase 4
- Status: Phases 1-3 DONE (commit `dba8ad8`).  Phase 4 pending data
  collection.
- [ ] Phase 4 — Evaluate for default enablement:
  - Criteria: ≥10% of issues get `anchor_match` disambiguation,
    ≤500ms overhead, <5% API failure rate over 1000 runs.
  - Run `tests/anchor-benchmark.rs` on real corpora (set `CORPUS_DIR`
    to a directory of `.md`/`.txt` files; 36.0 corpus fixtures provide
    seed text but require extraction to plain files first).
  - If criteria met: make `--verify` default-on, add `--no-verify`.

### 40.11 Phase 4 — Cross-strait terminology drift as scoring input
- [ ] Use zh-CN→zh-TW correction density as a composite score input
      vector in `src/engine/ai_score.rs`.
  - Gate: correction density contributes to composite score; validated
    against corpus with mixed zh-TW/zh-CN text.

### 43.1 Multi-round SC→TC conversion pipeline
- [~] `s2t.rs` already implements STPhrases (AC phrase substitution),
      STCharacters (single-char fallback), and TWVariants (Taiwan-specific
      variant normalization).  Missing: `TWPhrases.txt` TW-specific phrase
      normalization round (~500 entries from opencc-fmmseg).
  - [ ] Add TWPhrases round between STCharacters and TWVariants.
    Reuses the same `CharwiseDoubleArrayAhoCorasick` engine.
  - Note: 36.0 corpus confirmed that `S2TConverter::convert()` handles
    character + phrase + variant conversion; vocabulary normalization
    (cross-strait term replacement) is handled by the scanner as a
    separate pass.  The CN→TW corpus validates both stages independently.
  - Gate: `convert` on TWPhrases-sensitive terms produces correct forms.
    Round-trip stability test on 10KB zh-TW corpus.
  - Scope: `src/engine/s2t.rs`, `src/engine/s2t_data.rs`,
    `scripts/gen-s2t-tables.py`.

### 43.4 False-positive context export for rule refinement
- [ ] `zhtw-mcp tm export-context` dumps rejected matches with ±40-char
      context to TSV for offline analysis.
  - Prerequisite: 37.3 (translation memory, done).
  - Scope: `src/main.rs` (~40 LOC).
  - Gate: export produces valid TSV with >=1 row per rejected match.

---

## Deferred

### 45.3 Exclusion zone scanning optimization
- [ ] Demoted from P4: measured `build_exclusions_plain` = **0.7%** of
      scan CPU (0.11ms / 15.43ms, node1 2026-03-26).  Markdown exclusion
      is 1.29ms (8.4%) but only on markdown content type.  Complexity not
      justified given spelling dominates at 88%.
  - Three separate regex passes (URL, file path, @mention) plus
    pulldown-cmark for Markdown.  Feasible approach: `RegexSet` as
    prefilter to skip documents with zero exclusion-worthy content.
  - Re-evaluate only if exclusion building shows up on flamegraph after
    spelling optimization lands.
  - Scope: `src/engine/excluded.rs`.
  - Gate: identical exclusion zones.  Measurable improvement only on
    documents with many URLs/paths.

### 45.4 Overlap resolution: candidate reduction
- [ ] Current overlap resolution (`src/engine/scan/overlap.rs`) sorts all issues by
      (length DESC, severity DESC, offset ASC), then greedily removes
      overlapping lower-priority issues.  O(n log n) sort + O(n) sweep.
  - Inspired by Pratt/precedence-climbing parsers: the principle of
    resolving conflicts as early as possible to reduce downstream work.
  - Correctness constraint: a simple `last_emitted_end` watermark is
    NOT equivalent to the current resolver.  An early loser can become
    the correct winner after a later higher-priority span knocks out
    the intermediate winner ("ghost suppression").  Non-AC issues
    (punctuation, grammar) are merged before overlap resolution, so
    "inline during AC iteration" cannot produce identical output.
  - Deferred: the current resolver is correct and fast enough.  Better
    to reduce candidate count via existing levers first:
    1. AC absorption patterns (already done) eliminate most superstring
       overlaps before they reach the resolver.
    2. 43.2 positional clue migration reduces false-positive matches,
       shrinking the input set.
    3. Profile-gated rule filtering at AC build time (exclude rules
       disabled by active profile) reduces raw match volume.
  - Re-evaluate only if profiling shows overlap resolution is a
    measurable bottleneck after 43.2 and absorption are fully exploited.
  - If pursued: requires an interval heap/tree proving equivalence to
    the current longest-first resolver, not a simple watermark.
  - Scope: `src/engine/scan/overlap.rs`, `src/engine/scan/spelling.rs`.

### 34.3 POS-based 的/地/得 particle disambiguation
- [ ] Requires POS tagging infrastructure (jieba-rs or lindera).
  - Risk: POS accuracy ~92-95% on zh-TW; high false positives on
    informal text.
  - Deferred: implement only if pattern-based approach (34.0, done) AND
    43.2 (positional conditions) prove insufficient.  43.2 is the
    lightweight middle ground; exhaust it before POS.
  - Gate: POS accuracy ≥93% on curated corpus.  FP rate <3%.

### 31.6 Progress notifications for large text scans
- [ ] Deferred: measured full_default scan = 15.43ms on 100KB (node1,
      2026-03-26).  Even 1MB documents would be ~150ms.  MCP progress
      notifications add architectural complexity for no UX benefit.
  - Re-evaluate if scan latency exceeds 2s or async transport (7.2) lands.

### 7.2 Streamable HTTP transport (MCP 2025-03-26 spec)
- [ ] Deferred: no user demand.  Adds ~5 deps, +2-3 MiB, async redesign.
  - Re-evaluate when concrete remote deployment use case appears.

### 37.5 Interactive term disambiguation via MCP
- [ ] Human-in-the-loop disambiguation via `ambiguous`/`resolve` params.
  - Prerequisite: 37.3 (done), 35.9 (glossary), 35.2 (enriched explain).
  - Deferred: 35.2 explain + 37.3 TM covers 80% of the use case.

### 46.1 Build-time AC serialization (cold-start optimization)
- [ ] Scanner::new() builds the daachorse CharwiseDoubleArrayAhoCorasick
      at process startup.  The MCP server constructs one Scanner and
      reuses it for the process lifetime (`src/main.rs:541`,
      `src/mcp/tools.rs:36`), so this is a cold-start cost, NOT a
      per-request cost.  Matters primarily for CLI invocations where
      each `zhtw-mcp lint` spawns a new process.
  - The double-array packing algorithm (finding collision-free slots in
    BASE/CHECK arrays) is the expensive part; the resulting integer
    arrays are static once built.  Shift this cost to compile time.
  - Phase 1 -- `build.rs` serialization:
    1. Add `build.rs` that loads `assets/ruleset.json`, deduplicates
       patterns, builds the charwise DAAC, and serializes the resulting
       BASE/CHECK/FAIL/output arrays to a binary blob.
    2. Embed the blob via `include_bytes!("../target/ac_spelling.bin")`.
    3. At runtime, deserialize with zero-copy (daachorse's
       `DoubleArrayAhoCorasick::deserialize` or manual reconstruction
       from raw integer slices).
  - Phase 2 -- profile-aware variants:
    If profile-gated rule filtering (strict_moe vs default) produces
    meaningfully different pattern sets, build one serialized AC per
    profile.  Otherwise, a single AC with post-match profile gating
    (current approach) is simpler.
  - Constraint: daachorse does not currently expose a serialize/
    deserialize API.  Options: (a) upstream a PR, (b) fork with serde
    support, (c) serialize the pattern list + rebuild from sorted order
    (deterministic, ~2ms vs ~8ms for unsorted).  Evaluate (c) first as
    the lowest-risk path.
  - Measured (node1, 2026-03-26): `Scanner::new()` = 8.65ms (criterion,
    in-process).  CLI startup (post lazy-S2T fix) = 34ms total.
    Lazy S2TConverter already eliminated ~106ms of the original 140ms
    startup.  Remaining 34ms includes: process load, ruleset JSON parse,
    Scanner::new (8.65ms), cache load, arg parsing.
  - Deferred: 34ms CLI startup is acceptable.  Serializing the DAAC
    would save at most ~6ms (Scanner::new minus segmenter/case AC),
    but the build.rs complexity is high.  Re-evaluate only if startup
    exceeds 100ms again (e.g., ruleset grows significantly).
  - Relationship to 49.2: if 49.2 introduces `build.rs` for ruleset
    struct serialization, 46.1 can extend the same `build.rs` to
    also serialize the DAAC.  Do 49.2 first.
  - Scope: new `build.rs`, `src/engine/scan/mod.rs` (deserialization).
  - Gate: `time zhtw-mcp lint small.md` shows measurable startup
    reduction.  `cargo test` identical.  Binary size increase <500KB.

### 46.2 Split-AC by character class
- [ ] The monolithic spelling AC mixes pure-CJK patterns (~900 rules,
      3-byte UTF-8 chars) with mixed ASCII/CJK patterns (~200 rules).
      A single automaton's state space is the union of both alphabets,
      inflating the double-array and reducing L1 cache hit rate.
  - Inspired by the Split-AC algorithm (FPGA literature): partition rules
    into independent sub-automata that run in parallel over the same
    input.  Each sub-automaton has a smaller alphabet and fewer states,
    fitting more compactly in cache.
  - Proposed partition: (a) pure-CJK rules (all chars >= U+2E80) into a
    charwise DAAC, (b) ASCII-containing rules (case rules, mixed terms)
    into a bytewise AC.  This mirrors the existing charwise/bytewise
    fallback split but makes it intentional rather than error-driven.
  - Caveat: two AC passes over the same input doubles traversal work.
    Net benefit depends on cache pressure being the dominant cost, not
    traversal.  On small documents (<10KB) where the full AC fits in L2
    anyway, splitting may hurt.
  - Deferred: 44.2 perf stat shows L1-dcache miss rate = 2.21% on
    100KB — not a bottleneck.  Split-AC unlikely to help.
  - Scope: `src/engine/scan/mod.rs` (Scanner::new, dual-AC dispatch),
    `src/engine/scan/spelling.rs` (merge results from both ACs).
  - Gate: L1-dcache-load-misses reduction measurable via `perf stat`.
    Identical scan output.  No regression on documents <10KB.

### 46.3 SIMD structural prefilter for exclusion zone detection
- [ ] Markdown exclusion zone detection (code fences, inline code, HTML
      tags) currently uses pulldown-cmark, which parses the full AST.
      For documents where exclusion zones are sparse, a lightweight SIMD
      prefilter can skip pulldown-cmark entirely on clean segments.
  - The Teddy algorithm (Hyperscan lineage, integrated in BurntSushi's
    aho-corasick crate) is optimal for small pattern sets (<100 patterns)
    and scans at 10+ GB/s using PSHUFB vector shuffles.  Markdown
    structural tokens (```, ~~~, `<`, `<!--`, `[`, `![`) are exactly
    the kind of small, fixed set Teddy excels at.
  - Approach: run a Teddy-backed AC over the raw input to locate
    structural token positions.  If none found, bypass pulldown-cmark
    and scan the entire document as plain text.  If found, invoke
    pulldown-cmark only on the relevant regions.
  - Measured (node1, 2026-03-26): `build_exclusions_markdown` = 1.29ms
    on 100KB (8.4% of full_default); `build_exclusions_plain` = 0.11ms
    (0.7%).  SIMD prefilter benefits only the markdown path and only
    if the document has few structural tokens.  Low priority — spelling
    stage (88%) dwarfs this.
  - Deferred: 44.2 data confirms pulldown-cmark cost is marginal
    relative to the spelling hot path.  45.3 (exclusion zone
    optimization) is the broader item; this is a specific SIMD tactic
    within that scope.
  - Scope: `src/engine/markdown.rs` (prefilter gate),
    `src/engine/excluded.rs`.
  - Gate: plain-text-as-markdown scan time reduced.  No regression on
    actual Markdown documents.  Identical exclusion zones.

### 37.6 Full-text translation comparison mode
- [ ] Round-trip translation comparison (zh-TW→EN→zh-TW) to detect
      unnatural phrasing.  Experimental, expensive (1-3s, two API calls).
  - Deferred: re-evaluate after 35.6 Phase 4 completes.

### 48.x AC virtual machine architecture (sub-tasks 2-6 promoted to P1)

Sub-tasks 2-6 (IR refactor) promoted to P1 based on 44.2 profiling data
(2026-03-26): spelling is 88% of scan CPU; clue-heavy text 2.2x slower.
Sub-task 1 absorbed into 44.2; sub-task 7 is 37.1.  Sub-tasks 8-12
(bytecode VM, SIMD) remain Deferred pending post-IR profiling.

Post-match evaluation of spelling rules (context_clues, negative_context_clues,
exceptions, positional clues, MMSEG boundary checks, profile gating, superstring
absorption, deletion span extension) is currently implemented as monomorphized
Rust dispatch in `spelling.rs:process_match_dispatch`.  As rules gain richer
constraints (37.1 sentence boundaries, 34.3 POS tags, 35.9 glossary overrides),
these ad-hoc paths multiply.  A compiled IR → bytecode pipeline centralizes
rule semantics and enables offline optimization without touching the scan engine.

**Abstraction boundary**: the VM covers per-match predicates only.  Overlap
resolution (whole-issue-set pass, `overlap.rs`) and anchor confirmation
(optional post-scan MCP-layer annotation, `translate.rs`/`tools.rs`) are
explicitly excluded — they operate at different granularity and coupling them
into rule bytecode would break the Scanner's synchronous/deterministic contract.

Reviewed by Gemini and Codex (2026-03-26).  Key findings incorporated:
- Original IR enum was incomplete (missing 7+ predicates from the actual
  `process_match_dispatch` path).
- AnchorCheck belongs in MCP layer, not scanner IR.
- Scratch space must be additive (internal optimization underneath existing
  owned-return API), not a replacement.
- Existing panic hazards (`u16::try_from().expect()`, unchecked `usize` math)
  must be hardened before any VM work.
- 37.1 (sentence boundary index) is a missing dependency for boundary opcodes.

#### Sub-task 1: absorbed into 44.2 (done)
Profiling baseline harness merged into 44.2.  Criterion suite,
dhat, flamegraph, and perf stat all delivered.

#### Sub-task 2: Defensive arithmetic hardening
- [ ] Replace panicking assertions and unchecked arithmetic in the
      compile/eval hot path with fallible or saturating operations.
      A bytecode VM will amplify any unsoundness here.
  - `Scanner::new`: `u16::try_from(...).expect("clue index overflow")`
    and hard `<= 32` clue assertions (`mod.rs:587-657`) → return
    `anyhow::Error` on bad rules instead of panicking.
  - `spelling.rs:199-203`: exception span assembly uses unchecked
    `usize` addition → `saturating_add` or `checked_add`.
  - `spelling.rs:252-258`: deletion-span extension → bounds check.
  - `overlap.rs:39-42`: interval construction → bounds check.
  - Scope: `src/engine/scan/mod.rs`, `src/engine/scan/spelling.rs`,
    `src/engine/scan/overlap.rs`.
  - Complexity: S.  Risk: low.
  - Gate: zero ruleset-controlled panics in compile/eval paths;
    fuzzable interval code cannot overflow.
  - Dependencies: none.

#### Sub-task 3: Correct IR abstraction boundary
- [ ] Define `MatchPredicate` enum covering ALL current spelling
      predicates in `process_match_dispatch` (`spelling.rs:128-277`).
      The original 48.1 IR missed 7+ predicates; this sub-task gets
      the type system right before any code moves.
  - Required predicates (mapped from current dispatch branches):
    `RequireProfileFlag(RuleType, Profile)` — variant/ai_filler/
      political gating.
    `RejectIfExcluded(range)` — exclusion zone check.
    `RejectIfSuperstringAbsorbed(absorber_id)` — superstring
      suppression.
    `RejectIfBoundaryStraddles(segmenter)` — MMSEG word-boundary
      check.
    `RequireContextClues(clue_ids, radius)` — positive clue window.
    `RejectNegativeClues(clue_ids, radius)` — negative clue window.
    `RequirePositionalClues(clue_ids, position)` — positional clue
      check.
    `CheckExceptions(exception_phrases)` — compound false-positive
      guard.
    `ExtendDeletionSpan(target_char)` — deletion rule span rewrite.
    `EmitIssue(template)` — report metadata payload construction.
  - Explicitly excluded: overlap resolution, anchor confirmation,
    TM suppression (all post-scan, not per-match).
  - Scope: new `src/engine/scan/rule_ir.rs` (type definitions only,
    no evaluation logic yet).
  - Complexity: S.  Risk: med (getting the types wrong here cascades).
  - Gate: every branch in `process_match_dispatch` maps to a typed
    predicate or emit action.  Code review confirms no branch is
    unrepresented.
  - Dependencies: none.

#### Sub-task 4: Fallible rule compilation
- [ ] Extract clue interning, positional clue parsing, absorber
      construction, and flag/class derivation from `Scanner::new()`
      into a separate `compile_spelling_rules() ->
      Result<CompiledSpellingDb>`.  This is the "compile" half of
      the compiler — turning `SpellingRule` structs into `CompiledRule`
      structs containing `Vec<MatchPredicate>`.
  - Move all `expect()`/`assert!()` in the ruleset-driven compile
    path into `anyhow::Result` returns (builds on sub-task 2).
  - `CompiledSpellingDb` owns: the AC automaton, the compiled
    predicate chains per pattern, the interned clue table, and the
    absorber index.
  - Scope: `src/engine/scan/mod.rs` (extract from `Scanner::new`),
    `src/engine/scan/rule_ir.rs` (`CompiledRule`, `CompiledSpellingDb`).
  - Complexity: M.  Risk: med.
  - Gate: `Scanner::new()` delegates to `compile_spelling_rules()`;
    no panics remain in ruleset-driven compile paths; `cargo test`
    passes unchanged.
  - Dependencies: 2, 3.

#### Sub-task 5: IR-based evaluation path
- [ ] Replace direct `SpellingRule` field reads in
      `process_match_dispatch` with `eval_compiled_rule(rule: &CompiledRule,
      ctx: &MatchContext) -> Option<Issue>`.  The AC traversal is
      unchanged; only the post-match decision logic moves to IR
      evaluation.
  - `MatchContext`: borrows text, exclusion ranges, segmenter,
    current offset, config/profile — everything the predicates need.
  - The evaluator loops over `rule.predicates: &[MatchPredicate]`,
    short-circuiting on first rejection.  This is plain Rust, not
    bytecode — the interpreter is a `for` loop over an enum slice.
  - Keep the old monomorphized dispatch behind a `cfg(test)` flag
    temporarily for differential testing (sub-task 6).
  - Scope: `src/engine/scan/spelling.rs` (rewrite
    `process_match_dispatch`), `src/engine/scan/rule_ir.rs`
    (`eval_compiled_rule`, `MatchContext`).
  - Complexity: L.  Risk: high (most regression-prone step).
  - Gate: full test suite passes with byte-identical scanner output
    on all embedded rules.
  - Dependencies: 4.

#### Sub-task 6: Differential parity tests
- [ ] Before deleting the old dispatch path, add differential tests
      that run both evaluators on all 1100 rules + representative
      fixtures and compare offsets, lengths, severities, suggestions,
      and metadata fields.
  - Test helper: `assert_scan_identical(text, old_scanner, new_scanner)`
    comparing `ScanOutput` field by field.
  - Coverage must include: context clues, negative clues, positional
    clues, exceptions, deletion rules, variant gating, political
    stance gating, ai_filler profile gate, superstring absorption.
  - Once parity is confirmed, delete the old dispatch path and the
    `cfg(test)` gate.
  - Scope: `tests/` (new parity test file),
    `src/engine/scan/spelling.rs` (remove old path after parity).
  - Complexity: M.  Risk: low.
  - Gate: differential tests cover all predicate types listed in
    sub-task 3.  Zero output differences on the full test suite.
  - Dependencies: 5.

#### Sub-task 7: absorbed into 37.1 (P3)
Sentence/paragraph boundary index is 37.1.  Listed here only as a
dependency note: sub-tasks 9+ need it for `OP_ASSERT_BOUND` opcodes.

#### Sub-task 8: Internal scratch space ✅
Completed 2026-03-27.  See CHANGELOG.md.

#### Sub-tasks 9-12: Closed (superseded by direct IR optimization)

Sub-tasks 9 (bytecode lowering), 10 (VM interpreter), 11 (constraint
optimizer), and 12 (SIMD/Teddy) are closed.  They targeted post-match
predicate dispatch overhead which was 88% of scan time when the items
were written (2026-03-26).  After the optimization session (2026-03-27),
the bottleneck shifted fundamentally:

- Predicate dispatch is now <5% of spelling_only (CLASS_TRULY_SIMPLE
  inlined directly in AC loop, eval_simple #[inline] for CLASS_SIMPLE).
- Predicate reordering (sub-task 11) was implemented directly in
  `compile_rule_predicates()` without bytecode.
- The `aho-corasick` crate already auto-selects Teddy/SIMD for the
  grammar prefilter (35 patterns) and case AC (15 patterns).

The remaining 9.7ms in spelling_only (vs 0.175ms AC floor) is:
  bitmap construction ~1.5ms, per-hit eval ~1.5ms, sort+overlap ~1ms,
  inflation ~1ms, line/col+clue+detect ~1.3ms, misc ~3.2ms.
None of these are dispatch-bound.

See 50.x (promoted to P1) for the next generation of performance items.

---

## Epic Provenance

| Epic | Origin | Status |
|------|--------|--------|
| 25.x | MCP protocol compliance | 25.3-25.4 done; 25.5 open (P3); 25.7 absorbed into 44.1 Phase 2 |
| 36.x | Evaluation & CI | 36.0 done; 36.1 partial (pre-commit only); 36.2 partial (flags only); 36.3 open |
| 37.x | `externals/betterTranslation` | 37.3 done; 37.1, 37.2, 37.4-37.6 open |
| 39.x | Operational hardening | 39.1-39.2 done; 39.3 partial |
| 40.x | AI writing detection | 40.1-40.10 done; 40.11 Phases 1-3 done, Phase 4 open |
| 42.x | Python `zhtw` parity | 42.1 removed (redundant w/ 19.5); 42.9 partial (// prefix done, # prefix + short aliases open); rest open |
| 43.x | `Chinese_converter` + `opencc-fmmseg` | 43.1 partial, 43.2 done; 43.3/43.5/43.6/43.7 closed; 43.4 open; 43.8 done |
| 33.x | Output & hooks | 33.1 absorbed into 47.4/47.5; 33.2 open (P4); 33.3 open (P5) |
| 35.x | Explain / consistency / packs | 35.1 open; 35.2 partial (anchor provenance done, 5 structured fields pending); 35.5 partial (PackStore infra done, zero packs); 35.6 Phases 1-3 done, Phase 4 open; 35.7 partial (exclusion zones only); 35.8-35.9 open |
| 41.x | Profile naming | 41.1 absorbed into 35.8 |
| 44.x | Runtime instrumentation | 44.1 open (P1); 44.2 near-complete (P1: criterion + flamegraph + perf stat + dhat done; Tracy optional) |
| 45.x | VM/bytecode-inspired scan optimization | 45.2 done (steps 1+2, step 3 deferred); 45.1 done (-38% grammar); 45.3 demoted to Deferred (0.7% CPU); 45.4 deferred. 48.x extends VM vision |
| 46.x | AC automaton acceleration (DAAC/SIMD/Split) | 46.1 deferred (DAAC traversal = 0.175ms on 100KB, AC floor now known via ac_traversal_only benchmark); 46.2-46.3 deferred (L1 miss 2.21%, Split-AC unlikely to help) |
| 47.x | Code quality & structural cleanup | 47.1 done (PR #54); 47.2 done; 47.3-47.7 open (P3) |
| 48.x | AC virtual machine architecture | Sub-tasks 2-6 done (IR refactor, -29% spelling); sub-task 8 done (scratch space); sub-task 1 absorbed into 44.2; sub-task 7 is 37.1; sub-tasks 9-12 closed (superseded by direct IR optimization, dispatch now <5% of cost). Reviewed by Gemini+Codex 2026-03-27 |
| 50.x | Post-IR performance | New. 50.1 trie dict, 50.2 profile-aware AC, 50.3 sort-free overlap, 50.4 eval consolidation, 50.5 inflation reduction, 50.6 fused post-scan passes. 2026-03-28 |
| 49.x | Allocation reduction (dhat-driven) | 49.1 done (typed output), 49.2 done (postcard ruleset), 49.3 done (lazy cache). All P1. Suggested by Gemini+Codex 2026-03-26 |

Closed items: 43.3 (synonym acceptance: masks bad rules), 43.5/43.7
(won't-fix: wrong bottleneck per 30.7 profiling), 43.6 (JSON parse <1ms).

MoE standards reference tables in `docs/moe-standards.md`.

45.x references: Cox regex VM (swtch.com/~rsc/regexp/regexp2.html),
Xilinx regex-VM (Vitis_Libraries), MoonBit regex pearls, Odin VM,
Pratt/precedence-climbing (engr.mun.ca/~theo/Misc/exp_parsing.htm).

46.x references: Aho & Corasick (1975, Bell Labs), Aoe (1989, double-array trie),
Kanda "Engineering faster double-array AC" (daachorse paper),
Hyperscan Teddy/FDR (Intel, SIMD prefilters), Lin et al. PFAC (GPU failure-less AC),
Split-AC (FPGA bit-split partitioning), BurntSushi aho-corasick crate (Teddy integration).

48.x references: Intel Hyperscan regex decomposition (intel/hyperscan, "Building a
High Performance Regex Engine"), Hyperscan pattern DB/scratch separation API
(immutable compiled DB + per-thread scratch space), Thompson NFA VM (Pike VM:
split/match/jmp ISA), GoAWK tree-walking→bytecode migration (benhoyt.com/writings/
goawk-compiler-vm), YARA AC optimization (27% scan speed increase via fused AC+regex),
Snort/Suricata two-tier architecture (AC prefilter → rule VM), Shift-OR bit-parallelism
(Baeza-Yates & Gonnet 1992), Direct Filter Classification with SIMD gather (1.8-3.6x
on Haswell/Xeon-Phi), Teddy algorithm (Hyperscan lineage, SSSE3 PSHUFB multi-string
matching, up to 35x over naive AC on small pattern sets).  Codex review identified
concrete overflow hazards at spelling.rs:199-203, spelling.rs:252-258, overlap.rs:39-42
and panicking compile paths at mod.rs:587-657 that must be hardened pre-VM (sub-task 2).
