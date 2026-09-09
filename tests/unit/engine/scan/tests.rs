use super::overlap::resolve_overlaps;
use super::*;
use crate::rules::ruleset::RuleFamily;
use crate::rules::ruleset::RuleType;
use crate::rules::ruleset::SpacingPolicy;

fn sample_spelling_rules() -> Vec<SpellingRule> {
    vec![
        SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
        SpellingRule::new("內存", vec!["記憶體".into()], RuleType::CrossStrait),
        SpellingRule::new("服務器", vec!["伺服器".into()], RuleType::CrossStrait),
    ]
}

fn sample_case_rules() -> Vec<CaseRule> {
    vec![
        CaseRule {
            term: "JavaScript".into(),
            alternatives: Some(vec!["javascript".into(), "JAVASCRIPT".into()]),
            disabled: false,
        },
        CaseRule {
            term: "TypeScript".into(),
            alternatives: None,
            disabled: false,
        },
        CaseRule {
            term: "API".into(),
            alternatives: Some(vec!["Api".into(), "api".into(), "APIs".into()]),
            disabled: false,
        },
    ]
}

/// Select by detector family, which is now a field rather than a code
/// spelled into the human-readable message.
fn translationese_issues(issues: &[Issue], family: PhaseFamily) -> Vec<&Issue> {
    issues
        .iter()
        .filter(|issue| issue.phase_family.is_some_and(|(f, _)| f == family))
        .collect()
}

#[test]
fn basic_spelling_detection() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    let issues = scanner.scan("這個軟件很好用").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "軟件");
    assert_eq!(issues[0].suggestions[..], vec!["軟體"]);
    assert_eq!(issues[0].rule_type, IssueType::CrossStrait);
}

#[test]
fn a_heading_boosts_an_unpinned_info_finding() {
    // The Warning -> Error arm is covered through the CLI; the Info -> Warning
    // arm was not, so turning it into a no-op passed the whole suite. An
    // ai_filler finding is Info by type and pins nothing, which is exactly the
    // case the exemption must not swallow.
    let rule = SpellingRule::new("研究顯示", vec![String::new()], RuleType::AiFiller);
    let scanner = Scanner::new(vec![rule], vec![]);

    let plain =
        scanner.scan_for_content_type("研究顯示成果很好", ContentType::Markdown, Profile::Base);
    let flat = plain
        .issues
        .iter()
        .find(|i| i.found == "研究顯示")
        .expect("reported outside a heading");
    assert_eq!(flat.severity, Severity::Info, "Info away from a heading");

    let heading =
        scanner.scan_for_content_type("# 研究顯示成果很好", ContentType::Markdown, Profile::Base);
    let boosted = heading
        .issues
        .iter()
        .find(|i| i.found == "研究顯示")
        .expect("reported inside a heading");
    assert_eq!(
        boosted.severity,
        Severity::Warning,
        "an Info finding that pinned nothing takes the heading boost"
    );
}

#[test]
fn variant_advisory_status_comes_from_configured_severity() {
    let configured_info = SpellingRule {
        severity: Some(Severity::Info),
        ..SpellingRule::new("裏", vec!["裡".into()], RuleType::Variant)
    };
    let scanner = Scanner::new(vec![configured_info], vec![]);
    assert!(scanner
        .scan_with_config("裏", &[], Profile::Strict.config())
        .issues[0]
        .is_pinned_advisory());

    let scanner = Scanner::new(
        vec![SpellingRule::new(
            "佈署",
            vec!["部署".into()],
            RuleType::Variant,
        )],
        vec![],
    );
    let issue = scanner
        .scan_with_config("佈署", &[], Profile::Strict.config())
        .issues
        .pop()
        .unwrap();
    assert_eq!(issue.severity, Severity::Warning);
    assert!(
        !issue.is_pinned_advisory(),
        "a rule that declared nothing pins nothing"
    );
}

#[test]
fn reusable_scratch_plain_api_remains_available() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    let cfg = Profile::Base.config();
    let mut scratch = ScratchSpace::new();

    // This four-argument call is the public hot-loop API. Keep it as a
    // compile-time compatibility test while content-aware callers use the
    // separately named variant.
    let legacy = scanner.scan_with_config_into("這個軟件很好用", &[], cfg, &mut scratch);
    let mut content_scratch = ScratchSpace::new();
    let content_aware = scanner.scan_with_config_into_content_type(
        "這個軟件很好用",
        &[],
        cfg,
        ContentType::Plain,
        &mut content_scratch,
    );

    assert_eq!(legacy.issues.len(), 1);
    assert_eq!(legacy.issues[0].found, "軟件");
    assert_eq!(
        legacy
            .issues
            .iter()
            .map(|issue| (&issue.offset, &issue.found))
            .collect::<Vec<_>>(),
        content_aware
            .issues
            .iter()
            .map(|issue| (&issue.offset, &issue.found))
            .collect::<Vec<_>>()
    );
}

