// IR parity tests: verify the IR-based eval_predicates() path produces
// correct output across all rule types and filter stages.
//
// Since 48.5, the production scan path uses eval_predicates() exclusively.
// These tests exercise every rule category (cross-strait, variant, ai_filler,
// political, context-clued, deletion, exception, superstring absorption,
// positional clues, negative clues) through the production Scanner::scan*
// methods to confirm the IR path handles them correctly.
//
// After confirming parity, the old process_match_dispatch_legacy was deleted
// (48.6).

use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::loader::load_embedded_ruleset;
use zhtw_mcp::rules::ruleset::{PoliticalStance, Profile, RuleType, SpellingRule};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn full_scanner() -> Scanner {
    let rs = load_embedded_ruleset().expect("load embedded ruleset");
    Scanner::new(rs.spelling_rules, rs.case_rules)
}

fn spelling_with_clues(
    from: &str,
    to: &[&str],
    context_clues: Option<Vec<&str>>,
    negative_clues: Option<Vec<&str>>,
) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::CrossStrait,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: context_clues.map(|v| v.into_iter().map(String::from).collect()),
        negative_context_clues: negative_clues.map(|v| v.into_iter().map(String::from).collect()),
        positional_clues: None,
        tags: None,
    }
}

fn spelling_with_exceptions(from: &str, to: &[&str], exceptions: Vec<&str>) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::CrossStrait,
        disabled: false,
        context: None,
        english: None,
        exceptions: Some(exceptions.into_iter().map(String::from).collect()),
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
    }
}

fn spelling_variant(from: &str, to: &[&str]) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::Variant,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
    }
}

fn spelling_ai_filler(from: &str, to: &[&str]) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::AiFiller,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
    }
}

fn spelling_political(from: &str, to: &[&str]) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::PoliticalColoring,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
    }
}

fn spelling_deletion(from: &str) -> SpellingRule {
    // Deletion rule: must be AiFiller with to == [""] per is_deletion_rule().
    SpellingRule {
        from: from.into(),
        to: vec!["".to_string()],
        rule_type: RuleType::AiFiller,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
    }
}

fn spelling_with_positional(from: &str, to: &[&str], positional: Vec<&str>) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        rule_type: RuleType::CrossStrait,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: Some(positional.into_iter().map(String::from).collect()),
        tags: None,
    }
}

// ---------------------------------------------------------------------------
// Cross-strait rules (basic IR path)
// ---------------------------------------------------------------------------

#[test]
fn ir_cross_strait_fires() {
    let scanner = Scanner::new(
        vec![SpellingRule {
            from: "程序".into(),
            to: vec!["程式".into()],
            rule_type: RuleType::CrossStrait,
            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }],
        vec![],
    );
    let out = scanner.scan("這個程序有問題");
    assert_eq!(out.issues.len(), 1, "cross-strait rule should fire");
    assert_eq!(out.issues[0].found, "程序");
    assert_eq!(out.issues[0].suggestions[..], vec!["程式"]);
}

// ---------------------------------------------------------------------------
// Variant gating: skip on Simplified Chinese
// ---------------------------------------------------------------------------

#[test]
fn ir_variant_skipped_for_simplified() {
    let scanner = Scanner::new(vec![spelling_variant("着", &["著"])], vec![]);
    let out = scanner.scan_profiled("简体中文里着重", Profile::StrictMoe);
    assert_eq!(out.issues.len(), 0, "variant should be skipped for SC");
}

#[test]
fn ir_variant_fires_for_traditional() {
    let scanner = Scanner::new(vec![spelling_variant("着", &["著"])], vec![]);
    let out = scanner.scan_profiled("繁體中文裡面着色", Profile::StrictMoe);
    // Should fire since text is Traditional Chinese.
    assert_eq!(out.issues.len(), 1, "variant should fire once for TC text");
}

// ---------------------------------------------------------------------------
// AI filler profile gate
// ---------------------------------------------------------------------------

#[test]
fn ir_ai_filler_gated_by_profile() {
    let scanner = Scanner::new(
        vec![spelling_ai_filler("值得注意的是", &["（刪除）"])],
        vec![],
    );

    // Default profile does NOT enable AI filler detection.
    let out = scanner.scan("值得注意的是這件事");
    assert_eq!(out.issues.len(), 0, "ai_filler should be gated by profile");

    // Editorial profile enables AI filler detection.
    let out = scanner.scan_profiled("值得注意的是這件事", Profile::Editorial);
    assert_eq!(out.issues.len(), 1, "ai_filler should fire under Editorial");
}

// ---------------------------------------------------------------------------
// Political stance gating
// ---------------------------------------------------------------------------

#[test]
fn ir_political_gated_by_stance() {
    let scanner = Scanner::new(vec![spelling_political("中國台灣", &["臺灣"])], vec![]);

    // Default stance (RocCentric) should allow political rules.
    let out = scanner.scan("所謂中國台灣的問題");
    assert_eq!(
        out.issues.len(),
        1,
        "political rule fires under RocCentric stance"
    );

    // Neutral stance should suppress political rules.
    let mut cfg = Profile::Default.config();
    cfg.political_stance = PoliticalStance::Neutral;
    let out = scanner.scan_with_config("所謂中國台灣的問題", &[], cfg);
    assert_eq!(
        out.issues.len(),
        0,
        "political rule suppressed under Neutral stance"
    );
}

