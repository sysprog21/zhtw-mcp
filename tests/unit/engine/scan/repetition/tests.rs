use super::*;
use crate::engine::excluded::ByteRange;

fn scan(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    scan_repetition(&mut Emitter::new(text, &[], &mut issues));
    issues
}

// A short CJK unit is reported but carries no suggestion: reduplication is
// productive in Chinese, so the fixer must not delete the second copy.
#[test]
fn catches_cjk_single_char_duplicate() {
    let issues = scan("去去都知道");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "去去");
    assert!(issues[0].suggestions.is_empty());
}

#[test]
fn productive_reduplication_is_reported_without_a_fix() {
    for text in [
        "這件事我們研究研究。",
        "大家一起討論討論吧。",
        "整整一百年過去了。",
        "錯字連連。",
        "《茜茜公主》是經典。",
        "形形色色的人。",
    ] {
        let issues = scan(text);
        assert!(
            issues.iter().all(|i| i.suggestions.is_empty()),
            "offered to delete productive reduplication in {text}: {issues:?}"
        );
    }
}

#[test]
fn catches_latin_duplicate() {
    let issues = scan("the the quick brown fox");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "the the");
}

#[test]
fn catches_cache_cache() {
    let issues = scan("這個 cache cache 不錯");
    assert!(issues.iter().any(|i| i.found == "cache cache"));
}

#[test]
fn catches_duplicate_across_whitespace_variants() {
    // Tab-separated
    let issues = scan("the\tthe quick");
    assert_eq!(issues.len(), 1, "tab-separated duplicate missed");
    // Newline-separated
    let issues = scan("cache\ncache");
    assert_eq!(issues.len(), 1, "newline-separated duplicate missed");
    // Windows newline-separated
    let issues = scan("cache\r\ncache");
    assert_eq!(issues.len(), 1, "CRLF-separated duplicate missed");
    // Bare CR (legacy Mac / clipboard text)
    let issues = scan("cache\rcache");
    assert_eq!(issues.len(), 1, "bare CR-separated duplicate missed");
}

#[test]
fn skips_duplicates_split_by_blank_lines_or_control_whitespace() {
    assert!(scan("cache\n\ncache").is_empty());
    assert!(scan("cache\x0bcache").is_empty());
}

#[test]
fn skips_reduplication_whitelist() {
    for &word in REDUPLICATION_WHITELIST {
        let issues = scan(word);
        assert!(
            issues.is_empty(),
            "whitelist word '{}' should not be flagged",
            word
        );
    }
}

#[test]
fn skips_legitimate_step_by_step_reduplication() {
    assert!(scan("一步一步來").is_empty());
}

#[test]
fn skips_legitimate_text() {
    assert!(scan("謝謝你的幫忙").is_empty());
    assert!(scan("慢慢走不急").is_empty());
}

#[test]
fn skips_excluded_range() {
    let excluded = vec![ByteRange { start: 0, end: 20 }];
    let mut issues = Vec::new();
    scan_repetition(&mut Emitter::new("去去都知道", &excluded, &mut issues));
    assert!(issues.is_empty());
}

#[test]
fn does_not_flag_different_chars() {
    assert!(scan("去到那裡").is_empty());
    assert!(scan("hello world").is_empty());
}

#[test]
fn catches_adjacent_duplicate_runs() {
    let issues = scan("去去來來");
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].found, "去去");
    assert_eq!(issues[1].found, "來來");
}

#[test]
fn skips_internal_compound_double_char() {
    assert!(scan("財政政策").is_empty());
}

#[test]
fn catches_two_char_cjk_duplicate() {
    // Two-char unit: 作業作業 is NOT a reduplication whitelist entry.
    let issues = scan("作業作業完成了");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "作業作業");

    // 作業作業 reads as a duplicate here, but 研究研究 is grammar and the two
    // are indistinguishable without a dictionary, so neither is fixed.
    assert!(issues[0].suggestions.is_empty());
}

#[test]
fn catches_three_char_cjk_duplicate() {
    let issues = scan("處理器處理器效能高");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "處理器處理器");
    assert_eq!(issues[0].suggestions.as_ref(), ["處理器"]);
}

#[test]
fn latin_case_insensitive() {
    // 'Cache cache' differs in case but should still be caught.
    let issues = scan("Cache cache is fast");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "Cache cache");
}

#[test]
fn skips_short_latin_words() {
    // Words < 3 chars should not be flagged to avoid noise.
    assert!(scan("is is").is_empty());
    assert!(scan("to to").is_empty());
}

#[test]
fn latin_duplicate_at_end_of_string() {
    let issues = scan("fast cache cache");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "cache cache");
}

#[test]
fn cjk_duplicate_at_end_of_string() {
    let issues = scan("完成去去");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "去去");
}

#[test]
fn cjk_duplicate_at_string_start() {
    let issues = scan("去去了");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "去去");
}

#[test]
fn skips_boundary_compound_before() {
    // Single-char dup with CJK on both sides is legitimate morphology.
    assert!(scan("公共共識").is_empty());
}

#[test]
fn catches_boundary_compound_before_only() {
    // CJK before but NOT after: should fire.
    let issues = scan("公共共。");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "共共");
}

#[test]
fn latin_duplicate_not_partial_word() {
    // 'caching caching' is valid but 'cache caching' is not a dup.
    let issues = scan("cache caching data");
    assert!(issues.is_empty());
}

#[test]
fn latin_duplicate_not_partial_accented_word() {
    // 'cacheé' is a longer word with accented char; 'cache cache' must not
    // match.
    let issues = scan("cache cacheé data");
    assert!(
        issues.is_empty(),
        "accented suffix must prevent false match"
    );
}

#[test]
fn multiple_latin_duplicates() {
    let issues = scan("the the quick cache cache");
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].found, "the the");
    assert_eq!(issues[1].found, "cache cache");
}

#[test]
fn cjk_duplicate_mixed_with_non_cjk() {
    // Latin text between CJK should not cause false positives.
    assert!(scan("去 hello 去").is_empty());
}

// The CJK check covers Bopomofo and the extensions, not only U+4E00..U+9FFF.
#[test]
fn catches_duplicate_in_bopomofo_and_extensions() {
    assert_eq!(scan("ㄅㄅ").len(), 1);
    assert_eq!(scan("𠮷𠮷").len(), 1); // Ext B
    assert_eq!(scan("㐀㐀").len(), 1); // Ext A, U+3400
    assert_eq!(scan("ㄅㄆㄅㄆ").len(), 1); // two-char unit
}

#[test]
fn bopomofo_flanked_single_char_duplicate_is_skipped() {
    // Same compound rule as for unified ideographs, now applied to Ext A.
    assert!(scan("㐀㐁㐁㐂").is_empty());
}
