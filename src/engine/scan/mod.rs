// Core scanning engine.
//
// Builds Aho-Corasick automata from spelling and case rules, then scans input
// text for violations:
//
//   1. Build excluded ranges (URLs, paths, @mentions, code fences).
//   2. Detect Chinese type (Traditional vs Simplified).
//   3. Aho-Corasick scan for spelling rules: skip excluded positions,
//      skip variant rules when text is Simplified.
//   4. Aho-Corasick scan for case rules: check word boundaries and
//      compare matched text against valid forms (term + alternatives).
//   5. Punctuation, spacing, ellipsis, quote checks.
//   6. Overlap resolution (longest match wins).
//   7. Grammar checks (interlingual transfer, A-not-A + 嗎 clash), which
//      run after overlap resolution to avoid suppressing narrower issues.

mod acronym;
mod case_rule;
mod ellipsis;
mod emit;

mod grammar;
mod overlap;
mod punctuation;
mod quotes;
mod repetition;
pub(crate) mod rule_ir;
mod spacing;
mod spelling;

pub use rule_ir::ProfileFilter;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use self::emit::Emitter;
use super::excluded::{build_excluded_ranges, merge_ranges_pub, ByteRange};
use super::lineindex::{ColumnEncoding, LineIndex};
use super::markdown::{
    build_markdown_excluded_ranges_with_options, build_yaml_excluded_ranges, MdScanOptions,
};
use super::normalize::{map_offset, map_range_forward, normalize_nfc, Normalized};
use super::segment::{BoundaryBitmap, Segmenter};
use super::sentence::BoundaryIndex;
use super::suppression::build_suppression_ranges;
use serde::{Deserialize, Serialize};

use super::zhtype::ChineseType;
use crate::rules::ruleset::{
    CaseRule, Issue, IssueType, PhaseFamily, PhasePass, Profile, ProfileConfig, Register, RuleType,
    Severity, SpellingRule,
};

use self::ellipsis::scan_ellipsis;
use self::quotes::{fix_quote_pairing, validate_quote_hierarchy};

// Scratch space: reusable buffers for per-scan mutable state

/// Pre-allocated buffers for per-scan mutable state.
///
/// Creating one of these and passing it to `scan_with_config_into` avoids
/// repeated `Vec` allocations on the hot path.  Callers that process many
/// documents in a loop (e.g. the MCP server) can keep a single
/// `ScratchSpace` alive across requests.
///
/// All buffers are cleared (without deallocating) at the start of each
/// scan via [`ScratchSpace::clear`].
pub struct ScratchSpace {
    /// Accumulator for issues found during a scan.
    pub(crate) issues: Vec<Issue>,
    /// Document-wide clue hit index (byte_offset, clue_id).
    pub(crate) clue_index: Vec<(usize, u16)>,
    // -- overlap resolution scratch --
    /// Priority-order indices into the issues vec.
    pub(crate) overlap_order: Vec<usize>,
    /// Per-issue keep/discard flags.
    pub(crate) overlap_keep: Vec<bool>,
    /// Accepted byte intervals for overlap checking.
    pub(crate) overlap_accepted: Vec<(usize, usize)>,
}

impl ScratchSpace {
    /// Create a new scratch space with no pre-allocated capacity.
    pub fn new() -> Self {
        Self {
            issues: Vec::new(),
            clue_index: Vec::new(),
            overlap_order: Vec::new(),
            overlap_keep: Vec::new(),
            overlap_accepted: Vec::new(),
        }
    }

    /// Clear all buffers without releasing their backing memory.
    pub fn clear(&mut self) {
        self.issues.clear();
        self.clue_index.clear();
        self.overlap_order.clear();
        self.overlap_keep.clear();
        self.overlap_accepted.clear();
    }
}

impl Default for ScratchSpace {
    fn default() -> Self {
        Self::new()
    }
}

// Public types

/// Output of a scan operation: detected issues plus the Chinese script type
/// detected during scanning.  Returning detected_script here eliminates the
/// need for callers to run a second O(n) detect_chinese_type pass over the
/// same text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanOutput {
    pub issues: Vec<Issue>,
    pub detected_script: ChineseType,
    /// AI writing signature report.  Present only when AI scoring is
    /// requested (detect_ai flag or explicit ai_score).
    #[serde(default)]
    pub ai_signature: Option<crate::engine::ai_score::AiSignatureReport>,
    /// Translationese (翻譯腔/歐化) signature report.  Present only when
    /// translationese detection is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translationese_signature: Option<crate::engine::translationese_score::TranslationeseReport>,
    /// Coverage statistics for this scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageReport>,
    /// Ratio of oral/filler markers to total CJK characters (0.0-1.0).
    ///
    /// High values (>0.05) suggest transcript or spoken-style text.  This is
    /// a document-level metric, not a per-issue flag.  Omitted for texts
    /// with fewer than 20 CJK characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oral_density: Option<f32>,
    /// Signal-based quality flags for downstream consumers.
    ///
    /// Examples: "asr_artifacts", "stutter_detected", "high_oral_density",
    /// "spaced_acronyms".  Backward-compatible: existing consumers ignore
    /// unknown fields.  Empty vec is omitted from JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality_flags: Vec<String>,
}

/// How many rules were in scope and how many produced hits.
///
/// Lets callers distinguish 'no issues found' from 'nothing was checked'.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Spelling rules that were active for this profile (excludes case,
    /// punctuation, and other procedural checks which lack discrete counts).
    pub rules_checked: usize,
    /// Distinct spelling rules that produced at least one surviving issue
    /// (counted before inflate clears rule indices).
    pub rules_matched: usize,
}

/// Content type for determining exclusion strategy.
///
/// Shared between CLI and MCP pipelines (20.4 deduplication).  Lives in the
/// engine so both consumers can use the same scan_for_content_type method
/// without duplicating the dispatch logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Plain,
    Markdown,
    /// Like Markdown, but code blocks (fenced/indented) are NOT excluded from
    /// scanning.  Inline code and HTML blocks are still excluded.  Useful when
    /// code blocks contain prose (e.g. translated output, comments) that should
    /// be linted.
    MarkdownScanCode,
    Yaml,
}

impl ContentType {
    /// Canonical string name matching the MCP/CLI parameter values.
    pub fn name(self) -> &'static str {
        match self {
            ContentType::Plain => "plain",
            ContentType::Markdown => "markdown",
            ContentType::MarkdownScanCode => "markdown-scan-code",
            ContentType::Yaml => "yaml",
        }
    }

    /// The inverse of `name`, for the CLI flag and the MCP parameter.
    ///
    /// Beside `name` rather than at either caller, so the two directions
    /// cannot disagree and the CLI and the tool answer the same way.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "plain" => Some(ContentType::Plain),
            "markdown" => Some(ContentType::Markdown),
            "markdown-scan-code" => Some(ContentType::MarkdownScanCode),
            "yaml" => Some(ContentType::Yaml),
            _ => None,
        }
    }

    /// What a file name implies, when nothing was asked for explicitly.
    ///
    /// Case-insensitive: a `README.MD` is Markdown, and treating it as plain
    /// text rewrites what a fence was protecting.
    pub fn from_file_name(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            ContentType::Markdown
        } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
            ContentType::Yaml
        } else {
            ContentType::Plain
        }
    }

    /// Whether `#` introduces a comment in this format, which decides
    /// where an inline suppression pragma may sit.
    ///
    /// False for both Markdown types, where `#` starts a heading, so a
    /// heading that documents a pragma does not become one.  That costs
    /// `MarkdownScanCode` nothing even though it scans inside fences: the
    /// suppression pass reads raw lines, so `<!-- zhtw:ignore -->` works
    /// anywhere in the file, fenced code included.
    ///
    /// True for the code-bearing types: YAML, and the plain text that
    /// TOML, Python, shell, and locale files resolve to.
    pub fn hash_comments(self) -> bool {
        !matches!(self, ContentType::Markdown | ContentType::MarkdownScanCode)
    }
}

// Constants

/// Number of characters around a match to examine for context clues.
/// Shared with fixer.rs which re-exports it.
pub(crate) const CONTEXT_WINDOW_CHARS: usize = 40;

/// Minimum context clue matches for the scanner to emit a context-dependent
/// issue.  One nearby clue word is enough to confirm the right domain.
/// The fixer uses a stricter threshold (2) before auto-applying corrections.
const MIN_SCAN_CLUE_MATCHES: usize = 1;

/// Number of characters for positional clue windows (before:/after:).
/// Narrower than the general context window (40) because positional clues
/// express proximity, not just co-occurrence.
const POSITIONAL_WINDOW_CHARS: usize = 20;

fn dedup_translationese_phase_duplicates(issues: &mut Vec<Issue>) {
    let indexed_spans: Vec<(PhaseFamily, usize, usize)> = issues
        .iter()
        .filter_map(|issue| match issue.phase_family {
            Some((family, PhasePass::Indexed)) => {
                Some((family, issue.offset, issue.offset + issue.length))
            }
            _ => None,
        })
        .collect();

    issues.retain(|issue| match issue.phase_family {
        Some((family, PhasePass::Lexical)) => {
            let start = issue.offset;
            let end = issue.offset + issue.length;
            !indexed_spans
                .iter()
                .any(|&(indexed_family, indexed_start, indexed_end)| {
                    indexed_family == family && indexed_start < end && start < indexed_end
                })
        }
        _ => true,
    });
}

/// A parsed positional condition for disambiguation.
///
/// Positional clues constrain WHERE a context term must appear relative to
/// the AC match, unlike flat context_clues which check presence anywhere
/// in the +-40-char window.
#[derive(Debug, Clone)]
pub(crate) enum PositionalClue {
    /// TERM must appear within POSITIONAL_WINDOW_CHARS chars AFTER the match.
    Before(String),
    /// TERM must appear within POSITIONAL_WINDOW_CHARS chars BEFORE the match.
    After(String),
    /// TERM must be immediately adjacent to the match (no gap, either side).
    Adjacent(String),
    /// TERM must NOT appear within POSITIONAL_WINDOW_CHARS chars AFTER the
    /// match.
    NotBefore(String),
    /// TERM must NOT appear within POSITIONAL_WINDOW_CHARS chars BEFORE the
    /// match.
    NotAfter(String),
}

impl PositionalClue {
    /// Parse a positional clue string (e.g. "before:函式", "not_after:的").
    /// Returns None if the syntax is unrecognized.
    fn parse(s: &str) -> Option<Self> {
        // Order matters: longer prefixes (not_before, not_after) must be
        // checked before their shorter counterparts (before, after).
        if let Some(t) = s.strip_prefix("not_before:").filter(|t| !t.is_empty()) {
            return Some(PositionalClue::NotBefore(t.to_string()));
        }
        if let Some(t) = s.strip_prefix("not_after:").filter(|t| !t.is_empty()) {
            return Some(PositionalClue::NotAfter(t.to_string()));
        }
        if let Some(t) = s.strip_prefix("before:").filter(|t| !t.is_empty()) {
            return Some(PositionalClue::Before(t.to_string()));
        }
        if let Some(t) = s.strip_prefix("after:").filter(|t| !t.is_empty()) {
            return Some(PositionalClue::After(t.to_string()));
        }
        if let Some(t) = s.strip_prefix("adjacent:").filter(|t| !t.is_empty()) {
            return Some(PositionalClue::Adjacent(t.to_string()));
        }
        None
    }
}

// Shared helper functions

/// Whether the text between `prev_end` and `offset` holds a paragraph break.
///
/// Testing for the two pure forms missed a mixed "\n\r\n", so on such a
/// document the quote-nesting depth and the ASCII-quote parity never reset and
/// every suggestion after the break alternated wrongly. Same rule as the
/// splitters, from the same function.
fn has_paragraph_break(text: &str, prev_end: usize, offset: usize) -> bool {
    let Some(window) = text.get(prev_end..offset) else {
        return false;
    };
    let bytes = window.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\n' && crate::engine::sentence::blank_line_end(bytes, i).is_some())
}

/// Split text into paragraph blocks at double-newline boundaries.
///
/// Returns (byte_offset, paragraph_slice) pairs. Handles both \n\n (LF)
/// and \r\n\r\n (CRLF) paragraph separators.
pub(super) fn split_paragraphs(text: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut prev = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if let Some(next) = crate::engine::sentence::blank_line_end(bytes, i) {
                result.push((prev, trim_line_end(&text[prev..i])));
                prev = next;
                i = prev;
                continue;
            }
        }
        i += 1;
    }
    if prev < text.len() {
        result.push((prev, trim_line_end(&text[prev..])));
    }
    result
}

/// Drop a paragraph's own trailing line ending.
///
/// Callers test exclusion as "is the whole paragraph covered", so a slice that
/// carries its terminator reaches past the content an exclusion range ends at.
fn trim_line_end(para: &str) -> &str {
    para.trim_end_matches(['\r', '\n'])
}

/// Extract a surrounding text window (in chars) around a byte range.
///
/// Returns the substring spanning CONTEXT_WINDOW_CHARS characters before
/// the match start and after the match end, including the match itself.
pub(crate) fn surrounding_window(text: &str, start: usize, end: usize) -> &str {
    if text.is_empty() {
        return &text[0..0];
    }

    // Walk backward CONTEXT_WINDOW_CHARS characters from start.
    let mut byte_start = start;
    for _ in 0..CONTEXT_WINDOW_CHARS {
        if byte_start == 0 {
            break;
        }
        byte_start = text.floor_char_boundary(byte_start - 1);
    }

    // Walk forward CONTEXT_WINDOW_CHARS characters from end.
    let mut byte_end = end;
    for _ in 0..CONTEXT_WINDOW_CHARS {
        if byte_end >= text.len() {
            break;
        }
        byte_end = text.ceil_char_boundary(byte_end + 1);
    }

    &text[byte_start..byte_end]
}

