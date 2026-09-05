// Punctuation scanning: half-width to full-width detection, dunhao (enumeration
// comma), and range indicator normalization.

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, ProfileConfig, Severity};

use super::quotes::{plan_quote_conversions, QuoteKind, QuoteMark};
use super::{adjacent_cjk, immediate_cjk, punct_issue, Scanner};

impl Scanner {
    /// Punctuation scan: detect half-width punctuation that should be
    /// full-width
    /// in a CJK context.
    ///
    /// Handles: , . ! ? ; ( ) : (2.1 + 2.2).
    /// Colon enforcement is profile-dependent: relaxed allows half-width :.
    pub(crate) fn scan_punctuation(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
        cfg: &ProfileConfig,
    ) {
        let bytes = text.as_bytes();
        let len = bytes.len();

        // Straight double quotes are decided as pairs over the raw text, ahead
        // of the walk below, so that a quotation gets both halves converted or
        // neither. The plan ascends, and so does the walk, so one cursor keeps
        // the lookup at O(1) per mark.
        let ascii_quotes = plan_quote_conversions(text, excluded, QuoteKind::AsciiDouble);
        let mut ascii_cursor = 0usize;

        for (i, &b) in bytes.iter().enumerate() {
            // Cheap prefilter so the exclusion range check below runs only for
            // candidate bytes. The dispatch match further down repeats this
            // byte set; a byte added here but not there is skipped, not a
            // panic.
            match b {
                b',' | b'.' | b'!' | b'?' | b';' | b'(' | b')' | b':' | b'"' => {}
                _ => continue,
            }

            if is_excluded(i, i + 1, excluded) {
                continue;
            }

            match b {
                b',' => {
                    // Guard: digit on both sides → thousands separator (e.g.
                    // 1,000).
                    let digit_before = i > 0 && bytes[i - 1].is_ascii_digit();
                    let digit_after = i + 1 < len && bytes[i + 1].is_ascii_digit();
                    if digit_before && digit_after {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ",",
                        "\u{FF0C}",
                        "繁體中文應使用全形逗號「，」而非半形逗號「,」",
                    ));
                }
                b'.' => {
                    // Guard: adjacent period → ellipsis (.. or ...).
                    let period_before = i > 0 && bytes[i - 1] == b'.';
                    let period_after = i + 1 < len && bytes[i + 1] == b'.';
                    if period_before || period_after {
                        continue;
                    }

                    // Guard: followed by ASCII alphanumeric → decimal /
                    // extension.
                    if i + 1 < len && bytes[i + 1].is_ascii_alphanumeric() {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ".",
                        "\u{3002}",
                        "繁體中文應使用全形句號「。」而非半形句號「.」",
                    ));
                }
                b'!' | b'?' | b';' => {
                    // Guard: Markdown image syntax ![alt](url), ! followed by [
                    // is never a prose exclamation mark.
                    if b == b'!' && i + 1 < len && bytes[i + 1] == b'[' {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    let (found, suggestion, context) = match b {
                        b'!' => (
                            "!",
                            "\u{FF01}",
                            "繁體中文應使用全形驚嘆號「！」而非半形「!」",
                        ),
                        b'?' => ("?", "\u{FF1F}", "繁體中文應使用全形問號「？」而非半形「?」"),
                        _ => (";", "\u{FF1B}", "繁體中文應使用全形分號「；」而非半形「;」"),
                    };
                    issues.push(punct_issue(i, found, suggestion, context));
                }
                b'(' | b')' => {
                    // Require CJK immediately adjacent (no whitespace skip) on
                    // both sides to avoid flagging functional ASCII parens:
                    // method calls like foo(), markdown links [text](url),
                    // spaced 中文 (note).
                    if !immediate_cjk(text, i, true) || !immediate_cjk(text, i + 1, false) {
                        continue;
                    }

                    // A bracket is half of a pair, and the test above reads
                    // only its own neighbours. In 上海電影譯製廠(1957 成立)。
                    // the opener fails on the digit while the closer passes, so
                    // judging each one alone yields the mismatched (1957
                    // 成立）. Require the partner to qualify too.
                    if !paren_partner_is_convertible(text, i, b == b'(', excluded) {
                        continue;
                    }
                    let (found, suggestion, context) = if b == b'(' {
                        (
                            "(",
                            "\u{FF08}",
                            "繁體中文應使用全形左括號「（」而非半形「(」",
                        )
                    } else {
                        (
                            ")",
                            "\u{FF09}",
                            "繁體中文應使用全形右括號「）」而非半形「)」",
                        )
                    };
                    issues.push(punct_issue(i, found, suggestion, context));
                }
                b':' => {
                    // Colon enforcement controlled by profile config.
                    if !cfg.colon_enforcement {
                        continue;
                    }
                    // Guard: digit on both sides → time format (e.g. 12:30).
                    let digit_before = i > 0 && bytes[i - 1].is_ascii_digit();
                    let digit_after = i + 1 < len && bytes[i + 1].is_ascii_digit();
                    if digit_before && digit_after {
                        continue;
                    }
                    // Guard: followed by // → protocol (e.g. http://).
                    if i + 2 < len && bytes[i + 1] == b'/' && bytes[i + 2] == b'/' {
                        continue;
                    }

                    // Guard: ]: → Markdown reference/footnote definition
                    // ([^id]: text, [id]: url).
                    if i > 0 && bytes[i - 1] == b']' {
                        continue;
                    }

                    // Guard: definition-list colon, ": " at the start of a line
                    // (possibly indented) is Markdown structural markup.
                    // Pattern: (BOF or \n)(spaces/tabs)*": ".
                    if i + 1 < len && bytes[i + 1] == b' ' {
                        let line_start = if i == 0 {
                            true
                        } else {
                            // Walk backwards over spaces/tabs to find \n or
                            // BOF.
                            let mut j = i - 1;
                            loop {
                                if bytes[j] == b'\n' {
                                    break true;
                                }
                                if bytes[j] != b' ' && bytes[j] != b'\t' {
                                    break false;
                                }
                                if j == 0 {
                                    break true; // BOF after only whitespace
                                }
                                j -= 1;
                            }
                        };
                        if line_start {
                            continue;
                        }
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ":",
                        "\u{FF1A}",
                        "繁體中文應使用全形冒號「：」而非半形「:」",
                    ));
                }
                b'"' => {
                    while ascii_cursor < ascii_quotes.len() && ascii_quotes[ascii_cursor].offset < i
                    {
                        ascii_cursor += 1;
                    }
                    let Some(mark) = ascii_quotes.get(ascii_cursor).filter(|m| m.offset == i)
                    else {
                        continue;
                    };
                    let suggestion = if mark.opening {
                        "\u{300c}" // 「
                    } else {
                        "\u{300d}" // 」
                    };
                    issues.push(punct_issue(
                        i,
                        "\"",
                        suggestion,
                        "繁體中文應使用「」引號而非半形雙引號「\"」",
                    ));
                    ascii_cursor += 1;
                }

                // Not a candidate byte: the prefilter above already skipped it.
                // Skipping rather than panicking keeps a future edit that adds
                // a byte to only one of the two lists a missed lint instead of
                // a crash.
                _ => continue,
            }
        }
    }

    /// Enumeration comma (dunhao) detection.
    ///
    /// Scans for sequences of short items separated by full-width ， that
    /// likely represent coordinate lists and should use 、 instead.
    /// Severity: Info (advisory -- the heuristic false-positives on short
    /// clauses).
    pub(crate) fn scan_dunhao(&self, text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
        let comma = "\u{FF0C}"; // ，
        let comma_len = comma.len(); // 3 bytes
        let max_item_chars = 4;

        // Collect non-excluded full-width comma positions.
        let mut positions: Vec<usize> = Vec::new();
        let mut start = 0;
        while let Some(rel) = text[start..].find(comma) {
            let abs = start + rel;
            if !is_excluded(abs, abs + comma_len, excluded) {
                positions.push(abs);
            }
            start = abs + comma_len;
        }

        if positions.len() < 2 {
            return;
        }

        // is_short[j]: segment between positions[j] and positions[j+1] is 1-4
        // chars.
        let is_short: Vec<bool> = (0..positions.len() - 1)
            .map(|j| {
                let seg = text[positions[j] + comma_len..positions[j + 1]].trim();
                let count = seg.chars().count();
                count > 0 && count <= max_item_chars
            })
            .collect();

        // Find runs of consecutive short segments. A run of length N means N+1
        // commas bounding N+2 items. Require N >= 2.
        let mut i = 0;
        while i < is_short.len() {
            if !is_short[i] {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < is_short.len() && is_short[i] {
                i += 1;
            }
            let run_len = i - run_start;
            if run_len < 2 {
                continue;
            }
            for &pos in &positions[run_start..=i.min(positions.len() - 1)] {
                issues.push(super::punct_issue_sev(
                    pos,
                    "\u{FF0C}",
                    "\u{3001}",
                    "列舉項目建議使用頓號「、」而非逗號「，」",
                    Severity::Info,
                ));
            }
        }
    }

    /// CN curly quotation mark detection.
    ///
    /// Scans for CN-style curly double quotes \u{201c}/\u{201d} and single
    /// quotes \u{2018}/\u{2019}.  These are multi-byte UTF-8 characters that
    /// the byte-level ASCII scan in scan_punctuation() cannot detect.
    ///
    /// The conversion decision belongs to the pair, not the mark:
    /// [`plan_quote_conversions`] pairs over the raw text and converts a pair
    /// only when both its span and its surrounding prose establish CJK context.
    /// English typography, including smart quotes around a Chinese phrase,
    /// stays intact and a quotation is never half-rewritten. An unpaired mark
    /// has no span, so it keeps the older rule: convert when CJK sits within
    /// three spaces.
    ///
    /// Double quotes are emitted as issues; `fix_quote_pairing()` in quotes.rs
    /// then reassigns their suggestions with depth-based nesting (「」/『』).
    /// Single quotes map directly to 『/』 (secondary TW bracket quotes).
    pub(crate) fn scan_cn_curly_quotes(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
    ) {
        let doubles = plan_quote_conversions(text, excluded, QuoteKind::CurlyDouble);
        let singles = plan_quote_conversions(text, excluded, QuoteKind::CurlySingle);
        if doubles.is_empty() && singles.is_empty() {
            return;
        }

        // The pipeline skips its own sort when the issues already ascend, so
        // the two plans have to be merged rather than concatenated.
        let mut marks: Vec<(QuoteMark, QuoteKind)> = doubles
            .into_iter()
            .map(|m| (m, QuoteKind::CurlyDouble))
            .chain(singles.into_iter().map(|m| (m, QuoteKind::CurlySingle)))
            .collect();
        marks.sort_by_key(|(m, _)| m.offset);

        for (mark, kind) in marks {
            let (suggestion, context) = match (kind, mark.opening) {
                (QuoteKind::CurlyDouble, true) => (
                    "\u{300c}", // 「
                    "繁體中文應使用「」引號而非中國大陸式「\u{201c}\u{201d}」",
                ),
                (QuoteKind::CurlyDouble, false) => (
                    "\u{300d}", // 」
                    "繁體中文應使用「」引號而非中國大陸式「\u{201c}\u{201d}」",
                ),
                (_, true) => (
                    "\u{300e}", // 『
                    "繁體中文應使用『』引號而非中國大陸式「\u{2018}\u{2019}」",
                ),
                (_, false) => (
                    "\u{300f}", // 』
                    "繁體中文應使用『』引號而非中國大陸式「\u{2018}\u{2019}」",
                ),
            };
            issues.push(punct_issue(
                mark.offset,
                &text[mark.offset..mark.offset + mark.len],
                suggestion,
                context,
            ));
        }
    }

    /// Range indicator normalization.
    ///
    /// Detects ~ or - used as range indicators in CJK context and suggests
    /// the profile-appropriate full-width form: ～ (wave dash) for prose,
    /// – (en dash) for technical/UI contexts.
    pub(crate) fn scan_range_indicators(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
        cfg: &ProfileConfig,
    ) {
        let bytes = text.as_bytes();
        let len = bytes.len();

        let suggestion = if cfg.range_en_dash {
            "\u{2013}" // – (en dash)
        } else {
            "\u{FF5E}" // ～ (wave dash)
        };

        for (i, &b) in bytes.iter().enumerate() {
            if b != b'~' && b != b'-' {
                continue;
            }
            if is_excluded(i, i + 1, excluded) {
                continue;
            }

            if b == b'~' {
                // Tilde as range indicator: digit~digit or CJK~CJK.
                let left_digit = i > 0 && bytes[i - 1].is_ascii_digit();
                let right_digit = i + 1 < len && bytes[i + 1].is_ascii_digit();
                let left_cjk = adjacent_cjk(text, i, true);
                let right_cjk = adjacent_cjk(text, i + 1, false);

                if !(left_digit || left_cjk) || !(right_digit || right_cjk) {
                    continue;
                }

                // Require at least one CJK side to avoid flagging in pure
                // ASCII.
                if !left_cjk && !right_cjk {
                    continue;
                }
                // Guard: unary approximation ~N.
                if right_digit && !left_digit {
                    continue;
                }
            } else {
                // Hyphen as range indicator. Very conservative to avoid false
                // positives on markdown, CLI flags, minus signs. Skip
                // consecutive dashes.
                if (i > 0 && bytes[i - 1] == b'-') || (i + 1 < len && bytes[i + 1] == b'-') {
                    continue;
                }
                // Guard: Markdown list bullet, skip if - is at line start.
                let is_line_start = {
                    let mut j = i;
                    while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
                        j -= 1;
                    }
                    j == 0 || bytes[j - 1] == b'\n' || bytes[j - 1] == b'\r'
                };
                if is_line_start {
                    continue;
                }
                // Only flag when both adjacent non-whitespace chars are CJK.
                if !adjacent_cjk(text, i, true) || !adjacent_cjk(text, i + 1, false) {
                    continue;
                }
            }

            let found = if b == b'~' { "~" } else { "-" };
            issues.push(super::punct_issue_sev(
                i,
                found,
                suggestion,
                "範圍表示建議使用全形波浪號「～」或半形連接號「–」",
                Severity::Info,
            ));
        }
    }
}

