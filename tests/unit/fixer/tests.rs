use super::*;
use crate::engine::scan::surrounding_window;
use crate::rules::ruleset::{IssueType, PhaseFamily, PhasePass, Severity};

fn make_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        suggestions.into_iter().map(String::from).collect(),
        IssueType::CrossStrait,
        Severity::Warning,
    )
}

fn make_issue_with_clues(
    offset: usize,
    found: &str,
    suggestions: Vec<&str>,
    clues: Vec<&str>,
) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        suggestions.into_iter().map(String::from).collect(),
        IssueType::Confusable,
        Severity::Warning,
    )
    .with_english("program")
    .with_context_clues(clues.into_iter().map(String::from).collect())
}

fn make_punctuation_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        suggestions.into_iter().map(String::from).collect(),
        IssueType::Punctuation,
        Severity::Warning,
    )
}

/// A caller-supplied span that splits a character is skipped, not sliced.
///
/// The scanner never emits one, so this covers the guard rather than the
/// scan: apply_fixes is public, and both edges of the span reach a slice.
#[test]
fn a_span_off_a_character_boundary_is_skipped() {
    let text = "這個軟件很好用";
    for offset in [7, 8] {
        let issues = vec![make_issue(offset, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text, "text was rewritten at offset {offset}");
        assert_eq!(result.applied, 0);
    }

    // The same span on its real boundary still applies, so the guard is
    // rejecting the misalignment rather than the issue.
    let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
    assert_eq!(
        apply_fixes(text, &issues, FixMode::LexicalSafe, &[]).applied,
        1
    );
}

/// An end that splits a character is rejected for the same reason a start
/// is: the tail copy after the last fix slices there.
#[test]
fn a_span_ending_off_a_character_boundary_is_skipped() {
    let text = "這個軟件很好用";
    let mut issue = make_issue(6, "軟件", vec!["軟體"]);
    issue.length = 5;
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text);
    assert_eq!(result.applied, 0);
}

/// A rhythm (氣口) finding as the scanner emits it, identified by its
/// family rather than by the empty suggestion list that follows from it.
/// `rhythm_findings_carry_no_suggestion` in the grammar scanner proves the
/// real detectors produce this shape; here is the fixer's side.
fn make_rhythm_issue(offset: usize, found: &str, family: PhaseFamily) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        Vec::new(),
        IssueType::Translationese,
        Severity::Info,
    )
    .with_phase_family(family, PhasePass::Indexed)
}

#[test]
fn rhythm_issues_are_never_fixed_at_any_tier() {
    let text = "這份報告詳細說明了整個系統在過去一年之中所有功能的演進過程。";
    for family in [
        PhaseFamily::RhythmLongSentence,
        PhaseFamily::RhythmMonotony,
        PhaseFamily::RhythmStaccato,
    ] {
        // The second issue carries a lone suggestion, which is the write
        // condition for every other issue type. The family has to be what stops
        // it, or a future detector that offers a rewrite hint would start
        // editing prose on taste alone.
        let issues = vec![
            make_rhythm_issue(0, "這份報告", family),
            Issue::new(
                text.find("演進過程").unwrap(),
                "演進過程".len(),
                "演進過程",
                vec!["演進".to_string()],
                IssueType::Translationese,
                Severity::Info,
            )
            .with_phase_family(family, PhasePass::Indexed),
        ];
        for mode in [
            FixMode::None,
            FixMode::Orthographic,
            FixMode::LexicalSafe,
            FixMode::LexicalContextual,
        ] {
            let result = apply_fixes(text, &issues, mode, &[]);
            assert_eq!(result.text, text, "rhythm was rewritten at {mode:?}");
            assert_eq!(result.applied, 0, "rhythm was applied at {mode:?}");
            assert_eq!(
                result.declined, 0,
                "an advisory the fixer never acts on is out of scope, not a judgment call, at {mode:?}"
            );
        }
    }
}