/// Clamp a byte-offset window at excluded-range boundaries.
///
/// Given an unclamped window [win_start, win_end) around a match at
/// [match_start, match_end), narrows the window so it does not extend
/// past adjacent excluded ranges.  Snaps results to valid UTF-8 char
/// boundaries.  Returns (clamped_start, clamped_end).
fn clamp_at_excluded(
    text: &str,
    win_start: usize,
    win_end: usize,
    match_start: usize,
    match_end: usize,
    excluded: &[ByteRange],
) -> (usize, usize) {
    let mut clamped_start = win_start;
    let mut clamped_end = win_end;

    // Clamp left edge: excluded ranges ending before match_start.
    let left_idx = excluded.partition_point(|r| r.start < match_start);
    for excl in excluded[..left_idx].iter().rev() {
        if excl.end <= clamped_start {
            break;
        }
        if excl.end <= match_start && excl.end > clamped_start {
            clamped_start = excl.end;
        }
    }

    // Clamp right edge: excluded ranges starting after match_end.
    let right_idx = excluded.partition_point(|r| r.start < match_end);
    for excl in &excluded[right_idx..] {
        if excl.start >= clamped_end {
            break;
        }
        if excl.start >= match_end && excl.start < clamped_end {
            clamped_end = excl.start;
        }
    }

    // Snap inward to valid UTF-8 char boundaries.
    let clamped_start = text.ceil_char_boundary(clamped_start);
    let clamped_end = text.floor_char_boundary(clamped_end.min(text.len()));

    if clamped_start > clamped_end {
        (clamped_start, clamped_start)
    } else {
        (clamped_start, clamped_end)
    }
}

/// Like surrounding_window but clamps the window at excluded-range
/// boundaries so that context clues inside a code block (or other excluded
/// region) cannot influence rules that fire outside it.
// Only the native-gated fixer calls this, so it is dead in every build without
// that feature, not just the browser-wasm one.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn surrounding_window_bounded<'a>(
    text: &'a str,
    start: usize,
    end: usize,
    excluded: &[ByteRange],
) -> &'a str {
    let window = surrounding_window(text, start, end);
    if excluded.is_empty() {
        return window;
    }

    let win_start = window.as_ptr() as usize - text.as_ptr() as usize;
    let win_end = win_start + window.len();
    let (cs, ce) = clamp_at_excluded(text, win_start, win_end, start, end, excluded);
    &text[cs..ce]
}

/// Fused single-pass: detect SC/TC type, build LineIndex, and optionally
/// build BoundaryBitmap.  Shares one `char_indices()` iteration for all three.
fn detect_type_lineindex_and_bitmap<'a>(
    text: &'a str,
    segmenter: Option<&Segmenter>,
) -> (ChineseType, LineIndex<'a>, BoundaryBitmap) {
    use super::zhtype::{SIMPLIFIED_CHARS, TRADITIONAL_CHARS};

    let mut line_starts = vec![0usize];
    let mut simplified_count: usize = 0;
    let mut traditional_count: usize = 0;

    // Collect char indices (needed for bitmap forward probing).
    let chars: Vec<(usize, char)> = text.char_indices().collect();

    for &(byte_offset, ch) in &chars {
        if ch == '\n' {
            line_starts.push(byte_offset + 1);
        }
        if SIMPLIFIED_CHARS.contains(&ch) {
            simplified_count += 1;
        } else if TRADITIONAL_CHARS.contains(&ch) {
            traditional_count += 1;
        }
    }

    let zh_type = if text.trim().is_empty() {
        ChineseType::Unknown
    } else if simplified_count > traditional_count {
        ChineseType::Simplified
    } else if traditional_count > simplified_count {
        ChineseType::Traditional
    } else {
        ChineseType::Unknown
    };

    let line_index = LineIndex::from_parts(text, line_starts);

    // Build boundary bitmap from the same char indices if segmenter provided.
    let bitmap = if let Some(seg) = segmenter {
        seg.build_boundary_bitmap_from_chars(text, &chars)
    } else {
        BoundaryBitmap::empty()
    };

    (zh_type, line_index, bitmap)
}

/// Attach `(row, col)` coordinates to issues that fall inside a Markdown
/// table cell.  Cells are typically small spans, so a linear scan per issue
/// is sufficient.
fn annotate_table_cells(
    issues: &mut [crate::rules::ruleset::Issue],
    cell_spans: &[super::markdown::TableCellSpan],
) {
    for issue in issues.iter_mut() {
        let issue_end = issue.offset.saturating_add(issue.length);
        for span in cell_spans {
            if issue.offset >= span.start && issue_end <= span.end {
                issue.table_cell = Some(crate::rules::ruleset::TableCell {
                    row: span.row,
                    col: span.col,
                });
                break;
            }
        }
    }
}

/// Boost severity for issues fully contained in Markdown heading ranges.
/// Info -> Warning, Warning -> Error.  Error stays Error.
///
/// Uses strict containment (not overlap) so that issues spanning heading
/// boundaries (rare but possible across sectioned blocks) are not boosted.
///
/// Returns `true` when at least one issue was boosted, signalling to the
/// caller that the severity-descending sort contract may now be violated
/// and the issue vec should be re-sorted.
fn boost_heading_severity(issues: &mut [Issue], heading_ranges: &[ByteRange]) -> bool {
    let mut changed = false;
    for issue in issues.iter_mut() {
        let issue_end = issue.offset.saturating_add(issue.length);
        let inside_heading = heading_ranges
            .iter()
            .any(|r| issue.offset >= r.start && issue_end <= r.end);
        if inside_heading {
            let new_sev = match issue.severity {
                Severity::Info => Severity::Warning,
                Severity::Warning => Severity::Error,
                Severity::Error => Severity::Error,
            };
            if new_sev != issue.severity {
                issue.severity = new_sev;
                changed = true;
            }
        }
    }
    changed
}

/// Remap issue offsets from NFC-normalized text back to original positions.
/// Updates offset, length, found text, and recomputes line/col.
fn remap_issues_to_original(issues: &mut [Issue], original: &str, norm: &Normalized) {
    for issue in issues.iter_mut() {
        let orig_offset = map_offset(&norm.offset_map, issue.offset);
        let orig_end = map_offset(&norm.offset_map, issue.offset + issue.length);
        issue.offset = orig_offset;
        issue.length = orig_end.saturating_sub(orig_offset);
        let end = (orig_offset + issue.length).min(original.len());
        if let Some(found) = original.get(orig_offset..end) {
            issue.found = found.to_string();
        }
    }

    // NFC offset mapping is monotonically non-decreasing, so issues that were
    // sorted by NFC offset remain sorted by original offset. Use the
    // linear-pass fill to avoid O(log n) binary search per issue.
    let line_index = LineIndex::new(original);
    line_index.fill_line_col_sorted(issues, ColumnEncoding::Utf16);
}

/// Build suggestion list from a rule's `to` and `english` fields.
///
/// Filters empty strings from `to`. If no suggestions remain, falls back to
/// the `english` field (used when no Chinese translation exists).
///
/// AiFiller deletion rules (`to: [""]`) are special: the empty string is
/// the intended suggestion (delete the filler phrase), so it is preserved
/// as-is instead of being filtered away.
pub(crate) fn effective_suggestions(rule: &SpellingRule) -> Vec<String> {
    // AiFiller deletion: to == [""] means 'delete this phrase'. Preserve the
    // empty-string suggestion so the fixer can apply it.
    if rule.is_deletion_rule() {
        return rule.to.clone();
    }
    let to = &rule.to;
    // Fast path: most rules have no empty strings in to.
    if !to.is_empty() && to.iter().all(|s| !s.is_empty()) {
        return to.clone();
    }
    let filtered: Vec<String> = to.iter().filter(|s| !s.is_empty()).cloned().collect();
    if !filtered.is_empty() {
        return filtered;
    }
    match rule.english.as_deref() {
        Some(e) if !e.is_empty() => vec![e.to_string()],
        _ => Vec::new(),
    }
}

/// Slice up to `n` characters from a byte offset, char-boundary safe.
/// Returns the byte range that covers up to n chars from start_byte.
/// Out-of-range or non-char-boundary `start_byte` is clamped to `text.len()`
/// to keep all callers panic-free.
pub(crate) fn char_bounded_end(text: &str, start_byte: usize, n_chars: usize) -> usize {
    if start_byte >= text.len() || !text.is_char_boundary(start_byte) {
        return text.len();
    }
    text[start_byte..]
        .char_indices()
        .nth(n_chars)
        .map(|(i, _)| start_byte + i)
        .unwrap_or(text.len())
}

/// Returns true if ch is a CJK ideograph (unified, extensions A-I,
/// compatibility, or bopomofo).  Excludes CJK Symbols/Punctuation
/// (U+3000..U+303F) to avoid false positives when full-width marks sit
/// next to half-width punctuation.
pub(crate) fn is_cjk_ideograph(ch: char) -> bool {
    matches!(ch,
        '\u{3100}'..='\u{312F}' |   // Bopomofo
        '\u{3400}'..='\u{4DBF}' |   // CJK Extension A
        '\u{4E00}'..='\u{9FFF}' |   // CJK Unified Ideographs
        '\u{F900}'..='\u{FAFF}' |   // CJK Compatibility Ideographs
        '\u{20000}'..='\u{2A6DF}' | // CJK Extension B
        '\u{2A700}'..='\u{2B73F}' | // CJK Extension C
        '\u{2B740}'..='\u{2B81F}' | // CJK Extension D
        '\u{2B820}'..='\u{2CEAF}' | // CJK Extension E
        '\u{2CEB0}'..='\u{2EBEF}' | // CJK Extension F
        '\u{2EBF0}'..='\u{2EE5F}' | // CJK Extension I
        '\u{30000}'..='\u{3134F}' | // CJK Extension G
        '\u{31350}'..='\u{323AF}'   // CJK Extension H
    )
}

/// Returns true if ch is a CJK context character: either a CJK ideograph
/// or a CJK punctuation/bracket mark.  Used by adjacent_cjk so that
/// text like 他說「你好」. correctly recognises 」 as CJK context.
pub(crate) fn is_cjk_context(ch: char) -> bool {
    is_cjk_ideograph(ch)
        || matches!(ch,

            // CJK Symbols and Punctuation (U+3001..U+303F, skip U+3000 =
            // ideographic space)
            '\u{3001}'..='\u{303F}' |

            // Fullwidth Forms: fullwidth punctuation and letters
            // (U+FF01..U+FF60)
            '\u{FF01}'..='\u{FF60}' |
            // Halfwidth CJK punctuation (U+FF61..U+FF65)
            '\u{FF61}'..='\u{FF65}' |
            // CJK Compatibility Forms (U+FE30..U+FE4F)
            '\u{FE30}'..='\u{FE4F}'
        )
}

/// Scan backward (before=true) or forward (before=false) from byte_pos,
/// skipping all Unicode whitespace (including ideographic space U+3000),
/// and return true if the first non-whitespace character is a CJK context
/// character (ideograph or CJK punctuation).
fn adjacent_cjk(text: &str, byte_pos: usize, before: bool) -> bool {
    adjacent_cjk_inner(text, byte_pos, before, usize::MAX)
}

/// Check whether the immediately adjacent character (no whitespace skip) is
/// CJK.
fn immediate_cjk(text: &str, byte_pos: usize, before: bool) -> bool {
    adjacent_cjk_inner(text, byte_pos, before, 0)
}

/// Check whether the nearest non-whitespace character in the given direction
/// is a CJK context character.  `max_ws` limits how many whitespace chars
/// to skip (0 = immediate adjacency, usize::MAX = unlimited).
fn adjacent_cjk_inner(text: &str, byte_pos: usize, before: bool, max_ws: usize) -> bool {
    let mut ws = 0usize;
    if before {
        let mut pos = byte_pos;
        loop {
            if pos == 0 {
                return false;
            }
            pos = text.floor_char_boundary(pos - 1);
            let ch = match text[pos..].chars().next() {
                Some(c) => c,
                None => return false,
            };
            if ch.is_whitespace() {
                ws += 1;
                if ws > max_ws {
                    return false;
                }
                continue;
            }
            return is_cjk_context(ch);
        }
    } else {
        for ch in text[byte_pos..].chars() {
            if ch.is_whitespace() {
                ws += 1;
                if ws > max_ws {
                    return false;
                }
                continue;
            }
            return is_cjk_context(ch);
        }
        false
    }
}

/// Construct a punctuation Issue at the given byte offset with explicit
/// severity.
fn punct_issue_sev(
    offset: usize,
    found: &str,
    suggestion: &str,
    context: &str,
    severity: Severity,
) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        vec![suggestion.into()],
        IssueType::Punctuation,
        severity,
    )
    .with_context(context)
}

/// Construct a Warning-severity punctuation Issue at the given byte offset.
fn punct_issue(offset: usize, found: &str, suggestion: &str, context: &str) -> Issue {
    punct_issue_sev(offset, found, suggestion, context, Severity::Warning)
}

/// Build exclusion ranges for text based on content type.
///
/// Combines content-pattern exclusions (URLs, paths, mentions) with
/// structural exclusions appropriate to the content type and inline
/// suppression markers.  Shared between CLI and MCP pipelines.
pub fn build_exclusions_for_content_type(text: &str, content_type: ContentType) -> Vec<ByteRange> {
    build_exclusions_for_content_type_with_options(text, content_type, MdScanOptions::default())
}

