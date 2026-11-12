// MCP tool handler implementations.
//
// One tool exposed to the MCP client:
//   zhtw: unified lint / fix / gate for Traditional Chinese (Taiwan) text

use std::cell::OnceCell;
use std::sync::Arc;

use serde_json::{json, Value};

use super::resources;
use rmcp::model::{CacheScope, CallToolResult, ReadResourceResult};
use rmcp::ErrorData;

use super::sampling::{refine_issues_with_sampling, SamplingBridge, SamplingStats};
use super::telemetry::{TelemetryMetrics, TokenTelemetry};
use crate::audit::Trace;
use crate::engine::disambig::{disambiguate_batch, DisambigConfig, DisambigStats};
use crate::engine::s2t::S2TConverter;
use crate::engine::scan::{ContentType, Scanner};
#[cfg(feature = "translate")]
use crate::engine::translate::calibrate_issues;
use crate::engine::zhtype::{detect_chinese_type, ChineseType};
use crate::fixer::{
    apply_fixes_for_content_type, remap_to_post_fix, suppress_convergent_issues, FixMode,
};
use crate::rules::ignore::apply_ignore_set;
use crate::rules::loader::compute_ruleset_hash;
use crate::rules::ruleset::Ruleset;
use crate::rules::ruleset::{
    AttributionGenre, Issue, IssueType, PoliticalStance, Profile, ResolutionTier, Severity,
};
use crate::rules::store::{OverrideStore, PackStore, SuppressionStore, TranslationMemoryStore};

mod output;
mod params;
mod schema;

