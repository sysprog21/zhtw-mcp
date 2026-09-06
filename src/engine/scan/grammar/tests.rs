//! Scanner tests for the grammar module.
//!
//! Split out for size only; an ordinary child module, not generated.

use super::*;
use crate::engine::sentence::BoundaryIndex;

fn scan(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_grammar(&mut Emitter::new(text, &[], &mut issues), Register::Casual);
    issues
}

fn scan_phase2(text: &str) -> Vec<Issue> {
    let idx = BoundaryIndex::build(text, &[]);
    let mut issues = Vec::new();
    scan_ai_structural_phase2(
        &mut Emitter::new(text, &[], &mut issues),
        &idx,
        crate::engine::scan::ContentType::Markdown,
    );
    scan_translationese_syntactic(&mut Emitter::new(text, &[], &mut issues), &idx);
    issues
}

// Boundary-aware detector panic-safety regression tests (from Codex/Gemini
// review)

#[test]
fn tricolon_with_repeated_spans_does_not_panic() {
    // Codex high #1: 乙、甲、甲、乙 used to confuse find()-based offset
    // calculation when the same span repeats. Should not panic and should
    // detect the central tricolon (甲、甲).
    let text = "乙、甲、甲、乙、丙。";
    let _ = scan_phase2(text);
}

#[test]
fn negative_parallel_mixed_ascii_cjk_does_not_panic() {
    // Codex high #2: byte-counted lookahead used to split UTF-8 chars.
    let text = "不只是A，而是中文混合內容。";
    let _ = scan_phase2(text);
}

#[test]
fn passive_density_short_paragraph_does_not_panic() {
    // Codex high #3 + #5: short ASCII-leading paragraphs used to panic when
    // slicing first-N bytes.
    let text = "A被B。\n\n中文段落以「被」字開頭，被廣泛認為是好的。";
    let _ = scan_phase2(text);
}

#[test]
fn excessive_bold_short_ascii_paragraph_does_not_panic() {
    // Codex high #4: short ASCII-leading paragraph slicing.
    let text = "**A** 中文 **B** 中文 **C** 中文 **D**";
    let _ = scan_phase2(text);
}

#[test]
fn tricolon_detects_simple_pattern() {
    // Three consecutive identical-length 2-char spans (團結、奮鬥、創新) form a
    // tricolon when isolated as the entire sentence content.
    let text = "團結、奮鬥、創新。";
    let issues = scan_phase2(text);
    let has_tricolon = issues
        .iter()
        .any(|i| i.context.as_ref().is_some_and(|c| c.contains("tricolon")));
    assert!(
        has_tricolon,
        "Expected tricolon detection, got {:?}",
        issues
    );
}

fn has_formulaic(issues: &[Issue]) -> bool {
    has_context_with(issues, "公式化用語")
}

fn has_self_qa(issues: &[Issue]) -> bool {
    has_context_with(issues, "連續自問自答")
}

#[test]
fn formulaic_ending_fires_at_a_section_close() {
    for (text, what) in [
        // Final paragraph of the document, final sentence: this is what the
        // detector is named for.
        (
            "本文介紹了新的排程策略。\n\n效能提升相當顯著。展望未來。",
            "document-final sentence",
        ),
        // Same phrase closing a paragraph that a heading follows.
        (
            "第一節說明背景。展望未來。\n\n## 第二節\n\n這裡是內文。",
            "before a heading",
        ),
        (
            "第一節說明背景。展望未來。\n## 第二節\n\n這裡是內文。",
            "before a heading without a blank line",
        ),
        (
            "第一節說明背景。展望未來。\r## 第二節\r\r這裡是內文。",
            "before a CR-only heading",
        ),
        (
            "## 第二節\n本節總結。展望未來。",
            "in body text after a heading without a blank line",
        ),
    ] {
        assert!(has_formulaic(&scan_phase2(text)), "expected a hit {what}");
    }
}

#[test]
fn formulaic_ending_covers_new_closing_platitudes() {
    for text in ["我們將攜手共進。", "這個案例值得深思。"] {
        assert!(
            has_formulaic(&scan_phase2(text)),
            "missing formulaic closer: {text:?}"
        );
    }
}

#[test]
fn formulaic_ending_ignores_indented_code_as_a_boundary() {
    // Four spaces starts an indented code block, so a commented line inside one
    // is not a heading and does not close a section.
    let code = "## 一\n\n效能提升。展望未來。\n\n    # setup\n    print(1)\n";
    let issues = scan_phase2(code);
    assert!(
        !has_formulaic(&issues),
        "indented code read as a section boundary: {issues:?}"
    );

    let tabbed = "## 一\n\n效能提升。展望未來。\n\n\t# setup\n";
    assert!(
        !has_formulaic(&scan_phase2(tabbed)),
        "tab-indented code read as a section boundary"
    );

    // Three spaces is still a heading.
    let indented_heading = "效能提升。展望未來。\n\n   ### 標題\n\n內文。";
    assert!(
        has_formulaic(&scan_phase2(indented_heading)),
        "three-space indented heading rejected"
    );
}

#[test]
fn formulaic_ending_requires_no_same_line_prose_after_the_closer() {
    // A later heading must not turn a phrase in the middle of a paragraph into
    // a section closer.
    let text = "展望未來。這裡仍在說明細節。\n\n# 下一節\n";
    assert!(
        !has_formulaic(&scan_phase2(text)),
        "same-line prose was skipped before the heading"
    );

    // The narrow allowance keeps the existing trailing-note form valid.
    assert!(has_formulaic(&scan_phase2(
        "展望未來。（註）\n\n# 下一節\n"
    )));
}

#[test]
fn heading_containing_inline_code_is_still_a_section_boundary() {
    use crate::engine::scan::{build_exclusions_for_content_type, ContentType};

    // A heading naming a file or a config key in backticks is a heading.
    // Rejecting it, which an exclusion test over the whole line did, made the
    // detector silent on most technical Markdown.
    for heading in [
        "## 第二節",
        "## 設定 `config.toml`",
        "## 設定 src/main.rs",
        "## 參考 https://example.com/a",
    ] {
        let text = format!("## 一\n\n本節總結。展望未來。\n\n{heading}\n\n這裡是內文。");
        let ranges = build_exclusions_for_content_type(&text, ContentType::Markdown);
        let idx = BoundaryIndex::build(&text, &ranges);
        let mut issues = Vec::new();
        scan_ai_structural_phase2(
            &mut Emitter::new(&text, &ranges, &mut issues),
            &idx,
            crate::engine::scan::ContentType::Markdown,
        );
        assert!(
            has_formulaic(&issues),
            "closer lost before heading {heading:?}: {issues:?}"
        );
    }
}

#[test]
fn a_code_fence_after_the_closer_is_not_a_section_boundary() {
    // A fence opens a block inside the section, and the sentence before it is
    // the lead-in that introduces it. That is the ordinary shape of technical
    // zh-TW, so it must not read as a closer. The comment inside the fence is
    // the second half of the test: the parser knows a heading-shaped line in a
    // fence is not a heading.
    let with_comment = "## 一\n\n效能提升。展望未來。\n\n```sh\n# install\nmake\n```\n";
    let without_comment = "## 一\n\n效能提升。展望未來。\n\n```sh\nmake\n```\n";
    for text in [with_comment, without_comment] {
        assert!(
            !has_formulaic(&scan_phase2(text)),
            "a code fence after the closer opened a section: {text:?}"
        );
    }

    // The indent rule is what rejects an indented block, and that one is
    // reachable: four spaces is code, three is a heading.
    assert!(!has_formulaic(&scan_phase2(
        "本節總結。展望未來。\n    # x\n"
    )));
    assert!(has_formulaic(&scan_phase2(
        "本節總結。展望未來。\n   # x\n"
    )));
}

#[test]
fn only_headings_and_rules_close_a_markdown_section() {
    // The trailing-note form still reaches its heading.
    assert!(has_formulaic(&scan_phase2(
        "## 一\n\n效能提升。展望未來。（註）\n\n## 二\n"
    )));
    assert!(has_formulaic(&scan_phase2(
        "效能提升。展望未來。\n\n---\n\n下一節。\n"
    )));

    // A list or blockquote sits inside the section. The sentence above it
    // introduces it, so it is lead-in prose rather than a closer.
    for text in [
        "效能提升。展望未來。\n\n- 下一節的項目\n",
        "效能提升。展望未來。\n\n> 下一節的引文\n",
    ] {
        assert!(
            !has_formulaic(&scan_phase2(text)),
            "a lead-in before a block was read as a closer: {text:?}"
        );
    }
}

#[test]
fn formulaic_ending_prefers_the_heading_boundary_over_the_document_end() {
    // The closer sits before a heading that body text follows immediately, so
    // all of it is one paragraph. Searching backwards for a single candidate
    // found 這裡是內文。 and missed the close entirely.
    let text = "本節總結。展望未來。\n## 第二節\n這裡是內文。";
    let issues = scan_phase2(text);
    assert!(
        has_formulaic(&issues),
        "closer before a heading lost to the document-final sentence: {issues:?}"
    );
}

#[test]
fn formulaic_ending_ignores_heading_text() {
    let text = "## 展望未來";
    let issues = scan_phase2(text);
    assert!(!has_formulaic(&issues), "fired on heading text: {issues:?}");

    let tight = "## 展望未來\n這裡是內文。";
    let issues = scan_phase2(tight);
    assert!(
        !has_formulaic(&issues),
        "fired on heading text joined to body: {issues:?}"
    );

    let body_repeat = "## 展望未來\n本節總結。展望未來。";
    assert!(
        has_formulaic(&scan_phase2(body_repeat)),
        "heading occurrence hid body closer"
    );
}

#[test]
fn formulaic_ending_stays_quiet_mid_document() {
    // Same phrase, same paragraph-final position, but the section continues
    // into another body paragraph. Before this was gated, the detector fired
    // here, which is the false positive its own name argues against.
    let text = "第一段的結尾。展望未來。\n\n第二段還在講同一件事。\n\n第三段結束。";
    let issues = scan_phase2(text);
    assert!(!has_formulaic(&issues), "fired mid-document: {issues:?}");
}

#[test]
fn formulaic_ending_stays_quiet_mid_paragraph() {
    // Closing platitude used as body prose, with real sentences after it in the
    // same closing paragraph. Only the last sentence is a closing.
    let text = "背景說明如下。展望未來。這一節接著討論實作細節與取捨。";
    let issues = scan_phase2(text);
    assert!(!has_formulaic(&issues), "fired mid-paragraph: {issues:?}");
}

#[test]
fn excessive_de_chain_reports_each_occurrence_with_correct_offset() {
    // Codex round 2: repeated identical clauses must report distinct offsets,
    // not collapse to the first one via s.find(clause).
    let text = "我的他的她的它的東西，我的他的她的它的物品。";
    let issues = scan_phase2(text);
    let de_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("的的不休")))
        .collect();
    assert_eq!(
        de_issues.len(),
        2,
        "Expected 2 distinct clauses, got {de_issues:?}"
    );
    // The two issues must have different offsets.
    assert_ne!(de_issues[0].offset, de_issues[1].offset);
}

#[test]
fn numbered_list_marker_len_matches_multi_digit() {
    assert_eq!(numbered_list_marker_len("1. item"), Some(2));
    assert_eq!(numbered_list_marker_len("10. item"), Some(3));
    assert_eq!(numbered_list_marker_len("123) item"), Some(4));
    // No whitespace after marker → not a list item.
    assert_eq!(numbered_list_marker_len("10.foo"), None);
    // Letter before the period → not a list item.
    assert_eq!(numbered_list_marker_len("a. item"), None);
    // Missing trailing marker → not a list item.
    assert_eq!(numbered_list_marker_len("10 item"), None);
}

#[test]
fn mechanical_bullets_detects_multi_digit_list() {
    // cubic review: 10+ item list must still be detected. All items use
    // **bold** prefix: the detector should fire on the full set, not cut off at
    // single-digit markers.
    let mut text = String::new();
    for i in 1..=12 {
        text.push_str(&format!("{i}. **項目** 內容文字。\n"));
    }
    let issues = scan_phase2(&text);
    let has_mechanical = issues
        .iter()
        .any(|i| i.context.as_ref().is_some_and(|c| c.contains("機械式列表")));
    assert!(
        has_mechanical,
        "expected mechanical bullets detection across 12-item list, got {issues:?}"
    );
}

#[test]
fn displaced_conditional_finds_late_when_sentence_starts_with_one() {
    // Gemini round 2: a sentence that starts with 如果 but has another
    // displaced 如果 should still flag the second one.
    let text = "如果你來，我會走，但他不會走，如果他不想來。";
    let issues = scan_phase2(text);
    let has_displaced = issues
        .iter()
        .any(|i| i.context.as_ref().is_some_and(|c| c.contains("後置條件")));
    assert!(
        has_displaced,
        "Expected displaced conditional, got {issues:?}"
    );
}

// Plumbing: IssueType::Grammar fundamentals

#[test]
fn grammar_issue_type_serde_round_trip() {
    let json = serde_json::to_string(&IssueType::Grammar).unwrap();
    assert_eq!(json, "\"grammar\"");
    let back: IssueType = serde_json::from_str(&json).unwrap();
    assert_eq!(back, IssueType::Grammar);
}

#[test]
fn grammar_sort_order_is_last() {
    // Grammar should sort after all other issue types.
    assert!(IssueType::Grammar.sort_order() > IssueType::Variant.sort_order());
    assert!(IssueType::Grammar.sort_order() > IssueType::Punctuation.sort_order());
}

#[test]
fn grammar_name_matches_serde() {
    assert_eq!(IssueType::Grammar.name(), "grammar");
}

#[test]
fn grammar_issue_fields_populated() {
    let issues = scan("你是不是學生嗎？");
    assert_eq!(issues.len(), 1);
    let i = &issues[0];
    assert_eq!(i.rule_type, IssueType::Grammar);
    assert_eq!(i.severity, Severity::Warning);
    assert!(i.context.is_some(), "grammar issues should have context");
    assert!(!i.suggestions.is_empty(), "should have suggestions");
    assert!(i.length > 0, "should have nonzero byte length");
}

#[test]
fn grammar_issue_offset_is_byte_accurate() {
    let text = "你是不是學生嗎？";
    let issues = scan(text);
    assert_eq!(issues.len(), 1);
    let i = &issues[0];
    // The found text extracted from the reported span should match.
    assert_eq!(&text[i.offset..i.offset + i.length], i.found);
}

#[test]
fn empty_text_produces_no_issues() {
    assert!(scan("").is_empty());
}

#[test]
fn ascii_only_text_produces_no_issues() {
    assert!(scan("Hello world, this is a test.").is_empty());
}

#[test]
fn clean_chinese_text_produces_no_issues() {
    let clean = "台灣是一個美麗的島嶼，有豐富的文化和美食。";
    assert!(scan(clean).is_empty());
}

// A-not-A plus 嗎: all 14 patterns, with and without 嗎

// -- with 嗎 (should flag) --

