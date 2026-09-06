// The lint batch: scanning many files, applying fixes, and folding the per-file
// results into one verdict.
//
// Phase 1 (ScanCtx) reads and scans, optionally across rayon threads. Phase 2
// (process_scanned_file) is always sequential, because the output has to come
// out in the order the user named the files.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::discover::{resolve_diff_files, resolve_file_args};
use crate::cli::render::{
    self, collect_sarif, render_compact, render_human, render_json, render_tabular, CliFileOutput,
    FileReport, LintFormat, RenderOpts, SarifResult, SarifRuleDef,
};
use crate::cli::render::{use_color, Colors, COLORS_OFF, COLORS_ON};
use crate::{EXIT_FAILURE, EXIT_GATE};

pub(crate) struct LintBatchParams<'a> {
    pub(crate) file_args: &'a [String],
    pub(crate) format: LintFormat,
    pub(crate) max_errors: usize,
    pub(crate) max_warnings: Option<usize>,
    pub(crate) profile_name: Option<&'a str>,
    pub(crate) content_type_override: Option<&'a str>,
    pub(crate) overrides_path: &'a Path,
    pub(crate) packs_dir: &'a Path,
    pub(crate) active_packs: &'a [String],
    pub(crate) exclude_patterns: &'a [String],
    pub(crate) fix_mode: zhtw_mcp::fixer::FixMode,
    pub(crate) dry_run: bool,
    pub(crate) explain: bool,
    pub(crate) baseline_path: Option<&'a Path>,
    pub(crate) update_baseline: bool,
    pub(crate) diff_from: Option<&'a str>,
    #[cfg(feature = "translate")]
    pub(crate) verify: bool,
    pub(crate) relaxed: bool,
    pub(crate) exempt_blockquotes: bool,
    pub(crate) detect_ai: bool,
    pub(crate) detect_translationese: bool,
    /// Advisory rhythm (氣口) axis. Never fixable at any tier: the findings
    /// carry no suggestion, so every tier declines them for want of one.
    pub(crate) rhythm: bool,
    /// Emit composite three-axis style scorecard alongside the per-axis
    /// ai_signature / translationese_signature reports.  Set only by
    /// `--detect-style` (which also flips detect_ai +
    /// detect_translationese).
    pub(crate) detect_style: bool,
    pub(crate) translationese_domain: zhtw_mcp::engine::translationese_score::TranslationeseDomain,
    pub(crate) document_genre: zhtw_mcp::rules::ruleset::DocumentGenre,
    pub(crate) ai_threshold_multiplier: f32,
    pub(crate) tm_path: Option<PathBuf>,
    /// Project glossary (`[glossary]` section in `.zhtw-mcp.toml`).
    /// Applied as a post-scan step: `proper_nouns` suppress matching
    /// issues, `banned` injects synthetic Error issues for any
    /// occurrence the embedded ruleset missed.
    pub(crate) glossary: zhtw_mcp::rules::glossary::ProjectGlossary,
    /// Terms the project has declared uninteresting (`ignore_terms` in
    /// `.zhtw-mcp.toml`).  Still reported, but downgraded to Info so they
    /// stop failing the error and warning gates.  Same semantics as the
    /// MCP tool's `ignore_terms` argument.
    pub(crate) ignore_terms: &'a [String],
    /// When true, append a `consistency` block to JSON output:
    /// per-equivalence-class diagnostic when both the calque and the
    /// canonical TW form appear in the same document.
    pub(crate) consistency: bool,
    pub(crate) telemetry: bool,
}

/// Everything a lint batch builds once and then reuses for every file.
struct LintSetup {
    cfg: zhtw_mcp::rules::ruleset::ProfileConfig,
    scanner: zhtw_mcp::engine::scan::Scanner,
    /// Built on first Simplified input rather than during setup.  Its
    /// Aho-Corasick over ST_PHRASES dominated startup, and a zh-TW linter
    /// reading zh-TW never needs it: building it eagerly cost 183 ms per
    /// invocation against 44 ms lazily, measured interleaved on a one-line
    /// file with an empty cache.  OnceLock rather than a plain field so the
    /// rayon batch path can share one converter across threads.
    ///
    /// Built first thing in a fresh process the same call measures ~90 ms, but
    /// built here, after the ruleset and scanner have already warmed the
    /// allocator, Simplified input is not measurably slower than Traditional.
    /// Quote the end-to-end numbers rather than that isolated one.
    s2t: std::sync::OnceLock<zhtw_mcp::engine::s2t::S2TConverter>,
    /// Cache key template.  Every field is batch-wide except `content_type`,
    /// which is left empty here and filled per file by `ScanCtx::cache_params`.
    /// Building it once means the per-file path clones strings instead of
    /// re-running six `format!`s that cannot produce a different answer.
    cache_params: zhtw_mcp::cache::ScanParams,
    tm_store: Option<zhtw_mcp::rules::store::TranslationMemoryStore>,
    scan_cache: Option<std::sync::Mutex<zhtw_mcp::cache::ScanCache>>,
}