// ---------------------------------------------------------------------------
// Context clues (positive)
// ---------------------------------------------------------------------------

#[test]
fn ir_positive_clues_required() {
    let scanner = Scanner::new(
        vec![spelling_with_clues(
            "信息",
            &["資訊"],
            Some(vec!["技術"]),
            None,
        )],
        vec![],
    );

    // Without clue: should NOT fire.
    let out = scanner.scan("這是一條信息");
    assert_eq!(out.issues.len(), 0, "should not fire without context clue");

    // With clue: should fire.
    let out = scanner.scan("在技術領域信息很重要");
    assert_eq!(out.issues.len(), 1, "should fire with context clue present");
}

// ---------------------------------------------------------------------------
// Negative context clues
// ---------------------------------------------------------------------------

#[test]
fn ir_negative_clues_suppress() {
    let scanner = Scanner::new(
        vec![spelling_with_clues(
            "信息",
            &["資訊"],
            None,
            Some(vec!["信息素"]),
        )],
        vec![],
    );

    // Without negative clue: should fire.
    let out = scanner.scan("這是一條信息需處理");
    assert_eq!(out.issues.len(), 1, "should fire without negative clue");

    // With negative clue: should be suppressed.
    let out = scanner.scan("螞蟻釋放信息素來傳遞信息");
    assert_eq!(
        out.issues.len(),
        0,
        "negative clue should suppress the match"
    );
}

// ---------------------------------------------------------------------------
// Exception phrases
// ---------------------------------------------------------------------------

#[test]
fn ir_exception_suppresses_match() {
    let scanner = Scanner::new(
        vec![spelling_with_exceptions(
            "質量",
            &["品質"],
            vec!["質量守恆"],
        )],
        vec![],
    );

    // Standalone: should fire.
    let out = scanner.scan("提升質量很重要");
    assert_eq!(out.issues.len(), 1, "should fire for standalone match");

    // Inside exception phrase: suppressed.
    let out = scanner.scan("質量守恆定律");
    assert_eq!(
        out.issues.len(),
        0,
        "exception phrase should suppress match"
    );
}

// ---------------------------------------------------------------------------
// Superstring absorption
// ---------------------------------------------------------------------------

#[test]
fn ir_superstring_absorption() {
    // Rule where one of the `to` entries contains `from` as a substring.
    // When the surrounding text already has the correct superstring form,
    // the rule should not fire.
    let scanner = Scanner::new(
        vec![SpellingRule {
            from: "鏈接".into(),
            to: vec!["連結".into(), "鏈接池".into()],
            rule_type: RuleType::CrossStrait,
            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }],
        vec![],
    );

    // Match inside superstring 'to' form: should be absorbed.
    let out = scanner.scan("使用鏈接池來管理");
    assert_eq!(
        out.issues.len(),
        0,
        "superstring form should absorb the match"
    );

    // Standalone: should fire.
    let out = scanner.scan("點擊鏈接即可");
    assert_eq!(out.issues.len(), 1, "standalone should fire");
}

// ---------------------------------------------------------------------------
// Deletion rules (span extension)
// ---------------------------------------------------------------------------

#[test]
fn ir_deletion_extends_span_over_comma() {
    let scanner = Scanner::new(vec![spelling_deletion("進行")], vec![]);

    // Deletion rule (AiFiller) with trailing fullwidth comma: span should extend.
    // Must use Editorial profile since AiFiller requires ai_filler_detection.
    let out = scanner.scan_profiled("進行，後續工作", Profile::Editorial);
    assert_eq!(out.issues.len(), 1);
    // The extended span should include the comma.
    let issue = &out.issues[0];
    // The `found` field shows the matched phrase (without absorbed comma)
    // but `length` covers the full deletion span including the comma.
    assert_eq!(
        issue.found, "進行",
        "deletion found should be the rule's from pattern"
    );
    assert!(
        issue.length > "進行".len(),
        "deletion span should extend over trailing comma, length: {}",
        issue.length
    );
}

#[test]
fn ir_deletion_no_extension_without_comma() {
    let scanner = Scanner::new(vec![spelling_deletion("進行")], vec![]);
    // Must use Editorial profile for AiFiller rules.
    let out = scanner.scan_profiled("進行後續工作", Profile::Editorial);
    assert_eq!(out.issues.len(), 1);
    assert_eq!(out.issues[0].found, "進行");
}

// ---------------------------------------------------------------------------
// Positional clues
// ---------------------------------------------------------------------------