// The mention filter answers "is this phrase named or used", a question an
// invisible character cannot be on either side of. Scoping it to every AiStyle
// finding silenced the invisible-character layer inside a quotation and on a
// task-list line, which is where a hidden-instruction payload would sit.
#[test]
fn quoting_does_not_hide_an_invisible_character() {
    let scanner = Scanner::new(vec![], vec![]);
    let mut cfg = Profile::Base.config();
    cfg.ai_structural_patterns = true;
    let text = "注意「零寬\u{200B}空格」。\n- [ ] 檢查\u{200B}殘留\n零寬\u{200B}空格在外面。\n";
    let issues = scanner
        .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
        .issues;
    assert_eq!(
        issues
            .iter()
            .filter(|issue| issue.found == "\u{200B}")
            .count(),
        3,
        "every zero-width space must be reported: {issues:?}"
    );
}

// A document-level finding is anchored on one occurrence, so scoping the
// mention filter to it made the whole finding hostage to that line: the same
// document reported the mixed address with a bare 你 and said nothing once the
// first 你 appeared in a quotation.
#[test]
fn quoting_one_occurrence_does_not_hide_a_document_wide_finding() {
    let scanner = Scanner::new(vec![], vec![]);
    let mut cfg = Profile::Base.config();
    cfg.ai_structural_patterns = true;
    for text in [
        "手冊裡寫你這個字，但這裡一律用您。您可以參考說明。",
        "手冊裡寫「你」這個字，但這裡一律用您。您可以參考說明。",
    ] {
        let issues = scanner
            .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
            .issues;
        assert!(
            issues
                .iter()
                .any(|issue| issue.context.as_deref().is_some_and(|c| c.contains("混用"))),
            "mixed reader address lost for {text}: {issues:?}"
        );
    }
}

#[test]
fn multiple_spelling_issues() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    let issues = scanner.scan("這個軟件的服務器內存不夠").issues;
    assert_eq!(issues.len(), 3);
    assert_eq!(issues[0].found, "軟件");
    assert_eq!(issues[1].found, "服務器");
    assert_eq!(issues[2].found, "內存");
}

#[test]
fn spelling_in_code_fence_excluded() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    let issues = scanner.scan("請看 `軟件` 的說明").issues;
    assert_eq!(issues.len(), 0);
}

#[test]
fn spelling_in_url_excluded() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    let issues = scanner
        .scan("https://example.com/軟件/download 是連結")
        .issues;
    assert_eq!(
        issues.len(),
        0,
        "CJK inside URL path should be excluded: {issues:?}"
    );
}

#[test]
fn case_rule_basic() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("I use Javascript for work").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "Javascript");
    assert_eq!(issues[0].suggestions[..], vec!["JavaScript"]);
    assert_eq!(issues[0].rule_type, IssueType::Case);
}

#[test]
fn case_rule_correct_form_no_issue() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("I use JavaScript for work").issues;
    assert_eq!(issues.len(), 0);
}

#[test]
fn case_rule_alternative_no_issue() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("I use javascript for work").issues;
    assert_eq!(issues.len(), 0);
}

#[test]
fn case_rule_word_boundary() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("This is Unreactive").issues;
    assert_eq!(issues.len(), 0);
}

#[test]
fn case_rule_in_code_excluded() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("Use `typescript` in your code").issues;
    assert_eq!(issues.len(), 0);
}

#[test]
fn mixed_spelling_and_case() {
    let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
    let issues = scanner.scan("這個軟件用 typescript 寫的").issues;
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].found, "軟件");
    assert_eq!(issues[1].found, "typescript");
}

#[test]
fn empty_text() {
    let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
    let issues = scanner.scan("").issues;
    assert!(issues.is_empty());
}

#[test]
fn clean_text_no_issues() {
    let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
    let issues = scanner.scan("這個軟體用 TypeScript 寫的").issues;
    assert!(issues.is_empty());
}

#[test]
fn api_case_wrong() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    let issues = scanner.scan("This aPi is slow").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "aPi");
    assert_eq!(issues[0].suggestions[..], vec!["API"]);
}