/// Build exclusion ranges with explicit Markdown options.  Honors the
/// caller-supplied [MdScanOptions] (e.g. the blockquote exemption);
/// falls back to defaults for non-Markdown content types.
pub fn build_exclusions_for_content_type_with_options(
    text: &str,
    content_type: ContentType,
    md_opts: MdScanOptions,
) -> Vec<ByteRange> {
    let mut excluded = build_excluded_ranges(text);
    match content_type {
        ContentType::Markdown => {
            excluded.extend(build_markdown_excluded_ranges_with_options(text, md_opts));
        }
        ContentType::MarkdownScanCode => {
            // MarkdownScanCode forces scan_code_blocks=true regardless of the
            // caller-supplied flag, but still honors exempt_blockquotes so the
            // blockquote opt-in works for source-code content too.
            let opts = MdScanOptions::new(true, md_opts.exempt_blockquotes);
            excluded.extend(build_markdown_excluded_ranges_with_options(text, opts));
        }
        ContentType::Yaml => excluded.extend(build_yaml_excluded_ranges(text)),
        ContentType::Plain => {}
    }
    excluded.extend(build_suppression_ranges(text, content_type.hash_comments()));
    merge_ranges_pub(excluded)
}

/// Markdown options implied by a content type plus profile config.  One
/// definition so scan, glossary, CLI fix and MCP fix cannot drift apart on
/// which bytes count as structure.
fn md_scan_options(content_type: ContentType, cfg: &ProfileConfig) -> MdScanOptions {
    MdScanOptions::new(
        matches!(content_type, ContentType::MarkdownScanCode),
        cfg.exempt_blockquotes,
    )
}

/// Build exclusion ranges for the content type, deriving the Markdown
/// options from the profile config.  This is what every pipeline stage that
/// needs a structure mask should call.
pub fn build_exclusions_for_content_type_with_config(
    text: &str,
    content_type: ContentType,
    cfg: &ProfileConfig,
) -> Vec<ByteRange> {
    build_exclusions_for_content_type_with_options(
        text,
        content_type,
        md_scan_options(content_type, cfg),
    )
}

// Scanner struct and public API

/// Compiled scanner, reusable across multiple scan calls.
pub struct Scanner {
    /// Compiled spelling rule database (AC automata + per-rule data).
    spelling_db: rule_ir::CompiledSpellingDb,

    case_ac: Option<AhoCorasick>,
    case_rules: Vec<CaseRule>,

    /// MMSEG segmenter for fixer context-clue checks and public accessor.
    segmenter: Segmenter,

    /// Profile filter used at construction time.  Stored so scan-time
    /// config compatibility can be verified via debug_assert.
    build_filter: rule_ir::ProfileFilter,

    /// Phrases owned by procedural detectors rather than by the lexical pass.
    /// Built from the unfiltered rule set, so a profile that drops a rule type
    /// from the automaton does not also empty a detector with its own gate.
    guards: rule_ir::GuardRules,
}

impl Scanner {
    /// Read-only access to the spelling rules held by this scanner.
    pub fn spelling_rules(&self) -> &[SpellingRule] {
        &self.spelling_db.spelling_rules
    }

    /// Read-only access to the charwise double-array Aho-Corasick automaton.
    ///
    /// Exposed for benchmarking (e.g. measuring raw AC traversal cost
    /// independently of eval/overlap/line-col).  Returns `None` when the
    /// scanner fell back to bytewise AC (daachorse build failure).
    pub fn ac_charwise(&self) -> Option<&daachorse::CharwiseDoubleArrayAhoCorasick<usize>> {
        self.spelling_db.ac_charwise.as_ref()
    }

    /// Build boundary bitmap for the given text (for benchmarking).
    pub fn build_boundary_bitmap(&self, text: &str) -> BoundaryBitmap {
        self.segmenter.build_boundary_bitmap(text)
    }

    /// Run only the spelling scan stage, returning issue count (for
    /// benchmarking).
    /// Includes: clue pre-scan, boundary bitmap, eval.
    /// Does NOT include sort/overlap/inflation/line-col.
    pub fn bench_spelling_only_raw(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: &ProfileConfig,
    ) -> usize {
        let zh_type = super::zhtype::detect_chinese_type(text);
        let bitmap = if text.len() > 4096 {
            self.segmenter.build_boundary_bitmap(text)
        } else {
            BoundaryBitmap::empty()
        };
        let mut issues = Vec::new();
        let mut clue_buf = Vec::new();
        let mut em = Emitter::new(text, excluded, &mut issues);
        self.scan_spelling(&mut em, zh_type, cfg, &mut clue_buf, &bitmap);
        issues.len()
    }

    /// Collect raw issues from all scan passes (spelling, case, punctuation,
    /// spacing, ellipsis, quotes) WITHOUT sort, overlap, or inflate.
    /// For benchmarking the sort+overlap stage on realistic pre-sort input.
    pub fn bench_collect_raw_issues(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: &ProfileConfig,
    ) -> Vec<Issue> {
        let zh_type = super::zhtype::detect_chinese_type(text);
        let bitmap = if text.len() > 4096 {
            self.segmenter.build_boundary_bitmap(text)
        } else {
            BoundaryBitmap::empty()
        };
        let mut issues = Vec::new();
        let mut clue_buf = Vec::new();
        let mut em = Emitter::new(text, excluded, &mut issues);
        if cfg.spelling {
            self.scan_spelling(&mut em, zh_type, cfg, &mut clue_buf, &bitmap);
        }
        if cfg.casing {
            self.scan_case(&mut em);
        }
        if cfg.basic_punctuation {
            self.scan_punctuation(&mut em, cfg);
            self.scan_cn_curly_quotes(&mut em);
            self.scan_spacing(&mut em);
        }
        if cfg.ellipsis_normalization {
            scan_ellipsis(&mut em);
        }
        issues
    }