/// Convert `text` when it reads as Simplified, building the converter on the
/// first such file.  Returns whether a conversion happened, which the caller
/// reports as the `+s2t` label.
fn s2t_convert_if_simplified(
    s2t: &std::sync::OnceLock<zhtw_mcp::engine::s2t::S2TConverter>,
    text: &mut String,
) -> bool {
    use zhtw_mcp::engine::s2t::S2TConverter;
    use zhtw_mcp::engine::zhtype::{detect_chinese_type, ChineseType};

    if detect_chinese_type(text) != ChineseType::Simplified {
        return false;
    }
    *text = s2t.get_or_init(S2TConverter::new).convert(text);
    true
}

/// Resolve flags and config into the scanner, stores, and cache the batch
/// runs against.  Split out of `run_lint_batch` because none of it depends
/// on the files being linted: it is pure setup, and reviewing it does not
/// require holding the per-file loop in your head.
fn build_lint_setup(
    params: &LintBatchParams<'_>,
    profile: zhtw_mcp::rules::ruleset::Profile,
) -> Result<LintSetup> {
    // Effective config: profile base plus capability flags.
    let mut cfg = profile.config();
    if params.relaxed {
        cfg = cfg.with_relaxed();
    }
    if params.exempt_blockquotes {
        cfg = cfg.with_exempt_blockquotes(true);
    }
    if params.detect_ai {
        cfg.ai_filler_detection = true;
        cfg.ai_semantic_safety = true;
        cfg.ai_density_detection = true;
        cfg.ai_structural_patterns = true;
        cfg.ai_threshold_multiplier = params.ai_threshold_multiplier;
    }
    if params.detect_translationese {
        cfg.translationese_detection = true;
    }
    if params.rhythm {
        cfg = cfg.with_rhythm(true);
    }
    cfg.translationese_domain = params.translationese_domain;
    cfg.document_genre = params.document_genre;

    // Build scanner once for all files, merging overrides + active packs.
    let ruleset = zhtw_mcp::rules::loader::load_embedded_ruleset()?;
    let store = zhtw_mcp::rules::store::OverrideStore::open(params.overrides_path)?;
    let pack_store = zhtw_mcp::rules::store::PackStore::new(params.packs_dir.to_path_buf());

    let (spelling_rules, case_rules) = zhtw_mcp::rules::store::build_merged_rules(
        &ruleset.spelling_rules,
        &ruleset.case_rules,
        &store,
        &pack_store,
        params.active_packs,
    );
    let ruleset_hash = zhtw_mcp::rules::loader::compute_ruleset_hash(&spelling_rules, &case_rules);
    let filter = zhtw_mcp::engine::scan::ProfileFilter::from_config(&cfg);
    let scanner =
        zhtw_mcp::engine::scan::Scanner::new_filtered(spelling_rules, case_rules, &filter);

    // Open translation memory (if path provided and file exists/creatable).
    let tm_store = params.tm_path.as_ref().and_then(|p| {
        zhtw_mcp::rules::store::TranslationMemoryStore::open(p)
            .map_err(|e| tracing::warn!("failed to open TM at {}: {e}", p.display()))
            .ok()
    });

    // Scan cache: skip re-scanning unchanged files (lint-only, no fix).
    // Disabled when --verify is active (calibrate_issues needs the full text).
    // Wrapped in Mutex for rayon parallel scanning.
    let use_cache = params.fix_mode == zhtw_mcp::fixer::FixMode::None && {
        #[cfg(feature = "translate")]
        {
            !params.verify
        }
        #[cfg(not(feature = "translate"))]
        {
            true
        }
    };
    let scan_cache =
        use_cache.then(|| std::sync::Mutex::new(zhtw_mcp::cache::ScanCache::open_default()));

    let cache_params = zhtw_mcp::cache::ScanParams {
        ruleset_hash,

        // The whole effective config, not profile.name(). The name is the
        // profile the user asked for; the scanner is built from this struct,
        // and flags such as --relaxed change it without changing the name.
        // Keying on the name let a --relaxed run answer for a strict one and
        // vice versa, so a strict gate could report clean. Debug covers every
        // field, so a new one cannot be forgotten here.
        profile: format!("{cfg:?}"),
        content_type: String::new(),
        fix_mode: format!("{:?}", params.fix_mode),
        detect_ai: params.detect_ai,
        detect_translationese: cfg.translationese_detection,
        translationese_domain: cfg.translationese_domain.name().to_owned(),
        ai_threshold: format!("{:.1}", params.ai_threshold_multiplier),
        exempt_blockquotes: cfg.exempt_blockquotes,
        engine_version: format!(
            "{}+{}",
            env!("CARGO_PKG_VERSION"),
            env!("ZHTW_ENGINE_FINGERPRINT")
        ),
    };

    Ok(LintSetup {
        cache_params,
        cfg,
        scanner,
        s2t: std::sync::OnceLock::new(),
        tm_store,
        scan_cache,
    })
}

/// Running counts across every file in one lint batch.  Grouped because
/// the six of them are always read and written together; six loose
/// counters in a 700-line function is how one of them gets missed.
#[derive(Default)]
struct LintTotals {
    errors: usize,
    warnings: usize,
    deterministic: usize,
    heuristic: usize,
    llm_judged: usize,
    unresolved: usize,
}

impl LintTotals {
    fn report_telemetry(&self, file_count: usize) {
        eprintln!(
            "[telemetry] files={} total_issues={} errors={} warnings={}",
            file_count,
            self.errors + self.warnings,
            self.errors,
            self.warnings,
        );
        eprintln!(
            "[telemetry] resolution: deterministic={} heuristic={} llm_judged={} unresolved={}",
            self.deterministic, self.heuristic, self.llm_judged, self.unresolved,
        );
    }
}

