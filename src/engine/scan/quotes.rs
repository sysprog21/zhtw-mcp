// Quote pairing and hierarchy validation.
//
// - CN→TW quote conversion with depth-based nesting
// - Structural nesting validation of CJK bracket quotes

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, Severity};

use super::{has_paragraph_break, punct_issue_sev, split_paragraphs};

/// Fix CN quotation mark pairing with depth-based nesting.
///
/// CN text uses \u{201c} (opening) and \u{201d} (closing).  TW text uses
/// 「 and 」 at depth 0, 『 and 』 at depth 1, alternating for deeper nesting.
///
/// When quotes are well-formed (character-based open/close never underflows),
/// uses the Unicode character to determine direction and tracks nesting depth.
/// When misordered or all-same-char, falls back to alternating position.
///
/// Paragraph breaks (double newline) reset the nesting depth to prevent one
/// missing closing quote from flipping the rest of the document.
pub(crate) fn fix_quote_pairing(text: &str, issues: &mut [Issue]) {
    let quote_indices: Vec<usize> = issues
        .iter()
        .enumerate()
        .filter(|(_, i)| i.found == "\u{201c}" || i.found == "\u{201d}")
        .map(|(idx, _)| idx)
        .collect();

    if quote_indices.len() < 2 {
        return;
    }

    // Determine whether character-based open/close detection is safe.
    // Run a trial: treat \u{201c} as open and \u{201d} as close.
    // If depth never goes negative within each paragraph, character-based
    // mode is reliable.  The trial must reset depth at paragraph breaks
    // (double newline) to match the actual assignment loop below — otherwise
    // a single missing close quote in one paragraph would force the entire
    // document into positional fallback.
    let char_based_ok = {
        let first = &issues[quote_indices[0]].found;
        let all_same = quote_indices.iter().all(|&idx| issues[idx].found == *first);
        !all_same && {
            let mut d: i32 = 0;
            let mut trial_prev_end: usize = 0;
            quote_indices.iter().all(|&idx| {
                let offset = issues[idx].offset;
                // Reset depth at paragraph breaks, matching the assignment loop.
                if offset > trial_prev_end && has_paragraph_break(text, trial_prev_end, offset) {
                    d = 0;
                }
                trial_prev_end = offset + issues[idx].length;
                if issues[idx].found == "\u{201c}" {
                    d += 1;
                    true
                } else {
                    d -= 1;
                    d >= 0
                }
            })
        }
    };

    let mut depth: usize = 0;
    let mut prev_end: usize = 0;
    let mut pos_in_para: usize = 0;

    for &issue_idx in &quote_indices {
        let offset = issues[issue_idx].offset;

        // Paragraph break: reset depth and position counter.
        if offset > prev_end && has_paragraph_break(text, prev_end, offset) {
            depth = 0;
            pos_in_para = 0;
        }

        let is_opening = if char_based_ok {
            issues[issue_idx].found == "\u{201c}"
        } else {
            pos_in_para.is_multiple_of(2)
        };

        if is_opening {
            let bracket = if depth.is_multiple_of(2) {
                "\u{300c}" // 「 (primary)
            } else {
                "\u{300e}" // 『 (secondary)
            };
            issues[issue_idx].suggestions = vec![bracket.to_string()].into();
            depth += 1;
        } else {
            depth = depth.saturating_sub(1);
            let bracket = if depth.is_multiple_of(2) {
                "\u{300d}" // 」 (primary)
            } else {
                "\u{300f}" // 』 (secondary)
            };
            issues[issue_idx].suggestions = vec![bracket.to_string()].into();
        }

        prev_end = offset + issues[issue_idx].length;
        pos_in_para += 1;
    }

    // CN single curly quotes: \u{2018}/\u{2019} → 『/』 (always secondary).
    // Unlike double quotes which alternate depth, single quotes in CN text
    // are already the "inner" quote level, mapping directly to TW 『/』.
    // No depth tracking needed — fix_quote_pairing for doubles handles the
    // primary level.
    //
    // (suggestions are already set to 『/』 by scan_cn_curly_quotes, so
    //  this is a no-op unless future logic needs to adjust them.)
}

/// Stack-based quote hierarchy validator.
///
/// Walks text (skipping exclusion zones) and validates structural nesting of
/// CJK quote marks: 「」 (primary), 『』 (secondary), 《》 (book title).
///
/// Violations detected:
///   - Mismatched close: e.g. 「...』 or 『...」
///   - Secondary without primary: 『...』 at top level (not inside 「...」)
///   - Unclosed quotes at paragraph/block boundaries
///   - Interleaved quotes: 「...『...」...』
///
/// Operates per-paragraph (split on double newline) so one block's
/// unclosed quote doesn't cascade through the entire document.
///
/// Emits IssueType::Punctuation with Severity::Warning.
pub(crate) fn validate_quote_hierarchy(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    let paragraphs = split_paragraphs(text);

    for &(para_start, para) in &paragraphs {
        let mut stack: Vec<(char, usize)> = Vec::new(); // (opening char, byte offset)

        for (rel_offset, ch) in para.char_indices() {
            let abs_offset = para_start + rel_offset;
            let ch_len = ch.len_utf8();

            if is_excluded(abs_offset, abs_offset + ch_len, excluded) {
                continue;
            }

            match ch {
                '「' | '『' | '《' => {
                    stack.push((ch, abs_offset));
                }
                '」' | '』' | '》' => {
                    let (opener, found_str, interleave_msg, unmatched_msg) = match ch {
                        '」' => (
                            '「',
                            "」",
                            "引號層級錯誤：「」與『』交錯嵌套",
                            "多餘的關閉引號「」」，找不到對應的開啟引號「「」",
                        ),
                        '』' => (
                            '『',
                            "』",
                            "引號層級錯誤：「」與『』交錯嵌套",
                            "多餘的關閉引號「』」，找不到對應的開啟引號「『」",
                        ),
                        _ => (
                            '《',
                            "》",
                            "書名號層級錯誤：《》與引號交錯嵌套",
                            "多餘的關閉書名號「》」，找不到對應的開啟書名號「《」",
                        ),
                    };
                    match stack.last() {
                        Some(&(c, _)) if c == opener => {
                            stack.pop();
                            // Secondary quotes must be enclosed in primary.
                            if ch == '』' && !stack.iter().any(|(c, _)| *c == '「') {
                                issues.push(punct_issue_sev(
                                    abs_offset,
                                    "』",
                                    "",
                                    "『』應嵌套在「」內使用，不應出現在最外層",
                                    Severity::Warning,
                                ));
                            }
                        }
                        Some(_) => {
                            issues.push(punct_issue_sev(
                                abs_offset,
                                found_str,
                                "",
                                interleave_msg,
                                Severity::Warning,
                            ));
                        }
                        None => {
                            issues.push(punct_issue_sev(
                                abs_offset,
                                found_str,
                                "",
                                unmatched_msg,
                                Severity::Warning,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        // Report unclosed quotes at paragraph boundary.
        for (ch, offset) in stack.drain(..) {
            let (found, context) = match ch {
                '「' => ("「", "段落結束時「」未關閉"),
                '『' => ("『", "段落結束時『』未關閉"),
                '《' => ("《", "段落結束時《》未關閉"),
                _ => continue,
            };
            issues.push(punct_issue_sev(
                offset,
                found,
                "",
                context,
                Severity::Warning,
            ));
        }
    }
}
