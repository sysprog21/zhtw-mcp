// Quote pairing and hierarchy validation.
//
// - Which quotation marks convert at all, decided as pairs over the raw text
// - CN→TW quote conversion with depth-based nesting
// - Structural nesting validation of CJK bracket quotes

use std::ops::Range;

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, Severity};

use super::{
    adjacent_cjk_inner, has_paragraph_break, is_cjk_context, punct_issue_sev, split_paragraphs,
};

/// The three quotation mark shapes the scanner converts.  Each pairs on its own
/// terms.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuoteKind {
    /// U+201C and U+201D.  Directional, with a positional fallback for text
    /// that writes both halves with one of the two characters.
    CurlyDouble,
    /// U+2018 and U+2019.  Directional only: U+2019 is also the English
    /// apostrophe, so a positional fallback would pair up contractions.
    CurlySingle,
    /// ASCII ".  One character for both halves, so pairing is positional.
    AsciiDouble,
}

impl QuoteKind {
    fn open(self) -> char {
        match self {
            QuoteKind::CurlyDouble => '\u{201c}',
            QuoteKind::CurlySingle => '\u{2018}',
            QuoteKind::AsciiDouble => '"',
        }
    }

    fn close(self) -> char {
        match self {
            QuoteKind::CurlyDouble => '\u{201d}',
            QuoteKind::CurlySingle => '\u{2019}',
            QuoteKind::AsciiDouble => '"',
        }
    }

    /// Whether marks that carry no usable direction still pair off by position.
    fn positional_fallback(self) -> bool {
        self != QuoteKind::CurlySingle
    }
}

/// One quotation mark in the raw text, with the direction pairing settled on.
pub(crate) struct QuoteMark {
    pub(crate) offset: usize,
    pub(crate) len: usize,
    pub(crate) opening: bool,
    /// Whether this mark is a quotation mark at all. An English apostrophe is
    /// not, and cannot convert, but it still pairs: dropping it outright left
    /// its partner looking unpaired, and that partner then converted alone.
    eligible: bool,
}

/// How the marks of one paragraph pair off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PairingMode {
    /// The opening and closing characters are trusted, and a stack matches
    /// each closer to the opener it belongs to.
    Directional,
    /// The characters carry no direction, so the marks alternate: the first
    /// opens, the second closes, and so on.
    Positional,
    /// Neither is safe, so no pair forms and every mark falls back to
    /// adjacency on its own.
    None,
}

/// Whether one paragraph's marks carry usable direction: it spells both
/// halves and never closes a quote it did not open.
///
/// Both layers ask this, over different inputs.
/// `plan_quote_conversions` asks it of the marks in the raw text,
/// to decide how to pair them; `fix_quote_pairing` asks it of the
/// issues that survived, to decide which bracket each one gets.
/// They have to reach the same verdict or the second overwrites
/// the direction the first chose, so the rule lives in one place.
///
/// A document whose first paragraph reads 他說“你好” and whose
/// second reads 她說“再見“ used to have the well formed paragraph
/// vouch for the malformed one, and the fix came out as 「再見『.
fn direction_is_usable(openings: impl IntoIterator<Item = bool>) -> bool {
    let mut depth: i32 = 0;
    let mut saw_open = false;
    let mut saw_close = false;
    for opening in openings {
        if opening {
            saw_open = true;
            depth += 1;
        } else {
            saw_close = true;
            depth -= 1;
            if depth < 0 {
                return false;
            }
        }
    }
    saw_open && saw_close
}

/// Pick how one paragraph's marks pair.
fn pairing_mode(kind: QuoteKind, marks: &[QuoteMark]) -> PairingMode {
    if kind.open() != kind.close() && direction_is_usable(marks.iter().map(|m| m.opening)) {
        PairingMode::Directional
    } else if kind.positional_fallback() {
        PairingMode::Positional
    } else {
        PairingMode::None
    }
}

/// Split one paragraph's marks into the pairs they form and the marks left
/// over.  Every mark lands in exactly one of the two.
fn pair_marks(mode: PairingMode, marks: &[QuoteMark]) -> (Vec<(usize, usize)>, Vec<usize>) {
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut unpaired: Vec<usize> = Vec::new();
    match mode {
        PairingMode::Directional => {
            let mut stack: Vec<usize> = Vec::new();
            for (i, mark) in marks.iter().enumerate() {
                if mark.opening {
                    stack.push(i);
                } else if let Some(opener) = stack.pop() {
                    pairs.push((opener, i));
                } else {
                    unpaired.push(i);
                }
            }
            unpaired.extend(stack);
        }
        PairingMode::Positional => {
            let mut i = 0;
            while i + 1 < marks.len() {
                pairs.push((i, i + 1));
                i += 2;
            }
            unpaired.extend(i..marks.len());
        }

        // Single curly quotes with no usable direction: every mark stands alone
        // rather than risk pairing an apostrophe with a quote.
        PairingMode::None => unpaired.extend(0..marks.len()),
    }
    (pairs, unpaired)
}