/// How far to look for a bracket's partner. Bounded so that a line of nothing
/// but brackets cannot turn the enclosing per-byte walk quadratic; no real
/// parenthetical in prose runs longer.
const PAREN_PARTNER_SEARCH_BYTES: usize = 512;

/// Whether the bracket pairing with the one at "index" is itself surrounded by
/// CJK, so that converting both keeps the pair matched.
///
/// Depth counting rather than the first bracket in the direction of travel, so
/// a nested pair does not claim the outer partner. An unpaired bracket, one
/// whose partner is on another line, and one further away than the bound above
/// are all left half-width.
fn paren_partner_is_convertible(
    text: &str,
    index: usize,
    opening: bool,
    excluded: &[ByteRange],
) -> bool {
    let bytes = text.as_bytes();

    // The partner has to be convertible on the same terms this bracket was, and
    // an excluded one never is: the main walk skips it, so converting this half
    // alone would split the pair just as a failed CJK test would.
    let qualifies = |j: usize| {
        !is_excluded(j, j + 1, excluded)
            && immediate_cjk(text, j, true)
            && immediate_cjk(text, j + 1, false)
    };
    let mut depth = 0i32;
    if opening {
        let stop = bytes
            .len()
            .min(index.saturating_add(PAREN_PARTNER_SEARCH_BYTES));
        for (j, &byte) in bytes.iter().enumerate().take(stop).skip(index) {
            match byte {
                b'\n' => return false,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return qualifies(j);
                    }
                }
                _ => {}
            }
        }
    } else {
        let stop = index.saturating_sub(PAREN_PARTNER_SEARCH_BYTES);
        for j in (stop..=index).rev() {
            match bytes[j] {
                b'\n' => return false,
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        return qualifies(j);
                    }
                }
                _ => {}
            }
        }
    }
    false
}