#[test]
fn api_case_correct_alternatives() {
    let scanner = Scanner::new(vec![], sample_case_rules());
    assert!(scanner.scan("The API is fast").issues.is_empty());
    assert!(scanner.scan("The Api is fast").issues.is_empty());
    assert!(scanner.scan("The api is fast").issues.is_empty());
}

// Spelling AC (charwise / bytewise) tests

#[test]
fn charwise_ac_is_built_for_cjk_patterns() {
    let scanner = Scanner::new(sample_spelling_rules(), vec![]);
    assert!(
        scanner.spelling_db.ac_charwise.is_some(),
        "charwise AC should be built for CJK-only patterns"
    );
}

#[test]
fn charwise_and_bytewise_produce_identical_results() {
    let rules = sample_spelling_rules();
    let text = "這個軟件的服務器內存不夠，需要升級軟件的記憶體";
    let scanner = Scanner::new(rules.clone(), vec![]);

    // Run with charwise (default path).
    let charwise_issues = scanner.scan(text).issues;

    // Force bytewise path for comparison.
    let mut bytewise_scanner = Scanner::new(rules, vec![]);
    bytewise_scanner.force_bytewise();
    let bytewise_issues = bytewise_scanner.scan(text).issues;

    assert_eq!(
        charwise_issues.len(),
        bytewise_issues.len(),
        "charwise and bytewise should find the same number of issues"
    );
    for (cw, bw) in charwise_issues.iter().zip(bytewise_issues.iter()) {
        assert_eq!(cw.offset, bw.offset, "offsets must match");
        assert_eq!(cw.length, bw.length, "lengths must match");
        assert_eq!(cw.found, bw.found, "found text must match");
        assert_eq!(cw.suggestions, bw.suggestions, "suggestions must match");
    }
}

#[test]
fn spaced_acronym_sets_quality_flag_without_stutter() {
    let scanner = Scanner::new(vec![], vec![]);
    let output = scanner.scan("使用 C P U 架構處理工作負載");
    assert!(output.quality_flags.iter().any(|f| f == "spaced_acronyms"));
    assert!(!output.quality_flags.iter().any(|f| f == "stutter_detected"));
}

#[test]
fn repetition_sets_stutter_flag() {
    let scanner = Scanner::new(vec![], vec![]);
    let output = scanner.scan("去去來來看看這個結果");
    assert!(output.quality_flags.iter().any(|f| f == "stutter_detected"));
}

#[test]
fn clean_high_oral_density_text_keeps_document_flag() {
    let scanner = Scanner::new(vec![], vec![]);
    let output = scanner.scan("這個那個這個那個這個那個這個那個這個那個");
    assert!(output.issues.is_empty());
    assert_eq!(output.oral_density, Some(1.0));
    assert!(output
        .quality_flags
        .iter()
        .any(|f| f == "high_oral_density"));
}

#[test]
fn charwise_leftmost_longest_on_overlapping_patterns() {
    // "數據" and "數據庫" overlap: leftmost-longest must pick "數據庫".
    let rules = vec![
        SpellingRule::new("數據", vec!["資料".into()], RuleType::CrossStrait),
        SpellingRule::new("數據庫", vec!["資料庫".into()], RuleType::CrossStrait),
    ];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    let issues = scanner.scan("這個數據庫很大").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "數據庫");
    assert_eq!(issues[0].suggestions[..], vec!["資料庫"]);
}

#[test]
fn charwise_single_char_cjk_pattern() {
    // Single CJK character pattern: shortest possible charwise match.
    let rules = vec![SpellingRule::new(
        "裏",
        vec!["裡".into()],
        RuleType::Variant,
    )];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    let issues = scanner.scan_profiled("裏面有東西", Profile::Strict).issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "裏");
    assert_eq!(issues[0].suggestions[..], vec!["裡"]);
}

#[test]
fn charwise_mixed_cjk_ascii_patterns() {
    // Patterns with both CJK and ASCII characters.
    let rules = vec![
        SpellingRule::new("IP地址", vec!["IP 位址".into()], RuleType::CrossStrait),
        SpellingRule::new(
            "CPU使用率",
            vec!["CPU 使用率".into()],
            RuleType::CrossStrait,
        ),
    ];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    let issues = scanner.scan("查看IP地址和CPU使用率").issues;
    let spelling: Vec<_> = issues
        .iter()
        .filter(|i| i.rule_type == IssueType::from(RuleType::CrossStrait))
        .collect();
    assert_eq!(spelling.len(), 2);
    assert_eq!(spelling[0].found, "IP地址");
    assert_eq!(spelling[1].found, "CPU使用率");
}

