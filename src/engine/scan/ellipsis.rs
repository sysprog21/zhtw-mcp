// Ellipsis normalization.
//
// Detects non-standard ellipsis patterns adjacent to CJK text and suggests the
// MoE-standard …… (two U+2026 HORIZONTAL ELLIPSIS characters).

use super::emit::Emitter;
use crate::engine::excluded::is_excluded;
use crate::rules::ruleset::Severity;

use super::{adjacent_cjk, punct_issue_sev};

/// Ellipsis normalization.
///
/// Detects non-standard ellipsis patterns adjacent to CJK text and suggests
/// the MoE-standard …… (two U+2026 HORIZONTAL ELLIPSIS characters).
///
/// Patterns matched:
///   - ASCII ... (3+ dots)
///   - Circle periods 。。。 (3+ consecutive)
///   - Single … (should be doubled)
pub(crate) fn scan_ellipsis(em: &mut Emitter<'_>) {
    let text = em.text;
    let excluded = em.excluded;
    let issues = &mut *em.issues;

    let bytes = text.as_bytes();
    let len = bytes.len();
    let suggestion = "\u{2026}\u{2026}"; // ……

    // Single pass: iterate byte-by-byte, detect all three patterns at point of
    // encounter. ASCII '.' is 1 byte; '。' (U+3002) is 3 bytes (E3 80 82); '…'
    // (U+2026) is 3 bytes (E2 80 A6).
    let mut i = 0;
    while i < len {
        let b = bytes[i];

        // Pattern 1: ASCII dots, 3+ consecutive '.'
        if b == b'.' {
            let start = i;
            while i < len && bytes[i] == b'.' {
                i += 1;
            }
            if i - start >= 3 && !is_excluded(start, i, excluded) {
                // Guard: suppress false positives in inline math / code-comment
                // contexts. Compute the portion of the current line before the
                // dots.
                let line_start = bytes[..start]
                    .iter()
                    .rposition(|&c| c == b'\n' || c == b'\r')
                    .map_or(0, |p| p + 1);
                let line_prefix = &text[line_start..start];

                // Math notation: f(x) = ..., = is the last non-space char
                // before dots.
                let math_notation = line_prefix.trim_end().ends_with('=');

                // Code comment: line starts with // or /* (after optional
                // indent).
                let trimmed_line = line_prefix.trim_start();
                let code_comment = trimmed_line.starts_with("//") || trimmed_line.starts_with("/*");

                // TOC dot leader: a run of 5+ dots followed by optional spaces
                // and a page number near the line end (e.g. 第一章........1).
                // Real prose ellipsis is always 3 or 6 dots; longer runs are
                // leaders.
                let toc_leader = i - start >= 5 && {
                    let after = text[i..].trim_start_matches(' ');
                    after.starts_with(|c: char| c.is_ascii_digit())
                };

                if !math_notation
                    && !code_comment
                    && !toc_leader
                    && (adjacent_cjk(text, start, true) || adjacent_cjk(text, i, false))
                {
                    issues.push(punct_issue_sev(
                        start,
                        &text[start..i],
                        suggestion,
                        "MoE 標準省略號為「……」（兩個 U+2026），非 ASCII 句點",
                        Severity::Warning,
                    ));
                }
            }
            continue;
        }

        // Multi-byte patterns: check 3-byte UTF-8 sequences.
        if i + 3 <= len {
            // Pattern 2: circle period '。' (E3 80 82), 3+ consecutive
            if b == 0xE3 && bytes[i + 1] == 0x80 && bytes[i + 2] == 0x82 {
                let start = i;
                while i + 3 <= len
                    && bytes[i] == 0xE3
                    && bytes[i + 1] == 0x80
                    && bytes[i + 2] == 0x82
                {
                    i += 3;
                }
                let count = (i - start) / 3;
                if count >= 3 && !is_excluded(start, i, excluded) {
                    issues.push(punct_issue_sev(
                        start,
                        &text[start..i],
                        suggestion,
                        "MoE 標準省略號為「……」（兩個 U+2026），非重複句號「。」",
                        Severity::Warning,
                    ));
                }
                continue;
            }

            // Pattern 3: horizontal ellipsis '…' (E2 80 A6), single is
            // non-standard
            if b == 0xE2 && bytes[i + 1] == 0x80 && bytes[i + 2] == 0xA6 {
                let start = i;
                let mut count = 0;
                while i + 3 <= len
                    && bytes[i] == 0xE2
                    && bytes[i + 1] == 0x80
                    && bytes[i + 2] == 0xA6
                {
                    i += 3;
                    count += 1;
                }

                // Exactly 1 is non-standard (should be 2). 2 is correct. 3+ is
                // close enough.
                if count == 1
                    && !is_excluded(start, i, excluded)
                    && (adjacent_cjk(text, start, true) || adjacent_cjk(text, i, false))
                {
                    issues.push(punct_issue_sev(
                        start,
                        "\u{2026}",
                        suggestion,
                        "MoE 標準省略號為「……」（兩個 U+2026），單獨一個「…」不完整",
                        Severity::Info,
                    ));
                }
                continue;
            }
        }

        // Not a pattern of interest: advance by one character.
        i += text[i..].chars().next().map_or(1, |c| c.len_utf8());
    }
}
