// CJK spacing rules from Chinese Copywriting Guidelines.
//
// 1. Space between CJK and half-width Latin characters
// 2. Space between CJK and digits (except °, %)
// 3. No space adjacent to full-width punctuation
// 4. No repeated full-width punctuation marks
// 5. Full-width digits → half-width

use super::emit::Emitter;
use std::iter::Peekable;
use std::str::CharIndices;

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, IssueType, Severity};

use super::{is_cjk_ideograph, punct_issue_sev};

/// Create a zero-length insertion issue (missing space) at the given boundary.
fn missing_space_issue(boundary: usize, context: &str) -> Issue {
    punct_issue_sev(boundary, "", " ", context, Severity::Info)
}

/// Create an issue for unwanted spaces that should be removed.
fn unwanted_space_issue(offset: usize, space_len: usize, context: &str) -> Issue {
    Issue::new(
        offset,
        space_len,
        " ".repeat(space_len),
        vec!["".into()],
        IssueType::Punctuation,
        Severity::Info,
    )
    .with_context(context)
}

/// True if ch is a full-width CJK punctuation mark (，。！？；：、「」
/// 『』（）【】《》〈〉——…… etc.).
fn is_fullwidth_punct(ch: char) -> bool {
    matches!(ch,
        '\u{3001}'..='\u{3003}' | // 、。〃
        '\u{3008}'..='\u{3011}' | // 〈〉《》「」『』【】
        '\u{3014}'..='\u{301B}' | // 〔〕〖〗〘〙〚〛
        '\u{FF01}' | // ！
        '\u{FF08}' | // （
        '\u{FF09}' | // ）
        '\u{FF0C}' | // ，
        '\u{FF0E}' | // ．
        '\u{FF1A}' | // ：
        '\u{FF1B}' | // ；
        '\u{FF1F}' | // ？
        '\u{2014}' | // —
        '\u{2026}'   // …
    )
}

/// True if ch is a full-width digit (０-９).
fn is_fullwidth_digit(ch: char) -> bool {
    matches!(ch, '\u{FF10}'..='\u{FF19}')
}

/// Convert a full-width digit to its half-width equivalent.
fn fullwidth_to_halfwidth_digit(ch: char) -> char {
    debug_assert!(is_fullwidth_digit(ch));
    // Safe: fullwidth digits U+FF10..U+FF19 map to U+0030..U+0039.
    char::from_u32(ch as u32 - 0xFF10 + '0' as u32).expect("U+FF10..U+FF19 maps into ASCII 0-9")
}

impl super::Scanner {
    /// Scan for CJK spacing violations (Chinese Copywriting Guidelines).
    ///
    /// Detects:
    /// - Missing space between CJK and Latin characters (rule 1)
    /// - Missing space between CJK and digits (rule 2, except °/%)
    /// - Unwanted space adjacent to full-width punctuation (rule 3)
    /// - Repeated full-width punctuation marks (rule 4)
    /// - Full-width digits that should be half-width (rule 5)
    pub(crate) fn scan_spacing(&self, em: &mut Emitter<'_>) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

        if text.is_empty() {
            return;
        }

        // Sliding window: prev/curr/next chars with byte offsets. Avoids
        // materializing the full Vec<(usize, char)>.
        let mut iter = text.char_indices().peekable();
        let mut prev: Option<(usize, char)> = None;
        // Track consecutive identical punct run length for rule 4.
        let mut same_punct_run: usize = 0;

        while let Some((offset, ch)) = iter.next() {
            let ch_len = ch.len_utf8();
            let excluded_ch = is_excluded(offset, offset + ch_len, excluded);

            // Update punct run tracking (independent of exclusion).
            if !excluded_ch && is_fullwidth_punct(ch) {
                if prev.is_some_and(|(_, pc)| pc == ch) {
                    same_punct_run += 1;
                } else {
                    same_punct_run = 0;
                }
            } else {
                same_punct_run = 0;
            }

            if !excluded_ch {
                check_char(
                    &mut iter,
                    offset,
                    ch,
                    prev,
                    same_punct_run,
                    excluded,
                    issues,
                );
            }

            // Single update point for prev, which is why the per-character
            // checks live in a function rather than a continue statement.
            prev = Some((offset, ch));
        }
    }
}