/// How to parse a file: what `--content-type` says, or what its name implies.
///
/// An unrecognized `--content-type` falls back to the name rather than being
/// rejected, which is the behavior the flag has always had.
pub(crate) fn content_type_for(
    override_name: Option<&str>,
    file_arg: &str,
) -> zhtw_mcp::engine::scan::ContentType {
    use zhtw_mcp::engine::scan::ContentType;
    override_name
        .and_then(ContentType::from_name)
        .unwrap_or_else(|| ContentType::from_file_name(file_arg))
}

pub(crate) fn run_lint_batch(params: &LintBatchParams<'_>) -> Result<()> {
    let c = if use_color() { &COLORS_ON } else { &COLORS_OFF };

    // Before anything is read, scanned or rewritten. Checking this per file
    // meant "--fix --verify" had already written the file back to disk by the
    // time it refused, and then refused once per file with no report for any of
    // them. The refusal is about the whole invocation, so it belongs here.
    #[cfg(feature = "translate")]
    if params.verify {
        zhtw_mcp::engine::translate::refuse_if_network_disabled("--verify")
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    let profile = match params.profile_name {
        None => zhtw_mcp::rules::ruleset::Profile::Base,
        Some(s) => zhtw_mcp::rules::ruleset::Profile::from_str_strict(s)
            .ok_or_else(|| anyhow::anyhow!("unknown profile: {s} (expected 'base' or 'strict')"))?,
    };

    let setup = build_lint_setup(params, profile)?;

    // Only the cache is reached directly from here; the rest of the setup is
    // read through ScanCtx and FileCtx, which borrow it whole.
    let scan_cache = &setup.scan_cache;

    // --diff-from: resolve changed files via git, use as file args.
    let diff_files: Vec<String>;
    let file_args = if let Some(git_ref) = params.diff_from {
        diff_files = resolve_diff_files(git_ref)?;
        if diff_files.is_empty() {
            return Ok(());
        }
        &diff_files
    } else {
        params.file_args
    };

    // Resolve directories into individual files; de-duplicate and sort.
    let resolved = resolve_file_args(file_args, params.exclude_patterns)?;
    let multi = resolved.len() > 1;
    let mut state = BatchState {
        // Load baseline if provided.
        baseline: params
            .baseline_path
            .map(zhtw_mcp::baseline::Baseline::load)
            .transpose()?
            .unwrap_or_default(),
        ..Default::default()
    };

    // Phase 1: Read + S2T + cache check + scan.
    let scan_ctx = ScanCtx {
        params,
        setup: &setup,
    };

    // Parallel scan when multiple files and no stdin pipe. Rayon parallelism
    // gives N/cores speedup on multi-file lint.
    let has_stdin = resolved.iter().any(|f| f == "--");
    let scan_results: Vec<ScanResult> = if resolved.len() > 1 && !has_stdin {
        use rayon::prelude::*;
        resolved.par_iter().map(|f| scan_ctx.scan_one(f)).collect()
    } else {
        resolved.iter().map(|f| scan_ctx.scan_one(f)).collect()
    };

    // Phase 2: Fix + report (always sequential for ordered output).
    let ctx = FileCtx {
        params,
        colors: c,
        setup: &setup,
        profile,
        multi,
    };
    for (file_arg, scan_result) in resolved.iter().zip(scan_results) {
        // A file that cannot be read is reported and skipped, not fatal. One
        // latin-1 document in a directory used to abort the whole run and
        // discard the findings for every file already processed, which in JSON
        // mode meant empty output after doing all the work.
        if let Err(e) = process_scanned_file(&ctx, file_arg, scan_result, &mut state) {
            // No file prefix: every error on this path already carries the path
            // in its context, and prefixing printed it twice. Alternate Display
            // keeps one file to one line, which is what the rest of the
            // per-file output does.
            eprintln!("{}{:#}{}", c.bold, e, c.reset);
            state.failed_files += 1;
        }
    }

    // Multi-file JSON: emit array of per-file results.
    if multi && matches!(params.format, LintFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&state.file_results)?);
    }

    // --update-baseline: save the baseline file.
    if params.update_baseline {
        let bl_path = params
            .baseline_path
            .context("--update-baseline requires --baseline <file>")?;
        state.baseline.save(bl_path)?;
        eprintln!(
            "{}Baseline updated:{} {} fingerprint(s) in {}",
            c.dim,
            c.reset,
            state.baseline.len(),
            bl_path.display()
        );
    }

    // Report baseline summary if filtering was active.
    if params.baseline_path.is_some() && !params.update_baseline && state.baseline_count > 0 {
        eprintln!(
            "{}{} baseline issue(s) suppressed.{}",
            c.dim, state.baseline_count, c.reset
        );
    }

    // SARIF: emit the complete SARIF v2.1.0 document.
    if matches!(params.format, LintFormat::Sarif) {
        render::print_sarif(state.sarif_rules, &state.sarif_results)?;
    }

    // Flush scan cache before potential process::exit (which skips Drop).
    if let Some(ref cache_mtx) = scan_cache {
        if let Ok(mut c) = cache_mtx.lock() {
            c.flush();
        }
    }

    // Print telemetry summary to stderr when --telemetry is set.
    if params.telemetry {
        state.totals.report_telemetry(resolved.len());
    }

    // Exit codes are a contract with CI (see docs/cli.md): 1 means the text
    // failed a gate, 2 means the tool could not do its job. A skipped file
    // outranks a gate result, because the gate was computed over an incomplete
    // set and a clean verdict would be a lie.
    if state.failed_files > 0 {
        eprintln!(
            "{}{} file(s) could not be processed{}",
            c.dim, state.failed_files, c.reset
        );
        process::exit(EXIT_FAILURE);
    }

    let errors_exceeded = state.totals.errors > params.max_errors;
    let warnings_exceeded = params
        .max_warnings
        .is_some_and(|limit| state.totals.warnings > limit);
    if errors_exceeded || warnings_exceeded {
        process::exit(EXIT_GATE);
    }

    Ok(())
}