/// One paragraph's marks of one kind, in order, with the characters that are
/// not quotation marks at all already dropped.
fn collect_marks(
    text: &str,
    para_start: usize,
    para: &str,
    excluded: &[ByteRange],
    kind: QuoteKind,
) -> Vec<QuoteMark> {
    let (open, close) = (kind.open(), kind.close());
    let mut marks = Vec::new();
    for (rel, ch) in para.char_indices() {
        if ch != open && ch != close {
            continue;
        }
        let offset = para_start + rel;
        let len = ch.len_utf8();
        if is_excluded(offset, offset + len, excluded) {
            continue;
        }

        // An ASCII letter against the mark makes a U+2018/U+2019 an English
        // apostrophe (it's, don't, 's, Python's 語法), not a quote. It is kept
        // in the list all the same, unable to convert but able to pair:
        // dropping it made ‘Unix 哲學’ a lone closer, which then converted on
        // its own adjacency and left ‘Unix 哲學』.
        let eligible = kind != QuoteKind::CurlySingle || !ascii_letter_adjacent(text, offset, len);
        marks.push(QuoteMark {
            offset,
            len,
            opening: ch == open,
            eligible,
        });
    }
    marks
}

/// Decide which quotation marks of one kind convert, over the raw text, before
/// any single mark is judged on its own neighbours.
///
/// A quotation mark is half of a pair and the adjacency test reads only its own
/// neighbours, which fails twice. In 他引用了原句：“Do one thing.” only the
/// opener has CJK beside it, so converting each qualifying mark alone leaves
/// the quotation unbalanced; add ，這是 Unix 哲學。 after it and both halves
/// qualify, so an English quotation carrying its own typography gets rewritten.
/// Pair first and convert both halves or neither. The quoted span has to hold
/// Chinese and CJK prose has to sit elsewhere in the paragraph. The second
/// condition keeps English typography in He said “你好” then left. Same shape
/// as
/// `paren_partner_is_convertible` in punctuation.rs, for the same
/// mismatched-half failure.
///
/// An unpaired mark has no span to judge, so it keeps the adjacency behavior.
///
/// Pairing runs per paragraph, matching `fix_quote_pairing`, so one unclosed
/// quote cannot swallow the rest of the document into its span. Returns the
/// marks to convert, in ascending offset order.
pub(crate) fn plan_quote_conversions(
    text: &str,
    excluded: &[ByteRange],
    kind: QuoteKind,
) -> Vec<QuoteMark> {
    // Cheaper than splitting the paragraphs of a document that quotes nothing,
    // which is most of them. One scan when the two halves share a character.
    let (open, close) = (kind.open(), kind.close());
    if !text.contains(open) && (open == close || !text.contains(close)) {
        return Vec::new();
    }

    let mut plan = Vec::new();
    for &(para_start, para) in &split_paragraphs(text) {
        let mut marks = collect_marks(text, para_start, para, excluded, kind);
        if marks.is_empty() {
            continue;
        }

        let mode = pairing_mode(kind, &marks);
        let (pairs, unpaired) = pair_marks(mode, &marks);

        // Position settles the direction only for a mark that actually pairs. A
        // leftover has nothing to alternate against, so it keeps the direction
        // its own character carries: relabelling a lone U+201D as an opener
        // turned 他說你好\u{201d} into a suggestion of 「.
        if mode == PairingMode::Positional {
            for &(opener, closer) in &pairs {
                marks[opener].opening = true;
                marks[closer].opening = false;
            }
        }

        // A pair's span is the stretches of paragraph between the marks it
        // encloses, so one linear pass over those stretches answers every pair,
        // including the outer members of a nest. Scanning each span on its own
        // rereads the text once per nesting level.
        let mut convert = vec![false; marks.len()];
        if !pairs.is_empty() {
            let gaps = cjk_gap_prefix(text, para_start, para_start + para.len(), &marks, excluded);
            for &(opener, closer) in &pairs {
                if marks[opener].eligible
                    && marks[closer].eligible
                    && pair_is_chinese_owned(&gaps, opener, closer)
                    && (gaps.span_holds_cjk(opener, closer)
                        || quoted_term(text, &marks[opener], &marks[closer]))
                {
                    convert[opener] = true;
                    convert[closer] = true;
                }
            }
        }
        for &i in &unpaired {
            convert[i] = marks[i].eligible && unpaired_mark_converts(text, &marks[i]);
        }

        plan.extend(
            marks
                .into_iter()
                .zip(convert)
                .filter_map(|(mark, keep)| keep.then_some(mark)),
        );
    }

    plan
}