    /// Run sort + overlap on a pre-built issue vec (for benchmarking).
    pub fn bench_sort_and_overlap(issues: &mut Vec<Issue>) {
        issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(b.length.cmp(&a.length)));
        let mut order = Vec::new();
        let mut keep = Vec::new();
        let mut accepted = Vec::new();
        overlap::resolve_overlaps_with_scratch(issues, &mut order, &mut keep, &mut accepted);
    }

    /// Inflate deferred spelling issues (for benchmarking).
    pub fn bench_inflate(&self, text: &str, issues: &mut [Issue]) {
        rule_ir::inflate_spelling_issues(&self.spelling_db, text, &[], issues);
    }

    /// Build a scanner from loaded rules.
    ///
    /// The spelling automaton matches literally (Chinese terms don't need
    /// case folding). The case automaton is ASCII-case-insensitive so it
    /// catches e.g. "javascript" when the canonical form is "JavaScript".
    pub fn new(spelling_rules: Vec<SpellingRule>, case_rules: Vec<CaseRule>) -> Self {
        Self::new_filtered(spelling_rules, case_rules, &rule_ir::ProfileFilter::none())
    }

    /// Build a scanner with profile-aware rule filtering.
    ///
    /// Rules whose types are excluded by `filter` are omitted from the AC
    /// automaton entirely, shrinking it by ~5% under the default profile.
    /// Use this when the target profile is known at construction time.
    ///
    /// The resulting scanner is profile-locked: scan-time config flags
    /// cannot re-enable rule types that were excluded at build time.
    /// Callers must ensure the filter matches the scan-time config.
    pub fn new_filtered(
        spelling_rules: Vec<SpellingRule>,
        case_rules: Vec<CaseRule>,
        filter: &rule_ir::ProfileFilter,
    ) -> Self {
        let started = std::time::Instant::now();
        let _span = tracing::info_span!(
            "build_ac",
            spelling_rule_count = spelling_rules.len() as u64,
            case_rule_count = case_rules.len() as u64
        )
        .entered();
        let case_rules: Vec<CaseRule> = case_rules.into_iter().filter(|r| !r.disabled).collect();

        // Build segmenter from the FULL rule set (before profile filtering) so
        // word-boundary vocabulary is not lost when variant/ai_filler rules are
        // excluded from the AC automaton.
        let segmenter = Segmenter::from_rules(&spelling_rules);
        let guards = rule_ir::GuardRules::build(&spelling_rules);

        // Infallible: every automaton build inside logs and falls back to a
        // working alternative rather than propagating. This used to be a match
        // with an Err arm that substituted an empty database, which could never
        // fire and would have been the wrong recovery if it had: a scanner with
        // no spelling rules reports nothing, so the failure would have surfaced
        // as a clean bill of health.
        let spelling_db = rule_ir::compile_spelling_rules_filtered(spelling_rules, filter);

        let case_patterns: Vec<String> = case_rules.iter().map(|r| r.term.to_lowercase()).collect();

        let case_ac = match AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .ascii_case_insensitive(true)
            .build(&case_patterns)
        {
            Ok(ac) => Some(ac),
            Err(e) => {
                tracing::warn!("case AC build failed: {e}");
                None
            }
        };
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "build_ac completed"
        );

        Self {
            spelling_db,
            case_ac,
            case_rules,
            segmenter,
            build_filter: *filter,
            guards,
        }
    }

    /// Access the internal segmenter for context-clue analysis.
    pub fn segmenter(&self) -> &Segmenter {
        &self.segmenter
    }

    /// Force the scanner to use the bytewise AC fallback path for testing.
    /// Disables charwise and builds bytewise if not already present.
    #[cfg(test)]
    fn force_bytewise(&mut self) {
        if self.spelling_db.ac_bytewise.is_none() {
            let patterns: Vec<&str> = self
                .spelling_db
                .spelling_rules
                .iter()
                .map(|r| r.from.as_str())
                .chain(self.spelling_db.absorber_strings.iter().map(|s| s.as_str()))
                .collect();
            self.spelling_db.ac_bytewise = Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(&patterns)
                    .expect("build bytewise AC for test"),
            );
        }
        self.spelling_db.ac_charwise = None;
    }

    /// Scan text with Profile::Base and return all issues found.
    ///
    /// Applies NFC normalization, builds excluded ranges (including inline
    /// suppression markers), then scans and maps offsets back to the
    /// original text. Use scan_profiled for non-default profiles.
    pub fn scan(&self, text: &str) -> ScanOutput {
        self.scan_profiled(text, Profile::Base)
    }

    /// Scan text with the given profile and return all issues found.
    ///
    /// Uses pulldown-cmark for code block / inline code exclusion (handles
    /// both plain text and Markdown gracefully), plus regex-based exclusion
    /// for URLs, file paths, and @mentions.
    pub fn scan_profiled(&self, text: &str, profile: Profile) -> ScanOutput {
        self.scan_profiled_md(text, profile, true)
    }

    /// Scan with explicit control over Markdown structure exclusion.
    ///
    /// When use_markdown is true, pulldown-cmark detects code blocks (fenced
    /// and indented), inline code, and HTML -- matching Markdown input.
    /// When false, only content-pattern exclusions (URLs, paths, @mentions) and
    /// inline suppression markers are applied. Use false for plain text to
    /// avoid 4-space-indented paragraphs being falsely excluded as code.
    pub fn scan_profiled_md(&self, text: &str, profile: Profile, use_markdown: bool) -> ScanOutput {
        let content_type = if use_markdown {
            ContentType::Markdown
        } else {
            ContentType::Plain
        };
        self.scan_nfc_with_content_type(text, None, &[], profile.config(), content_type)
    }

    /// Scan YAML text with key-token exclusion.
    ///
    /// Excludes YAML key tokens (key name + colon) so that bare ASCII colons
    /// in key-value separators do not trigger false-positive colon warnings.
    /// YAML values after the colon are scanned normally as prose.
    pub fn scan_profiled_yaml(&self, text: &str, profile: Profile) -> ScanOutput {
        self.scan_nfc_with_content_type(text, None, &[], profile.config(), ContentType::Yaml)
    }

    /// Scan with NFC normalization, reusing pre-built excluded ranges.
    ///
    /// When the input text is already NFC (common case), the provided
    /// excluded ranges are used directly, avoiding a redundant
    /// recomputation of exclusion zones. When NFC normalization changes
    /// byte offsets, exclusions are rebuilt on the normalized text.
    ///
    /// content_type controls which structural exclusion pass is applied
    /// during the NFC-rebuild slow path (Markdown, YAML, or plain text).
    ///
    /// CALLER CONTRACT: config-driven Markdown options like
    /// [`ProfileConfig::exempt_blockquotes`] only take effect on
    /// the NFC-rebuild path, where exclusions are recomputed from the
    /// supplied [`ProfileConfig`].  On the fast path the caller-supplied
    /// `excluded` slice is used verbatim: if the caller wants
    /// blockquotes excluded, they must build the slice with
    /// [`build_exclusions_for_content_type_with_options`] using a
    /// matching [`MdScanOptions`].
    pub fn scan_with_prebuilt_excluded(
        &self,
        text: &str,
        excluded: &[ByteRange],
        profile: Profile,
        content_type: ContentType,
    ) -> ScanOutput {
        self.scan_nfc_with_content_type(text, Some(excluded), &[], profile.config(), content_type)
    }

    /// Like scan_with_prebuilt_excluded but with explicit ProfileConfig.
    pub fn scan_with_prebuilt_excluded_config(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: ProfileConfig,
        content_type: ContentType,
    ) -> ScanOutput {
        self.scan_nfc_with_content_type(text, Some(excluded), &[], cfg, content_type)
    }

    /// Scan text using the content-type-aware exclusion strategy.
    ///
    /// Shared entry point for CLI and MCP pipelines (20.4 deduplication).
    /// Dispatches to the appropriate scan method based on content type.
    pub fn scan_for_content_type(
        &self,
        text: &str,
        content_type: ContentType,
        profile: Profile,
    ) -> ScanOutput {
        self.scan_nfc_with_content_type(text, None, &[], profile.config(), content_type)
    }

    /// Scan with content-type-aware exclusions and explicit ProfileConfig.
    /// Use this when the caller needs to override individual config flags
    /// (e.g. detect_ai enabling density detection on the base profile).
    pub fn scan_for_content_type_with_config(
        &self,
        text: &str,
        content_type: ContentType,
        cfg: ProfileConfig,
    ) -> ScanOutput {
        self.scan_nfc_with_content_type(text, None, &[], cfg, content_type)
    }

    /// Scan with content-type exclusions plus ranges the caller already knows
    /// are not zh-TW prose.
    ///
    /// The browser extension is the caller that needs this: it flattens a page
    /// into one string and only it can see that a run sat under an ancestor
    /// declaring a non-Chinese lang.  Ranges are byte offsets into "text",
    /// need not be sorted, and are merged with the exclusions the engine
    /// builds for itself.  A range that is empty, inverted, or past the end of
    /// the text is dropped where they are merged in, so a stale offset from
    /// the page costs nothing.
    pub fn scan_for_content_type_with_extra_excluded(
        &self,
        text: &str,
        content_type: ContentType,
        cfg: ProfileConfig,
        extra_excluded: &[ByteRange],
    ) -> ScanOutput {
        self.scan_nfc_with_content_type(text, None, extra_excluded, cfg, content_type)
    }

    /// Core NFC-normalize → build exclusions → scan → remap pipeline.
    ///
    /// "caller_excluded" holds ranges the caller derived from something the
    /// engine cannot see, such as a lang attribute on a DOM ancestor of the
    /// text the browser extension flattened.  They arrive in "text" offsets
    /// and are mapped forward when normalization moved the bytes, so they are
    /// honored on both paths rather than only on the one that skips the
    /// rebuild.
    fn scan_nfc_with_content_type(
        &self,
        text: &str,
        prebuilt_excluded: Option<&[ByteRange]>,
        caller_excluded: &[ByteRange],
        cfg: ProfileConfig,
        content_type: ContentType,
    ) -> ScanOutput {
        let started = std::time::Instant::now();
        let _span = tracing::info_span!(
            "scan",
            content_length = text.len() as u64,
            content_type = content_type.name()
        )
        .entered();
        let norm = normalize_nfc(text);
        let scan_text = &norm.text;
        let nfc_changed = !norm.offset_map.is_empty();

        let mut output = match prebuilt_excluded {
            // Prebuilt ranges are measured against the text as handed in, so
            // normalization having moved the bytes retires them, and a caller
            // range is a second source this arm has nothing to merge with.
            Some(excl) if !nfc_changed && caller_excluded.is_empty() => {
                self.scan_with_config_content_type(scan_text, excl, cfg, content_type)
            }

            // Prebuilt ranges reach here only alongside caller ranges, which no
            // caller does today, so they are rebuilt rather than kept: the
            // rebuild is what the NFC path does anyway and it needs no second
            // branch to say so.
            _ => {
                let mut excl =
                    build_exclusions_for_content_type_with_config(scan_text, content_type, &cfg);
                if !caller_excluded.is_empty() {
                    // Clipping the end here rather than at the public wrapper
                    // keeps the bound next to the code that relies on it. A
                    // range that is empty, inverted, or wholly past the end
                    // then falls out of map_range_forward.
                    //
                    // The bound is the original length, not the normalized one,
                    // because the caller measured the range against the text it
                    // handed in. Composition makes the normalized text the
                    // shorter of the two, so clipping against it would cut a
                    // range that ends near the tail, and once the shrink
                    // exceeds the run's own length the range collapses and is
                    // dropped. The offset map's last entry is the original
                    // length, so this bound is also what keeps the mapped end
                    // inside the normalized text.
                    excl.extend(caller_excluded.iter().filter_map(|r| {
                        map_range_forward(&norm.offset_map, r.start, r.end.min(text.len()))
                            .map(|(start, end)| ByteRange { start, end })
                    }));

                    // Both sources are merged on their own; only mixing them
                    // can produce an overlap, which the binary search in
                    // is_excluded would then read wrong.
                    excl = merge_ranges_pub(excl);
                }
                self.scan_with_config_content_type(scan_text, &excl, cfg, content_type)
            }
        };

        if nfc_changed {
            remap_issues_to_original(&mut output.issues, text, &norm);
        }

        // Heading severity boost for Markdown content: issues inside headings
        // get +1 severity because heading text is high-visibility. Gated by
        // ProfileConfig::heading_severity_boost (default true).
        if cfg.heading_severity_boost
            && matches!(
                content_type,
                ContentType::Markdown | ContentType::MarkdownScanCode
            )
        {
            let heading_ranges =
                super::markdown::extract_heading_ranges(if nfc_changed { text } else { scan_text });
            if !heading_ranges.is_empty()
                && boost_heading_severity(&mut output.issues, &heading_ranges)
            {
                // Mutating severity can break the (offset asc, severity desc)
                // sort contract for issues sharing the same offset. Re-sort to
                // preserve the documented deterministic output order.
                output.issues.sort_by(|a, b| {
                    a.offset
                        .cmp(&b.offset)
                        .then(b.severity.cmp(&a.severity))
                        .then(a.rule_type.sort_order().cmp(&b.rule_type.sort_order()))
                });
            }
        }

        // Annotate issues that fall inside Markdown table cells with their
        // (row, col) coordinates for editor integration.
        if matches!(
            content_type,
            ContentType::Markdown | ContentType::MarkdownScanCode
        ) {
            let cell_spans = super::markdown::extract_table_cell_spans(if nfc_changed {
                text
            } else {
                scan_text
            });
            if !cell_spans.is_empty() {
                annotate_table_cells(&mut output.issues, &cell_spans);
            }
        }

        tracing::info!(
            issue_count = output.issues.len() as u64,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "scan completed"
        );
        output
    }

    /// Scan text using pre-built excluded ranges and a profile.
    ///
    /// Use this when the caller also needs the excluded ranges for a
    /// subsequent apply_fixes call, avoiding a redundant recomputation.
    ///
    /// excluded must be sorted by start position and non-overlapping
    /// (as returned by build_excluded_ranges). The is_excluded check
    /// uses binary search for large lists and will produce wrong results
    /// if ranges are unsorted.
    pub fn scan_with_excluded(
        &self,
        text: &str,
        excluded: &[ByteRange],
        profile: Profile,
    ) -> ScanOutput {
        self.scan_with_config(text, excluded, profile.config())
    }

    /// Scan with a fully-specified ProfileConfig (allows stance overrides).
    ///
    /// Allocates a fresh [`ScratchSpace`] internally.  For hot loops that
    /// process many documents, prefer [`scan_with_config_into`] with a
    /// reusable scratch buffer.
    pub fn scan_with_config(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: ProfileConfig,
    ) -> ScanOutput {
        self.scan_with_config_content_type(text, excluded, cfg, ContentType::Plain)
    }

    fn scan_with_config_content_type(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: ProfileConfig,
        content_type: ContentType,
    ) -> ScanOutput {
        let mut scratch = ScratchSpace::new();
        self.scan_with_config_into_content_type(text, excluded, cfg, content_type, &mut scratch)
    }

    /// Lexical and procedural passes: everything that matches on the text
    /// itself, before overlap resolution decides which spans survive.
    ///
    /// All of these emit issues in offset order, which is what lets the
    /// caller skip a sort in the common case.
    fn run_lexical_passes(
        &self,
        em: &mut Emitter<'_>,
        zh_type: ChineseType,
        cfg: &ProfileConfig,
        clue_index: &mut Vec<(usize, u16)>,
        boundary_bitmap: &BoundaryBitmap,
    ) {
        if cfg.spelling {
            self.scan_spelling(em, zh_type, cfg, clue_index, boundary_bitmap);
        }
        if cfg.casing {
            self.scan_case(em);
        }
        if cfg.basic_punctuation {
            self.scan_punctuation(em, cfg);
        }
        if cfg.dunhao_detection {
            self.scan_dunhao(em);
        }
        if cfg.range_normalization {
            self.scan_range_indicators(em, cfg);
        }
        if cfg.ellipsis_normalization {
            scan_ellipsis(em);
        }
        if cfg.basic_punctuation {
            self.scan_cn_curly_quotes(em);
            self.scan_spacing(em);
        }
        // Repetition detection (CJK duplicates + Latin duplicates).
        repetition::scan_repetition(em);
        // Spaced-acronym rejoining (C P U to CPU).
        acronym::scan_spaced_acronyms(em);
    }

    /// Catch a scan config that re-enables a rule type the scanner was built
    /// without. The rules are simply absent, so the pass would silently find
    /// nothing rather than fail, which reads as clean text.
    fn debug_assert_config_within_build(&self, cfg: &ProfileConfig) {
        debug_assert!(
            !(self.build_filter.exclude_variant && cfg.variant_normalization),
            "scan config enables variant_normalization but scanner was built without variant rules"
        );
        debug_assert!(
            !(self.build_filter.exclude_ai_filler && cfg.ai_filler_detection),
            "scan config enables ai_filler_detection but scanner was built without ai_filler rules"
        );
        debug_assert!(
            !(self.build_filter.exclude_translationese && cfg.translationese_detection),
            "scan config enables translationese_detection but scanner was built without translationese rules"
        );
    }

    /// Spelling rules this profile actually consults, for coverage reporting.
    ///
    /// Case and punctuation are procedural rather than discrete rules, so they
    /// do not count. One pass, subtracting each rule whose type the config
    /// gates off.
    fn count_active_spelling_rules(&self, cfg: &ProfileConfig) -> usize {
        if !cfg.spelling {
            return 0;
        }
        self.spelling_db
            .spelling_rules
            .iter()
            .filter(|r| match r.rule_type {
                RuleType::Variant => cfg.variant_normalization,
                RuleType::AiFiller => cfg.ai_filler_detection,
                RuleType::Translationese => cfg.translationese_detection,
                _ => true,
            })
            .count()
    }

    /// Scan plain text with a fully-specified ProfileConfig, reusing a
    /// caller-provided "ScratchSpace" to avoid per-scan allocations.
    ///
    /// This is the stable hot-loop API. Call
    /// "scan_with_config_into_content_type" when structural passes need a
    /// Markdown- or YAML-aware content type.
    pub fn scan_with_config_into(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: ProfileConfig,
        scratch: &mut ScratchSpace,
    ) -> ScanOutput {
        self.scan_with_config_into_content_type(text, excluded, cfg, ContentType::Plain, scratch)
    }

    /// Content-type-aware form of "scan_with_config_into".
    ///
    /// Kept separately named so adding content-type-aware structural passes
    /// does not source-break callers of the reusable-scratch API.
    ///
    /// The scratch buffers are cleared at entry; on return the issues live
    /// in the returned `ScanOutput` (moved out of `scratch.issues`).
    pub fn scan_with_config_into_content_type(
        &self,
        text: &str,
        excluded: &[ByteRange],
        cfg: ProfileConfig,
        content_type: ContentType,
        scratch: &mut ScratchSpace,
    ) -> ScanOutput {
        self.debug_assert_config_within_build(&cfg);

        // Counts document-scoped index builds for the duration of this scan;
        // see engine::index_guard. Compiled out of release builds.
        crate::engine::index_guard::reset();

        scratch.clear();

        let rules_checked = self.count_active_spelling_rules(&cfg);

        if text.is_empty() {
            return ScanOutput {
                issues: Vec::new(),
                detected_script: ChineseType::Unknown,
                ai_signature: None,
                translationese_signature: None,
                coverage: Some(CoverageReport {
                    rules_checked,
                    rules_matched: 0,
                }),
                oral_density: None,
                quality_flags: Vec::new(),
            };
        }

        // Fused single-pass: detect SC/TC type, build LineIndex, and optionally
        // build BoundaryBitmap -- shares one char_indices() iteration.
        let build_bitmap = cfg.spelling && text.len() > 4096;
        let (zh_type, line_index, boundary_bitmap) = detect_type_lineindex_and_bitmap(
            text,
            if build_bitmap {
                Some(&self.segmenter)
            } else {
                None
            },
        );

        // Destructure scratch to allow simultaneous mutable borrows of
        // independent fields (avoids borrow-checker conflict on &mut scratch).
        let ScratchSpace {
            ref mut issues,
            ref mut clue_index,
            ref mut overlap_order,
            ref mut overlap_keep,
            ref mut overlap_accepted,
        } = *scratch;

        let mut em = Emitter::new(text, excluded, issues);
        self.run_lexical_passes(&mut em, zh_type, &cfg, clue_index, &boundary_bitmap);

        // All scanners (AC, punctuation, spacing, ellipsis, quotes) emit issues
        // in offset order. Skip the O(n log n) sort when already sorted (common
        // case), falling back to sort only if the invariant breaks.
        let already_sorted = issues.windows(2).all(|w| {
            w[0].offset < w[1].offset || (w[0].offset == w[1].offset && w[0].length >= w[1].length)
        });
        if !already_sorted {
            issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(b.length.cmp(&a.length)));
        }

        // Remove overlapping issues: longer match wins; on tie, higher severity
        // wins. Handles both same-offset and cross-offset overlaps.
        overlap::resolve_overlaps_with_scratch(
            issues,
            overlap_order,
            overlap_keep,
            overlap_accepted,
        );

        // Inflate deferred spelling issues: fill in suggestions, context,
        // english, context_clues from the compiled DB. Only survivors of
        // overlap resolution get the full clone cost. Must run before
        // fix_quote_pairing which overwrites suggestions on CN quote issues. In
        // offset_only mode, skip context/english/context_clues (not
        // serialized). Count distinct spelling rules before inflate (which
        // clears spelling_rule_idx via take()).
        let rules_matched = {
            let mut seen = std::collections::HashSet::new();
            for issue in issues.iter() {
                if let Some(idx) = issue.spelling_rule_idx {
                    seen.insert(idx);
                }
            }
            seen.len()
        };

        if cfg.offset_only {
            rule_ir::inflate_spelling_issues_compact(&self.spelling_db, text, excluded, issues);
        } else {
            rule_ir::inflate_spelling_issues(&self.spelling_db, text, excluded, issues);
        }

        let mentions =
            run_structural_passes(text, excluded, issues, &cfg, content_type, &self.guards);

        // Fix CN quotation mark pairing with depth-based nesting: well-formed
        // quotes use character-based depth tracking; misordered or
        // all-same-char quotes fall back to positional alternation. Paragraph
        // breaks reset nesting depth.
        fix_quote_pairing(text, issues);

        // Validate structural nesting of existing TW bracket quotes: checks for
        // mismatched, interleaved, and unclosed quotes per paragraph.
        validate_quote_hierarchy(text, excluded, issues);

        // Compute AI signature score when any AI detection flag is active.
        let ai_signature = if cfg.ai_filler_detection
            || cfg.ai_semantic_safety
            || cfg.ai_density_detection
            || cfg.ai_structural_patterns
        {
            crate::engine::ai_score::compute_ai_score(
                text,
                issues,
                excluded,
                &mentions,
                cfg.ai_threshold_multiplier,
            )
        } else {
            None
        };

        // Compute translationese signature when detection is active.
        let translationese_signature = if cfg.translationese_detection {
            crate::engine::translationese_score::compute_translationese_score_with_domain(
                text,
                issues,
                excluded,
                cfg.translationese_domain,
            )
        } else {
            None
        };

        let oral_density = compute_oral_density(text);

        // No empty-issues shortcut here on purpose: with no issues the sort,
        // the line/col fill, and build_quality_flags are all no-ops and
        // rules_matched is already 0, so a special case would only be a second
        // copy of this same tail.
        //
        // Deterministic output contract: issues are sorted by byte offset
        // ascending, then severity descending, then rule_type discriminant for
        // stable, diffable output.
        issues.sort_by(|a, b| {
            a.offset
                .cmp(&b.offset)
                .then(b.severity.cmp(&a.severity))
                .then(a.rule_type.sort_order().cmp(&b.rule_type.sort_order()))
        });

        // Fill line/col coordinates AFTER the final sort so that the
        // linear-pass cursor correctly advances through offset-sorted issues.
        // Grammar/AI issues appended after overlap resolution are now in order.
        if !cfg.offset_only {
            line_index.fill_line_col_sorted(issues, ColumnEncoding::Utf16);
        }

        // Derive quality flags from issue composition.
        let mut quality_flags = build_quality_flags(issues);
        if oral_density.is_some_and(|d| d > 0.05) {
            quality_flags.push("high_oral_density".into());
        }

        crate::engine::index_guard::assert_built_once_per_document();

        ScanOutput {
            issues: std::mem::take(issues),
            detected_script: zh_type,
            ai_signature,
            translationese_signature,
            coverage: Some(CoverageReport {
                rules_checked,
                rules_matched,
            }),
            oral_density,
            quality_flags,
        }
    }
}