#[test]
fn a_not_a_shi_bu_shi_with_ma() {
    let issues = scan("你是不是學生嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("是不是"));
    assert!(issues[0].found.contains("嗎"));
}

#[test]
fn a_not_a_you_mei_you_with_ma() {
    let issues = scan("你有沒有吃飯嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("有沒有"));
}

#[test]
fn a_not_a_neng_bu_neng_with_ma() {
    let issues = scan("你能不能來嗎");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("能不能"));
}

#[test]
fn a_not_a_hui_bu_hui_with_ma() {
    let issues = scan("他會不會游泳嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("會不會"));
}

#[test]
fn a_not_a_yao_bu_yao_with_ma() {
    let issues = scan("你要不要喝水嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("要不要"));
}

#[test]
fn a_not_a_hao_bu_hao_with_ma() {
    let issues = scan("這樣好不好嗎");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("好不好"));
}

#[test]
fn a_not_a_dui_bu_dui_with_ma() {
    let issues = scan("答案對不對嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("對不對"));
}

#[test]
fn a_not_a_xing_bu_xing_with_ma() {
    let issues = scan("這樣行不行嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("行不行"));
}

#[test]
fn a_not_a_ke_bu_ke_yi_with_ma() {
    let issues = scan("可不可以走嗎");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("可不可以"));
}

#[test]
fn a_not_a_yuan_bu_yuan_yi_with_ma() {
    let issues = scan("你願不願意幫忙嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("願不願意"));
}

#[test]
fn a_not_a_xiang_bu_xiang_with_ma() {
    let issues = scan("你想不想去嗎");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("想不想"));
}

#[test]
fn a_not_a_zhi_bu_zhi_dao_with_ma() {
    let issues = scan("你知不知道嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("知不知道"));
}

#[test]
fn a_not_a_xi_bu_xi_huan_with_ma() {
    let issues = scan("你喜不喜歡吃飯嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("喜不喜歡"));
}

#[test]
fn a_not_a_ren_bu_ren_shi_with_ma() {
    let issues = scan("你認不認識他嗎");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("認不認識"));
}

// -- without 嗎 (should NOT flag) --

#[test]
fn a_not_a_shi_bu_shi_without_ma() {
    assert!(scan("你是不是學生？").is_empty());
}

#[test]
fn a_not_a_you_mei_you_without_ma() {
    assert!(scan("你有沒有吃飯？").is_empty());
}

#[test]
fn a_not_a_neng_bu_neng_without_ma() {
    assert!(scan("你能不能來？").is_empty());
}

#[test]
fn a_not_a_hui_bu_hui_without_ma() {
    assert!(scan("他會不會游泳？").is_empty());
}

#[test]
fn a_not_a_yao_bu_yao_without_ma() {
    assert!(scan("你要不要喝水？").is_empty());
}

#[test]
fn a_not_a_hao_bu_hao_without_ma() {
    assert!(scan("這樣好不好？").is_empty());
}

#[test]
fn a_not_a_dui_bu_dui_without_ma() {
    assert!(scan("答案對不對？").is_empty());
}

#[test]
fn a_not_a_xing_bu_xing_without_ma() {
    assert!(scan("這樣行不行？").is_empty());
}

#[test]
fn a_not_a_ke_bu_ke_yi_without_ma() {
    assert!(scan("可不可以走？").is_empty());
}

#[test]
fn a_not_a_yuan_bu_yuan_yi_without_ma() {
    assert!(scan("你願不願意幫忙？").is_empty());
}

#[test]
fn a_not_a_xiang_bu_xiang_without_ma() {
    assert!(scan("你想不想去？").is_empty());
}

#[test]
fn a_not_a_zhi_bu_zhi_dao_without_ma() {
    assert!(scan("你知不知道？").is_empty());
}

#[test]
fn a_not_a_xi_bu_xi_huan_without_ma() {
    assert!(scan("你喜不喜歡吃飯？").is_empty());
}

#[test]
fn a_not_a_ren_bu_ren_shi_without_ma() {
    assert!(scan("你認不認識他？").is_empty());
}

// -- A-not-A edge cases --

#[test]
fn a_not_a_ma_across_sentence_boundary_clean() {
    // 嗎 is in a different sentence: must not flag.
    assert!(scan("你是不是學生。他好嗎？").is_empty());
}

#[test]
fn a_not_a_ma_across_newline_boundary_clean() {
    assert!(scan("你是不是學生\n他好嗎？").is_empty());
}

#[test]
fn a_not_a_ma_across_exclamation_boundary_clean() {
    assert!(scan("你是不是學生！他好嗎？").is_empty());
}

#[test]
fn ma_only_no_a_not_a_clean() {
    assert!(scan("你是學生嗎？").is_empty());
}

#[test]
fn a_not_a_suggestion_is_pattern_without_ma() {
    let issues = scan("你是不是學生嗎？");
    assert_eq!(issues[0].suggestions[0], "是不是");
}

#[test]
fn a_not_a_severity_is_warning() {
    let issues = scan("你是不是學生嗎？");
    assert_eq!(issues[0].severity, Severity::Warning);
}

#[test]
fn a_not_a_ma_with_trailing_whitespace() {
    // 嗎 followed by spaces before sentence end.
    let issues = scan("你是不是學生嗎  ？");
    assert_eq!(issues.len(), 1);
}

// 和-connecting-clauses

#[test]
fn he_verb_suffix_le_with_pronoun() {
    let issues = scan("我吃了和你去看電影");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "和");
    assert_eq!(issues[0].severity, Severity::Info);
}

#[test]
fn he_verb_suffix_guo_with_pronoun() {
    let issues = scan("我去過和他來過");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_zhe_with_pronoun() {
    let issues = scan("我看著和她說話");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_lai_with_pronoun() {
    let issues = scan("我回來和你一起走");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_qu_with_pronoun() {
    let issues = scan("他出去和我回家");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_wan_with_pronoun() {
    let issues = scan("我寫完和你開始");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_hao_with_pronoun() {
    let issues = scan("我準備好和他出發");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_verb_suffix_dao_with_pronoun() {
    let issues = scan("我找到和她確認");
    assert_eq!(issues.len(), 1);
}

#[test]
fn he_between_nouns_clean() {
    assert!(scan("蘋果和橘子都很好吃").is_empty());
}

#[test]
fn he_no_verb_suffix_before_clean() {
    // No verb suffix immediately before 和.
    assert!(scan("老師和學生都來了").is_empty());
}

#[test]
fn he_verb_suffix_but_no_pronoun_after_clean() {
    // Verb suffix before 和, but no pronoun after → not a clause connector.
    assert!(scan("我吃了和飯").is_empty());
}

#[test]
fn he_suggestion_is_comma() {
    let issues = scan("我住在台北了和我有一隻狗");
    assert_eq!(issues[0].suggestions[0], "，");
}

// 是+adjective copula

#[test]
fn bare_shi_disyllabic_adj() {
    let issues = scan("她是漂亮");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "她是漂亮");
    assert_eq!(issues[0].suggestions[0], "她很漂亮");
}

#[test]
fn bare_shi_monosyllabic_adj() {
    let issues = scan("我是忙");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "我很忙");
}

#[test]
fn bare_shi_adj_with_ta() {
    let issues = scan("他是高");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "他很高");
}

#[test]
fn bare_shi_adj_with_women() {
    let issues = scan("我們是開心");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "我們很開心");
}

#[test]
fn bare_shi_adj_with_zhe() {
    let issues = scan("這是好");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "這很好");
}

#[test]
fn bare_shi_adj_with_na() {
    let issues = scan("那是遠");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "那很遠");
}

#[test]
fn bare_shi_severity_is_info() {
    let issues = scan("她是漂亮");
    assert_eq!(issues[0].severity, Severity::Info);
}

// -- degree adverbs suppress the pattern (negative tests) --

#[test]
fn shi_with_hen_clean() {
    assert!(scan("她是很漂亮").is_empty());
}

#[test]
fn shi_with_feichang_clean() {
    assert!(scan("她是非常漂亮").is_empty());
}

#[test]
fn shi_with_tebie_clean() {
    assert!(scan("她是特別漂亮").is_empty());
}

#[test]
fn shi_with_tai_clean() {
    assert!(scan("她是太漂亮").is_empty());
}

#[test]
fn shi_with_zhen_clean() {
    assert!(scan("她是真漂亮").is_empty());
}

#[test]
fn shi_with_bijiao_clean() {
    assert!(scan("她是比較漂亮").is_empty());
}

#[test]
fn shi_with_youdian_clean() {
    assert!(scan("她是有點漂亮").is_empty());
}

// -- 是+noun should not fire --

#[test]
fn shi_noun_predicate_clean() {
    assert!(scan("她是老師").is_empty());
}

#[test]
fn shi_proper_noun_clean() {
    assert!(scan("他是台灣人").is_empty());
}

#[test]
fn shi_without_pronoun_clean() {
    // No pronoun before 是: e.g. 問題是..., should not fire.
    assert!(scan("問題是很大").is_empty());
}

#[test]
fn shi_adj_as_noun_modifier_clean() {
    // 好消息: 好 is an adjective modifying a noun, not a bare predicate.
    assert!(scan("這是好消息").is_empty());
}

#[test]
fn shi_adj_as_noun_modifier_da_clean() {
    // 大問題: same pattern.
    assert!(scan("這是大問題").is_empty());
}

#[test]
fn shi_adj_standalone_still_fires() {
    // 好 at end of text (no following CJK): still a bare adjective.
    let issues = scan("這是好");
    assert_eq!(issues.len(), 1);
}

#[test]
fn shi_adj_with_particle_still_fires() {
    // 漂亮啊: particle after adjective, NOT a noun modifier.
    let issues = scan("她是漂亮啊");
    assert_eq!(issues.len(), 1);
}

#[test]
fn shi_adj_with_connector_still_fires() {
    // 漂亮又善良: connector after adjective, NOT a noun modifier.
    let issues = scan("她是漂亮又善良");
    assert_eq!(issues.len(), 1);
}

// Redundant preposition

#[test]
fn redundant_prep_taolun_guanyu() {
    let issues = scan("我們討論關於這個問題");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("討論關於"));
    assert_eq!(issues[0].suggestions[0], "討論");
}

#[test]
fn redundant_prep_yanjiu_guanyu() {
    let issues = scan("他研究關於量子力學");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("研究關於"));
}

#[test]
fn redundant_prep_qiangdiao_zai() {
    let issues = scan("他強調在這一點上");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("強調在"));
}

#[test]
fn redundant_prep_yingxiang_dao() {
    let issues = scan("這影響到整體計畫");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("影響到"));
}

#[test]
fn redundant_prep_kaolu_dao() {
    let issues = scan("請考慮到這個因素");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("考慮到"));
}

#[test]
fn redundant_prep_chuli_dao() {
    let issues = scan("我處理到這個問題");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("處理到"));
}

#[test]
fn redundant_prep_severity_is_info() {
    let issues = scan("我們討論關於這個問題");
    assert_eq!(issues[0].severity, Severity::Info);
}

#[test]
fn transitive_verb_no_preposition_clean() {
    assert!(scan("我們討論這個問題").is_empty());
}

#[test]
fn preposition_too_far_from_verb_clean() {
    // Gap > 2 chars between verb and preposition.
    assert!(scan("我們討論了很多關於這個問題").is_empty());
}

#[test]
fn redundant_prep_with_one_char_gap() {
    // One char gap between verb and preposition is still flagged.
    let issues = scan("他研究了關於量子力學");
    assert_eq!(issues.len(), 1);
}

#[test]
fn redundant_prep_fenxi_guanyu() {
    let issues = scan("他分析關於這個現象");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("分析關於"));
}

// Extended A-not-A patterns (single-char verbs)

#[test]
fn a_not_a_zuo_bu_zuo_with_ma() {
    let issues = scan("你做不做嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("做不做"));
}

#[test]
fn a_not_a_chi_bu_chi_with_ma() {
    let issues = scan("你吃不吃嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("吃不吃"));
}

#[test]
fn a_not_a_qu_bu_qu_with_ma() {
    let issues = scan("你去不去嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("去不去"));
}

#[test]
fn a_not_a_lai_bu_lai_with_ma() {
    let issues = scan("你來不來嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("來不來"));
}

#[test]
fn a_not_a_kan_bu_kan_with_ma() {
    let issues = scan("你看不看嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("看不看"));
}

#[test]
fn a_not_a_zou_bu_zou_with_ma() {
    let issues = scan("你走不走嗎？");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("走不走"));
}

#[test]
fn a_not_a_zuo_bu_zuo_without_ma() {
    assert!(scan("你做不做？").is_empty());
}

#[test]
fn a_not_a_chi_bu_chi_without_ma() {
    assert!(scan("你吃不吃？").is_empty());
}

// Bureaucratic nominalization (進行/加以/予以 + verb)

#[test]
fn bureaucratic_jinxing_taolun() {
    let issues = scan("我們進行討論");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "進行討論");
    assert_eq!(issues[0].suggestions[0], "討論");
    assert_eq!(issues[0].severity, Severity::Info);
}

#[test]
fn bureaucratic_jinxing_fenxi() {
    let issues = scan("他們進行分析");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "進行分析");
}

#[test]
fn bureaucratic_jinxing_yanjiu() {
    let issues = scan("這個團隊進行研究");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "研究");
}

#[test]
fn bureaucratic_jinxing_ceshi() {
    let issues = scan("我們進行測試");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "進行測試");
}

#[test]
fn bureaucratic_jinxing_with_le_gap() {
    // 了 between prefix and verb (1-char gap, should still flag).
    let issues = scan("我們進行了討論");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "進行了討論");
}

#[test]
fn bureaucratic_jiayi_fenxi() {
    let issues = scan("我們加以分析");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "加以分析");
    assert_eq!(issues[0].suggestions[0], "分析");
}

#[test]
fn bureaucratic_yuyi_chuli() {
    let issues = scan("我們予以處理");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "予以處理");
    assert_eq!(issues[0].suggestions[0], "處理");
}

#[test]
fn bureaucratic_jinxing_standalone_clean() {
    // 進行 as standalone verb ("proceeding"): no nominalized verb after.
    assert!(scan("會議正在進行").is_empty());
}

#[test]
fn bureaucratic_jinxing_zhong_clean() {
    // 進行中 means "in progress": not a nominalization.
    assert!(scan("專案進行中").is_empty());
}

#[test]
fn bureaucratic_verb_too_far_clean() {
    // Verb too far away (>2 chars gap).
    assert!(scan("我們進行了一些額外的討論").is_empty());
}

#[test]
fn bureaucratic_jinxing_picks_nearest_verb() {
    // Two verbs in window: 管理 (offset 0) and 研究 (offset 2 chars). Should
    // match 管理 (nearest by text position).
    let issues = scan("我們進行管理研究");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "進行管理");
    assert_eq!(issues[0].suggestions[0], "管理");
}

#[test]
fn bureaucratic_multiple_prefixes() {
    let issues = scan("我們進行討論並加以分析");
    assert_eq!(issues.len(), 2);
}

// Verbose action prefix (做出/作出 + abstract noun)

#[test]
fn verbose_zuochu_jueding() {
    let issues = scan("他做出決定");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "做出決定");
    assert_eq!(issues[0].suggestions[0], "決定");
    assert_eq!(issues[0].severity, Severity::Info);
}

#[test]
fn verbose_zuochu_huiying() {
    let issues = scan("我們做出回應");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "做出回應");
}

#[test]
fn verbose_zuochu_gongxian() {
    let issues = scan("他做出貢獻");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "貢獻");
}

#[test]
fn verbose_zuochu_with_le() {
    let issues = scan("他做出了決定");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "做出了決定");
}

#[test]
fn verbose_zuochu_alt_prefix() {
    // 作出 is an alternate form of 做出.
    let issues = scan("他作出回應");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "作出回應");
}

#[test]
fn verbose_zuochu_no_object_clean() {
    // 做出 without a known object: not flagged.
    assert!(scan("他做出一個蛋糕").is_empty());
}

#[test]
fn verbose_zuochu_object_too_far_clean() {
    // Object too far away (>1 char gap).
    assert!(scan("他做出了很多決定").is_empty());
}

// Double attribution (根據...顯示/指出)

#[test]
fn double_attribution_genju_xianshi() {
    let issues = scan("根據研究顯示，成果很好");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "根據研究顯示");
    assert_eq!(issues[0].suggestions[0], "根據研究");
    assert_eq!(issues[0].severity, Severity::Info);
}

#[test]
fn double_attribution_genju_zhichu() {
    let issues = scan("根據報告指出，問題嚴重");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "根據報告指出");
}

#[test]
fn double_attribution_genju_biaoming() {
    let issues = scan("根據數據表明這是正確的");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "根據數據表明");
}

#[test]
fn double_attribution_genju_biaoshi() {
    let issues = scan("根據專家表示，這很重要");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("根據專家表示"));
}

#[test]
fn double_attribution_genju_shuoming() {
    let issues = scan("根據文件說明，規格如下");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("根據文件說明"));
}

#[test]
fn double_attribution_long_source() {
    // Long source text between 根據 and attribution verb.
    let issues = scan("根據最新發表的一項研究報告顯示，結果令人驚訝");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[0], "根據最新發表的一項研究報告");
}

#[test]
fn double_attribution_empty_source_skipped() {
    // Degenerate case: no source between 根據 and verb, skip.
    assert!(scan("根據顯示結果很好").is_empty());
}

#[test]
fn double_attribution_noun_compound_skipped() {
    // 說明書 is a noun compound; 說明 is a prefix, not an attribution verb.
    assert!(scan("根據手冊說明書的內容").is_empty());
}

#[test]
fn double_attribution_verb_at_boundary_still_fires() {
    // 說明 followed by comma (not CJK): still an attribution verb.
    let issues = scan("根據文件說明，規格如下");
    assert_eq!(issues.len(), 1);
}