/// Build a one-rule scanner whose rule carries context-selected groups.
fn context_suggestion_scanner(
    groups: Option<Vec<crate::rules::ruleset::ContextSuggestion>>,
) -> Scanner {
    let rules = vec![SpellingRule {
        context_suggestions: groups,
        ..SpellingRule::new("優化", vec!["最佳化".into()], RuleType::CrossStrait)
    }];
    Scanner::new(rules, vec![])
}

fn only_suggestions(scanner: &Scanner, text: &str) -> Vec<String> {
    let issues = scanner.scan_profiled(text, Profile::Base).issues;
    assert_eq!(issues.len(), 1, "expected exactly one issue for {text:?}");
    issues[0].suggestions.to_vec()
}

#[test]
fn context_suggestions_select_by_window() {
    use crate::rules::ruleset::ContextSuggestion;
    let scanner = context_suggestion_scanner(Some(vec![ContextSuggestion {
        clues: vec!["流程".into(), "服務".into()],
        to: vec!["改善".into(), "提升".into()],
    }]));

    // A clue in the window swaps the whole replacement set.
    assert_eq!(only_suggestions(&scanner, "優化流程"), ["改善", "提升"]);
    // Clue before the match counts too: the window spans both sides.
    assert_eq!(only_suggestions(&scanner, "服務要優化"), ["改善", "提升"]);
    // No clue nearby falls back to the rule default.
    assert_eq!(only_suggestions(&scanner, "優化演算法"), ["最佳化"]);
}

#[test]
fn context_suggestions_first_matching_group_wins() {
    use crate::rules::ruleset::ContextSuggestion;
    // Both groups match "流程效能"; ruleset order decides.
    let scanner = context_suggestion_scanner(Some(vec![
        ContextSuggestion {
            clues: vec!["流程".into()],
            to: vec!["改善".into()],
        },
        ContextSuggestion {
            clues: vec!["效能".into()],
            to: vec!["最佳化".into()],
        },
    ]));
    assert_eq!(only_suggestions(&scanner, "優化流程效能"), ["改善"]);
}

#[test]
fn context_suggestions_drop_unusable_groups() {
    use crate::rules::ruleset::ContextSuggestion;

    // An empty clue list can never select and an empty replacement list would
    // erase the default, so both are dropped at compile time and the rule
    // behaves as if it had no groups at all. A list holding an empty string is
    // the same case: the empty string is the deletion sentinel, so keeping it
    // would turn a substitution into a deletion.
    //
    // The last group is the one that matters. Filtering the empty entry out and
    // keeping "改善" would leave a one-entry group, and one entry is
    // auto-fixable, so a malformed group would quietly gain the write
    // permission the author's two candidates were meant to deny. Drop the group
    // instead of repairing it.
    let scanner = context_suggestion_scanner(Some(vec![
        ContextSuggestion {
            clues: vec![],
            to: vec!["改善".into()],
        },
        ContextSuggestion {
            clues: vec!["流程".into()],
            to: vec![],
        },
        ContextSuggestion {
            clues: vec!["服務".into()],
            to: vec![String::new()],
        },
        ContextSuggestion {
            clues: vec!["品質".into()],
            to: vec!["改善".into(), String::new()],
        },
        // An empty clue is the dangerous one: "window.contains(\"\")" is true
        // for every window, so a single stray entry makes the group the answer
        // for every match of the rule, anywhere, with no clue present at all.
        // Dropped like the rest.
        ContextSuggestion {
            clues: vec![String::new()],
            to: vec!["永遠".into()],
        },
    ]));
    assert_eq!(only_suggestions(&scanner, "優化流程"), ["最佳化"]);
    assert_eq!(only_suggestions(&scanner, "優化服務"), ["最佳化"]);
    assert_eq!(only_suggestions(&scanner, "優化品質"), ["最佳化"]);
    assert_eq!(
        only_suggestions(&scanner, "完全無關的內容優化在這裡"),
        ["最佳化"]
    );
    assert!(scanner
        .spelling_db
        .spelling_context_suggestions
        .iter()
        .all(Option::is_none));
}

#[test]
fn context_suggestions_dropped_on_deletion_rules() {
    use crate::rules::ruleset::ContextSuggestion;

    // Inflation derives the reported span from the rule's own "to": for a
    // deletion rule it uses from.len() so the user sees the phrase to delete
    // rather than any punctuation the span absorbed. A group offering a real
    // replacement would therefore report a span shorter than the one it
    // rewrites, so the combination is refused.
    let rules = vec![SpellingRule {
        context_suggestions: Some(vec![ContextSuggestion {
            clues: vec!["流程".into()],
            to: vec!["請注意".into()],
        }]),
        ..SpellingRule::new("值得注意的是", vec![String::new()], RuleType::AiFiller)
    }];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner
        .spelling_db
        .spelling_context_suggestions
        .iter()
        .all(Option::is_none));
}