/// Grammar, AI, and translationese passes.
///
/// These run after overlap resolution, not with the lexical passes: a broad
/// grammar span such as 是不是…嗎 would otherwise swallow the narrower
/// spelling or case issue sitting inside its range.
fn run_structural_passes(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
    cfg: &ProfileConfig,
    content_type: ContentType,
    guards: &rule_ir::GuardRules,
) -> Vec<ByteRange> {
    // Resolved once for the document and handed to every detector that asks, so
    // that two detectors cannot disagree about the same page. Detecting it
    // costs a pass per anchor, so skip that when no consumer is enabled, the
    // same reason the boundary index below is built on demand.
    let register = if cfg.grammar_checks || cfg.translationese_detection {
        resolve_register(text, excluded, cfg.register)
    } else {
        Register::Casual
    };

    // One emitter for the whole stage: every check below reads this document
    // through this mask and writes into this list.
    let mut em = Emitter::new(text, excluded, issues);

    if cfg.grammar_checks {
        grammar::scan_grammar(&mut em, register);
    }

    // Build boundaries only when an AI structural or translationese detector
    // needs them. This costs one pass over the text.
    let needs_boundary_index =
        cfg.ai_structural_patterns || cfg.translationese_detection || cfg.rhythm;
    let boundary_index = if needs_boundary_index {
        Some(BoundaryIndex::build(text, excluded))
    } else {
        None
    };

    run_ai_filter(&mut em, cfg, content_type, boundary_index.as_ref(), guards);

    // The spans judged as mentions are handed back: the phrase-density signal
    // reads the text directly, so without them the score would still count a
    // phrase whose findings were just suppressed.
    let mentions = drop_mentioned_style_findings(em.text, em.issues);

    // Syntactic translationese detectors (G1-G8, Y1-Y2, S3, V7, V13).
    if cfg.translationese_detection {
        if let Some(ref idx) = boundary_index {
            grammar::scan_translationese_syntactic(&mut em, idx);

            // Boundary-aware translationese detectors (ZY1b/ZY2b/ZY3b/ZY5).
            // cfg.translationese_domain selects the per-domain threshold table
            // that drives firing behavior at scan time.
            grammar::scan_translationese_indexed(
                &mut em,
                idx,
                cfg.translationese_domain,
                cfg.rhythm,
                register,
            );
        }

        // Substring-only translationese detectors (ZY1a/ZY2a/ZY3a/ZY4a), which
        // need no boundary index.
        grammar::scan_translationese_lexical(&mut em, register);
        dedup_translationese_phase_duplicates(em.issues);
    }

    // Rhythm (氣口), advisory and opt-in. Independent of
    // translationese_detection on purpose: rhythm composes with any profile,
    // including one that has the translationese pass switched off.
    if cfg.rhythm {
        if let Some(ref idx) = boundary_index {
            grammar::scan_rhythm(&mut Emitter::new(text, excluded, issues), idx);
        }
    }

    mentions
}

/// Resolve automatic register detection from the same prose that scanners see.
///
/// Excluded spans are replaced rather than removed so an anchor on either side
/// cannot join across code, markup, or a suppression range.
fn resolve_register(
    text: &str,
    excluded: &[ByteRange],
    mode: crate::rules::ruleset::RegisterMode,
) -> Register {
    if mode != crate::rules::ruleset::RegisterMode::Auto {
        return mode.resolve(text);
    }
    if excluded.is_empty() {
        return mode.resolve(text);
    }

    let mut visible = String::with_capacity(text.len());
    let mut range = 0;
    for (at, ch) in text.char_indices() {
        while range < excluded.len() && excluded[range].end <= at {
            range += 1;
        }
        if excluded
            .get(range)
            .is_some_and(|span| span.start <= at && at < span.end)
        {
            visible.push(if ch.is_whitespace() { ch } else { ' ' });
        } else {
            visible.push(ch);
        }
    }
    mode.resolve(&visible)
}

/// Drop style findings that name a phrase rather than use it.
///
/// A document about writing quotes the words it is warning against, and a
/// checklist asks whether they appear. Reporting those is noise, and fixing
/// them is destruction: "檢查有沒有「值得注意的是」" became
/// "檢查有沒有「」" under the delete sentinel.
///
/// Both reference skills carry this as a founding rule, in the same words:
/// 被討論的詞一律放行, 那是在提及這個詞，不是在使用它.
///
/// Scoped to AiStyle on purpose. A quoted zh-CN term is a different question:
/// the reader may still want to know the source wrote 視頻, so the
/// cross-strait family keeps reporting inside quotes.
///
/// The test is per line, because an unclosed quote must not silence the rest
/// of the document, and a Markdown task-list line is covered whole: its entire
/// point is to name the thing being looked for.
fn drop_mentioned_style_findings(text: &str, issues: &mut Vec<Issue>) -> Vec<ByteRange> {
    // One forward sweep rather than a lookup per issue. Finding the line by
    // scanning back from each offset, then counting quote marks before it, is
    // quadratic in line length: a 1.5 MB document written as one paragraph,
    // which unwrapped Chinese prose routinely is, took 50 seconds on an
    // ordinary lint. This walks the text once and answers every issue on a line
    // from counters carried along it.
    let mut by_offset: Vec<usize> = (0..issues.len())
        .filter(|&i| is_mention_candidate(&issues[i]))
        .collect();
    if by_offset.is_empty() {
        return Vec::new();
    }

    // Issues arrive in offset order for the lexical passes, but nothing
    // guarantees it across passes, so sort the candidate view rather than
    // assume.
    by_offset.sort_unstable_by_key(|&idx| issues[idx].offset);

    let mut drop = vec![false; issues.len()];
    let mut next = 0usize;
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let line_end = line_start + line.len();
        // Every candidate that starts on this line.
        let first = next;
        while next < by_offset.len() && issues[by_offset[next]].offset < line_end {
            next += 1;
        }
        if first < next {
            if is_mention_marker_line(line) {
                for &idx in &by_offset[first..next] {
                    drop[idx] = true;
                }
            } else {
                mark_quoted_on_line(line, line_start, &by_offset[first..next], issues, &mut drop);
            }
        }
        line_start = line_end;
        if next == by_offset.len() {
            break;
        }
    }

    // The spans that were mentions, in offset order, for the phrase-density
    // signal, which reads the text rather than this list.
    let spans: Vec<ByteRange> = by_offset
        .iter()
        .filter(|&&idx| drop[idx])
        .map(|&idx| ByteRange {
            start: issues[idx].offset,
            end: issues[idx].offset + issues[idx].length,
        })
        .collect();

    // "is_excluded" binary searches once past ten ranges and relies on them
    // being disjoint, so two findings on one quoted phrase could otherwise hide
    // a third between them. This is the builder every other producer of
    // exclusion-shaped ranges ends in.
    let mentions = merge_ranges_pub(spans);

    // Consume the mask alongside retain's in-order visit, so no index can drift
    // out of step with it.
    let mut mask = drop.into_iter();
    issues.retain(|_| !mask.next().unwrap_or(false));
    mentions
}

/// Whether a finding could be a phrase named rather than used.
///
/// Only a lexical phrase match can be. A structural finding measures shape
/// over a whole document and is merely anchored somewhere:
/// 全文混用「你」與「您」
/// points at the first 你, and when that one sat in a quotation the
/// document-wide finding disappeared. An invisible character has no mention
/// reading at all, since writing about hidden characters does not put one in
/// the text; dropping those silenced the layer that closes the
/// hidden-instruction channel.
fn is_mention_candidate(issue: &Issue) -> bool {
    issue.rule_type == IssueType::AiStyle
        && issue.length > 0
        && issue.structural_family.is_none()
        && !issue
            .found
            .chars()
            .all(crate::engine::ai_score::is_zero_width_candidate)
}

#[cfg(test)]
mod mention_tests {
    use super::*;

    // Every blank-line form has to split, including the mixed ones a patch or a
    // merge leaves behind. Matching only "\n\n" and "\r\n\r\n" let a mixed
    // document read as one paragraph, and the paragraph-level detectors
    // returned under their minimum count without a word.
    #[test]
    fn a_blank_line_splits_whatever_terminators_it_uses() {
        for sep in ["\n\n", "\r\n\r\n", "\n\r\n", "\r\n\n"] {
            let doc = format!("第一段。{sep}第二段。{sep}第三段。");
            let paras = split_paragraphs(&doc);
            assert_eq!(
                paras.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
                ["第一段。", "第二段。", "第三段。"],
                "separator {sep:?}"
            );
        }
    }

    // The phrase-density signal reads the text, not the issue list, so a
    // document that quotes a tell had every finding suppressed and still scored
    // for the phrase: "No issues found" beside "AI score: 0.92".
    #[test]
    fn a_quoted_phrase_scores_no_higher_than_its_absence() {
        let body = "這裡示範一個常見的寫作毛病。".repeat(30);
        let quoted = "避免使用「值得注意的是」這個詞。".repeat(8);
        let used = "值得注意的是，這個設計很好。".repeat(8);

        let score = |doc: &str| {
            let scanner = Scanner::new(
                crate::rules::loader::load_embedded_ruleset()
                    .unwrap()
                    .spelling_rules,
                Vec::new(),
            );
            let mut cfg = Profile::Base.config();
            cfg.ai_filler_detection = true;
            cfg.ai_density_detection = true;
            cfg.ai_structural_patterns = true;
            cfg.ai_semantic_safety = true;
            scanner
                .scan_with_config(doc, &[], cfg)
                .ai_signature
                .map_or(0.0, |r| r.score)
        };

        let quoting = score(&format!("{body}\n\n{quoted}\n\n{body}"));
        let using = score(&format!("{body}\n\n{used}\n\n{body}"));
        assert!(
            quoting < using,
            "quoting a tell must score below using it: {quoting} vs {using}"
        );
    }
}