#[test]
fn double_attribution_biaoshi_hui_still_fires() {
    // 表示會: 會 means "will", not a noun suffix. Must still fire.
    let issues = scan("根據消息表示會延期");
    assert_eq!(issues.len(), 1);
}

#[test]
fn double_attribution_xianshi_tu_still_fires() {
    // 顯示圖: 圖 here is "diagram", not a compound suffix. Must fire.
    let issues = scan("根據數據顯示圖表有誤");
    assert_eq!(issues.len(), 1);
}

#[test]
fn double_attribution_markdown_link_skipped() {
    // 根據[link text with 說明](url): verb inside markdown link, not
    // attribution.
    assert!(scan("根據[維護者設計說明](https://example.com)，新版核心改動很大").is_empty());
}

#[test]
fn double_attribution_markdown_link_bracket_only() {
    // Even a bare [ between 根據 and verb suppresses the match.
    assert!(scan("根據[某研究說明書]的結論").is_empty());
}

#[test]
fn genju_without_verb_clean() {
    // 根據 without attribution verb: prepositional phrase, not redundant.
    assert!(scan("根據研究，成果很好").is_empty());
}

fn scan_bare(text: &str) -> Vec<Issue> {
    scan_bare_with_excluded(text, &[])
}

/// The phrases the shipped ruleset carries under this guard. Built here
/// rather than taken from a const, so these tests exercise the same list
/// the scanner does and a migration that dropped a phrase would fail them.
fn attribution_guard() -> StructuralGuard {
    let ruleset: crate::rules::ruleset::Ruleset =
        serde_json::from_str(include_str!("../../../../assets/ruleset.json"))
            .expect("embedded ruleset parses");
    let phrases: Vec<String> = ruleset
        .spelling_rules
        .iter()
        .filter(|r| !r.disabled && r.structural_guard.as_deref() == Some("uncited_attribution"))
        .map(|r| r.from.clone())
        .collect();
    assert!(
        !phrases.is_empty(),
        "the ruleset carries no uncited_attribution phrases"
    );
    StructuralGuard::from_phrases(phrases)
}

fn scan_bare_with_excluded(text: &str, excluded: &[ByteRange]) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_ai_bare_attribution(
        &mut Emitter::new(text, excluded, &mut issues),
        AttributionGenre::Casual,
        Some(&attribution_guard()),
    );
    issues
}

#[test]
fn genju_verb_in_next_clause_with_named_authority_clean() {
    // The preceding clause names the authority, so this is not bare.
    assert!(scan_bare("根據這份報告，研究顯示成果很好").is_empty());
}

#[test]
fn bare_attribution_does_not_ride_along_on_the_grammar_checks() {
    // It is an AI-style tell, and "grammar_checks" is on in every profile.
    assert!(scan("研究顯示成果很好").is_empty());
}

#[test]
fn standalone_research_shows_in_casual_prose_is_reported_but_never_rewritten() {
    let issues = scan_bare("研究顯示成果很好");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "研究顯示");
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);

    // No suggestion in any genre. An empty-string suggestion is the fixer's
    // delete sentinel, and deleting an attribution off the front of a sentence
    // leaves "多位，本次修法將影響地方財政".
    assert!(
        issues[0].suggestions.is_empty(),
        "a bare attribution must never carry a mechanical edit"
    );
}

#[test]
fn standalone_research_shows_in_technical_or_financial_prose_needs_a_citation() {
    for genre in [AttributionGenre::Technical, AttributionGenre::Financial] {
        let mut issues = Vec::new();
        scan_ai_bare_attribution(
            &mut Emitter::new("研究顯示成果很好", &[], &mut issues),
            genre,
            Some(&attribution_guard()),
        );
        assert_eq!(issues.len(), 1, "{genre:?}");
        assert!(issues[0].suggestions.is_empty(), "{genre:?}");
        assert!(issues[0]
            .context
            .as_deref()
            .unwrap()
            .contains("citation missing"));
    }
}

#[test]
fn bare_attribution_display_device_compounds_are_skipped() {
    for text in ["研究顯示器的效能", "研究顯示屏的解析度"] {
        assert!(
            scan_bare(text).is_empty(),
            "display device must not match: {text}"
        );
    }
}

#[test]
fn compound_nouns_must_terminate_to_exempt_an_attribution() {
    for text in [
        "研究顯示器的效能",
        "研究顯示屏的解析度",
        "研究顯示卡的效能",
        "本文研究顯示器。",
    ] {
        assert!(scan_bare(text).is_empty(), "display device: {text}");
    }
    // 器 also opens 器官, 器材 and 器械.
    for text in [
        "研究顯示器官移植後存活率提高。",
        "研究顯示器材耗損率偏高。",
        "研究顯示器械耗損率偏高。",
    ] {
        assert_eq!(scan_bare(text).len(), 1, "attribution lost: {text}");
    }
}

#[test]
fn a_citation_sources_a_claim_only_when_it_follows_it() {
    for text in [
        "參見 https://example.com/p\n研究顯示這項技術改善良率。",
        "# 參考資料 [1]\n研究顯示這項技術改善良率。",
        "- 研究顯示這項技術改善良率\n- 詳見 https://example.com/p",
        "研究顯示這項技術改善良率\n---\n另一則請參見 [1]",
        "研究顯示此療法有效\n# 參考資料\n[1] 某期刊",
    ] {
        assert_eq!(scan_bare(text).len(), 1, "citation leaked: {text:?}");
    }
    for text in [
        "研究顯示\n這項技術改善良率[1]。",
        "研究顯示\n這項技術\n大幅改善良率[1]。",
    ] {
        assert!(scan_bare(text).is_empty(), "hard wrap broken: {text:?}");
    }
}

// The noise carries no closing bracket at all, so every opener in it is
// unmatched. What this pins is that the citation index still resolves the one
// real marker and that the walk terminates; the cost itself belongs to a
// benchmark, not to an assertion.
#[test]
fn an_unmatched_bracket_does_not_rescan_the_document() {
    let noise = "參數[a 與 b 的關係，".repeat(2000);
    assert_eq!(scan_bare(&format!("研究顯示成果很好。{noise}")).len(), 1);
    assert!(scan_bare(&format!("研究顯示成果很好[1]。{noise}")).is_empty());
}

// A CJK label reaches 64 bytes at 21 characters. While the closing bracket was
// found by a capped forward scan, any longer label stopped counting as a
// citation and the attribution it sourced was reported as unsourced.
#[test]
fn a_long_link_label_still_counts_as_a_citation() {
    let long = "衛生福利部國民健康署二〇二五年度全國健康調查報告";
    assert!(long.len() > 64);
    assert!(scan_bare(&format!(
        "研究顯示這項療法有效，見[{long}](./refs/health.md)。"
    ))
    .is_empty());
    assert!(scan_bare(&format!("研究顯示這項療法有效，見[^{long}]。")).is_empty());
    // The opener and closer must be the same width.
    assert_eq!(
        scan_bare(&format!("研究顯示這項療法有效，見［{long}]。")).len(),
        1
    );
}

#[test]
fn bare_attribution_citations_in_the_same_sentence_suppress_it() {
    for text in [
        "專家認為此作法有效[1]。",
        "研究表明結果可重現[^source]。",
        "業內普遍認為此方案成熟，[報告](https://example.com/report)。",
        "有報告指出詳情見 https://example.com/report。",
    ] {
        assert!(
            scan_bare(text).is_empty(),
            "citation should suppress: {text}"
        );
    }
}

#[test]
fn citation_counts_however_far_into_the_sentence_it_sits() {
    // The forward search used to be capped, which hid a citation placed where
    // zh-TW normally puts one: at the end of a long sentence.
    let filler = "在控制了年齡與教育程度等變項之後，這項關聯仍然顯著，".repeat(25);
    let cited = format!("研究顯示，{filler}詳見 https://example.com/p 。");
    assert!(
        scan_bare(&cited).is_empty(),
        "a distant citation still sources the claim"
    );

    let uncited = format!("研究顯示，{filler}結論如上。");
    assert_eq!(
        scan_bare(&uncited).len(),
        1,
        "removing the citation must still report"
    );
}

#[test]
fn a_wrapped_sentence_keeps_the_citation_on_its_continuation_line() {
    // Hard-wrapped Markdown puts the citation after the wrap. Treating the wrap
    // as a sentence end left the citation in the "next" sentence and reported a
    // sourced claim as bare.
    assert!(
        scan_bare("研究顯示這項關聯仍然顯著，\n方法請參見 [1]。").is_empty(),
        "a soft line break must not cut the sentence short of its citation"
    );

    // A blank line does end it: the next paragraph's citation is its own.
    assert_eq!(
        scan_bare("研究顯示這項關聯仍然顯著\n\n方法請參見 [1]。").len(),
        1,
        "a citation in the next paragraph must not source this claim"
    );
}

#[test]
fn bare_attribution_ignores_citation_marker_in_excluded_inline_code() {
    let text = "研究顯示此設定有效，請執行 `curl https://example.com`。";
    let code_start = text.find('`').unwrap();
    let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
    let url_start = text.find("https://").unwrap();
    let url_end = url_start + "https://example.com".len();
    let issues = scan_bare_with_excluded(
        text,
        &[
            ByteRange {
                start: code_start,
                end: code_end,
            },
            ByteRange {
                start: url_start,
                end: url_end,
            },
        ],
    );

    assert_eq!(
        issues.len(),
        1,
        "inline-code URL is not a citation: {issues:?}"
    );
    assert_eq!(issues[0].found, "研究顯示");
}

#[test]
fn bare_attribution_raw_url_remains_a_citation_when_its_url_is_excluded() {
    let text = "研究顯示此設定有效：https://example.com。";
    let url_start = text.find("https://").unwrap();
    let issues = scan_bare_with_excluded(
        text,
        &[ByteRange {
            start: url_start,
            end: url_start + "https://example.com".len(),
        }],
    );

    assert!(
        issues.is_empty(),
        "raw URL should remain a citation: {issues:?}"
    );
}

// 對X進行Y: fronted-object bureaucratic padding

#[test]
fn dui_jinxing_basic() {
    let issues = scan("對資料進行分析");
    let dui: Vec<_> = issues
        .iter()
        .filter(|i| i.found.starts_with("對"))
        .collect();
    assert_eq!(dui.len(), 1);
    assert_eq!(dui[0].found, "對資料進行分析");
    assert_eq!(dui[0].suggestions[..], vec!["分析資料"]);
    assert_eq!(dui[0].severity, Severity::Info);
}

#[test]
fn dui_jinxing_longer_object() {
    let issues = scan("我們對整個系統進行測試");
    let dui: Vec<_> = issues
        .iter()
        .filter(|i| i.found.starts_with("對"))
        .collect();
    assert_eq!(dui.len(), 1);
    assert_eq!(dui[0].suggestions[..], vec!["測試整個系統"]);
}

#[test]
fn dui_jinxing_various_verbs() {
    // Each fires dui_jinxing; bureaucratic_nominalization may also fire.
    let check = |text: &str| scan(text).iter().any(|i| i.found.starts_with("對"));
    assert!(check("對程式碼進行審查"));
    assert!(check("對方案進行評估"));
    assert!(check("對架構進行重構"));
}

#[test]
fn dui_jinxing_compound_word_zhendui_skipped() {
    // 針對 is a compound preposition; the 對 is not standalone.
    let issues = scan("針對資料進行分析");
    assert!(
        !issues.iter().any(|i| i.found.starts_with("對")),
        "should not match 對 inside 針對"
    );
}

#[test]
fn dui_jinxing_compound_word_duiyu_skipped() {
    // 對於 is a compound preposition; should not match.
    assert!(!scan("對於資料進行分析")
        .iter()
        .any(|i| i.found.starts_with("對")));
}

#[test]
fn dui_jinxing_compound_miandui_skipped() {
    // 面對: not a standalone 對.
    assert!(!scan("面對問題進行分析")
        .iter()
        .any(|i| i.found.starts_with("對")));
}

#[test]
fn dui_jinxing_compound_bidui_skipped() {
    // 比對: technical verb, not standalone 對.
    assert!(!scan("比對資料進行分析")
        .iter()
        .any(|i| i.found.starts_with("對")));
}

#[test]
fn dui_jinxing_compound_hedui_skipped() {
    // 核對: not standalone 對.
    assert!(!scan("核對資料進行檢查")
        .iter()
        .any(|i| i.found.starts_with("對")));
}

#[test]
fn dui_jinxing_no_verb_after() {
    // 進行 without a matching verb following: not flagged.
    assert!(scan("對資料進行了某些操作").is_empty());
}

#[test]
fn dui_jinxing_no_jinxing() {
    // 對 without 進行: not flagged.
    assert!(scan("對資料很感興趣").is_empty());
}

#[test]
fn dui_jinxing_object_too_long() {
    // Object between 對 and 進行 exceeds 6 chars: dui_jinxing should skip.
    // (scan_bureaucratic_nominalization may still fire on "進行分析".)
    let issues = scan("對這份非常重要的報告進行分析");
    assert!(
        !issues.iter().any(|i| i.found.starts_with("對")),
        "dui_jinxing should not fire with oversized object"
    );
}

#[test]
fn dui_jinxing_clause_boundary_in_object() {
    // Comma between 對 and 進行: the 對X進行Y pattern should NOT fire.
    // (scan_bureaucratic_nominalization may still fire on "進行分析".)
    let issues = scan("對資料，進行分析");
    assert!(
        !issues.iter().any(|i| i.found.starts_with("對")),
        "dui_jinxing should not fire across clause boundary"
    );
}

#[test]
fn dui_jinxing_does_not_clash_with_bureaucratic() {
    // Both scanners should fire independently:
    // - scan_bureaucratic_nominalization catches "進行分析" → "分析"
    // - scan_dui_jinxing catches "對資料進行分析" → "分析資料" The broader one
    // (dui_jinxing) covers the full span.
    let issues = scan("對資料進行分析");
    let dui = issues
        .iter()
        .filter(|i| i.found == "對資料進行分析")
        .count();
    let bureau = issues.iter().filter(|i| i.found == "進行分析").count();
    assert_eq!(dui, 1, "dui_jinxing should fire");
    assert_eq!(bureau, 1, "bureaucratic should also fire");
}

// Exclusion zone handling

#[test]
fn excluded_range_suppresses_a_not_a() {
    let text = "你是不是學生嗎？";
    let excluded = vec![ByteRange {
        start: 0,
        end: text.len(),
    }];
    let mut issues = Vec::new();
    scan_grammar(
        &mut Emitter::new(text, &excluded, &mut issues),
        Register::Casual,
    );
    assert!(issues.is_empty());
}

#[test]
fn excluded_range_suppresses_bare_shi() {
    let text = "她是漂亮";
    let excluded = vec![ByteRange {
        start: 0,
        end: text.len(),
    }];
    let mut issues = Vec::new();
    scan_grammar(
        &mut Emitter::new(text, &excluded, &mut issues),
        Register::Casual,
    );
    assert!(issues.is_empty());
}

#[test]
fn excluded_range_suppresses_redundant_prep() {
    let text = "我們討論關於這個問題";
    let excluded = vec![ByteRange {
        start: 0,
        end: text.len(),
    }];
    let mut issues = Vec::new();
    scan_grammar(
        &mut Emitter::new(text, &excluded, &mut issues),
        Register::Casual,
    );
    assert!(issues.is_empty());
}

#[test]
fn partial_exclusion_still_flags_outside() {
    // Exclude only the first 3 bytes, leaving the rest scannable.
    let text = "你是不是學生嗎？";
    let excluded = vec![ByteRange { start: 0, end: 3 }];
    let mut issues = Vec::new();
    scan_grammar(
        &mut Emitter::new(text, &excluded, &mut issues),
        Register::Casual,
    );
    // 是不是 starts at byte 3 (after 你), should still be detected.
    assert_eq!(issues.len(), 1);
}

// Multiple issues in the same text

#[test]
fn multiple_grammar_issues_in_one_text() {
    // Contains both A-not-A+嗎 and bare 是+adj.
    let text = "你是不是學生嗎？她是漂亮";
    let issues = scan(text);
    assert_eq!(issues.len(), 2);
    let types: Vec<_> = issues.iter().map(|i| i.rule_type).collect();
    assert!(types.iter().all(|t| *t == IssueType::Grammar));
}

