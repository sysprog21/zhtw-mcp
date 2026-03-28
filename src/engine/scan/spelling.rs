// Spelling rule scan using Aho-Corasick (charwise daachorse primary,
// bytewise fallback).  Context-clue checking uses a windowed AC scan
// over bounded slices rather than full-document pre-scan.

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::segment::BoundaryBitmap;
use crate::engine::zhtype::ChineseType;
use crate::rules::ruleset::{Issue, IssueType, ProfileConfig};

use super::rule_ir::{self, MatchContext};
use super::{
    clamp_at_excluded, PositionalClue, Scanner, CONTEXT_WINDOW_CHARS, POSITIONAL_WINDOW_CHARS,
};

// Per-rule bitflags gating optional filter stages in process_spelling_match.
// Most rules have flags == 0 (no optional stages), skipping all guarded
// blocks at near-zero cost.
pub(crate) const FILTER_HAS_SUPERSTRING: u8 = 1 << 0;
pub(crate) const FILTER_HAS_EXCEPTIONS: u8 = 1 << 1;
pub(crate) const FILTER_HAS_POS_CLUES: u8 = 1 << 2;
pub(crate) const FILTER_HAS_NEG_CLUES: u8 = 1 << 3;
pub(crate) const FILTER_HAS_POSITIONAL: u8 = 1 << 4;
pub(crate) const FILTER_IS_DELETION: u8 = 1 << 5;

// Rule dispatch classes: monomorphic fast paths for common rule shapes.
// Computed once at AC build time, dispatched per-match to eliminate dead
// branches in the filter cascade (45.2 step 2).
pub(crate) const CLASS_SIMPLE: u8 = 0; // no context clues, no positional
pub(crate) const CLASS_CLUED: u8 = 1; // context clues only (pos/neg)
pub(crate) const CLASS_FULL: u8 = 2; // has positional clues (± context)
pub(crate) const CLASS_TRULY_SIMPLE: u8 = 3; // filter_flags == 0 (no superstrings, no exceptions, etc.)