#[test]
fn lexical_safe_single_suggestion() {
    let text = "這個軟件很好用";
    let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個軟體很好用");
    assert_eq!(result.applied, 1);
    assert_eq!(result.skipped, 0);
}

#[test]
fn lexical_safe_multiple_suggestions_skipped() {
    let text = "這個視頻很好看";
    let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text); // unchanged
    assert_eq!(result.applied, 0);
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_contextual_skips_multi_suggestion_non_clue() {
    // Multi-suggestion lexical issue without context_clues: both LexicalSafe
    // and LexicalContextual skip it (no disambiguation).
    let text = "這個視頻很好看";
    let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
    let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
    assert_eq!(result.text, text); // unchanged -- ambiguous, no clues
    assert_eq!(result.skipped, 1);
}

#[test]
fn multiple_fixes() {
    let text = "這個軟件的內存";
    let issues = vec![
        make_issue(6, "軟件", vec!["軟體"]),
        make_issue(15, "內存", vec!["記憶體"]),
    ];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個軟體的記憶體");
    assert_eq!(result.applied, 2);
}

#[test]
fn excluded_offset_skipped() {
    let text = "這個軟件很好用";
    let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
    let result = apply_fixes(
        text,
        &issues,
        FixMode::LexicalSafe,
        &[ByteRange { start: 0, end: 21 }],
    );
    assert_eq!(result.text, text);
    assert_eq!(result.skipped, 1);
}

#[test]
fn unsorted_excluded_ranges_still_protect() {
    // Past ten ranges is_excluded binary-searches, which needs sorted,
    // non-overlapping input. A caller that hands over ranges in any other order
    // must still get its protected bytes back untouched rather than a silent
    // rewrite.
    let text = "這個軟件很好用";
    let offset = text.find("軟件").unwrap();

    // Twelve ranges, descending, with the one covering the issue last so a
    // binary search over the unsorted slice cannot find it.
    let mut excluded: Vec<ByteRange> = (0..11)
        .map(|i| ByteRange {
            start: 100 + i * 10,
            end: 100 + i * 10 + 5,
        })
        .rev()
        .collect();
    excluded.push(ByteRange {
        start: offset,
        end: offset + "軟件".len(),
    });

    let issues = vec![make_issue(offset, "軟件", vec!["軟體"])];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &excluded);
    assert_eq!(result.text, text, "protected span was rewritten");
    assert_eq!(result.skipped, 1);

    // Sorted but overlapping is the other way the binary search breaks: it
    // assumes only the immediately preceding range can overlap, so a wide early
    // range that covers the issue is missed once narrower ranges sort between
    // them.
    let mut nested = vec![ByteRange {
        start: 0,
        end: text.len(),
    }];
    nested.extend((0..11).map(|i| ByteRange {
        start: 100 + i * 10,
        end: 100 + i * 10 + 5,
    }));
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &nested);
    assert_eq!(
        result.text, text,
        "range covering the whole text was missed"
    );
    assert_eq!(result.skipped, 1);
}

#[test]
fn zero_length_insertion_inside_excluded_is_skipped() {
    // Spacing rules emit zero-length insertions (missing_space_issue in
    // src/engine/scan/spacing.rs). A zero-width span overlaps nothing, so the
    // generic overlap test reports it as outside every range. The mask still
    // has to stop it: writing a space into the middle of a code span corrupts
    // the code exactly as a replacement would.
    let text = "這是 `中文abc` 的說明";
    let code_start = text.find('`').unwrap();
    let code_end = text[code_start + 1..].find('`').unwrap() + code_start + 2;
    let boundary = text.find("abc").unwrap();
    assert!(boundary > code_start && boundary < code_end);

    let insertion = Issue::new(
        boundary,
        0,
        "",
        vec![" ".into()],
        IssueType::Punctuation,
        Severity::Info,
    );
    let result = apply_fixes(
        text,
        &[insertion],
        FixMode::Orthographic,
        &[ByteRange {
            start: code_start,
            end: code_end,
        }],
    );
    assert_eq!(result.text, text, "must not write inside the code span");
    assert_eq!(result.skipped, 1);
}

