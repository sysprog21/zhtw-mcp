// MCP tool handler implementations.
//
// One tool exposed to the MCP client:
//   zhtw: unified lint / fix / gate for Traditional Chinese (Taiwan) text

use std::cell::OnceCell;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use super::prompts;
use super::resources;
use rmcp::model::{
    CacheScope, CallToolResult, ContentBlock, GetPromptResult, JsonObject, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, ReadResourceResult, Tool,
    ToolAnnotations,
};
use rmcp::ErrorData;

use super::sampling::{refine_issues_with_sampling, SamplingBridge, SamplingStats};
use super::telemetry::{TelemetryMetrics, TokenTelemetry};
use crate::audit::Trace;
use crate::engine::disambig::{disambiguate_batch, DisambigConfig, DisambigStats};
use crate::engine::s2t::S2TConverter;
use crate::engine::scan::{is_spaced_acronym_issue, ContentType, Scanner};
#[cfg(feature = "translate")]
use crate::engine::translate::calibrate_issues;
use crate::engine::zhtype::{detect_chinese_type, ChineseType};
use crate::fixer::{
    apply_fixes_with_context, remap_to_post_fix, suppress_convergent_issues, FixMode,
};
use crate::rules::ignore::apply_ignore_set;
use crate::rules::loader::compute_ruleset_hash;
use crate::rules::ruleset::Ruleset;
use crate::rules::ruleset::{
    AttributionGenre, Issue, IssueType, PoliticalStance, Profile, ResolutionTier, Severity,
};
use crate::rules::store::{OverrideStore, PackStore, SuppressionStore, TranslationMemoryStore};

/// What the server reads and never changes: the compiled scanner and the
/// ruleset metadata derived from it.
///
/// Split out of `Server` so the handlers that only read it need no lock: a
/// lint holds the server mutex for its whole run, and `resources/read` has no
/// reason to queue behind one.
pub struct Catalog {
    scanner: Scanner,
    ruleset_hash: String,
    /// Rendered `zh-tw://dictionary/ambiguous` payload, built on first read.
    ambiguous_dict: std::sync::OnceLock<String>,
}

/// The MCP tool server. Holds the read-only [`Catalog`], the override and
/// suppression stores, and the state the handshake and the scan mutate.
pub struct Server {
    catalog: Arc<Catalog>,
    /// SC→TC converter for auto-converting Simplified Chinese input.
    /// Built lazily on first Simplified input: its automaton costs ~200ms to
    /// construct, which would otherwise sit on the startup/handshake path, and
    /// traditional-only sessions never need it at all.
    s2t: OnceCell<S2TConverter>,
    suppression_store: SuppressionStore,
    /// Translation memory: persistent correction tracking.
    tm_store: Option<TranslationMemoryStore>,
    /// Span-level judgment cache for persistent LLM disambiguation results.
    judgment_cache: crate::rules::judgment_cache::JudgmentCache,
    /// Client name from initialize handshake, used for auto-compact detection.
    client_name: Option<String>,
}

impl Server {
    /// Create a new server from the embedded ruleset + override/pack stores.
    pub fn new(
        store: OverrideStore,
        suppression_store: SuppressionStore,
        pack_store: PackStore,
        active_packs: Vec<String>,
        tm_store: Option<TranslationMemoryStore>,
    ) -> anyhow::Result<Self> {
        let base_ruleset = crate::rules::loader::load_embedded_ruleset()?;

        let (scanner, ruleset_hash) =
            Self::build_scanner(&base_ruleset, &store, &pack_store, &active_packs);

        let judgment_cache = crate::rules::judgment_cache::JudgmentCache::open_default();

        Ok(Self {
            catalog: Arc::new(Catalog {
                scanner,
                ruleset_hash,
                ambiguous_dict: std::sync::OnceLock::new(),
            }),
            s2t: OnceCell::new(),
            suppression_store,
            tm_store,
            judgment_cache,
            client_name: None,
        })
    }

    /// The immutable half, for handlers that need it without the lock.
    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        self.catalog.clone()
    }

    /// Build a scanner from the base ruleset, overrides, and active packs.
    fn build_scanner(
        base_ruleset: &Ruleset,
        store: &OverrideStore,
        pack_store: &PackStore,
        active_packs: &[String],
    ) -> (Scanner, String) {
        let (merged_spelling, merged_case) = crate::rules::store::build_merged_rules(
            &base_ruleset.spelling_rules,
            &base_ruleset.case_rules,
            store,
            pack_store,
            active_packs,
        );

        let ruleset_hash = compute_ruleset_hash(&merged_spelling, &merged_case);
        let scanner = Scanner::new(merged_spelling, merged_case);

        (scanner, ruleset_hash)
    }

    /// Run the `zhtw` tool.
    ///
    /// An unknown tool name is a tool-level error rather than a protocol one,
    /// which is what lets a client show it to the user instead of failing the
    /// call.
    ///
    /// `declared_client` is whoever this request said it is, which on the
    /// handshake-free revision is the only place the identity appears. It is
    /// passed rather than stored because the declaration is request-scoped:
    /// stored, one declared call would move the default output mode for every
    /// later undeclared call on the same connection.
    pub(crate) fn call_tool(
        &mut self,
        name: &str,
        arguments: &Value,
        bridge: Option<&mut SamplingBridge<'_>>,
        declared_client: Option<&str>,
    ) -> ParamResult<CallToolResult> {
        let _span = tracing::info_span!("mcp_request", method = "tools/call").entered();
        if name != "zhtw" {
            return Ok(tool_error(format!("unknown tool: {name}")));
        }
        if !arguments.is_object() {
            let actual = json_type_name(arguments);
            return Err(ErrorData::invalid_params(
                format!("arguments must be an object, got {actual}"),
                Some(
                    json!({ "field": "arguments", "expected_type": "object", "actual_type": actual }),
                ),
            ));
        }
        if let Some(error) = reject_unknown_params(arguments) {
            return Err(error);
        }

        // Refuse before scanning rather than after. "verify" ships sentence
        // excerpts of the caller's text to a third party, and the operator who
        // set the variable outranks the model that asked.
        #[cfg(feature = "translate")]
        if parse_verify(arguments) {
            if let Err(reason) = crate::engine::translate::refuse_if_network_disabled("\"verify\"")
            {
                return Err(ErrorData::invalid_params(
                    format!("{reason}; retry without it"),
                    Some(json!({ "field": "verify", "reason": "network_disabled" })),
                ));
            }
        }
        let result = self.tool_check(arguments, bridge, declared_client);

        // Whatever this call judged is written now rather than at exit. A
        // stateless client opens a process per call and ends it with a signal,
        // which runs neither Drop nor the exit flush, so a session's judgments
        // were being thrown away on the teardown path the clients actually use.
        // A call that judged nothing writes nothing: eviction alone does not
        // earn a rewrite of the whole store, because the next open redoes it
        // anyway.
        self.judgment_cache.flush_if_judged();
        result
    }

    // Tool implementation

    /// Maximum allowed size of the text field (256 KiB). Requests exceeding
    /// this trigger a structured error before any processing begins.
    const MAX_TEXT_BYTES: usize = 256 * 1024;

    /// Scan, stance-filter, calibrate, disambiguate, sample, and suppress.
    ///
    /// Both `fix_mode` paths run exactly this before they diverge; keeping
    /// it in one place is what stops a change landing on only one of them.
    fn run_scan_stage(
        &mut self,
        args: &ScanStageArgs<'_>,
        bridge: &mut Option<&mut SamplingBridge<'_>>,
    ) -> ScanStage {
        let ScanStageArgs {
            text,
            content_type,
            cfg,
            profile,
            stance,
            s2t_converted,
            ..
        } = *args;

        // Build the exclusion ranges once: the fix path reuses them for the
        // fixer and the post-fix remap.
        let excluded = crate::engine::scan::build_exclusions_for_content_type_with_config(
            text,
            content_type,
            &cfg,
        );
        let mut scan = self.catalog.scanner.scan_with_prebuilt_excluded_config(
            text,
            &excluded,
            cfg,
            content_type,
        );

        let detected_script = if s2t_converted {
            "simplified"
        } else {
            scan.detected_script.name()
        };
        let mut issues = std::mem::take(&mut scan.issues);
        let scanner_hit_count = issues.len();
        if let Some(st) = stance {
            filter_by_stance(&mut issues, st);
        }

        // Calibrate issues via Google Translate anchor matching.
        #[cfg(feature = "translate")]
        let calibrate_result = if args.verify {
            Some(calibrate_issues(text, &mut issues))
        } else {
            None
        };

        // Tier 2: local disambiguation. Resolves issues via context clues,
        // profile priors, and collocations before LLM sampling.
        let disambig_cfg = DisambigConfig {
            profile,
            ..Default::default()
        };
        let disambig_stats = disambiguate_batch(&mut issues, text, &disambig_cfg);

        // Tier 3: LLM sampling for gray-zone issues only.
        let sampling_stats = if let Some(b) = bridge.as_mut() {
            let mut cache_ctx = super::sampling::SamplingCacheCtx {
                cache: &mut self.judgment_cache,
                ruleset_hash: &self.catalog.ruleset_hash,
                profile: profile.name(),
                content_type: content_type.name(),
            };
            refine_issues_with_sampling(&mut issues, b, text, Some(&mut cache_ctx))
        } else {
            SamplingStats::default()
        };

        self.apply_suppressions(&mut issues);

        ScanStage {
            excluded,
            scan,
            issues,
            detected_script,
            scanner_hit_count,
            disambig_stats,
            sampling_stats,
            #[cfg(feature = "translate")]
            calibrate_result,
        }
    }

    fn tool_check(
        &mut self,
        args: &Value,
        mut bridge: Option<&mut SamplingBridge<'_>>,
        declared_client: Option<&str>,
    ) -> ParamResult<CallToolResult> {
        let started = std::time::Instant::now();
        // Snapshot cache counters at start for per-request telemetry.
        let cache_hits_before = self.judgment_cache.hits;
        let cache_misses_before = self.judgment_cache.misses;

        let text = require_str_validated(args, "text")?;

        if text.len() > Self::MAX_TEXT_BYTES {
            return Err(param_error(
                "text",
                &format!("{} bytes", text.len()),
                &[&format!("<= {} bytes (256 KiB)", Self::MAX_TEXT_BYTES)],
            ));
        }

        // Auto-detect Simplified Chinese and convert to Traditional via S2T.
        let s2t_converted: Option<String> = if detect_chinese_type(text) == ChineseType::Simplified
        {
            Some(self.s2t.get_or_init(S2TConverter::new).convert(text))
        } else {
            None
        };
        let text = s2t_converted.as_deref().unwrap_or(text);

        // This request's own declaration first, the handshake's second. Both
        // name the same client in practice; the order is what keeps a
        // declaration from outliving the request that made it.
        let client = declared_client.or(self.client_name.as_deref());
        let params = CheckParams::parse(args, default_output_mode(client))?;

        // Copy fields bind by value, the two owned ones by reference, so the
        // struct itself stays put for CheckRequest to borrow below.
        let CheckParams {
            fix_mode,
            profile,
            content_type,
            stance,
            output_mode,
            detect_style,
            detect_ai_opt,
            detect_translationese_opt,
            ai_threshold,
            relaxed,
            exempt_blockquotes,
            rhythm,
            include_telemetry,
            include_stats,
            #[cfg(feature = "translate")]
            verify,
            ref ignore_terms,
            ref translationese_domain_opt,
            ref document_genre_opt,
            ref register_opt,
            ..
        } = params;

        let ignore_set: std::collections::HashSet<&str> =
            ignore_terms.iter().map(String::as_str).collect();
        let stance_name = stance.unwrap_or(PoliticalStance::RocCentric).name();

        let _span = tracing::info_span!(
            "tool_check",
            content_length = text.len() as u64,
            content_type = content_type.name(),
            profile = profile.name()
        )
        .entered();

        // Tabular output carries none of these payloads, so asking for one
        // alongside it is a contradiction rather than a silent drop. The first
        // flag in this order is the one reported.
        let tabular_conflict = [
            ("include_telemetry", include_telemetry),
            ("include_stats", include_stats),
            ("detect_style", detect_style),
        ]
        .into_iter()
        .find(|&(_, requested)| requested)
        .filter(|_| output_mode == OutputMode::Tabular);
        if let Some((name, _)) = tabular_conflict {
            // The constraint is about output, so the machine-readable list is
            // the output modes that allow this flag, derived rather than
            // copied; the prose belongs in the message.
            let allowed: Vec<&str> = accepted_values("output")
                .into_iter()
                .filter(|mode| *mode != "tabular")
                .collect();
            return Err(ErrorData::invalid_params(
                format!("'{name}' cannot be used with output=tabular"),
                Some(json!({ "field": name, "value": true, "accepted": allowed })),
            ));
        }

        let cfg = build_check_config(
            profile,
            &CheckFlags {
                relaxed,
                exempt_blockquotes,
                stance,
                detect_style,
                detect_ai: detect_ai_opt,
                detect_translationese: detect_translationese_opt,
                translationese_domain: translationese_domain_opt.as_deref(),
                document_genre: document_genre_opt.as_deref(),
                register: register_opt.as_deref(),
                ai_threshold,
                rhythm,
            },
        )?;

        let stage_args = ScanStageArgs {
            text,
            content_type,
            cfg,
            profile,
            stance,
            s2t_converted: s2t_converted.is_some(),
            #[cfg(feature = "translate")]
            verify,
        };

        let request = CheckRequest {
            text,
            s2t_applied: s2t_converted.is_some(),
            params: &params,
            ignore_set: &ignore_set,
            stance_name,
            stage: stage_args,
            cache_hits_before,
            cache_misses_before,
        };

        let result = match fix_mode {
            FixMode::None => self.check_lint_only(&request, &mut bridge),
            mode @ (FixMode::Orthographic | FixMode::LexicalSafe | FixMode::LexicalContextual) => {
                self.check_with_fixes(mode, &request, &mut bridge)
            }
        };
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "tool_check completed"
        );
        Ok(result)
    }

    /// Per-request token telemetry, with the judgment-cache counters reduced
    /// to this request's share of the process totals.
    #[allow(clippy::too_many_arguments)]
    fn request_telemetry(
        &self,
        text: &str,
        scanner_hit_count: usize,
        disambig_stats: &DisambigStats,
        sampling_stats: &SamplingStats,
        bridge: Option<&&mut SamplingBridge<'_>>,
        applied_fixes: usize,
        cache_before: (u64, u64),
    ) -> TelemetryMetrics {
        build_telemetry(
            text,
            scanner_hit_count,
            disambig_stats,
            sampling_stats,
            bridge,
            applied_fixes,
            (
                self.judgment_cache.hits.saturating_sub(cache_before.0),
                self.judgment_cache.misses.saturating_sub(cache_before.1),
            ),
        )
    }

    /// Lint only: the shared scan stage is the whole pipeline.
    fn check_lint_only(
        &mut self,
        request: &CheckRequest<'_>,
        bridge: &mut Option<&mut SamplingBridge<'_>>,
    ) -> CallToolResult {
        let &CheckRequest {
            text,
            s2t_applied,
            params,
            ignore_set,
            stance_name,
            stage: ref stage_args,
            cache_hits_before,
            cache_misses_before,
        } = request;
        let &CheckParams {
            profile,
            content_type,
            max_errors,
            max_warnings,
            explain,
            output_mode,
            fix_output,
            detect_style,
            consistency_requested,
            include_telemetry,
            include_stats,
            ref glossary,
            ..
        } = params;
        let cfg = stage_args.cfg;

        // Lint-only path: the shared stage is the whole pipeline.
        let stage = self.run_scan_stage(stage_args, bridge);
        let ScanStage {
            scan,
            mut issues,
            detected_script,
            scanner_hit_count,
            disambig_stats,
            sampling_stats,
            #[cfg(feature = "translate")]
            calibrate_result,
            ..
        } = stage;
        let coverage = scan.coverage.as_ref();
        let oral_density = scan.oral_density;
        let quality_flags = &scan.quality_flags;
        let ai_signature = scan.ai_signature;
        let translationese_signature = scan.translationese_signature;

        // TM applies here because nothing rewrites the text on this path; the
        // fix path defers it until after the rescan.
        let tm_suppressed = self.apply_tm(&mut issues);
        apply_ignore_set(&mut issues, ignore_set);

        // Apply project glossary precedence (banned > TM): proper_nouns
        // suppress, banned inject synthetic Errors.
        issues = crate::rules::glossary::apply_glossary_with_coordinates(
            text,
            content_type,
            &cfg,
            issues,
            glossary,
        );

        // Document-wide consistency report.
        let consistency_report = consistency_requested
            .then(|| {
                crate::engine::consistency::compute_consistency_report(text, &issues, glossary)
            })
            .filter(|r| !r.is_empty());

        // Build telemetry if requested.
        let telemetry = include_telemetry.then(|| {
            self.request_telemetry(
                text,
                scanner_hit_count,
                &disambig_stats,
                &sampling_stats,
                bridge.as_ref(),
                0,
                (cache_hits_before, cache_misses_before),
            )
        });

        let trace =
            Trace::new("zhtw", &self.catalog.ruleset_hash, text).with_issue_count(issues.len());

        // Pre-build the composite scorecard so its lifetime spans the
        // build_check_output call (the params struct only borrows it).
        let style_scorecard = style_scorecard_for(
            detect_style,
            ai_signature.as_ref(),
            translationese_signature.as_ref(),
            &issues,
            text,
        );

        build_check_output(&CheckOutputParams {
            result_text: text,
            issues: &issues,
            applied_fixes: 0,
            max_errors,
            max_warnings,
            profile,
            stance_name,
            detected_script,
            s2t_applied,
            trace: &trace,
            explain,
            output_mode,
            has_fixes: s2t_applied,
            fix_output,
            original_text: text,
            fix_records: &[],
            #[cfg(feature = "translate")]
            calibrate_result,
            coverage,
            oral_density,
            quality_flags,
            ai_signature: ai_signature.as_ref(),
            translationese_signature: translationese_signature.as_ref(),
            style_scorecard: style_scorecard.as_ref(),
            tm_suppressed,
            sampling_stats,
            disambig_stats,
            telemetry,
            include_stats,
            consistency: consistency_report.as_ref(),
        })
    }

    /// Fix: the shared scan stage, then apply the fixes and re-scan the result
    /// for what is left.
    fn check_with_fixes(
        &mut self,
        mode: FixMode,
        request: &CheckRequest<'_>,
        bridge: &mut Option<&mut SamplingBridge<'_>>,
    ) -> CallToolResult {
        let &CheckRequest {
            text,
            s2t_applied,
            params,
            ignore_set,
            stance_name,
            stage: ref stage_args,
            cache_hits_before,
            cache_misses_before,
        } = request;
        let &CheckParams {
            profile,
            content_type,
            stance,
            max_errors,
            max_warnings,
            explain,
            output_mode,
            fix_output,
            detect_style,
            consistency_requested,
            include_telemetry,
            include_stats,
            ref glossary,
            ..
        } = params;
        let cfg = stage_args.cfg;

        // Fix path: shared stage, then apply fixes and re-scan for residual
        // issues.
        let stage = self.run_scan_stage(stage_args, bridge);
        let ScanStage {
            excluded,
            mut issues,
            detected_script,
            scanner_hit_count,
            disambig_stats,
            sampling_stats,
            #[cfg(feature = "translate")]
            calibrate_result,
            ..
        } = stage;

        // TM is NOT applied here: the fixer filter (should_suppress) prevents
        // fixing TM-rejected terms, and the post-fix apply_tm handles severity
        // downgrade + counting on the final residual.
        apply_ignore_set(&mut issues, ignore_set);
        issues = crate::rules::glossary::apply_glossary_with_coordinates(
            text,
            content_type,
            &cfg,
            issues,
            glossary,
        );

        // Snapshot AFTER suppressions so restored severity reflects final
        // state.
        let preserved_states = snapshot_states(&issues);

        // Filter out TM-suppressed issues before fixing: a term the user
        // deliberately rejected must not be auto-corrected.
        let fix_issues: Vec<Issue> = match &self.tm_store {
            Some(tm) => issues
                .iter()
                .filter(|i| !tm.should_suppress(&i.found))
                .cloned()
                .collect(),
            None => issues.clone(),
        };

        let fix_result = apply_fixes_with_context(
            text,
            &fix_issues,
            mode,
            &excluded,
            Some(self.catalog.scanner.segmenter()),
        );

        // Re-scan after fixes: use post-fix ai_signature, not pre-fix. Remap
        // exclusion zones to post-fix coordinates instead of rebuilding from
        // scratch (avoids re-parsing markdown/URLs on the entire document for
        // every fix cycle).
        let remapped_excl = crate::fixer::remap_exclusions(&excluded, &fix_result.applied_fixes);
        let rescan_out = self.catalog.scanner.scan_with_prebuilt_excluded_config(
            &fix_result.text,
            &remapped_excl,
            cfg,
            content_type,
        );
        let coverage = rescan_out.coverage.as_ref();
        let oral_density = rescan_out.oral_density;
        let quality_flags = &rescan_out.quality_flags;
        let ai_signature = rescan_out.ai_signature;
        let translationese_signature = rescan_out.translationese_signature;
        let mut remaining_issues = rescan_out.issues;
        if let Some(st) = stance {
            filter_by_stance(&mut remaining_issues, st);
        }
        self.apply_suppressions(&mut remaining_issues);
        apply_ignore_set(&mut remaining_issues, ignore_set);

        restore_preserved_states(
            &mut remaining_issues,
            &preserved_states,
            &fix_result.applied_fixes,
        );

        // Suppress convergent-chain noise: remove re-scan issues whose offset
        // falls within a byte range written by the fixer.
        suppress_convergent_issues(&mut remaining_issues, &fix_result.applied_fixes);

        remaining_issues = crate::rules::glossary::apply_glossary_with_coordinates(
            &fix_result.text,
            content_type,
            &cfg,
            remaining_issues,
            glossary,
        );

        // Apply TM after preserved state restoration so the count reflects the
        // true final state, not a pre-fix snapshot.
        let tm_suppressed = self.apply_tm(&mut remaining_issues);

        let consistency_report = consistency_requested
            .then(|| {
                crate::engine::consistency::compute_consistency_report(
                    &fix_result.text,
                    &remaining_issues,
                    glossary,
                )
            })
            .filter(|r| !r.is_empty());

        // Build telemetry if requested.
        let telemetry = include_telemetry.then(|| {
            self.request_telemetry(
                text,
                scanner_hit_count,
                &disambig_stats,
                &sampling_stats,
                bridge.as_ref(),
                fix_result.applied,
                (cache_hits_before, cache_misses_before),
            )
        });

        let trace = Trace::new("zhtw", &self.catalog.ruleset_hash, text)
            .with_issue_count(remaining_issues.len())
            .with_output(&fix_result.text);

        // Composite scorecard against the post-fix text and remaining issues,
        // so the scorecard reflects the user-visible state.
        let style_scorecard = style_scorecard_for(
            detect_style,
            ai_signature.as_ref(),
            translationese_signature.as_ref(),
            &remaining_issues,
            &fix_result.text,
        );

        build_check_output(&CheckOutputParams {
            result_text: &fix_result.text,
            issues: &remaining_issues,
            applied_fixes: fix_result.applied,
            max_errors,
            max_warnings,
            profile,
            stance_name,
            detected_script,
            s2t_applied,
            trace: &trace,
            explain,
            output_mode,
            has_fixes: fix_result.applied > 0 || s2t_applied,
            fix_output,
            original_text: text,
            fix_records: &fix_result.applied_fixes,
            #[cfg(feature = "translate")]
            calibrate_result,
            coverage,
            oral_density,
            quality_flags,
            ai_signature: ai_signature.as_ref(),
            translationese_signature: translationese_signature.as_ref(),
            style_scorecard: style_scorecard.as_ref(),
            tm_suppressed,
            sampling_stats,
            disambig_stats,
            telemetry,
            include_stats,
            consistency: consistency_report.as_ref(),
        })
    }

    /// Record the client identity a handshake established.
    ///
    /// The SDK owns `initialize` itself, so this is how the negotiated state
    /// still reaches the pipeline: `client_name` selects the default output
    /// mode for calls that do not name a client themselves. A request that
    /// does name one passes it to `call_tool` instead, because on the
    /// handshake-free revision the declaration belongs to that request alone.
    /// Per-request capabilities are handled by the SDK adapter.
    pub(crate) fn set_client(&mut self, name: String) {
        self.client_name = Some(name);
    }

    /// Persist the judgment cache. `process::exit` skips `Drop`.
    pub(crate) fn flush_judgment_cache(&mut self) {
        self.judgment_cache.flush();
    }

    /// Downgrade suppressed issues to Info severity.
    fn apply_suppressions(&self, issues: &mut [Issue]) {
        for issue in issues {
            if self.suppression_store.is_suppressed(&issue.found) {
                issue.severity = Severity::Info;
            }
        }
    }

    /// Apply translation memory, if one is configured.  See
    /// [`TranslationMemoryStore::suppress_issues`](crate::rules::store::TranslationMemoryStore::suppress_issues)
    /// for which issue types it may touch; the CLI shares that policy.
    fn apply_tm(&self, issues: &mut [Issue]) -> usize {
        self.tm_store
            .as_ref()
            .map_or(0, |tm| tm.suppress_issues(issues))
    }
}