#[test]
fn context_suggestions_stop_at_paragraph_breaks() {
    use crate::rules::ruleset::ContextSuggestion;

    // The selection window is the clue gate's window, which stops at a
    // paragraph break. Without that clamp a heading or an adjacent paragraph
    // within 40 chars silently swaps the replacement set, and because the
    // business group carries two entries it also disables auto-fix for a match
    // that is squarely in the IT sense.
    let scanner = context_suggestion_scanner(Some(vec![ContextSuggestion {
        clues: vec!["流程".into()],
        to: vec!["改善".into(), "提升".into()],
    }]));

    assert_eq!(
        only_suggestions(&scanner, "我們要優化演算法。\n\n流程改造報告"),
        ["最佳化"]
    );
    // Same clue, same distance, no break: the group still selects.
    assert_eq!(
        only_suggestions(&scanner, "我們要優化演算法。流程改造報告"),
        ["改善", "提升"]
    );
}

#[test]
fn charwise_exception_phrase_respected() {
    // Exception phrases must work identically on both AC paths.
    let rules = vec![SpellingRule {
        exceptions: Some(vec!["下著".into()]),
        ..SpellingRule::new("著", vec!["著".into()], RuleType::Variant)
    }];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    // "下著" is an exception: should not fire.
    let issues = scanner.scan_profiled("下著棋", Profile::Strict).issues;
    assert!(
        issues.is_empty(),
        "exception phrase '下著' should suppress the match: {issues:?}"
    );
}