/// Whether a line labels a phrase rather than using one.
///
/// Two markers mean it. A task-list item names the thing being looked for, and
/// a ❌/✅ pair marks a specimen, which is how every zh-TW writing guide
/// vendored here shows the wrong version of a sentence. Both make the phrase a
/// mention, so a document that teaches people to delete 值得注意的是 is not
/// reported for containing it.
///
/// One predicate rather than one per marker. They were two, and the two
/// disagreed: the specimen test peeled list and blockquote markers first, the
/// task-list test did not, so a blockquoted checkbox reported while a
/// blockquoted ❌ on the next line did not. Peeling once and widening the
/// marker set makes that a data question instead of a second function.
fn is_mention_marker_line(line: &str) -> bool {
    let mut rest = line.trim_start();

    // Peel containers, innermost last: "> - ❌ …" is still a specimen. Each
    // pass must consume something, so this terminates.
    loop {
        let stripped = if let Some(after) = rest.strip_prefix('>') {
            after
        } else if grammar::is_bullet_item(rest) {
            &rest[2..]
        } else if let Some(len) = grammar::numbered_list_marker_len(rest) {
            &rest[len..]
        } else {
            break;
        };
        rest = stripped.trim_start();
    }

    // A checkbox, or a specimen mark. Anything else is ordinary prose, and a
    // marker used mid-sentence is being used rather than labelling.
    matches!(rest.as_bytes(), [b'[', b' ' | b'x' | b'X', b']', ..])
        || rest.starts_with(['\u{274C}', '\u{2705}', '\u{2713}', '\u{2717}'])
}

/// Mark which of a line's candidates sit inside a quotation.
///
/// One pass over the line rather than one per candidate. The old form counted
/// quote marks in the prefix for every issue, so a line carrying thousands of
/// findings rescanned it thousands of times.
///
/// Depth counting rather than pairing, so a nested 「…『…』…」 still reads as
/// open. Straight double quotes have no distinct closer, so they go by parity.
/// A span counts as quoted only if the quotation also closes after it, which
/// is what keeps an unclosed quote from silencing the rest of the line.
fn mark_quoted_on_line(
    line: &str,
    line_start: usize,
    candidates: &[usize],
    issues: &[Issue],
    drop: &mut [bool],
) {
    // A span is enclosed only if a closer follows it, which is true exactly
    // when the last closer is at or after its end. Collecting every closer to
    // ask that would put a linear scan inside the per-character loop, which is
    // the shape this rewrite exists to remove.
    let last_closer = line.rfind(['」', '』']);
    let last_ascii = line.rfind('"');
    if last_closer.is_none() && last_ascii.is_none() {
        return;
    }

    let mut cand = candidates.iter().peekable();
    let mut depth = 0i32;
    let mut ascii_open = false;
    for (i, ch) in line.char_indices() {
        // Answer every candidate that starts here, before this character is
        // counted: a quote mark at the span's own start is not enclosing it.
        while let Some(&&idx) = cand.peek() {
            let rel = issues[idx].offset - line_start;
            if rel != i {
                break;
            }
            cand.next();
            let end = rel + issues[idx].length;
            drop[idx] = if depth > 0 {
                last_closer.is_some_and(|c| c >= end)
            } else {
                ascii_open && last_ascii.is_some_and(|c| c >= end)
            };
        }
        match ch {
            '「' | '『' => depth += 1,
            '」' | '』' => depth -= 1,
            '"' => ascii_open = !ascii_open,
            _ => {}
        }
        if cand.peek().is_none() {
            return;
        }
    }
}

/// Run procedural AI detectors while retaining each profile switch as its own
/// gate. Rule-backed lexical fillers stay in the lexical pass so rule packs
/// and overrides apply. Semantic, density, and structural checks stay separate
/// because their false-positive tradeoffs differ.
fn run_ai_filter(
    em: &mut Emitter<'_>,
    cfg: &ProfileConfig,
    content_type: ContentType,
    boundary_index: Option<&BoundaryIndex>,
    guards: &rule_ir::GuardRules,
) {
    if cfg.ai_semantic_safety {
        grammar::scan_ai_grammar(em);

        // A style tell, not a grammar error, so it must neither ride along on
        // "grammar_checks" (on by default in every profile) nor vanish with it
        // ("--relaxed" clears it).
        grammar::scan_ai_bare_attribution(
            em,
            cfg.document_genre,
            guards.get("uncited_attribution"),
        );
    }

    if cfg.ai_structural_patterns {
        grammar::scan_ai_structural(em, cfg.ai_threshold_multiplier);
    }

    // Invisible characters are not a structural pattern and never were: they
    // rode along inside that pass because that is where the call sat. The score
    // counts them whenever any AI stage is on, so gating the findings on one
    // stage left a caller able to see a zero-width count with no issue to fix.
    // Same condition as the score.
    if cfg.ai_filler_detection
        || cfg.ai_semantic_safety
        || cfg.ai_density_detection
        || cfg.ai_structural_patterns
    {
        grammar::scan_ai_zero_width(em);
    }

    if cfg.ai_density_detection {
        grammar::scan_ai_density(em, cfg.ai_threshold_multiplier);
    }

    if cfg.ai_structural_patterns {
        if let Some(idx) = boundary_index {
            grammar::scan_ai_structural_phase2(em, idx, content_type);
        }
    }
}

/// Oral marker characters and phrases common in spoken Chinese.
const ORAL_MARKERS: &[&str] = &[
    "嗯",
    "啊",
    "呢",
    "吧",
    "哦",
    "喔",
    "欸",
    "哎",
    "唉",
    "嘛",
    "齁",
    "蛤",
    "咧",
    "啦",
    "耶",
    "哇",
    "呀",
    "喂",
    "就是",
    "其實",
    "然後",
    "所以說",
    "基本上",
    "對不對",
    "就是說",
    "怎麼說",
    "那個",
    "這個",
];

/// Compute oral density: ratio of oral/filler marker chars to total CJK chars.
/// Returns None if fewer than 20 CJK characters (too short to be meaningful).
fn compute_oral_density(text: &str) -> Option<f32> {
    let mut cjk_count = 0u32;
    for ch in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            cjk_count += 1;
        }
    }
    if cjk_count < 20 {
        return None;
    }

    // Collect byte ranges of all marker hits, then union them to avoid
    // double-counting overlaps (e.g. "就是" inside "就是說").
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for marker in ORAL_MARKERS {
        for (start, matched) in text.match_indices(marker) {
            spans.push((start, start + matched.len()));
        }
    }
    spans.sort_unstable();
    // Merge overlapping spans and count CJK chars in merged ranges.
    let mut marker_chars = 0u32;
    let mut cur_end = 0usize;
    for (s, e) in spans {
        let s = s.max(cur_end); // clip to avoid double-count
        if s < e {
            for ch in text[s..e].chars() {
                if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
                    marker_chars += 1;
                }
            }
            cur_end = e;
        }
    }
    Some(marker_chars as f32 / cjk_count as f32)
}

/// Derive quality signal flags from issue composition.
///
/// Signals are additive strings that downstream consumers can check.
/// An empty vec means no notable quality signals were detected.
fn build_quality_flags(issues: &[Issue]) -> Vec<String> {
    let mut flags = Vec::new();
    let mut has_confusable = false;
    let mut has_repetition = false;
    let mut has_spaced_acronym = false;

    for issue in issues {
        match issue.rule_type {
            // Only flag as ASR artifact if rule context explicitly says so.
            IssueType::Confusable if issue.context.as_ref().is_some_and(|c| c.contains("ASR")) => {
                has_confusable = true;
            }
            IssueType::Repetition => {
                if is_spaced_acronym_issue(issue) {
                    has_spaced_acronym = true;
                } else {
                    has_repetition = true;
                }
            }
            _ => {}
        }
    }

    if has_confusable {
        flags.push("asr_artifacts".into());
    }
    if has_repetition {
        flags.push("stutter_detected".into());
    }
    if has_spaced_acronym {
        flags.push("spaced_acronyms".into());
    }
    flags
}

pub(crate) fn is_spaced_acronym_issue(issue: &Issue) -> bool {
    if issue.rule_type != IssueType::Repetition || issue.suggestions.len() != 1 {
        return false;
    }
    let joined = &issue.suggestions[0];
    if !joined.bytes().all(|b| b.is_ascii_uppercase()) {
        return false;
    }
    let parts: Vec<&str> = issue.found.split(' ').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| part.len() == 1 && part.as_bytes()[0].is_ascii_uppercase())
        && *joined == parts.concat()
}

// Tests

#[cfg(test)]
mod tests {
    use super::overlap::resolve_overlaps;
    use super::*;
    use crate::rules::ruleset::RuleType;

    fn sample_spelling_rules() -> Vec<SpellingRule> {
        vec![
            SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
            SpellingRule::new("內存", vec!["記憶體".into()], RuleType::CrossStrait),
            SpellingRule::new("服務器", vec!["伺服器".into()], RuleType::CrossStrait),
        ]
    }

    fn sample_case_rules() -> Vec<CaseRule> {
        vec![
            CaseRule {
                term: "JavaScript".into(),
                alternatives: Some(vec!["javascript".into(), "JAVASCRIPT".into()]),
                disabled: false,
            },
            CaseRule {
                term: "TypeScript".into(),
                alternatives: None,
                disabled: false,
            },
            CaseRule {
                term: "API".into(),
                alternatives: Some(vec!["Api".into(), "api".into(), "APIs".into()]),
                disabled: false,
            },
        ]
    }