/// Result of parsing a request or tool argument.
///
/// The error side is RMCP's own, because that is what the adapter hands back
/// and nothing between here and the wire adds to it. It carries the JSON-RPC
/// code, the message, and the structured data clients render diagnostics from.
/// Which request id the error correlates to is RMCP's business, not this
/// layer's, which is why none of these helpers take one.
pub(crate) type ParamResult<T> = Result<T, ErrorData>;

/// Return an INVALID_PARAMS JSON-RPC error if `args` contains keys not in
/// the known parameter set. Returns `None` when all keys are recognized.
fn reject_unknown_params(args: &Value) -> Option<ErrorData> {
    let obj = args.as_object()?;
    let known = input_schema_properties();
    let unexpected: Vec<&str> = obj
        .keys()
        .filter(|k| !known.contains_key(k.as_str()))
        .map(String::as_str)
        .collect();
    if unexpected.is_empty() {
        return None;
    }
    Some(ErrorData::invalid_params(
        format!(
            "unknown parameter{}: {}",
            if unexpected.len() > 1 { "s" } else { "" },
            unexpected.join(", "),
        ),
        Some(json!({ "unexpected": unexpected })),
    ))
}

/// The values the schema declares for an enum-valued parameter.
///
/// Empty for a parameter the schema does not constrain to a list, which is
/// what `param_error` is for.
fn accepted_values(field: &str) -> Vec<&'static str> {
    input_schema_properties()
        .get(field)
        .and_then(|prop| prop.get("enum"))
        .and_then(|values| values.as_array())
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Reject a value the schema does not allow, naming what it does.
///
/// The list comes from the schema rather than from the call site: stating it
/// twice is how a value gets added to what the tool advertises and still
/// rejected by what parses it.
fn enum_param_error(field: &str, value: &str) -> ErrorData {
    param_error(field, value, &accepted_values(field))
}

/// Build a structured INVALID_PARAMS JSON-RPC error for a bad tool parameter.
/// The `data` field carries `{"field", "value", "accepted"}` so clients can
/// render actionable diagnostics without parsing the message string.
fn param_error(field: &str, value: &str, accepted: &[&str]) -> ErrorData {
    ErrorData::invalid_params(
        format!("invalid '{field}': '{value}'"),
        Some(json!({ "field": field, "value": value, "accepted": accepted })),
    )
}

/// Extract a required string field from a JSON object, returning a
/// structured INVALID_PARAMS error on failure. Distinguishes missing
/// field from present-but-wrong-type so clients get actionable diagnostics.
fn require_str_validated<'a>(args: &'a Value, field: &str) -> ParamResult<&'a str> {
    match args.get(field) {
        None => Err(ErrorData::invalid_params(
            format!("missing required parameter '{field}'"),
            Some(json!({ "field": field })),
        )),
        Some(v) => v.as_str().ok_or_else(|| {
            let type_name = json_type_name(v);
            ErrorData::invalid_params(
                format!("'{field}' must be a string, got {type_name}"),
                Some(
                    json!({ "field": field, "expected_type": "string", "actual_type": type_name }),
                ),
            )
        }),
    }
}

/// Extract an optional string field, returning INVALID_PARAMS if the
/// value is present but not a string. Returns `Ok(None)` when absent.
fn optional_str_validated<'a>(args: &'a Value, field: &str) -> ParamResult<Option<&'a str>> {
    match args.get(field) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s)),
            None => {
                let type_name = json_type_name(v);
                Err(ErrorData::invalid_params(
                    format!("'{field}' must be a string, got {type_name}"),
                    Some(
                        json!({ "field": field, "expected_type": "string", "actual_type": type_name }),
                    ),
                ))
            }
        },
    }
}

/// Human-readable JSON type name for error diagnostics.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
        Value::String(_) => "string",
    }
}

/// Everything `tool_check` reads out of its JSON arguments.
///
/// Exists to give the parsing a name and a home, not to be passed around: the
/// caller destructures it immediately, so the body works with the same locals
/// it always did.  That is deliberate.  A struct threaded through four hundred
/// lines would have meant renaming every use, which is a lot of silent risk for
/// a change whose whole point is legibility.
struct CheckParams<'a> {
    fix_mode: FixMode,
    profile: Profile,
    content_type: ContentType,
    stance: Option<PoliticalStance>,
    max_errors: Option<u64>,
    max_warnings: Option<u64>,
    ignore_terms: Vec<String>,
    explain: bool,
    output_mode: OutputMode,
    fix_output: FixOutputMode,
    #[cfg(feature = "translate")]
    verify: bool,
    /// Explicit bool overrides the profile default; absent means inherit.  The
    /// default profile enables both AI-filler and translationese detection.
    detect_ai_opt: Option<bool>,
    detect_translationese_opt: Option<bool>,
    /// Composite three-axis scorecard, opt-in.  Mirrors the CLI
    /// `--detect-style` shorthand; off by default to keep the payload lean.
    detect_style: bool,
    translationese_domain_opt: Option<String>,
    document_genre_opt: Option<String>,
    register_opt: Option<String>,
    ai_threshold: Option<&'a str>,
    relaxed: bool,
    exempt_blockquotes: bool,
    /// Advisory rhythm (氣口) axis. Opt-in and never fixable, exactly as on
    /// the CLI: the tool exposes it so an agent can ask for the same advice a
    /// human gets from --rhythm.
    rhythm: bool,
    glossary: crate::rules::glossary::ProjectGlossary,
    consistency_requested: bool,
    include_telemetry: bool,
    include_stats: bool,
}