/// Whether the quotation marks belong to Chinese prose rather than to the
/// foreign prose around them.
///
/// The span answers what language the quotation is in; this answers what
/// language owns the marks. Reading only inside rewrites the English smart
/// quotes in He said “你好” then left., so Chinese outside the pair is what
/// settles it.
///
/// A paragraph with nothing outside the pair settles it the other way. A
/// heading, a blockquote or a pull-quote that is nothing but the quotation has
/// no outside prose to read, and demanding some left 「敏捷開發」 unconverted.
/// What competes for the marks is foreign prose, so the absence of a Latin
/// letter outside the pair stands in for the presence of Chinese.
fn pair_is_chinese_owned(gaps: &GapPrefix, opener: usize, closer: usize) -> bool {
    gaps.cjk_outside(opener, closer) || !gaps.latin_outside(opener, closer)
}

/// Whether the pair encloses a term rather than a quotation.
///
/// A quotation is running text and running text has spaces in it, so a span
/// with no whitespace inside it is one token: a key name, an identifier, a
/// number, a nickname.  請按“Enter”鍵 and 設定“font-size”屬性 are Chinese
/// sentences that quote one token, not English quotations carrying their own
/// typography, and 「」 is what zh-TW writes around them.  Issue #132 is about
/// leaving the second kind alone, and the space is what tells them apart
/// without guessing at a length.
///
/// Still needs Chinese beside the pair, or He pressed “Enter” then left. would
/// convert in prose that is English throughout.  That is the same adjacency
/// test an unpaired mark uses, which is what this rule was before pairing
/// existed.
fn quoted_term(text: &str, opener: &QuoteMark, closer: &QuoteMark) -> bool {
    let span = opener.offset + opener.len..closer.offset;
    text.get(span)
        .is_some_and(|s| !s.contains(char::is_whitespace))
        && (unpaired_mark_converts(text, opener) || unpaired_mark_converts(text, closer))
}

/// The pre-pairing rule, kept for a mark with no partner: convert when the
/// nearest non-whitespace character within three spaces on either side is CJK.
fn unpaired_mark_converts(text: &str, mark: &QuoteMark) -> bool {
    adjacent_cjk_inner(text, mark.offset, true, 3)
        || adjacent_cjk_inner(text, mark.offset + mark.len, false, 3)
}

/// Whether an ASCII letter sits immediately against the mark.
fn ascii_letter_adjacent(text: &str, offset: usize, len: usize) -> bool {
    let bytes = text.as_bytes();
    (offset > 0 && bytes[offset - 1].is_ascii_alphabetic())
        || (offset + len < bytes.len() && bytes[offset + len].is_ascii_alphabetic())
}

/// What one stretch of text holds, as far as the pair decision cares.
#[derive(Clone, Copy)]
struct GapContent {
    cjk: bool,
    latin: bool,
}

/// Running counts, over the gaps between the marks, of the gaps holding
/// Chinese and of the gaps holding Latin letters.
///
/// Gap `i` is the text between mark `i-1` and mark `i`, gap 0 the text before
/// the first mark, and the final entry adds the gap after the last mark. Entry
/// `i` counts the gaps strictly before gap `i`, so every question one pair asks
/// is a subtraction and the whole paragraph is one pass.
struct GapPrefix {
    cjk: Vec<u32>,
    latin: Vec<u32>,
}

impl GapPrefix {
    /// Whether the pair encloses Chinese. Its span is gaps o+1 through c plus
    /// the marks between them, and a quotation mark is never CJK.
    fn span_holds_cjk(&self, opener: usize, closer: usize) -> bool {
        self.cjk[closer + 1] > self.cjk[opener + 1]
    }

    /// Whether Chinese prose stands outside the pair: before the opening mark,
    /// or after the closing one.
    fn cjk_outside(&self, opener: usize, closer: usize) -> bool {
        moves_outside(&self.cjk, opener, closer)
    }