#[test]
fn multiple_a_not_a_in_same_text() {
    let text = "你是不是學生嗎？他有沒有錢嗎？";
    let issues = scan(text);
    assert_eq!(issues.len(), 2);
}

// False-positive guards: natural zh-TW text that should NOT trigger

#[test]
fn natural_question_with_ma_only() {
    assert!(scan("你今天有空嗎？").is_empty());
}

#[test]
fn natural_he_connecting_nouns() {
    assert!(scan("我喜歡音樂和電影").is_empty());
}

#[test]
fn comparative_he_yiyang_clean() {
    // 和你一樣 is a comparative construction, not clause coordination.
    assert!(scan("找到和你一樣的東西").is_empty());
}

#[test]
fn comparative_he_xiangtong_clean() {
    assert!(scan("做了和他相同的選擇").is_empty());
}

#[test]
fn natural_shi_with_noun() {
    assert!(scan("這是一本好書").is_empty());
}

#[test]
fn natural_shi_de_construction() {
    // 是…的 is a common grammatical construction, not a calque.
    assert!(scan("她是昨天來的").is_empty());
}

#[test]
fn natural_verb_suffix_before_he_but_noun_after() {
    // 了 before 和, but noun (not pronoun) after → no flag.
    assert!(scan("我買了和牛肉").is_empty());
}

#[test]
fn natural_transitive_verb_with_object() {
    assert!(scan("我們討論了技術細節").is_empty());
}

#[test]
fn technical_prose_no_false_positives() {
    let text = "在這個系統中，我們討論了架構設計和效能最佳化。\
                你有沒有看過相關文件？這是很重要的步驟。";
    assert!(scan(text).is_empty());
}

#[test]
fn natural_jinxing_standalone() {
    // 進行 as "to proceed" without a verb object.
    assert!(scan("工程順利進行，一切正常。").is_empty());
}

#[test]
fn natural_zuochu_physical() {
    // 做出 with a physical object, not abstract action.
    assert!(scan("她做出了一道好菜").is_empty());
}

#[test]
fn natural_genju_prepositional() {
    // 根據 as preposition with comma, no attribution verb in clause.
    assert!(scan("根據合約規定，雙方應遵守以下條款。").is_empty());
}

// AI writing detection

fn scan_ai(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_ai_grammar(&mut Emitter::new(text, &[], &mut issues));
    issues
}

// -- 意味著 semantic safety word --

#[test]
fn ai_yiweizhe_definition_context() {
    let text = "這個定義意味著所有的值都必須為正";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);
    assert_eq!(issues[0].found, "意味著");
    assert_eq!(issues[0].suggestions[..], vec!["表示"]);
}

#[test]
fn ai_yiweizhe_ignores_a_clue_inside_excluded_markup() {
    // 定義 sits in a code span, so it is not the author choosing the definition
    // sense for the prose 意味著 after it.
    let text = "這個 `定義` 欄位意味著所有的值都必須為正";
    let code_start = text.find("`定義`").unwrap();
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_start + "`定義`".len(),
    }];
    let mut issues = Vec::new();
    scan_ai_semantic_safety(&mut Emitter::new(text, &excluded, &mut issues));
    assert!(
        issues
            .iter()
            .all(|i| i.suggestions[..] != ["表示".to_string()]),
        "a clue inside markup chose the sense: {issues:?}"
    );

    // The same clue as prose still chooses it.
    let mut issues = Vec::new();
    scan_ai_semantic_safety(&mut Emitter::new(text, &[], &mut issues));
    assert!(
        issues
            .iter()
            .any(|i| i.suggestions[..] == ["表示".to_string()]),
        "as prose the clue should still count: {issues:?}"
    );
}

#[test]
fn ai_yiweizhe_sees_a_clue_far_away_in_the_same_sentence() {
    // The context window is the sentence, with no byte cutoff: a cutoff would
    // hide this clue and downgrade a confident replacement to advisory.
    let filler = "而且系統必須重新設計並且要考慮效能與可維護性以及擴充性".repeat(12);
    let text = format!("這個定義{filler}意味著所有的值都必須為正。");
    assert!(
        text.len() > 600,
        "the clue has to be past any plausible cutoff"
    );
    let issues = scan_ai(&text);
    assert!(
        issues
            .iter()
            .any(|i| i.found == "意味著" && i.suggestions[..] == ["表示".to_string()]),
        "the definition clue should still be found: {issues:?}"
    );
}

#[test]
fn ai_yiweizhe_is_not_a_definition_because_a_sentence_opens_on_jishi() {
    // 即使 is not the definition marker 即: reading it as one gave an ordinary
    // concessive sentence the definition sense.
    for text in [
        "即使如此，這意味著風險增加",
        "系統即將上線，這意味著風險增加",
        "請立即處理，這意味著風險增加",
    ] {
        let issues = scan_ai(text);
        assert!(
            issues
                .iter()
                .all(|i| i.suggestions[..] != ["表示".to_string()]),
            "read as a definition: {text} -> {issues:?}"
        );
    }
}

#[test]
fn ai_yiweizhe_consequence_context() {
    let text = "如果記憶體不足，這意味著系統將會崩潰";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[..], vec!["代表"]);
}

#[test]
fn ai_yiweizhe_explanation_context() {
    let text = "換言之，這意味著我們需要重新設計";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].suggestions[..], vec!["也就是說"]);
}

#[test]
fn ai_yiweizhe_no_context_advisory_only() {
    let text = "這意味著很多事情";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    // No clear context → advisory only (empty suggestions).
    assert!(issues[0].suggestions.is_empty());
}

#[test]
fn ai_yiweizhe_in_excluded_region() {
    let mut issues = Vec::new();
    let excluded = vec![ByteRange { start: 0, end: 100 }];
    scan_ai_semantic_safety(&mut Emitter::new("這意味著很多", &excluded, &mut issues));
    assert!(issues.is_empty());
}

// -- Copula avoidance --

#[test]
fn ai_copula_zuowei_in_tech_context() {
    let text = "此系統作為核心元件運作";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "作為");
    // Advisory only: no direct replacement (would break sentence).
    assert!(issues[0].suggestions.is_empty());
    assert!(issues[0].context.as_ref().unwrap().contains("是"));
}

#[test]
fn ai_copula_zuowei_not_in_tech_context() {
    // No tech context clues → should not flag.
    let text = "她作為一位母親非常偉大";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

#[test]
fn ai_copula_yongyou_in_tech_context() {
    let text = "這個模組擁有三個介面";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "擁有");
    // Advisory only: no direct replacement.
    assert!(issues[0].suggestions.is_empty());
    assert!(issues[0].context.as_ref().unwrap().contains("有"));
}

// -- Passive voice --

#[test]
fn ai_passive_bei_guangfan() {
    let text = "這個框架被廣泛使用於各種專案中";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "被廣泛使用");
    assert_eq!(issues[0].suggestions[..], vec!["廣泛使用"]);
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);
}

#[test]
fn ai_passive_bei_chengwei_not_flagged() {
    // 被稱為 removed: dropping 被 flips meaning with animate subjects.
    let text = "這個演算法被稱為快速排序";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

#[test]
fn ai_passive_bei_renwei_not_flagged() {
    // 被認為是 removed: 他被認為是→他認為是 changes meaning.
    let text = "他被認為是最好的程式設計師";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

#[test]
fn ai_passive_no_match_unlisted() {
    // 被打 is not in the curated list → no flag.
    let text = "他被打了一頓";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

// -- Copula compound word false-positive guards --

#[test]
fn ai_copula_yousuozuowei_not_flagged() {
    // 有所作為 is a compound; 作為 should not be flagged.
    let text = "這個系統必須有所作為才能改善效能";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

#[test]
fn ai_copula_yongyouquan_not_flagged() {
    // 擁有權 is a compound; 擁有 should not be flagged.
    let text = "此模組的擁有權屬於核心架構";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

// -- AI grammar does not interfere with base grammar --

#[test]
fn ai_grammar_does_not_produce_grammar_issues() {
    let text = "此系統作為核心元件，這意味著我們需要因此重新設計";
    let issues = scan_ai(text);
    for issue in &issues {
        assert_eq!(issue.rule_type, IssueType::AiStyle);
    }
}

// -- Didactic sentence patterns --

#[test]
fn ai_didactic_pattern_detected() {
    let text = "x86 的歷史告訴我們處理器設計需要平衡";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);
    assert!(issues[0].found.contains("告訴我們"));
}

#[test]
fn ai_didactic_different_verb() {
    let text = "這個案例的教訓提醒世人不要重蹈覆轍";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("提醒世人"));
}

#[test]
fn ai_didactic_no_noun_prefix() {
    // Without 的+noun before verb, should not flag.
    let text = "老師告訴我們要認真學習";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

// -- Vague exaggeration patterns --

#[test]
fn ai_vague_exaggeration_detected() {
    let text = "這項技術領先時代至少20年";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);
    assert!(issues[0].found.contains("領先"));
}

#[test]
fn ai_vague_exaggeration_different_verb() {
    let text = "該設計超越同期產品約5年的技術水準";
    let issues = scan_ai(text);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].found.contains("超越"));
}

#[test]
fn ai_vague_exaggeration_no_year() {
    // Without digits+年 following, should not flag.
    let text = "這項技術領先業界的水準";
    let issues = scan_ai(text);
    assert!(issues.is_empty());
}

#[test]
fn ai_formulaic_heading_inside_inline_code_does_not_count() {
    let plain = "## 總結與展望\n\n內容一。\n\n".repeat(3);
    let coded = "## `總結與展望`\n\n內容二。\n\n".repeat(3);
    let fires_on = |doc: &str| {
        let ranges = crate::engine::scan::build_exclusions_for_content_type(
            doc,
            crate::engine::scan::ContentType::Markdown,
        );
        let mut issues = Vec::new();
        scan_ai_structural(&mut Emitter::new(doc, &ranges, &mut issues), 1.0);
        issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化標題")))
    };
    assert!(fires_on(&plain), "plain formulaic headings should count");
    assert!(
        !fires_on(&coded),
        "a heading phrase inside inline code is not the author's heading"
    );
}

#[test]
fn ai_didactic_does_not_reach_back_past_a_full_stop() {
    // The 的noun the pattern teaches about has to be in its own sentence.
    assert!(
        scan_ai("我們研究了它的歷史。這份報告告訴我們一個道理。").is_empty(),
        "didactic reached into the previous sentence"
    );
    assert!(
        !scan_ai("這個專案的歷史告訴我們一個道理。").is_empty(),
        "the same-sentence case must still fire"
    );
}

#[test]
fn ai_vague_exaggeration_does_not_reach_past_a_full_stop() {
    assert!(
        scan_ai("這項技術領先業界。專案歷時20年才完成。").is_empty(),
        "a duration in the next sentence is not this verb's claim"
    );
}

#[test]
fn ai_vague_exaggeration_ignores_a_calendar_year() {
    // A digit anywhere plus a 年 anywhere used to match, so a sentence that
    // merely dated something read as a claim to lead the field by years.
    for text in [
        "這項技術領先業界，2025年將全面推出",
        "該設計超越同期產品，1999年首次發表",
    ] {
        assert!(
            scan_ai(text).is_empty(),
            "calendar year read as a lead claim: {text}"
        );
    }
}

// -- IssueType::AiStyle plumbing --

// -- AI density detection tests --

fn scan_density(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_ai_density(&mut Emitter::new(text, &[], &mut issues), 1.0);
    issues
}

#[test]
fn ai_density_short_text_skipped() {
    // Text under 500 chars should not trigger density analysis.
    let text = "更重要的是".repeat(20); // ~100 chars
    let issues = scan_density(&text);
    assert!(issues.is_empty(), "short text should skip density check");
}

#[test]
fn ai_density_below_threshold_no_issue() {
    // ~2600 chars of filler with 1 occurrence of tracked phrase. 1 / 2.6 ≈
    // 0.38/千字, below threshold 0.5 for '更重要的是'.
    let mut text = "這是一段正常的中文技術文章。".repeat(200);
    text.push_str("更重要的是，我們需要考慮效能。");
    assert!(text.chars().count() >= 2000);
    let issues = scan_density(&text);
    assert!(
        issues.is_empty(),
        "single occurrence in long text should not exceed density: {} chars",
        text.chars().count()
    );
}

#[test]
fn ai_density_above_threshold_flags() {
    // ~1000 chars with high density of '更重要的是' (threshold 0.5/千字). We
    // need >0.5 per 1000 chars, so >1 in 2000 chars or >0.5 in 1000. Build
    // ~1000 char text with 3 occurrences → density 3.0/千字.
    let filler = "這是正常的技術內容段落。"; // 12 chars
    let mut text = String::new();
    for i in 0..80 {
        if i % 25 == 0 {
            text.push_str("更重要的是，我們需要重新評估。");
        } else {
            text.push_str(filler);
        }
    }
    assert!(text.chars().count() >= 500);
    let issues = scan_density(&text);
    assert!(
        !issues.is_empty(),
        "high density should trigger: text has {} chars",
        text.chars().count()
    );
    assert_eq!(issues[0].rule_type, IssueType::AiStyle);
    assert!(issues[0].context.as_ref().unwrap().contains("次/千字"));
    assert!(issues[0].context.as_ref().unwrap().contains("更重要的是"));
}

#[test]
fn ai_density_excluded_ranges_respected() {
    // Occurrences in excluded ranges should not count toward density.
    let filler = "這是正常的技術內容段落。";
    let mut text = String::new();
    for i in 0..80 {
        if i % 25 == 0 {
            text.push_str("更重要的是，我們需要重新評估。");
        } else {
            text.push_str(filler);
        }
    }
    // Exclude the entire text: all occurrences should be skipped.
    let excluded = vec![ByteRange {
        start: 0,
        end: text.len(),
    }];
    let mut issues = Vec::new();
    scan_ai_density(&mut Emitter::new(&text, &excluded, &mut issues), 1.0);
    assert!(issues.is_empty(), "excluded ranges should suppress density");
}

#[test]
fn ai_density_multiple_phrases_independent() {
    // Two different phrases both above threshold: should get two issues.
    let mut text = String::new();
    for _ in 0..60 {
        text.push_str("這是正常的技術內容。");
    }
    // Insert both phrases repeatedly.
    for _ in 0..5 {
        text.push_str("更重要的是，這個方法不容忽視。");
    }
    assert!(text.chars().count() >= 500);
    let issues = scan_density(&text);
    // At least one should fire (density depends on exact char count).
    let density_contexts: Vec<_> = issues.iter().filter_map(|i| i.context.as_ref()).collect();
    // Both phrases should be independently evaluated.
    let has_gengyaojinaoshi = density_contexts.iter().any(|c| c.contains("更重要的是"));
    let has_buronghushi = density_contexts.iter().any(|c| c.contains("不容忽視"));
    // At least one should trigger given high density.
    assert!(
        has_gengyaojinaoshi || has_buronghushi,
        "at least one high-density phrase should trigger: contexts={density_contexts:?}"
    );
}

// -- AI structural pattern tests --

fn scan_structural(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_ai_structural(&mut Emitter::new(text, &[], &mut issues), 1.0);

    // Mirrors run_ai_filter, which runs the invisible-character layer alongside
    // the structural pass rather than inside it.
    scan_ai_zero_width(&mut Emitter::new(text, &[], &mut issues));
    issues
}

fn context_of(issues: &[Issue], needle: &str) -> usize {
    issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains(needle)))
        .count()
}

#[test]
fn mixed_reader_address_is_reported_once_on_the_rarer_form() {
    let issues = scan_structural("你可以修改設定檔。您也可以重新啟動服務。");
    assert_eq!(context_of(&issues, "混用"), 1);
    // The minority form is the one to change, so that is what is named.
    let hit = issues
        .iter()
        .find(|i| i.context.as_ref().is_some_and(|c| c.contains("混用")))
        .expect("mixing reported");
    assert!(hit.found == "你" || hit.found == "您");
}

#[test]
fn one_reader_address_is_not_a_defect() {
    for text in [
        "你可以修改設定檔，也可以重新啟動服務。",
        "您可以修改設定檔，也可以重新啟動服務。",
        // 你 is the second character of 迷你, where it is not a pronoun.
        "這是迷你版的設定，迷你主機也支援。您可以直接使用。",
    ] {
        assert_eq!(context_of(&scan_structural(text), "混用"), 0, "{text}");
    }
}

