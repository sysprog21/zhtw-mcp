    // Included by mod.rs inside its "mod tests" block, which is why everything
    // here sits at one extra indent level. Despite the filename nothing
    // generates it; edit it like any other test file.

    #[test]
    fn compound_term_takes_priority_over_shorter() {
        // When both "數據" and "數據庫" are patterns, "數據庫" should match as
        // a single compound via leftmost-longest, not as bare "數據".
        let rules = vec![
            SpellingRule::new("數據", vec!["資料".into()], RuleType::CrossStrait),
            SpellingRule::new("數據庫", vec!["資料庫".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);

        let issues = scanner.scan("這個數據庫很大").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "數據庫");
        assert_eq!(issues[0].suggestions[..], vec!["資料庫"]);

        // Bare "數據" without "庫" should still match the shorter pattern.
        let issues = scanner.scan("這些數據很重要").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "數據");
        assert_eq!(issues[0].suggestions[..], vec!["資料"]);
    }

    #[test]
    fn context_propagated_to_issue() {
        let ctx = "token: 驗證=權杖；加密貨幣=代幣；NLP=詞元";
        let rules = vec![
            SpellingRule {
                context: Some(ctx.into()),
                ..SpellingRule::new("令牌", vec!["權杖".into(), "代幣".into(), "詞元".into()], RuleType::CrossStrait)
            },
            SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);

        let issues = scanner.scan("這個令牌和軟件").issues;
        assert_eq!(issues.len(), 2);
        // Rule with context: context is propagated.
        assert_eq!(issues[0].found, "令牌");
        assert_eq!(issues[0].context, Some(ctx.into()));
        // Rule without context: context is None.
        assert_eq!(issues[1].found, "軟件");
        assert_eq!(issues[1].context, None);
    }

    fn algorithm_rule() -> Vec<SpellingRule> {
        vec![SpellingRule::new("算法", vec!["演算法".into()], RuleType::CrossStrait)]
    }

    #[test]
    fn already_correct_not_flagged() {
        // "演算法" already contains the correct form: no issue.
        let scanner = Scanner::new(algorithm_rule(), vec![]);
        let issues = scanner.scan("這個演算法很好用").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn wrong_form_still_flagged() {
        // Standalone "算法" without the "演" prefix is still wrong.
        let scanner = Scanner::new(algorithm_rule(), vec![]);
        let issues = scanner.scan("這個算法很好用").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "算法");
        assert_eq!(issues[0].suggestions[..], vec!["演算法"]);
    }

    #[test]
    fn mixed_correct_and_wrong() {
        // "演算法" (correct, skip) followed by standalone "算法" (wrong, flag).
        let scanner = Scanner::new(algorithm_rule(), vec![]);
        let issues = scanner.scan("演算法和算法").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "算法");
    }

    fn quote_rules() -> Vec<SpellingRule> {
        vec![
            SpellingRule {
                english: Some("left double quotation mark".into()),
                ..SpellingRule::new("\u{201c}", vec!["\u{300c}".into()], RuleType::CrossStrait)
            },
            SpellingRule {
                english: Some("right double quotation mark".into()),
                ..SpellingRule::new("\u{201d}", vec!["\u{300d}".into()], RuleType::CrossStrait)
            },
        ]
    }

    #[test]
    fn cn_quotes_balanced_pair() {
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("他說\u{201c}你好\u{201d}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "\u{201c}");
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(issues[1].found, "\u{201d}");
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_quotes_all_opening_fixed_by_pairing() {
        // Both quotes are \u{201c} (opening): pairing fix should make the
        // second one a closing 」.
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("他說\u{201c}你好\u{201c}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_quotes_all_closing_fixed_by_pairing() {
        // Both quotes are \u{201d} (closing): pairing fix should make the first
        // one an opening 「.
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("他說\u{201d}你好\u{201d}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_quotes_single_not_rewritten() {
        // Single quote: no pairing fix attempted.
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("他說\u{201c}你好").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
    }

    #[test]
    fn cn_quotes_reversed_pair_fixed() {
        // Closing before opening: \u{201d}...\u{201c}, balanced count but wrong
        // order. Pairing fix should correct to 「...」.
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("他說\u{201d}你好\u{201c}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_quotes_in_code_fence_excluded() {
        let scanner = Scanner::new(quote_rules(), vec![]);
        let issues = scanner.scan("看 `\u{201c}text\u{201d}` 的說明").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn wrong_repeated_in_correct_form() {
        // wrong="A" appears twice in correct="ABA" (at index 0 and 2). Text
        // "ABA" is the correct form: neither "A" should be flagged.
        let rules = vec![SpellingRule::new("A", vec!["ABA".into()], RuleType::Typo)];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("ABA").issues;
        assert!(
            issues.is_empty(),
            "should not flag parts of the correct form"
        );
    }

    // resolve_overlaps tests (inlined from engine/overlap.rs)

    fn overlap_issue(offset: usize, length: usize, sev: Severity) -> Issue {
        Issue::new(offset, length, "x".repeat(length), vec![], IssueType::CrossStrait, sev)
    }

    #[test]
    fn overlap_no_overlap() {
        let mut issues = vec![
            overlap_issue(0, 3, Severity::Warning),
            overlap_issue(5, 3, Severity::Warning),
        ];
        resolve_overlaps(&mut issues);
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn overlap_longer_wins() {
        let mut issues = vec![
            overlap_issue(0, 6, Severity::Warning),
            overlap_issue(2, 3, Severity::Warning),
        ];
        resolve_overlaps(&mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].length, 6);
    }

    #[test]
    fn overlap_higher_severity_wins_on_tie() {
        let mut issues = vec![
            overlap_issue(0, 4, Severity::Warning),
            overlap_issue(2, 4, Severity::Error),
        ];
        resolve_overlaps(&mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Error);
    }

    #[test]
    fn overlap_empty_and_single() {
        let mut empty: Vec<Issue> = vec![];
        resolve_overlaps(&mut empty);
        assert!(empty.is_empty());

        let mut single = vec![overlap_issue(0, 3, Severity::Info)];
        resolve_overlaps(&mut single);
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn overlap_ghost_suppression() {
        // A=(0,1) and C=(1,3) are non-overlapping. Under the old forward greedy
        // scan: B=(0,2) beats A, C=(1,3) beats B, yet A stays discarded even
        // though A and C never overlapped. The priority-based algorithm
        // processes C first (longest), accepts it, then accepts A
        // (non-overlapping with C), and rejects B (overlaps C).
        let mut issues = vec![
            overlap_issue(0, 1, Severity::Warning), // A: [0,1)
            overlap_issue(0, 2, Severity::Warning), // B: [0,2)
            overlap_issue(1, 3, Severity::Warning), // C: [1,4)
        ];
        // Pre-sort as the scanner does: offset ASC, length DESC on tie.
        issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(b.length.cmp(&a.length)));
        resolve_overlaps(&mut issues);
        // C wins; A is non-overlapping with C and must survive.
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].offset, 0);
        assert_eq!(issues[0].length, 1); // A
        assert_eq!(issues[1].offset, 1);
        assert_eq!(issues[1].length, 3); // C
    }

    // is_cjk_ideograph tests

    #[test]
    fn cjk_ideograph_basic() {
        assert!(is_cjk_ideograph('你'));
        assert!(is_cjk_ideograph('好'));
        assert!(is_cjk_ideograph('世'));
        assert!(!is_cjk_ideograph('a'));
        assert!(!is_cjk_ideograph('1'));
        assert!(!is_cjk_ideograph('，')); // full-width comma is CJK Symbol, not ideograph
        assert!(!is_cjk_ideograph('。')); // full-width period is CJK Symbol
    }

    #[test]
    fn cjk_ideograph_bopomofo() {
        assert!(is_cjk_ideograph('ㄅ')); // U+3105
        assert!(is_cjk_ideograph('ㄆ')); // U+3106
    }

    // -- punctuation scan tests -----------------------------------------------

    fn empty_scanner() -> Scanner {
        Scanner::new(vec![], vec![])
    }

    #[test]
    fn punct_comma_cjk_both_sides() {
        let scanner = empty_scanner();
        let issues = scanner.scan("你好, 世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
        assert_eq!(issues[0].suggestions[..], vec!["，"]);
        assert_eq!(issues[0].rule_type, IssueType::Punctuation);
    }

    #[test]
    fn punct_comma_cjk_before() {
        let scanner = empty_scanner();
        let issues = scanner.scan("你好, world").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_comma_cjk_after() {
        let scanner = empty_scanner();
        let issues = scanner.scan("Hello, 世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_comma_english_only_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("Hello, world").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_comma_thousands_separator_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("1,000").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_period_cjk_before() {
        let scanner = empty_scanner();
        let issues = scanner.scan("這是句子.").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
        assert_eq!(issues[0].rule_type, IssueType::Punctuation);
    }

    #[test]
    fn punct_period_followed_by_space_then_cjk() {
        let scanner = empty_scanner();
        let issues = scanner.scan("這是句子. 再見").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
    }

    #[test]
    fn punct_period_english_only_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("Hello world.").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_period_decimal_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("3.14").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_period_file_extension_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("file.txt").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_ellipsis_ascii_dots() {
        let scanner = empty_scanner();
        // 3+ ASCII dots adjacent to CJK → ……
        let issues = scanner.scan("等一下...").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "...");
        assert_eq!(issues[0].suggestions[..], vec!["……"]);
        // 2 dots is not enough for ellipsis detection.
        assert!(scanner.scan("等一下..").issues.is_empty());
    }

    #[test]
    fn punct_in_code_fence_excluded() {
        let scanner = empty_scanner();
        let issues = scanner.scan("看 `a, b.` 的說明").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_already_fullwidth_no_flag() {
        let scanner = empty_scanner();
        let issues = scanner.scan("你好，世界。").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_mixed_with_spelling() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("這個軟件, 很好用.").issues;
        assert_eq!(issues.len(), 3); // 軟件 + comma + period
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[1].found, ",");
        assert_eq!(issues[2].found, ".");
    }

    #[test]
    fn punct_no_space_comma() {
        let scanner = empty_scanner();
        let issues = scanner.scan("你好,世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_period_followed_by_alpha_no_flag() {
        // e.g. abbreviations or identifiers
        let scanner = empty_scanner();
        let issues = scanner.scan("方法.call()").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn punct_period_after_cjk_closing_quote() {
        // 」(U+300D) is CJK punctuation: should count as CJK context.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說「你好」.").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
    }

    #[test]
    fn punct_comma_after_cjk_closing_quote() {
        let scanner = empty_scanner();
        let issues = scanner.scan("「你好」, 世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_period_after_cjk_bracket() {
        let scanner = empty_scanner();
        let issues = scanner.scan("參考【附錄】.").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
    }

    #[test]
    fn punct_comma_ideographic_space() {
        // U+3000 ideographic space must be skipped to find CJK adjacency.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好\u{3000}, 世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_multiple_commas() {
        let scanner = empty_scanner();
        let issues = scanner.scan("蘋果, 香蕉, 橘子").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, ",");
        assert_eq!(issues[1].found, ",");
    }

    // -- punctuation edge case tests ------------------------------------------

    #[test]
    fn punct_edge_period_at_position_zero() {
        // Period at byte 0 with CJK after. The period rule requires the
        // PRECEDING non-whitespace char to be CJK. At position 0 there is
        // nothing before, so adjacent_cjk(text, 0, true) returns false. No
        // issue expected.
        let scanner = empty_scanner();
        let issues = scanner.scan(".你好").issues;
        assert!(
            issues.is_empty(),
            "period at pos 0 has no preceding CJK context"
        );
    }

    #[test]
    fn punct_edge_comma_at_position_zero() {
        // Comma at byte 0 with CJK after. The comma rule fires when at least
        // one adjacent non-whitespace char is CJK. adjacent_cjk forward from
        // byte 1 finds '你' (CJK ideograph), so the comma should be flagged.
        let scanner = empty_scanner();
        let issues = scanner.scan(",你好").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
        assert_eq!(issues[0].offset, 0);
        assert_eq!(issues[0].suggestions[..], vec!["，"]);
    }

    #[test]
    fn punct_edge_comma_at_end_of_string() {
        // Comma at end of string. CJK before ('好') satisfies the "at least one
        // adjacent CJK" requirement. Forward check finds nothing (empty tail),
        // which is not CJK, but one side is enough.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好,").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ",");
    }

    #[test]
    fn punct_edge_period_at_end_of_string() {
        // Period at end of string. CJK before ('好') satisfies the
        // preceding-CJK requirement. Nothing follows, so the "followed by
        // alphanumeric" guard does not trigger.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好.").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
    }

    #[test]
    fn punct_edge_multiple_periods_ellipsis() {
        // Four consecutive periods after CJK → single ellipsis issue.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好....").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "....");
        assert_eq!(issues[0].suggestions[..], vec!["……"]);
    }

    #[test]
    fn punct_edge_period_followed_by_open_paren() {
        // Period followed by '(' -- not ASCII alphanumeric, so the
        // extension/decimal guard does not trigger. CJK before ('好') satisfies
        // the preceding-CJK check.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好.(").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
    }

    #[test]
    fn punct_edge_period_followed_by_close_paren() {
        // Period followed by ')' -- same logic as open paren. Not ASCII
        // alphanumeric, CJK before, period should flag.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好.)").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
    }

    #[test]
    fn punct_edge_comma_between_fullwidth_digits() {
        // Fullwidth digits '１' (U+FF11) and '２' (U+FF12) are multi-byte
        // UTF-8, so bytes[i-1] and bytes[i+1] are NOT ASCII digits -- the
        // thousands separator guard does not trigger.
        //
        // is_cjk_context('１') returns true because U+FF11 is in the FF01..FF60
        // fullwidth forms range. So adjacent_cjk backward finds '１' as CJK
        // context and the comma IS flagged.
        let scanner = empty_scanner();
        let issues = scanner.scan("１,２").issues;
        // 3 issues: fullwidth digit １, half-width comma, fullwidth digit ２.
        assert_eq!(issues.len(), 3);
        let comma: Vec<_> = issues.iter().filter(|i| i.found == ",").collect();
        assert_eq!(comma.len(), 1);
        assert_eq!(comma[0].suggestions[..], vec!["，"]);
    }

    #[test]
    fn punct_edge_whitespace_only_context_no_flag() {
        // Comma surrounded by whitespace only -- no CJK context on either side.
        // adjacent_cjk skips whitespace and finds nothing, so both backward and
        // forward checks return false. No issue expected.
        let scanner = empty_scanner();
        let issues = scanner.scan("  ,  ").issues;
        assert!(
            issues.is_empty(),
            "comma with only whitespace around should not flag"
        );
    }

    #[test]
    fn punct_edge_fullwidth_comma_before_halfwidth_period() {
        // '，' (U+FF0C) is in the FF01..FF60 fullwidth forms range, so
        // is_cjk_context returns true. The period's preceding-CJK check
        // (adjacent_cjk backward) finds '，' and returns true. No adjacent
        // period, nothing follows, so the period should be flagged.
        let scanner = empty_scanner();
        let issues = scanner.scan("，.").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
    }

    #[test]
    fn punct_edge_spelling_rule_with_comma_overlap_resolution() {
        // A spelling rule whose matched text sits next to punctuation. The
        // spelling issue and punctuation issue have different byte offsets and
        // lengths, so overlap resolution keeps both.
        let rules = vec![SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait)];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("軟件,好用").issues;

        // "軟件" is a spelling issue; "," is a punctuation issue. They don't
        // overlap (軟件 = 6 bytes at offset 0, comma at offset 6).
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[0].rule_type, IssueType::CrossStrait);
        assert_eq!(issues[1].found, ",");
        assert_eq!(issues[1].rule_type, IssueType::Punctuation);
    }

    #[test]
    fn punct_edge_period_followed_by_newline() {
        // Period followed by newline '\n'. Newline (0x0A) is not ASCII
        // alphanumeric, so the extension/decimal guard does not trigger. CJK
        // before ('好') satisfies the preceding check.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好.\n再見").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, ".");
        assert_eq!(issues[0].suggestions[..], vec!["。"]);
    }

    #[test]
    fn punct_edge_consecutive_commas_both_flagged() {
        // Two consecutive commas between CJK text. First comma: CJK before
        // ('好'), forward finds ',' which is not CJK -- but one CJK side is
        // enough. Second comma: backward finds ',' which is not CJK, but
        // forward finds '世' which IS CJK. Both commas should be flagged
        // independently.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好,,世界").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, ",");
        assert_eq!(issues[1].found, ",");
        // Verify they are at distinct byte offsets.
        assert_ne!(issues[0].offset, issues[1].offset);
    }

    // line/col position tests

    #[test]
    fn line_col_single_line() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("這個軟件很好用").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 1);
        assert_eq!(issues[0].col, 3); // 這(1) 個(2) 軟件(3)
    }

    #[test]
    fn line_col_multi_line() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("第一行\n這個軟件在第二行").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 2);
        assert_eq!(issues[0].col, 3); // 這(1) 個(2) 軟件(3)
    }

    #[test]
    fn line_col_mixed_ascii_cjk() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        // Line 1: "Hello 你好\n" Line 2: "The 軟件 is good"
        let issues = scanner.scan("Hello 你好\nThe 軟件 is good").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 2);
        // "The " = 4 chars (UTF-16 code units), so 軟件 at col 5
        assert_eq!(issues[0].col, 5);
    }

    #[test]
    fn line_col_punctuation() {
        let scanner = empty_scanner();
        let issues = scanner.scan("第一行\n你好, 世界").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 2);
        // 你(1) 好(2) ,(3)
        assert_eq!(issues[0].col, 3);
    }

    // deterministic output ordering tests

    #[test]
    fn deterministic_ordering_offset_ascending() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("內存和軟件").issues;
        assert_eq!(issues.len(), 2);
        // 內存 at byte 0, 軟件 at byte 9 (內存=6b + 和=3b)
        assert!(issues[0].offset < issues[1].offset);
    }

    #[test]
    fn deterministic_ordering_mixed_types() {
        // Spelling issue and punctuation issue at different offsets: output
        // must be sorted by offset ascending.
        let rules = vec![SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait)];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("軟件, 好用.").issues;
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].rule_type, IssueType::CrossStrait);
        assert_eq!(issues[1].rule_type, IssueType::Punctuation);
        assert_eq!(issues[2].rule_type, IssueType::Punctuation);
        assert!(issues[0].offset < issues[1].offset);
        assert!(issues[1].offset < issues[2].offset);
    }

    // -- Markdown exclusion tests (pulldown-cmark handles both plain & md)

    #[test]
    fn markdown_code_block_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "前言\n```\n軟件\n```\n後語\n";
        let issues = scanner.scan(md).issues;
        // "軟件" inside fenced code block should be excluded.
        assert!(
            issues.is_empty(),
            "code block content should be excluded: {issues:?}"
        );
    }

    #[test]
    fn markdown_inline_code_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "使用 `軟件` 來測試\n";
        let issues = scanner.scan(md).issues;
        assert!(
            issues.is_empty(),
            "inline code content should be excluded: {issues:?}"
        );
    }

    #[test]
    fn markdown_text_outside_code_detected() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "這個軟件很好\n```\ncode\n```\n";
        let issues = scanner.scan(md).issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "軟件");
    }

    #[test]
    fn markdown_frontmatter_values_are_scanned() {
        // Frontmatter VALUES are now scanned (key+colon and "---" fences are
        // still excluded). This catches lint issues in title/description that
        // were previously hidden.
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "---\ntitle: 軟件測試\n---\n這是正文\n";
        let issues = scanner
            .scan_for_content_type(md, ContentType::Markdown, Profile::Base)
            .issues;
        // "軟件" in the value should now be detected.
        assert_eq!(issues.len(), 1, "expected one issue, got: {issues:?}");
        assert_eq!(issues[0].found, "軟件");
    }

    #[test]
    fn markdown_table_cell_coordinates_attached() {
        // Issues inside a Markdown table cell get (row, col) coordinates for
        // editor integration / SARIF region output.
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "| 標題 A | 標題 B |\n|---|---|\n| 正文 | 軟件 |\n";
        let issues = scanner
            .scan_for_content_type(md, ContentType::Markdown, Profile::Base)
            .issues;
        let cell_issue = issues
            .iter()
            .find(|i| i.found == "軟件")
            .expect("expected 軟件 issue");
        let cell = cell_issue
            .table_cell
            .expect("issue inside table cell should have table_cell metadata");

        // The body row is row index 1 (header is 0). Column 1 is the second
        // cell.
        assert_eq!(cell.row, 1, "expected body row 1, got {cell:?}");
        assert_eq!(cell.col, 1, "expected column 1, got {cell:?}");
    }

    #[test]
    fn heading_boost_preserves_sort_contract() {
        // cubic review: mutating severity post-sort can leave issues out of
        // (offset asc, severity desc) order. Ensure the output remains sorted
        // after the heading boost promotes Warning → Error.
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "# 軟件與硬盤管理\n\n軟件是正文的一部分。\n";
        let issues = scanner
            .scan_for_content_type(md, ContentType::Markdown, Profile::Base)
            .issues;
        // Verify sort contract: offsets ascending.
        for pair in issues.windows(2) {
            assert!(
                pair[0].offset <= pair[1].offset,
                "issue offsets must be ascending, got {} > {}: {issues:?}",
                pair[0].offset,
                pair[1].offset
            );
            if pair[0].offset == pair[1].offset {
                // Severity descending on offset tie (Error > Warning > Info).
                assert!(
                    pair[0].severity >= pair[1].severity,
                    "severity must be descending on offset tie, got {:?} < {:?}",
                    pair[0].severity,
                    pair[1].severity
                );
            }
        }
        // The heading issues should be Error; body issues Warning.
        let heading_issue = issues
            .iter()
            .find(|i| i.found == "軟件" && i.line == 1)
            .expect("expected 軟件 in heading");
        assert_eq!(heading_issue.severity, Severity::Error);
    }

    #[test]
    fn markdown_html_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "<div>軟件</div>\n正文\n";
        let issues = scanner.scan(md).issues;
        // "軟件" inside HTML block should be excluded.
        assert!(
            issues.is_empty(),
            "HTML block content should be excluded: {issues:?}"
        );
    }

    #[test]
    fn markdown_url_still_excluded() {
        // CJK text inside a URL path (IRI) is excluded; prose after the URL is
        // still scanned. [^\s「」『』《》]+ allows Unicode path segments but
        // stops at the six CJK quote/bracket characters.
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "看 https://example.com/軟件/path 的資料\n";
        let issues = scanner.scan(md).issues;
        // "軟件" inside the URL should be excluded (not flagged).
        assert!(
            issues.is_empty(),
            "URL content should be excluded: {issues:?}"
        );
    }

    #[test]
    fn markdown_mixed_exclusions() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        // Code block must start on its own line for pulldown-cmark.
        let md = "這個軟件, `內存` 問題\n\n```\n服務器\n```\n\n都有問題.\n";
        let issues = scanner.scan(md).issues;

        // "軟件" in plain text flagged; "內存" in inline code excluded;
        // "服務器" in code block excluded; comma and period flagged.
        let found: Vec<&str> = issues.iter().map(|i| i.found.as_str()).collect();
        assert!(found.contains(&"軟件"), "軟件 should be flagged");
        assert!(
            !found.contains(&"內存"),
            "內存 in inline code should be excluded"
        );
        assert!(
            !found.contains(&"服務器"),
            "服務器 in code block should be excluded"
        );
    }

    #[test]
    fn markdown_suppression_respected() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let md = "這個軟件很好\n<!-- zhtw:ignore-next-line -->\n這個內存也好\n";
        let issues = scanner.scan(md).issues;
        // "軟件" should be flagged; "內存" on the suppressed line excluded.
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "軟件");
    }

    #[test]
    fn markdown_indented_code_block_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        // 4-space indented code block (pulldown-cmark recognizes this).
        let md = "前言\n\n    軟件\n\n後語\n";
        let issues = scanner.scan(md).issues;
        // "軟件" inside indented code block should be excluded.
        assert!(
            issues.is_empty(),
            "indented code block content should be excluded: {issues:?}"
        );
    }

    // Full-width punctuation: !, ?, ;, (, )

    #[test]
    fn halfwidth_exclamation_flagged_near_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("太好了!").issues;
        let found: Vec<&str> = issues.iter().map(|i| i.found.as_str()).collect();
        assert!(
            found.contains(&"!"),
            "half-width ! after CJK should be flagged"
        );
        let exc = issues.iter().find(|i| i.found == "!").unwrap();
        assert_eq!(exc.suggestions[..], vec!["！"]);
    }

    #[test]
    fn halfwidth_exclamation_not_flagged_ascii() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("Hello world!").issues;
        let found: Vec<&str> = issues.iter().map(|i| i.found.as_str()).collect();
        assert!(
            !found.contains(&"!"),
            "! in pure-ASCII context should not be flagged"
        );
    }

    #[test]
    fn halfwidth_question_flagged_near_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("你好嗎?").issues;
        let q = issues.iter().find(|i| i.found == "?").unwrap();
        assert_eq!(q.suggestions[..], vec!["？"]);
    }

    #[test]
    fn halfwidth_question_not_flagged_ascii() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("really?").issues;
        assert!(
            !issues.iter().any(|i| i.found == "?"),
            "? in ASCII context should not be flagged"
        );
    }

    #[test]
    fn halfwidth_semicolon_flagged_near_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("前者;後者").issues;
        let s = issues.iter().find(|i| i.found == ";").unwrap();
        assert_eq!(s.suggestions[..], vec!["；"]);
    }

    #[test]
    fn halfwidth_semicolon_not_flagged_in_code() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("var x = 1;").issues;
        assert!(
            !issues.iter().any(|i| i.found == ";"),
            "; in ASCII code should not be flagged"
        );
    }

    #[test]
    fn halfwidth_parens_flagged_cjk_both_sides() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("中文(測試)中文").issues;
        let parens: Vec<&str> = issues
            .iter()
            .filter(|i| i.found == "(" || i.found == ")")
            .map(|i| i.found.as_str())
            .collect();
        assert_eq!(
            parens,
            vec!["(", ")"],
            "parens with CJK on both sides should be flagged"
        );
    }

    #[test]
    fn halfwidth_parens_not_flagged_mixed() {
        let scanner = Scanner::new(vec![], vec![]);
        // Markdown-like pattern: ] before ( and space after )
        let issues = scanner.scan("這是 [hi](test) 的文字").issues;
        assert!(
            !issues.iter().any(|i| i.found == "(" || i.found == ")"),
            "parens in markdown-like context should not be flagged"
        );
    }

    #[test]
    fn halfwidth_parens_not_flagged_ascii_function() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("call foo() now").issues;
        assert!(
            !issues.iter().any(|i| i.found == "(" || i.found == ")"),
            "parens in function call should not be flagged"
        );
    }

    #[test]
    fn halfwidth_exclamation_in_url_excluded() {
        let scanner = Scanner::new(vec![], vec![]);
        // URL should be excluded entirely
        let issues = scanner
            .scan("請看 https://example.com/path!bang 的頁面")
            .issues;
        assert!(
            !issues.iter().any(|i| i.found == "!"),
            "! inside URL should be excluded"
        );
    }

    #[test]
    fn ascii_double_quotes_flagged_in_cjk_prose() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("對於核心應該有多\"大\"").issues;
        let quotes: Vec<_> = issues.iter().filter(|i| i.found == "\"").collect();
        assert_eq!(quotes.len(), 2, "ASCII quotes in CJK prose should be flagged");
        assert_eq!(quotes[0].suggestions[..], vec!["「"]);
        assert_eq!(quotes[1].suggestions[..], vec!["」"]);
        assert!(quotes.iter().all(|i| i.rule_type == IssueType::Punctuation));
    }

    #[test]
    fn ascii_double_quotes_not_flagged_in_ascii_context() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("core should be \"big\"").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "ASCII quotes in pure ASCII context should not be flagged"
        );
    }

    #[test]
    fn ascii_double_quotes_in_url_excluded() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("請看 https://example.com/?q=\"core\" 的頁面")
            .issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "\" inside URL should be excluded"
        );
    }

    #[test]
    fn ascii_double_quotes_in_code_excluded() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("請使用 `println!(\"x\")` 測試").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "\" in inline code should be excluded"
        );
    }

    #[test]
    fn ascii_double_quotes_english_term_in_cjk_flagged() {
        // One token with no space in it is a term, not a quotation, so the
        // Chinese sentence around it gets 「」. The multi-word cases below are
        // what issue #132 leaves alone.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\"hello\"").issues;
        let quotes: Vec<_> = issues.iter().filter(|i| i.found == "\"").collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["「"]);
        assert_eq!(quotes[1].suggestions[..], vec!["」"]);
    }

    #[test]
    fn ascii_double_quotes_term_needs_cjk_beside_it() {
        // Same single token, but the prose around it is English throughout.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("He pressed \"Enter\" then left.").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "a term in English prose is not a zh-TW quotation"
        );
    }

    #[test]
    fn ascii_double_quotes_chinese_phrase_in_english_prose_not_flagged() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("He said \"你好\" then left.").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "Chinese inside English smart-quote typography is not zh-TW prose"
        );
    }

    #[test]
    fn curly_double_quotes_chinese_phrase_in_english_prose_not_flagged() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("He said “你好” then left.").issues;
        assert!(
            !issues.iter().any(|i| i.found == "“" || i.found == "”"),
            "Chinese inside English smart-quote typography is not zh-TW prose"
        );
    }

    #[test]
    fn ascii_double_quotes_english_sentence_in_cjk_not_flagged() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("他引用了原句：\"Do one thing, and do it well.\"，這是 Unix 哲學。")
            .issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "an English sentence quoted in CJK prose should not be converted"
        );
    }

    #[test]
    fn ascii_double_quotes_never_half_converted() {
        // Only the opening mark has CJK beside it. Before pairing, that half
        // alone was rewritten and the quotation came out unbalanced.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他引用了原句：\"Do one thing.\"").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "a pair converts on both halves or on neither"
        );
    }

    #[test]
    fn ascii_double_quotes_unpaired_keeps_adjacency() {
        // An unpaired mark has no span to judge, so it keeps the older rule.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\"你好").issues;
        let quotes: Vec<_> = issues.iter().filter(|i| i.found == "\"").collect();
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].suggestions[..], vec!["「"]);
    }

    // Full-width colon

    #[test]
    fn halfwidth_colon_flagged_near_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("標題:內容").issues;
        let c = issues.iter().find(|i| i.found == ":").unwrap();
        assert_eq!(c.suggestions[..], vec!["："]);
    }

    #[test]
    fn halfwidth_colon_not_flagged_in_time() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("會議在 12:30 開始").issues;
        assert!(
            !issues.iter().any(|i| i.found == ":"),
            ": in time format should not be flagged"
        );
    }

    #[test]
    fn halfwidth_colon_not_flagged_in_url() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("請看 https://example.com 的頁面").issues;
        assert!(
            !issues.iter().any(|i| i.found == ":"),
            ": in URL should not be flagged"
        );
    }

    #[test]
    fn halfwidth_colon_allowed_with_relaxed() {
        let scanner = Scanner::new(vec![], vec![]);
        let cfg = Profile::Base.config().with_relaxed();
        let issues = scanner
            .scan_for_content_type_with_config("標題:內容", ContentType::Plain, cfg)
            .issues;
        assert!(
            !issues.iter().any(|i| i.found == ":"),
            ": should be allowed with relaxed capability"
        );
    }

    #[test]
    fn halfwidth_colon_flagged_in_base_profile() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan_profiled("標題:內容", Profile::Base).issues;
        assert!(
            issues.iter().any(|i| i.found == ":"),
            ": should be flagged in Base profile"
        );
    }

    // Enumeration comma (dunhao)

    #[test]
    fn dunhao_suggested_for_short_item_list() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("紅，橙，黃，綠，藍").issues;
        let dunhao: Vec<_> = issues
            .iter()
            .filter(|i| i.suggestions.contains(&"、".to_string()))
            .collect();
        assert!(
            !dunhao.is_empty(),
            "short items separated by ， should suggest 、"
        );
        // All should be Info severity.
        for d in &dunhao {
            assert_eq!(d.severity, Severity::Info);
        }
    }

    #[test]
    fn dunhao_not_suggested_for_long_clauses() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("我喜歡游泳，他喜歡跑步，她喜歡畫畫").issues;
        let dunhao: Vec<_> = issues
            .iter()
            .filter(|i| i.suggestions.contains(&"、".to_string()))
            .collect();
        assert!(
            dunhao.is_empty(),
            "long clauses separated by ， should not suggest 、"
        );
    }

    #[test]
    fn dunhao_not_triggered_for_two_items() {
        let scanner = Scanner::new(vec![], vec![]);
        // Only two items: not enough for a list heuristic.
        let issues = scanner.scan("紅，藍").issues;
        let dunhao: Vec<_> = issues
            .iter()
            .filter(|i| i.suggestions.contains(&"、".to_string()))
            .collect();
        assert!(
            dunhao.is_empty(),
            "two items should not trigger dunhao suggestion"
        );
    }

    // Quotation mark hierarchy

    #[test]
    fn quotes_converted_to_corner_brackets() {
        // Quote conversion uses curly quotes (\u{201c}/\u{201d}) via spelling
        // rules.
        let rules = vec![
            SpellingRule::new("\u{201c}", vec!["\u{300c}".into()], RuleType::CrossStrait),
            SpellingRule::new("\u{201d}", vec!["\u{300d}".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("他說\u{201c}你好\u{201d}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn nested_quotes_use_secondary_brackets() {
        // Outer: curly double quotes → 「」; inner: same quotes → 『』 (depth
        // 1).
        let rules = vec![
            SpellingRule::new("\u{201c}", vec!["\u{300c}".into()], RuleType::CrossStrait),
            SpellingRule::new("\u{201d}", vec!["\u{300d}".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        // Nested: "outer "inner" outer"
        let text = "他說\u{201c}她說\u{201c}你好\u{201d}了\u{201d}";
        let issues = scanner.scan(text).issues;
        assert_eq!(issues.len(), 4);
        // Outer open → 「, inner open → 『, inner close → 』, outer close → 」
        assert_eq!(issues[0].suggestions[0], "\u{300c}"); // 「
        assert_eq!(issues[1].suggestions[0], "\u{300e}"); // 『
        assert_eq!(issues[2].suggestions[0], "\u{300f}"); // 』
        assert_eq!(issues[3].suggestions[0], "\u{300d}"); // 」
    }

    #[test]
    fn quotes_paragraph_break_resets_depth() {
        let rules = vec![
            SpellingRule::new("\u{201c}", vec!["\u{300c}".into()], RuleType::CrossStrait),
            SpellingRule::new("\u{201d}", vec!["\u{300d}".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        // Unmatched open in first paragraph; paragraph break resets depth.
        let text = "他說\u{201c}你好\n\n她說\u{201c}再見\u{201d}";
        let issues = scanner.scan(text).issues;
        // Second paragraph's quotes should be primary (depth 0 after reset).
        let para2: Vec<_> = issues
            .iter()
            .filter(|i| {
                (i.found == "\u{201c}" || i.found == "\u{201d}") && i.offset > 16
                // past \n\n
            })
            .collect();
        assert!(para2.len() >= 2, "should have 2 quotes in para2");
        assert_eq!(para2[0].suggestions[0], "\u{300c}"); // 「
        assert_eq!(para2[1].suggestions[0], "\u{300d}"); // 」
    }

    // CN curly quotes detected by punctuation scanner (no spelling rules)

    #[test]
    fn cn_curly_double_quotes_detected_without_spelling_rules() {
        // Scanner with NO spelling rules still detects CN curly double quotes
        // via the punctuation scanner.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{201c}你好\u{201d}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "\u{201c}");
        assert_eq!(issues[0].suggestions[..], vec!["\u{300c}"]); // 「
        assert_eq!(issues[1].found, "\u{201d}");
        assert_eq!(issues[1].suggestions[..], vec!["\u{300d}"]); // 」
    }

    #[test]
    fn cn_curly_single_quotes_detected() {
        // CN single curly quotes → TW secondary bracket quotes 『/』.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{2018}你好\u{2019}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "\u{2018}");
        assert_eq!(issues[0].suggestions[..], vec!["\u{300e}"]); // 『
        assert_eq!(issues[1].found, "\u{2019}");
        assert_eq!(issues[1].suggestions[..], vec!["\u{300f}"]); // 』
    }

    #[test]
    fn cn_curly_nested_double_quotes_without_rules() {
        // Nested CN curly double quotes detected and depth-adjusted by
        // fix_quote_pairing without any spelling rules needed.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}她說\u{201c}你好\u{201d}了\u{201d}";
        let issues = scanner.scan(text).issues;
        assert_eq!(issues.len(), 4);
        assert_eq!(issues[0].suggestions[0], "\u{300c}"); // 「
        assert_eq!(issues[1].suggestions[0], "\u{300e}"); // 『
        assert_eq!(issues[2].suggestions[0], "\u{300f}"); // 』
        assert_eq!(issues[3].suggestions[0], "\u{300d}"); // 」
    }

    #[test]
    fn cn_curly_mixed_double_and_single_quotes() {
        // Mixed: CN double quotes wrapping CN single quotes.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}她說\u{2018}你好\u{2019}了\u{201d}";
        let issues = scanner.scan(text).issues;
        assert_eq!(issues.len(), 4);
        // Outer double → 「/」, inner single → 『/』
        assert_eq!(issues[0].found, "\u{201c}");
        assert_eq!(issues[1].found, "\u{2018}");
        assert_eq!(issues[1].suggestions[..], vec!["\u{300e}"]); // 『
        assert_eq!(issues[2].found, "\u{2019}");
        assert_eq!(issues[2].suggestions[..], vec!["\u{300f}"]); // 』
        assert_eq!(issues[3].found, "\u{201d}");
    }

    #[test]
    fn cn_curly_quotes_skip_english_smart_quotes() {
        // English smart quotes should NOT be flagged: no CJK context.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("\u{201c}Hello,\u{201d} she said.").issues;
        let quote_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(
            quote_issues.len(),
            0,
            "should not flag English smart quotes: {quote_issues:?}"
        );
    }

    #[test]
    fn cn_curly_single_quote_skip_english_apostrophe() {
        // U+2019 is the standard typographic apostrophe in English. "it's",
        // "don't" must NOT be flagged.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("It\u{2019}s a test, don\u{2019}t worry.").issues;
        let quote_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2018}" || i.found == "\u{2019}")
            .collect();
        assert_eq!(
            quote_issues.len(),
            0,
            "should not flag English apostrophes: {quote_issues:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_fire_with_cjk_context() {
        // CN curly quotes adjacent to CJK text should still be flagged.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{201c}你好\u{201d}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "\u{201c}");
        assert_eq!(issues[1].found, "\u{201d}");
    }

    #[test]
    fn cn_curly_quotes_skip_english_quotation_in_cjk() {
        // The pair encloses no Chinese, so neither half converts even though
        // the sentence around it is Chinese. Issue #132.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他引用了原句：\u{201c}Do one thing, and do it well.\u{201d}，這是 Unix 哲學。";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert!(
            quotes.is_empty(),
            "English quotation should be left alone: {quotes:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_never_half_converted() {
        // Sentence-final quotation: only the opening mark has CJK beside it.
        // Judging each mark alone converted that half and left 「...\u{201d}.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他引用了原句：\u{201c}Do one thing, and do it well.\u{201d}";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert!(
            quotes.is_empty(),
            "a pair converts on both halves or on neither: {quotes:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_chinese_quotation_still_converted() {
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他引用了原句：\u{201c}讀書百遍，其義自見\u{201d}，這是古訓。";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(quotes[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_quotes_mixed_span_converted() {
        // A span mixing Latin and Chinese is a Chinese quotation.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{201c}Unix 哲學\u{201d}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2);
    }

    #[test]
    fn cn_curly_quotes_english_inside_chinese_quotation() {
        // Outer pair holds Chinese and converts; the nested English pair does
        // not, so the two decisions are independent.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}原句是 \u{201c}do one thing\u{201d} 沒錯\u{201d}";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2, "only the outer pair converts: {quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(quotes[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_single_quotes_skip_english_quotation_in_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他引用了 \u{2018}do one thing\u{2019} 這句話").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2018}" || i.found == "\u{2019}")
            .collect();
        assert!(
            quotes.is_empty(),
            "English single-quoted span should be left alone: {quotes:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_unbalanced_pair_and_stray_mark() {
        // Three marks: the first two pair over a Chinese span and convert; the
        // stray third is unpaired and falls back to adjacency.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}你好\u{201d}，她說\u{201c}再見";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 3);
        assert_eq!(quotes[2].suggestions[..], vec!["\u{300c}"]);
    }

    #[test]
    fn cn_curly_quotes_pair_across_paragraphs_not_formed() {
        // Pairing is per paragraph, so an unclosed quote cannot swallow the
        // next block into its span. Were these two paired, the span would hold
        // 你好 and both would convert; unpaired, each is judged on adjacency
        // and only the first one qualifies.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}你好\n\nhe said hello\u{201d}";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 1, "{quotes:?}");
        assert_eq!(quotes[0].found, "\u{201c}");
    }

    #[test]
    fn cn_curly_quotes_ignore_cjk_inside_excluded_span() {
        // The only Chinese in the span sits in inline code, which is not prose.
        // Reading it would convert an English quotation.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{201c}English `中文` text\u{201d}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert!(quotes.is_empty(), "{quotes:?}");
    }

    #[test]
    fn cn_curly_quotes_read_prose_around_an_excluded_span() {
        // Same shape, but Chinese also stands outside the code span, so the
        // pair converts. Pins that the exclusion skip does not swallow the gap.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("他說\u{201c}English `中文` 真的 text\u{201d}")
            .issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
    }

    #[test]
    fn cn_curly_quotes_convert_when_chinese_is_deep_in_the_span() {
        // The span test reads all of the span, not a bounded prefix of it: a
        // long Latin run before the Chinese must not hide it.
        let scanner = Scanner::new(vec![], vec![]);
        // Spaced, so the term rule cannot be what converts it.
        let text = format!("他說\u{201c}{}你好\u{201d}", "a ".repeat(2500));
        let issues = scanner.scan(&text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
    }

    #[test]
    fn ascii_double_quotes_ignore_cjk_inside_excluded_span() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\"English `中文` text\"").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "inline code inside the span must not make it a Chinese quotation"
        );
    }

    #[test]
    fn cn_curly_quotes_span_test_ignores_a_nested_other_kind() {
        // A single-quote pair inside the double-quote span partitions the text
        // the span test reads. The Chinese between the single quotes still has
        // to count towards the outer pair.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{201c}He said \u{2018}你好\u{2019} ok\u{201d}").issues;
        let doubles: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(doubles.len(), 2, "{doubles:?}");
        let singles: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2018}" || i.found == "\u{2019}")
            .collect();
        assert_eq!(singles.len(), 2, "{singles:?}");
    }

    #[test]
    fn cn_curly_quotes_english_pair_inside_a_chinese_pair() {
        // Mirror image: the outer span holds Chinese and converts, the nested
        // single-quote pair holds none and does not.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("他說\u{201c}原句是 \u{2018} do it \u{2019} 沒錯\u{201d}")
            .issues;
        let doubles: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(doubles.len(), 2, "{doubles:?}");
        assert!(
            !issues
                .iter()
                .any(|i| i.found == "\u{2018}" || i.found == "\u{2019}"),
            "the nested English pair should be left alone"
        );
    }

    #[test]
    fn cn_curly_quotes_direction_is_decided_per_paragraph() {
        // The first paragraph spells both halves, the second writes two
        // openers. Deciding direction once for the document let the first vouch
        // for the second, and the fix came out as 「再見『.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}你好\u{201d}\n\n她說\u{201c}再見\u{201c}";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 4, "{quotes:?}");
        assert_eq!(quotes[2].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(quotes[3].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_quotes_span_test_takes_any_cjk_context_character() {
        // The span test asks for a CJK context character, not a Han ideograph,
        // so a full-width Latin letter counts. Pinned because it is the visible
        // edge of that choice.
        let scanner = Scanner::new(vec![], vec![]);
        let wide = scanner.scan("他說\u{201c}\u{FF21}\u{201d}").issues;
        assert_eq!(
            wide.iter()
                .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
                .count(),
            2,
            "a full-width letter is CJK context: {wide:?}"
        );

        // Half-width Latin is not CJK context. Spelled as two words so the term
        // rule does not convert it for the other reason.
        let narrow = scanner.scan("他說\u{201c}A B\u{201d}").issues;
        assert!(
            !narrow
                .iter()
                .any(|i| i.found == "\u{201c}" || i.found == "\u{201d}"),
            "a half-width letter is not: {narrow:?}"
        );
    }

    #[test]
    fn cn_curly_single_quotes_without_direction_stay_unpaired() {
        // Two U+2018 in one paragraph carry no direction, and single quotes get
        // no positional fallback because U+2019 is also the apostrophe. Each
        // mark falls back to adjacency, so only the one with CJK beside it
        // converts.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{2018} hello \u{2018}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2018}" || i.found == "\u{2019}")
            .collect();
        assert_eq!(quotes.len(), 1, "{quotes:?}");
        assert_eq!(quotes[0].offset, 6);
    }

    #[test]
    fn ascii_double_quotes_direction_is_decided_per_paragraph() {
        // ASCII quotes are always positional, so the second paragraph's pair
        // has to restart at an opener rather than continue the first's parity.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\"你好\"\n\n她說\"再見\"").issues;
        let quotes: Vec<_> = issues.iter().filter(|i| i.found == "\"").collect();
        assert_eq!(quotes.len(), 4, "{quotes:?}");
        assert_eq!(quotes[2].suggestions[..], vec!["「"]);
        assert_eq!(quotes[3].suggestions[..], vec!["」"]);
    }

    #[test]
    fn cn_curly_quotes_lone_closer_keeps_its_own_direction() {
        // One U+201D on its own carries no usable direction, but positional
        // alternation takes marks two at a time and this one has no partner.
        // Relabelling it as an opener suggested 「 for a closing quote.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說你好\u{201d}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 1, "{quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_quotes_lone_closer_keeps_direction_beside_a_pair() {
        // Same leftover, but with a well formed paragraph ahead of it so that
        // fix_quote_pairing runs instead of returning early on a single issue.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}你好\u{201d}\n\n結束\u{201d}";
        let issues = scanner.scan(text).issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 3, "{quotes:?}");
        assert_eq!(quotes[2].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_quotes_standalone_chinese_quotation_converts() {
        // Nothing stands outside the pair, so no foreign prose competes for the
        // marks. Requiring CJK outside left a pull-quote unconverted.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("\u{201c}這是一段獨立引文\u{201d}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
            .collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["\u{300c}"]);
        assert_eq!(quotes[1].suggestions[..], vec!["\u{300d}"]);
    }

    #[test]
    fn cn_curly_quotes_heading_that_is_a_quotation_converts() {
        // Same shape behind Markdown structure: the marker is not prose.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("# \u{201c}敏捷開發\u{201d}").issues;
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
                .count(),
            2,
            "{issues:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_blockquote_that_is_a_quotation_converts() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("> \u{201c}軟體工程是一門學問\u{201d}").issues;
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.found == "\u{201c}" || i.found == "\u{201d}")
                .count(),
            2,
            "{issues:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_latin_prose_outside_keeps_the_marks() {
        // The mirror of the three above: Latin prose outside the pair claims
        // the marks even though the pair holds Chinese.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("Tom said \u{201c}你好\u{201d} once.").issues;
        assert!(
            !issues
                .iter()
                .any(|i| i.found == "\u{201c}" || i.found == "\u{201d}"),
            "{issues:?}"
        );
    }

    #[test]
    fn cn_curly_single_quotes_contraction_inside_a_pair_stays_whole() {
        // The opening mark is an apostrophe by the ASCII-letter test, so it is
        // passed over. Converting the surviving closer on its own adjacency
        // left 他說‘it’s 好』.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{2018}it\u{2019}s 好\u{2019}").issues;
        assert!(
            !issues
                .iter()
                .any(|i| i.found == "\u{2018}" || i.found == "\u{2019}"),
            "{issues:?}"
        );
    }

    #[test]
    fn cn_curly_single_quotes_pair_around_an_inner_contraction() {
        // Same shape with the marks clear of any letter: the pair forms, the
        // contraction inside it is passed over, and the pair still converts.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{2018}我 don\u{2019}t 知道\u{2019}").issues;
        let quotes: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2018}" || i.found == "\u{2019}")
            .collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["\u{300e}"]);
        assert_eq!(quotes[1].suggestions[..], vec!["\u{300f}"]);
    }

    #[test]
    fn ascii_double_quotes_excluded_partner_stays_whole() {
        // The URL pattern swallows the closing mark, so only the opener is
        // visible. Converting it alone left 他說「https://example.com".
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\"https://example.com\"").issues;
        assert!(
            !issues.iter().any(|i| i.found == "\""),
            "{issues:?}"
        );
    }

    #[test]
    fn cn_curly_quotes_excluded_partner_stays_whole() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("他說\u{201c}https://example.com\u{201d}")
            .issues;
        assert!(
            !issues
                .iter()
                .any(|i| i.found == "\u{201c}" || i.found == "\u{201d}"),
            "{issues:?}"
        );
    }

    #[test]
    fn ascii_double_quotes_pair_across_an_excluded_pair() {
        // Two marks passed over inside inline code, and the prose pair around
        // them still converts: passing a mark over blocks only the leftover
        // fallback, not pairing.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner
            .scan("他說\"你好，程式碼是 `printf(\"a\")`\"")
            .issues;
        let quotes: Vec<_> = issues.iter().filter(|i| i.found == "\"").collect();
        assert_eq!(quotes.len(), 2, "{quotes:?}");
        assert_eq!(quotes[0].suggestions[..], vec!["「"]);
        assert_eq!(quotes[1].suggestions[..], vec!["」"]);
    }

    #[test]
    fn cn_curly_single_quote_skip_possessive_near_cjk() {
        // "Python's 語法": the 's is an English possessive, NOT a CN quote,
        // even though CJK text is nearby. The ASCII letter guard must fire.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("Python\u{2019}s 語法").issues;
        let quote_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2019}")
            .collect();
        assert_eq!(
            quote_issues.len(),
            0,
            "should not flag English possessive 's near CJK: {quote_issues:?}"
        );
    }

    #[test]
    fn cn_curly_single_quote_skip_contraction_near_cjk() {
        // "中文 don't worry": contraction near CJK must not be flagged.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("中文 don\u{2019}t worry").issues;
        let quote_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.found == "\u{2019}")
            .collect();
        assert_eq!(
            quote_issues.len(),
            0,
            "should not flag contraction near CJK: {quote_issues:?}"
        );
    }

    #[test]
    fn cn_curly_single_quotes_fire_when_wrapping_cjk() {
        // '\u{2018}CJK\u{2019}' with NO ASCII letters adjacent: should fire.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他說\u{2018}你好\u{2019}").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "\u{2018}");
        assert_eq!(issues[1].found, "\u{2019}");
    }

    #[test]
    fn char_based_ok_respects_paragraph_breaks() {
        // Paragraph 1 has an unmatched open quote. Paragraph 2 has a valid
        // pair. The char_based_ok trial should reset at \n\n so paragraph 2
        // uses character-based mode (not positional fallback).
        let scanner = Scanner::new(vec![], vec![]);
        let text = "他說\u{201c}你好\n\n她說\u{201c}再見\u{201d}";
        let issues = scanner.scan(text).issues;
        // Find para2 quotes (after the \n\n boundary).
        let para2: Vec<_> = issues
            .iter()
            .filter(|i| {
                (i.found == "\u{201c}" || i.found == "\u{201d}")
                    && i.offset > text.find("\n\n").unwrap()
            })
            .collect();
        assert!(para2.len() >= 2, "should have 2 quotes in para2");
        // Both should be primary level (depth 0 after paragraph reset).
        assert_eq!(para2[0].suggestions[0], "\u{300c}"); // 「
        assert_eq!(para2[1].suggestions[0], "\u{300d}"); // 」
    }

    // Range indicator normalization

    #[test]
    fn tilde_range_flagged_in_cjk_context() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("第一~第五").issues;
        let tilde: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(!tilde.is_empty(), "~ between CJK should be flagged");
        assert_eq!(tilde[0].suggestions[..], vec!["～"]); // Base profile → wave dash
    }

    #[test]
    fn tilde_range_not_flagged_pure_ascii() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("foo~bar").issues;
        assert!(
            !issues.iter().any(|i| i.found == "~"),
            "~ between ASCII should not be flagged"
        );
    }

    #[test]
    fn tilde_range_relaxed_uses_en_dash() {
        let scanner = Scanner::new(vec![], vec![]);
        let cfg = Profile::Base.config().with_relaxed();
        let issues = scanner
            .scan_for_content_type_with_config("第一~第五", ContentType::Plain, cfg)
            .issues;
        let tilde: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(!tilde.is_empty(), "~ should still be flagged with relaxed");
        assert_eq!(tilde[0].suggestions[..], vec!["\u{2013}"]); // en dash
    }

    #[test]
    fn hyphen_range_flagged_between_cjk() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("甲-乙").issues;
        let h: Vec<_> = issues.iter().filter(|i| i.found == "-").collect();
        assert!(!h.is_empty(), "- between CJK characters should be flagged");
    }

    #[test]
    fn hyphen_range_not_flagged_digits() {
        let scanner = Scanner::new(vec![], vec![]);
        // Pure digit range like "1-5" should not be flagged (no CJK).
        let issues = scanner.scan("1-5").issues;
        assert!(
            !issues.iter().any(|i| i.found == "-"),
            "- between digits should not be flagged"
        );
    }

    #[test]
    fn hyphen_range_not_flagged_in_url() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("請看 https://my-site.com 的頁面").issues;
        assert!(
            !issues.iter().any(|i| i.found == "-"),
            "- in URL should not be flagged"
        );
    }

    // Character variant normalization

    fn variant_rule(from: &str, to: &str) -> SpellingRule {
        SpellingRule::new(from, vec![to.into()], RuleType::Variant)
    }

    fn variant_rule_with_exceptions(from: &str, to: &str, exceptions: Vec<&str>) -> SpellingRule {
        SpellingRule {
            exceptions: Some(exceptions.into_iter().map(String::from).collect()),
            ..SpellingRule::new(from, vec![to.into()], RuleType::Variant)
        }
    }

    #[test]
    fn variant_fires_under_strict() {
        let rules = vec![variant_rule("裏", "裡")];
        let scanner = Scanner::new(rules, vec![]);
        // Traditional Chinese context (東西 has traditional-exclusive chars).
        let text = "在裏面的東西都是好的";
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "裏");
        assert_eq!(issues[0].suggestions[..], vec!["裡"]);
        assert_eq!(issues[0].rule_type, IssueType::Variant);
        assert_eq!(issues[0].severity, Severity::Warning);
    }

    #[test]
    fn variant_silent_under_default() {
        let rules = vec![variant_rule("裏", "裡")];
        let scanner = Scanner::new(rules, vec![]);
        let text = "在裏面的東西都是好的";
        let issues = scanner.scan(text).issues; // Base profile
        assert!(
            !issues.iter().any(|i| i.rule_type == IssueType::Variant),
            "variant rules should not fire under Base profile"
        );
    }

    #[test]
    fn variant_silent_under_relaxed() {
        let rules = vec![variant_rule("裏", "裡")];
        let scanner = Scanner::new(rules, vec![]);
        let text = "在裏面的東西都是好的";
        let cfg = Profile::Base.config().with_relaxed();
        let issues = scanner
            .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
            .issues;
        assert!(
            !issues.iter().any(|i| i.rule_type == IssueType::Variant),
            "variant rules should not fire with relaxed capability"
        );
    }

    #[test]
    fn variant_multiple_chars() {
        let rules = vec![
            variant_rule("裏", "裡"),
            variant_rule("麪", "麵"),
            variant_rule("着", "著"),
        ];
        let scanner = Scanner::new(rules, vec![]);
        // Traditional context: 這, 條, 觀 are traditional-exclusive.
        let text = "這裏的麪條很好吃，觀眾看着他們";
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        assert_eq!(issues.len(), 3, "should flag 裏, 麪, 着");
        assert_eq!(issues[0].found, "裏");
        assert_eq!(issues[1].found, "麪");
        assert_eq!(issues[2].found, "着");
    }

    #[test]
    fn variant_exception_skips_phrase() {
        // Rule: 着→著, but skip when inside "下着棋" (chess context). Include
        // enough traditional-exclusive chars (國際學術) so text is detected as
        // Traditional Chinese (variant rules skip Simplified).
        let rules = vec![variant_rule_with_exceptions("着", "著", vec!["下着棋"])];
        let scanner = Scanner::new(rules, vec![]);
        let text = "國際學術比賽中他下着棋，觀眾看着他";
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        // Only the second 着 (看着他) should be flagged.
        assert_eq!(
            issues.len(),
            1,
            "should only flag 着 outside exception phrase"
        );
        assert!(
            text[issues[0].offset..].starts_with("着他"),
            "should flag the 着 in 看着他, not the one in 下着棋"
        );
    }

    // 臺/台 phrase rules

    #[test]
    fn tai_phrase_fires_under_strict() {
        let rules = vec![variant_rule("台灣", "臺灣"), variant_rule("台北", "臺北")];
        let scanner = Scanner::new(rules, vec![]);
        let text = "我住在台灣的台北市";
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "台灣");
        assert_eq!(issues[0].suggestions[..], vec!["臺灣"]);
        assert_eq!(issues[1].found, "台北");
        assert_eq!(issues[1].suggestions[..], vec!["臺北"]);
    }

    #[test]
    fn tai_phrase_silent_under_default() {
        let rules = vec![variant_rule("台灣", "臺灣")];
        let scanner = Scanner::new(rules, vec![]);
        let text = "我住在台灣";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.is_empty(),
            "台→臺 should not fire under Base profile"
        );
    }

    #[test]
    fn tai_exception_pingtai_not_flagged() {
        // Phrase-level matching: only "台灣", "台北" etc. are patterns. "平台"
        // does not match any variant rule, so it's never flagged.
        let rules = vec![variant_rule("台灣", "臺灣"), variant_rule("台北", "臺北")];
        let scanner = Scanner::new(rules, vec![]);
        let text = "這個平台很好用，台灣也有很多使用者";
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        assert_eq!(issues.len(), 1, "should only flag 台灣, not 平台");
        assert_eq!(issues[0].found, "台灣");
    }

    #[test]
    fn variant_skipped_for_simplified() {
        let rules = vec![variant_rule("裏", "裡")];
        let scanner = Scanner::new(rules, vec![]);
        // Simplified Chinese text: variant rules should be skipped.
        let text = "这裏面的东西都是好的"; // Simplified context
        let issues = scanner.scan_profiled(text, Profile::Strict).issues;
        assert!(
            issues.is_empty(),
            "variant rule should not fire on Simplified Chinese text"
        );
    }

    // Profile processing chain tests

    #[test]
    fn profile_strict_catches_variants_base_does_not() {
        let rules = vec![
            SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
            variant_rule("裏", "裡"),
        ];
        let scanner = Scanner::new(rules, vec![]);
        let text = "軟件裏面有錯誤";

        let default_issues = scanner.scan_profiled(text, Profile::Base).issues;
        let strict_issues = scanner.scan_profiled(text, Profile::Strict).issues;

        // Default catches spelling but not variants.
        assert_eq!(default_issues.len(), 1);
        assert_eq!(default_issues[0].found, "軟件");

        // Strict catches both spelling and variants.
        assert_eq!(strict_issues.len(), 2);
    }

    #[test]
    fn relaxed_skips_dunhao_and_colon() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let text = "類型:蘋果，香蕉，橘子，芒果";

        let base_issues = scanner.scan_profiled(text, Profile::Base).issues;
        let relaxed_cfg = Profile::Base.config().with_relaxed();
        let relaxed_issues = scanner
            .scan_for_content_type_with_config(text, ContentType::Plain, relaxed_cfg)
            .issues;

        // Base flags the colon and dunhao commas.
        let colon_base = base_issues.iter().any(|i| i.found == ":");
        let dunhao_base = base_issues.iter().any(|i| i.found == "\u{FF0C}");
        assert!(colon_base, "Base should flag colon");
        assert!(dunhao_base, "Base should flag dunhao");

        // Relaxed skips colon and dunhao.
        let colon_relaxed = relaxed_issues.iter().any(|i| i.found == ":");
        let dunhao_relaxed = relaxed_issues.iter().any(|i| i.found == "\u{FF0C}");
        assert!(!colon_relaxed, "relaxed should not flag colon");
        assert!(!dunhao_relaxed, "relaxed should not flag dunhao");
    }

    #[test]
    fn relaxed_range_en_dash() {
        let scanner = Scanner::new(vec![], vec![]);
        let text = "第一~第五";
        let cfg = Profile::Base.config().with_relaxed();
        let issues = scanner
            .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
            .issues;
        let tilde: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(!tilde.is_empty());
        assert_eq!(tilde[0].suggestions[..], vec!["–"]); // en dash
    }

    #[test]
    fn profile_base_range_wave_dash() {
        let scanner = Scanner::new(vec![], vec![]);
        let text = "第一~第五";
        let issues = scanner.scan_profiled(text, Profile::Base).issues;
        let tilde: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(!tilde.is_empty());
        assert_eq!(tilde[0].suggestions[..], vec!["～"]); // wave dash
    }

    #[test]
    fn profile_config_consistency() {
        // Verify that config() returns sensible values for each profile.
        let default_cfg = Profile::Base.config();
        assert!(default_cfg.spelling);
        assert!(default_cfg.basic_punctuation);
        assert!(default_cfg.colon_enforcement);
        assert!(!default_cfg.variant_normalization);
        assert!(!default_cfg.range_en_dash);

        let strict_cfg = Profile::Strict.config();
        assert!(strict_cfg.spelling);
        assert!(strict_cfg.variant_normalization);
        assert!(strict_cfg.colon_enforcement);
        assert!(!strict_cfg.range_en_dash);

        let relaxed_cfg = Profile::Base.config().with_relaxed();
        assert!(relaxed_cfg.spelling);
        assert!(!relaxed_cfg.colon_enforcement);
        assert!(!relaxed_cfg.dunhao_detection);
        assert!(!relaxed_cfg.variant_normalization);
        assert!(relaxed_cfg.range_en_dash);
        assert!(!relaxed_cfg.grammar_checks);
    }

    // ellipsis normalization tests

    #[test]
    fn ellipsis_circle_periods() {
        let scanner = empty_scanner();
        let issues = scanner.scan("等一下。。。再說").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "。。。");
        assert_eq!(issues[0].suggestions[..], vec!["……"]);
    }

    #[test]
    fn ellipsis_single_u2026() {
        let scanner = empty_scanner();
        let issues = scanner.scan("等一下…再說").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "\u{2026}");
        assert_eq!(issues[0].suggestions[..], vec!["……"]);
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn ellipsis_correct_double_u2026() {
        let scanner = empty_scanner();
        // Two consecutive … is the correct MoE form: no issue.
        assert!(scanner.scan("等一下……再說").issues.is_empty());
    }

    #[test]
    fn ellipsis_ascii_dots_no_cjk() {
        let scanner = empty_scanner();
        // ASCII dots without adjacent CJK: no issue (could be English).
        assert!(scanner.scan("wait...").issues.is_empty());
    }

    #[test]
    fn ellipsis_ascii_dots_in_code() {
        let scanner = empty_scanner();
        // Inside backtick code: excluded.
        assert!(scanner.scan("看 `...` 的說明").issues.is_empty());
    }

    #[test]
    fn ellipsis_math_notation_not_flagged() {
        // f(x) = ... is math notation; should not be flagged.
        let scanner = empty_scanner();
        assert!(
            scanner.scan("函數 f(x) = ... 的定義").issues.is_empty(),
            "math notation `= ...` should not fire ellipsis rule"
        );
    }

    #[test]
    fn ellipsis_code_comment_not_flagged() {
        // // comment ... is a code comment; should not be flagged.
        let scanner = empty_scanner();
        assert!(
            scanner.scan("// 其他程式碼 ...").issues.is_empty(),
            "// comment `...` should not fire ellipsis rule"
        );
        assert!(
            scanner.scan("  /* TODO: 補充 ... */").issues.is_empty()
                || scanner
                    .scan("  /* TODO: 補充 ... */")
                    .issues
                    .iter()
                    .all(|i| i.found != "..."),
            "/* ... */ comment should not fire ellipsis rule"
        );
    }

    #[test]
    fn ellipsis_prose_still_flagged() {
        // Plain prose ellipsis ... adjacent to CJK should still fire.
        let scanner = empty_scanner();
        let issues = scanner.scan("詳見第三章...").issues;
        assert!(
            issues.iter().any(|i| i.found == "..."),
            "prose `...` after CJK should still be flagged"
        );
    }

    #[test]
    fn toc_dot_leader_not_flagged() {
        // 第一章........1: dot leader before page number; must not fire
        // ellipsis rule.
        let scanner = empty_scanner();
        assert!(
            scanner.scan("第一章........1").issues.is_empty(),
            "TOC dot leader must not fire ellipsis rule"
        );
        assert!(
            scanner.scan("緒論..............3").issues.is_empty(),
            "long dot leader must not fire ellipsis rule"
        );
    }

    #[test]
    fn tilde_unary_approximation_not_flagged() {
        // 約 ~10 分鐘: unary ~N is an approximation prefix, not a range.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("約 ~10 分鐘可完成。").issues;
        let tildes: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(
            tildes.is_empty(),
            "unary `~N` approximation must not be flagged: {tildes:?}"
        );
    }

    #[test]
    fn tilde_range_still_flagged() {
        // 一~十: both sides CJK → genuine range indicator, must still fire.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("範圍一~十。").issues;
        let tildes: Vec<_> = issues.iter().filter(|i| i.found == "~").collect();
        assert!(
            !tildes.is_empty(),
            "CJK~CJK range `一~十` should still be flagged"
        );
    }

    #[test]
    fn punct_issue_length_matches_found() {
        // Verify that punct_issue uses found.len(), not a hardcoded 1.
        let issue = punct_issue(0, ",", "，", "test");
        assert_eq!(issue.length, 1); // ASCII comma: 1 byte

        // Multi-byte character: full-width semicolon is 3 bytes in UTF-8.
        let issue = punct_issue(0, "；", "；", "test");
        assert_eq!(issue.length, "；".len());
        assert_eq!(issue.length, 3);
    }

    // 12.1 Stack-based quote hierarchy validation

    #[test]
    fn quote_hierarchy_balanced_primary() {
        // Properly balanced 「...」: no issues.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說「你好」再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_deref()
                    .is_some_and(|c| c.contains("引號") || c.contains("未關閉"))
            })
            .collect();
        assert!(
            hierarchy.is_empty(),
            "balanced primary quotes should produce no hierarchy issues"
        );
    }

    #[test]
    fn quote_hierarchy_balanced_nested() {
        // Properly nested 「...『...』...」: no issues.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說「她說『你好』了」再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_deref()
                    .is_some_and(|c| c.contains("引號") || c.contains("未關閉"))
            })
            .collect();
        assert!(
            hierarchy.is_empty(),
            "balanced nested quotes should produce no hierarchy issues"
        );
    }

    #[test]
    fn quote_hierarchy_unbalanced_unclosed_primary() {
        // Unclosed 「 at paragraph end.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說「你好再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("未關閉")))
            .collect();
        assert_eq!(hierarchy.len(), 1, "unclosed 「 should be flagged");
        assert_eq!(hierarchy[0].found, "「");
    }

    #[test]
    fn quote_hierarchy_unbalanced_extra_close() {
        // Extra 」 without matching 「.
        let scanner = empty_scanner();
        let issues = scanner.scan("你好」再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("多餘")))
            .collect();
        assert_eq!(hierarchy.len(), 1, "extra 」 should be flagged");
        assert_eq!(hierarchy[0].found, "」");
    }

    #[test]
    fn quote_hierarchy_interleaved() {
        // Interleaved: 「...『...」...』, both mismatches flagged.
        let scanner = empty_scanner();
        let issues = scanner.scan("他「她『你好」世界』再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("交錯")))
            .collect();
        assert!(
            !hierarchy.is_empty(),
            "interleaved quotes should be flagged"
        );
    }

    #[test]
    fn quote_hierarchy_secondary_at_top_level() {
        // 『...』 not inside 「...」: secondary without primary.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說『你好』再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("最外層")))
            .collect();
        assert_eq!(
            hierarchy.len(),
            1,
            "secondary quotes at top level should be flagged"
        );
    }

    #[test]
    fn quote_hierarchy_book_title_balanced() {
        // 《...》 inside 「...」 with prose: valid.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說「我讀了《哈利波特》」再見").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_deref()
                    .is_some_and(|c| c.contains("書名號") || c.contains("未關閉"))
            })
            .collect();
        assert!(
            hierarchy.is_empty(),
            "balanced book title marks should produce no hierarchy issues"
        );
    }

    #[test]
    fn quote_hierarchy_book_title_url_balanced() {
        // 《[title](url)》: with the old \S+ URL regex the 》 was swallowed
        // into the excluded zone, causing a spurious "unclosed 《" diagnostic.
        // The fix is RE_URL using [^\s「」『』《》]+, which stops before 》 so
        // the quote checker sees a balanced pair.
        let scanner = empty_scanner();
        let issues = scanner
            .scan_profiled_md(
                "依據《[重編國語辭典](https://dict.revised.moe.edu.tw/)》，本文採用台灣標準。",
                Profile::Base,
                true,
            )
            .issues;
        let book_errs: Vec<_> = issues
            .iter()
            .filter(|i| {
                matches!(&i.context, Some(ctx) if ctx.contains("書名號") || ctx.contains("未關閉"))
            })
            .collect();
        assert!(
            book_errs.is_empty(),
            "《[title](url)》 must be treated as balanced: {book_errs:?}"
        );
    }

    #[test]
    fn quote_hierarchy_book_title_unmatched() {
        // Unclosed 《.
        let scanner = empty_scanner();
        let issues = scanner.scan("他說《哈利波特").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("未關閉")))
            .collect();
        assert_eq!(hierarchy.len(), 1, "unclosed 《 should be flagged");
        assert_eq!(hierarchy[0].found, "《");
    }

    #[test]
    fn quote_hierarchy_paragraph_reset() {
        // Unclosed 「 in first paragraph should not affect second paragraph.
        let scanner = empty_scanner();
        let text = "他說「你好\n\n她說「再見」";
        let issues = scanner.scan(text).issues;
        let unclosed: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("未關閉")))
            .collect();
        // First paragraph has unclosed 「; second paragraph is balanced.
        assert_eq!(
            unclosed.len(),
            1,
            "only first paragraph's unclosed 「 flagged"
        );
    }

    #[test]
    fn quote_hierarchy_multi_depth() {
        // Triple nesting: 「...『...「...」...』...」, valid in MoE.
        let scanner = empty_scanner();
        let issues = scanner.scan("「外層『中層「內層」中層』外層」").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context.as_deref().is_some_and(|c| {
                    c.contains("引號")
                        || c.contains("未關閉")
                        || c.contains("交錯")
                        || c.contains("最外層")
                })
            })
            .collect();
        assert!(
            hierarchy.is_empty(),
            "properly nested multi-depth quotes should produce no hierarchy issues"
        );
    }

    #[test]
    fn quote_hierarchy_in_code_excluded() {
        // Quotes inside code block should be excluded from hierarchy check.
        let scanner = empty_scanner();
        let issues = scanner.scan("正常文字 `「未關閉` 結束").issues;
        let hierarchy: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_deref().is_some_and(|c| c.contains("未關閉")))
            .collect();
        assert!(
            hierarchy.is_empty(),
            "quotes in code should be excluded from hierarchy check"
        );
    }

    // Markdown false-positive guards: image syntax and bullet lists

    #[test]
    fn markdown_image_exclamation_not_flagged() {
        // ![alt](url): the ! is part of Markdown image syntax, not prose.
        let scanner = Scanner::new(vec![], vec![]);
        let md = "詳見圖例 ![示意圖](figure.png) 所示。";
        let issues = scanner.scan(md).issues;
        let excl: Vec<_> = issues.iter().filter(|i| i.found == "!").collect();
        assert!(
            excl.is_empty(),
            "! in Markdown image syntax should not be flagged: {excl:?}"
        );
    }

    #[test]
    fn markdown_bullet_dash_not_flagged() {
        // - 項目 at line start is a Markdown list bullet, not a range
        // indicator.
        let scanner = Scanner::new(vec![], vec![]);
        let md = "清單如下：\n\n- 第一項\n- 第二項\n- 第三項\n";
        let issues = scanner
            .scan_profiled_md(md, crate::rules::ruleset::Profile::Base, true)
            .issues;
        let dashes: Vec<_> = issues.iter().filter(|i| i.found == "-").collect();
        assert!(
            dashes.is_empty(),
            "line-start `-` in Markdown list should not be flagged: {dashes:?}"
        );
    }

    #[test]
    fn prose_dash_range_still_flagged() {
        // A hyphen used as a range between CJK text should still be flagged.
        let scanner = Scanner::new(vec![], vec![]);
        let text = "請參考第一節-第三節的內容。";
        let issues = scanner.scan(text).issues;
        let dashes: Vec<_> = issues.iter().filter(|i| i.found == "-").collect();
        assert!(
            !dashes.is_empty(),
            "prose `-` between CJK text should still be flagged"
        );
    }

    #[test]
    fn prose_exclamation_still_flagged() {
        // A bare ! after CJK text (not followed by [) should still fire.
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("真的嗎! 我不敢相信").issues;
        let excl: Vec<_> = issues.iter().filter(|i| i.found == "!").collect();
        assert!(
            !excl.is_empty(),
            "prose ! after CJK should still be flagged"
        );
    }

    // negative_context_clues tests

    #[test]
    fn negative_clues_veto_fires_when_clue_present() {
        // Rule fires normally without a negative clue in the window.
        let rules = vec![SpellingRule {
    negative_context_clues: Some(vec!["的".into(), "等".into()]),
    ..SpellingRule::new("項目", vec!["專案".into()], RuleType::CrossStrait)
}];
        let scanner = Scanner::new(rules.clone(), vec![]);

        // No negative clue nearby → issue is emitted.
        let issues = scanner.scan("這個項目進度超前").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag 項目 when no negative clue present"
        );
        assert_eq!(issues[0].found, "項目");

        // Negative clue "的" is adjacent → issue is suppressed.
        let issues = scanner.scan("清單的項目需要確認").issues;
        assert!(
            issues.iter().all(|i| i.found != "項目"),
            "should NOT flag 項目 when preceded by negative clue '的'"
        );
    }

    #[test]
    fn negative_clues_do_not_block_when_absent() {
        // When none of the negative clues appear, the rule fires as normal.
        let rules = vec![SpellingRule {
    negative_context_clues: Some(vec!["獨家".into()]),
    ..SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait)
}];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("這個軟件很好用").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag 軟件 when negative clue absent"
        );
    }

    // surrounding_window_bounded tests

    #[test]
    fn window_bounded_stops_at_code_block_boundary() {
        // Layout: "項目 \n\n代碼\n\n 文字" "項" = bytes 0-2, "目" = bytes 3-5 →
        // match end = 6 "\n代碼\n" spans bytes 8-21 (fenced code block) "代碼"
        // is at bytes 12-17 (inside the code block)
        //
        // The bounded window must stop at byte 8 and therefore must NOT include
        // "代碼". The unbounded window reaches bytes 8-21+ and would include
        // it.
        use crate::engine::excluded::ByteRange;
        let text = "項目 \n```\n代碼\n```\n 文字";
        let code_block = ByteRange { start: 8, end: 22 };

        let window = surrounding_window_bounded(text, 0, 6, &[code_block]);
        assert!(
            !window.contains("代碼"),
            "bounded window should not include text inside excluded code block, got: {:?}",
            window
        );

        // Confirm the unbounded window does include it (showing the bug is
        // real).
        let unbounded = surrounding_window(text, 0, 6);
        assert!(
            unbounded.contains("代碼"),
            "unbounded window does include code block content (confirming the bug exists)"
        );
    }

    #[test]
    fn window_bounded_empty_text_no_panic() {
        // surrounding_window() returned the static literal "" for empty input,
        // causing pointer-arithmetic UB in surrounding_window_bounded(). After
        // fix, surrounding_window() returns &text[0..0] (a proper subslice) so
        // pointer subtraction is always valid.
        use crate::engine::excluded::ByteRange;
        // Even with a non-empty excluded list the function must not panic.
        let result = surrounding_window_bounded("", 0, 0, &[ByteRange { start: 0, end: 0 }]);
        assert_eq!(result, "");
    }

    #[test]
    fn window_bounded_inward_snap_does_not_expand_past_excluded() {
        // If clamped_start/end fall in the middle of a multi-byte char, inward
        // snapping (ceil/floor) must not re-include the excluded region. Use a
        // 3-byte CJK char "中" at bytes 3..6, excluded as [3..6]. Match is at
        // bytes 0..3 ("台"). The right edge of the window would normally extend
        // past byte 3, but the exclusion clamps it to 3. Ceil of 3 is still 3
        // (already on boundary).
        use crate::engine::excluded::ByteRange;
        let text = "台中南";
        assert_eq!(text.len(), 9); // 3 bytes per CJK char
        let excl = ByteRange { start: 3, end: 6 }; // "中" excluded
        let window = surrounding_window_bounded(text, 0, 3, &[excl]);
        assert!(
            !window.contains("中"),
            "excluded char must not appear in bounded window"
        );
        assert!(
            !window.contains("南"),
            "text after excluded region must not bleed in"
        );
    }