impl Scanner {
    /// Single-pass spelling scan with BoundaryBitmap fast-path.
    ///
    /// Uses the IR-based evaluation path: each AC hit is evaluated against
    /// its precompiled predicate chain via `eval_predicates()`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scan_spelling(
        &self,
        text: &str,
        excluded: &[ByteRange],
        zh_type: ChineseType,
        issues: &mut Vec<Issue>,
        cfg: &ProfileConfig,
        clue_buf: &mut Vec<(usize, u16)>,
        boundary_bitmap: &BoundaryBitmap,
    ) {
        let mut excl_cursor: usize = 0;
        let n_rules = self.spelling_db.spelling_rules.len();

        // Pre-compute profile gates.
        let skip_variant = !cfg.variant_normalization || zh_type == ChineseType::Simplified;
        let skip_ai = !cfg.ai_filler_detection;

        // Lazy clue index.
        clue_buf.clear();
        let mut clue_index_built = false;

        macro_rules! eval_hit {
            ($start:expr, $end:expr, $idx:expr) => {
                let idx = $idx;
                if idx >= n_rules {
                    continue;
                }
                let compiled = &self.spelling_db.rules[idx];

                // Fast-reject profile-gated rules.
                match compiled.rule_type {
                    crate::rules::ruleset::RuleType::Variant if skip_variant => {}
                    crate::rules::ruleset::RuleType::AiFiller if skip_ai => {}
                    crate::rules::ruleset::RuleType::PoliticalColoring
                        if !cfg
                            .political_stance
                            .allows_rule(&self.spelling_db.spelling_rules[idx].from) => {}
                    _ => {
                        let class = self.spelling_db.rule_classes[idx];
                        if !clue_index_built && (class == CLASS_CLUED || class == CLASS_FULL) {
                            rule_ir::build_clue_index_into(
                                self.spelling_db.clue_ac.as_ref(),
                                text,
                                clue_buf,
                            );
                            clue_index_built = true;
                        }

                        if class == CLASS_TRULY_SIMPLE {
                            // Inline fast path: no MatchContext, no function call.
                            // CLASS_TRULY_SIMPLE = no superstring, no exception,
                            // no deletion, no clues, no positional.
                            let start = $start;
                            let end = $end;

                            // Exclusion cursor (amortized O(1)).
                            while excl_cursor < excluded.len() && excluded[excl_cursor].end <= start
                            {
                                excl_cursor += 1;
                            }
                            if excl_cursor < excluded.len()
                                && excluded[excl_cursor].start < end
                                && start < excluded[excl_cursor].end
                            {
                            } else {
                                // Boundary bitmap (authoritative).
                                let straddle = if boundary_bitmap.is_empty() {
                                    self.segmenter
                                        .match_straddles_word_boundary(text, start, end)
                                } else {
                                    boundary_bitmap.start_straddles(start)
                                        || boundary_bitmap.end_straddles(end, start)
                                };
                                if !straddle {
                                    let mut issue = Issue::new(
                                        start,
                                        end - start,
                                        "",
                                        Vec::new(),
                                        IssueType::from(compiled.rule_type),
                                        compiled.rule_type.default_severity(),
                                    );
                                    issue.spelling_rule_idx = Some(compiled.rule_idx);
                                    issues.push(issue);
                                }
                            }
                        } else {
                            // CLASS_SIMPLE / CLASS_CLUED / CLASS_FULL: full eval path.
                            let mut ctx = MatchContext {
                                text,
                                excluded,
                                excl_cursor: &mut excl_cursor,
                                cfg,
                                zh_type,
                                start: $start,
                                end: $end,
                                clue_index: clue_buf.as_slice(),
                                boundary_bitmap,
                            };
                            let result = if class == CLASS_SIMPLE {
                                rule_ir::eval_simple(
                                    &self.spelling_db,
                                    compiled,
                                    &mut ctx,
                                    &self.segmenter,
                                )
                            } else {
                                rule_ir::eval_predicates(
                                    &self.spelling_db,
                                    compiled,
                                    &mut ctx,
                                    &self.segmenter,
                                )
                            };
                            if let Some(issue) = result {
                                issues.push(issue);
                            }
                        }
                    }
                }
            };
        }

        if let Some(ref cw_ac) = self.spelling_db.ac_charwise {
            for mat in cw_ac.leftmost_find_iter(text) {
                eval_hit!(mat.start(), mat.end(), mat.value());
            }
        } else if let Some(ref bw_ac) = self.spelling_db.ac_bytewise {
            for mat in bw_ac.find_iter(text) {
                eval_hit!(mat.start(), mat.end(), mat.pattern().as_usize());
            }
        }
    }
}

/// Compute the byte-offset window for context-clue proximity checks,
/// clamped at paragraph breaks and excluded-range boundaries.
pub(crate) fn context_byte_window(
    text: &str,
    match_start: usize,
    match_end: usize,
    excluded: &[ByteRange],
) -> (usize, usize) {
    let bytes = text.as_bytes();
    let max_search = CONTEXT_WINDOW_CHARS * 4;
    let para_start = {
        let search_start = match_start.saturating_sub(max_search);
        let search = &bytes[search_start..match_start];
        find_last_paragraph_break(search).map_or(0, |pos| search_start + pos + 1)
    };
    let para_end = {
        let search_end = (match_end + max_search).min(text.len());
        let search = &bytes[match_end..search_end];
        find_first_paragraph_break(search).map_or(text.len(), |pos| match_end + pos)
    };

    let mut byte_start = match_start;
    for _ in 0..CONTEXT_WINDOW_CHARS {
        if byte_start <= para_start {
            byte_start = para_start;
            break;
        }
        byte_start = text.floor_char_boundary(byte_start - 1);
    }
    byte_start = byte_start.max(para_start);

    let mut byte_end = match_end;
    for _ in 0..CONTEXT_WINDOW_CHARS {
        if byte_end >= para_end {
            byte_end = para_end;
            break;
        }
        byte_end = text.ceil_char_boundary(byte_end + 1);
    }
    byte_end = byte_end.min(para_end);

    if excluded.is_empty() {
        return (byte_start, byte_end);
    }

    clamp_at_excluded(text, byte_start, byte_end, match_start, match_end, excluded)
}