    /// Select by detector family, which is now a field rather than a code
    /// spelled into the human-readable message.
    fn translationese_issues(issues: &[Issue], family: PhaseFamily) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|issue| issue.phase_family.is_some_and(|(f, _)| f == family))
            .collect()
    }

    #[test]
    fn basic_spelling_detection() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("這個軟件很好用").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[0].suggestions[..], vec!["軟體"]);
        assert_eq!(issues[0].rule_type, IssueType::CrossStrait);
    }

    #[test]
    fn reusable_scratch_plain_api_remains_available() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let cfg = Profile::Base.config();
        let mut scratch = ScratchSpace::new();

        // This four-argument call is the public hot-loop API. Keep it as a
        // compile-time compatibility test while content-aware callers use the
        // separately named variant.
        let legacy = scanner.scan_with_config_into("這個軟件很好用", &[], cfg, &mut scratch);
        let mut content_scratch = ScratchSpace::new();
        let content_aware = scanner.scan_with_config_into_content_type(
            "這個軟件很好用",
            &[],
            cfg,
            ContentType::Plain,
            &mut content_scratch,
        );

        assert_eq!(legacy.issues.len(), 1);
        assert_eq!(legacy.issues[0].found, "軟件");
        assert_eq!(
            legacy
                .issues
                .iter()
                .map(|issue| (&issue.offset, &issue.found))
                .collect::<Vec<_>>(),
            content_aware
                .issues
                .iter()
                .map(|issue| (&issue.offset, &issue.found))
                .collect::<Vec<_>>()
        );
    }

    // The mention filter answers "is this phrase named or used", a question an
    // invisible character cannot be on either side of. Scoping it to every
    // AiStyle finding silenced the invisible-character layer inside a quotation
    // and on a task-list line, which is where a hidden-instruction payload
    // would sit.
    #[test]
    fn quoting_does_not_hide_an_invisible_character() {
        let scanner = Scanner::new(vec![], vec![]);
        let mut cfg = Profile::Base.config();
        cfg.ai_structural_patterns = true;
        let text = "注意「零寬\u{200B}空格」。\n- [ ] 檢查\u{200B}殘留\n零寬\u{200B}空格在外面。\n";
        let issues = scanner
            .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
            .issues;
        assert_eq!(
            issues
                .iter()
                .filter(|issue| issue.found == "\u{200B}")
                .count(),
            3,
            "every zero-width space must be reported: {issues:?}"
        );
    }

    // A document-level finding is anchored on one occurrence, so scoping the
    // mention filter to it made the whole finding hostage to that line: the
    // same document reported the mixed address with a bare 你 and said nothing
    // once the first 你 appeared in a quotation.
    #[test]
    fn quoting_one_occurrence_does_not_hide_a_document_wide_finding() {
        let scanner = Scanner::new(vec![], vec![]);
        let mut cfg = Profile::Base.config();
        cfg.ai_structural_patterns = true;
        for text in [
            "手冊裡寫你這個字，但這裡一律用您。您可以參考說明。",
            "手冊裡寫「你」這個字，但這裡一律用您。您可以參考說明。",
        ] {
            let issues = scanner
                .scan_for_content_type_with_config(text, ContentType::Plain, cfg)
                .issues;
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.context.as_deref().is_some_and(|c| c.contains("混用"))),
                "mixed reader address lost for {text}: {issues:?}"
            );
        }
    }

    #[test]
    fn multiple_spelling_issues() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("這個軟件的服務器內存不夠").issues;
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[1].found, "服務器");
        assert_eq!(issues[2].found, "內存");
    }

    #[test]
    fn spelling_in_code_fence_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner.scan("請看 `軟件` 的說明").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn spelling_in_url_excluded() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        let issues = scanner
            .scan("https://example.com/軟件/download 是連結")
            .issues;
        assert_eq!(
            issues.len(),
            0,
            "CJK inside URL path should be excluded: {issues:?}"
        );
    }

    #[test]
    fn case_rule_basic() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("I use Javascript for work").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "Javascript");
        assert_eq!(issues[0].suggestions[..], vec!["JavaScript"]);
        assert_eq!(issues[0].rule_type, IssueType::Case);
    }

    #[test]
    fn case_rule_correct_form_no_issue() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("I use JavaScript for work").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn case_rule_alternative_no_issue() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("I use javascript for work").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn case_rule_word_boundary() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("This is Unreactive").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn case_rule_in_code_excluded() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("Use `typescript` in your code").issues;
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn mixed_spelling_and_case() {
        let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
        let issues = scanner.scan("這個軟件用 typescript 寫的").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[1].found, "typescript");
    }

    #[test]
    fn empty_text() {
        let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
        let issues = scanner.scan("").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn clean_text_no_issues() {
        let scanner = Scanner::new(sample_spelling_rules(), sample_case_rules());
        let issues = scanner.scan("這個軟體用 TypeScript 寫的").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn api_case_wrong() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        let issues = scanner.scan("This aPi is slow").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "aPi");
        assert_eq!(issues[0].suggestions[..], vec!["API"]);
    }

    #[test]
    fn api_case_correct_alternatives() {
        let scanner = Scanner::new(vec![], sample_case_rules());
        assert!(scanner.scan("The API is fast").issues.is_empty());
        assert!(scanner.scan("The Api is fast").issues.is_empty());
        assert!(scanner.scan("The api is fast").issues.is_empty());
    }

    // Spelling AC (charwise / bytewise) tests

    #[test]
    fn charwise_ac_is_built_for_cjk_patterns() {
        let scanner = Scanner::new(sample_spelling_rules(), vec![]);
        assert!(
            scanner.spelling_db.ac_charwise.is_some(),
            "charwise AC should be built for CJK-only patterns"
        );
    }

    #[test]
    fn charwise_and_bytewise_produce_identical_results() {
        let rules = sample_spelling_rules();
        let text = "這個軟件的服務器內存不夠，需要升級軟件的記憶體";
        let scanner = Scanner::new(rules.clone(), vec![]);

        // Run with charwise (default path).
        let charwise_issues = scanner.scan(text).issues;

        // Force bytewise path for comparison.
        let mut bytewise_scanner = Scanner::new(rules, vec![]);
        bytewise_scanner.force_bytewise();
        let bytewise_issues = bytewise_scanner.scan(text).issues;

        assert_eq!(
            charwise_issues.len(),
            bytewise_issues.len(),
            "charwise and bytewise should find the same number of issues"
        );
        for (cw, bw) in charwise_issues.iter().zip(bytewise_issues.iter()) {
            assert_eq!(cw.offset, bw.offset, "offsets must match");
            assert_eq!(cw.length, bw.length, "lengths must match");
            assert_eq!(cw.found, bw.found, "found text must match");
            assert_eq!(cw.suggestions, bw.suggestions, "suggestions must match");
        }
    }

    #[test]
    fn spaced_acronym_sets_quality_flag_without_stutter() {
        let scanner = Scanner::new(vec![], vec![]);
        let output = scanner.scan("使用 C P U 架構處理工作負載");
        assert!(output.quality_flags.iter().any(|f| f == "spaced_acronyms"));
        assert!(!output.quality_flags.iter().any(|f| f == "stutter_detected"));
    }

    #[test]
    fn repetition_sets_stutter_flag() {
        let scanner = Scanner::new(vec![], vec![]);
        let output = scanner.scan("去去來來看看這個結果");
        assert!(output.quality_flags.iter().any(|f| f == "stutter_detected"));
    }

    #[test]
    fn clean_high_oral_density_text_keeps_document_flag() {
        let scanner = Scanner::new(vec![], vec![]);
        let output = scanner.scan("這個那個這個那個這個那個這個那個這個那個");
        assert!(output.issues.is_empty());
        assert_eq!(output.oral_density, Some(1.0));
        assert!(output
            .quality_flags
            .iter()
            .any(|f| f == "high_oral_density"));
    }

    #[test]
    fn charwise_leftmost_longest_on_overlapping_patterns() {
        // "數據" and "數據庫" overlap: leftmost-longest must pick "數據庫".
        let rules = vec![
            SpellingRule::new("數據", vec!["資料".into()], RuleType::CrossStrait),
            SpellingRule::new("數據庫", vec!["資料庫".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        let issues = scanner.scan("這個數據庫很大").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "數據庫");
        assert_eq!(issues[0].suggestions[..], vec!["資料庫"]);
    }

    #[test]
    fn charwise_single_char_cjk_pattern() {
        // Single CJK character pattern: shortest possible charwise match.
        let rules = vec![SpellingRule::new(
            "裏",
            vec!["裡".into()],
            RuleType::Variant,
        )];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        let issues = scanner.scan_profiled("裏面有東西", Profile::Strict).issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "裏");
        assert_eq!(issues[0].suggestions[..], vec!["裡"]);
    }

    #[test]
    fn charwise_mixed_cjk_ascii_patterns() {
        // Patterns with both CJK and ASCII characters.
        let rules = vec![
            SpellingRule::new("IP地址", vec!["IP 位址".into()], RuleType::CrossStrait),
            SpellingRule::new(
                "CPU使用率",
                vec!["CPU 使用率".into()],
                RuleType::CrossStrait,
            ),
        ];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        let issues = scanner.scan("查看IP地址和CPU使用率").issues;
        let spelling: Vec<_> = issues
            .iter()
            .filter(|i| i.rule_type == IssueType::from(RuleType::CrossStrait))
            .collect();
        assert_eq!(spelling.len(), 2);
        assert_eq!(spelling[0].found, "IP地址");
        assert_eq!(spelling[1].found, "CPU使用率");
    }

    /// Build a one-rule scanner whose rule carries context-selected groups.
    fn context_suggestion_scanner(
        groups: Option<Vec<crate::rules::ruleset::ContextSuggestion>>,
    ) -> Scanner {
        let rules = vec![SpellingRule {
            context_suggestions: groups,
            ..SpellingRule::new("優化", vec!["最佳化".into()], RuleType::CrossStrait)
        }];
        Scanner::new(rules, vec![])
    }

    fn only_suggestions(scanner: &Scanner, text: &str) -> Vec<String> {
        let issues = scanner.scan_profiled(text, Profile::Base).issues;
        assert_eq!(issues.len(), 1, "expected exactly one issue for {text:?}");
        issues[0].suggestions.to_vec()
    }

    #[test]
    fn context_suggestions_select_by_window() {
        use crate::rules::ruleset::ContextSuggestion;
        let scanner = context_suggestion_scanner(Some(vec![ContextSuggestion {
            clues: vec!["流程".into(), "服務".into()],
            to: vec!["改善".into(), "提升".into()],
        }]));

        // A clue in the window swaps the whole replacement set.
        assert_eq!(only_suggestions(&scanner, "優化流程"), ["改善", "提升"]);
        // Clue before the match counts too: the window spans both sides.
        assert_eq!(only_suggestions(&scanner, "服務要優化"), ["改善", "提升"]);
        // No clue nearby falls back to the rule default.
        assert_eq!(only_suggestions(&scanner, "優化演算法"), ["最佳化"]);
    }

    #[test]
    fn context_suggestions_first_matching_group_wins() {
        use crate::rules::ruleset::ContextSuggestion;
        // Both groups match "流程效能"; ruleset order decides.
        let scanner = context_suggestion_scanner(Some(vec![
            ContextSuggestion {
                clues: vec!["流程".into()],
                to: vec!["改善".into()],
            },
            ContextSuggestion {
                clues: vec!["效能".into()],
                to: vec!["最佳化".into()],
            },
        ]));
        assert_eq!(only_suggestions(&scanner, "優化流程效能"), ["改善"]);
    }

    #[test]
    fn context_suggestions_drop_unusable_groups() {
        use crate::rules::ruleset::ContextSuggestion;

        // An empty clue list can never select and an empty replacement list
        // would erase the default, so both are dropped at compile time and the
        // rule behaves as if it had no groups at all. A list holding an empty
        // string is the same case: the empty string is the deletion sentinel,
        // so keeping it would turn a substitution into a deletion.
        //
        // The last group is the one that matters. Filtering the empty entry out
        // and keeping "改善" would leave a one-entry group, and one entry is
        // auto-fixable, so a malformed group would quietly gain the write
        // permission the author's two candidates were meant to deny. Drop the
        // group instead of repairing it.
        let scanner = context_suggestion_scanner(Some(vec![
            ContextSuggestion {
                clues: vec![],
                to: vec!["改善".into()],
            },
            ContextSuggestion {
                clues: vec!["流程".into()],
                to: vec![],
            },
            ContextSuggestion {
                clues: vec!["服務".into()],
                to: vec![String::new()],
            },
            ContextSuggestion {
                clues: vec!["品質".into()],
                to: vec!["改善".into(), String::new()],
            },
            // An empty clue is the dangerous one: "window.contains(\"\")" is
            // true for every window, so a single stray entry makes the group
            // the answer for every match of the rule, anywhere, with no clue
            // present at all. Dropped like the rest.
            ContextSuggestion {
                clues: vec![String::new()],
                to: vec!["永遠".into()],
            },
        ]));
        assert_eq!(only_suggestions(&scanner, "優化流程"), ["最佳化"]);
        assert_eq!(only_suggestions(&scanner, "優化服務"), ["最佳化"]);
        assert_eq!(only_suggestions(&scanner, "優化品質"), ["最佳化"]);
        assert_eq!(
            only_suggestions(&scanner, "完全無關的內容優化在這裡"),
            ["最佳化"]
        );
        assert!(scanner
            .spelling_db
            .spelling_context_suggestions
            .iter()
            .all(Option::is_none));
    }

    #[test]
    fn context_suggestions_dropped_on_deletion_rules() {
        use crate::rules::ruleset::ContextSuggestion;

        // Inflation derives the reported span from the rule's own "to": for a
        // deletion rule it uses from.len() so the user sees the phrase to
        // delete rather than any punctuation the span absorbed. A group
        // offering a real replacement would therefore report a span shorter
        // than the one it rewrites, so the combination is refused.
        let rules = vec![SpellingRule {
            context_suggestions: Some(vec![ContextSuggestion {
                clues: vec!["流程".into()],
                to: vec!["請注意".into()],
            }]),
            ..SpellingRule::new("值得注意的是", vec![String::new()], RuleType::AiFiller)
        }];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner
            .spelling_db
            .spelling_context_suggestions
            .iter()
            .all(Option::is_none));
    }

    #[test]
    fn context_suggestions_stop_at_paragraph_breaks() {
        use crate::rules::ruleset::ContextSuggestion;

        // The selection window is the clue gate's window, which stops at a
        // paragraph break. Without that clamp a heading or an adjacent
        // paragraph within 40 chars silently swaps the replacement set, and
        // because the business group carries two entries it also disables
        // auto-fix for a match that is squarely in the IT sense.
        let scanner = context_suggestion_scanner(Some(vec![ContextSuggestion {
            clues: vec!["流程".into()],
            to: vec!["改善".into(), "提升".into()],
        }]));

        assert_eq!(
            only_suggestions(&scanner, "我們要優化演算法。\n\n流程改造報告"),
            ["最佳化"]
        );
        // Same clue, same distance, no break: the group still selects.
        assert_eq!(
            only_suggestions(&scanner, "我們要優化演算法。流程改造報告"),
            ["改善", "提升"]
        );
    }

    #[test]
    fn charwise_exception_phrase_respected() {
        // Exception phrases must work identically on both AC paths.
        let rules = vec![SpellingRule {
            exceptions: Some(vec!["下著".into()]),
            ..SpellingRule::new("著", vec!["著".into()], RuleType::Variant)
        }];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        // "下著" is an exception: should not fire.
        let issues = scanner.scan_profiled("下著棋", Profile::Strict).issues;
        assert!(
            issues.is_empty(),
            "exception phrase '下著' should suppress the match: {issues:?}"
        );
    }

    #[test]
    fn charwise_context_clues_gate() {
        // Context clues must gate correctly on the charwise path.
        let rules = vec![SpellingRule {
            context_clues: Some(vec!["程式".into(), "軟體".into()]),
            ..SpellingRule::new("支持", vec!["支援".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        // No context clue present: should NOT fire.
        let issues = scanner.scan("我支持你的決定").issues;
        assert!(
            issues.is_empty(),
            "should not fire without context clues: {issues:?}"
        );

        // Context clue present: SHOULD fire.
        let issues = scanner.scan("這個程式支持多種格式").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "支持");
    }

    #[test]
    fn charwise_negative_clues_veto() {
        // Negative context clues must veto correctly on the charwise path.
        let rules = vec![SpellingRule {
            negative_context_clues: Some(vec!["掛載".into(), "mount".into()]),
            ..SpellingRule::new("卸載", vec!["解除安裝".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        // No negative clue: should fire.
        let issues = scanner.scan("請卸載這個應用程式").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "卸載");

        // Negative clue present: should NOT fire.
        let issues = scanner.scan("掛載和卸載檔案系統").issues;
        assert!(
            issues.is_empty(),
            "negative clue '掛載' should veto: {issues:?}"
        );
    }

    #[test]
    fn bytewise_fallback_when_charwise_unavailable() {
        // Force bytewise path, verify results still correct.
        let rules = sample_spelling_rules();
        let mut scanner = Scanner::new(rules, vec![]);
        scanner.force_bytewise();

        let issues = scanner.scan("這個軟件的服務器內存不夠").issues;
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[1].found, "服務器");
        assert_eq!(issues[2].found, "內存");
    }

    #[test]
    fn charwise_many_patterns_same_prefix() {
        // Stress the double-array trie with patterns sharing a common prefix.
        let rules = vec![
            {
                let mut r = SpellingRule::new("數", vec!["數".into()], RuleType::CrossStrait);
                r.context_clues = Some(vec!["不存在的線索".into()]);
                r
            },
            SpellingRule::new("數據", vec!["資料".into()], RuleType::CrossStrait),
            SpellingRule::new("數據庫", vec!["資料庫".into()], RuleType::CrossStrait),
            SpellingRule::new("數據結構", vec!["資料結構".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        // Leftmost-longest: "數據結構" beats "數據" beats "數".
        let issues = scanner.scan("學習數據結構").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "數據結構");
        assert_eq!(issues[0].suggestions[..], vec!["資料結構"]);

        // When only "數據" present, the shorter match wins.
        let issues = scanner.scan("處理數據").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "數據");

        // "數" alone has context_clues that won't match, so it stays quiet.
        let issues = scanner.scan("數字很大").issues;
        assert!(issues.is_empty());
    }

    #[test]
    fn charwise_adjacent_non_overlapping_matches() {
        // Two patterns that appear back-to-back without overlap.
        let rules = vec![
            SpellingRule::new("軟件", vec!["軟體".into()], RuleType::CrossStrait),
            SpellingRule::new("開發", vec!["研發".into()], RuleType::CrossStrait),
        ];
        let scanner = Scanner::new(rules, vec![]);
        assert!(scanner.spelling_db.ac_charwise.is_some());

        // "軟件開發": both patterns match adjacently.
        let issues = scanner.scan("軟件開發很重要").issues;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].found, "軟件");
        assert_eq!(issues[1].found, "開發");
    }

    #[test]
    fn charwise_full_ruleset_builds() {
        // Verify the embedded ruleset (776+ patterns) builds charwise
        // successfully.
        let ruleset = crate::rules::loader::load_embedded_ruleset().unwrap();
        let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
        assert!(
            scanner.spelling_db.ac_charwise.is_some(),
            "charwise AC should build for the full embedded ruleset"
        );
    }

    #[test]
    fn contact_cta_rules_do_not_force_low_editorial_confidence() {
        let ruleset = crate::rules::loader::load_embedded_ruleset().unwrap();
        let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

        let issues = scanner.scan("如需協助請聯繫客服").issues;
        assert_eq!(
            issues.len(),
            1,
            "expected CTA phrase to match once: {issues:?}"
        );
        assert_eq!(issues[0].found, "如需協助請聯繫");
        assert_eq!(issues[0].editorial_confidence, None);

        let issues = scanner.scan("歡迎聯繫我們").issues;
        assert_eq!(
            issues.len(),
            1,
            "expected contact-us phrase to match once: {issues:?}"
        );
        assert_eq!(issues[0].found, "聯繫我們");
        assert_eq!(issues[0].editorial_confidence, None);
    }

    // positional_clues tests

    #[test]
    fn positional_before_fires_when_term_follows() {
        // before:函式 means 函式 must appear within 20 chars AFTER the match.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["before:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 函式 follows 調用: should fire.
        let issues = scanner.scan("請調用函式來處理").issues;
        assert_eq!(issues.len(), 1, "should fire when 函式 follows: {issues:?}");
        assert_eq!(issues[0].found, "調用");

        // 函式 absent: should NOT fire.
        let issues = scanner.scan("請調用這個方法").issues;
        assert!(
            issues.is_empty(),
            "should not fire without 函式 after match: {issues:?}"
        );
    }

    #[test]
    fn positional_after_fires_when_term_precedes() {
        // after:請 means 請 must appear within 20 chars BEFORE the match.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["after:請".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 請 precedes 調用: should fire.
        let issues = scanner.scan("請調用函式").issues;
        assert_eq!(issues.len(), 1, "should fire when 請 precedes: {issues:?}");

        // 請 absent: should NOT fire.
        let issues = scanner.scan("直接調用函式").issues;
        assert!(
            issues.is_empty(),
            "should not fire without 請 before match: {issues:?}"
        );
    }

    #[test]
    fn positional_adjacent_fires_when_immediately_next() {
        // adjacent:函式 means 函式 must be immediately adjacent (no gap).
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["adjacent:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 函式 immediately after 調用: should fire.
        let issues = scanner.scan("調用函式").issues;
        assert_eq!(
            issues.len(),
            1,
            "should fire when 函式 is adjacent: {issues:?}"
        );

        // Gap between them: should NOT fire.
        let issues = scanner.scan("調用某個函式").issues;
        assert!(
            issues.is_empty(),
            "should not fire with gap between match and term: {issues:?}"
        );

        // 函式 immediately before 調用: should also fire (adjacent = either
        // side).
        let issues = scanner.scan("函式調用方式").issues;
        assert_eq!(
            issues.len(),
            1,
            "should fire when 函式 is adjacent before: {issues:?}"
        );
    }

    #[test]
    fn positional_not_before_vetoes() {
        // not_before:的 means 的 must NOT appear within 20 chars after.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["not_before:的".into()]),
            ..SpellingRule::new("項目", vec!["專案".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // No 的 after: should fire.
        let issues = scanner.scan("這個項目進度超前").issues;
        assert_eq!(issues.len(), 1, "should fire without veto term: {issues:?}");

        // 的 follows: should NOT fire.
        let issues = scanner.scan("項目的名稱").issues;
        assert!(
            issues.is_empty(),
            "should be vetoed by 的 after match: {issues:?}"
        );
    }

    #[test]
    fn positional_not_after_vetoes() {
        // not_after:清單 means 清單 must NOT appear within 20 chars before.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["not_after:清單".into()]),
            ..SpellingRule::new("項目", vec!["專案".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 清單 absent: should fire.
        let issues = scanner.scan("這個項目進度超前").issues;
        assert_eq!(issues.len(), 1, "should fire without veto term: {issues:?}");

        // 清單 precedes: should NOT fire.
        let issues = scanner.scan("清單項目需要確認").issues;
        assert!(
            issues.is_empty(),
            "should be vetoed by 清單 before match: {issues:?}"
        );
    }

    #[test]
    fn positional_and_context_clues_both_required() {
        // Rule has both context_clues and positional_clues.  Both must pass.
        let rules = vec![SpellingRule {
            context_clues: Some(vec!["程式".into()]),
            positional_clues: Some(vec!["before:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // Both satisfied: 程式 in window AND 函式 after, should fire.
        let issues = scanner.scan("這個程式調用函式").issues;
        assert_eq!(
            issues.len(),
            1,
            "should fire when both context and positional match: {issues:?}"
        );

        // context_clues satisfied but positional NOT: should not fire.
        let issues = scanner.scan("這個程式調用方法").issues;
        assert!(
            issues.is_empty(),
            "positional fails, should not fire: {issues:?}"
        );

        // positional satisfied but context_clues NOT: should not fire.
        let issues = scanner.scan("直接調用函式").issues;
        assert!(
            issues.is_empty(),
            "context_clues fails, should not fire: {issues:?}"
        );
    }

    #[test]
    fn positional_multiple_conditions_all_must_pass() {
        // Multiple positional conditions: all must pass (AND).
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["after:請".into(), "before:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // Both conditions met.
        let issues = scanner.scan("請調用函式").issues;
        assert_eq!(
            issues.len(),
            1,
            "both positional conditions met: {issues:?}"
        );

        // Only one condition met.
        let issues = scanner.scan("請調用方法").issues;
        assert!(
            issues.is_empty(),
            "only after: met, before: not — should not fire: {issues:?}"
        );

        let issues = scanner.scan("直接調用函式").issues;
        assert!(
            issues.is_empty(),
            "only before: met, after: not — should not fire: {issues:?}"
        );
    }

    #[test]
    fn positional_no_regression_without_positional_clues() {
        // Rules without positional_clues should behave exactly as before.
        let rules = vec![SpellingRule::new(
            "軟件",
            vec!["軟體".into()],
            RuleType::CrossStrait,
        )];
        let scanner = Scanner::new(rules, vec![]);
        let issues = scanner.scan("這個軟件很好用").issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "軟件");
    }

    #[test]
    fn positional_before_stops_at_paragraph_break() {
        // before:函式 should NOT match across a paragraph boundary.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["before:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 函式 is in the next paragraph: should NOT fire.
        let issues = scanner.scan("請調用方法\n\n函式定義在此").issues;
        assert!(
            issues.is_empty(),
            "before: must not match across paragraph break: {issues:?}"
        );

        // 函式 is in the same paragraph: should fire.
        let issues = scanner.scan("請調用函式").issues;
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn positional_after_stops_at_paragraph_break() {
        // after:請 should NOT match across a paragraph boundary.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["after:請".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 請 is in the previous paragraph: should NOT fire.
        let issues = scanner.scan("請看這裡\n\n調用方法").issues;
        assert!(
            issues.is_empty(),
            "after: must not match across paragraph break: {issues:?}"
        );
    }

    #[test]
    fn positional_before_stops_at_code_span() {
        // In Markdown, before:函式 should NOT match text inside a code span.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["before:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 函式 is inside a code span: positional window should stop at the
        // excluded range boundary, so the clue is invisible.
        let md_text = "調用`函式`來處理";
        let issues = scanner
            .scan_for_content_type(md_text, ContentType::Markdown, Profile::Base)
            .issues;
        assert!(
            issues.is_empty(),
            "before: must not see text inside code spans: {issues:?}"
        );

        // Same text without code span: should fire.
        let plain_text = "調用函式來處理";
        let issues = scanner
            .scan_for_content_type(plain_text, ContentType::Markdown, Profile::Base)
            .issues;
        assert_eq!(
            issues.len(),
            1,
            "should fire when 函式 is not in code span: {issues:?}"
        );
    }

    #[test]
    fn positional_adjacent_excluded_region() {
        // adjacent:函式 should NOT match if 函式 is inside an excluded region.
        let rules = vec![SpellingRule {
            positional_clues: Some(vec!["adjacent:函式".into()]),
            ..SpellingRule::new("調用", vec!["呼叫".into()], RuleType::CrossStrait)
        }];
        let scanner = Scanner::new(rules, vec![]);

        // 函式 inside a code span (Markdown): adjacent should not match.
        let md_text = "調用`函式`";
        let issues = scanner
            .scan_for_content_type(md_text, ContentType::Markdown, Profile::Base)
            .issues;
        assert!(
            issues.is_empty(),
            "adjacent: must not match term inside excluded region: {issues:?}"
        );
    }

    #[test]
    fn lian_xi_contact_copy_uses_phrase_rules_without_general_prose_fp() {
        let rules = vec![
            SpellingRule::new("聯繫我們", vec!["聯絡我們".into()], RuleType::CrossStrait),
            SpellingRule::new("聯繫方式", vec!["聯絡方式".into()], RuleType::CrossStrait),
            SpellingRule::new("聯繫資訊", vec!["聯絡資訊".into()], RuleType::CrossStrait),
            SpellingRule::new("聯繫管道", vec!["聯絡管道".into()], RuleType::CrossStrait),
            SpellingRule::new("聯繫電話", vec!["聯絡電話".into()], RuleType::CrossStrait),
            SpellingRule::new("聯繫客服", vec!["聯絡客服".into()], RuleType::CrossStrait),
            SpellingRule::new(
                "如需協助請聯繫",
                vec!["如需協助請聯絡".into()],
                RuleType::CrossStrait,
            ),
        ];
        let scanner = Scanner::new(rules, vec![]);

        let issues = scanner.scan("歡迎聯繫我們").issues;
        assert_eq!(issues.len(), 1, "should flag contact CTA: {issues:?}");

        let issues = scanner.scan("請查看聯繫方式").issues;
        assert_eq!(issues.len(), 1, "should flag contact label: {issues:?}");

        let issues = scanner.scan("最新聯繫資訊如下").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag contact info label: {issues:?}"
        );

        let issues = scanner.scan("若需協助可參考聯繫管道").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag contact channel label: {issues:?}"
        );

        let issues = scanner.scan("聯繫電話：02-1234-5678").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag contact phone label: {issues:?}"
        );

        let issues = scanner.scan("請聯繫客服取得協助").issues;
        assert_eq!(issues.len(), 1, "should flag support CTA: {issues:?}");

        let issues = scanner.scan("如需協助請聯繫").issues;
        assert_eq!(
            issues.len(),
            1,
            "should flag imperative support CTA: {issues:?}"
        );

        let issues = scanner.scan("我們與學界保持密切聯繫").issues;
        assert!(
            issues.is_empty(),
            "should not flag ordinary prose: {issues:?}"
        );

        let issues = scanner.scan("請加強國際聯繫").issues;
        assert!(
            issues.is_empty(),
            "should not flag ordinary prose: {issues:?}"
        );

        let issues = scanner.scan("我們透過電話聯繫對方").issues;
        assert!(
            issues.is_empty(),
            "should not flag ordinary prose: {issues:?}"
        );
    }

    #[test]
    fn translationese_pipeline_keeps_only_indexed_zy2_issue() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("因為下雨了，所以我們待在屋裡。").issues;
        let zy2 = translationese_issues(&issues, PhaseFamily::Connective);
        assert_eq!(zy2.len(), 1, "expected one surviving ZY2 issue: {issues:?}");
        assert!(
            zy2[0]
                .phase_family
                .is_some_and(|(_, pass)| pass == PhasePass::Indexed),
            "the boundary-aware half should win: {issues:?}"
        );
    }

    #[test]
    fn translationese_pipeline_keeps_only_indexed_zy3_issue() {
        let scanner = Scanner::new(vec![], vec![]);
        let issues = scanner.scan("他完成改善的提升的發現工作。").issues;
        let zy3 = translationese_issues(&issues, PhaseFamily::Nominalization);
        assert_eq!(zy3.len(), 1, "expected one surviving ZY3 issue: {issues:?}");
        assert!(
            zy3[0]
                .phase_family
                .is_some_and(|(_, pass)| pass == PhasePass::Indexed),
            "the boundary-aware half should win: {issues:?}"
        );
    }

    // Remaining tests are included from the original scan.rs via include.
    // Rather than duplicating 2000+ lines inline, the tests are appended by
    // extracting from the original monolithic file.
    include!("tests_generated.rs");
}