    /// Whether Latin prose stands outside the pair, read the same way.
    fn latin_outside(&self, opener: usize, closer: usize) -> bool {
        moves_outside(&self.latin, opener, closer)
    }
}

/// Whether one running count moves anywhere outside the pair.
fn moves_outside(prefix: &[u32], opener: usize, closer: usize) -> bool {
    prefix[opener + 1] > 0
        || prefix
            .last()
            .is_some_and(|&total| total > prefix[closer + 1])
}

/// Walk one paragraph's gaps once, counting both.
fn cjk_gap_prefix(
    text: &str,
    para_start: usize,
    para_end: usize,
    marks: &[QuoteMark],
    excluded: &[ByteRange],
) -> GapPrefix {
    let mut prefix = GapPrefix {
        cjk: Vec::with_capacity(marks.len() + 2),
        latin: Vec::with_capacity(marks.len() + 2),
    };
    let (mut cjk, mut latin) = (0u32, 0u32);
    let mut gap_start = para_start;

    // The gaps ascend and so do the exclusion ranges, so one cursor carries
    // across the whole paragraph rather than each gap searching from scratch.
    let mut next_excluded = excluded.partition_point(|r| r.end <= para_start);
    prefix.cjk.push(cjk);
    prefix.latin.push(latin);
    for mark in marks {
        let content = range_content(text, gap_start, mark.offset, excluded, &mut next_excluded);
        cjk += u32::from(content.cjk);
        latin += u32::from(content.latin);
        prefix.cjk.push(cjk);
        prefix.latin.push(latin);
        gap_start = mark.offset + mark.len;
    }
    let content = range_content(text, gap_start, para_end, excluded, &mut next_excluded);
    cjk += u32::from(content.cjk);
    latin += u32::from(content.latin);
    prefix.cjk.push(cjk);
    prefix.latin.push(latin);
    prefix
}

/// What the text in [start, end) holds that the scanner is allowed to read.
///
/// An exclusion range is inline code, a URL or a path rather than prose, so
/// nothing inside one counts: its Chinese must not turn an English quotation
/// into a Chinese one, and its Latin must not make a Chinese paragraph read as
/// English prose.
///
/// `next` is the caller's cursor into `excluded`, advanced past every range
/// this span leaves behind. Ranges are sorted and non-overlapping and spans are
/// asked for in ascending order, so it never has to walk back.
fn range_content(
    text: &str,
    start: usize,
    end: usize,
    excluded: &[ByteRange],
    next: &mut usize,
) -> GapContent {
    let mut found = GapContent {
        cjk: false,
        latin: false,
    };
    let mut pos = start;
    while pos < end {
        while *next < excluded.len() && excluded[*next].end <= pos {
            *next += 1;
        }
        let chunk_end = match excluded.get(*next) {
            // Inside an excluded range: resume past it.
            Some(range) if range.start <= pos => {
                pos = range.end.max(pos + 1);
                continue;
            }
            Some(range) => end.min(range.start),
            None => end,
        };

        // A ByteRange carries byte offsets and promises nothing about UTF-8
        // boundaries. Rounding the readable chunk inwards from both ends keeps
        // a character straddling a range edge excluded, which is how
        // is_excluded reads it, and keeps the skip from stepping back onto text
        // this loop already passed.
        let lo = text.ceil_char_boundary(pos.min(text.len()));
        let hi = text.floor_char_boundary(chunk_end.min(text.len()));
        if lo < hi {
            for ch in text[lo..hi].chars() {
                found.cjk |= is_cjk_context(ch);
                found.latin |= ch.is_ascii_alphabetic();
                if found.cjk && found.latin {
                    return found;
                }
            }
        }
        pos = chunk_end;
    }
    found
}