/// Apply rules 1 to 5 at one character. Rules 4 and 5 are mutually exclusive
/// with the adjacency rules, so each match returns.
fn check_char(
    iter: &mut Peekable<CharIndices<'_>>,
    offset: usize,
    ch: char,
    prev: Option<(usize, char)>,
    same_punct_run: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    // Rule 5: full-width digits should be half-width.
    if is_fullwidth_digit(ch) {
        let hw = fullwidth_to_halfwidth_digit(ch);
        issues.push(punct_issue_sev(
            offset,
            &ch.to_string(),
            &hw.to_string(),
            "數字應使用半形字元",
            Severity::Warning,
        ));
        return;
    }

    // Rule 4: repeated full-width punctuation. Paired punct (…… and ——) is
    // allowed at exactly two, so it only trips from the third onward.
    if is_fullwidth_punct(ch) && same_punct_run > 0 {
        let limit = if is_paired_punct(ch) { 2 } else { 1 };
        if same_punct_run >= limit {
            issues.push(punct_issue_sev(
                offset,
                &ch.to_string(),
                "",
                "不重複使用標點符號",
                Severity::Warning,
            ));
        }
        return;
    }

    // Rules 1 to 3 all compare against the following character.
    let Some(&(next_offset, next_ch)) = iter.peek() else {
        return;
    };
    if is_excluded(next_offset, next_offset + next_ch.len_utf8(), excluded) {
        return;
    }

    // Rule 1: CJK immediately adjacent to Latin.
    if (is_cjk_ideograph(ch) && next_ch.is_ascii_alphabetic())
        || (ch.is_ascii_alphabetic() && is_cjk_ideograph(next_ch))
    {
        issues.push(missing_space_issue(
            offset + ch.len_utf8(),
            "中英文之間需要增加空格",
        ));
    }

    // Rule 2: CJK immediately adjacent to a digit.
    if (is_cjk_ideograph(ch) && next_ch.is_ascii_digit())
        || (ch.is_ascii_digit() && is_cjk_ideograph(next_ch))
    {
        issues.push(missing_space_issue(
            offset + ch.len_utf8(),
            "中文與數字之間需要增加空格",
        ));
    }

    // Rule 3: no space on either side of full-width punctuation.
    check_space_before_punct(iter, offset, ch, prev, issues);
    check_space_after_punct(iter, next_offset, ch, next_ch, issues);
}

/// Rule 3, leading half: a space run between content and full-width punct.
fn check_space_before_punct(
    iter: &Peekable<CharIndices<'_>>,
    offset: usize,
    ch: char,
    prev: Option<(usize, char)>,
    issues: &mut Vec<Issue>,
) {
    if ch != ' ' {
        return;
    }
    let Some((_, content_ch)) = prev.filter(|&(_, pc)| pc != ' ') else {
        return;
    };
    if !is_cjk_ideograph(content_ch) && !content_ch.is_ascii_alphanumeric() {
        return;
    }
    let Some((punct_offset, punct_ch)) = next_non_space(iter.clone()) else {
        return;
    };
    if !is_fullwidth_punct(punct_ch) {
        return;
    }
    issues.push(unwanted_space_issue(
        offset,
        punct_offset - offset,
        "全形標點與其他字元之間不加空格",
    ));
}

