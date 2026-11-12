// Consecutive duplicate word/character detection.
//
// Catches ASR stutters and copy-paste errors:
//   - CJK: consecutive identical 1-3 char sequences (去去, 都都)
//   - Latin: duplicate words separated by whitespace (cache cache, the the)
//
// Uses manual scanning (no regex backreferences needed).

use super::super::excluded::is_excluded;
use super::emit::Emitter;
use super::is_cjk_ideograph;
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// Lexicalized reduplications common enough that an Info line is not worth
/// printing. Suppression only: since "deduplication_suggestion" stopped
/// offering a fix for short units, nothing here decides a rewrite.
///
/// Not a test for legitimacy, which the comment on that function explains no
/// lexicon can be: reduplication is productive, so this list is a sample of a
/// class it cannot close.
static REDUPLICATION_WHITELIST: &[&str] = &[
    // Family/people
    "媽媽",
    "爸爸",
    "哥哥",
    "姐姐",
    "弟弟",
    "妹妹",
    "爺爺",
    "奶奶",
    "叔叔",
    "伯伯",
    "姑姑",
    "舅舅",
    "嬸嬸",
    "婆婆",
    "公公",
    "太太", // Greetings / interjections
    "謝謝",
    "哈哈",
    "嘿嘿",
    "呵呵",
    "嗯嗯",
    "喔喔", // Adverbs / adjectives
    "常常",
    "慢慢",
    "漸漸",
    "快快",
    "好好",
    "早早",
    "偷偷",
    "悄悄",
    "剛剛",
    "僅僅",
    "淡淡",
    "默默",
    "靜靜",
    "輕輕",
    "深深",
    "滿滿",
    "多多",
    "少少",
    "大大",
    "小小",
    "明明",
    "真真",
    "最最",
    "乖乖",
    // Verb reduplication
    "看看",
    "想想",
    "試試",
    "聽聽",
    "說說",
    "走走",
    "談談",
    "問問",
    "查查",
    "算算",
    "猜猜",
    "找找",
    "玩玩",
    "等等",
    "改改",
    "講講",
    "聊聊",
    "碰碰",
    "摸摸",
    "拍拍",
    "拉拉",
    "推推",
    "搖搖",
    "動動",
    "跑跑",
    "跳跳",
    "唸唸",
    "寫寫",
    "畫畫",
    "讀讀",
    "學學",
    "做做",
    "用用",
    "吃吃",
    // Nature / objects
    "星星",
    "點點",
    "毛毛", // Pronoun / measure
    "某某",
    "一一", // Other legitimate
    "往往",
    "處處",
    "步步",
    "層層",
    "年年",
    "天天",
    "人人",
    "家家",
    "個個",
    "條條",
    "種種",
    "件件",
    "樣樣",
    "沾沾", // ABAB natural-language reduplications
    "一步一步",
    // AABC idiom prefixes (欣欣向榮, 彬彬有禮, etc.)
    "欣欣",
    "彬彬",
    "娓娓",
    "津津",
    "歷歷",
    "源源",
    "嗷嗷",
    "洋洋",
    "碌碌",
    "赫赫",
    "蒸蒸",
    "岌岌",
    "喋喋",
    "耿耿",
    "落落",
    "諄諄",
    "鼎鼎",
    "惴惴",
    "孜孜",
    "矇矇",
];

/// What to offer for a repeated unit.
///
/// A repeated one- or two-character unit is as often morphology as it is a
/// mistake: 研究研究, 討論討論, 整整一百年 and 錯字連連 are correct zh-TW, and
/// no lexical test separates them from a stutter without a dictionary.
/// Deleting the second copy is silent corruption, so short units are reported
/// without a suggestion and a human decides. Function characters are no
/// exception, which is what makes the allowlist above unfixable rather than
/// merely incomplete: 的的確確, 不了了之, 是是非非 and 在在顯示 all reduplicate
/// a particle. Three-character units are long enough that reduplication does
/// not reach them, so a copy-paste duplicate is the only reading left.
fn deduplication_suggestion(unit: &str) -> Vec<String> {
    if unit.chars().count() >= 3 {
        vec![unit.to_string()]
    } else {
        Vec::new()
    }
}

/// Consume a separator between duplicate Latin words.
///
/// Accepts horizontal spacing, or a single logical line break (`\n` or `\r\n`)
/// with optional surrounding spaces/tabs. Rejects blank-line separators.
fn consume_duplicate_separator(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    let mut saw_separator = false;

    while i < len && matches!(bytes[i], b' ' | b'\t') {
        saw_separator = true;
        i += 1;
    }

    if i < len && bytes[i] == b'\r' {
        i += 1;
        // Consume optional \n after \r (CRLF), but bare \r is also accepted.
        if i < len && bytes[i] == b'\n' {
            i += 1;
        }
        saw_separator = true;
    } else if i < len && bytes[i] == b'\n' {
        i += 1;
        saw_separator = true;
    }

    while i < len && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }

    if !saw_separator {
        return None;
    }

    if i < len && matches!(bytes[i], b'\r' | b'\n') {
        return None;
    }

    Some(i)
}