impl<'a> CheckParams<'a> {
    fn parse(args: &'a Value, default_output: OutputMode) -> ParamResult<Self> {
        Ok(Self {
            fix_mode: parse_fix_mode(args)?,
            profile: parse_profile(args)?,
            content_type: parse_content_type(args)?,
            stance: parse_political_stance(args)?,
            max_errors: args.get("max_errors").and_then(|v| v.as_u64()),
            max_warnings: args.get("max_warnings").and_then(|v| v.as_u64()),
            ignore_terms: parse_ignore_terms(args),
            explain: parse_explain(args),
            output_mode: parse_output_mode(args, default_output)?,
            fix_output: parse_fix_output(args)?,
            #[cfg(feature = "translate")]
            verify: parse_verify(args),
            detect_ai_opt: parse_flag_opt(args, "detect_ai"),
            detect_translationese_opt: parse_flag_opt(args, "detect_translationese"),
            detect_style: parse_flag(args, "detect_style"),
            translationese_domain_opt: args
                .get("translationese_domain")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            document_genre_opt: optional_str_validated(args, "document_genre")?.map(str::to_string),
            register_opt: optional_str_validated(args, "register")?.map(str::to_string),
            ai_threshold: optional_str_validated(args, "ai_threshold")?,
            relaxed: parse_flag(args, "relaxed"),
            exempt_blockquotes: parse_flag(args, "exempt_blockquotes"),
            rhythm: parse_flag(args, "rhythm"),
            glossary: parse_glossary(args),
            consistency_requested: parse_flag(args, "consistency"),
            include_telemetry: parse_flag(args, "include_telemetry"),
            include_stats: parse_flag(args, "include_stats"),
        })
    }
}

/// An optional boolean argument, absent meaning "inherit the default".
fn parse_flag_opt(args: &Value, field: &str) -> Option<bool> {
    args.get(field).and_then(|v| v.as_bool())
}

/// A boolean argument that defaults to false when absent or malformed.
fn parse_flag(args: &Value, field: &str) -> bool {
    parse_flag_opt(args, field).unwrap_or(false)
}

/// Parse the optional "fix_mode" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_fix_mode(args: &Value) -> ParamResult<FixMode> {
    match optional_str_validated(args, "fix_mode")? {
        Some("orthographic") => Ok(FixMode::Orthographic),
        Some("lexical_safe") => Ok(FixMode::LexicalSafe),
        Some("lexical_contextual") => Ok(FixMode::LexicalContextual),
        None | Some("none") => Ok(FixMode::None),
        Some(other) => Err(enum_param_error("fix_mode", other)),
    }
}

/// Parse the optional "content_type" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_content_type(args: &Value) -> ParamResult<ContentType> {
    match optional_str_validated(args, "content_type")? {
        // Plain, not the file-name guess the CLI makes: a tool call carries
        // text and no name to guess from, and reading unmarked text as Markdown
        // would skip whatever looks like a fence inside it.
        None => Ok(ContentType::Plain),
        Some(other) => {
            ContentType::from_name(other).ok_or_else(|| enum_param_error("content_type", other))
        }
    }
}

/// Parse the optional "profile" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_profile(args: &Value) -> ParamResult<Profile> {
    match optional_str_validated(args, "profile")? {
        None => Ok(Profile::Base),
        Some(s) => Profile::from_str_strict(s).ok_or_else(|| enum_param_error("profile", s)),
    }
}

/// Parse the optional "political_stance" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_political_stance(args: &Value) -> ParamResult<Option<PoliticalStance>> {
    match optional_str_validated(args, "political_stance")? {
        None => Ok(None),
        Some(s) => PoliticalStance::from_str_strict(s)
            .map(Some)
            .ok_or_else(|| enum_param_error("political_stance", s)),
    }
}

/// Fix output format: how corrected text is returned when fixes are applied.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FixOutputMode {
    /// Return the full corrected text (backward compat default).
    Full,
    /// Return search/replace blocks (LLM-friendly patching format).
    SearchReplace,
    /// Return a patches array with byte offsets into the original text.
    Patch,
}

impl FixOutputMode {
    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SearchReplace => "search_replace",
            Self::Patch => "patch",
        }
    }
}

/// Parse the optional "fix_output" parameter from tool arguments.
fn parse_fix_output(args: &Value) -> ParamResult<FixOutputMode> {
    match optional_str_validated(args, "fix_output")? {
        Some("full") | None => Ok(FixOutputMode::Full),
        Some("search_replace") => Ok(FixOutputMode::SearchReplace),
        Some("patch") => Ok(FixOutputMode::Patch),
        Some(other) => Err(enum_param_error("fix_output", other)),
    }
}