// A style guide has to write the forbidden phrase down once to forbid it, and
// ❌/✅ is the convention every zh-TW writing guide we vendored uses for it.
// Without the specimen rule the linter flags the document that teaches people
// to delete the phrase. The integration side is covered by the
// native_specimen_* corpus cases; this pins the marker parsing, which is where
// the edge cases are.
#[test]
fn mention_marker_line_parsing() {
    for marked in [
        "❌ 值得注意的是",
        "- ❌ 值得注意的是",
        "* ✅ 這個做法更精確",
        "+ ✓ 通過",
        "> ❌ 值得注意的是",
        "> - ❌ 值得注意的是",
        "> > + ✗ 值得注意的是",
        "1. ❌ 值得注意的是",
        "12) ✅ 值得注意的是",
        "   - ❌ 值得注意的是",
    ] {
        assert!(
            is_mention_marker_line(marked),
            "should be a mention marker: {marked:?}"
        );
    }
    for ordinary in [
        "值得注意的是這個模組會阻塞。",
        // Mid-sentence the marker is being used, not labelling a specimen.
        "這個功能已完成 ✅ 值得注意的是它會阻塞。",
        // A bullet without a marker is just a bullet.
        "- 值得注意的是這件事",
        // The digits have to be a list number, not the start of the content.
        "2026 年的計畫 ❌ 尚未定案",
        "",
    ] {
        assert!(
            !is_mention_marker_line(ordinary),
            "should not be a mention marker: {ordinary:?}"
        );
    }
}