/// Scan for consecutive duplicate words/characters.
pub(crate) fn scan_repetition(em: &mut Emitter<'_>) {
    let _text = em.text;
    let _excluded = em.excluded;

    scan_cjk_duplicates(em);
    scan_latin_duplicates(em);
}

/// Detect consecutive identical CJK character sequences (1-3 chars).
fn scan_cjk_duplicates(em: &mut Emitter<'_>) {
    let text = em.text;
    let excluded = em.excluded;
    let issues = &mut *em.issues;

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        if !is_cjk_ideograph(chars[i].1) {
            i += 1;
            continue;
        }

        let mut advanced = false;
        // Try matching 1, 2, or 3 char units (prefer longest).
        for unit_len in [3usize, 2, 1] {
            if i + unit_len * 2 > n {
                continue;
            }
            // Check that all chars in the unit are CJK.
            let all_cjk = (0..unit_len).all(|k| is_cjk_ideograph(chars[i + k].1));
            if !all_cjk {
                continue;
            }
            // Check if the next unit_len chars match.
            let matches = (0..unit_len).all(|k| chars[i + k].1 == chars[i + unit_len + k].1);
            if !matches {
                continue;
            }

            let start = chars[i].0;
            let end_idx = i + unit_len * 2;
            let end = if end_idx < n {
                chars[end_idx].0
            } else {
                text.len()
            };

            // Single-char reduplication inside a longer uninterrupted CJK
            // compound is often legitimate morphology, e.g. 財政政策.
            if unit_len == 1 {
                let has_cjk_before = i > 0 && is_cjk_ideograph(chars[i - 1].1);
                let has_cjk_after = end_idx < n && is_cjk_ideograph(chars[end_idx].1);
                if has_cjk_before && has_cjk_after {
                    continue;
                }
            }

            if is_excluded(start, end, excluded) {
                i += unit_len * 2;
                advanced = true;
                break;
            }

            let matched = &text[start..end];
            if REDUPLICATION_WHITELIST.contains(&matched) {
                i += unit_len * 2;
                advanced = true;
                break;
            }

            let unit = &text[start..chars[i + unit_len].0];
            issues.push(Issue::new(
                start,
                end - start,
                matched.to_string(),
                deduplication_suggestion(unit),
                IssueType::Repetition,
                Severity::Info,
            ));
            i += unit_len * 2;
            advanced = true;
            break;
        }
        // If no unit matched, advance by 1.
        if !advanced {
            i += 1;
        }
    }
}

/// Detect consecutive duplicate Latin words (e.g. "cache cache").
fn scan_latin_duplicates(em: &mut Emitter<'_>) {
    let text = em.text;
    let excluded = em.excluded;
    let issues = &mut *em.issues;

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Skip to start of a word (ASCII alphanumeric).
        if !bytes[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }

        // Extract word1.
        let w1_start = i;
        while i < len && bytes[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let w1_end = i;
        let w1_len = w1_end - w1_start;
        if w1_len < 3 {
            continue;
        }

        // Expect spaces/tabs or a single line break between duplicate words.
        let Some(separator_end) = consume_duplicate_separator(bytes, i) else {
            continue; // missing or unsupported separator
        };
        i = separator_end;

        // Extract word2.
        let w2_start = i;
        let w2_end_max = (w2_start + w1_len).min(len);
        if w2_end_max - w2_start < w1_len {
            continue;
        }

        // Check that word2 == word1 (case-insensitive). Ensure we don't slice
        // in the middle of a multi-byte char.
        if !text.is_char_boundary(w2_end_max) {
            i = w1_end;
            continue;
        }
        let w1 = &text[w1_start..w1_end];
        let w2_candidate = &text[w2_start..w2_end_max];
        if !w1.eq_ignore_ascii_case(w2_candidate) {
            // Reset to after word1 to try again from there.
            i = w1_end;
            continue;
        }

        // Ensure word2 ends at a word boundary. Check the next Unicode scalar
        // (not just ASCII byte) so accented Latin chars like 'é' in "cacheé"
        // correctly prevent a false match on "cache".
        if w2_end_max < len {
            let rest = &text[w2_end_max..];
            if let Some(ch) = rest.chars().next() {
                if ch.is_alphanumeric() {
                    i = w1_end;
                    continue;
                }
            }
        }

        if is_excluded(w1_start, w2_end_max, excluded) {
            i = w2_end_max;
            continue;
        }

        let matched = &text[w1_start..w2_end_max];
        issues.push(Issue::new(
            w1_start,
            w2_end_max - w1_start,
            matched.to_string(),
            vec![w1.to_string()],
            IssueType::Repetition,
            Severity::Info,
        ));
        i = w2_end_max;
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/engine/scan/repetition/tests.rs"]
mod tests;