use output::*;
pub use output::{
    compress_locations, escape_tsv_field, group_issues, shorten_severity, shorten_type, IssueGroup,
};
pub(crate) use params::ParamResult;
use params::*;
pub(crate) use schema::{
    get_prompt, list_prompts, list_resource_templates, list_resources, list_tools,
};

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

        // This request's own declaration first, the handshake's second. Both
        // name the same client in practice; the order is what keeps a
        // declaration from outliving the request that made it.
        let client = declared_client.or(self.client_name.as_deref());
        let params = CheckParams::parse(args, default_output_mode(client))?;

        // JSON keys and placeholders must not be converted as prose.
        let s2t_converted = if params.content_type.converts_s2t()
            && detect_chinese_type(text) == ChineseType::Simplified
        {
            Some(self.s2t.get_or_init(S2TConverter::new).convert(text))
        } else {
            None
        };
        let text = s2t_converted.as_deref().unwrap_or(text);

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
            spacing,
            include_telemetry,
            include_stats,
            #[cfg(feature = "translate")]
            verify,
            ref ignore_terms,
            ref translationese_domain_opt,
            ref document_genre_opt,
            ref register_opt,
            ref off,
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
                spacing,
                off,
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
    fn request_telemetry(
        &self,
        counts: TelemetryCounts<'_>,
        cache_before: (u64, u64),
    ) -> TelemetryMetrics {
        build_telemetry(
            counts,
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
                TelemetryCounts {
                    text,
                    scanner_hit_count,
                    disambig_stats: &disambig_stats,
                    sampling_stats: &sampling_stats,
                    est_tokens: est_tokens(bridge.as_ref()),
                    applied_fixes: 0,
                },
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

    /// Apply `mode` to the issues the translation memory has not vetoed.
    ///
    /// A term the user deliberately rejected must not be auto-corrected, so
    /// the veto is applied to what goes into the fixer rather than to what
    /// comes out of it.
    fn apply_vetted_fixes(
        &self,
        text: &str,
        mut issues: Vec<Issue>,
        mode: FixMode,
        excluded: &[crate::engine::excluded::ByteRange],
        content_type: ContentType,
    ) -> crate::fixer::FixResult {
        // Taken by value and filtered in place. The caller is done with the
        // list, and cloning it here would allocate a String per issue to hand
        // the fixer what it already had.
        if let Some(tm) = &self.tm_store {
            issues.retain(|i| !tm.should_suppress(&i.found));
        }
        apply_fixes_for_content_type(
            text,
            &issues,
            mode,
            excluded,
            Some(self.catalog.scanner.segmenter()),
            content_type,
        )
    }

    /// Bring the re-scan's issues back in line with what the request asked
    /// for, and report how many the translation memory suppressed.
    ///
    /// Order matters throughout: severity is restored before the TM runs, so
    /// the count reflects the final state rather than a pre-fix snapshot.
    fn reconcile_rescan(
        &mut self,
        remaining_issues: &mut Vec<Issue>,
        ctx: RescanContext<'_>,
    ) -> usize {
        if let Some(st) = ctx.stance {
            filter_by_stance(remaining_issues, st);
        }
        self.apply_suppressions(remaining_issues);
        apply_ignore_set(remaining_issues, ctx.ignore_set);

        restore_preserved_states(
            remaining_issues,
            ctx.preserved_states,
            &ctx.fix_result.applied_fixes,
        );

        // Suppress convergent-chain noise: remove re-scan issues whose offset
        // falls within a byte range written by the fixer.
        suppress_convergent_issues(remaining_issues, &ctx.fix_result.applied_fixes);

        *remaining_issues = crate::rules::glossary::apply_glossary_with_coordinates(
            &ctx.fix_result.text,
            ctx.content_type,
            ctx.cfg,
            std::mem::take(remaining_issues),
            ctx.glossary,
        );

        // Apply TM after preserved state restoration so the count reflects the
        // true final state, not a pre-fix snapshot.
        self.apply_tm(remaining_issues)
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
        // state, then fix what the TM has not vetoed.
        let preserved_states = snapshot_states(&issues);
        let fix_result = self.apply_vetted_fixes(text, issues, mode, &excluded, content_type);

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
        let tm_suppressed = self.reconcile_rescan(
            &mut remaining_issues,
            RescanContext {
                stance,
                ignore_set,
                preserved_states: &preserved_states,
                fix_result: &fix_result,
                content_type,
                cfg: &cfg,
                glossary,
            },
        );

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
                TelemetryCounts {
                    text,
                    scanner_hit_count,
                    disambig_stats: &disambig_stats,
                    sampling_stats: &sampling_stats,
                    est_tokens: est_tokens(bridge.as_ref()),
                    applied_fixes: fix_result.applied,
                },
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

/// Remove political_coloring issues that the given stance suppresses.
fn filter_by_stance(issues: &mut Vec<Issue>, stance: PoliticalStance) {
    issues.retain(|issue| {
        issue.rule_type != IssueType::PoliticalColoring || stance.allows_rule(&issue.found)
    });
}

// EditorialConfidence is canonical-defined in crate::rules::ruleset so that
// SpellingRule.editorial_confidence and the per-issue field share a single
// type. Re-exported here for the explain pipeline.
use crate::rules::ruleset::EditorialConfidence;

/// What one request counted, gathered from the stages that produced it.
/// These six always travel together, from the pipeline that fills them to
/// the single struct that reads them, so they arrive as one binding rather
/// than as a signature at clippy's argument limit.
struct TelemetryCounts<'a> {
    text: &'a str,
    scanner_hit_count: usize,
    disambig_stats: &'a DisambigStats,
    sampling_stats: &'a SamplingStats,
    /// Prompt and completion tokens the sampling bridge estimated, zero when
    /// no sampling ran.  Read off the bridge at the call site rather than
    /// held here: the bridge carries three lifetimes of its own, and a struct
    /// that named them would tie them to every other borrow in this one.
    est_tokens: (u64, u64),
    applied_fixes: usize,
}

/// The token estimate a sampling bridge accumulated, or zeroes without one.
fn est_tokens(bridge: Option<&&mut SamplingBridge<'_>>) -> (u64, u64) {
    bridge
        .map(|b| (b.est_prompt_tokens, b.est_completion_tokens))
        .unwrap_or((0, 0))
}

/// Build telemetry metrics from accumulated counters.
/// `cache_counts` is (hits, misses) from the judgment cache.
fn build_telemetry(counts: TelemetryCounts<'_>, cache_counts: (u64, u64)) -> TelemetryMetrics {
    let (est_prompt_tokens, est_completion_tokens) = counts.est_tokens;

    // ambiguous_terms: all terms that entered Tier 2 evaluation (resolved +
    // suppressed + gray_zone), not just those forwarded to Tier 3.
    let disambig_stats = counts.disambig_stats;
    let ambiguous_terms = (disambig_stats.tier2_resolved
        + disambig_stats.suppressed
        + disambig_stats.gray_zone) as u64;
    let t = TokenTelemetry {
        input_chars: counts.text.chars().count() as u64,
        rule_hits: counts.scanner_hit_count as u64,
        ambiguous_terms,
        tier2_resolved: disambig_stats.tier2_resolved as u64,
        llm_round_trips: counts.sampling_stats.used as u64,
        final_fixes: counts.applied_fixes as u64,
        prompt_tokens: est_prompt_tokens,
        completion_tokens: est_completion_tokens,
        cache_hits: cache_counts.0,
        cache_misses: cache_counts.1,
    };
    t.derive_metrics()
}

/// What [Server::reconcile_rescan] needs to bring a re-scan back in line with
/// the request. Seven values the fix path already holds, handed over as one
/// binding rather than as a signature at clippy's argument limit.
struct RescanContext<'a> {
    stance: Option<PoliticalStance>,
    ignore_set: &'a std::collections::HashSet<&'a str>,
    preserved_states: &'a [PreservedState],
    fix_result: &'a crate::fixer::FixResult,
    content_type: ContentType,
    cfg: &'a crate::rules::ruleset::ProfileConfig,
    glossary: &'a crate::rules::glossary::ProjectGlossary,
}

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
    spacing: Option<&'a str>,
    off: &'a [crate::rules::ruleset::RuleFamily],
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
    if let Some(policy) = flags.spacing {
        match crate::rules::ruleset::SpacingPolicy::from_str_strict(policy) {
            Some(policy) => cfg = cfg.with_spacing_policy(policy),
            None => return Err(enum_param_error("spacing", policy)),
        }
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
    cfg = cfg.with_disabled(flags.off);
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

/// Issue grouping key: (found, rule_type, suggestions_joined, severity).
pub type IssueGroupKey<'a> = (&'a str, &'a str, String, &'a str);

// Tool definitions (JSON Schema for zhtw)

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

#[cfg(test)]
#[path = "../../tests/unit/mcp/tools/tests.rs"]
mod tests;