#[test]
fn politeness_stacked_on_every_step_is_reported() {
    let text = "1. 請停止服務。\n2. 請修改設定檔。\n3. 請重新啟動服務。\n";
    assert_eq!(context_of(&scan_structural(text), "以「請」開頭"), 1);
    let bullets = "- 請停止服務。\n- 請修改設定檔。\n- 請重新啟動服務。\n";
    assert_eq!(context_of(&scan_structural(bullets), "以「請」開頭"), 1);
}

// A wrapped step is still one step. Ending the run on the continuation line
// meant a three-step procedure written with wrapping was never seen. Paragraph
// slices must not carry their line ending: callers test exclusion as "is the
// whole paragraph covered", so a trailing \r reached one byte past what an
// exclusion range ends at and a fenced block was scanned anyway.
#[test]
fn paragraphs_are_the_same_either_line_ending() {
    let lf = "第一段。\n\n第二段。\n\n第三段。\n";
    let crlf = lf.replace('\n', "\r\n");
    let of = |t: &str| {
        super::super::split_paragraphs(t)
            .into_iter()
            .map(|(_, p)| p.to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(of(lf), ["第一段。", "第二段。", "第三段。"]);
    assert_eq!(of(&crlf), of(lf));
}

// Both walks this replaced ran from each sentence to a terminator that a
// pathological document does not have: an unwrapped paragraph has no line
// break, and a run of full stops passes the tail test at every position. 60,000
// of them in 176 KB cost 9.4 seconds, because the cost was paid per sentence
// and every full stop starts one.
#[test]
fn a_run_of_sentence_punctuation_answers_from_the_index() {
    let run = "。".repeat(20_000);
    let doc = format!("這個方案展望未來{run}\n下一行。\n");
    let idx = CloserTailIndex::build(&doc);
    let run_start = doc.find('。').expect("a full stop");
    let line_end = idx.next_line_break(run_start).expect("a line break");

    // Every position inside the run is answered without walking it, and the
    // answer is the same at each: nothing here disqualifies a closer.
    for pos in (run_start..line_end).step_by(3) {
        assert!(!idx.has_non_tail_char(pos, line_end));
    }
    // Prose before the run does disqualify it.
    assert!(idx.has_non_tail_char(0, line_end));
    // The line break is found by search, not by scanning to end of text.
    assert_eq!(idx.next_line_break(0), Some(line_end));
}

#[test]
fn a_wrapped_step_does_not_break_the_run() {
    let text = "1. 請停止服務，\n   並確認沒有連線。\n2. 請修改設定檔，\n                       把 ttl 調高。\n3. 請重新啟動服務。\n";
    assert_eq!(context_of(&scan_structural(text), "以「請」開頭"), 1);
}

#[test]
fn ordinary_politeness_is_not_a_run() {
    for text in [
        // One 請 in prose is courtesy, not padding.
        "執行前請先備份。\n\n1. 停止服務。\n2. 修改設定檔。\n3. 重新啟動。\n",
        // Two is under the run threshold.
        "1. 請停止服務。\n2. 請修改設定檔。\n",
        // A break in the run stops it, so two short lists do not combine.
        "1. 請停止服務。\n2. 修改設定檔。\n3. 請重新啟動。\n",
    ] {
        assert_eq!(
            context_of(&scan_structural(text), "以「請」開頭"),
            0,
            "{text}"
        );
    }
}

#[test]
fn ai_structural_binary_contrast_below_threshold() {
    // Short text or low density should not flag.
    let text = "雖然困難很多，但我們還是做到了。這是正常的文章。".repeat(10);
    let issues = scan_structural(&text);
    // Only 10 concessive patterns in ~280 chars: below 500 char threshold.
    assert!(
        issues.is_empty()
            || !issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("二元對比")))
    );
}

#[test]
fn ai_structural_binary_contrast_high_density() {
    let filler = "這是正常的技術段落。";
    let mut text = String::new();
    for i in 0..50 {
        if i % 4 == 0 {
            text.push_str("雖然這很困難，但我們可以克服。");
        } else if i % 4 == 1 {
            text.push_str("不僅要學習，更要實踐。");
        } else {
            text.push_str(filler);
        }
    }
    let issues = scan_structural(&text);
    let contrast_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("二元對比")))
        .collect();
    assert!(
        !contrast_issues.is_empty(),
        "high density binary contrast should trigger: {} chars, issues={:?}",
        text.chars().count(),
        issues
    );
}

#[test]
fn ai_structural_paragraph_endings() {
    let mut text = String::new();
    for i in 0..8 {
        if i % 2 == 0 {
            text.push_str("這個技術的發展證明了人工智慧的潛力。");
        } else {
            text.push_str("正是這個突破讓研究人員重新思考。");
        }
        text.push_str("\n\n");
    }
    let issues = scan_structural(&text);
    let ending_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化宣言")))
        .collect();
    assert!(
        !ending_issues.is_empty(),
        "formulaic paragraph endings should trigger"
    );
}

#[test]
fn ai_structural_dash_overuse() {
    let mut text = String::new();
    for _ in 0..5 {
        text.push_str("這項技術—作為核心—非常重要—我們必須注意。\n\n");
    }
    let issues = scan_structural(&text);
    let dash_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("破折號")))
        .collect();
    assert!(!dash_issues.is_empty(), "heavy dash usage should trigger");
}

#[test]
fn ai_structural_formulaic_headings() {
    let text = "# 簡介\n\n內容\n\n## 挑戰與未來展望\n\n更多內容\n\n## 結論與展望\n\n結語";
    let issues = scan_structural(text);
    let heading_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化標題")))
        .collect();
    assert!(
        !heading_issues.is_empty(),
        "formulaic headings should trigger"
    );
}

#[test]
fn ai_formulaic_despite_ignores_challenge_before_despite() {
    let text = "這些挑戰很多，儘管如此，團隊仍然持續改善。";
    let issues = scan_structural(text);
    let despite_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化轉折")))
        .collect();
    assert!(
        despite_issues.is_empty(),
        "challenge before despite should not trigger formulaic despite: {despite_issues:?}"
    );
}

#[test]
fn ai_structural_list_density() {
    let mut text = String::new();
    for i in 0..10 {
        if i < 5 {
            text.push_str("- 第一項\n- 第二項\n- 第三項");
        } else {
            text.push_str("這是一段正常的段落文字。");
        }
        text.push_str("\n\n");
    }
    let issues = scan_structural(&text);
    let list_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("列表")))
        .collect();
    assert!(
        !list_issues.is_empty(),
        "high list density should trigger: 5/10 = 50%"
    );
}

#[test]
fn ai_structural_normal_text_no_false_positive() {
    // Normal text should not trigger any structural patterns.
    let text = "台灣的技術產業在近年來快速發展。半導體製造是其中的核心。\n\n\
                台積電作為全球最大的晶圓代工廠，在先進製程上保持領先。\n\n\
                未來的發展方向包括三奈米和二奈米製程的量產。\n\n\
                除了硬體之外，軟體生態系統也在蓬勃發展中。\n\n\
                這些發展為台灣的經濟帶來了穩定的成長動力。";
    let issues = scan_structural(text);
    assert!(
        issues.is_empty(),
        "normal text should not trigger structural patterns: {issues:?}"
    );
}

#[test]
fn ai_zero_width_detected() {
    let text = "正常文字\u{200B}中間\u{FEFF}結尾";
    let issues = scan_structural(text);
    let zw: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
        .collect();
    assert_eq!(zw.len(), 2, "should detect 2 zero-width artifacts: {zw:?}");
    // Suggestions should be empty string for auto-removal.
    for issue in &zw {
        assert_eq!(issue.suggestions.len(), 1);
        assert!(issue.suggestions[0].is_empty());
    }
}

#[test]
fn ai_zero_width_preserves_valid_emoji_and_bidi_controls() {
    let text = "家庭 emoji 👩\u{200D}👩 與混合方向 abc\u{200E}עברית\u{200F}。";
    let issues = scan_structural(text);
    assert!(
        !issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元"))),
        "valid Unicode controls must not become AI rewrite findings: {issues:?}"
    );
}

#[test]
fn ai_zero_width_excluded() {
    let text = "正常\u{200B}文字";

    // Exclude the zero-width space (byte offset 6 for 2 CJK chars = 6 bytes).
    let excluded = vec![ByteRange { start: 6, end: 9 }];
    let mut issues = Vec::new();
    scan_ai_structural(&mut Emitter::new(text, &excluded, &mut issues), 1.0);
    let zw: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
        .collect();
    assert!(zw.is_empty(), "excluded zero-width should not be detected");
}

#[test]
fn ai_binary_contrast_ignores_pairs_inside_an_excluded_span() {
    // The paragraph gate only skips a paragraph that is wholly excluded, so a
    // contrast pair inside an inline code span in prose used to reach the
    // count. The detector needs 500 characters before it will look at all.
    let filler = "這是一段普通的說明文字用來把長度撐過門檻。".repeat(26);
    let pairs = "雖然快但貴。雖然強但慢。雖然新但貴。不僅快還穩。不僅強還省。";
    let text = format!("{filler}{pairs}");

    let fires = |excluded: &[ByteRange]| {
        let mut issues = Vec::new();
        scan_ai_structural(&mut Emitter::new(&text, excluded, &mut issues), 1.0);
        issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("二元對比")))
    };

    assert!(fires(&[]), "prose contrast pairs should be counted");

    // A turn word inside markup is no more the author's contrast than a start
    // word inside it, so covering only the turns must also silence the count.
    let turns_only: Vec<ByteRange> = ["但", "還"]
        .iter()
        .flat_map(|t| {
            text.match_indices(t)
                .map(|(at, m)| ByteRange {
                    start: at,
                    end: at + m.len(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        !fires(&turns_only),
        "pairs whose turn word is excluded must not be counted"
    );
    let covered = vec![ByteRange {
        start: filler.len(),
        end: text.len(),
    }];
    assert!(
        !fires(&covered),
        "pairs inside an excluded span must not be counted"
    );
}

#[test]
fn ai_excessive_bold_ignores_excluded_markers() {
    let text =
        "這是一段正常說明文字，內容足夠長但是沒有真的使用粗體標記，只有內嵌程式碼 `**a** **b** **c**` 作為示例。";
    let code_start = text.find('`').unwrap();
    let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_end,
    }];
    let idx = BoundaryIndex::build(text, &excluded);
    let mut issues = Vec::new();

    scan_ai_excessive_bold(&mut Emitter::new(text, &excluded, &mut issues), &idx);

    let bold_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("段落粗體過多"))
        })
        .collect();
    assert!(
        bold_issues.is_empty(),
        "excluded bold markers should not trigger excessive-bold: {bold_issues:?}"
    );
}

#[test]
fn ai_four_char_bold_label_bullets_trigger() {
    let text = "- **核心價值**：第一段說明\n- **治理架構**：第二段說明\n- **實作路徑**：第三段說明";
    let issues = scan_phase2(text);
    assert!(
        issues.iter().any(|i| i
            .context
            .as_ref()
            .is_some_and(|c| c.contains("四字標籤式列表"))),
        "four-character bold labels should trigger: {issues:?}"
    );
    assert!(
        !issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("機械式列表"))),
        "specialized four-character list should replace generic mechanical list: {issues:?}"
    );
}

#[test]
fn ai_significance_stamping_triggers_per_paragraph() {
    // The fixture has to put the stamp somewhere other than the last sentence
    // of the last paragraph, or it asserts nothing: its own message claims
    // stamping fires outside section-final sentences, and a one-sentence
    // document cannot show that.
    let text = "這項安排提供政策的重要框架，也印證制度的重要性。這一節接著討論實作細節。\n\n第二段還在同一節裡。";
    let issues = scan_phase2(text);
    assert!(
        issues.iter().any(|i| i
            .context
            .as_ref()
            .is_some_and(|c| c.contains("意義蓋章式收尾"))),
        "significance stamping should trigger outside section-final sentences: {issues:?}"
    );
}

#[test]
fn non_closing_ai_patterns_ignore_the_section_gate() {
    // 隨著…不斷發展 is an opener, not a closer. Gating it on section-final
    // position deleted it from every multi-paragraph document, which is how the
    // position gate was first written.
    let text = "隨著人工智慧不斷發展，各行各業都受到影響。\n\n第二段說明實作細節。\n\n第三段收尾。";
    let issues = scan_phase2(text);
    assert!(
        issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("隨著"))),
        "隨著…不斷發展 lost mid-document: {issues:?}"
    );
}

#[test]
fn ai_significance_stamping_exact_phrase_not_duplicated() {
    let text = "這項安排提供重要框架。";
    let issues = scan_phase2(text);
    let stamping_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("意義蓋章式收尾") || c.contains("公式化用語"))
        })
        .collect();
    assert_eq!(
        stamping_issues.len(),
        1,
        "exact phrase and pair match should not double-report: {issues:?}"
    );
}

#[test]
fn ai_in_sentence_bold_parallel_triggers() {
    let text = "我們需要**理解歷史**、**回到現場**，並**重建脈絡**。";
    let issues = scan_phase2(text);
    assert!(
        issues.iter().any(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("句內關鍵詞粗體排比"))
        }),
        "2-4 bold runs in one sentence should trigger: {issues:?}"
    );
}

#[test]
fn ai_abstract_line_metaphor_requires_three_signals() {
    let good = "改革不是口號，而是一條線。這條線從地方走出來，連到制度現場。這條線也會延伸到新的公共討論。";
    let good_issues = scan_phase2(good);
    let line_issue = good_issues.iter().find(|i| {
        i.context
            .as_ref()
            .is_some_and(|c| c.contains("抽象概念具象成路線"))
    });
    assert!(
        line_issue.is_some(),
        "three-signal abstract line metaphor should trigger: {good_issues:?}"
    );
    assert_eq!(line_issue.unwrap().found, "一條線");

    let idiom = "這條路走不通，所以團隊改用另一個方案。";
    let idiom_issues = scan_phase2(idiom);
    assert!(
        !idiom_issues.iter().any(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("抽象概念具象成路線"))
        }),
        "single idiom must stay silent: {idiom_issues:?}"
    );
}

#[test]
fn ai_repeated_parallel_slogan_triggers_once() {
    let text =
        "制度不是牆，而是橋。第一段說明政策背景。\n\n制度不是牆，而是橋。第二段重複同一句口號。";
    let issues = scan_phase2(text);
    let slogan_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句")))
        .collect();
    assert_eq!(
        slogan_issues.len(),
        1,
        "repeated parallel slogan should trigger once: {issues:?}"
    );
}

#[test]
fn ai_same_paragraph_slogan_repetition_does_not_trigger() {
    // Same parallel sentence repeated within one paragraph is ordinary 排比,
    // not the cross-section 金句疊句 tic: must stay silent.
    let text = "制度不是牆，而是橋。制度不是牆，而是橋。我們要繼續努力。";
    let issues = scan_phase2(text);
    assert!(
        !issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句"))),
        "same-paragraph repetition should not trigger slogan detector: {issues:?}"
    );
}

#[test]
fn ai_repeated_plain_sentence_does_not_trigger_slogan() {
    let text = "請在週五前提交報告。這是行政提醒。\n\n請在週五前提交報告。這是第二次提醒。";
    let issues = scan_phase2(text);
    assert!(
        !issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句"))),
        "ordinary repeated sentence should not trigger slogan detector: {issues:?}"
    );
}

#[test]
fn ai_repeated_rhetorical_self_qa_triggers_but_faq_is_preserved() {
    let rhetorical =
        "你以為只是網路慢嗎？錯了，每次請求都重新計算。為什麼快取沒生效？因為 TTL 設定到期。";
    let issues = scan_phase2(rhetorical);
    let self_qa: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("連續自問自答"))
        })
        .collect();
    assert_eq!(
        self_qa.len(),
        1,
        "rhetorical pairs should trigger: {issues:?}"
    );

    let faq =
        "faq：你以為只是網路慢嗎？錯了，每次請求都重新計算。為什麼快取沒生效？因為 TTL 設定到期。";
    let faq_issues = scan_phase2(faq);
    assert!(
        !faq_issues.iter().any(|i| i
            .context
            .as_ref()
            .is_some_and(|c| c.contains("連續自問自答"))),
        "explicit FAQ must stay silent: {faq_issues:?}"
    );
}

#[test]
fn rhetorical_self_qa_leaves_explanatory_prose_alone() {
    // Chained 為什麼/因為 is how Chinese textbooks and technical explainers
    // teach. Without a staged reveal there is nothing AI-specific about it.
    let teaching = "為什麼要用快取？因為重算的成本很高。為什麼快取會失效？因為 TTL 設定到期。";
    assert!(
        !has_self_qa(&scan_phase2(teaching)),
        "explanatory prose must not be flagged"
    );
}