#[test]
fn declined_excluded_grammar_span_does_not_block_lexical_fix() {
    let text = "我們對`x`進行軟件處理。";
    let outer = text.find('對').unwrap();
    let inner = text.find("進行").unwrap();
    let lexical = text.find("軟件").unwrap();
    let code = text.find('`').unwrap();
    let code_end = text[code + 1..].find('`').unwrap() + code + 2;
    let issues = vec![
        Issue::new(
            outer,
            "對`x`進行軟件處理".len(),
            "對`x`進行軟件處理",
            vec!["處理`x`".into()],
            IssueType::Grammar,
            Severity::Info,
        ),
        Issue::new(
            inner,
            "進行軟件處理".len(),
            "進行軟件處理",
            vec!["軟件處理".into()],
            IssueType::Grammar,
            Severity::Info,
        ),
        make_issue(lexical, "軟件", vec!["軟體"]),
    ];

    let result = apply_fixes(
        text,
        &issues,
        FixMode::LexicalContextual,
        &[ByteRange {
            start: code,
            end: code_end,
        }],
    );

    assert_eq!(result.text, "我們對`x`進行軟體處理。");
    assert_eq!(result.applied, 1);
    assert_eq!(result.skipped, 2);
}

#[test]
fn empty_issues() {
    let text = "hello";
    let result = apply_fixes(text, &[], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "hello");
    assert_eq!(result.applied, 0);
}

// -- Orthographic tier tests --

#[test]
fn orthographic_fixes_punctuation() {
    let text = "你好,世界";
    let issues = vec![make_punctuation_issue(6, ",", vec!["，"])];
    let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
    assert_eq!(result.text, "你好，世界");
    assert_eq!(result.applied, 1);
}

#[test]
fn orthographic_skips_lexical_issues() {
    let text = "這個軟件很好用";
    let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
    let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
    assert_eq!(result.text, text); // unchanged -- orthographic skips CrossStrait
    assert_eq!(result.skipped, 1);
}

// -- Anchor-match gating tests --

#[test]
fn lexical_safe_skips_anchor_rejected() {
    let text = "這個軟件很好用";
    let mut issue = make_issue(6, "軟件", vec!["軟體"]);
    issue.anchor_match = Some(false); // calibration rejected
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text); // unchanged -- anchor rejected
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_safe_applies_anchor_confirmed() {
    let text = "這個軟件很好用";
    let mut issue = make_issue(6, "軟件", vec!["軟體"]);
    issue.anchor_match = Some(true); // calibration confirmed
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個軟體很好用");
    assert_eq!(result.applied, 1);
}

#[test]
fn lexical_safe_applies_anchor_none() {
    let text = "這個軟件很好用";
    let issue = make_issue(6, "軟件", vec!["軟體"]);
    // anchor_match == None (no calibration) -- should apply unconditionally
    assert!(issue.anchor_match.is_none());
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個軟體很好用");
    assert_eq!(result.applied, 1);
}

#[test]
fn fix_modes_are_ordered_by_tier() {
    // The tier gate compares with "<", so its meaning rides on variant
    // declaration order. Reordering the enum compiles clean and passes clippy
    // while silently inverting the gate; this pins it.
    assert!(FixMode::None < FixMode::Orthographic);
    assert!(FixMode::Orthographic < FixMode::LexicalSafe);
    assert!(FixMode::LexicalSafe < FixMode::LexicalContextual);
}

#[test]
fn lexical_safe_skips_low_editorial_confidence() {
    let text = "需要優化性能";
    let mut issue = make_issue(6, "優化", vec!["最佳化"]);
    issue.editorial_confidence = Some(EditorialConfidence::Low);

    let safe = apply_fixes(text, &[issue.clone()], FixMode::LexicalSafe, &[]);
    assert_eq!(safe.text, text);
    assert_eq!(safe.skipped, 1);
    assert_eq!(safe.declined, 1, "the annotation is a judgment call");

    let contextual = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
    assert_eq!(contextual.text, "需要最佳化性能");
    assert_eq!(contextual.applied, 1);
}