/// One file after phase 1: (raw text, was-SC input, char count, scan
/// output, content type).  Aliased so the tuple slot ordering has a
/// single source of truth.
type ScanResult = Result<(
    String,
    bool,
    usize,
    zhtw_mcp::engine::scan::ScanOutput,
    zhtw_mcp::engine::scan::ContentType,
)>;

/// Maximum file size for CLI lint mode (16 MiB).
const MAX_CLI_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Phase 1 context: what scanning one file needs, and nothing else.
///
/// A named struct rather than a closure over the whole batch scope. Rayon
/// needs this shared across threads, and spelling the fields out keeps the
/// parallel and sequential paths reading the same set instead of whatever a
/// closure happened to capture.
struct ScanCtx<'a> {
    params: &'a LintBatchParams<'a>,
    setup: &'a LintSetup,
}

impl ScanCtx<'_> {
    /// The cache key for one file under this batch's settings.
    fn cache_params(
        &self,
        content_type: zhtw_mcp::engine::scan::ContentType,
    ) -> zhtw_mcp::cache::ScanParams {
        zhtw_mcp::cache::ScanParams {
            content_type: format!("{content_type:?}"),
            ..self.setup.cache_params.clone()
        }
    }

    /// Whether any phase after the scan still reads the original buffer.
    ///
    /// Glossary banned-term injection and the consistency report both scan it,
    /// as do fix and verify, so the cache fast path can only skip the read
    /// when none of them is active.
    fn needs_text_after_scan(&self) -> bool {
        self.params.fix_mode != zhtw_mcp::fixer::FixMode::None
            || !self.params.glossary.is_empty()
            || self.params.consistency
            || {
                #[cfg(feature = "translate")]
                {
                    self.params.verify
                }
                #[cfg(not(feature = "translate"))]
                {
                    false
                }
            }
    }

    fn scan_one(&self, file_arg: &str) -> ScanResult {
        let content_type = content_type_for(self.params.content_type_override, file_arg);
        if file_arg == "--" {
            self.scan_stdin(content_type)
        } else {
            self.scan_path(file_arg, content_type)
        }
    }

    fn scan_path(
        &self,
        file_arg: &str,
        content_type: zhtw_mcp::engine::scan::ContentType,
    ) -> ScanResult {
        let cache_params = self.cache_params(content_type);
        let scan_cache = &self.setup.scan_cache;

        // Open via fd and stat from that same fd, so the bytes measured are the
        // bytes read. Cache is consulted before the read: a hit skips file I/O
        // entirely.
        let file =
            std::fs::File::open(file_arg).with_context(|| format!("open file: {file_arg}"))?;
        let meta = file
            .metadata()
            .with_context(|| format!("stat file: {file_arg}"))?;
        anyhow::ensure!(
            meta.len() <= MAX_CLI_FILE_BYTES,
            "{file_arg}: file too large ({} bytes, limit {MAX_CLI_FILE_BYTES})",
            meta.len()
        );

        // Fast-path: check mtime+size before reading the file.
        let fast_hit = scan_cache.as_ref().and_then(|mtx| {
            let mut c = mtx.lock().ok()?;
            let mtime = zhtw_mcp::cache::mtime_secs(&meta);
            c.check_fast(file_arg, mtime, meta.len(), &cache_params)
                .into_hit()
        });

        let need_text_post_scan = self.needs_text_after_scan();
        if let Some(hit) = fast_hit {
            if !hit.input_was_sc && !need_text_post_scan && hit.output.issues.is_empty() {
                // Cache hit AND no later phase needs the text: skip file read
                // and scan. Tier 2 needs the text for every scanned issue.
                return Ok((
                    String::new(),
                    false,
                    hit.text_char_count,
                    hit.output,
                    content_type,
                ));
            }

            // SC files need the text for S2T write-back; glossary / consistency
            // / fix / verify need the original buffer. Fall through to the slow
            // path so we read the file and reuse the cached scan output below.
        }

        // Slow path: read file from the same fd.
        let mut text = String::with_capacity(meta.len() as usize);
        std::io::BufReader::new(file)
            .read_to_string(&mut text)
            .with_context(|| format!("read file: {file_arg}"))?;

        let input_was_sc = s2t_convert_if_simplified(&self.setup.s2t, &mut text);
        let text_char_count = text.chars().count();

        // Slow-path cache: check content hash (mtime missed but content may be
        // unchanged, e.g. after touch).
        let content_hit = scan_cache.as_ref().and_then(|mtx| {
            let mut c = mtx.lock().ok()?;
            c.check_content(file_arg, text.as_bytes(), &cache_params)
        });
        let output = match content_hit {
            Some(hit) => hit.output,
            None => {
                let o = self.setup.scanner.scan_for_content_type_with_config(
                    &text,
                    content_type,
                    self.setup.cfg,
                );
                if let Some(Ok(mut c)) = scan_cache.as_ref().map(|mtx| mtx.lock()) {
                    let mtime = zhtw_mcp::cache::mtime_secs(&meta);
                    c.put(
                        file_arg,
                        text.as_bytes(),
                        mtime,
                        meta.len(),
                        &cache_params,
                        o.clone(),
                        input_was_sc,
                        text_char_count,
                    );
                }
                o
            }
        };

        // The buffer used to be dropped here when no later phase was thought to
        // need it, to keep a parallel scan from holding every file's text at
        // once. Tier 2 was not counted among those phases, and it reads the
        // document: disambiguate_batch weighs the words around each issue to
        // decide whether a clue-gated term really is the technical sense. With
        // an empty buffer it finds nothing and every such issue keeps its raw
        // severity, so "學習的進程" stayed a warning against the gate instead
        // of being downgraded to info. Phase 2 always disambiguates, so the
        // text is always needed.
        Ok((text, input_was_sc, text_char_count, output, content_type))
    }

    fn scan_stdin(&self, content_type: zhtw_mcp::engine::scan::ContentType) -> ScanResult {
        let mut text = String::new();
        std::io::stdin()
            .take(MAX_CLI_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .context("read stdin")?;
        anyhow::ensure!(
            text.len() as u64 <= MAX_CLI_FILE_BYTES,
            "stdin input exceeds {MAX_CLI_FILE_BYTES} byte limit"
        );

        let input_was_sc = s2t_convert_if_simplified(&self.setup.s2t, &mut text);
        let text_char_count = text.chars().count();
        let output = self.setup.scanner.scan_for_content_type_with_config(
            &text,
            content_type,
            self.setup.cfg,
        );

        Ok((text, input_was_sc, text_char_count, output, content_type))
    }
}