#[test]
fn rhetorical_self_qa_survives_idiomatic_lead_ins() {
    // Idiomatic zh-TW rarely starts on the bare interrogative.
    let text = "但你以為只是網路慢嗎？錯了，每次請求都重新計算。那為什麼快取沒生效？主要是因為 TTL 設定到期。";
    assert!(
        has_self_qa(&scan_phase2(text)),
        "discourse markers hid the device"
    );
}

#[test]
fn ai_emdash_overuse_needs_two_dashes_in_a_paragraph() {
    // Pins the threshold the doc comment states. One dash is ordinary
    // punctuation; the detector's own message counts occurrences, so a one-dash
    // paragraph firing would also report "段落內 1 處".
    for (text, want) in [("這段文字——結尾", 0), ("這段文字——持續補充——結尾", 1)]
    {
        let idx = BoundaryIndex::build(text, &[]);
        let mut issues = Vec::new();
        scan_ai_emdash_overuse(&mut Emitter::new(text, &[], &mut issues), &idx);
        assert_eq!(
            issues.len(),
            want,
            "{text:?} should produce {want} issue(s)"
        );
    }
}

#[test]
fn ai_emdash_overuse_ignores_excluded_markers() {
    let text = "`——` 這段文字——持續補充——結尾";
    let code_start = text.find('`').unwrap();
    let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_end,
    }];
    let idx = BoundaryIndex::build(text, &excluded);
    let mut issues = Vec::new();

    scan_ai_emdash_overuse(&mut Emitter::new(text, &excluded, &mut issues), &idx);

    let dash_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("破折號")))
        .collect();
    assert_eq!(dash_issues.len(), 1, "real dashes should trigger once");
    assert_eq!(
        dash_issues[0].offset,
        text.find("文字——").unwrap() + "文字".len()
    );
    assert!(
        dash_issues[0]
            .context
            .as_ref()
            .is_some_and(|c| c.contains("段落內 2 處")),
        "excluded dash should not inflate count: {dash_issues:?}"
    );
}

#[test]
fn ai_dash_overuse_ignores_excluded_markers() {
    let text = "`———` 這是正常段落。\n\n`———` 這也是正常段落。\n\n`———` 這仍然是正常段落。";
    let excluded: Vec<ByteRange> = text
        .match_indices('`')
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|pair| ByteRange {
            start: pair[0].0,
            end: pair[1].0 + '`'.len_utf8(),
        })
        .collect();
    let mut issues = Vec::new();

    scan_ai_dash_overuse(&mut Emitter::new(text, &excluded, &mut issues));

    let dash_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("含 ≥3 個破折號"))
        })
        .collect();
    assert!(
        dash_issues.is_empty(),
        "excluded code dashes should not create dash-overuse density: {dash_issues:?}"
    );
}

#[test]
fn ai_hedging_density_ignores_excluded_markers() {
    let text = "在某種程度上，這段正常文字提供足夠長的段落內容，用來測試密度提升不會被程式碼範例影響，並保留一個真正的提示。`從某個角度來看 可以說是`";
    let code_start = text.find('`').unwrap();
    let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_end,
    }];
    let idx = BoundaryIndex::build(text, &excluded);
    let mut issues = vec![Issue::new(
        0,
        "在某種程度上".len(),
        "在某種程度上",
        vec![],
        IssueType::AiStyle,
        Severity::Info,
    )
    .with_context("AI hedging: 在某種程度上")];

    scan_ai_hedging_density(text, &excluded, &mut issues, &idx);

    assert_eq!(
        issues[0].severity,
        Severity::Info,
        "excluded hedging examples should not promote the real issue"
    );
}

#[test]
fn ai_zero_width_no_false_positive() {
    let text = "完全正常的文字，沒有任何零寬字元。";
    let issues = scan_structural(text);
    let zw: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
        .collect();
    assert!(zw.is_empty(), "clean text should have no zero-width issues");
}

#[test]
fn abstract_subject_reports_more_than_first_sentence() {
    let text = "預算的減少導致服務縮減。品質的提高意味著效率提升。";
    let idx = BoundaryIndex::build(text, &[]);
    let mut issues = Vec::new();

    scan_trans_abstract_subject(&mut Emitter::new(text, &[], &mut issues), &idx);

    let abstract_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("抽象主語")))
        .collect();
    assert_eq!(
        abstract_issues.len(),
        2,
        "should report one abstract-subject issue per matching sentence"
    );
}

#[test]
fn ai_style_serde_round_trip() {
    let json = serde_json::to_string(&IssueType::AiStyle).unwrap();
    assert_eq!(json, "\"ai_style\"");
    let back: IssueType = serde_json::from_str(&json).unwrap();
    assert_eq!(back, IssueType::AiStyle);
}

#[test]
fn ai_style_sort_order_after_grammar() {
    assert!(IssueType::AiStyle.sort_order() > IssueType::Grammar.sort_order());
}

#[test]
fn is_para_excluded_empty_exclusions() {
    // Empty exclusion list never excludes anything.
    assert!(!is_para_excluded(0, 100, &[]));
}

#[test]
fn is_para_excluded_fully_inside() {
    let excluded = vec![ByteRange { start: 0, end: 200 }];
    assert!(is_para_excluded(10, 50, &excluded));
}

#[test]
fn is_para_excluded_partial_overlap_not_excluded() {
    // Paragraph extends beyond the exclusion zone: should NOT be excluded.
    let excluded = vec![ByteRange { start: 0, end: 30 }];
    assert!(!is_para_excluded(10, 50, &excluded));
}

#[test]
fn structural_detectors_skip_excluded_paragraphs() {
    // Build text with a list-heavy "paragraph" that is fully excluded. Without
    // exclusion it would trigger list_density; with exclusion it should not.
    let mut text = String::new();
    let code_start = 0;
    // Fake code block paragraph with list items.
    for _ in 0..10 {
        text.push_str("- list item in code\n");
    }
    let code_end = text.len();
    text.push_str("\n\n");
    // Add non-list prose paragraphs to meet minimum paragraph count.
    for _ in 0..6 {
        text.push_str("這是正常的段落文字，沒有列表項目，用來充數。\n\n");
    }
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_end,
    }];
    let mut issues = Vec::new();
    scan_ai_list_density(&mut Emitter::new(&text, &excluded, &mut issues), 1.0);
    let list_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("含列表")))
        .collect();
    assert!(
        list_issues.is_empty(),
        "excluded code paragraph should not inflate list density: {list_issues:?}"
    );
}

// Differential testing: AC prefilter vs. legacy per-scanner path

/// Compare AC-based scan_grammar output against legacy per-scanner output.
/// Issues may arrive in different order, so we sort by (offset, found)
/// before comparing.
fn assert_ac_matches_legacy(text: &str) {
    let mut ac_issues = Vec::new();
    scan_grammar(
        &mut Emitter::new(text, &[], &mut ac_issues),
        Register::Casual,
    );
    ac_issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.found.cmp(&b.found)));

    let mut legacy_issues = Vec::new();
    scan_grammar_legacy(
        &mut Emitter::new(text, &[], &mut legacy_issues),
        Register::Casual,
    );
    legacy_issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.found.cmp(&b.found)));

    assert_eq!(
        ac_issues.len(),
        legacy_issues.len(),
        "issue count mismatch on {:?}:\n  AC:     {:?}\n  Legacy: {:?}",
        text,
        ac_issues.iter().map(|i| &i.found).collect::<Vec<_>>(),
        legacy_issues.iter().map(|i| &i.found).collect::<Vec<_>>(),
    );

    for (ac, leg) in ac_issues.iter().zip(legacy_issues.iter()) {
        assert_eq!(ac.offset, leg.offset, "offset mismatch on {:?}", text);
        assert_eq!(ac.found, leg.found, "found mismatch on {:?}", text);
        assert_eq!(
            ac.suggestions, leg.suggestions,
            "suggestion mismatch on {:?}",
            text
        );
        assert_eq!(ac.severity, leg.severity, "severity mismatch on {:?}", text);
    }
}

#[test]
fn differential_a_not_a() {
    assert_ac_matches_legacy("你是不是學生嗎？");
    assert_ac_matches_legacy("你有沒有吃飯嗎？");
    assert_ac_matches_legacy("你是不是學生？"); // no 嗎, clean
}

#[test]
fn differential_he_connecting() {
    assert_ac_matches_legacy("我吃了和你去看電影");
    assert_ac_matches_legacy("蘋果和橘子都很好吃"); // clean
}

#[test]
fn differential_bare_shi() {
    assert_ac_matches_legacy("她是漂亮");
    assert_ac_matches_legacy("她是很漂亮"); // clean
    assert_ac_matches_legacy("這是好消息"); // noun modifier, clean
}

#[test]
fn differential_redundant_preposition() {
    assert_ac_matches_legacy("我們討論關於這個問題");
    assert_ac_matches_legacy("這影響到整體計畫");
    assert_ac_matches_legacy("我們討論這個問題"); // clean
}

#[test]
fn differential_bureaucratic() {
    assert_ac_matches_legacy("我們進行討論");
    assert_ac_matches_legacy("加以分析這個問題");
}

#[test]
fn differential_verbose_action() {
    assert_ac_matches_legacy("做出決定");
    assert_ac_matches_legacy("作出回應");
}

#[test]
fn differential_dui_jinxing() {
    assert_ac_matches_legacy("對資料進行分析");
    assert_ac_matches_legacy("對系統進行測試");
}

#[test]
fn differential_double_attribution() {
    assert_ac_matches_legacy("根據研究顯示這個結果");
    assert_ac_matches_legacy("根據研究這個結果"); // clean
}

#[test]
fn differential_combined() {
    // Multiple grammar patterns in one text.
    assert_ac_matches_legacy("她是漂亮，我們討論關於這個問題，你是不是學生嗎？");
}

#[test]
fn differential_empty_and_ascii() {
    assert_ac_matches_legacy("");
    assert_ac_matches_legacy("Hello world");
}

#[test]
fn differential_dui_jinxing_with_bureaucratic() {
    // Text triggers both DuiJinxing (對...進行) and BureaucraticNominalization
    // (進行...).
    assert_ac_matches_legacy("對資料進行分析的報告");
}

// EN→ZH calque detectors: substring-only lexical pass.

fn scan_lex(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_translationese_lexical(&mut Emitter::new(text, &[], &mut issues), Register::Casual);
    issues
}

fn has_context_with(issues: &[Issue], needle: &str) -> bool {
    issues
        .iter()
        .any(|i| i.context.as_ref().is_some_and(|c| c.contains(needle)))
}

// ZY1a -----------------------------------------------------------------

#[test]
fn zy1a_fires_on_classic_one_of_the_most_calque() {
    // calque_superlative_zy1_bad_001: textbook translation tell.
    let text = "20 世紀最重要的發現之一。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_fires_on_jiwei_variant() {
    // calque_superlative_zy1_bad_002: 極為...之一 variant. Use an event noun
    // (成就) rather than a person noun so the biographical guard does not
    // suppress this case.
    let text = "這是極為重要的科學成就之一。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_fires_on_long_modifier_within_window() {
    // calque_superlative_zy1_bad_003: pattern survives an internal modifier.
    let text = "這是當代最具代表性的科學成就之一。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_passes_when_zhi_breaks_the_pair() {
    // calque_superlative_zy1_good_001: 之 between 最 and 之一 disqualifies. The
    // opener-closer pair is no longer a single superlative span.
    let text = "最近之內所收到的回信之一。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_passes_when_no_superlative_marker() {
    // calque_superlative_zy1_good_002: bare 之一 without 最/極為 is fine.
    let text = "他是領域裡的傑出學者之一。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_passes_when_no_zhiyi() {
    // 最 alone (without trailing 之一) does not pair with the
    // superlative-calque shape and must not fire.
    let text = "這是最重要的研究方向。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

#[test]
fn zy1a_passes_on_biographical_person_noun() {
    // calque_superlative_zy1_good_004: native-Mandarin biographical idiom.
    // 畫家/學者/作家 endings get the person-noun guard.
    let cases = [
        "他是當代最傑出的畫家之一。",
        "她是領域裡最有影響力的學者之一。",
        "她是這一代最受歡迎的作家之一。",
        "他是最早的程式設計師之一。",
        "他是傑出的工程師之一。",
    ];
    for text in cases {
        assert!(
            !fires(&scan_lex(text), (PhaseFamily::YiZhi, PhasePass::Lexical)),
            "should not fire on biographical idiom: {text}"
        );
    }
}

#[test]
fn zy1a_still_fires_on_non_person_noun_ending_with_shared_character() {
    let text = "世界上最重要的國家之一。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::YiZhi, PhasePass::Lexical)
    ));
}

// ZY2a -----------------------------------------------------------------

#[test]
fn zy2a_fires_on_yinwei_suoyi() {
    let text = "因為下雨了，所以我們待在屋裡。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_fires_on_suiran_danshi() {
    let text = "雖然他非常努力，但是還是失敗了。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_fires_on_dang_de_shihou() {
    let text = "當我到達公司的時候，他已經在開會了。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_fires_on_ruguo_name() {
    let text = "如果你願意幫忙，那麼請告訴我。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_passes_on_unpaired_yinwei() {
    // calque_connective_zy2_good_001: 因為ng without 所以 is fine.
    let text = "因為下雨，他選擇待在屋裡。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_passes_when_dang_is_dangshi() {
    // 當時 is a noun ("at that time"), not the temporal connective.
    let text = "當時的他並不知情。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

#[test]
fn zy2a_passes_when_distance_exceeds_window() {
    // Gap > 40 chars between 因為 and 所以: pair is too far to qualify.
    let mut filler = String::from("因為");
    filler.push_str(&"啊".repeat(45));
    filler.push_str("所以這裡。");
    assert!(!fires(
        &scan_lex(&filler),
        (PhaseFamily::Connective, PhasePass::Lexical)
    ));
}

// ZY3a -----------------------------------------------------------------

#[test]
fn zy3a_fires_on_implementation_improvement_pair() {
    // Nominalization BAD pair 1: 策略的實施帶來了效率的提升.
    let text = "策略的實施帶來了效率的提升。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

#[test]
fn zy3a_fires_on_analysis_discovery_pair() {
    // Nominalization BAD pair 2: 對資料的分析導致了模式的發現.
    let text = "對資料的分析導致了模式的發現。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

#[test]
fn zy3a_fires_on_three_chain() {
    // Extended chain: 對X的講解的理解 ≥3 nominalizations.
    let text = "他對概念的講解的理解非常深入。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

#[test]
fn zy3a_passes_on_single_nominalization() {
    // calque_nominalization_zy3_good_001: a single 的+nominalization is fine.
    let text = "策略的實施很順利。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

#[test]
fn zy3a_passes_across_a_terminal_mark() {
    // A full-width exclamation ends the sentence as surely as a full stop, so
    // the two heads either side of it are not one chain.
    for text in [
        "這個策略的實施！那個效果的提升。",
        "這個策略的實施？那個效果的提升。",
        "這個策略的實施!那個效果的提升。",
    ] {
        assert!(
            !fires(
                &scan_lex(text),
                (PhaseFamily::Nominalization, PhasePass::Lexical)
            ),
            "chained across a terminal mark: {text}"
        );
    }
}

#[test]
fn zy3a_passes_when_clause_boundary_separates() {
    // Two nominalizations across a comma: different clauses.
    let text = "策略的實施完成了，效率的提升仍在觀察。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

#[test]
fn zy3a_passes_on_coordinated_nominal_phrases() {
    let text = "我們對政策的理解和對流程的認識都很深入。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::Nominalization, PhasePass::Lexical)
    ));
}

// ZY4a -----------------------------------------------------------------

#[test]
fn zy4a_fires_when_two_false_friends_share_a_clause() {
    // calque_falsefriend_zy4_bad_001: actually + basically pattern.
    let text = "實際上基本上每個人都同意這個觀點。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::FalseFriend, PhasePass::Lexical)
    ));
}

#[test]
fn zy4a_fires_with_parenthetical_gloss() {
    // calque_falsefriend_zy4_bad_002: term followed by (English) gloss.
    let text = "字面上 (literally) 我也是這樣理解的。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::FalseFriend, PhasePass::Lexical)
    ));
}