#[test]
fn out_of_tier_issues_are_skipped_but_not_declined() {
    // "declined" is what the CLI prints, so it has to mean the fixer weighed
    // the issue and said no. A lexical issue under --fix=orthographic was never
    // in scope; counting it would make orthographic runs on ordinary prose
    // report every cross-strait term as a verdict the fixer never reached.
    let text = "這個軟件很好用";
    let issues = vec![make_issue(6, "軟件", vec!["軟體"])];

    let ortho = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
    assert_eq!(ortho.text, text);
    assert_eq!(ortho.skipped, 1);
    assert_eq!(ortho.declined, 0);

    // Same issue, same decision to leave it alone, but now on its merits.
    let ambiguous = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
    let safe = apply_fixes("這個視頻很好看", &ambiguous, FixMode::LexicalSafe, &[]);
    assert_eq!(safe.skipped, 1);
    assert_eq!(safe.declined, 1);
}

#[test]
fn orthographic_ignores_low_editorial_confidence() {
    // The gate is guarded by !orthographic: editorial confidence is a
    // lexical-judgment signal, so an orthographic issue carrying the annotation
    // is still fixed at every tier.
    let text = "他說,好";
    let mut issue = make_punctuation_issue(6, ",", vec!["，"]);
    issue.editorial_confidence = Some(EditorialConfidence::Low);

    let fixed = apply_fixes(text, &[issue], FixMode::Orthographic, &[]);
    assert_eq!(fixed.text, "他說，好");
    assert_eq!(fixed.applied, 1);
    assert_eq!(fixed.skipped, 0);
}

#[test]
fn lexical_contextual_respects_anchor_rejection_for_non_clue() {
    // Non-clue lexical issue with anchor rejection: LexicalContextual respects
    // it because there is no independent disambiguation signal.
    let text = "這個軟件很好用";
    let mut issue = make_issue(6, "軟件", vec!["軟體"]);
    issue.anchor_match = Some(false);
    let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
    assert_eq!(result.text, text); // unchanged -- anchor rejected, no clues
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_contextual_skips_tier2_suppressed_issue() {
    let text = "學習的進程需要耐心和毅力";
    let offset = text.find("進程").unwrap();
    let mut issue = make_issue(offset, "進程", vec!["行程"]);
    issue.tier2_outcome = Tier2Outcome::Suppressed;
    issue.severity = Severity::Info;
    let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
    assert_eq!(result.text, text);
    assert_eq!(result.skipped, 1);
}

// -- Combined anchor_match + context_clues tests --

#[test]
fn lexical_safe_skips_clue_rule_even_with_anchor_confirmed() {
    // anchor_match == Some(true) but has context_clues → LexicalSafe still
    // refuses because context-clue rules need LexicalContextual.
    let text = "我需要編寫一個程序來執行";
    let offset = text.find("程序").unwrap();
    let mut issue = make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    );
    issue.anchor_match = Some(true);
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text); // unchanged -- context_clues gate takes precedence
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_contextual_applies_clue_rule_despite_anchor_rejection() {
    // anchor_match == Some(false) + context_clues present. LexicalContextual
    // overrides anchor rejection and applies if segmenter confirms clues.
    let text = "我需要編寫一個程序來執行";
    let offset = text.find("程序").unwrap();
    let mut issue = make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    );
    issue.anchor_match = Some(false);
    let seg = Segmenter::new(
        ["編寫", "代碼", "執行", "開發", "程序", "程式"]
            .iter()
            .map(|s| s.to_string()),
    );
    let result =
        apply_fixes_with_context(text, &[issue], FixMode::LexicalContextual, &[], Some(&seg));
    assert_eq!(result.text, "我需要編寫一個程式來執行");
    assert_eq!(result.applied, 1);
}