/// Last `\n\n` (or `\r\n\r\n`) offset in `bytes`, pointing at the second `\n`.
fn find_last_paragraph_break(bytes: &[u8]) -> Option<usize> {
    // Scan backward for \n\n.
    let len = bytes.len();
    if len < 2 {
        return None;
    }
    let mut i = len - 1;
    while i > 0 {
        if bytes[i] == b'\n' && bytes[i - 1] == b'\n' {
            return Some(i);
        }
        // Handle \r\n\r\n: bytes[i]=\n, bytes[i-1]=\r, bytes[i-2]=\n
        if i >= 2 && bytes[i] == b'\n' && bytes[i - 1] == b'\r' && bytes[i - 2] == b'\n' {
            return Some(i);
        }
        i -= 1;
    }
    None
}

/// First `\n\n` (or `\n\r\n`) offset in `bytes`, pointing at the first `\n`.
fn find_first_paragraph_break(bytes: &[u8]) -> Option<usize> {
    let len = bytes.len();
    if len < 2 {
        return None;
    }
    for i in 0..len - 1 {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            return Some(i);
        }
        // \n\r\n also counts.
        if i + 2 < len && bytes[i] == b'\n' && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
            return Some(i);
        }
    }
    None
}

/// Check all positional clues for a match at [start, end).
/// Positive clues use AND semantics; any negative clue vetoes.
pub(crate) fn check_positional_clues(
    text: &str,
    start: usize,
    end: usize,
    excluded: &[ByteRange],
    clues: &[PositionalClue],
) -> bool {
    let mut after_win: Option<(usize, usize)> = None;
    let mut before_win: Option<(usize, usize)> = None;

    for clue in clues {
        match clue {
            PositionalClue::Before(term) => {
                let (ws, we) =
                    *after_win.get_or_insert_with(|| positional_bounds_after(text, end, excluded));
                if !text[ws..we].contains(term.as_str()) {
                    return false;
                }
            }
            PositionalClue::After(term) => {
                let (ws, we) = *before_win
                    .get_or_insert_with(|| positional_bounds_before(text, start, excluded));
                if !text[ws..we].contains(term.as_str()) {
                    return false;
                }
            }
            PositionalClue::Adjacent(term) => {
                // Immediately before: term ends right at match start.
                let before_ok = start >= term.len()
                    && text.get(start - term.len()..start) == Some(term.as_str())
                    && !is_excluded(start - term.len(), start, excluded);
                // Immediately after: term starts right at match end.
                let after_ok = text.get(end..end + term.len()) == Some(term.as_str())
                    && !is_excluded(end, end + term.len(), excluded);
                if !before_ok && !after_ok {
                    return false;
                }
            }
            PositionalClue::NotBefore(term) => {
                let (ws, we) =
                    *after_win.get_or_insert_with(|| positional_bounds_after(text, end, excluded));
                if text[ws..we].contains(term.as_str()) {
                    return false;
                }
            }
            PositionalClue::NotAfter(term) => {
                let (ws, we) = *before_win
                    .get_or_insert_with(|| positional_bounds_before(text, start, excluded));
                if text[ws..we].contains(term.as_str()) {
                    return false;
                }
            }
        }
    }
    true
}