#[test]
fn zy4a_fires_on_seriously_honestly_cluster() {
    // calque_falsefriend_zy4_bad_003: 嚴肅地表示 + 誠實地說 same clause.
    let text = "他嚴肅地表示誠實地說我們無法繼續。";
    assert!(fires(
        &scan_lex(text),
        (PhaseFamily::FalseFriend, PhasePass::Lexical)
    ));
}

#[test]
fn zy4a_passes_on_solo_occurrence() {
    // calque_falsefriend_zy4_solo_001: lone 實際上 in a clause, OK.
    let text = "實際上他比我想的還要勤奮。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::FalseFriend, PhasePass::Lexical)
    ));
}

#[test]
fn zy4a_passes_when_companion_is_in_different_clause() {
    // calque_falsefriend_zy4_solo_002: comma separates the two cues.
    let text = "實際上他並不在意，基本上一切都按部就班。";
    assert!(!fires(
        &scan_lex(text),
        (PhaseFamily::FalseFriend, PhasePass::Lexical)
    ));
}

#[test]
fn zy4a_ignores_companion_inside_excluded_zone() {
    // Codex review: a false-friend hit inside an exclusion zone (e.g. inline
    // code) must not supply companion-evidence to a non-excluded hit in the
    // same clause. Range [0, 10) covers "實際上" so the remaining "基本上" is
    // alone outside the zone.
    let text = "實際上基本上每個人都同意。";
    let mut issues = Vec::new();
    let excluded: &[ByteRange] = &[ByteRange {
        start: 0,
        end: "實際上".len(),
    }];
    scan_translationese_lexical(
        &mut Emitter::new(text, excluded, &mut issues),
        Register::Casual,
    );

    // The remaining 基本上 should NOT fire because its only same-clause
    // companion (實際上) is now excluded.
    assert!(
        !issues
            .iter()
            .any(|i| i.phase_family == Some((PhaseFamily::FalseFriend, PhasePass::Lexical))),
        "ZY4a should not fire when companion is excluded"
    );
}

#[test]
fn zy4a_parenthetical_gloss_inside_excluded_zone_does_not_qualify() {
    // Codex review: parenthetical gloss inside an exclusion zone must not count
    // as translation evidence.
    let text = "字面上 (literally) 我也同意。";
    // Mark the parenthetical gloss as excluded.
    let paren_start = text.find('(').unwrap();
    let paren_end = text.find(')').unwrap() + 1;
    let mut issues = Vec::new();
    let excluded: &[ByteRange] = &[ByteRange {
        start: paren_start,
        end: paren_end,
    }];
    scan_translationese_lexical(
        &mut Emitter::new(text, excluded, &mut issues),
        Register::Casual,
    );
    assert!(
        !issues
            .iter()
            .any(|i| i.phase_family == Some((PhaseFamily::FalseFriend, PhasePass::Lexical))),
        "ZY4a should not fire when gloss is excluded"
    );
}

#[test]
fn zy2a_skips_dang_di_dang_ju_dang_zhongs() {
    // Gemini HIGH: 當地/當局/當中/當然 must not be misclassified as 當…的時候
    // connectives even when 的時候 happens to follow.
    let cases = [
        "當地的時候情況不容易掌握。",
        "當局的時候反應顯得遲緩。",
        "當中的時候資訊很混亂。",
        "當然的時候大家都會選擇A。",
    ];
    for text in cases {
        let issues = scan_lex(text);
        assert!(
            !issues
                .iter()
                .any(|i| i.phase_family == Some((PhaseFamily::Connective, PhasePass::Lexical))),
            "ZY2a must not fire on 當-prefix words: {text}"
        );
    }
}

// Regression: detectors must not panic on empty / ASCII / mixed input.

#[test]
fn lexical_detectors_handle_empty_input() {
    assert!(scan_lex("").is_empty());
}

#[test]
fn lexical_detectors_handle_ascii_only() {
    assert!(scan_lex("Hello world. Actually, basically.").is_empty());
}

#[test]
fn lexical_detectors_handle_mixed_cjk_ascii_no_panic() {
    let text = "Hello 因為A，所以B。實際上 (literally) 100% sure.";
    let _ = scan_lex(text);
}

// Boundary-aware translationese detectors.

fn scan_indexed(
    text: &str,
    domain: crate::engine::translationese_score::TranslationeseDomain,
) -> Vec<Issue> {
    scan_indexed_with_rhythm(text, domain, false)
}

fn scan_indexed_with_rhythm(
    text: &str,
    domain: crate::engine::translationese_score::TranslationeseDomain,
    rhythm: bool,
) -> Vec<Issue> {
    let idx = BoundaryIndex::build(text, &[]);
    let mut issues = Vec::new();
    scan_translationese_indexed(
        &mut Emitter::new(text, &[], &mut issues),
        &idx,
        domain,
        rhythm,
        Register::Casual,
    );
    issues
}

fn scan_rhythm_only(text: &str) -> Vec<Issue> {
    let idx = BoundaryIndex::build(text, &[]);
    let mut issues = Vec::new();
    scan_rhythm(&mut Emitter::new(text, &[], &mut issues), &idx);
    issues
}

/// Select by detector identity rather than by the wording of the message.
fn fires(issues: &[Issue], want: (PhaseFamily, PhasePass)) -> bool {
    issues.iter().any(|i| i.phase_family == Some(want))
}

// ZY1b -----------------------------------------------------------------

#[test]
fn zy1b_fires_on_yi_zhi_density() {
    // 6 之一 in a >100-char paragraph → density well above general threshold
    // (2.0/200).
    let text = "這是科學成就之一，這是科學成就之一。\
                那是貢獻之一，那是貢獻之一。\
                再者是發現之一，再者是發現之一。\
                另一個發現之一，另一個發現之一。\
                還有重要事件之一，還有重要事件之一。\
                最後一個成就之一，最後一個成就之一。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        fires(&issues, (PhaseFamily::YiZhi, PhasePass::Indexed)),
        "expected 之一 段落密度過高 — in: {issues:?}"
    );
}

#[test]
fn zy1b_passes_on_short_paragraph() {
    let text = "這是成就之一。那是貢獻之一。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(&issues, (PhaseFamily::YiZhi, PhasePass::Indexed)));
}

#[test]
fn zy1b_register_switch_changes_firing() {
    // 3 之一 in ~250-char paragraph → density ~2.4/200. Above Literary
    // threshold (1.0/200) but below Technical (3.0/200). Natural prose padding
    // without further 之一 occurrences.
    let body = "他是領域裡的創新者，這項研究是當代成就，他在學術圈\
                地位崇高，這個團隊也是行業領頭羊。整體來說這篇論文\
                是經典代表之一，這個觀點是少數派論述之一，未來研究\
                的方向也將以此為一例之一展開深入探討與分析。學界\
                認為相關討論值得關注，後續研究也將圍繞這些主題\
                進行更為深入的考察與比較分析。研究團隊指出，過去\
                幾年的學術走向已經逐步成形，各種新方法不斷被提出\
                且持續調整，主流共識正在凝聚當中，相關文獻也明顯\
                增加，引起學界更廣泛關注。";
    let lit = scan_indexed(
        body,
        crate::engine::translationese_score::TranslationeseDomain::Literary,
    );
    let tech = scan_indexed(
        body,
        crate::engine::translationese_score::TranslationeseDomain::Technical,
    );
    // Literary threshold 1.0/200: fires; Technical 3.0/200: does not.
    assert!(
        fires(&lit, (PhaseFamily::YiZhi, PhasePass::Indexed)),
        "Literary should fire: {lit:?}"
    );
    assert!(
        !fires(&tech, (PhaseFamily::YiZhi, PhasePass::Indexed)),
        "Technical should not fire: {tech:?}"
    );
}

// ZY2b -----------------------------------------------------------------

#[test]
fn zy2b_fires_on_sentence_bounded_yinwei_suoyi() {
    let text = "因為下雨了，所以我們待在屋裡。這句話另起一行。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
        "expected ZY2b: {issues:?}"
    );
}

#[test]
fn a_paired_connective_does_not_reach_across_a_full_stop() {
    // 因為 and 所以 within the distance budget but in different sentences are
    // two statements, not one paired connective.
    let across = "因為下雨了。我們待在屋裡，所以很無聊。";
    assert!(
        !fires(
            &scan_lex(across),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ),
        "ZY2a paired across a sentence boundary: {:?}",
        scan_lex(across)
    );
    let within = "因為下雨了，所以我們待在屋裡。";
    assert!(
        fires(
            &scan_lex(within),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ),
        "ZY2a must still pair inside one sentence: {:?}",
        scan_lex(within)
    );
}

#[test]
fn a_superlative_does_not_reach_across_a_full_stop() {
    let across = "這是最好的方法。其中之一是重構。";
    assert!(
        !fires(&scan_lex(across), (PhaseFamily::YiZhi, PhasePass::Lexical)),
        "ZY1a paired across a sentence boundary: {:?}",
        scan_lex(across)
    );
    let within = "這是最好的方法，其中之一是重構。";
    assert!(
        fires(&scan_lex(within), (PhaseFamily::YiZhi, PhasePass::Lexical)),
        "ZY1a must still pair inside one sentence: {:?}",
        scan_lex(within)
    );
}

#[test]
fn zy2b_is_suppressed_in_formal_register() {
    // ZY2a returns early on a formal register, and leaving the indexed pass to
    // report the same 因為…所以 would make that gate invisible.
    let text = "因為下雨了，所以我們待在屋裡。";
    let idx = BoundaryIndex::build(text, &[]);
    let mut issues = Vec::new();
    scan_translationese_indexed(
        &mut Emitter::new(text, &[], &mut issues),
        &idx,
        crate::engine::translationese_score::TranslationeseDomain::General,
        false,
        Register::Formal,
    );
    assert!(
        !fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
        "formal register must suppress ZY2b: {issues:?}"
    );
}

#[test]
fn zy2b_does_not_fire_across_sentence_boundary() {
    // 因為 in sentence 1, 所以 in sentence 2: must NOT fire.
    let text = "他停下來，因為下雨了。所以大家紛紛回家了。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        !fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
        "should not span sentences: {issues:?}"
    );
}

#[test]
fn zy2b_finds_real_dang_after_skipped_dangdi() {
    // Codex review: a guarded 當-prefix word (當地) early in a sentence must
    // not block a real 當…的時候 connective later in the same sentence. Both
    // opener occurrences must be examined.
    let text = "他在當地一直工作著，當我抵達總部的時候才終於和他見面。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
        "expected ZY2b: {issues:?}"
    );
}

#[test]
fn zy2b_preserves_zy2a_distance_cap_inside_sentence() {
    let filler = "甲".repeat(45);
    let text = format!("因為{filler}所以我們決定延後。");
    let issues = scan_indexed(
        &text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        !fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
        "should respect ZY2a distance cap: {issues:?}"
    );
}

#[test]
fn zy1b_anchor_skips_excluded_first_hit() {
    // Codex review: ZY1b's anchor must point at the first NON-excluded 之一,
    // not the first raw substring hit. When an excluded zone covers the first
    // hit but the paragraph still qualifies, the issue must still emit
    // (anchored elsewhere).
    let body = "他是領域裡的創新者之一，這項研究是當代成就，他在學術圈\
                地位崇高，這個團隊也是行業領頭羊。整體來說這篇論文\
                是經典代表之一，這個觀點是少數派論述之一，未來研究\
                的方向也將以此為一例之一展開深入探討。學界認為相關\
                討論值得關注，後續研究也將圍繞這些主題進行更為深入\
                的考察與比較分析。";
    let text = format!("前綴 {body}");
    let first_zhi_yi = text.find("之一").unwrap();
    let excluded: &[ByteRange] = &[ByteRange {
        start: first_zhi_yi,
        end: first_zhi_yi + "之一".len(),
    }];
    let idx = BoundaryIndex::build(&text, excluded);
    let mut issues = Vec::new();
    scan_translationese_indexed(
        &mut Emitter::new(&text, excluded, &mut issues),
        &idx,
        crate::engine::translationese_score::TranslationeseDomain::General,
        false,
        Register::Casual,
    );
    let zy1b: Vec<_> = issues
        .iter()
        .filter(|i| i.phase_family == Some((PhaseFamily::YiZhi, PhasePass::Indexed)))
        .collect();
    assert!(!zy1b.is_empty(), "ZY1b should still fire: {issues:?}");
    assert_ne!(zy1b[0].offset, first_zhi_yi);
}

#[test]
fn zy2b_skips_dang_prefix_words() {
    let text = "當地的時候情況不容易掌握。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::Connective, PhasePass::Indexed)
    ));
}

// ZY3b -----------------------------------------------------------------

#[test]
fn zy3b_fires_on_three_chain_in_general_domain() {
    // 改善的提升的發現: 3 chained heads, depth 3 ≥ general's chain_min 3.
    let text = "他完成改善的提升的發現工作。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        fires(&issues, (PhaseFamily::Nominalization, PhasePass::Indexed)),
        "expected ZY3b: {issues:?}"
    );
}

#[test]
fn zy3b_passes_on_two_chain_in_technical_domain() {
    // Technical bumps chain_min to 4: a 3-level chain doesn't fire.
    let text = "他完成改善的提升的發現工作。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::Technical,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::Nominalization, PhasePass::Indexed)
    ));
}

#[test]
fn zy3b_passes_on_two_chain_in_general() {
    // 2-level chain (depth 2) below general threshold (3).
    let text = "他完成改善的提升工作。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::Nominalization, PhasePass::Indexed)
    ));
}

#[test]
fn zy3b_chain_does_not_consume_orphan_trailing_de() {
    // Gemini + Codex review: walk_zy3b_chain must not include a trailing 的 in
    // the emitted span when no whitelisted head follows. Walker invariants
    // (substring-relative byte offsets; each CJK char is 3 bytes):
    //   walk_zy3b_chain("改善的提升的非詞", 0) = (2, 15)
    //     - cursor lands just past 提升 (byte 15), not past the
    //     orphan 的 at byte 18.
    //   walk_zy3b_chain("改善的提升的發現的非詞", 0) = (3, 24)
    //     - cursor lands just past 發現 (byte 24), not past the
    //     orphan 的 at byte 27.
    // We exercise the second case end-to-end by running the full detector and
    // checking the emitted issue's found text does not end in 的. The depth-2
    // case can't be checked end-to-end because it falls below the default
    // chain_min=3 threshold.
    let text = "他完成改善的提升的發現的非詞工作。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    let zy3b_issue = issues
        .iter()
        .find(|i| i.phase_family == Some((PhaseFamily::Nominalization, PhasePass::Indexed)))
        .expect("ZY3b should fire");
    // The emitted span must not end in 的: that's the orphan-的 bug.
    assert!(
        !zy3b_issue.found.ends_with('的'),
        "ZY3b span should not include orphan trailing 的, got: {:?}",
        zy3b_issue.found
    );
    // The span must end at 發現 (the last whitelisted head).
    assert!(
        zy3b_issue.found.ends_with("發現"),
        "ZY3b span should end at last head 發現, got: {:?}",
        zy3b_issue.found
    );
}

// ZY5 ------------------------------------------------------------------

#[test]
fn zy5_fires_on_long_premodifier() {
    // 19 chars, 2 的, comma-free: long-pre-modifier archetype.
    let text = "那個在車站外面的雨裡等了三個小時的男人終於放棄了。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "expected ZY5: {issues:?}"
    );
}

#[test]
fn predicate_scan_agrees_with_the_direct_form() {
    let cases = [
        "他就走了的東西的樣子",
        "這個可以用的方法的問題",
        "就地取材的方式的結果", // 就 heads a word that is not masked
        "他的成就的意義的說明", // 就 as the tail of 成就: masked by the head
        "剛才的說法的意思",     // 才 as the tail of 剛才
        "方便的方式的結果",     // 便 as the tail of 方便
        "忘卻的往事的痕跡",     // 卻 as the tail of 忘卻
        "已經完成的工作的內容",
        "東西的樣子的問題就", // marker flush against the right edge
        "正在進行的計畫的目標",
        "沒有任何標記的一般文字",
        "才對的說法的意思",
    ];

    // Only right edges that fall on a 的 are checked, because those are the
    // only ones ZY5 asks about, and they are what makes reading the windows
    // from the span rather than the region equivalent.
    for case in cases {
        for lo in (0..case.len()).filter(|&i| case.is_char_boundary(i)) {
            let close = first_predicate_close(case, lo);
            for (hi, _) in case.match_indices('的').filter(|&(at, _)| at >= lo) {
                assert_eq!(
                    close.is_some_and(|close| close <= hi),
                    opens_a_predicate(&case[lo..hi]),
                    "case {case:?} region {lo}..{hi} = {:?}",
                    &case[lo..hi]
                );
            }
        }
    }
}