// -- Context clue tests --

#[test]
fn lexical_safe_skips_issues_with_context_clues() {
    let text = "我需要編寫一個程序來執行";
    let offset = text.find("程序").unwrap();
    let issues = vec![make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    )];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text); // unchanged -- lexical_safe refuses context-clue rules
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_contextual_with_segmenter_applies_when_clues_match() {
    let text = "我需要編寫一個程序來執行";
    let offset = text.find("程序").unwrap();
    let issues = vec![make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    )];
    let seg = Segmenter::new(
        ["編寫", "代碼", "執行", "開發", "程序", "程式"]
            .iter()
            .map(|s| s.to_string()),
    );
    let result =
        apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
    assert_eq!(result.text, "我需要編寫一個程式來執行");
    assert_eq!(result.applied, 1);
}

#[test]
fn lexical_contextual_with_segmenter_skips_when_clues_insufficient() {
    let text = "這個程序很重要";
    let offset = text.find("程序").unwrap();
    let issues = vec![make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    )];
    let seg = Segmenter::new(
        ["編寫", "代碼", "執行", "開發", "程序", "程式"]
            .iter()
            .map(|s| s.to_string()),
    );
    let result =
        apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
    assert_eq!(result.text, text); // unchanged -- insufficient clues
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_contextual_without_segmenter_skips_clue_rules() {
    let text = "這個程序很重要";
    let offset = text.find("程序").unwrap();
    let issues = vec![make_issue_with_clues(
        offset,
        "程序",
        vec!["程式"],
        vec!["編寫", "代碼", "執行", "開發"],
    )];
    let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
    assert_eq!(result.text, text); // unchanged -- no segmenter, cannot verify clues
    assert_eq!(result.skipped, 1);
}

// -- AiStyle tier exclusion tests --

fn make_ai_style_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        suggestions.into_iter().map(String::from).collect(),
        IssueType::AiStyle,
        Severity::Info,
    )
}

#[test]
fn orthographic_skips_ai_style_issues() {
    let text = "這個系統作為核心元件";
    let offset = text.find("作為").unwrap();
    let issues = vec![make_ai_style_issue(offset, "作為", vec!["是"])];
    let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
    assert_eq!(result.text, text); // unchanged: AiStyle not orthographic
    assert_eq!(result.skipped, 1);
}

#[test]
fn lexical_safe_applies_single_suggestion_ai_style() {
    // Semantic safety words (意味著→表示) have a single suggestion and are
    // eligible for lexical_safe auto-fix.
    let text = "這個定義意味著所有值";
    let offset = text.find("意味著").unwrap();
    let issues = vec![make_ai_style_issue(offset, "意味著", vec!["表示"])];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個定義表示所有值");
    assert_eq!(result.applied, 1);
}

#[test]
fn lexical_safe_skips_ai_style_no_suggestions() {
    let text = "這意味著很多事情";
    let offset = text.find("意味著").unwrap();
    let issues = vec![make_ai_style_issue(offset, "意味著", vec![])];
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, text); // unchanged: no suggestion
    assert_eq!(result.skipped, 1);
}

#[test]
fn surrounding_window_basic() {
    let text = "AABBCCDDEE";
    let window = surrounding_window(text, 4, 6);
    // Window should include chars around the CC range
    assert!(window.contains('A'));
    assert!(window.contains('E'));
}

#[test]
fn surrounding_window_cjk() {
    let text = "我需要編寫一個程序來執行這個任務";
    let offset = text.find("程序").unwrap();
    let end = offset + "程序".len();
    let window = surrounding_window(text, offset, end);
    assert!(window.contains("編寫"));
    assert!(window.contains("執行"));
}

#[test]
fn surrounding_window_empty_text() {
    let window = surrounding_window("", 0, 0);
    assert_eq!(window, "");
}