/// Rule 3, trailing half: a space run between full-width punct and content.
fn check_space_after_punct(
    iter: &Peekable<CharIndices<'_>>,
    next_offset: usize,
    ch: char,
    next_ch: char,
    issues: &mut Vec<Issue>,
) {
    if !is_fullwidth_punct(ch) || next_ch != ' ' {
        return;
    }
    // Skip the space already peeked, then find what the run leads to.
    let mut fwd = iter.clone();
    fwd.next();
    let Some((content_offset, content_ch)) = next_non_space(fwd) else {
        return;
    };
    if !is_cjk_ideograph(content_ch) && !content_ch.is_ascii_alphanumeric() {
        return;
    }
    issues.push(unwanted_space_issue(
        next_offset,
        content_offset - next_offset,
        "全形標點與其他字元之間不加空格",
    ));
}

/// First non-space character at or after `fwd`'s current position.
fn next_non_space(fwd: Peekable<CharIndices<'_>>) -> Option<(usize, char)> {
    fwd.into_iter().find(|&(_, ch)| ch != ' ')
}

/// True for punctuation that is legitimately used in pairs (…… and ——).
fn is_paired_punct(ch: char) -> bool {
    ch == '\u{2026}' || ch == '\u{2014}'
}

#[cfg(test)]
mod tests {
    use super::super::Scanner;
    use crate::rules::ruleset::IssueType;

    fn spacing_issues(text: &str) -> Vec<(String, String)> {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan(text).issues;
        issues
            .into_iter()
            .filter(|i| {
                i.rule_type == IssueType::Punctuation
                    && i.context.as_deref().is_some_and(|c| {
                        c.contains("空格")
                            || c.contains("標點")
                            || c.contains("數字應使用")
                            || c.contains("不重複")
                    })
            })
            .map(|i| {
                (
                    i.context.as_deref().unwrap_or("").to_string(),
                    i.suggestions.first().cloned().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn cjk_latin_missing_space() {
        let issues = spacing_issues("在LeanCloud上");
        assert!(
            issues.iter().any(|(c, _)| c.contains("中英文")),
            "should flag missing space between CJK and Latin: {issues:?}"
        );
    }

    #[test]
    fn cjk_latin_has_space() {
        let issues = spacing_issues("在 LeanCloud 上");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("中英文")),
            "should not flag when space exists: {issues:?}"
        );
    }

    #[test]
    fn cjk_digit_missing_space() {
        let issues = spacing_issues("花了5000元");
        assert!(
            issues.iter().any(|(c, _)| c.contains("數字")),
            "should flag missing space between CJK and digit: {issues:?}"
        );
    }

    #[test]
    fn cjk_digit_has_space() {
        let issues = spacing_issues("花了 5000 元");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("數字")),
            "should not flag when space exists: {issues:?}"
        );
    }