/// Fix CN quotation mark pairing with depth-based nesting.
///
/// CN text uses \u{201c} (opening) and \u{201d} (closing).  TW text uses
/// 「 and 」 at depth 0, 『 and 』 at depth 1, alternating for deeper nesting.
///
/// When quotes are well-formed (character-based open/close never underflows),
/// uses the Unicode character to determine direction and tracks nesting depth.
/// When misordered or all-same-char, falls back to alternating position.
///
/// Both the direction decision and the nesting depth are per paragraph, the
/// granularity `plan_quote_conversions` already pairs at, so that one missing
/// closing quote cannot flip the rest of the document.
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

    // Group by paragraph before deciding anything. Deciding direction once for
    // the whole document let a well formed paragraph vouch for a malformed one:
    // 他說“你好” followed by a blank line and 她說“再見“ read the second
    // paragraph's two openers as an opener and a nested opener, and the fix
    // came out as 「再見『. The scanner pairs per paragraph, so this has to as
    // well.
    let mut paragraphs: Vec<Range<usize>> = Vec::new();
    let mut prev_end: usize = 0;
    for (pos, &idx) in quote_indices.iter().enumerate() {
        let offset = issues[idx].offset;
        let broke = offset > prev_end && has_paragraph_break(text, prev_end, offset);
        match paragraphs.last_mut() {
            Some(last) if !broke => last.end = pos + 1,
            _ => paragraphs.push(pos..pos + 1),
        }
        prev_end = offset + issues[idx].length;
    }

    for range in &paragraphs {
        let group = &quote_indices[range.clone()];
        let char_based_ok =
            direction_is_usable(group.iter().map(|&idx| issues[idx].found == "\u{201c}"));

        // Positional alternation takes the marks two at a time, so an odd
        // trailing mark has no partner to alternate against. It keeps its own
        // character's direction, which is what plan_quote_conversions settled
        // on; forcing it to an opener rewrote a lone U+201D to 「.
        let alternating = group.len() & !1;

        let mut depth: usize = 0;
        for (pos_in_para, &issue_idx) in group.iter().enumerate() {
            let is_opening = if char_based_ok || pos_in_para >= alternating {
                issues[issue_idx].found == "\u{201c}"
            } else {
                pos_in_para.is_multiple_of(2)
            };

            let bracket = if is_opening {
                let bracket = if depth.is_multiple_of(2) {
                    "\u{300c}" // 「 (primary)
                } else {
                    "\u{300e}" // 『 (secondary)
                };
                depth += 1;
                bracket
            } else {
                depth = depth.saturating_sub(1);
                if depth.is_multiple_of(2) {
                    "\u{300d}" // 」 (primary)
                } else {
                    "\u{300f}" // 』 (secondary)
                }
            };
            issues[issue_idx].suggestions = vec![bracket.to_string()].into();
            issues[issue_idx].refresh_suggested_rewrite();
        }
    }

    // CN single curly quotes: \u{2018}/\u{2019} → 『/』 (always secondary).
    // Unlike double quotes which alternate depth, single quotes in CN text are
    // already the "inner" quote level, mapping directly to TW 『/』. No depth
    // tracking needed: fix_quote_pairing for doubles handles the primary level.
    //
    // (suggestions are already set to 『/』 by scan_cn_curly_quotes, so this is
    // a no-op unless future logic needs to adjust them.)
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
                '「' | '『' | '《' => stack.push((ch, abs_offset)),
                '」' | '』' | '》' => close_quote(ch, abs_offset, &mut stack, issues),
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

/// Match one closing mark against the open stack, reporting the three ways it
/// can be wrong: interleaved with a different pair, unmatched entirely, or a
/// secondary quote standing outside a primary one.
fn close_quote(
    ch: char,
    abs_offset: usize,
    stack: &mut Vec<(char, usize)>,
    issues: &mut Vec<Issue>,
) {
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

    let message = match stack.last() {
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
            return;
        }
        Some(_) => interleave_msg,
        None => unmatched_msg,
    };
    issues.push(punct_issue_sev(
        abs_offset,
        found_str,
        "",
        message,
        Severity::Warning,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_content_treats_a_straddling_char_as_excluded() {
        // A ByteRange that cuts a character in half is not something the
        // builders produce, but the type does not forbid it, and rounding the
        // readable chunk outwards would both read excluded text and step the
        // walk backwards onto bytes it had already skipped.
        let text = "\u{201c}\u{4e2d}\u{201d}";
        let excluded = [ByteRange { start: 4, end: 5 }];
        let mut next = 0;
        assert!(!range_content(text, 3, 6, &excluded, &mut next).cjk);
    }

    #[test]
    fn range_content_reads_prose_beside_an_excluded_range() {
        let text = "\u{201c}ab\u{4e2d}\u{201d}";
        let excluded = [ByteRange { start: 3, end: 5 }];
        let mut next = 0;
        assert!(range_content(text, 3, 8, &excluded, &mut next).cjk);
    }

    #[test]
    fn range_content_handles_an_empty_range_list() {
        let text = "\u{201c}\u{4e2d}\u{201d}";
        let mut next = 0;
        assert!(range_content(text, 3, 6, &[], &mut next).cjk);
        let mut next = 0;
        assert!(!range_content(text, 3, 3, &[], &mut next).cjk);
    }
}