#[test]
fn ir_positional_clue_exercises_predicate() {
    // Verify that the RequirePositionalClues predicate is exercised by
    // the IR path.  We test with adjacent: which is the simplest to
    // validate (no window ambiguity).
    let scanner = Scanner::new(
        vec![spelling_with_positional(
            "測試",
            &["考驗"],
            vec!["adjacent:用例"],
        )],
        vec![],
    );

    // Adjacent clue present: '測試用例' has '用例' immediately after '測試'.
    let out = scanner.scan("測試用例很重要");
    assert!(
        !out.issues.is_empty(),
        "adjacent positional clue should fire when term is adjacent"
    );

    // Adjacent clue absent: '進行了測試之後' -- no '用例' adjacent to '測試'.
    let out = scanner.scan("進行了測試之後回報");
    let positional_fired = out.issues.iter().any(|i| i.found == "測試");
    assert!(
        !positional_fired,
        "adjacent positional clue should suppress when term is not adjacent"
    );
}

// ---------------------------------------------------------------------------
// Full ruleset: smoke test each category with embedded rules
// ---------------------------------------------------------------------------

#[test]
fn ir_full_ruleset_cross_strait_sample() {
    let scanner = full_scanner();
    // '視頻' is a well-known cross-strait term (cn: video).
    let out = scanner.scan("觀看視頻內容");
    let found: Vec<&str> = out.issues.iter().map(|i| i.found.as_str()).collect();
    assert!(
        found.contains(&"視頻"),
        "expected '視頻' in issues, got: {found:?}"
    );
}

#[test]
fn ir_full_ruleset_variant_skipped_for_simplified() {
    let scanner = full_scanner();
    let out = scanner.scan_profiled("简体中文里着重强调内容", Profile::StrictMoe);
    // Variant rules should not fire on SC text.
    let variant_issues: Vec<_> = out.issues.iter().filter(|i| i.found == "着").collect();
    assert!(
        variant_issues.is_empty(),
        "variant '着' should be skipped on SC text"
    );
}

#[test]
fn ir_full_ruleset_ai_filler_gated() {
    let scanner = full_scanner();

    // Under default profile, AI filler rules should not fire.
    let out = scanner.scan("值得注意的是這個問題");
    let ai_issues: Vec<_> = out
        .issues
        .iter()
        .filter(|i| i.found == "值得注意的是")
        .collect();
    assert!(
        ai_issues.is_empty(),
        "AI filler should not fire under Default profile"
    );

    // Under Editorial profile, AI filler rules should fire.
    let out = scanner.scan_profiled("值得注意的是這個問題", Profile::Editorial);
    let ai_issues: Vec<_> = out
        .issues
        .iter()
        .filter(|i| i.found == "值得注意的是")
        .collect();
    assert_eq!(
        ai_issues.len(),
        1,
        "AI filler should fire exactly once under Editorial profile"
    );
}

#[test]
fn ir_full_ruleset_context_clue_suppression() {
    let scanner = full_scanner();
    // Rules with context clues should only fire when clues are present.
    // '渲染' with rendering context should fire; without should not.
    // (渲染 has context_clues like '3D', 'GPU', etc.)
    let out_no_clue = scanner.scan("這幅畫的渲染效果很好");
    let out_with_clue = scanner.scan("使用GPU進行渲染加速");
    let no_clue_count = out_no_clue
        .issues
        .iter()
        .filter(|i| i.found == "渲染")
        .count();
    let with_clue_count = out_with_clue
        .issues
        .iter()
        .filter(|i| i.found == "渲染")
        .count();
    // Without context clues the rule should NOT fire.
    assert_eq!(
        no_clue_count, 0,
        "渲染 should not fire without rendering context clues"
    );
    // With GPU context clue the rule SHOULD fire.
    assert_eq!(
        with_clue_count, 1,
        "渲染 should fire exactly once when GPU/3D context clue is present"
    );
}

// ---------------------------------------------------------------------------
// Markdown exclusion through IR path
// ---------------------------------------------------------------------------

#[test]
fn ir_markdown_code_exclusion() {
    let scanner = Scanner::new(
        vec![SpellingRule {
            from: "視頻".into(),
            to: vec!["影片".into()],
            rule_type: RuleType::CrossStrait,
            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }],
        vec![],
    );

    // Inside code block: excluded.
    let md = "```\n視頻\n```\n";
    let out = scanner.scan_for_content_type(md, ContentType::Markdown, Profile::Default);
    assert_eq!(out.issues.len(), 0, "code block should exclude matches");

    // Outside code block: fires.
    let md = "觀看視頻內容\n";
    let out = scanner.scan_for_content_type(md, ContentType::Markdown, Profile::Default);
    assert_eq!(out.issues.len(), 1, "outside code block should fire");
}

// ---------------------------------------------------------------------------
// Determinism: same input -> same output
// ---------------------------------------------------------------------------

#[test]
fn ir_deterministic_output() {
    let scanner = full_scanner();
    let text = "使用視頻會議進行交流，程序設計需要數據庫支持";
    let out1 = scanner.scan(text);
    let out2 = scanner.scan(text);
    assert_eq!(
        out1.issues.len(),
        out2.issues.len(),
        "output should be deterministic"
    );
    for (a, b) in out1.issues.iter().zip(out2.issues.iter()) {
        assert_eq!(a.offset, b.offset);
        assert_eq!(a.length, b.length);
        assert_eq!(a.found, b.found);
        assert_eq!(a.suggestions, b.suggestions);
        assert_eq!(a.severity, b.severity);
    }
}