    #[test]
    fn space_before_fullwidth_punct() {
        let issues = spacing_issues("iPhone ，好開心");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag space before fullwidth comma: {issues:?}"
        );
    }

    #[test]
    fn no_space_around_fullwidth_punct() {
        let issues = spacing_issues("iPhone，好開心");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should not flag correct punctuation: {issues:?}"
        );
    }

    #[test]
    fn repeated_fullwidth_punct() {
        let issues = spacing_issues("太厲害了！！");
        assert!(
            issues.iter().any(|(c, _)| c.contains("不重複")),
            "should flag repeated exclamation: {issues:?}"
        );
    }

    #[test]
    fn single_fullwidth_punct_ok() {
        let issues = spacing_issues("太厲害了！");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "should not flag single exclamation: {issues:?}"
        );
    }

    #[test]
    fn fullwidth_digit_flagged() {
        let issues = spacing_issues("只賣１０００元");
        assert!(
            issues.iter().any(|(c, _)| c.contains("數字應使用")),
            "should flag fullwidth digits: {issues:?}"
        );
    }

    #[test]
    fn mixed_exclamation_question_not_repeated() {
        // ！？ are different punctuation marks: not "repeated".
        let issues = spacing_issues("真的嗎！？");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "should not flag mixed ！？ as repeated: {issues:?}"
        );
    }

    #[test]
    fn multi_space_before_fullwidth_punct() {
        // Multiple spaces before fullwidth comma should still be flagged.
        let issues = spacing_issues("iPhone  ，好開心");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag multi-space before fullwidth comma: {issues:?}"
        );
    }

    #[test]
    fn multi_space_after_fullwidth_punct() {
        // Multiple spaces after fullwidth comma should still be flagged.
        let issues = spacing_issues("好，  開心");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag multi-space after fullwidth comma: {issues:?}"
        );
    }

    #[test]
    fn double_ellipsis_ok() {
        // …… (exactly 2) is standard zh-TW form: should NOT be flagged.
        let issues = spacing_issues("他說……算了");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "should not flag standard double ellipsis: {issues:?}"
        );
    }

    #[test]
    fn triple_ellipsis_flagged() {
        // ……… (3+) is non-standard: should be flagged.
        let issues = spacing_issues("他說………算了");
        assert!(
            issues.iter().any(|(c, _)| c.contains("不重複")),
            "should flag triple ellipsis: {issues:?}"
        );
    }

    #[test]
    fn double_em_dash_ok() {
        // —— (exactly 2) is standard zh-TW form: should NOT be flagged.
        let issues = spacing_issues("他——就是那個人");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "should not flag standard double em dash: {issues:?}"
        );
    }

    #[test]
    fn triple_em_dash_flagged() {
        // ——— (3+) is non-standard: should be flagged.
        let issues = spacing_issues("他———就是那個人");
        assert!(
            issues.iter().any(|(c, _)| c.contains("不重複")),
            "should flag triple em dash: {issues:?}"
        );
    }

    #[test]
    fn space_after_fullwidth_punct_before_latin() {
        // Per guidelines: "全形標點與其他字元之間不加空格" applies to Latin
        // too.
        let issues = spacing_issues("好， Test很好");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag space after fullwidth comma before Latin: {issues:?}"
        );
    }

    #[test]
    fn no_space_after_fullwidth_punct_before_latin() {
        let issues = spacing_issues("好，Test很好");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should not flag when no space after fullwidth punct: {issues:?}"
        );
    }

    #[test]
    fn space_after_fullwidth_punct_before_digit() {
        let issues = spacing_issues("共 3 項，其中有 2 項");

        // "，其" has no space → OK. But "， 2" would be flagged. This input has
        // no space after comma, so no flag.
        assert!(
            !issues
                .iter()
                .any(|(c, s)| c.contains("全形標點") && s.is_empty()),
            "no space after comma here: {issues:?}"
        );
        // Now with space after comma before digit:
        let issues2 = spacing_issues("共有， 2項");
        assert!(
            issues2.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag space after fullwidth comma before digit: {issues2:?}"
        );
    }

    // Edge-case stress tests for sliding-window rewrite

    #[test]
    fn space_at_text_start_before_fullwidth_punct() {
        // Leading space before fullwidth punct: prev is None, so rule 3
        // space-before-punct requires prev to be non-space content → no fire.
        let issues = spacing_issues(" ，好開心");
        assert!(
            !issues
                .iter()
                .any(|(c, s)| c.contains("全形標點") && s.is_empty()),
            "leading space before punct should not flag (no preceding content): {issues:?}"
        );
    }

    #[test]
    fn trailing_spaces_after_fullwidth_punct() {
        // Text ends with spaces after fullwidth punct: the forward scan for
        // space-after-punct should not fire because after_ch stays ' '.
        let issues = spacing_issues("好，   ");
        assert!(
            !issues
                .iter()
                .any(|(c, s)| c.contains("全形標點") && s.is_empty()),
            "trailing spaces after punct at end of text should not flag: {issues:?}"
        );
    }

    #[test]
    fn fullwidth_punct_then_space_then_fullwidth_punct() {
        // ， ？: space between two fullwidth puncts. Rule 3 space-after-punct
        // only fires if after_ch is CJK/alphanumeric, not another punct.
        let issues = spacing_issues("好， ？");
        assert!(
            !issues
                .iter()
                .any(|(c, s)| c.contains("全形標點") && s.is_empty()),
            "space between two fullwidth puncts should not flag: {issues:?}"
        );
    }

    #[test]
    fn punct_run_resets_after_non_punct() {
        // ！好！: the second ！ should not be flagged as repeated because a CJK
        // char intervenes, resetting same_punct_run.
        let issues = spacing_issues("太棒！好！");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "punct separated by content should not flag as repeated: {issues:?}"
        );
    }

    #[test]
    fn different_punct_not_repeated() {
        // ，。: different fullwidth punct chars should not trigger rule 4.
        let issues = spacing_issues("好，好。");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("不重複")),
            "different punct chars should not flag as repeated: {issues:?}"
        );
    }

    #[test]
    fn single_char_text() {
        // Single CJK character: no next char, no prev initially.
        let issues = spacing_issues("好");
        assert!(
            issues.is_empty(),
            "single char should produce no spacing issues: {issues:?}"
        );
    }

    #[test]
    fn only_spaces() {
        let issues = spacing_issues("   ");
        assert!(
            issues.is_empty(),
            "only spaces should produce no spacing issues: {issues:?}"
        );
    }

    #[test]
    fn empty_text() {
        let issues = spacing_issues("");
        assert!(
            issues.is_empty(),
            "empty text should produce no spacing issues"
        );
    }

    #[test]
    fn cjk_space_latin_space_cjk_correct() {
        // Properly spaced: CJK SPACE Latin SPACE CJK, no issues.
        let issues = spacing_issues("好 ABC 好");
        assert!(
            !issues.iter().any(|(c, _)| c.contains("中英文")),
            "properly spaced CJK-Latin-CJK should not flag: {issues:?}"
        );
    }

    #[test]
    fn fullwidth_digit_adjacent_to_cjk() {
        // Fullwidth digit next to CJK should flag rule 5 (fullwidth→halfwidth)
        // but NOT rule 2 (CJK-digit spacing), because the fullwidth digit is
        // not an ASCII digit.
        let issues = spacing_issues("有３項");
        let has_fw_digit = issues.iter().any(|(c, _)| c.contains("數字應使用"));
        assert!(has_fw_digit, "should flag fullwidth digit: {issues:?}");
    }

    #[test]
    fn rule3_space_before_punct_with_latin_content() {
        // Latin char before space before fullwidth punct.
        let issues = spacing_issues("test ，好");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag space before fullwidth punct after Latin: {issues:?}"
        );
    }

    #[test]
    fn rule3_space_before_punct_with_digit_content() {
        // Digit before space before fullwidth punct.
        let issues = spacing_issues("123 ，好");
        assert!(
            issues.iter().any(|(c, _)| c.contains("全形標點")),
            "should flag space before fullwidth punct after digit: {issues:?}"
        );
    }

    #[test]
    fn rule4_quadruple_ellipsis() {
        // 4 consecutive ellipsis marks: run=1 is OK (paired), run=2 and 3
        // flagged.
        let issues = spacing_issues("他說…………算了");
        let repeat_count = issues.iter().filter(|(c, _)| c.contains("不重複")).count();
        assert_eq!(
            repeat_count, 2,
            "4 ellipsis should flag 2 extras: {issues:?}"
        );
    }

    #[test]
    fn rule1_boundary_latin_then_cjk() {
        // Latin immediately followed by CJK.
        let issues = spacing_issues("Hello世界");
        assert!(
            issues.iter().any(|(c, _)| c.contains("中英文")),
            "should flag missing space Latin→CJK: {issues:?}"
        );
    }

    #[test]
    fn rule2_boundary_digit_then_cjk() {
        // Digit immediately followed by CJK.
        let issues = spacing_issues("42個");
        assert!(
            issues.iter().any(|(c, _)| c.contains("數字")),
            "should flag missing space digit→CJK: {issues:?}"
        );
    }
}