/// Immutable context shared by every file in a lint batch.
struct FileCtx<'a> {
    params: &'a LintBatchParams<'a>,
    colors: &'a Colors,
    setup: &'a LintSetup,
    profile: zhtw_mcp::rules::ruleset::Profile,
    multi: bool,
}

/// Everything the per-file pass accumulates and the batch drains after
/// the loop.  These move together, so they live together.
#[derive(Default)]
struct BatchState {
    totals: LintTotals,
    file_results: Vec<CliFileOutput>,
    sarif_results: Vec<SarifResult>,
    sarif_rules: std::collections::BTreeMap<String, SarifRuleDef>,
    baseline: zhtw_mcp::baseline::Baseline,
    baseline_count: usize,
    tabular_header_printed: bool,
    /// Files that could not be processed at all: unreadable, oversized, or not
    /// UTF-8.  Counted rather than propagated, so one bad file in a directory
    /// does not throw away the findings for every other file.
    failed_files: usize,
}

/// What `emit_fix_result` left behind for the phases that follow it.
struct FixEmission<'a> {
    /// The buffer every later phase reads: the fixer's output when it ran, the
    /// original otherwise.
    text: &'a str,
    /// True when the document on disk (or on stdout) now differs from the
    /// input, so reporting has to run against the rewritten text.
    wrote_changes: bool,
}