/// Positional window AFTER the match, clamped at paragraph/excluded boundaries.
fn positional_bounds_after(text: &str, match_end: usize, excluded: &[ByteRange]) -> (usize, usize) {
    if match_end >= text.len() {
        return (text.len(), text.len());
    }
    let bytes = text.as_bytes();
    let max_search = POSITIONAL_WINDOW_CHARS * 4;
    let search_end = (match_end + max_search).min(text.len());
    let para_end = {
        let search = &bytes[match_end..search_end];
        find_first_paragraph_break(search).map_or(text.len(), |pos| match_end + pos)
    };

    let mut byte_end = match_end;
    for _ in 0..POSITIONAL_WINDOW_CHARS {
        if byte_end >= para_end {
            byte_end = para_end;
            break;
        }
        byte_end = text.ceil_char_boundary(byte_end + 1);
    }
    byte_end = byte_end.min(para_end);

    if !excluded.is_empty() {
        let right_idx = excluded.partition_point(|r| r.start < match_end);
        for excl in &excluded[right_idx..] {
            if excl.start >= byte_end {
                break;
            }
            if excl.start >= match_end && excl.start < byte_end {
                byte_end = excl.start;
            }
        }
    }

    let byte_end = text.floor_char_boundary(byte_end.min(text.len()));
    if match_end > byte_end {
        return (match_end, match_end);
    }
    (match_end, byte_end)
}

