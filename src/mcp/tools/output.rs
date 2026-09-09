//! Building the tool's response: the typed output shapes, the summaries and
//! explanations they carry, and the four renderings a client can ask for.
//!
//! Nothing here scans or fixes.  It reads what the pipeline in [`super`]
//! produced and decides how it reaches the client.

use serde::Serialize;

use rmcp::model::ContentBlock;

use crate::engine::scan::is_spaced_acronym_issue;

use super::*;

/// Generate a cultural/linguistic explanation for an issue.
///
/// Draws from the context, english, and rule_type fields to produce
/// a brief explanation useful for AI agents and educational applications.
pub(super) fn build_explanation(issue: &Issue) -> Option<String> {
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

/// Issue severity summary counts.
#[derive(Serialize)]
pub(super) struct IssueSummary {
    pub(super) errors: usize,
    pub(super) warnings: usize,
    pub(super) info: usize,
    /// Number of issues downgraded to Info by translation memory.
    /// Omitted (0) when TM is inactive or had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) tm_suppressed: usize,
    /// Issues resolved by Tier 2 local disambiguation (context clues,
    /// profile priors, collocations).  Omitted (0) when Tier 2 had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) tier2_resolved: usize,
    /// Issues in Tier 2 gray zone (forwarded to Tier 3 LLM).
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) tier2_gray_zone: usize,
    /// Number of sampling calls made during this invocation.
    /// Omitted (0) when sampling is inactive or unused.
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) sampling_used: usize,
    /// Number of eligible issues skipped because the sampling budget was
    /// exhausted.
    /// Omitted (0) when budget was not exhausted.
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) sampling_skipped: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Resolution tier counts and confidence distribution for the session.
/// Included in tool output when `include_stats` is true.
#[derive(Serialize)]
pub(super) struct SummaryMetrics {
    pub(super) deterministic_fixes: usize,
    pub(super) heuristic_fixes: usize,
    pub(super) llm_judged_fixes: usize,
    pub(super) unresolved: usize,
    pub(super) llm_calls: usize,
    pub(super) llm_tokens: u64,
    pub(super) confidence_distribution: ConfidenceDistribution,
}

/// Confidence buckets: high (deterministic + heuristic), medium (llm_judged),
/// low (unresolved).
#[derive(Serialize)]
pub(super) struct ConfidenceDistribution {
    pub(super) high: usize,
    pub(super) medium: usize,
    pub(super) low: usize,
}

/// Build summary_metrics from issues and accumulated stats.
pub(super) fn build_summary_metrics(
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
pub(super) struct GateInfo {
    pub(super) enabled: bool,
    pub(super) max_errors: usize,
    pub(super) residual_errors: usize,
    pub(super) max_warnings: usize,
    pub(super) residual_warnings: usize,
}

/// Anchor provenance for explain mode (borrowed).
#[derive(Serialize)]
struct AnchorProvenance<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_en: Option<&'a str>,
    anchor_match: Option<bool>,
}

/// Structured per-issue explain metadata.
///
/// Surfaced only when `explain` is requested.  Helps reviewers understand
/// the confidence behind each suggestion without parsing free-form prose.
#[derive(Serialize)]
pub(super) struct ExplainMeta<'a> {
    /// Why this is flagged.  Sourced from rule context + MoE refs when
    /// available; falls back to a structured restatement of the
    /// suggestion target.
    rationale: String,
    /// Domain that triggered the rule.  Parsed from `@domain X` markers
    /// in the rule's context field; defaults to "general".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) domain: Option<&'a str>,
    /// True when the surface form is identical across zh-CN and zh-TW
    /// but the meaning differs (e.g. 文件: document vs file).
    pub(super) is_false_friend: bool,
    /// Whether `--fix` would safely apply this suggestion.
    pub(super) auto_fix_safe: bool,
    /// Whether the suggestion benefits from manual review.
    pub(super) needs_review: bool,
    /// Per-issue editorial confidence: distinguishes binary corrections
    /// from style preferences (e.g. 場景 is correct zh-TW for a film or
    /// stage scene, so rewriting it to 情境 is an IT-context judgment call,
    /// whereas 線程 to 執行緒 is simply the zh-TW term).
    pub(super) editorial_confidence: EditorialConfidence,
}