/// Write the fixed document and report what happened to it.
///
/// Split out of `process_scanned_file` because the two halves are easy to get
/// out of step: stdout carries the document for a stdin filter and the report
/// for every machine format, while status lines always belong on stderr.
/// Keeping the decision in one place means there is one answer to "where does
/// this text go", not one per branch.
fn emit_fix_result<'a>(
    file_arg: &str,
    text: &'a str,
    fix_result: Option<&'a zhtw_mcp::fixer::FixResult>,
    input_was_sc: bool,
    params: &LintBatchParams<'_>,
    c: &Colors,
) -> Result<FixEmission<'a>> {
    // Write fixed text (unless --dry-run). Text is written when either S2T
    // conversion was applied or ruleset fixes were made.
    let fix_applied = fix_result.map_or(0, |f| f.applied);
    let fix_declined = fix_result.map_or(0, |f| f.declined);
    let has_text_changes = input_was_sc || fix_applied > 0;

    // Declined fixes are the only signal that --fix looked at an issue and
    // chose not to rewrite it. Without this the count reaches JSON consumers
    // only, and a file where every issue is declined prints no fix line at all.
    //
    // Reports f.declined, not f.skipped: the latter also counts issues that
    // were never in scope, so "--fix=orthographic" on ordinary prose would
    // report every cross-strait term as declined. Deliberately does not name a
    // tier that would apply them either: declines come from several gates
    // (multiple suggestions, anchor rejection, tier-2 suppression, editorial
    // confidence) and only some are tier-liftable.
    let declined = if fix_declined > 0 {
        format!(", {}{fix_declined} declined{}", c.dim, c.reset)
    } else {
        String::new()
    };

    // The buffer every later phase reads: the fixer's output when it ran, the
    // original otherwise.
    let current_text = fix_result.map_or(text, |f| f.text.as_str());

    // stdin with --fix is a filter, so stdout carries the document whether or
    // not anything changed. Gating this on has_text_changes made "lint -- --fix
    // > out.md" emit nothing for a clean document, which truncates the user's
    // content on the one input that has no copy on disk to fall back to. A dry
    // run still emits nothing: it reports what would happen and leaves the text
    // alone.
    //
    // S2T conversion counts too, with or without --fix. It rewrites the
    // document exactly as a fix does, and the file branch below writes it back
    // unconditionally, so withholding it on stdin loses the converted text with
    // nothing on disk to recover it.
    //
    // Human format only, because stdout can carry one product. Every other
    // format puts its report there, and a document printed ahead of it made
    // "--fix --format json" unparseable. That combination asks for the report,
    // so the report is what stdout gets; the fixed text is reachable by
    // rerunning without --format, or by fixing a file instead.
    let stdin_emits_document =
        file_arg == "--" && (fix_result.is_some() || input_was_sc) && !params.dry_run;
    if stdin_emits_document && !params.format.report_owns_stdout() {
        print!("{}", current_text);
    } else if stdin_emits_document {
        // Say what was dropped. Silence here is how "--fix --format compact"
        // over a pipe emptied a document with nothing on either stream and an
        // exit code of 0: compact and tabular print nothing at all for a clean
        // file, so the discard was indistinguishable from success.
        eprintln!(
            "{}--{}: rewritten text not emitted: --format owns stdout; \
             rerun without --format, or process a file",
            c.bold, c.reset
        );
    }

    // A dry run computes the fixes but emits nothing, so reporting has to stay
    // on the text the user still has.
    let wrote_changes = has_text_changes && !params.dry_run;
    if has_text_changes {
        let s2t_label = if input_was_sc && fix_applied == 0 {
            " (S2T only)"
        } else {
            ""
        };
        if params.dry_run {
            eprintln!(
                "{}{}{}: {} fix(es) would be applied{s2t_label}{declined} {}(dry run){}",
                c.bold, file_arg, c.reset, fix_applied, c.dim, c.reset
            );
        } else if file_arg == "--" {
            // The document already went to stdout above, once, for every stdin
            // path that rewrites it rather than only the one where --fix
            // changed something.
            //
            // Unconditional, like the file branch. Gating it on a nonzero
            // decline count meant stdin reported a fix only when something was
            // also turned down, which is the opposite of what a file does.
            // stdout stays reserved for the document.
            eprintln!(
                "{}--{}: {} fix(es) applied{s2t_label}{declined}",
                c.bold, c.reset, fix_applied
            );
        } else {
            // Atomic write: tempfile + rename in the same directory. Worth the
            // rename semantics here, unlike the baseline: this is the user's
            // source file, and a torn write loses their content rather than a
            // regenerable artifact.
            let file_path = Path::new(file_arg);
            let parent = file_path.parent().unwrap_or(Path::new("."));
            let mut tmp = tempfile::NamedTempFile::new_in(parent)
                .with_context(|| format!("{file_arg}: create tempfile in {}", parent.display()))?;
            std::io::Write::write_all(&mut tmp, current_text.as_bytes())
                .with_context(|| format!("write tempfile for {file_arg}"))?;

            // A temp file is created 0600. Carry over the mode of the file
            // being replaced, or --fix silently turns every source file it
            // touches into 0600 and git reports a mode change on each one. The
            // file was just read, so metadata is expected to succeed; a
            // cosmetic mode bit is not worth failing the write over.
            #[cfg(unix)]
            if let Ok(meta) = std::fs::metadata(file_path) {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode() & 0o7777;
                let _ = tmp
                    .as_file()
                    .set_permissions(std::fs::Permissions::from_mode(mode));
            }
            tmp.persist(file_path)
                .with_context(|| format!("rename tempfile to {file_arg}"))?;
            eprintln!(
                "{}{}{}: {} fix(es) applied{s2t_label}{declined}",
                c.bold, file_arg, c.reset, fix_applied
            );
        }
    } else if fix_declined > 0 {
        // --fix ran but rewrote nothing. Say so, or the run is
        // indistinguishable from one where --fix was never passed.
        let dry = if params.dry_run {
            format!(" {}(dry run){}", c.dim, c.reset)
        } else {
            String::new()
        };
        eprintln!(
            "{}{}{}: no fixes applied{declined}{dry}",
            c.bold, file_arg, c.reset
        );
    }

    Ok(FixEmission {
        text: current_text,
        wrote_changes,
    })
}