#[test]
fn surrounding_window_at_boundaries() {
    // Match spans entire text -- window should return the whole string.
    let text = "程序";
    let window = surrounding_window(text, 0, text.len());
    assert_eq!(window, "程序");
}

// -- suppress_convergent_issues O(n) equivalence tests --

#[test]
fn suppress_convergent_o_n_matches_o_n2() {
    // Verify the O(n) forward-pass remap produces identical fix_ranges to the
    // old per-fix remap_to_post_fix approach.
    let cases: Vec<Vec<AppliedFix>> = vec![
        // Empty
        vec![],
        // Single fix, same length (no shift)
        vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: "軟體".into(),
        }],
        // Single fix, expansion (6 bytes -> 9 bytes)
        vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: "記憶體".into(),
        }],
        // Single fix, contraction (9 bytes -> 6 bytes)
        vec![AppliedFix {
            offset: 6,
            old_len: 9,
            replacement: "軟體".into(),
        }],
        // Single fix, deletion (6 bytes -> 0 bytes)
        vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: String::new(),
        }],
        // Two fixes, both same length
        vec![
            AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "軟體".into(),
            },
            AppliedFix {
                offset: 15,
                old_len: 6,
                replacement: "記憶".into(),
            },
        ],
        // Two fixes, first expands
        vec![
            AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "記憶體".into(),
            },
            AppliedFix {
                offset: 15,
                old_len: 6,
                replacement: "軟體".into(),
            },
        ],
        // Two fixes, first contracts
        vec![
            AppliedFix {
                offset: 6,
                old_len: 9,
                replacement: "AB".into(),
            },
            AppliedFix {
                offset: 20,
                old_len: 6,
                replacement: "CD".into(),
            },
        ],
        // Two fixes, first is deletion
        vec![
            AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: String::new(),
            },
            AppliedFix {
                offset: 15,
                old_len: 6,
                replacement: "XY".into(),
            },
        ],
        // Three fixes with mixed shifts
        vec![
            AppliedFix {
                offset: 0,
                old_len: 3,
                replacement: "ABCDE".into(),
            },
            AppliedFix {
                offset: 10,
                old_len: 6,
                replacement: "X".into(),
            },
            AppliedFix {
                offset: 20,
                old_len: 3,
                replacement: "YZW".into(),
            },
        ],
    ];

    for (i, fixes) in cases.iter().enumerate() {
        // O(n^2) reference: call remap_to_post_fix per fix
        let expected: Vec<(usize, usize)> = fixes
            .iter()
            .map(|fix| {
                let post = remap_to_post_fix(fix.offset, fixes);
                (post, post + fix.replacement.len())
            })
            .collect();

        // O(n) forward pass
        let mut delta: isize = 0;
        let actual: Vec<(usize, usize)> = fixes
            .iter()
            .map(|fix| {
                let post = (fix.offset as isize + delta).max(0) as usize;
                delta += fix.replacement.len() as isize - fix.old_len as isize;
                (post, post + fix.replacement.len())
            })
            .collect();

        assert_eq!(expected, actual, "case {i} mismatch: fixes={fixes:?}");
    }
}

#[test]
fn suppress_convergent_deletion_suppresses_touching_issue() {
    // A deletion (replacement is empty) should suppress issues that touch the
    // deletion point.
    let fixes = vec![AppliedFix {
        offset: 6,
        old_len: 6,
        replacement: String::new(),
    }];
    // Issue at post-fix offset 6 (the deletion point) should be suppressed.
    let mut issues = vec![make_issue(6, "XX", vec!["YY"])];
    suppress_convergent_issues(&mut issues, &fixes);
    assert!(
        issues.is_empty(),
        "issue touching deletion point should be suppressed"
    );
}

#[test]
fn suppress_convergent_preserves_non_overlapping_issue() {
    let fixes = vec![AppliedFix {
        offset: 6,
        old_len: 6,
        replacement: "軟體".into(),
    }];
    // Issue at offset 20, well past the fix range -- should survive.
    let mut issues = vec![make_issue(20, "內存", vec!["記憶體"])];
    suppress_convergent_issues(&mut issues, &fixes);
    assert_eq!(issues.len(), 1, "non-overlapping issue should be preserved");
}