/// Heuristic fallback when an issue lacks a rule-level
/// `editorial_confidence`.  Translationese / AI-style / grammar hits, any
/// `Info`-severity issue, and anchor-rejected issues are surfaced as `Low`;
/// hits with explicit context support climb to `Medium`; everything else
/// is `High`.
///
/// An issue the ruleset pinned to Info is the exception among the `Info`
/// findings: the pin is a statement about how much the finding matters, not
/// doubt about the correction, so it stays safe to apply.
fn heuristic_editorial_confidence(issue: &Issue) -> EditorialConfidence {
    use crate::rules::ruleset::{IssueType, Severity};

    let always_low = matches!(
        issue.rule_type,
        IssueType::Translationese | IssueType::AiStyle | IssueType::Grammar
    ) || (issue.severity == Severity::Info && !issue.is_pinned_advisory())
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
pub(super) fn derive_explain_meta(issue: &Issue) -> ExplainMeta<'_> {
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
    pub(super) translationese_signature:
        Option<&'a crate::engine::translationese_score::TranslationeseReport>,
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
    pub(super) translationese_signature:
        Option<&'a crate::engine::translationese_score::TranslationeseReport>,
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
pub(super) struct SummaryOutput<'a> {
    pub(super) accepted: bool,
    pub(super) summary: &'a IssueSummary,
    pub(super) gate: GateInfo,
    pub(super) profile: &'a str,
    pub(super) detected_script: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) translationese_signature:
        Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) summary_metrics: Option<&'a SummaryMetrics>,
}

/// Count issues by severity.
pub(super) fn build_summary(
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
pub(super) struct CheckOutputParams<'a> {
    pub(super) result_text: &'a str,
    pub(super) issues: &'a [Issue],
    pub(super) applied_fixes: usize,
    pub(super) max_errors: Option<u64>,
    pub(super) max_warnings: Option<u64>,
    pub(super) profile: Profile,
    pub(super) stance_name: &'a str,
    pub(super) detected_script: &'a str,
    /// Whether S2T conversion was applied (input was Simplified Chinese).
    pub(super) s2t_applied: bool,
    pub(super) trace: &'a Trace,
    pub(super) explain: bool,
    pub(super) output_mode: OutputMode,
    pub(super) has_fixes: bool,
    /// Fix output mode: full text, search/replace blocks, or patch array.
    pub(super) fix_output: FixOutputMode,
    /// Original text before fixes (needed for search_replace and patch modes).
    pub(super) original_text: &'a str,
    /// Applied fix records for patch/search_replace output.
    pub(super) fix_records: &'a [crate::fixer::AppliedFix],
    #[cfg(feature = "translate")]
    pub(super) calibrate_result: Option<crate::engine::translate::CalibrateResult>,
    pub(super) coverage: Option<&'a crate::engine::scan::CoverageReport>,
    pub(super) oral_density: Option<f32>,
    pub(super) quality_flags: &'a [String],
    pub(super) ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    pub(super) translationese_signature:
        Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite style scorecard (or `None` when not requested).
    /// The caller computes this once per scan; build_check_output forwards
    /// it untouched into the chosen output mode.
    pub(super) style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    /// Number of issues downgraded by translation memory.
    pub(super) tm_suppressed: usize,
    /// Sampling budget usage statistics.
    pub(super) sampling_stats: SamplingStats,
    /// Tier 2 disambiguation statistics.
    pub(super) disambig_stats: DisambigStats,
    /// Token telemetry metrics (only when include_telemetry is true).
    pub(super) telemetry: Option<TelemetryMetrics>,
    /// Whether to include per-issue resolution tier and summary_metrics.
    pub(super) include_stats: bool,
    /// Document-wide consistency report.  Some only when the
    /// caller requested `consistency: true` AND mixed regional usage
    /// is detected.
    pub(super) consistency: Option<&'a crate::engine::consistency::ConsistencyReport>,
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
pub(super) fn build_check_output(params: &CheckOutputParams<'_>) -> CallToolResult {
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
            let tsv =
                build_tabular_output(params, accepted, &summary, effective_text, fix_mode_label);
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
/// `result_text` is the post-fix text, which is not `params.result_text`, so
/// it stays its own argument; the four values the caller was spreading out of
/// `CheckOutputParams` are read back off it.
fn build_tabular_output(
    params: &CheckOutputParams<'_>,
    accepted: bool,
    summary: &IssueSummary,
    result_text: &str,
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
    if params.applied_fixes > 0 {
        let _ = write!(out, "\tfix={}", params.applied_fixes);
    }
    if params.has_fixes {
        let _ = write!(out, "\ttxt={}", result_text.len());
    }
    if let Some(mode) = fix_output_mode {
        let _ = write!(out, "\tfix_fmt={mode}");
    }
    out.push('\n');

    let groups = group_issues(params.issues, params.explain);

    // Header row.
    if params.explain {
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
        // O(groups*params.issues) scan that could also mismatch when the same
        // found term appears in multiple groups.
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
        if params.explain {
            out.push('\t');
            if let Some(expl) = &group.explanation {
                out.push_str(&escape_tsv_field(expl));
            }
        }
        out.push('\n');
    }

    // If fixes were applied, append the fixed text after a separator.
    if params.has_fixes {
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

/// A tool-level error: the call succeeded at the protocol layer and failed at
/// the tool layer, which is what lets a client show it rather than fail.
pub(super) fn tool_error(message: String) -> CallToolResult {
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