#[test]
fn charwise_context_clues_gate() {
    // Context clues must gate correctly on the charwise path.
    let rules = vec![SpellingRule {
        context_clues: Some(vec!["程式".into(), "軟體".into()]),
        ..SpellingRule::new("支持", vec!["支援".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    // No context clue present: should NOT fire.
    let issues = scanner.scan("我支持你的決定").issues;
    assert!(
        issues.is_empty(),
        "should not fire without context clues: {issues:?}"
    );

    // Context clue present: SHOULD fire.
    let issues = scanner.scan("這個程式支持多種格式").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "支持");
}

#[test]
fn charwise_negative_clues_veto() {
    // Negative context clues must veto correctly on the charwise path.
    let rules = vec![SpellingRule {
        negative_context_clues: Some(vec!["掛載".into(), "mount".into()]),
        ..SpellingRule::new("卸載", vec!["解除安裝".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    // No negative clue: should fire.
    let issues = scanner.scan("請卸載這個應用程式").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "卸載");

    // Negative clue present: should NOT fire.
    let issues = scanner.scan("掛載和卸載檔案系統").issues;
    assert!(
        issues.is_empty(),
        "negative clue '掛載' should veto: {issues:?}"
    );
}

#[test]
fn bytewise_fallback_when_charwise_unavailable() {
    // Force bytewise path, verify results still correct.
    let rules = sample_spelling_rules();
    let mut scanner = Scanner::new(rules, vec![]);
    scanner.force_bytewise();

    let issues = scanner.scan("這個軟件的服務器內存不夠").issues;
    assert_eq!(issues.len(), 3);
    assert_eq!(issues[0].found, "軟件");
    assert_eq!(issues[1].found, "服務器");
    assert_eq!(issues[2].found, "內存");
}

#[test]
fn charwise_many_patterns_same_prefix() {
    // Stress the double-array trie with patterns sharing a common prefix.
    let rules = vec![
        {
            let mut r = SpellingRule::new("數", vec!["數".into()], RuleType::CrossStrait);
            r.context_clues = Some(vec!["不存在的線索".into()]);
            r
        },
        SpellingRule::new("數據", vec!["資料".into()], RuleType::CrossStrait),
        SpellingRule::new("數據庫", vec!["資料庫".into()], RuleType::CrossStrait),
        SpellingRule::new("數據結構", vec!["資料結構".into()], RuleType::CrossStrait),
    ];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    // Leftmost-longest: "數據結構" beats "數據" beats "數".
    let issues = scanner.scan("學習數據結構").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "數據結構");
    assert_eq!(issues[0].suggestions[..], vec!["資料結構"]);

    // When only "數據" present, the shorter match wins.
    let issues = scanner.scan("處理數據").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "數據");

    // "數" alone has context_clues that won't match, so it stays quiet.
    let issues = scanner.scan("數字很大").issues;
    assert!(issues.is_empty());
}

#[test]
fn charwise_adjacent_non_overlapping_matches() {
    // Two patterns that appear back-to-back without overlap.
    let rules = vec![
        SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
        SpellingRule::new("開發", vec!["研發".into()], RuleType::CrossStrait),
    ];
    let scanner = Scanner::new(rules, vec![]);
    assert!(scanner.spelling_db.ac_charwise.is_some());

    // "軟件開發": both patterns match adjacently.
    let issues = scanner.scan("軟件開發很重要").issues;
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].found, "軟件");
    assert_eq!(issues[1].found, "開發");
}

#[test]
fn charwise_full_ruleset_builds() {
    // Verify the embedded ruleset (776+ patterns) builds charwise successfully.
    let ruleset = crate::rules::loader::load_embedded_ruleset().unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    assert!(
        scanner.spelling_db.ac_charwise.is_some(),
        "charwise AC should build for the full embedded ruleset"
    );
}

#[test]
fn contact_cta_rules_do_not_force_low_editorial_confidence() {
    let ruleset = crate::rules::loader::load_embedded_ruleset().unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

    let issues = scanner.scan("如需協助請聯繫客服").issues;
    assert_eq!(
        issues.len(),
        1,
        "expected CTA phrase to match once: {issues:?}"
    );
    assert_eq!(issues[0].found, "如需協助請聯繫");
    assert_eq!(issues[0].editorial_confidence, None);

    let issues = scanner.scan("歡迎聯繫我們").issues;
    assert_eq!(
        issues.len(),
        1,
        "expected contact-us phrase to match once: {issues:?}"
    );
    assert_eq!(issues[0].found, "聯繫我們");
    assert_eq!(issues[0].editorial_confidence, None);
}

// positional_clues tests

#[test]
fn positional_before_fires_when_term_follows() {
    // before:函式 means 函式 must appear within 20 chars AFTER the match.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["before:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 函式 follows 調用: should fire.
    let issues = scanner.scan("請調用函式來處理").issues;
    assert_eq!(issues.len(), 1, "should fire when 函式 follows: {issues:?}");
    assert_eq!(issues[0].found, "調用");

    // 函式 absent: should NOT fire.
    let issues = scanner.scan("請調用這個方法").issues;
    assert!(
        issues.is_empty(),
        "should not fire without 函式 after match: {issues:?}"
    );
}

#[test]
fn positional_after_fires_when_term_precedes() {
    // after:請 means 請 must appear within 20 chars BEFORE the match.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["after:請".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 請 precedes 調用: should fire.
    let issues = scanner.scan("請調用函式").issues;
    assert_eq!(issues.len(), 1, "should fire when 請 precedes: {issues:?}");

    // 請 absent: should NOT fire.
    let issues = scanner.scan("直接調用函式").issues;
    assert!(
        issues.is_empty(),
        "should not fire without 請 before match: {issues:?}"
    );
}

#[test]
fn positional_adjacent_fires_when_immediately_next() {
    // adjacent:函式 means 函式 must be immediately adjacent (no gap).
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["adjacent:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 函式 immediately after 調用: should fire.
    let issues = scanner.scan("調用函式").issues;
    assert_eq!(
        issues.len(),
        1,
        "should fire when 函式 is adjacent: {issues:?}"
    );

    // Gap between them: should NOT fire.
    let issues = scanner.scan("調用某個函式").issues;
    assert!(
        issues.is_empty(),
        "should not fire with gap between match and term: {issues:?}"
    );

    // 函式 immediately before 調用: should also fire (adjacent = either side).
    let issues = scanner.scan("函式調用方式").issues;
    assert_eq!(
        issues.len(),
        1,
        "should fire when 函式 is adjacent before: {issues:?}"
    );
}

#[test]
fn positional_not_before_vetoes() {
    // not_before:的 means 的 must NOT appear within 20 chars after.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["not_before:的".into()]),
        ..SpellingRule::new("項目", vec!["專案".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // No 的 after: should fire.
    let issues = scanner.scan("這個項目進度超前").issues;
    assert_eq!(issues.len(), 1, "should fire without veto term: {issues:?}");

    // 的 follows: should NOT fire.
    let issues = scanner.scan("項目的名稱").issues;
    assert!(
        issues.is_empty(),
        "should be vetoed by 的 after match: {issues:?}"
    );
}

#[test]
fn positional_not_after_vetoes() {
    // not_after:清單 means 清單 must NOT appear within 20 chars before.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["not_after:清單".into()]),
        ..SpellingRule::new("項目", vec!["專案".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 清單 absent: should fire.
    let issues = scanner.scan("這個項目進度超前").issues;
    assert_eq!(issues.len(), 1, "should fire without veto term: {issues:?}");

    // 清單 precedes: should NOT fire.
    let issues = scanner.scan("清單項目需要確認").issues;
    assert!(
        issues.is_empty(),
        "should be vetoed by 清單 before match: {issues:?}"
    );
}

#[test]
fn positional_and_context_clues_both_required() {
    // Rule has both context_clues and positional_clues.  Both must pass.
    let rules = vec![SpellingRule {
        context_clues: Some(vec!["程式".into()]),
        positional_clues: Some(vec!["before:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // Both satisfied: 程式 in window AND 函式 after, should fire.
    let issues = scanner.scan("這個程式調用函式").issues;
    assert_eq!(
        issues.len(),
        1,
        "should fire when both context and positional match: {issues:?}"
    );

    // context_clues satisfied but positional NOT: should not fire.
    let issues = scanner.scan("這個程式調用方法").issues;
    assert!(
        issues.is_empty(),
        "positional fails, should not fire: {issues:?}"
    );

    // positional satisfied but context_clues NOT: should not fire.
    let issues = scanner.scan("直接調用函式").issues;
    assert!(
        issues.is_empty(),
        "context_clues fails, should not fire: {issues:?}"
    );
}

#[test]
fn positional_multiple_conditions_all_must_pass() {
    // Multiple positional conditions: all must pass (AND).
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["after:請".into(), "before:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // Both conditions met.
    let issues = scanner.scan("請調用函式").issues;
    assert_eq!(
        issues.len(),
        1,
        "both positional conditions met: {issues:?}"
    );

    // Only one condition met.
    let issues = scanner.scan("請調用方法").issues;
    assert!(
        issues.is_empty(),
        "only after: met, before: not — should not fire: {issues:?}"
    );

    let issues = scanner.scan("直接調用函式").issues;
    assert!(
        issues.is_empty(),
        "only before: met, after: not — should not fire: {issues:?}"
    );
}

#[test]
fn positional_no_regression_without_positional_clues() {
    // Rules without positional_clues should behave exactly as before.
    let rules = vec![SpellingRule::new(
        "軟件",
        vec!["軟體".into()],
        RuleType::CrossStrait,
    )];
    let scanner = Scanner::new(rules, vec![]);
    let issues = scanner.scan("這個軟件很好用").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "軟件");
}

#[test]
fn positional_before_stops_at_paragraph_break() {
    // before:函式 should NOT match across a paragraph boundary.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["before:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 函式 is in the next paragraph: should NOT fire.
    let issues = scanner.scan("請調用方法\n\n函式定義在此").issues;
    assert!(
        issues.is_empty(),
        "before: must not match across paragraph break: {issues:?}"
    );

    // 函式 is in the same paragraph: should fire.
    let issues = scanner.scan("請調用函式").issues;
    assert_eq!(issues.len(), 1);
}

#[test]
fn positional_after_stops_at_paragraph_break() {
    // after:請 should NOT match across a paragraph boundary.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["after:請".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 請 is in the previous paragraph: should NOT fire.
    let issues = scanner.scan("請看這裡\n\n調用方法").issues;
    assert!(
        issues.is_empty(),
        "after: must not match across paragraph break: {issues:?}"
    );
}

#[test]
fn positional_before_stops_at_code_span() {
    // In Markdown, before:函式 should NOT match text inside a code span.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["before:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 函式 is inside a code span: positional window should stop at the excluded
    // range boundary, so the clue is invisible.
    let md_text = "調用`函式`來處理";
    let issues = scanner
        .scan_for_content_type(md_text, ContentType::Markdown, Profile::Base)
        .issues;
    assert!(
        issues.is_empty(),
        "before: must not see text inside code spans: {issues:?}"
    );

    // Same text without code span: should fire.
    let plain_text = "調用函式來處理";
    let issues = scanner
        .scan_for_content_type(plain_text, ContentType::Markdown, Profile::Base)
        .issues;
    assert_eq!(
        issues.len(),
        1,
        "should fire when 函式 is not in code span: {issues:?}"
    );
}

#[test]
fn positional_adjacent_excluded_region() {
    // adjacent:函式 should NOT match if 函式 is inside an excluded region.
    let rules = vec![SpellingRule {
        positional_clues: Some(vec!["adjacent:函式".into()]),
        ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
    }];
    let scanner = Scanner::new(rules, vec![]);

    // 函式 inside a code span (Markdown): adjacent should not match.
    let md_text = "調用`函式`";
    let issues = scanner
        .scan_for_content_type(md_text, ContentType::Markdown, Profile::Base)
        .issues;
    assert!(
        issues.is_empty(),
        "adjacent: must not match term inside excluded region: {issues:?}"
    );
}

#[test]
fn lian_xi_contact_copy_uses_phrase_rules_without_general_prose_fp() {
    let rules = vec![
        SpellingRule::new("聯繫我們", vec!["聯絡我們".into()], RuleType::CrossStrait),
        SpellingRule::new("聯繫方式", vec!["聯絡方式".into()], RuleType::CrossStrait),
        SpellingRule::new("聯繫資訊", vec!["聯絡資訊".into()], RuleType::CrossStrait),
        SpellingRule::new("聯繫管道", vec!["聯絡管道".into()], RuleType::CrossStrait),
        SpellingRule::new("聯繫電話", vec!["聯絡電話".into()], RuleType::CrossStrait),
        SpellingRule::new("聯繫客服", vec!["聯絡客服".into()], RuleType::CrossStrait),
        SpellingRule::new(
            "如需協助請聯繫",
            vec!["如需協助請聯絡".into()],
            RuleType::CrossStrait,
        ),
    ];
    let scanner = Scanner::new(rules, vec![]);

    let issues = scanner.scan("歡迎聯繫我們").issues;
    assert_eq!(issues.len(), 1, "should flag contact CTA: {issues:?}");

    let issues = scanner.scan("請查看聯繫方式").issues;
    assert_eq!(issues.len(), 1, "should flag contact label: {issues:?}");

    let issues = scanner.scan("最新聯繫資訊如下").issues;
    assert_eq!(
        issues.len(),
        1,
        "should flag contact info label: {issues:?}"
    );

    let issues = scanner.scan("若需協助可參考聯繫管道").issues;
    assert_eq!(
        issues.len(),
        1,
        "should flag contact channel label: {issues:?}"
    );

    let issues = scanner.scan("聯繫電話：02-1234-5678").issues;
    assert_eq!(
        issues.len(),
        1,
        "should flag contact phone label: {issues:?}"
    );

    let issues = scanner.scan("請聯繫客服取得協助").issues;
    assert_eq!(issues.len(), 1, "should flag support CTA: {issues:?}");

    let issues = scanner.scan("如需協助請聯繫").issues;
    assert_eq!(
        issues.len(),
        1,
        "should flag imperative support CTA: {issues:?}"
    );

    let issues = scanner.scan("我們與學界保持密切聯繫").issues;
    assert!(
        issues.is_empty(),
        "should not flag ordinary prose: {issues:?}"
    );

    let issues = scanner.scan("請加強國際聯繫").issues;
    assert!(
        issues.is_empty(),
        "should not flag ordinary prose: {issues:?}"
    );

    let issues = scanner.scan("我們透過電話聯繫對方").issues;
    assert!(
        issues.is_empty(),
        "should not flag ordinary prose: {issues:?}"
    );
}

#[test]
fn translationese_pipeline_keeps_only_indexed_zy2_issue() {
    let scanner = Scanner::new(vec![], vec![]);
    let issues = scanner.scan("因為下雨了，所以我們待在屋裡。").issues;
    let zy2 = translationese_issues(&issues, PhaseFamily::Connective);
    assert_eq!(zy2.len(), 1, "expected one surviving ZY2 issue: {issues:?}");
    assert!(
        zy2[0]
            .phase_family
            .is_some_and(|(_, pass)| pass == PhasePass::Indexed),
        "the boundary-aware half should win: {issues:?}"
    );
}

#[test]
fn translationese_pipeline_keeps_only_indexed_zy3_issue() {
    let scanner = Scanner::new(vec![], vec![]);
    let issues = scanner.scan("他完成改善的提升的發現工作。").issues;
    let zy3 = translationese_issues(&issues, PhaseFamily::Nominalization);
    assert_eq!(zy3.len(), 1, "expected one surviving ZY3 issue: {issues:?}");
    assert!(
        zy3[0]
            .phase_family
            .is_some_and(|(_, pass)| pass == PhasePass::Indexed),
        "the boundary-aware half should win: {issues:?}"
    );
}

// Remaining tests are included from the original scan.rs via include. Rather
// than duplicating 2000+ lines inline, the tests are appended by extracting
// from the original monolithic file.
include!("tests_generated.rs");