/// Run the fixer over one file's issues, or nothing when no tier is active.
///
/// TM-suppressed issues are dropped first, so the fixer never auto-corrects a
/// term the user deliberately rejected.
fn apply_fixes_for_file(
    ctx: &FileCtx<'_>,
    text: &str,
    content_type: zhtw_mcp::engine::scan::ContentType,
    issues: &[zhtw_mcp::rules::ruleset::Issue],
) -> Option<zhtw_mcp::fixer::FixResult> {
    let params = ctx.params;
    if params.fix_mode == zhtw_mcp::fixer::FixMode::None {
        return None;
    }
    let cfg = ctx.setup.cfg;

    let fix_issues: Vec<_> = match ctx.setup.tm_store {
        Some(ref tm) => issues
            .iter()
            .filter(|i| !tm.should_suppress(&i.found))
            .cloned()
            .collect(),
        None => issues.to_vec(),
    };

    // Write-side structure guard: the same exclusion ranges the MCP fix path
    // passes (src/mcp/tools.rs), built with the same options so the two front
    // ends cannot disagree about which bytes --fix may touch. Scan-time
    // exclusion is not enough, because a multi-part grammar match can span an
    // excluded region sitting between its parts, e.g. a fronted object written
    // as inline code sits between the parts.
    //
    // Rebuilt here rather than carried out of the scan because the scan builds
    // its ranges on the NFC-normalized text while the issues reaching the fixer
    // have been remapped back to original coordinates. Nothing to fix means
    // nothing to mask, and the Markdown build is a second full parse of the
    // document, so skip it on clean files.
    let excluded = if fix_issues.is_empty() {
        Vec::new()
    } else {
        zhtw_mcp::engine::scan::build_exclusions_for_content_type_with_config(
            text,
            content_type,
            &cfg,
        )
    };
    Some(zhtw_mcp::fixer::apply_fixes_with_context(
        text,
        &fix_issues,
        params.fix_mode,
        &excluded,
        Some(ctx.setup.scanner.segmenter()),
    ))
}

/// Re-read the document the fixer just wrote, so the report describes the file
/// on disk rather than the one that went in.
///
/// One rescan serves both the issue list and the signature refresh; the
/// signatures are updated in place, and only for the detectors that are
/// actually on, so an inactive one keeps whatever the first scan produced.
fn rescan_written_text(
    ctx: &FileCtx<'_>,
    current_text: &str,
    content_type: zhtw_mcp::engine::scan::ContentType,
    fix_result: Option<&zhtw_mcp::fixer::FixResult>,
    ai_signature: &mut Option<zhtw_mcp::engine::ai_score::AiSignatureReport>,
    translationese_signature: &mut Option<
        zhtw_mcp::engine::translationese_score::TranslationeseReport,
    >,
) -> Vec<zhtw_mcp::rules::ruleset::Issue> {
    let cfg = ctx.setup.cfg;
    let rescan_output =
        ctx.setup
            .scanner
            .scan_for_content_type_with_config(current_text, content_type, cfg);

    let ai_active = cfg.ai_filler_detection
        || cfg.ai_semantic_safety
        || cfg.ai_density_detection
        || cfg.ai_structural_patterns;
    if ai_active {
        *ai_signature = rescan_output.ai_signature;
    }
    if cfg.translationese_detection {
        *translationese_signature = rescan_output.translationese_signature;
    }

    let mut rescan = rescan_output.issues;
    if let Some(fix) = fix_result {
        // Suppress convergent-chain noise from the fixer's own replacements.
        zhtw_mcp::fixer::suppress_convergent_issues(&mut rescan, &fix.applied_fixes);
    }
    zhtw_mcp::rules::glossary::apply_glossary_with_coordinates(
        current_text,
        content_type,
        &cfg,
        rescan,
        &ctx.params.glossary,
    )
}

/// Drop baseline issues and fold what remains into the batch totals.
///
/// Returns the issues to report plus their error and warning counts, which the
/// caller needs for the per-file report and the exit-code gate.
fn apply_baseline_and_count(
    params: &LintBatchParams<'_>,
    file_arg: &str,
    report_issues: Vec<zhtw_mcp::rules::ruleset::Issue>,
    state: &mut BatchState,
) -> (Vec<zhtw_mcp::rules::ruleset::Issue>, usize, usize) {
    use zhtw_mcp::rules::ruleset::{ResolutionTier, Severity};

    // --update-baseline: record everything, and report on everything.
    if params.update_baseline {
        for issue in &report_issues {
            state.baseline.insert(file_arg, issue);
        }
    }

    // --baseline: filter out known issues, counting them separately so the
    // batch can say how many it suppressed.
    let new_issues: Vec<_> = if params.baseline_path.is_some() && !params.update_baseline {
        report_issues
            .into_iter()
            .filter(|i| {
                let known = state.baseline.contains(file_arg, i);
                if known {
                    state.baseline_count += 1;
                }
                !known
            })
            .collect()
    } else {
        report_issues
    };

    let error_count = new_issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    let warning_count = new_issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .count();
    state.totals.errors += error_count;
    state.totals.warnings += warning_count;

    // Resolution tier stats, from the issues actually reported.
    for issue in &new_issues {
        match ResolutionTier::classify(issue) {
            ResolutionTier::Deterministic => state.totals.deterministic += 1,
            ResolutionTier::Heuristic => state.totals.heuristic += 1,
            ResolutionTier::LlmJudged => state.totals.llm_judged += 1,
            ResolutionTier::Unresolved => state.totals.unresolved += 1,
        }
    }

    (new_issues, error_count, warning_count)
}

/// The subset of the batch parameters the output formatters read.
fn render_opts<'a>(params: &'a LintBatchParams<'a>) -> RenderOpts<'a> {
    RenderOpts {
        detect_style: params.detect_style,
        consistency: params.consistency,
        explain: params.explain,
        glossary: &params.glossary,
    }
}