#[test]
fn empty_context_clues_vec_treated_as_no_clues() {
    // Issue with context_clues: Some(vec![]) should NOT be skipped in
    // lexical_safe because the empty vec means no ambiguity.
    let text = "這個軟件很好用";
    let mut issue = make_issue(6, "軟件", vec!["軟體"]);
    issue.context_clues = Some(Arc::from(Vec::<String>::new()));
    let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
    assert_eq!(result.text, "這個軟體很好用");
    assert_eq!(result.applied, 1);
}

// remap_exclusions tests

use crate::engine::excluded::ByteRange;

fn br(start: usize, end: usize) -> ByteRange {
    ByteRange { start, end }
}

#[test]
fn remap_exclusions_no_fixes() {
    let excl = vec![br(10, 20), br(30, 40)];
    let result = remap_exclusions(&excl, &[]);
    assert_eq!(result, vec![br(10, 20), br(30, 40)]);
}

#[test]
fn remap_exclusions_fix_before_exclusion_grows() {
    // Fix at offset 5 replaces 2 bytes with 4 bytes (+2 delta). Exclusion at
    // (10, 20) should shift to (12, 22).
    let excl = vec![br(10, 20)];
    let fixes = vec![AppliedFix {
        offset: 5,
        old_len: 2,
        replacement: "abcd".to_string(),
    }];
    let result = remap_exclusions(&excl, &fixes);
    assert_eq!(result, vec![br(12, 22)]);
}

#[test]
fn remap_exclusions_fix_before_exclusion_shrinks() {
    // Fix at offset 2 replaces 4 bytes with 1 byte (-3 delta). Exclusion at
    // (10, 20) should shift to (7, 17).
    let excl = vec![br(10, 20)];
    let fixes = vec![AppliedFix {
        offset: 2,
        old_len: 4,
        replacement: "x".to_string(),
    }];
    let result = remap_exclusions(&excl, &fixes);
    assert_eq!(result, vec![br(7, 17)]);
}

#[test]
fn remap_exclusions_fix_after_exclusion() {
    // Fix at offset 25 is after the exclusion at (10, 20) -- no shift.
    let excl = vec![br(10, 20)];
    let fixes = vec![AppliedFix {
        offset: 25,
        old_len: 3,
        replacement: "abcdef".to_string(),
    }];
    let result = remap_exclusions(&excl, &fixes);
    assert_eq!(result, vec![br(10, 20)]);
}

#[test]
fn remap_exclusions_multiple_fixes_multiple_zones() {
    // Fix at 5: 2->4 (+2), fix at 25: 3->1 (-2). Exclusion (10,20) shifts by +2
    // -> (12,22). Exclusion (30,40) shifts by +2-2=0 -> (30,40).
    let excl = vec![br(10, 20), br(30, 40)];
    let fixes = vec![
        AppliedFix {
            offset: 5,
            old_len: 2,
            replacement: "abcd".to_string(),
        },
        AppliedFix {
            offset: 25,
            old_len: 3,
            replacement: "x".to_string(),
        },
    ];
    let result = remap_exclusions(&excl, &fixes);
    assert_eq!(result, vec![br(12, 22), br(30, 40)]);
}

#[test]
fn remap_exclusions_zero_length_insertion_at_boundary() {
    // Spacing fix: zero-length insertion (old_len=0) at offset 10, which is
    // exactly the exclusion start. The insertion should shift the exclusion
    // right by the replacement length.
    let excl = vec![br(10, 20)];
    let fixes = vec![AppliedFix {
        offset: 10,
        old_len: 0,
        replacement: " ".to_string(),
    }];
    let result = remap_exclusions(&excl, &fixes);
    assert_eq!(result, vec![br(11, 21)]);
}