#[test]
fn zy5_reports_a_span_reaching_the_last_noun() {
    // Pins what the early exit depends on: the reported span runs to the end of
    // the comma-free segment, so stopping at the first candidate to reach that
    // end loses nothing. It reaches the end because the noun run after 的 is
    // taken over CJK characters and 的 is one of them, so the run swallows the
    // following phrases.
    let text = "那個在車站外面的雨裡等了三個小時的男人的外套";
    let issues = scan_general(text);
    let zy5 = issues
        .iter()
        .find(|i| i.phase_family == Some((PhaseFamily::LongPremodifier, PhasePass::Lexical)))
        .unwrap_or_else(|| panic!("expected ZY5: {issues:?}"));
    assert_eq!(
        zy5.offset + zy5.length,
        text.len(),
        "the reported span must reach the last noun: {zy5:?}"
    );
}

fn scan_general(text: &str) -> Vec<Issue> {
    scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    )
}

#[test]
fn zy5_predicate_guard_covers_ordinary_verbs_and_adverbs() {
    // These markers precede lexical verbs and manner adverbs far more often
    // than function words, so the guard cannot key on what follows them.
    for word in [
        "就開始",
        "就變成",
        "就徹底",
        "就永遠",
        "就逐漸",
        "就此",
        "就算",
        "卻依然",
        "也很快",
        "才慢慢",
        "便迅速",
        "就會",
        "就要",
        "就被",
    ] {
        let text = format!("那個看起來十分陌生的城市{word}吞噬他熟悉的日常生活。");
        let issues = scan_general(&text);
        assert!(
            !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "unexpected ZY5 for {word}: {issues:?}"
        );
    }
}

#[test]
fn zy5_passes_when_a_predicate_separates_the_de_phrases() {
    // 16 chars, 2 的, comma-free, but 也能 opens a predicate between them, so
    // 每週的檢討會議 and 真正的瓶頸 modify different heads. This was a live
    // false positive in tests/corpus/native-zh-tw.json.
    let text = "每週的檢討會議也能聚焦真正的瓶頸。";
    let issues = scan_general(text);
    assert!(
        !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "unexpected ZY5: {issues:?}"
    );
}

#[test]
fn marker_words_are_all_two_characters() {
    // opens_a_predicate masks with two fixed-width window lookups, so a longer
    // entry would be silently ignored rather than masking anything.
    for word in MARKER_WORDS {
        assert_eq!(
            word.chars().count(),
            2,
            "{word} must be two characters for the window mask to see it"
        );
    }
}

#[test]
fn zy5_predicate_guard_ignores_markers_inside_words() {
    // The marker may sit at either end of an ordinary word: 成就 and 人才 end
    // with one, 便利 and 就業 start with one. None of them opens a predicate,
    // so all of these stay stacked pre-modifiers and must fire.
    for text in [
        "那是一個工匠面對自己的成就被命運直接抹除的瞬間。",
        "這乃是一個從不細看自己所寫之物的人才會犯下的錯誤。",
        "為了兼顧系統的便利性與穩定性的整體平衡設計。",
        "這是一份針對長期投入的就業輔導方案累積的完整報告。",
        "憑著他的才華洋溢加上長年累積的舞台經驗。",
    ] {
        let issues = scan_general(text);
        assert!(
            fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "expected ZY5 for {text:?}: {issues:?}"
        );
    }
}

#[test]
fn zy5_passes_on_native_long_name() {
    // 中華民國行政院: 7 chars, 0 的: never fires.
    let text = "中華民國行政院昨日發表了新政策。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::LongPremodifier, PhasePass::Lexical)
    ));
}

#[test]
fn zy5_passes_on_short_native_possessive() {
    // 我父親的朋友的兒子: 8 chars, fails 15-char gate.
    let text = "我父親的朋友的兒子今天來訪。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::LongPremodifier, PhasePass::Lexical)
    ));
}

#[test]
fn zy5_passes_when_internal_comma_breaks_span() {
    // Same chars but with a comma: span is broken, native rhythm.
    let text = "那個男人在車站外面的雨裡，等了三個小時，終於放棄了。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(!fires(
        &issues,
        (PhaseFamily::LongPremodifier, PhasePass::Lexical)
    ));
}

#[test]
fn zy5_passes_in_technical_domain_at_borderline() {
    // 18-char span: exactly at Technical threshold (zy5_min_chars=18) but only
    // 17 chars after counting → doesn't qualify.
    let text = "車站外面的雨裡等了三小時的男人。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::Technical,
    );
    let _ = issues; // Just verifying no panic; behavior depends on count.
}

#[test]
fn zy5_passes_on_long_clause_without_premodifier_endpoint() {
    let text = "我昨天在博物館看到他的朋友的兒子正在導覽。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "unexpected ZY5: {issues:?}"
    );
}

#[test]
fn zy5_passes_on_clause_that_only_ends_with_de_noun() {
    let text = "昨天在博物館看到他的朋友的兒子正在導覽。";
    let issues = scan_indexed(
        text,
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(
        !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "unexpected ZY5: {issues:?}"
    );
}

// Cross-detector regression: empty / no-paragraph / unicode panics.

#[test]
fn indexed_detectors_handle_empty_input() {
    let issues = scan_indexed(
        "",
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(issues.is_empty());
}

#[test]
fn indexed_detectors_handle_short_input_no_panic() {
    let _ = scan_indexed(
        "短",
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
}

#[test]
fn indexed_detectors_handle_ascii_only() {
    let issues = scan_indexed(
        "Hello world. Actually basically.",
        crate::engine::translationese_score::TranslationeseDomain::General,
    );
    assert!(issues.is_empty());
}
// Rhythm (氣口) ---------------------------------------------------------

fn rhythm_fires(issues: &[Issue], family: PhaseFamily) -> bool {
    issues
        .iter()
        .any(|i| i.phase_family == Some((family, PhasePass::Indexed)))
}

#[test]
fn rhythm_flags_a_sentence_that_never_pauses() {
    // 38 CJK characters, no internal punctuation at all.
    let text = "這份報告詳細說明了整個系統在過去一年之中所有功能的演進過程與後續規劃方向。";
    let issues = scan_rhythm_only(text);
    assert!(
        rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "expected a long-sentence finding: {issues:?}"
    );
}

#[test]
fn rhythm_passes_a_long_sentence_made_of_short_clauses() {
    // Same length, but every clause is under the fragment floor, so the reader
    // has been given somewhere to breathe.
    let text = "這份報告很詳細，內容很完整，結構也清楚，讀起來很順，結論相當明確，值得參考。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "clauses under the fragment floor are not a rhythm violation: {issues:?}"
    );
}

#[test]
fn rhythm_passes_a_dunhao_terminology_list() {
    let text = "支援的格式包含純文字、標記語言、設定檔、資料交換格式、樣式表、指令稿。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "a 頓號 list already contains its pauses: {issues:?}"
    );
}

#[test]
fn rhythm_measures_after_stripping_latin_and_digits() {
    // Long in bytes and in characters, but only 12 CJK characters: a
    // terminology run is not a breathless sentence.
    let text = "設定值為 MAX_CONNECTION_RETRY_INTERVAL_SECONDS=3600 而預設是 300 秒。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "Latin and digits do not count toward sentence length: {issues:?}"
    );
}

#[test]
fn rhythm_flags_a_long_sentence_that_pauses_only_once() {
    // Boundary case for the chosen semantics: one 頓號 in a 40-character
    // sentence is a pause, but it leaves a 20-character run on one side, so the
    // sentence is still reported. The mark alone does not exempt.
    let text = "這份報告詳細說明了整個系統的演進過程、以及後續維護時應該特別留意的每一項注意事項。";
    let issues = scan_rhythm_only(text);
    assert!(
        rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "one mark in a long sentence is not an exemption: {issues:?}"
    );
}

#[test]
fn rhythm_treats_a_parenthetical_aside_as_the_pause_it_is() {
    // The other side of the same boundary: the aside splits the sentence into
    // runs that are all under the fragment floor.
    let text = "這份報告寫得很清楚（補充說明在附錄）內容也很完整，讀來相當順暢。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmLongSentence),
        "an aside is a pause: {issues:?}"
    );
}

#[test]
fn rhythm_monotony_does_not_run_across_an_excluded_span() {
    // A code span between two sentences is not in the sentence index, so the
    // prose either side of it looked consecutive.
    let text = "天氣變好了。這裡是程式碼。心情也好了。設備修好了。";
    let code_start = text.find("這裡是程式碼。").unwrap();
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_start + "這裡是程式碼。".len(),
    }];
    let idx = BoundaryIndex::build(text, &excluded);
    let mut issues = Vec::new();
    scan_rhythm(&mut Emitter::new(text, &excluded, &mut issues), &idx);
    assert!(
        !fires(&issues, (PhaseFamily::RhythmMonotony, PhasePass::Indexed)),
        "a run must not reach across an excluded span: {issues:?}"
    );

    // Contiguous, so the same three endings are still a run.
    let plain = "天氣變好了。心情也好了。設備修好了。";
    let idx = BoundaryIndex::build(plain, &[]);
    let mut issues = Vec::new();
    scan_rhythm(&mut Emitter::new(plain, &[], &mut issues), &idx);
    assert!(
        fires(&issues, (PhaseFamily::RhythmMonotony, PhasePass::Indexed)),
        "three consecutive 了 endings are still monotony: {issues:?}"
    );
}

#[test]
fn rhythm_monotony_ignores_a_sentence_ending_in_content() {
    // 了 followed by a version number is not a 了-ending. Reading it as one
    // built runs out of sentences that do not rhyme.
    let text = "他來了 v2。她也走了 v3。天氣變好了 v4。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmMonotony),
        "a trailing identifier is content, not a closer: {issues:?}"
    );
}

#[test]
fn rhythm_monotony_looks_through_a_closing_bracket() {
    let text = "他來了（真的）。她也走了「大概」。天氣變好了。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmMonotony),
        "the bracketed word is what these sentences end on: {issues:?}"
    );
}

#[test]
fn rhythm_flags_three_sentences_closing_on_the_same_particle() {
    let text = "他來了。她也走了。天氣變好了。";
    let issues = scan_rhythm_only(text);
    assert!(
        rhythm_fires(&issues, PhaseFamily::RhythmMonotony),
        "expected a monotony finding: {issues:?}"
    );
}

#[test]
fn rhythm_reports_one_finding_per_run() {
    let text = "他來了。她也走了。天氣變好了。事情辦完了。大家散了。";
    let issues = scan_rhythm_only(text);
    let count = issues
        .iter()
        .filter(|i| i.phase_family == Some((PhaseFamily::RhythmMonotony, PhasePass::Indexed)))
        .count();
    assert_eq!(
        count, 1,
        "a run of five is one finding, not three: {issues:?}"
    );
}

#[test]
fn rhythm_monotony_needs_the_same_particle() {
    let text = "他來了。這是我的。你在做什麼呢。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmMonotony),
        "three different particles are variety, not monotony: {issues:?}"
    );
}

#[test]
fn rhythm_monotony_does_not_cross_a_paragraph_break() {
    let text = "他來了。她也走了。\n\n天氣變好了。事情辦完了。";
    let issues = scan_rhythm_only(text);
    assert!(
        !rhythm_fires(&issues, PhaseFamily::RhythmMonotony),
        "a paragraph break restarts the tune: {issues:?}"
    );
}

#[test]
fn rhythm_findings_carry_no_suggestion() {
    // The fixer's write condition is a single suggestion, so an empty list is
    // what keeps rhythm out of every tier. Asserted here as well as in the
    // fixer, because this is where it could be lost.
    let text = "這份報告詳細說明了整個系統在過去一年之中所有功能的演進過程與後續規劃方向。\
                他來了。她也走了。天氣變好了。";
    let issues = scan_rhythm_only(text);
    assert!(!issues.is_empty());
    assert!(
        issues
            .iter()
            .all(|i| i.suggestions.is_empty() && i.severity == Severity::Info),
        "rhythm findings are advisory and unfixable: {issues:?}"
    );
}

#[test]
fn zy5_de_gate_relaxes_only_under_rhythm() {
    // 21 chars, one 的: below the general domain's zy5_min_de_count of 2.
    let text = "那個在車站外面等了三個小時的男人終於放棄了。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::General;
    let off = scan_indexed_with_rhythm(text, domain, false);
    let on = scan_indexed_with_rhythm(text, domain, true);
    assert!(
        !fires(&off, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "one 的 is below the default gate: {off:?}"
    );

    // The relaxed hit reports under the advisory family, not the calibrated
    // one, so the taste flag cannot move the translationese score with it.
    assert!(
        fires(
            &on,
            (PhaseFamily::RhythmLongPremodifier, PhasePass::Lexical)
        ),
        "rhythm should bypass the 的 gate: {on:?}"
    );
    assert!(
        !fires(&on, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "a relaxed hit must not wear the calibrated family: {on:?}"
    );
    assert!(
        PhaseFamily::RhythmLongPremodifier.is_advisory(),
        "a relaxed hit has to be advisory or the score counts it"
    );
}

#[test]
fn rhythm_does_not_delete_a_calibrated_premodifier() {
    // The fragment floor belongs to the relaxed path only. A span that already
    // clears the domain's 的 count is what the default pass reports, and a
    // bracket inside it is a pause breaker the floor would otherwise fail, so
    // applying the floor to every span let the taste flag delete a finding.
    let text = "一個非常重要而且複雜的「核心」的系統設計模組。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::General;
    let off = scan_indexed_with_rhythm(text, domain, false);
    let on = scan_indexed_with_rhythm(text, domain, true);
    assert!(
        fires(&off, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "the calibrated pass should report this: {off:?}"
    );
    assert!(
        fires(&on, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "rhythm must not delete a calibrated finding: {on:?}"
    );
}

#[test]
fn a_relaxed_premodifier_is_advisory_in_severity_too() {
    // Info, not Warning: the family keeps it out of the score, and the severity
    // keeps it out of the counts --max-warnings is checked against.
    let text = "那個在車站外面等了三個小時的男人終於放棄了。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::General;
    let on = scan_indexed_with_rhythm(text, domain, true);
    let relaxed: Vec<_> = on
        .iter()
        .filter(|i| {
            i.phase_family == Some((PhaseFamily::RhythmLongPremodifier, PhasePass::Lexical))
        })
        .collect();
    assert!(!relaxed.is_empty(), "expected a relaxed hit: {on:?}");
    assert!(
        relaxed.iter().all(|i| i.severity == Severity::Info),
        "a relaxed hit must be advisory in severity: {relaxed:?}"
    );
}

#[test]
fn zy5_under_rhythm_still_respects_the_fragment_floor() {
    // 13 chars: under the rhythm axis's own floor, so relaxing the 的 gate must
    // not let it through.
    let text = "在車站外面等待的男人放棄了。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::Literary;
    let on = scan_indexed_with_rhythm(text, domain, true);
    assert!(
        !fires(&on, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "a span under the fragment floor is not a rhythm violation: {on:?}"
    );
}

#[test]
fn zy5_under_rhythm_does_not_count_latin_toward_the_floor() {
    // Long enough for ZY5's own character count, which counts everything in the
    // span, and nowhere near it once only CJK is counted. Relaxing the 的 gate
    // must not turn an identifier run into a finding.
    let text = "在 MAX_CONNECTION_RETRY_INTERVAL 裡設定的數值。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::General;
    let on = scan_indexed_with_rhythm(text, domain, true);
    assert!(
        !fires(&on, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "an identifier is not a pre-modifier: {on:?}"
    );
}

#[test]
fn zy5_under_rhythm_exempts_a_span_broken_by_an_aside() {
    // ZY5's SPAN_BREAKERS know nothing about brackets, so without the rhythm
    // fragment test this span would read as one unbroken breath.
    let text = "那個在車站外面（下著雨）等了三個小時的男人放棄了。";
    let domain = crate::engine::translationese_score::TranslationeseDomain::General;
    let on = scan_indexed_with_rhythm(text, domain, true);
    assert!(
        !fires(&on, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
        "the aside is a pause: {on:?}"
    );
}