/// Fix, rescan, verify, and report one already-scanned file.
///
/// Split out of `run_lint_batch` so the per-file pipeline can be read
/// without the batch setup and the phase-1 parallel scan around it.
fn process_scanned_file(
    ctx: &FileCtx<'_>,
    file_arg: &str,
    scan_result: ScanResult,
    state: &mut BatchState,
) -> Result<()> {
    let params = ctx.params;
    let c = ctx.colors;
    let cfg = ctx.setup.cfg;
    let tm_store = &ctx.setup.tm_store;
    let profile = ctx.profile;
    let multi = ctx.multi;

    let (text, input_was_sc, text_char_count, output, content_type) = scan_result?;

    let detected_script = if input_was_sc {
        "simplified"
    } else {
        output.detected_script.name()
    };
    let mut ai_signature = output.ai_signature;
    let mut translationese_signature = output.translationese_signature;
    let mut issues = output.issues;

    // Apply project glossary precedence (proper_noun suppression + banned-term
    // injection) before disambiguation, so the rest of the pipeline sees the
    // canonical issue list. Synthetic banned-term issues land with line 0 and
    // col 0 from Issue::new; reapply LineIndex so output formatters and the
    // consistency report see correct coordinates.
    issues = zhtw_mcp::rules::glossary::apply_glossary_with_coordinates(
        &text,
        content_type,
        &cfg,
        issues,
        &params.glossary,
    );

    // Tier 2: local disambiguation.
    let disambig_cfg = zhtw_mcp::engine::disambig::DisambigConfig {
        profile,
        ..Default::default()
    };
    let _disambig_stats =
        zhtw_mcp::engine::disambig::disambiguate_batch(&mut issues, &text, &disambig_cfg);

    let fix_result = apply_fixes_for_file(ctx, &text, content_type, &issues);

    // Writing the document and reporting on it are one step, kept in one
    // function. They used to be inline here, and the seam between "put the text
    // somewhere" and "tell the user what happened" is where the stdin
    // passthrough went wrong twice: once emitting nothing for an unchanged
    // document, once emitting it on top of a JSON report.
    let emitted = emit_fix_result(
        file_arg,
        &text,
        fix_result.as_ref(),
        input_was_sc,
        params,
        c,
    )?;
    let current_text = emitted.text;
    let wrote_changes = emitted.wrote_changes;

    let report_issues = if wrote_changes {
        rescan_written_text(
            ctx,
            current_text,
            content_type,
            fix_result.as_ref(),
            &mut ai_signature,
            &mut translationese_signature,
        )
    } else {
        issues
    };

    // --verify: calibrate issues via Google Translate.
    #[cfg(feature = "translate")]
    let report_issues = if params.verify {
        let calibrate_text = if wrote_changes {
            current_text
        } else {
            text.as_str()
        };
        let mut issues_mut = report_issues;
        let result = zhtw_mcp::engine::translate::calibrate_issues(calibrate_text, &mut issues_mut);
        eprintln!(
            "{}  verify: {} matched, {} unmatched, {} no_english, api_ok={}{}",
            c.dim, result.matched, result.unmatched, result.no_english, result.api_ok, c.reset,
        );
        issues_mut
    } else {
        report_issues
    };

    // Apply TM suppressions. Shared with the MCP tool so the two front ends
    // cannot drift on which issue types the TM is allowed to touch.
    let mut report_issues = report_issues;
    let tm_suppressed = tm_store
        .as_ref()
        .map_or(0, |tm| tm.suppress_issues(&mut report_issues));

    // Project ignore_terms, applied after TM for the same reason and through
    // the same function the MCP tool calls: the term stays visible but drops to
    // Info, so it counts against neither gate.
    if !params.ignore_terms.is_empty() {
        let ignore_set: std::collections::HashSet<&str> =
            params.ignore_terms.iter().map(String::as_str).collect();
        zhtw_mcp::rules::ignore::apply_ignore_set(&mut report_issues, &ignore_set);
    }

    // Baseline filtering and every running total the batch keeps, in one pass
    // over the issues that survive to the report.
    let (report_issues, error_count, warning_count) =
        apply_baseline_and_count(params, file_arg, report_issues, state);

    let report_text_char_count = if wrote_changes {
        fix_result
            .as_ref()
            .map_or(text_char_count, |f| f.text.chars().count())
    } else {
        text_char_count
    };

    let report = FileReport {
        file_arg,
        detected_script,
        issues: &report_issues,
        error_count,
        warning_count,
        tm_suppressed,
        fixes_applied: fix_result.as_ref().map(|f| f.applied),
        fixes_skipped: fix_result.as_ref().map(|f| f.skipped),
        fixes_declined: fix_result.as_ref().map(|f| f.declined),
        ai_signature: ai_signature.as_ref(),
        translationese_signature: translationese_signature.as_ref(),
        consistency_text: if wrote_changes {
            current_text
        } else {
            text.as_str()
        },
        text_char_count: report_text_char_count,
        multi,
    };

    match params.format {
        LintFormat::Json => {
            let output = render_json(&report, render_opts(params));
            if multi {
                state.file_results.push(output);
            } else {
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        }
        LintFormat::Human => render_human(&report, render_opts(params), c),
        LintFormat::Compact => render_compact(&report, params.explain),
        LintFormat::Tabular => {
            render_tabular(&report, params.explain, &mut state.tabular_header_printed);
        }
        LintFormat::Sarif => {
            collect_sarif(&report, &mut state.sarif_rules, &mut state.sarif_results)
        }
    }
    Ok(())
}