/// Positional window BEFORE the match, clamped at paragraph/excluded boundaries.
fn positional_bounds_before(
    text: &str,
    match_start: usize,
    excluded: &[ByteRange],
) -> (usize, usize) {
    if match_start == 0 {
        return (0, 0);
    }
    let bytes = text.as_bytes();
    let max_search = POSITIONAL_WINDOW_CHARS * 4;
    let search_start = match_start.saturating_sub(max_search);
    let para_start = {
        let search = &bytes[search_start..match_start];
        find_last_paragraph_break(search).map_or(0, |pos| search_start + pos + 1)
    };

    let mut byte_start = match_start;
    for _ in 0..POSITIONAL_WINDOW_CHARS {
        if byte_start <= para_start {
            byte_start = para_start;
            break;
        }
        byte_start = text.floor_char_boundary(byte_start - 1);
    }
    byte_start = byte_start.max(para_start);

    if !excluded.is_empty() {
        let left_idx = excluded.partition_point(|r| r.start < match_start);
        for excl in excluded[..left_idx].iter().rev() {
            if excl.end <= byte_start {
                break;
            }
            if excl.end <= match_start && excl.end > byte_start {
                byte_start = excl.end;
            }
        }
    }

    let byte_start = text.ceil_char_boundary(byte_start);
    if byte_start > match_start {
        return (match_start, match_start);
    }
    (byte_start, match_start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_scanner() -> Scanner {
        use crate::rules::loader::load_embedded_ruleset;
        let rs = load_embedded_ruleset().expect("load embedded ruleset");
        Scanner::new(rs.spelling_rules, rs.case_rules)
    }

    #[test]
    fn filter_flags_match_rule_properties() {
        // Derive expected flags from raw rule fields only, NOT from scanner
        // caches (which share the same normalization pipeline as the flags).
        let scanner = make_scanner();
        for (i, rule) in scanner.spelling_db.spelling_rules.iter().enumerate() {
            let f = scanner.spelling_db.rule_filter_flags[i];
            assert_eq!(
                f & FILTER_HAS_SUPERSTRING != 0,
                rule.to.iter().any(|t| t.contains(&rule.from)),
                "rule '{}': SUPERSTRING mismatch",
                rule.from
            );
            assert_eq!(
                f & FILTER_HAS_EXCEPTIONS != 0,
                rule.exceptions.as_ref().is_some_and(|v| !v.is_empty()),
                "rule '{}': EXCEPTIONS mismatch",
                rule.from
            );
            assert_eq!(
                f & FILTER_HAS_POS_CLUES != 0,
                rule.context_clues.as_ref().is_some_and(|v| !v.is_empty()),
                "rule '{}': POS_CLUES mismatch",
                rule.from
            );
            assert_eq!(
                f & FILTER_HAS_NEG_CLUES != 0,
                rule.negative_context_clues
                    .as_ref()
                    .is_some_and(|v| !v.is_empty()),
                "rule '{}': NEG_CLUES mismatch",
                rule.from
            );
            assert_eq!(
                f & FILTER_HAS_POSITIONAL != 0,
                rule.positional_clues
                    .as_ref()
                    .is_some_and(|v| v.iter().any(|s| PositionalClue::parse(s).is_some())),
                "rule '{}': POSITIONAL mismatch",
                rule.from
            );
            assert_eq!(
                f & FILTER_IS_DELETION != 0,
                rule.is_deletion_rule(),
                "rule '{}': IS_DELETION mismatch",
                rule.from
            );
        }
    }

    #[test]
    fn filter_vecs_aligned() {
        let scanner = make_scanner();
        let n = scanner.spelling_db.spelling_rules.len();
        assert_eq!(scanner.spelling_db.rule_filter_flags.len(), n);
        assert_eq!(scanner.spelling_db.rule_classes.len(), n);
        assert_eq!(scanner.spelling_db.rule_pos_clue_ids.len(), n);
        assert_eq!(scanner.spelling_db.rule_neg_clue_ids.len(), n);
        assert_eq!(scanner.spelling_db.rule_positional_clues.len(), n);
        assert_eq!(scanner.spelling_db.spelling_suggestions.len(), n);
    }

    #[test]
    fn rule_classes_match_filter_flags() {
        let scanner = make_scanner();
        for (i, &f) in scanner.spelling_db.rule_filter_flags.iter().enumerate() {
            let has_clues = f & (FILTER_HAS_POS_CLUES | FILTER_HAS_NEG_CLUES) != 0;
            let has_positional = f & FILTER_HAS_POSITIONAL != 0;
            let expected = if has_positional {
                CLASS_FULL
            } else if has_clues {
                CLASS_CLUED
            } else if f == 0 {
                CLASS_TRULY_SIMPLE
            } else {
                CLASS_SIMPLE
            };
            assert_eq!(
                scanner.spelling_db.rule_classes[i], expected,
                "rule '{}': class mismatch (flags=0x{:02x})",
                scanner.spelling_db.spelling_rules[i].from, f
            );
        }
    }

    #[test]
    fn rule_class_distribution() {
        // Sanity check: majority of rules should be CLASS_SIMPLE (the 79%
        // from PR #49 analysis).  At least 60% to guard against drift.
        let scanner = make_scanner();
        let total = scanner.spelling_db.rule_classes.len();
        let truly_simple = scanner
            .spelling_db
            .rule_classes
            .iter()
            .filter(|&&c| c == CLASS_TRULY_SIMPLE)
            .count();
        let simple = scanner
            .spelling_db
            .rule_classes
            .iter()
            .filter(|&&c| c == CLASS_SIMPLE)
            .count();
        let clued = scanner
            .spelling_db
            .rule_classes
            .iter()
            .filter(|&&c| c == CLASS_CLUED)
            .count();
        let full = scanner
            .spelling_db
            .rule_classes
            .iter()
            .filter(|&&c| c == CLASS_FULL)
            .count();
        assert_eq!(truly_simple + simple + clued + full, total);
        // CLASS_TRULY_SIMPLE + CLASS_SIMPLE together form the 'simple' bucket.
        let simple_total = truly_simple + simple;
        assert!(
            simple_total * 100 / total >= 60,
            "expected >= 60% simple rules, got {simple_total}/{total} ({:.0}%)",
            simple_total as f64 / total as f64 * 100.0
        );
        eprintln!(
            "rule class distribution: truly_simple={truly_simple} ({:.0}%), simple={simple} ({:.0}%), clued={clued} ({:.0}%), full={full} ({:.0}%)",
            truly_simple as f64 / total as f64 * 100.0,
            simple as f64 / total as f64 * 100.0,
            clued as f64 / total as f64 * 100.0,
            full as f64 / total as f64 * 100.0,
        );
    }
}
