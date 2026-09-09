use super::schema::{input_schema_properties, tool_definitions};
use super::*;
use crate::rules::ruleset::{RuleFamily, Tier2Outcome};
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

#[test]
fn explain_meta_configured_info_variant_remains_safe_without_review() {
    let mut issue = Issue::new(
        0,
        "裏".len(),
        "裏",
        vec!["裡".into()],
        IssueType::Variant,
        Severity::Info,
    );
    issue.configured_severity = Some(Severity::Info);
    let meta = derive_explain_meta(&issue);
    assert!(matches!(
        meta.editorial_confidence,
        EditorialConfidence::High
    ));
    assert!(meta.auto_fix_safe);
    assert!(!meta.needs_review);
}

/// A user suppression changes the displayed severity, not the rule's
/// configured severity. It must not make a normally-warning variant look
/// like a ruleset-pinned advisory in explain output.
#[test]
fn explain_meta_suppressed_variant_requires_review() {
    let issue = Issue::new(
        0,
        "佈署".len(),
        "佈署",
        vec!["部署".into()],
        IssueType::Variant,
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
    // Regression: the main Translationese arm already appends the context, so
    // the shared "Context:" tail must be suppressed for this issue type or the
    // narrative gets repeated.
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
    // s2t is built lazily on first Simplified input; this exercises that path
    // end-to-end (get_or_init + convert) and confirms the flag is set.
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
    // Generic over the schema rather than naming the field, the same way the
    // accepted-values check is: a '"default"' a client reads and a default the
    // parser applies are two statements of one fact, and the drift between them
    // is silent.
    for (field, prop) in input_schema_properties() {
        let Some(default) = prop.get("default").and_then(Value::as_str) else {
            continue;
        };
        assert!(
            accepted_values(field).contains(&default),
            "{field} declares a default the schema does not accept: {default:?}"
        );

        // Only content_type has a parser to compare against here. Another field
        // growing a default should add its own parser beside this, not inherit
        // this one and fail with a message about the wrong parser.
        if field == "content_type" {
            assert_eq!(
                parse_content_type(&json!({})).ok(),
                ContentType::from_name(default),
                "{field}: the parser's default and the schema's disagree"
            );
        }
    }
}

#[test]
fn omitting_content_type_leaves_the_text_plain() {
    // A tool call has no file name to infer from, and treating unmarked text as
    // Markdown silently skips anything in it that looks like a fence. Nothing
    // else pins this default.
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
    // The schema is what a client reads to learn what it may send, and these
    // parsers are what decides. Stating the vocabulary in both places is how a
    // value gets advertised and then refused as invalid, so the check is that
    // each advertised value survives its parser.
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

    // Every field routed through enum_param_error needs a schema enum to quote,
    // including the two parsed inline rather than by a named parser: without
    // one the rejection reports an empty accepted list.
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
        "off",
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

    for family in accepted_values("off") {
        assert!(
            parse_off(&json!({ "off": [family] })).is_ok(),
            "the schema advertises off={family:?} but the parser rejects it"
        );
    }
}

#[test]
fn a_rejected_value_is_told_what_the_schema_allows() {
    let err = enum_param_error("profile", "nonsense");
    let accepted = err.data.as_ref().and_then(|d| d.get("accepted")).cloned();
    assert_eq!(accepted, Some(json!(["base", "strict"])));
}

#[test]
fn a_rejected_off_family_is_told_the_family_list() {
    // An array parameter keeps its enum one level down, on the item schema.
    // Reading the top level only would name no family at all, which tells the
    // client that nothing is accepted.
    let err = parse_off(&json!({ "off": ["punctuaton"] })).expect_err("a typo is rejected");
    let accepted = err
        .data
        .as_ref()
        .and_then(|d| d.get("accepted"))
        .and_then(|v| v.as_array())
        .cloned()
        .expect("the rejection carries the accepted list");
    assert!(accepted.contains(&json!("punctuation")));
    assert_eq!(accepted.len(), RuleFamily::ALL.len());
}

#[test]
fn schema_documents_every_parameter_the_tool_takes() {
    // The validator reads the accepted set straight off the schema, so a
    // parameter documented but not accepted is no longer possible. What this
    // pins is the other direction: that the schema still carries each of these,
    // since dropping one now silently stops accepting it too.
    let known = input_schema_properties();
    for p in [
        "text",
        "fix_mode",
        "max_errors",
        "max_warnings",
        "profile",
        "spacing",
        "off",
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
    let text =
        "這個系統在使用者完成註冊並且通過驗證之後就會自動建立一組預設的設定檔然後開始同步資料。";
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
fn tools_call_spacing_strip_removes_stored_boundary_spaces() {
    let (mut server, _dir) = make_initialized_server();
    let resp = call_zhtw(
        &mut server,
        serde_json::json!({ "text": "在 Hello", "spacing": "strip" }),
    );
    let output = assert_tool_success(&resp);
    assert!(
        output["issues"].as_array().unwrap().iter().any(|issue| {
            issue["context"]
                .as_str()
                .is_some_and(|context| context.contains("中英文之間不加空格"))
        }),
        "strip policy should report the stored boundary space: {output}"
    );
}

#[test]
fn tools_call_rejects_an_unknown_spacing_policy() {
    let (mut server, _dir) = make_initialized_server();
    let err = assert_tool_error(call_zhtw(
        &mut server,
        serde_json::json!({ "text": "", "spacing": "off" }),
    ));
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(err.data.unwrap()["field"], "spacing");
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
fn tools_call_explain_keeps_suppressed_variant_conservative() {
    let (mut server, _dir) = make_initialized_server();
    let resp = call_zhtw(
        &mut server,
        serde_json::json!({
            "text": "佈署",
            "profile": "strict",
            "ignore_terms": ["佈署"],
            "explain": true,
            "output": "full"
        }),
    );
    let output = assert_tool_success(&resp);
    let issue = output["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["found"] == "佈署")
        .unwrap();
    assert_eq!(issue["severity"], "info");
    assert_eq!(issue["explain_meta"]["editorial_confidence"], "low");
    assert_eq!(issue["explain_meta"]["auto_fix_safe"], false);
    assert_eq!(issue["explain_meta"]["needs_review"], true);
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