/// Parse the optional "explain" boolean from tool arguments.
fn parse_explain(args: &Value) -> bool {
    args.get("explain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Output mode for zhtw responses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Full,
    Compact,
    /// Header-once TSV format for LLM-facing responses.
    /// Eliminates JSON syntax tax (repeated keys, braces, quotes) that
    /// inflates BPE token count by 40-60% with zero semantic value.
    Tabular,
    /// AI summary only: issue counts + AI signature report.
    /// No individual issues, no text. Lets downstream tools quickly
    /// decide whether to trigger a full review.
    Summary,
}

/// Parse the optional "output" mode from tool arguments.
/// When no explicit value is given, uses the provided default (which may
/// be auto-detected from the client identity).
fn parse_output_mode(args: &Value, default: OutputMode) -> ParamResult<OutputMode> {
    match optional_str_validated(args, "output")? {
        Some("compact") => Ok(OutputMode::Compact),
        Some("full") => Ok(OutputMode::Full),
        Some("tabular") => Ok(OutputMode::Tabular),
        Some("summary") => Ok(OutputMode::Summary),
        None => Ok(default),
        Some(other) => Err(enum_param_error("output", other)),
    }
}

/// Known AI agent/CLI client names that benefit from compact output.
/// Matched as exact full-name against the lowercased `clientInfo.name`.
/// Only programmatic agents/CLIs: NOT desktop GUI apps like "Claude Desktop".
const AI_AGENT_CLIENTS: &[&str] = &[
    "claude-code",
    "claude code",
    "cursor",
    "cline",
    "continue",
    "zed",
    "windsurf",
    "copilot",
    "aider",
    "cody",
    "roo",
    "roo-code",
    "roo code",
];

/// Determine default output mode from client identity.
/// Uses exact full-name match only to avoid false positives on clients
/// like "Claude Desktop" that happen to share a token with an agent name.
/// Strips trailing version suffixes (`/1.0`, ` 1.0`) before matching,
/// since some clients embed version info in the name field.
fn default_output_mode(client_name: Option<&str>) -> OutputMode {
    match client_name {
        Some(name) => {
            let lower = name.to_ascii_lowercase();

            // Strip trailing version suffix: "Cursor/0.1.0" → "cursor", "cline
            // 1.2" → "cline"
            let base = lower
                .split('/')
                .next()
                .unwrap_or(&lower)
                .trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')
                .trim();
            if AI_AGENT_CLIENTS.contains(&base) {
                OutputMode::Compact
            } else {
                OutputMode::Full
            }
        }
        None => OutputMode::Full,
    }
}

/// Parse the optional "verify" flag from tool arguments.
#[cfg(feature = "translate")]
fn parse_verify(args: &Value) -> bool {
    args.get("verify")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Generate a cultural/linguistic explanation for an issue.
///
/// Draws from the context, english, and rule_type fields to produce
/// a brief explanation useful for AI agents and educational applications.
fn build_explanation(issue: &Issue) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    match issue.rule_type {
        IssueType::CrossStrait => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is a mainland Chinese term for '{}'; Taiwan uses '{}'.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            } else if !issue.suggestions.is_empty() {
                parts.push(format!(
                    "'{}' is a mainland Chinese expression; Taiwan standard: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::Confusable => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is ambiguous across the strait. English anchor: '{}'. Taiwan form: {}.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::PoliticalColoring => {
            parts.push(format!(
                "'{}' carries mainland political connotations; prefer {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Variant => {
            parts.push(format!(
                "'{}' is a non-standard character variant; MoE standard form: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Typo => {
            parts.push(format!(
                "'{}' appears to be a typo; suggested: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Case => {
            parts.push(format!(
                "'{}' has incorrect casing; standard form: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Punctuation => {
            parts.push(format!(
                "'{}' should use the full-width equivalent {} in CJK prose per MoE standards.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Grammar => {
            if let Some(ctx) = &issue.context {
                parts.push(format!(
                    "'{}' — {}. Suggested: {}.",
                    issue.found,
                    ctx,
                    issue.suggestions.join(" / "),
                ));
            } else {
                parts.push(format!(
                    "'{}' is a grammatical issue; suggested: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::AiStyle => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }

            // Read the suggestions directly rather than the derived
            // suggested_rewrite field, so a stale derivation cannot change what
            // the reader is told.
            match &*issue.suggestions {
                // Advice only: the context already says what to do, and telling
                // a reader to remove an unsourced attribution would delete the
                // claim rather than source it.
                [] => {}
                [one] if !one.is_empty() => parts.push(format!("Suggested rewrite: {one}.")),
                all if all.iter().any(|s| !s.is_empty()) => parts.push(
                    "Rewrite the surrounding clause; do not choose an alternative mechanically."
                        .to_string(),
                ),
                _ => parts.push("Consider removing or rephrasing.".to_string()),
            }
        }
        IssueType::Translationese => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }
            if !issue.suggestions.is_empty() {
                let sugg = issue.suggestions.join(" / ");
                parts.push(format!("Suggested rewrite: {sugg}."));
            } else {
                parts.push(
                    "Translationese / 歐化 pattern; consider an idiomatic zh-TW rewrite."
                        .to_string(),
                );
            }
        }
        IssueType::Repetition => {
            if is_spaced_acronym_issue(issue) {
                parts.push(format!(
                    "'{}' should be written as '{}'; the spacing looks like a transcription artifact.",
                    issue.found,
                    issue.suggestions[0],
                ));
            } else {
                parts.push(format!(
                    "'{}' is a consecutive duplicate; remove the repetition.",
                    issue.found,
                ));
            }
        }
    }

    // Grammar, AiStyle, and Translationese issues already embed context in the
    // main explanation; skip the shared Context: append to avoid duplication.
    if !matches!(
        issue.rule_type,
        IssueType::Grammar | IssueType::AiStyle | IssueType::Translationese
    ) {
        if let Some(ctx) = &issue.context {
            parts.push(format!("Context: {ctx}"));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Parse the optional "ignore_terms" array from tool arguments.
fn parse_ignore_terms(args: &Value) -> Vec<String> {
    args.get("ignore_terms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the optional `glossary` object.  Shape:
/// `{ "banned": [...], "preferred": [...], "proper_nouns": [...] }`.
/// Each field is optional.  Missing object → empty glossary.
fn parse_glossary(args: &Value) -> crate::rules::glossary::ProjectGlossary {
    fn array_of_strings(v: Option<&Value>) -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
    let Some(glossary) = args.get("glossary").and_then(|v| v.as_object()) else {
        return crate::rules::glossary::ProjectGlossary::default();
    };
    crate::rules::glossary::ProjectGlossary {
        banned: array_of_strings(glossary.get("banned")),
        preferred: array_of_strings(glossary.get("preferred")),
        proper_nouns: array_of_strings(glossary.get("proper_nouns")),
    }
}

/// Remove political_coloring issues that the given stance suppresses.
fn filter_by_stance(issues: &mut Vec<Issue>, stance: PoliticalStance) {
    issues.retain(|issue| {
        issue.rule_type != IssueType::PoliticalColoring || stance.allows_rule(&issue.found)
    });
}

/// Issue severity summary counts.
#[derive(Serialize)]
struct IssueSummary {
    errors: usize,
    warnings: usize,
    info: usize,
    /// Number of issues downgraded to Info by translation memory.
    /// Omitted (0) when TM is inactive or had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    tm_suppressed: usize,
    /// Issues resolved by Tier 2 local disambiguation (context clues,
    /// profile priors, collocations).  Omitted (0) when Tier 2 had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    tier2_resolved: usize,
    /// Issues in Tier 2 gray zone (forwarded to Tier 3 LLM).
    #[serde(skip_serializing_if = "is_zero")]
    tier2_gray_zone: usize,
    /// Number of sampling calls made during this invocation.
    /// Omitted (0) when sampling is inactive or unused.
    #[serde(skip_serializing_if = "is_zero")]
    sampling_used: usize,
    /// Number of eligible issues skipped because the sampling budget was
    /// exhausted.
    /// Omitted (0) when budget was not exhausted.
    #[serde(skip_serializing_if = "is_zero")]
    sampling_skipped: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Resolution tier counts and confidence distribution for the session.
/// Included in tool output when `include_stats` is true.
#[derive(Serialize)]
struct SummaryMetrics {
    deterministic_fixes: usize,
    heuristic_fixes: usize,
    llm_judged_fixes: usize,
    unresolved: usize,
    llm_calls: usize,
    llm_tokens: u64,
    confidence_distribution: ConfidenceDistribution,
}

/// Confidence buckets: high (deterministic + heuristic), medium (llm_judged),
/// low (unresolved).
#[derive(Serialize)]
struct ConfidenceDistribution {
    high: usize,
    medium: usize,
    low: usize,
}

/// Build summary_metrics from issues and accumulated stats.
fn build_summary_metrics(
    issues: &[Issue],
    sampling_stats: &SamplingStats,
    telemetry: Option<&TelemetryMetrics>,
) -> SummaryMetrics {
    let mut deterministic = 0usize;
    let mut heuristic = 0usize;
    let mut llm_judged = 0usize;
    let mut unresolved = 0usize;

    for issue in issues {
        match ResolutionTier::classify(issue) {
            ResolutionTier::Deterministic => deterministic += 1,
            ResolutionTier::Heuristic => heuristic += 1,
            ResolutionTier::LlmJudged => llm_judged += 1,
            ResolutionTier::Unresolved => unresolved += 1,
        }
    }

    let llm_tokens = telemetry.map_or(0, |t| {
        t.raw
            .estimated_prompt_tokens
            .saturating_add(t.raw.estimated_completion_tokens)
    });

    SummaryMetrics {
        deterministic_fixes: deterministic,
        heuristic_fixes: heuristic,
        llm_judged_fixes: llm_judged,
        unresolved,
        llm_calls: sampling_stats.used,
        llm_tokens,
        confidence_distribution: ConfidenceDistribution {
            high: deterministic + heuristic,
            medium: llm_judged,
            low: unresolved,
        },
    }
}

/// Gate status in the tool response.
#[derive(Serialize)]
struct GateInfo {
    enabled: bool,
    max_errors: usize,
    residual_errors: usize,
    max_warnings: usize,
    residual_warnings: usize,
}

/// Anchor provenance for explain mode (borrowed).
#[derive(Serialize)]
struct AnchorProvenance<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_en: Option<&'a str>,
    anchor_match: Option<bool>,
}

// EditorialConfidence is canonical-defined in crate::rules::ruleset so that
// SpellingRule.editorial_confidence and the per-issue field share a single
// type. Re-exported here for the explain pipeline.
use crate::rules::ruleset::EditorialConfidence;

/// Structured per-issue explain metadata.
///
/// Surfaced only when `explain` is requested.  Helps reviewers understand
/// the confidence behind each suggestion without parsing free-form prose.
#[derive(Serialize)]
struct ExplainMeta<'a> {
    /// Why this is flagged.  Sourced from rule context + MoE refs when
    /// available; falls back to a structured restatement of the
    /// suggestion target.
    rationale: String,
    /// Domain that triggered the rule.  Parsed from `@domain X` markers
    /// in the rule's context field; defaults to "general".
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<&'a str>,
    /// True when the surface form is identical across zh-CN and zh-TW
    /// but the meaning differs (e.g. 文件: document vs file).
    is_false_friend: bool,
    /// Whether `--fix` would safely apply this suggestion.
    auto_fix_safe: bool,
    /// Whether the suggestion benefits from manual review.
    needs_review: bool,
    /// Per-issue editorial confidence: distinguishes binary corrections
    /// from style preferences (e.g. 場景 is correct zh-TW for a film or
    /// stage scene, so rewriting it to 情境 is an IT-context judgment call,
    /// whereas 線程 to 執行緒 is simply the zh-TW term).
    editorial_confidence: EditorialConfidence,
}

/// Heuristic fallback when an issue lacks a rule-level
/// `editorial_confidence`.  Translationese / AI-style / grammar hits and
/// any `Info`-severity or anchor-rejected issue are surfaced as `Low`;
/// hits with explicit context support climb to `Medium`; everything else
/// is `High`.
fn heuristic_editorial_confidence(issue: &Issue) -> EditorialConfidence {
    use crate::rules::ruleset::{IssueType, Severity};

    let always_low = matches!(
        issue.rule_type,
        IssueType::Translationese | IssueType::AiStyle | IssueType::Grammar
    ) || issue.severity == Severity::Info
        || issue.anchor_match == Some(false);
    if always_low {
        return EditorialConfidence::Low;
    }
    if issue.context_clues.is_some() || issue.anchor_match == Some(true) {
        EditorialConfidence::Medium
    } else {
        EditorialConfidence::High
    }
}

/// Derive structured explain metadata for an issue.
///
/// Confidence resolution order:
///   1. Honor `issue.editorial_confidence` if the rule annotated it
///      (set in `assets/ruleset.json` per-rule).
///   2. Otherwise, fall back to heuristics on rule type / severity /
///      anchor_match / context_clues.
///
/// Invariants: `editorial_confidence == Low` ⇒ `auto_fix_safe = false`
/// AND `needs_review = true`.
fn derive_explain_meta(issue: &Issue) -> ExplainMeta<'_> {
    use crate::rules::ruleset::IssueType;

    // -- Domain extraction from "@domain X" markers in the rule context.
    let domain = issue.context.as_deref().and_then(|c| {
        let needle = "@domain ";
        c.find(needle).map(|i| {
            let rest = &c[i + needle.len()..];
            // Take up to the first whitespace, full-width comma, or period.
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '\u{FF0C}' || c == '\u{3002}')
                .unwrap_or(rest.len());
            rest[..end].trim()
        })
    });

    // -- Editorial confidence. Rule-level annotation wins (from
    // assets/ruleset.json editorial_confidence); else heuristics on rule type /
    // severity / anchor_match / context_clues.
    let editorial_confidence = issue
        .editorial_confidence
        .unwrap_or_else(|| heuristic_editorial_confidence(issue));

    // -- False-friend detection. Confusable rules are the canonical false
    // friends. Rule-tagged low-confidence terms are also surfaced as false
    // friends because their surface form is shared across regions with
    // divergent senses.
    let is_false_friend = matches!(issue.rule_type, IssueType::Confusable)
        || matches!(editorial_confidence, EditorialConfidence::Low)
            && issue.editorial_confidence.is_some();

    // -- Auto-fix safety + review need. Invariant: low confidence forces
    // auto_fix_safe=false + needs_review=true. Otherwise punctuation / case /
    // variant / typo hits with a single suggestion are auto-fix safe.
    let single_unambiguous = issue.suggestions.len() == 1
        && matches!(
            issue.rule_type,
            IssueType::Punctuation | IssueType::Case | IssueType::Variant | IssueType::Typo
        );

    let auto_fix_safe =
        !matches!(editorial_confidence, EditorialConfidence::Low) && single_unambiguous;

    let needs_review = matches!(editorial_confidence, EditorialConfidence::Low)
        || issue.suggestions.len() > 1
        || matches!(
            issue.rule_type,
            IssueType::Translationese | IssueType::AiStyle | IssueType::Grammar
        );

    let rationale = build_explanation(issue)
        .unwrap_or_else(|| format!("'{}' flagged by {:?} rule.", issue.found, issue.rule_type));

    ExplainMeta {
        rationale,
        domain,
        is_false_friend,
        auto_fix_safe,
        needs_review,
        editorial_confidence,
    }
}

/// Anchor provenance for compact mode (owned).
#[derive(Serialize)]
struct AnchorProvenanceOwned {
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_en: Option<String>,
    anchor_match: Option<bool>,
}

/// Issue with optional explain/stats annotations, serialized directly without
/// intermediate Value allocation.
#[derive(Serialize)]
struct AnnotatedIssue<'a> {
    #[serde(flatten)]
    issue: &'a Issue,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_provenance: Option<AnchorProvenance<'a>>,
    /// Structured per-issue explain metadata.  Present only in
    /// explain mode.  Carries domain, false-friend flag, auto-fix
    /// safety, review burden, and editorial confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    explain_meta: Option<ExplainMeta<'a>>,
    /// Resolution tier: which pipeline stage authored this issue's resolution.
    /// Present only when `include_stats` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ResolutionTier>,
}

/// Issues list: either plain references or annotated wrappers.
#[derive(Serialize)]
#[serde(untagged)]
enum IssuesList<'a> {
    Plain(&'a [Issue]),
    Annotated(Vec<AnnotatedIssue<'a>>),
}

/// Location in compact mode.
#[derive(Serialize)]
struct CompactLocation {
    line: usize,
    col: usize,
}

/// Calibration stats from translation verification.
#[cfg(feature = "translate")]
#[derive(Serialize)]
struct VerifyStats {
    api_ok: bool,
    matched: usize,
    unmatched: usize,
    no_english: usize,
}

/// Full-detail tool response (serialized directly, no intermediate Value).
#[derive(Serialize)]
struct FullOutput<'a> {
    accepted: bool,
    text: &'a str,
    issues: IssuesList<'a>,
    applied_fixes: usize,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    political_stance: &'a str,
    detected_script: &'a str,
    s2t_applied: bool,
    trace: &'a Trace,
    /// Present when fix_output != "full": indicates the `text` field contains
    /// a diff representation (search_replace blocks or patch JSON) instead of
    /// the full corrected text.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_output_mode: Option<&'a str>,
    #[cfg(feature = "translate")]
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<VerifyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
    /// Document-wide consistency report.  Present only when the
    /// caller passed `consistency: true` AND mixed regional usage
    /// (both `線程` and `執行緒`, etc.) is detected in the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    consistency: Option<&'a crate::engine::consistency::ConsistencyReport>,
}

/// Compact tool response (serialized directly, no intermediate Value).
#[derive(Serialize)]
struct CompactOutput<'a> {
    accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    issues: Vec<CompactGroup>,
    applied_fixes: usize,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    detected_script: &'a str,
    s2t_applied: bool,
    /// Present when fix_output != "full": indicates the `text` field contains
    /// a diff representation instead of the full corrected text.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_output_mode: Option<&'a str>,
    #[cfg(feature = "translate")]
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<VerifyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
}

/// Summary-only output: issue counts + AI signature, no individual issues or
/// text.
#[derive(Serialize)]
struct SummaryOutput<'a> {
    accepted: bool,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    detected_script: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
}

/// Count issues by severity.
fn build_summary(
    issues: &[Issue],
    tm_suppressed: usize,
    sampling_stats: SamplingStats,
    disambig_stats: &DisambigStats,
) -> IssueSummary {
    let mut s = IssueSummary {
        errors: 0,
        warnings: 0,
        info: 0,
        tm_suppressed,
        tier2_resolved: disambig_stats.tier2_resolved,
        tier2_gray_zone: disambig_stats.gray_zone,
        sampling_used: sampling_stats.used,
        sampling_skipped: sampling_stats.skipped,
    };
    for issue in issues {
        match issue.severity {
            Severity::Error => s.errors += 1,
            Severity::Warning => s.warnings += 1,
            Severity::Info => s.info += 1,
        }
    }
    s
}

/// Parameters for build_check_output.
struct CheckOutputParams<'a> {
    result_text: &'a str,
    issues: &'a [Issue],
    applied_fixes: usize,
    max_errors: Option<u64>,
    max_warnings: Option<u64>,
    profile: Profile,
    stance_name: &'a str,
    detected_script: &'a str,
    /// Whether S2T conversion was applied (input was Simplified Chinese).
    s2t_applied: bool,
    trace: &'a Trace,
    explain: bool,
    output_mode: OutputMode,
    has_fixes: bool,
    /// Fix output mode: full text, search/replace blocks, or patch array.
    fix_output: FixOutputMode,
    /// Original text before fixes (needed for search_replace and patch modes).
    original_text: &'a str,
    /// Applied fix records for patch/search_replace output.
    fix_records: &'a [crate::fixer::AppliedFix],
    #[cfg(feature = "translate")]
    calibrate_result: Option<crate::engine::translate::CalibrateResult>,
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    oral_density: Option<f32>,
    quality_flags: &'a [String],
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite style scorecard (or `None` when not requested).
    /// The caller computes this once per scan; build_check_output forwards
    /// it untouched into the chosen output mode.
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    /// Number of issues downgraded by translation memory.
    tm_suppressed: usize,
    /// Sampling budget usage statistics.
    sampling_stats: SamplingStats,
    /// Tier 2 disambiguation statistics.
    disambig_stats: DisambigStats,
    /// Token telemetry metrics (only when include_telemetry is true).
    telemetry: Option<TelemetryMetrics>,
    /// Whether to include per-issue resolution tier and summary_metrics.
    include_stats: bool,
    /// Document-wide consistency report.  Some only when the
    /// caller requested `consistency: true` AND mixed regional usage
    /// is detected.
    consistency: Option<&'a crate::engine::consistency::ConsistencyReport>,
}

/// Build telemetry metrics from accumulated counters.
/// `cache_counts` is (hits, misses) from the judgment cache.
fn build_telemetry(
    text: &str,
    scanner_hit_count: usize,
    disambig_stats: &DisambigStats,
    sampling_stats: &SamplingStats,
    bridge: Option<&&mut SamplingBridge<'_>>,
    applied_fixes: usize,
    cache_counts: (u64, u64),
) -> TelemetryMetrics {
    let (est_prompt_tokens, est_completion_tokens) = bridge
        .map(|b| (b.est_prompt_tokens, b.est_completion_tokens))
        .unwrap_or((0, 0));

    // ambiguous_terms: all terms that entered Tier 2 evaluation (resolved +
    // suppressed + gray_zone), not just those forwarded to Tier 3.
    let ambiguous_terms = (disambig_stats.tier2_resolved
        + disambig_stats.suppressed
        + disambig_stats.gray_zone) as u64;
    let t = TokenTelemetry {
        input_chars: text.chars().count() as u64,
        rule_hits: scanner_hit_count as u64,
        ambiguous_terms,
        tier2_resolved: disambig_stats.tier2_resolved as u64,
        llm_round_trips: sampling_stats.used as u64,
        final_fixes: applied_fixes as u64,
        prompt_tokens: est_prompt_tokens,
        completion_tokens: est_completion_tokens,
        cache_hits: cache_counts.0,
        cache_misses: cache_counts.1,
    };
    t.derive_metrics()
}

/// Build the unified zhtw JSON response and wrap it in a CallToolResult.
///
/// Both the lint-only and fix paths produce the same output shape; only the
/// concrete values differ. Compact mode omits text (in lint-only), trace,
/// byte offsets/lengths, and deduplicates repeated issues.
///
/// Serializes typed structs directly to avoid intermediate `serde_json::Value`
/// allocations. Uses compact JSON by default; set `ZHTW_PRETTY=1` env var
/// for indented output during debugging.
/// Inputs to [Server::run_scan_stage], which both `fix_mode` paths share.
/// Everything `tool_check`'s prologue settled, handed to whichever pipeline
/// runs. Both pipelines need nearly all of it, so this is one binding instead
/// of twenty parameters.
struct CheckRequest<'a> {
    /// Post-S2T text: what every stage below scans.
    text: &'a str,
    s2t_applied: bool,
    params: &'a CheckParams<'a>,
    ignore_set: &'a std::collections::HashSet<&'a str>,
    stance_name: &'static str,
    stage: ScanStageArgs<'a>,
    /// Judgment-cache counters as of the start of the request, so telemetry
    /// reports this request's share rather than the process total.
    cache_hits_before: u64,
    cache_misses_before: u64,
}

struct ScanStageArgs<'a> {
    text: &'a str,
    content_type: crate::engine::scan::ContentType,
    cfg: crate::rules::ruleset::ProfileConfig,
    profile: Profile,
    stance: Option<PoliticalStance>,
    /// True when the input was Simplified and got converted upstream, in
    /// which case the detected script is reported as such.
    s2t_converted: bool,
    #[cfg(feature = "translate")]
    verify: bool,
}

/// What the shared stage produces: the scan plus the issue list after
/// stance filtering, anchor calibration, Tier 2, Tier 3, and suppressions.
///
/// Translation memory is deliberately NOT applied here.  It is the one
/// step whose position differs between the two paths, so it stays at the
/// call sites where the difference is visible.
struct ScanStage {
    /// Exclusion ranges, kept so the fix path can hand them to the fixer
    /// and remap them instead of rebuilding.
    excluded: Vec<crate::engine::excluded::ByteRange>,
    /// The scan output with `issues` drained into the field below.
    scan: crate::engine::scan::ScanOutput,
    issues: Vec<Issue>,
    detected_script: &'static str,
    /// Issue count straight out of the scanner, before any filtering.
    scanner_hit_count: usize,
    disambig_stats: crate::engine::disambig::DisambigStats,
    sampling_stats: SamplingStats,
    #[cfg(feature = "translate")]
    calibrate_result: Option<crate::engine::translate::CalibrateResult>,
}

/// Capability flags as parsed from the `zhtw` tool arguments, before they
/// are folded into a [ProfileConfig].  `None` means "inherit the profile
/// default"; `Some` is an explicit caller override.
struct CheckFlags<'a> {
    relaxed: bool,
    exempt_blockquotes: bool,
    stance: Option<PoliticalStance>,
    detect_style: bool,
    detect_ai: Option<bool>,
    detect_translationese: Option<bool>,
    translationese_domain: Option<&'a str>,
    document_genre: Option<&'a str>,
    register: Option<&'a str>,
    ai_threshold: Option<&'a str>,
    rhythm: bool,
}

/// Fold the profile base and the caller's capability flags into one
/// config.  Separate from `tool_check` because it is pure argument
/// resolution: every rejection it can produce is a bad parameter value,
/// and none of it depends on the text being checked.
fn build_check_config(
    profile: Profile,
    flags: &CheckFlags<'_>,
) -> ParamResult<crate::rules::ruleset::ProfileConfig> {
    let mut cfg = profile.config();
    if flags.relaxed {
        cfg = cfg.with_relaxed();
    }
    if flags.exempt_blockquotes {
        cfg = cfg.with_exempt_blockquotes(true);
    }
    if let Some(st) = flags.stance {
        cfg = cfg.with_stance(st);
    }

    // detect_style mirrors the CLI shorthand: it always computes the full
    // three-axis scorecard, regardless of explicit per-axis disables.
    if flags.detect_style {
        cfg.translationese_detection = true;
    } else if let Some(b) = flags.detect_translationese {
        cfg.translationese_detection = b;
    }
    if let Some(domain_str) = flags.translationese_domain {
        match crate::engine::translationese_score::TranslationeseDomain::from_str_strict(domain_str)
        {
            Some(d) => cfg.translationese_domain = d,
            None => {
                return Err(enum_param_error("translationese_domain", domain_str));
            }
        }
    }
    if let Some(genre_str) = flags.document_genre {
        match AttributionGenre::from_str_strict(genre_str) {
            Some(genre) => cfg.document_genre = genre,
            None => return Err(enum_param_error("document_genre", genre_str)),
        }
    }
    if let Some(register_str) = flags.register {
        match crate::rules::ruleset::RegisterMode::from_str_strict(register_str) {
            Some(mode) => cfg = cfg.with_register(mode),
            None => return Err(enum_param_error("register", register_str)),
        }
    }
    if flags.rhythm {
        cfg = cfg.with_rhythm(true);
    }

    // Resolve effective AI detection: explicit arg wins over profile default.
    // All four AI sub-flags move as a unit: enabling detection turns them all
    // on, disabling turns them all off.
    let detect_ai = if flags.detect_style {
        true
    } else {
        flags.detect_ai.unwrap_or(cfg.ai_filler_detection)
    };
    cfg.ai_filler_detection = detect_ai;
    cfg.ai_semantic_safety = detect_ai;
    cfg.ai_density_detection = detect_ai;
    cfg.ai_structural_patterns = detect_ai;
    if detect_ai {
        // Apply threshold level: low=0.5 (sensitive), medium=1.0, high=1.5
        // (conservative).
        cfg.ai_threshold_multiplier = match flags.ai_threshold {
            Some("low") => 0.5,
            Some("medium") | None => 1.0,
            Some("high") => 1.5,
            Some(other) => {
                return Err(enum_param_error("ai_threshold", other));
            }
        };
    }
    Ok(cfg)
}

/// Build the composite three-axis scorecard when the caller explicitly
/// opts in via `detect_style` (CLI: `--detect-style` flag, MCP:
/// `detect_style: true` argument).  Pure aggregation, returns `None`
/// when not requested so the standard payload stays lean.
fn style_scorecard_for(
    detect_style: bool,
    ai: Option<&crate::engine::ai_score::AiSignatureReport>,
    trans: Option<&crate::engine::translationese_score::TranslationeseReport>,
    issues: &[Issue],
    text: &str,
) -> Option<crate::engine::style_score::StyleScorecard> {
    if !detect_style {
        return None;
    }
    Some(crate::engine::style_score::StyleScorecard::build(
        ai,
        trans,
        issues,
        text.chars().count(),
    ))
}

fn build_check_output(params: &CheckOutputParams<'_>) -> CallToolResult {
    let summary = build_summary(
        params.issues,
        params.tm_suppressed,
        params.sampling_stats,
        &params.disambig_stats,
    );

    let stats_metrics = if params.include_stats {
        Some(build_summary_metrics(
            params.issues,
            &params.sampling_stats,
            params.telemetry.as_ref(),
        ))
    } else {
        None
    };

    let max_err = params.max_errors.unwrap_or(0) as usize;
    let max_warn = params.max_warnings.unwrap_or(0) as usize;
    let gate_enabled = params.max_errors.is_some() || params.max_warnings.is_some();
    let accepted = params.max_errors.is_none_or(|_| summary.errors <= max_err)
        && params
            .max_warnings
            .is_none_or(|_| summary.warnings <= max_warn);

    let gate = GateInfo {
        enabled: gate_enabled,
        max_errors: max_err,
        residual_errors: summary.errors,
        max_warnings: max_warn,
        residual_warnings: summary.warnings,
    };

    #[cfg(feature = "translate")]
    let verify = params.calibrate_result.as_ref().map(|cr| VerifyStats {
        api_ok: cr.api_ok,
        matched: cr.matched,
        unmatched: cr.unmatched,
        no_english: cr.no_english,
    });

    // When fix_output is not Full and fixes were applied, replace the text
    // field with a diff representation to save output tokens.
    let diff_text: Option<String> = if params.has_fixes
        && params.fix_output != FixOutputMode::Full
        && !params.fix_records.is_empty()
    {
        Some(build_fix_diff(
            params.original_text,
            params.fix_records,
            params.fix_output,
        ))
    } else {
        None
    };
    let effective_text = diff_text.as_deref().unwrap_or(params.result_text);

    let fix_mode_label = if diff_text.is_some() {
        Some(params.fix_output.name())
    } else {
        None
    };
    let quality_flags = (!params.quality_flags.is_empty()).then_some(params.quality_flags);

    let serialize_result = match params.output_mode {
        OutputMode::Full => {
            let issues = build_issues_list(params.issues, params.explain, params.include_stats);
            let output = FullOutput {
                accepted,
                text: effective_text,
                issues,
                applied_fixes: params.applied_fixes,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                political_stance: params.stance_name,
                detected_script: params.detected_script,
                s2t_applied: params.s2t_applied,
                trace: params.trace,
                fix_output_mode: fix_mode_label,
                #[cfg(feature = "translate")]
                verify,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
                consistency: params.consistency,
            };
            serialize_output(&output)
        }
        OutputMode::Compact => {
            let issues = build_compact_groups(params.issues, params.explain, params.include_stats);
            let output = CompactOutput {
                accepted,
                text: if params.has_fixes {
                    Some(effective_text)
                } else {
                    None
                },
                issues,
                applied_fixes: params.applied_fixes,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                detected_script: params.detected_script,
                s2t_applied: params.s2t_applied,
                fix_output_mode: fix_mode_label,
                #[cfg(feature = "translate")]
                verify,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
            };
            serialize_output(&output)
        }
        OutputMode::Tabular => {
            let tsv = build_tabular_output(
                accepted,
                params.issues,
                params.applied_fixes,
                &summary,
                params.has_fixes,
                effective_text,
                params.explain,
                fix_mode_label,
            );
            Ok(tsv)
        }
        OutputMode::Summary => {
            let output = SummaryOutput {
                accepted,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                detected_script: params.detected_script,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
            };
            serialize_output(&output)
        }
    };

    match serialize_result {
        Ok(json_str) => {
            if accepted {
                tool_text(json_str)
            } else {
                tool_error(json_str)
            }
        }
        Err(e) => {
            tracing::error!("failed to serialize check output: {e}");
            tool_error("internal server error".into())
        }
    }
}

/// Serialize to compact JSON by default; pretty-print when `ZHTW_PRETTY=1`.
fn serialize_output(output: &impl serde::Serialize) -> serde_json::Result<String> {
    if std::env::var_os("ZHTW_PRETTY").is_some_and(|v| v == "1") {
        serde_json::to_string_pretty(output)
    } else {
        serde_json::to_string(output)
    }
}

/// Build issues list for full output mode: either plain references (no extra
/// fields) or annotated wrappers with explanation, anchor provenance, and/or
/// resolution tier.
fn build_issues_list<'a>(
    issues: &'a [Issue],
    explain: bool,
    include_stats: bool,
) -> IssuesList<'a> {
    if explain || include_stats {
        let annotated: Vec<AnnotatedIssue<'a>> = issues
            .iter()
            .map(|issue| {
                let explanation = if explain {
                    build_explanation(issue)
                } else {
                    None
                };
                let anchor_provenance = if explain && issue.anchor_match.is_some() {
                    Some(AnchorProvenance {
                        anchor_en: issue.english.as_deref(),
                        anchor_match: issue.anchor_match,
                    })
                } else {
                    None
                };
                let resolution = if include_stats {
                    Some(ResolutionTier::classify(issue))
                } else {
                    None
                };
                let explain_meta = if explain {
                    Some(derive_explain_meta(issue))
                } else {
                    None
                };
                AnnotatedIssue {
                    issue,
                    explanation,
                    anchor_provenance,
                    explain_meta,
                    resolution,
                }
            })
            .collect();
        IssuesList::Annotated(annotated)
    } else {
        IssuesList::Plain(issues)
    }
}

/// Build compact deduplicated issues array.
///
/// Groups issues by (found, rule_type, suggestions, severity) key. Each group
/// becomes one entry with count and locations. Serialized directly via
/// `#[derive(Serialize)]` on `CompactGroup`: no intermediate `Value` per
/// group.
fn build_compact_groups(issues: &[Issue], explain: bool, include_stats: bool) -> Vec<CompactGroup> {
    use std::collections::BTreeMap;

    // Key: (found, rule_type, suggestions_joined, severity,
    // resolution_tier_discriminant) Include severity so that sampling can
    // produce mixed-severity occurrences of the same term without silently
    // inheriting the first occurrence's level. When include_stats is true, also
    // partition by resolution tier so the per-group resolution field is
    // accurate. Uses shared IssueType::name() and Severity::name() from
    // ruleset.rs. We use BTreeMap for deterministic ordering.
    let mut groups: BTreeMap<(&str, &str, String, &str, u8), CompactGroup> = BTreeMap::new();

    for issue in issues {
        let rt = issue.rule_type.name();
        let sug_key = issue.suggestions.join("|");
        let sev_key = issue.severity.name();

        // Compute resolution tier once; reuse for both grouping key and field
        // value. Discriminant 0 when stats disabled (all group together);
        // distinct per-tier when enabled so the resolution field stays
        // accurate.
        let tier = if include_stats {
            Some(ResolutionTier::classify(issue))
        } else {
            None
        };
        let tier_disc = tier.map_or(0, |t| t as u8 + 1);
        let key = (issue.found.as_str(), rt, sug_key, sev_key, tier_disc);

        let group = groups.entry(key).or_insert_with(|| CompactGroup {
            found: issue.found.clone(),
            suggestions: issue.suggestions.to_vec(),
            suggested_rewrite: issue.suggested_rewrite.clone(),
            rule_type: rt.to_string(),
            severity: issue.severity.name().to_string(),
            context: issue.context.as_deref().map(str::to_string),
            english: issue.english.as_deref().map(str::to_string),
            explanation: if explain {
                build_explanation(issue)
            } else {
                None
            },
            anchor_provenance: if explain && issue.anchor_match.is_some() {
                Some(AnchorProvenanceOwned {
                    anchor_en: issue.english.as_deref().map(str::to_string),
                    anchor_match: issue.anchor_match,
                })
            } else {
                None
            },
            resolution: tier,
            count: 0,
            locations: Vec::new(),
        });
        group.count += 1;
        group.locations.push(CompactLocation {
            line: issue.line,
            col: issue.col,
        });
    }

    groups.into_values().collect()
}

/// Escape tab, newline, and carriage return in a TSV field to prevent
/// column/row injection.  Returns a borrowed reference when no escaping
/// is needed, avoiding allocation on the common path.
pub fn escape_tsv_field(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes()
        .any(|b| b == b'\\' || b == b'\t' || b == b'\n' || b == b'\r')
    {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Deduplicated issue group shared by MCP tabular output and CLI tabular
/// format.
///
/// Groups issues by (found, rule_type, suggestions, severity) key. Each group
/// stores shared fields once and collects per-occurrence locations.
pub struct IssueGroup {
    pub suggestions: Vec<String>,
    pub count: usize,
    pub locs: Vec<(usize, usize)>,
    pub explanation: Option<String>,
}

/// Issue grouping key: (found, rule_type, suggestions_joined, severity).
pub type IssueGroupKey<'a> = (&'a str, &'a str, String, &'a str);

/// Group issues by (found, rule_type, suggestions, severity) into a BTreeMap
/// for deterministic ordering. Optionally generates explanations per group.
pub fn group_issues<'a>(
    issues: &'a [Issue],
    explain: bool,
) -> std::collections::BTreeMap<IssueGroupKey<'a>, IssueGroup> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<IssueGroupKey<'a>, IssueGroup> = BTreeMap::new();
    for issue in issues {
        let rt = issue.rule_type.name();
        let sug_key = issue.suggestions.join("|");
        let sev = issue.severity.name();
        let key: IssueGroupKey<'a> = (issue.found.as_str(), rt, sug_key, sev);
        let entry = groups.entry(key).or_insert_with(|| IssueGroup {
            suggestions: issue.suggestions.to_vec(),
            count: 0,
            locs: Vec::new(),
            explanation: if explain {
                build_explanation(issue)
            } else {
                None
            },
        });
        entry.count += 1;
        entry.locs.push((issue.line, issue.col));
    }
    groups
}

/// Map full severity name to single-letter code for tabular output.
pub fn shorten_severity(sev: &str) -> &str {
    match sev {
        "error" => "E",
        "warning" => "W",
        "info" => "I",
        _ => sev,
    }
}

/// Map full issue type name to abbreviated code for tabular output.
pub fn shorten_type(rt: &str) -> &str {
    match rt {
        "political_coloring" => "pol",
        "cross_strait" => "cs",
        "typo" => "typo",
        "confusable" => "cf",
        "case" => "case",
        "punctuation" => "punc",
        "variant" => "v",
        "grammar" => "gram",
        _ => rt,
    }
}

/// Compress a list of (line, col) locations into a compact string.
///
/// When all locations share the same column, emits "L1,L4,L7:C" instead of
/// the verbose "1:C,4:C,7:C" form -- saves tokens on repeated issues.
pub fn compress_locations(locs: &[(usize, usize)]) -> String {
    use std::fmt::Write;
    if locs.is_empty() {
        return String::new();
    }
    if locs.len() == 1 {
        return format!("{}:{}", locs[0].0, locs[0].1);
    }
    // Check if all columns are identical.
    let first_col = locs[0].1;
    if locs.iter().all(|(_, c)| *c == first_col) {
        let mut s = String::new();
        for (i, (line, _)) in locs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{line}");
        }
        let _ = write!(s, ":{first_col}");
        s
    } else {
        locs.iter()
            .map(|(l, c)| format!("{l}:{c}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Build header-once TSV output for LLM-facing responses.
///
/// Eliminates JSON syntax tax: no repeated keys, braces, or quotes per issue.
/// Header row defines column semantics; data rows are tab-separated.
/// Achieves >=50% token reduction vs compact JSON on typical responses.
#[allow(clippy::too_many_arguments)]
fn build_tabular_output(
    accepted: bool,
    issues: &[Issue],
    applied_fixes: usize,
    summary: &IssueSummary,
    has_fixes: bool,
    result_text: &str,
    explain: bool,
    fix_output_mode: Option<&str>,
) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(256);

    // Meta line: key=value pairs, omitting zero-count fields to save tokens.
    let _ = write!(out, "#ok={}", accepted);
    if summary.errors > 0 {
        let _ = write!(out, "\terr={}", summary.errors);
    }
    if summary.warnings > 0 {
        let _ = write!(out, "\twarn={}", summary.warnings);
    }
    if summary.info > 0 {
        let _ = write!(out, "\tinfo={}", summary.info);
    }
    if applied_fixes > 0 {
        let _ = write!(out, "\tfix={}", applied_fixes);
    }
    if has_fixes {
        let _ = write!(out, "\ttxt={}", result_text.len());
    }
    if let Some(mode) = fix_output_mode {
        let _ = write!(out, "\tfix_fmt={mode}");
    }
    out.push('\n');

    let groups = group_issues(issues, explain);

    // Header row.
    if explain {
        out.push_str("found\tsug\ttype\tsev\tn\tloc\texpl\n");
    } else {
        out.push_str("found\tsug\ttype\tsev\tn\tloc\n");
    }

    // Data rows. Use abbreviated severity (E/W/I) and rule type codes
    // (cs/cf/v/pol/typo/punc/case/gram) to reduce token count. Escape
    // tab/newline in data fields to prevent TSV injection.
    for ((found, rt, _, sev), group) in &groups {
        let found_safe = escape_tsv_field(found);
        let suggestions_str = group
            .suggestions
            .iter()
            .map(|s| escape_tsv_field(s))
            .collect::<Vec<_>>()
            .join(",");

        // Map full group-key names to abbreviated codes directly, avoiding an
        // O(groups*issues) scan that could also mismatch when the same found
        // term appears in multiple groups.
        let short_rt = shorten_type(rt);
        let short_sev = shorten_severity(sev);

        // Compress locations: if all share the same column, emit "L1,L4,L7:C"
        // instead of "L1:C,L4:C,L7:C".
        let locs_str = compress_locations(&group.locs);

        let _ = write!(
            out,
            "{found_safe}\t{suggestions_str}\t{short_rt}\t{short_sev}\t{}\t{locs_str}",
            group.count,
        );
        if explain {
            out.push('\t');
            if let Some(expl) = &group.explanation {
                out.push_str(&escape_tsv_field(expl));
            }
        }
        out.push('\n');
    }

    // If fixes were applied, append the fixed text after a separator.
    if has_fixes {
        out.push_str("#text\n");
        out.push_str(result_text);
    }

    out
}

/// Build diff representation of fixes for token-efficient output.
///
/// For SearchReplace mode: emits <<<<<<< SEARCH / ======= REPLACE / >>>>>>> END
/// blocks that LLMs can parse reliably without byte arithmetic.
/// For Patch mode: emits a JSON patches array with byte offsets, sorted
/// descending by offset so clients can apply in order without index shifting.
fn build_fix_diff(
    original_text: &str,
    fix_records: &[crate::fixer::AppliedFix],
    mode: FixOutputMode,
) -> String {
    match mode {
        FixOutputMode::SearchReplace => {
            let mut out = String::with_capacity(fix_records.len() * 80);
            for fix in fix_records {
                // Safe slice: get() returns None if offset/end are out of
                // bounds or not on UTF-8 char boundaries.
                if let Some(found) = original_text.get(fix.offset..fix.offset + fix.old_len) {
                    out.push_str("<<<<<<< SEARCH\n");
                    out.push_str(found);
                    out.push_str("\n======= REPLACE\n");
                    out.push_str(&fix.replacement);
                    out.push_str("\n>>>>>>> END\n");
                }
            }
            out
        }
        FixOutputMode::Patch => {
            use std::fmt::Write;

            // TSV patch format: header-once, sorted descending by offset so
            // clients can apply in order without index shifting.
            let mut patches: Vec<(usize, usize, &str, &str)> = fix_records
                .iter()
                .filter_map(|fix| {
                    let found = original_text.get(fix.offset..fix.offset + fix.old_len)?;
                    Some((fix.offset, fix.old_len, found, fix.replacement.as_str()))
                })
                .collect();
            patches.sort_by_key(|p| std::cmp::Reverse(p.0));

            let mut out = String::with_capacity(patches.len() * 40);
            let _ = writeln!(out, "#patches={}", patches.len());
            out.push_str("offset\tlength\tfound\treplacement\n");
            for (offset, length, found, replacement) in &patches {
                let _ = writeln!(
                    out,
                    "{offset}\t{length}\t{}\t{}",
                    escape_tsv_field(found),
                    escape_tsv_field(replacement),
                );
            }
            out
        }
        FixOutputMode::Full => {
            // Should never reach here; caller guards.
            String::new()
        }
    }
}

/// Helper for compact mode issue grouping.
#[derive(Serialize)]
struct CompactGroup {
    found: String,
    suggestions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_rewrite: Option<String>,
    rule_type: String,
    severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    english: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_provenance: Option<AnchorProvenanceOwned>,
    /// Resolution tier for all issues in this group.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ResolutionTier>,
    count: usize,
    locations: Vec<CompactLocation>,
}

// Tool definitions (JSON Schema for zhtw)

/// The properties of the `zhtw` tool's input schema.
///
/// Built once and shared, so what the tool advertises and what it accepts are
/// the same list rather than two lists that have to be kept in step. They were
/// two, and the accepted one was spelled out twice more, once per `translate`
/// build, so adding a parameter meant editing three places and being told it
/// was unknown if you missed one.
fn input_schema() -> &'static std::sync::Arc<JsonObject> {
    static SCHEMA: std::sync::OnceLock<std::sync::Arc<JsonObject>> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = JsonObject::new();
        schema.insert("type".into(), json!("object"));
        schema.insert(
            "properties".into(),
            Value::Object(input_schema_properties().clone()),
        );
        schema.insert("required".into(), json!(["text"]));
        std::sync::Arc::new(schema)
    })
}

/// The parameter names the schema declares, which is what the tool accepts.
fn input_schema_properties() -> &'static JsonObject {
    static PROPS: std::sync::OnceLock<JsonObject> = std::sync::OnceLock::new();
    PROPS.get_or_init(|| {
        let mut props = serde_json::Map::new();
        props.insert("text".into(), json!({ "type": "string" }));
        props.insert(
            "fix_mode".into(),
            json!({
                "type": "string",
                "enum": ["none", "orthographic", "lexical_safe", "lexical_contextual"]
            }),
        );
        props.insert("max_errors".into(), json!({ "type": "integer" }));
        props.insert("max_warnings".into(), json!({ "type": "integer" }));
        props.insert("profile".into(), json!({
                "type": "string",
                "enum": ["base", "strict"],
                "description": "Norm strictness: 'base' (default) or 'strict' (full MoE with character variants)"
            }));
        props.insert("relaxed".into(), json!({
                "type": "boolean",
                "description": "Capability flag for software UI strings: disables colon enforcement, dunhao detection, grammar checks; uses en-dash for ranges"
            }));
        props.insert("exempt_blockquotes".into(), json!({
                "type": "boolean",
                "description": "Markdown only: exclude pulldown-cmark `Tag::BlockQuote` ranges from scanning.  Useful when a document quotes mainland-Chinese sources for illustrative purposes.  Off by default."
            }));
        props.insert(
            "content_type".into(),
            json!({
                "type": "string",
                "default": "plain",
                "enum": ["plain", "markdown", "markdown-scan-code", "yaml"]
            }),
        );
        props.insert(
            "political_stance".into(),
            json!({
                "type": "string",
                "enum": ["roc_centric", "international", "neutral"]
            }),
        );
        props.insert(
            "ignore_terms".into(),
            json!({
                "type": "array",
                "items": { "type": "string" }
            }),
        );
        props.insert("glossary".into(), json!({
                "type": "object",
                "description": "Project-level glossary.  `banned` terms always fire (project-wide truth, banned > TM); `proper_nouns` suppress matching issues; `preferred` chooses canonical TW form for the consistency report.",
                "properties": {
                    "banned": { "type": "array", "items": { "type": "string" } },
                    "preferred": { "type": "array", "items": { "type": "string" } },
                    "proper_nouns": { "type": "array", "items": { "type": "string" } },
                }
            }));
        props.insert("consistency".into(), json!({
                "type": "boolean",
                "description": "Emit a `consistency` block when both regional variants of one concept appear in the document (e.g. both 線程 and 執行緒).  Off by default."
            }));
        props.insert("explain".into(), json!({ "type": "boolean" }));
        props.insert("fix_output".into(), json!({
                "type": "string",
                "enum": ["full", "search_replace", "patch"],
                "description": "Fix output format: full text (default), search/replace blocks, or patch array with byte offsets"
            }));
        #[cfg(feature = "translate")]
        props.insert(
            "verify".into(),
            json!({
                "type": "boolean",
                "description": "Anchor-verify issues via Google Translate. \
Sends the sentences around each issue to Google. Off unless asked, and refused \
when the server has ZHTW_NO_NETWORK set."
            }),
        );
        props.insert("output".into(), json!({
                "type": "string",
                "enum": ["full", "compact", "tabular", "summary"],
                "description": "Output mode. 'summary' returns only issue counts + AI signature (no individual issues)"
            }));
        props.insert("detect_ai".into(), json!({
                "type": "boolean",
                "description": "Enable AI writing artifact detection (density + grammar patterns). Default: on. Set false to suppress AI filler findings."
            }));
        props.insert("detect_translationese".into(), json!({
                "type": "boolean",
                "description": "Enable translationese (翻譯腔 / 歐化) detection — Europeanized syntax and calques from the dewesternise checklist. Default: on. Orthogonal to detect_ai; reported separately."
            }));
        props.insert("detect_style".into(), json!({
                "type": "boolean",
                "description": "Composite style scorecard: emit `style_scorecard` with three orthogonal axes (ai, translationese, regional_density) plus top contributing issues. Default: false. Three scores never collapsed into a single number."
            }));
        props.insert("translationese_domain".into(), json!({
                "type": "string",
                "enum": ["general", "technical", "literary", "news"],
                "description": "Per-domain calibration for translationese scoring thresholds. 'technical' tolerates more passive voice and weak-verb nominalization; 'literary' is the strictest; 'news' favors active voice. Default: 'general'."
            }));
        props.insert("document_genre".into(), json!({
                "type": "string",
                "enum": ["casual", "technical", "financial"],
                "description": "How strictly the document is held to sourcing, for unsupported authority attributions. Requires detect_ai. Distinct from the register parameter, which is a property of the prose and suppresses findings; this one only selects advice and never suppresses. Never suggests an edit: casual prose is advised to name the source or drop the appeal, technical and financial prose that the claim needs a citation. Default: casual."
            }));
        props.insert("register".into(), json!({
                "type": "string",
                "enum": ["auto", "formal", "casual"],
                "description": "Register the document is written in. 'auto' (default) reads it off the text: a 公文 opens 敬啟者 and signs off 謹啟. 'formal' licenses the forms that register mandates, so 予以核准 and 因為…所以 stop being reported. Suppression only; never changes what is suggested for anything it does report."
            }));
        props.insert("rhythm".into(), json!({
                "type": "boolean",
                "description": "Advisory rhythm (氣口) checks: over-long sentences, consecutive sentences closing on the same particle, and a relaxed 定語堆疊 gate. Default: false. Advisory only, never applied by any fix tier."
            }));
        props.insert("ai_threshold".into(), json!({
                "type": "string",
                "enum": ["low", "medium", "high"],
                "description": "AI detection sensitivity: 'low' (sensitive, catches more), 'medium' (balanced), 'high' (conservative). Only effective with detect_ai=true"
            }));
        props.insert("include_telemetry".into(), json!({
                "type": "boolean",
                "description": "Include per-request token telemetry metrics in the response (LLM cost accounting)"
            }));
        props.insert("include_stats".into(), json!({
                "type": "boolean",
                "description": "Include per-issue resolution tier and session-level summary_metrics (deterministic/heuristic/llm_judged/unresolved counts, confidence distribution)"
            }));
        props
    })
}

fn tool_definitions() -> Vec<Tool> {
    // Cloning the Arc, not the schema: the value is identical on every listing,
    // and deep-copying a dozen nested property objects to produce it again is
    // work with no result.
    let input_schema = input_schema().clone();

    vec![Tool::new(
        "zhtw",
        "Lint/fix/gate zh-TW text. Auto-converts Simplified Chinese to Traditional before applying rules. Use verify=true to calibrate issues via Google Translate anchor matching.",
        input_schema,
    )
    .with_annotations(ToolAnnotations::new().read_only(true).idempotent(true))]
}

/// The tools this server exposes.
///
/// The lists below all say the same thing about caching, and say it because
/// they have to: `ttlMs` and `cacheScope` are required of a cacheable result
/// from 2026-07-28 on and the SDK leaves both unset. Zero and private is the
/// honest answer here, since the ruleset is fixed for the process but a
/// restart with different overrides or packs changes these lists and nothing
/// would tell the client.
pub(crate) fn list_tools() -> ListToolsResult {
    ListToolsResult::with_all_items(tool_definitions())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn list_resources() -> ListResourcesResult {
    resources::list_resources()
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

/// No resource templates: this server exposes two fixed URIs and no patterns.
pub(crate) fn list_resource_templates() -> ListResourceTemplatesResult {
    ListResourceTemplatesResult::with_all_items(Vec::new())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn list_prompts() -> ListPromptsResult {
    ListPromptsResult::with_all_items(prompts::list_prompts())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn get_prompt(
    name: &str,
    arguments: &std::collections::HashMap<String, String>,
) -> ParamResult<GetPromptResult> {
    prompts::get_prompt(name, arguments)
        .ok_or_else(|| ErrorData::invalid_params(format!("unknown prompt: {name}"), None))
}

impl Catalog {
    /// Read one resource. Needs the ruleset, so it takes the catalogue rather
    /// than the server, and therefore takes no lock.
    pub(crate) fn read_resource(&self, uri: &str) -> ParamResult<ReadResourceResult> {
        resources::read_resource(uri, self.scanner.spelling_rules(), &self.ambiguous_dict)
            .map(|result| {
                // Not cacheable, for the reason given on list_tools.
                result.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
            })
            // resource_not_found rather than invalid_params: the URI is
            // well-formed, it just names nothing. The SDK maps this to -32002,
            // and upgrades it to -32602 for a peer on 2026-07-28 or newer, per
            // SEP-2164, so each revision gets the code it expects.
            .ok_or_else(|| {
                ErrorData::resource_not_found(format!("unknown resource URI: {uri}"), None)
            })
    }
}

/// What a scan concluded about an issue, kept across a fix so the re-scan
/// does not have to conclude it again.
///
/// Tier 2 and Tier 3 reach these by calibration and by asking the client, and
/// neither runs on the re-scan: without carrying them over, a fixed document
/// reports its remaining issues stripped of everything that was learned.
struct PreservedState {
    term: String,
    orig_offset: usize,
    length: usize,
    english: Option<Arc<str>>,
    severity: Severity,
    anchor_match: Option<bool>,
    context: Option<Arc<str>>,
    suggestions: Vec<String>,
}

fn snapshot_states(issues: &[Issue]) -> Vec<PreservedState> {
    issues
        .iter()
        .map(|i| PreservedState {
            term: i.found.clone(),
            orig_offset: i.offset,
            length: i.length,
            english: i.english.clone(),
            severity: i.severity,
            anchor_match: i.anchor_match,
            context: i.context.clone(),
            suggestions: i.suggestions.to_vec(),
        })
        .collect()
}

/// Put each preserved judgment back on the issue it belongs to.
///
/// The fix moves text, so the offsets a snapshot was taken at no longer
/// address the same place. Offsets are remapped once and indexed, rather than
/// remapped per issue, and a match has to agree on term, length and anchor as
/// well as offset: two issues can land on one offset after a fix, and giving
/// one of them the other's judgment is worse than giving it none.
fn restore_preserved_states(
    issues: &mut [Issue],
    preserved: &[PreservedState],
    applied: &[crate::fixer::AppliedFix],
) {
    use rustc_hash::FxHashMap;
    let mut by_offset: FxHashMap<usize, Vec<usize>> =
        FxHashMap::with_capacity_and_hasher(preserved.len(), Default::default());
    for (idx, state) in preserved.iter().enumerate() {
        by_offset
            .entry(remap_to_post_fix(state.orig_offset, applied))
            .or_default()
            .push(idx);
    }

    for issue in issues {
        let Some(candidates) = by_offset.get(&issue.offset) else {
            continue;
        };
        let matched = candidates.iter().find(|&&idx| {
            let s = &preserved[idx];
            s.term == issue.found && s.length == issue.length && s.english == issue.english
        });
        if let Some(&idx) = matched {
            let state = &preserved[idx];
            issue.severity = state.severity;
            issue.anchor_match = state.anchor_match;
            issue.context = state.context.clone();
            issue.suggestions = state.suggestions.clone().into();
            issue.refresh_suggested_rewrite();
        }
    }
}

/// A tool-level error: the call succeeded at the protocol layer and failed at
/// the tool layer, which is what lets a client show it rather than fail.
fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// One text block, which is the only shape this tool returns.
///
/// `isError` is cleared rather than sent as `false`: it was absent on success
/// before the SDK landed, and a client testing for the key's presence rather
/// than its value would read the explicit `false` as a failure.
fn tool_text(text: String) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.is_error = None;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ruleset::Tier2Outcome;
    use rmcp::model::ErrorCode;

    /// Tool annotations must serialize with the MCP-spec `*Hint` wire names;
    /// any other spelling is silently dropped by spec-compliant clients.
    #[test]
    fn tool_annotations_use_spec_hint_wire_names() {
        let defs = serde_json::to_value(tool_definitions()).unwrap();
        let ann = &defs[0]["annotations"];
        assert_eq!(ann["idempotentHint"], true);
        assert_eq!(ann["readOnlyHint"], true);
        // Bare (non-Hint) spellings must not appear on the wire.
        assert!(ann.get("idempotent").is_none());
        assert!(ann.get("readOnly").is_none());
        assert!(ann.get("destructive").is_none());
    }

    /// High confidence: cross_strait without context_clues, single
    /// suggestion.  Auto-fix safety still gated on rule_type being one of
    /// the unambiguous classes (Punctuation/Case/Variant/Typo); a plain
    /// CrossStrait keeps `auto_fix_safe=false` because the choice between
    /// suggestions is editorial.
    #[test]
    fn explain_meta_high_confidence_for_unambiguous_cross_strait() {
        let mut issue = Issue::new(
            0,
            6,
            "線程",
            vec!["執行緒".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(std::sync::Arc::from("thread"));
        let meta = derive_explain_meta(&issue);
        assert!(matches!(
            meta.editorial_confidence,
            EditorialConfidence::High
        ));
        assert!(!meta.is_false_friend);
        assert!(!meta.needs_review);
    }

    /// Rule-tagged low confidence (e.g. `優化`, `算法`, `場景`
    /// in `assets/ruleset.json`) surfaces as `low` so reviewers know
    /// they are editorial preference, not binary error.  Invariant:
    /// low ⇒ auto_fix_safe=false AND needs_review=true.
    #[test]
    fn explain_meta_low_confidence_for_rule_tagged_boundary_terms() {
        for boundary in &["優化", "算法", "場景"] {
            let mut issue = Issue::new(
                0,
                boundary.len(),
                *boundary,
                vec!["演算法".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            );
            issue.editorial_confidence = Some(EditorialConfidence::Low);
            let meta = derive_explain_meta(&issue);
            assert!(
                matches!(meta.editorial_confidence, EditorialConfidence::Low),
                "rule-tagged {boundary} must be low confidence",
            );
            assert!(
                !meta.auto_fix_safe,
                "rule-tagged {boundary}: low ⇒ !auto_fix_safe"
            );
            assert!(
                meta.needs_review,
                "rule-tagged {boundary}: low ⇒ needs_review"
            );
            assert!(
                meta.is_false_friend,
                "rule-tagged {boundary}: marked false friend"
            );
        }
    }

    /// `@domain X` extraction populates the `domain` field.
    #[test]
    fn explain_meta_extracts_domain_from_context() {
        let mut issue = Issue::new(
            0,
            6,
            "用戶",
            vec!["使用者".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.context = Some(std::sync::Arc::from("@domain IT。其他註解"));
        let meta = derive_explain_meta(&issue);
        assert_eq!(meta.domain, Some("IT"));
    }

    /// Translationese / AiStyle / Grammar always demand review.
    #[test]
    fn explain_meta_translationese_marks_low_confidence() {
        let issue = Issue::new(
            0,
            3,
            "被",
            vec!["主動句".into()],
            IssueType::Translationese,
            Severity::Info,
        );
        let meta = derive_explain_meta(&issue);
        assert!(matches!(
            meta.editorial_confidence,
            EditorialConfidence::Low
        ));
        assert!(!meta.auto_fix_safe);
        assert!(meta.needs_review);
    }

    /// `parse_glossary` extracts banned/preferred/proper_nouns
    /// from the tool args object.
    #[test]
    fn parse_glossary_extracts_three_lists() {
        let args = serde_json::json!({
            "glossary": {
                "banned": ["線程", "內存"],
                "preferred": ["執行緒"],
                "proper_nouns": ["TSMC"],
            }
        });
        let g = parse_glossary(&args);
        assert_eq!(g.banned, vec!["線程".to_string(), "內存".to_string()]);
        assert_eq!(g.preferred, vec!["執行緒".to_string()]);
        assert_eq!(g.proper_nouns, vec!["TSMC".to_string()]);
    }

    #[test]
    fn parse_glossary_missing_object_returns_empty() {
        let args = serde_json::json!({});
        let g = parse_glossary(&args);
        assert!(g.is_empty());
    }

    #[test]
    fn parse_glossary_partial_fields_default_to_empty() {
        let args = serde_json::json!({"glossary": {"banned": ["X"]}});
        let g = parse_glossary(&args);
        assert_eq!(g.banned, vec!["X".to_string()]);
        assert!(g.preferred.is_empty());
        assert!(g.proper_nouns.is_empty());
    }

    /// Punctuation with single suggestion is auto-fix safe.
    #[test]
    fn explain_meta_punctuation_is_auto_fix_safe() {
        let issue = Issue::new(
            0,
            1,
            ",",
            vec!["，".into()],
            IssueType::Punctuation,
            Severity::Warning,
        );
        let meta = derive_explain_meta(&issue);
        assert!(meta.auto_fix_safe);
        assert!(!meta.needs_review);
    }

    #[test]
    fn issue_summary_omits_zero_sampling_fields() {
        let summary = IssueSummary {
            errors: 1,
            warnings: 2,
            info: 0,
            tm_suppressed: 0,
            tier2_resolved: 0,
            tier2_gray_zone: 0,
            sampling_used: 0,
            sampling_skipped: 0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("sampling_used"));
        assert!(!json.contains("sampling_skipped"));
        assert!(!json.contains("tm_suppressed"));
    }

    #[test]
    fn issue_summary_includes_nonzero_sampling_fields() {
        let summary = IssueSummary {
            errors: 0,
            warnings: 3,
            info: 1,
            tm_suppressed: 0,
            tier2_resolved: 0,
            tier2_gray_zone: 0,
            sampling_used: 2,
            sampling_skipped: 5,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["sampling_used"], 2);
        assert_eq!(parsed["sampling_skipped"], 5);
        // tm_suppressed still omitted when zero.
        assert!(parsed.get("tm_suppressed").is_none());
    }

    #[test]
    fn build_summary_threads_sampling_stats() {
        let issues = vec![
            Issue::new(0, 3, "foo", vec![], IssueType::CrossStrait, Severity::Error),
            Issue::new(
                3,
                3,
                "bar",
                vec![],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
        ];
        let stats = SamplingStats {
            used: 3,
            skipped: 7,
        };
        let disambig = DisambigStats::default();
        let summary = build_summary(&issues, 1, stats, &disambig);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.tm_suppressed, 1);
        assert_eq!(summary.sampling_used, 3);
        assert_eq!(summary.sampling_skipped, 7);
    }

    #[test]
    fn build_explanation_for_spaced_acronym_is_not_duplicate_text() {
        let issue = Issue::new(
            0,
            5,
            "C P U",
            vec!["CPU".into()],
            IssueType::Repetition,
            Severity::Info,
        );
        let explanation = build_explanation(&issue).expect("explanation");
        assert!(explanation.contains("CPU"));
        assert!(explanation.contains("transcription artifact"));
        assert!(!explanation.contains("consecutive duplicate"));
    }

    #[test]
    fn build_explanation_for_translationese_does_not_duplicate_context() {
        // Regression: the main Translationese arm already appends the context,
        // so the shared "Context:" tail must be suppressed for this issue type
        // or the narrative gets repeated.
        let issue = Issue::new(
            0,
            3,
            "透過",
            vec!["藉由".into(), "經由".into()],
            IssueType::Translationese,
            Severity::Info,
        )
        .with_context("abstract-means calque; prefer 藉由");
        let explanation = build_explanation(&issue).expect("explanation");
        assert_eq!(
            explanation.matches("abstract-means calque").count(),
            1,
            "context must appear exactly once: {explanation}"
        );
        assert!(explanation.contains("Suggested rewrite"));
    }

    #[test]
    fn ai_style_explanation_marks_a_single_replacement_as_a_rewrite() {
        let issue = Issue::new(
            0,
            "被廣泛使用".len(),
            "被廣泛使用",
            vec!["廣泛使用".into()],
            IssueType::AiStyle,
            Severity::Info,
        );
        let explanation = build_explanation(&issue).expect("explanation");
        assert!(explanation.contains("Suggested rewrite: 廣泛使用"));
    }

    #[test]
    fn build_explanation_for_repetition_keeps_duplicate_text() {
        let issue = Issue::new(
            0,
            6,
            "cache cache",
            vec!["cache".into()],
            IssueType::Repetition,
            Severity::Info,
        );
        let explanation = build_explanation(&issue).expect("explanation");
        assert!(explanation.contains("consecutive duplicate"));
    }

    #[test]
    fn resolution_tier_classify_deterministic() {
        let issue = Issue::new(0, 3, "foo", vec![], IssueType::Punctuation, Severity::Error);
        assert_eq!(
            ResolutionTier::classify(&issue),
            ResolutionTier::Deterministic
        );
    }

    #[test]
    fn resolution_tier_classify_heuristic() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::Resolved;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Heuristic);
    }

    #[test]
    fn resolution_tier_classify_llm_judged() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::GrayZone;
        issue.llm_judged = true;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::LlmJudged);
    }

    #[test]
    fn resolution_tier_classify_unresolved_gray_zone() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::GrayZone;
        // No LLM annotation: stays unresolved.
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Unresolved);
    }

    #[test]
    fn resolution_tier_classify_suppressed() {
        let mut issue = Issue::new(0, 3, "foo", vec![], IssueType::CrossStrait, Severity::Info);
        issue.tier2_outcome = Tier2Outcome::Suppressed;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Unresolved);
    }

    #[test]
    fn summary_metrics_counts_tiers() {
        let mut issues = vec![
            Issue::new(0, 1, "a", vec![], IssueType::Punctuation, Severity::Error),
            Issue::new(
                1,
                1,
                "b",
                vec!["c".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
            Issue::new(
                2,
                1,
                "d",
                vec!["e".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
            Issue::new(
                3,
                1,
                "f",
                vec!["g".into()],
                IssueType::Confusable,
                Severity::Warning,
            ),
        ];
        issues[1].tier2_outcome = Tier2Outcome::Resolved;
        issues[2].tier2_outcome = Tier2Outcome::GrayZone;
        issues[2].llm_judged = true;
        issues[3].tier2_outcome = Tier2Outcome::Suppressed;

        let stats = SamplingStats {
            used: 1,
            skipped: 0,
        };
        let metrics = build_summary_metrics(&issues, &stats, None);

        assert_eq!(metrics.deterministic_fixes, 1);
        assert_eq!(metrics.heuristic_fixes, 1);
        assert_eq!(metrics.llm_judged_fixes, 1);
        assert_eq!(metrics.unresolved, 1);
        assert_eq!(metrics.llm_calls, 1);
        assert_eq!(metrics.llm_tokens, 0);
        assert_eq!(metrics.confidence_distribution.high, 2);
        assert_eq!(metrics.confidence_distribution.medium, 1);
        assert_eq!(metrics.confidence_distribution.low, 1);
    }

    #[test]
    fn summary_metrics_omitted_without_flag() {
        let issues = vec![Issue::new(
            0,
            3,
            "foo",
            vec![],
            IssueType::CrossStrait,
            Severity::Error,
        )];
        let disambig = DisambigStats::default();
        let summary = build_summary(&issues, 0, SamplingStats::default(), &disambig);
        let output = SummaryOutput {
            accepted: true,
            summary: &summary,
            gate: GateInfo {
                enabled: false,
                max_errors: 0,
                residual_errors: 1,
                max_warnings: 0,
                residual_warnings: 0,
            },
            profile: "base",
            detected_script: "traditional",
            coverage: None,
            oral_density: None,
            quality_flags: None,
            ai_signature: None,
            translationese_signature: None,
            style_scorecard: None,
            telemetry: None,
            summary_metrics: None,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(!json.contains("summary_metrics"));
        assert!(!json.contains("deterministic_fixes"));
    }

    #[test]
    fn resolution_tier_serializes_snake_case() {
        let tier = ResolutionTier::LlmJudged;
        let json = serde_json::to_value(tier).unwrap();
        assert_eq!(json, serde_json::json!("llm_judged"));
    }

    #[test]
    fn build_summary_excludes_hard_anchors_from_tier2_resolved() {
        let issues = vec![Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        )];
        let disambig = DisambigStats {
            hard_anchor: 2,
            tier2_resolved: 3,
            suppressed: 0,
            gray_zone: 1,
            not_eligible: 0,
        };

        let summary = build_summary(&issues, 0, SamplingStats::default(), &disambig);
        assert_eq!(summary.tier2_resolved, 3);
        assert_eq!(summary.tier2_gray_zone, 1);
    }

    /// A server past the handshake.
    ///
    /// The handshake itself belongs to the SDK adapter now, so this records
    /// the negotiated client state the same way `SdkServer::initialize` does.
    fn make_initialized_server() -> (Server, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Server::new(
            OverrideStore::open(&dir.path().join("overrides.json")).unwrap(),
            SuppressionStore::open(&dir.path().join("suppressions.json")).unwrap(),
            PackStore::new(dir.path().join("packs")),
            vec![],
            None,
        )
        .unwrap();
        server.set_client("test".into());
        (server, dir)
    }

    /// Drive one `zhtw` call the way the adapter does.
    fn call_zhtw(server: &mut Server, args: serde_json::Value) -> ParamResult<CallToolResult> {
        server.call_tool("zhtw", &args, None, None)
    }

    /// The tool's JSON payload, asserting it reported success on the way.
    fn assert_tool_success(resp: &ParamResult<CallToolResult>) -> serde_json::Value {
        let result = resp.as_ref().expect("tool call succeeded");
        assert!(result.is_error.is_none());
        let text = result.content[0]
            .as_text()
            .expect("the tool returns one text block");
        serde_json::from_str(&text.text).unwrap()
    }

    /// The error of a call that was expected to fail.
    fn assert_tool_error(resp: ParamResult<CallToolResult>) -> ErrorData {
        resp.expect_err("tool call failed")
    }

    #[test]
    fn tools_call_simplified_input_builds_s2t_lazily() {
        // s2t is built lazily on first Simplified input; this exercises that
        // path end-to-end (get_or_init + convert) and confirms the flag is set.
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": "软件测试" }));
        let output = assert_tool_success(&resp);
        assert_eq!(output["s2t_applied"], true);
        // A second call reuses the already-built converter (no re-init panic).
        let resp2 = call_zhtw(&mut server, serde_json::json!({ "text": "软件" }));
        assert_eq!(assert_tool_success(&resp2)["s2t_applied"], true);
    }

    #[test]
    fn tools_call_arguments_not_object() {
        let (mut server, _dir) = make_initialized_server();
        let resp = server.call_tool("zhtw", &serde_json::json!("not_an_object"), None, None);
        let err = resp.expect_err("a non-object arguments value is a parameter error");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn tools_call_text_exceeds_max_size() {
        let (mut server, _dir) = make_initialized_server();
        let big_text = "あ".repeat(Server::MAX_TEXT_BYTES + 1);
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": big_text }));
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn tools_call_detect_style_rejected_for_tabular_output() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "正確的軟體",
                "output": "tabular",
                "detect_style": true
            }),
        );
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("detect_style"));
    }

    #[test]
    fn tools_call_empty_text_input() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": "" }));
        assert!(resp.is_ok());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], false);
        assert_eq!(output["text"], "");
    }

    #[test]
    fn a_declared_default_is_the_one_the_parser_applies() {
        // Generic over the schema rather than naming the field, the same way
        // the accepted-values check is: a '"default"' a client reads and a
        // default the parser applies are two statements of one fact, and the
        // drift between them is silent.
        for (field, prop) in input_schema_properties() {
            let Some(default) = prop.get("default").and_then(Value::as_str) else {
                continue;
            };
            assert!(
                accepted_values(field).contains(&default),
                "{field} declares a default the schema does not accept: {default:?}"
            );
            assert_eq!(
                parse_content_type(&json!({})).ok(),
                ContentType::from_name(default),
                "{field}: the parser's default and the schema's disagree"
            );
        }
    }

    #[test]
    fn omitting_content_type_leaves_the_text_plain() {
        // A tool call has no file name to infer from, and treating unmarked
        // text as Markdown silently skips anything in it that looks like a
        // fence. Nothing else pins this default.
        assert_eq!(
            parse_content_type(&json!({})).expect("no content_type is allowed"),
            ContentType::Plain
        );
        assert_eq!(
            parse_content_type(&json!({ "content_type": "markdown" })).unwrap(),
            ContentType::Markdown
        );
    }

    #[test]
    fn every_value_the_schema_advertises_actually_parses() {
        // The schema is what a client reads to learn what it may send, and
        // these parsers are what decides. Stating the vocabulary in both places
        // is how a value gets advertised and then refused as invalid, so the
        // check is that each advertised value survives its parser.
        /// A parameter name and the parser that decides its values.
        type ParserFor = (&'static str, fn(&Value) -> bool);
        let cases: &[ParserFor] = &[
            ("fix_mode", |v| parse_fix_mode(v).is_ok()),
            ("content_type", |v| parse_content_type(v).is_ok()),
            ("profile", |v| parse_profile(v).is_ok()),
            ("political_stance", |v| parse_political_stance(v).is_ok()),
            ("fix_output", |v| parse_fix_output(v).is_ok()),
            ("output", |v| parse_output_mode(v, OutputMode::Full).is_ok()),
        ];

        // Every field routed through enum_param_error needs a schema enum to
        // quote, including the two parsed inline rather than by a named parser:
        // without one the rejection reports an empty accepted list.
        for field in [
            "fix_mode",
            "content_type",
            "profile",
            "political_stance",
            "fix_output",
            "output",
            "translationese_domain",
            "document_genre",
            "register",
            "ai_threshold",
        ] {
            assert!(
                !accepted_values(field).is_empty(),
                "{field} is rejected against the schema, so the schema must declare its values"
            );
        }

        for (field, parses) in cases {
            let accepted = accepted_values(field);
            for value in accepted {
                let args = json!({ *field: value });
                assert!(
                    parses(&args),
                    "the schema advertises {field}={value:?} but the parser rejects it"
                );
            }
        }
    }

    #[test]
    fn a_rejected_value_is_told_what_the_schema_allows() {
        let err = enum_param_error("profile", "nonsense");
        let accepted = err.data.as_ref().and_then(|d| d.get("accepted")).cloned();
        assert_eq!(accepted, Some(json!(["base", "strict"])));
    }

    #[test]
    fn schema_documents_every_parameter_the_tool_takes() {
        // The validator reads the accepted set straight off the schema, so a
        // parameter documented but not accepted is no longer possible. What
        // this pins is the other direction: that the schema still carries each
        // of these, since dropping one now silently stops accepting it too.
        let known = input_schema_properties();
        for p in [
            "text",
            "fix_mode",
            "max_errors",
            "max_warnings",
            "profile",
            "relaxed",
            "exempt_blockquotes",
            "content_type",
            "political_stance",
            "ignore_terms",
            "glossary",
            "consistency",
            "explain",
            "fix_output",
            "output",
            "detect_ai",
            "detect_translationese",
            "detect_style",
            "translationese_domain",
            "document_genre",
            "register",
            "rhythm",
            "ai_threshold",
            "include_telemetry",
            "include_stats",
        ] {
            assert!(
                known.contains_key(p),
                "parameter {p:?} missing from the tool's input schema",
            );
        }
    }

    #[test]
    fn tools_call_response_gate_accepts_no_errors_when_max_errors_set() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "max_errors": 0 }),
        );
        assert!(resp.is_ok());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
        assert_eq!(output["gate"]["max_errors"], 0);
    }

    /// The tool's JSON payload for a call the response gate turned down.
    fn assert_tool_rejected(resp: &ParamResult<CallToolResult>) -> serde_json::Value {
        let result = resp.as_ref().expect("the gate reports through the result");
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .expect("the tool returns one text block");
        serde_json::from_str(&text.text).unwrap()
    }

    #[test]
    fn tools_call_response_gate_rejects_when_errors_exceed_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "乞業", "max_errors": 0 }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
        assert!(output["gate"]["residual_errors"].as_u64().unwrap() > 0);
    }

    #[test]
    fn tools_call_response_gate_accepts_when_errors_within_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "乞業", "max_errors": 10 }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_enabled_when_only_max_warnings_set() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "max_warnings": 0 }),
        );
        assert!(resp.is_ok());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
        assert_eq!(output["gate"]["max_warnings"], 0);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_warnings_exceed_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "軟件", "max_warnings": 0 }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
        assert!(output["gate"]["residual_warnings"].as_u64().unwrap() > 0);
    }

    #[test]
    fn tools_call_response_gate_accepts_when_warnings_within_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "軟件", "max_warnings": 10 }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_errors_pass_but_warnings_exceed() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "max_errors": 10, "max_warnings": 0
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_warnings_pass_but_errors_exceed() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "乞業", "max_errors": 0, "max_warnings": 10
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_accepts_after_stance_filters_political_errors() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "max_errors": 0, "political_stance": "neutral"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
    }

    #[test]
    fn tools_call_register_formal_licenses_the_bureaucratic_prefix() {
        let (mut server, _dir) = make_initialized_server();
        let casual = call_zhtw(
            &mut server,
            serde_json::json!({"text": "我們予以處理這件事。", "register": "casual"}),
        );
        let casual = assert_tool_success(&casual);
        assert!(
            !casual["issues"].as_array().unwrap().is_empty(),
            "casual prose should still report 予以處理: {casual}"
        );

        let formal = call_zhtw(
            &mut server,
            serde_json::json!({"text": "我們予以處理這件事。", "register": "formal"}),
        );
        let formal = assert_tool_success(&formal);
        assert!(
            formal["issues"].as_array().unwrap().is_empty(),
            "a formal register licenses 予以處理: {formal}"
        );
    }

    #[test]
    fn tools_call_rejects_an_unknown_register() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({"text": "測試", "register": "poetic"}),
        );
        let body = serde_json::to_string(&resp).unwrap();
        assert!(body.contains("register"), "{body}");
    }

    #[test]
    fn tools_call_rhythm_is_opt_in() {
        let (mut server, _dir) = make_initialized_server();
        let text = "這個系統在使用者完成註冊並且通過驗證之後就會自動建立一組預設的設定檔然後開始同步資料。";
        let off = call_zhtw(&mut server, serde_json::json!({"text": text}));
        let on = call_zhtw(
            &mut server,
            serde_json::json!({"text": text, "rhythm": true}),
        );
        let off = assert_tool_success(&off)["issues"]
            .as_array()
            .unwrap()
            .len();
        let on = assert_tool_success(&on)["issues"].as_array().unwrap().len();
        assert!(on > off, "rhythm should add advisories: {on} vs {off}");
    }

    #[test]
    fn tools_call_response_gate_rejects_when_stance_keeps_political_errors() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "max_errors": 0, "political_stance": "roc_centric"
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
    }

    #[test]
    fn tools_call_response_gate_accepts_after_ignore_terms_downgrades_error() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "乞業", "max_errors": 0, "ignore_terms": ["乞業"]
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
    }

    #[test]
    fn tools_call_fix_respects_exempt_blockquotes() {
        let (mut server, _dir) = make_initialized_server();
        let text = "> 用戶輸入需要驗證。\n";
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": text,
                "content_type": "markdown",
                "exempt_blockquotes": true,
                "fix_mode": "lexical_safe"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["text"], text);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            !issues.iter().any(|i| i["found"] == "用戶"),
            "blockquote text must stay exempt on MCP fix path; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_honors_glossary_banned_terms() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "ABC 不該出現在文件中。\n",
                "fix_mode": "lexical_safe",
                "glossary": { "banned": ["ABC"] }
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            issues.iter().any(|i| i["found"] == "ABC"),
            "glossary banned terms must remain active on fix path; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_honors_glossary_proper_nouns() {
        let (mut server, _dir) = make_initialized_server();
        let text = "我們的線程實作。\n";
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": text,
                "fix_mode": "lexical_safe",
                "glossary": { "proper_nouns": ["線程"] }
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["text"], text);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            !issues.iter().any(|i| i["found"] == "線程"),
            "proper_nouns must suppress fix-path issues; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_returns_consistency_report() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "我們的線程太慢，需要重構執行緒。\n",
                "fix_mode": "orthographic",
                "consistency": true
            }),
        );
        let output = assert_tool_success(&resp);
        let groups = output["consistency"]["groups"]
            .as_array()
            .expect("consistency groups");
        assert!(
            groups.iter().any(|g| g["term_group"] == "thread"),
            "fix-path consistency report must be returned; got {groups:?}"
        );
    }

    #[test]
    fn tools_call_set_invalid_content_type() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "content_type": "invalid_type" }),
        );
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "content_type");
        assert_eq!(data["value"], "invalid_type");
    }

    #[test]
    fn tools_call_set_invalid_profile() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "profile": "invalid_profile" }),
        );
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "profile");
        assert_eq!(data["value"], "invalid_profile");
    }

    #[test]
    fn tools_call_set_invalid_fix_mode() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "fix_mode": "invalid_fix_mode" }),
        );
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "fix_mode");
        assert_eq!(data["value"], "invalid_fix_mode");
    }

    #[test]
    fn tools_call_set_invalid_political_stance() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "political_stance": "invalid_stance" }),
        );
        let err = assert_tool_error(resp);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "political_stance");
        assert_eq!(data["value"], "invalid_stance");
    }

    #[test]
    fn tools_call_explain_true_includes_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": true, "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_some());
    }

    #[test]
    fn tools_call_explain_false_omits_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": false, "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
    }

    #[test]
    fn tools_call_explain_non_bool_treated_as_false() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": "not_a_boolean", "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
    }

    #[test]
    fn tools_call_explain_true_compact_includes_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": true, "output": "compact"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_some());
        assert!(issues[0].get("count").is_some());
        assert!(issues[0].get("locations").is_some());
    }

    #[test]
    fn tools_call_explain_false_compact_omits_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": false, "output": "compact"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
        assert!(issues[0].get("count").is_some());
        assert!(issues[0].get("locations").is_some());
    }

    #[test]
    fn tools_call_lint_stance_roc_centric_keeps_political_issue() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "political_stance": "roc_centric"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(issues
            .iter()
            .any(|i| i["rule_type"] == "political_coloring"));
    }

    #[test]
    fn tools_call_lint_stance_neutral_removes_political_issue() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "political_stance": "neutral"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues
            .iter()
            .any(|i| i["rule_type"] == "political_coloring"));
    }

    #[test]
    fn tools_call_full_output_includes_scan_metadata() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "使用 C P U 架構處理工作負載",
                "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        assert!(output["coverage"]["rules_checked"].as_u64().unwrap() > 0);
        assert_eq!(output["coverage"]["rules_matched"], 0);
        assert!(output["quality_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "spaced_acronyms"));
    }

    #[test]
    fn tools_call_summary_output_keeps_document_level_flags() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "這個那個這個那個這個那個這個那個這個那個",
                "output": "summary"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["summary"]["errors"], 0);
        assert_eq!(output["summary"]["warnings"], 0);
        assert_eq!(output["summary"]["info"], 0);
        assert_eq!(output["oral_density"], 1.0);
        assert!(output["quality_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "high_oral_density"));
    }
}
