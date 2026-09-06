// Punctuation scanning: half-width to full-width detection, dunhao (enumeration
// comma), and range indicator normalization.

use super::emit::Emitter;
use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{ProfileConfig, Severity};

use super::quotes::{plan_quote_conversions, QuoteKind, QuoteMark};
use super::{adjacent_cjk, immediate_cjk, punct_issue, Scanner};

impl Scanner {
    /// Punctuation scan: detect half-width punctuation that should be
    /// full-width
    /// in a CJK context.
    ///
    /// Handles: , . ! ? ; ( ) : (2.1 + 2.2).
    /// Colon enforcement is profile-dependent: relaxed allows half-width :.
    pub(crate) fn scan_punctuation(&self, em: &mut Emitter<'_>, cfg: &ProfileConfig) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

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
                    if digits_both_sides(bytes, i) {
                        continue;
                    }
                    if !mark_is_chinese_owned(text, i, excluded) {
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

                    // One-sided, and that is what leaves the period out of
                    // mark_is_chinese_owned: CJK before it is what a Chinese
                    // sentence's period has, and no Latin run can supply it, so
                    // the embedded-clause test below it would never fire.
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
                    if !mark_is_chinese_owned(text, i, excluded) {
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
                    if colon_is_notation(bytes, i) {
                        continue;
                    }
                    if !mark_is_chinese_owned(text, i, excluded) {
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
    pub(crate) fn scan_dunhao(&self, em: &mut Emitter<'_>) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

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
    pub(crate) fn scan_cn_curly_quotes(&self, em: &mut Emitter<'_>) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

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
    pub(crate) fn scan_range_indicators(&self, em: &mut Emitter<'_>, cfg: &ProfileConfig) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

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
                if at_line_start(bytes, i) {
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

/// How many Latin words the run ending at a mark has to carry before the mark
/// is read as the English text's own punctuation rather than the surrounding
/// Chinese sentence's.
///
/// Two is where a clause parts from a term. "I agree" and "do it well" are
/// clauses that punctuate themselves; "Docker", "Node.js", "Windows 11" and
/// "3.14" are single terms a Chinese sentence borrows and then punctuates in
/// its own script, and none of them reaches two words. The corpus picked the
/// number: one costs thirty true positives on the AI corpus, three and four
/// leave standing the false positives this guard exists to remove.
const LATIN_CLAUSE_WORDS: usize = 2;

/// How far back to look for the run's start. It bounds the walk the way
/// [`PAREN_PARTNER_SEARCH_BYTES`] bounds its own, and it is a distance
/// heuristic besides: a clause half a kilobyte away is not the clause the mark
/// trails, so the words inside the bound are the ones that answer. No English
/// sentence reaches this far.
const LATIN_RUN_SEARCH_BYTES: usize = 512;

/// Whether the period at "index" joins a term rather than ending a sentence.
///
/// A period between two ASCII alphanumerics is inside one word and a run
/// reaches through it. Anywhere else it closes the run.
///
/// Not `sentence::is_latin_sentence_end`, which asks a different question: it
/// splits sentences for the detectors, so it wants whitespace and a capital
/// after the period and consults an abbreviation list. Here the question is
/// only whether the period is inside a word, and Dr. Smith, wants the run to
/// close so that the comma trails a name rather than a clause.
fn is_dotted_term_period(bytes: &[u8], index: usize) -> bool {
    index > 0
        && bytes[index - 1].is_ascii_alphanumeric()
        && bytes.get(index + 1).is_some_and(u8::is_ascii_alphanumeric)
}

/// Whether the byte at "index" closes a Latin run reaching back through it.
///
/// A run is ASCII: letters, digits, spaces, tabs and punctuation. Testing
/// bytes rather than chars is safe because every run character is one byte, so
/// any byte of a Chinese character or a full-width mark closes the run, and so
/// does a newline.
///
/// A mark closes it as well, because a mark ends the clause it trails and the
/// words beyond it answer for that clause rather than for this one: in
/// nginx, Google Chrome, the second comma follows a proper name however
/// ordinary the word the first one followed.
fn closes_latin_run(bytes: &[u8], index: usize) -> bool {
    match bytes[index] {
        b',' | b';' | b':' | b'!' | b'?' => true,
        b'.' => !is_dotted_term_period(bytes, index),
        b' ' | b'\t' => false,
        byte => !byte.is_ascii_graphic(),
    }
}

/// Whether the byte at "index" is preceded on its line by nothing but blanks.
///
/// Shared by the marks whose Markdown role depends on it: a hyphen opening a
/// line is a list bullet, and ": " opening one is a definition-list marker.
/// Text that starts with the indentation counts, which is the None arm.
fn at_line_start(bytes: &[u8], index: usize) -> bool {
    bytes[..index]
        .iter()
        .rev()
        .find(|&&byte| byte != b' ' && byte != b'\t')
        .is_none_or(|&byte| byte == b'\n' || byte == b'\r')
}

/// Whether the mark at "index" separates two digits, which makes it notation
/// rather than punctuation: 1,000 and 12:30, not a comma or a colon in prose.
fn digits_both_sides(bytes: &[u8], index: usize) -> bool {
    index > 0
        && bytes[index - 1].is_ascii_digit()
        && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
}

/// Whether the colon at "index" carries a notation rather than prose.
///
/// Three shapes, none of which a Chinese sentence punctuates: a time, 12:30; a
/// Markdown reference or footnote definition, [id]: url and [^id]: text; and a
/// definition-list marker, ": " opening a line. Everything else is a colon in
/// running text and goes on to the ownership test.
///
/// A URL scheme needs no shape here. Its colon sits inside the range RE_URL
/// (`src/engine/excluded.rs`) matches, and the walk skips an excluded byte
/// before it ever reaches this arm; a scheme with nothing after it fails the
/// ownership test anyway, since neither neighbour is CJK.
fn colon_is_notation(bytes: &[u8], index: usize) -> bool {
    digits_both_sides(bytes, index)
        || (index > 0 && bytes[index - 1] == b']')
        || (bytes.get(index + 1) == Some(&b' ') && at_line_start(bytes, index))
}

/// Whether the Chinese around the mark at "index" owns it, rather than an
/// embedded English clause the Chinese happens to quote.
///
/// The two halves of the question: Chinese on one side or the other, and a run
/// before it that is not a clause of its own. Three marks in this scan ask it,
/// , and ! ? ; and :. The period asks a one-sided version inline, the brackets
/// have their own pairing model in [`paren_partner_is_convertible`], and the
/// range indicators and the ellipsis pass ask only the first half; TODO item
/// 38 records what that costs them.
fn mark_is_chinese_owned(text: &str, index: usize, excluded: &[ByteRange]) -> bool {
    (adjacent_cjk(text, index, true) || adjacent_cjk(text, index + 1, false))
        && !in_latin_clause(text, index, excluded)
}

/// How far back the Latin run may reach before it meets content this scan does
/// not judge: the end of the nearest excluded range before "index", or 0 when
/// there is none.
///
/// An inline code span, a URL and a path are excluded because they are not
/// prose, and a stretch of text that is not prose is not an English clause
/// either. Without this, a code span spelling npm run build reads as three
/// lower-case words and takes the comma after it, when that comma is the
/// Chinese sentence's and the code span is only a term the sentence borrowed.
///
/// The ranges are sorted and non-overlapping, so their ends ascend and the last
/// one starting before the mark is the nearest. The "is_excluded" test in the
/// walk above already established that no range covers the mark itself, so that
/// end cannot pass it; the min is there for a caller that has not.
fn excluded_run_floor(index: usize, excluded: &[ByteRange]) -> usize {
    let after = excluded.partition_point(|range| range.start < index);
    if after == 0 {
        0
    } else {
        excluded[after - 1].end.min(index)
    }
}

/// Whether the mark at "index" ends a run of Latin text long enough to be a
/// clause of its own.
///
/// The run is the stretch ending at the mark that [`closes_latin_run`] and
/// [`excluded_run_floor`] bound, so an embedded English clause is measured on
/// its own: not on the Chinese around it, not on whatever preceded the mark
/// before it, and not on a code span or a URL this scan already declined to
/// read as prose. ASCII only, and deliberately so: a run of Latin script
/// carrying its own diacritics closes
/// on the first one, so el esta bien reads as a clause and "él está bien" does
/// not. English is what the embedded runs in zh-TW technical prose are, and
/// widening the character class would widen this guard's reach with no corpus
/// to say by how much.
///
/// A word is a whitespace-delimited token holding at least one ASCII letter,
/// which keeps a term whole however it is spelled. Counting letter runs
/// instead would split Node.js in two and read a lone term as a clause.
///
/// Reaching the count is not enough on its own: one word has to begin in lower
/// case. A borrowed multi-word term is a proper name, Visual Studio Code,
/// Google Chrome, New York, and carries none, while a clause is built out of
/// ordinary words, "I agree", "file not found", "do one thing and do it well".
/// The cost is at the two edges the case signal cannot read: a title-cased or
/// shouted clause, "I Agree," and "FILE NOT FOUND:", keeps its mark converted,
/// and a lower-case brand, iPhone Pro Max, keeps its own. Both are rarer in
/// zh-TW prose than the multi-word product name the signal buys back.
///
/// Reading forward as well would silence 他說, I agree, where the comma
/// follows Chinese and the English merely comes after it.
fn in_latin_clause(text: &str, index: usize, excluded: &[ByteRange]) -> bool {
    let bytes = text.as_bytes();
    let floor = index
        .saturating_sub(LATIN_RUN_SEARCH_BYTES)
        .max(excluded_run_floor(index, excluded));

    // Every step back crosses one ASCII byte, so the start stays on a character
    // boundary and the slice below is always valid. Stopping on the floor keeps
    // it: the walk only reaches the floor after reading the byte there and
    // finding it an ASCII one that does not close the run.
    let mut start = index;
    while start > floor && !closes_latin_run(bytes, start - 1) {
        start -= 1;
    }

    let mut words = 0usize;
    let mut has_lower_case_word = false;

    // The first letter of the token, not its first byte: a leading quote or
    // bracket says nothing about the word's case, and a token holding no letter
    // at all is no word.
    for first in text[start..index]
        .split_ascii_whitespace()
        .filter_map(|token| token.bytes().find(u8::is_ascii_alphabetic))
    {
        words += 1;
        has_lower_case_word |= first.is_ascii_lowercase();
    }

    words >= LATIN_CLAUSE_WORDS && has_lower_case_word
}
