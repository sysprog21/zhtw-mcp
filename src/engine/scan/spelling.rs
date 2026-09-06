// Spelling rule scan using Aho-Corasick (charwise daachorse primary, bytewise
// fallback). Context-clue checking uses a windowed AC scan over bounded slices
// rather than full-document pre-scan.

use super::emit::Emitter;
use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::segment::BoundaryBitmap;
use crate::engine::zhtype::ChineseType;
use crate::rules::ruleset::{Issue, IssueType, ProfileConfig};

use super::rule_ir::{self, MatchContext};
use super::{
    clamp_at_excluded, PositionalClue, Scanner, CONTEXT_WINDOW_CHARS, POSITIONAL_WINDOW_CHARS,
};

// Per-rule bitflags gating optional filter stages in process_spelling_match.
// Most rules have flags == 0 (no optional stages), skipping all guarded blocks
// at near-zero cost.
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
    pub(crate) fn scan_spelling(
        &self,
        em: &mut Emitter<'_>,
        zh_type: ChineseType,
        cfg: &ProfileConfig,
        clue_buf: &mut Vec<(usize, u16)>,
        boundary_bitmap: &BoundaryBitmap,
    ) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

        let mut excl_cursor: usize = 0;
        let n_rules = self.spelling_db.spelling_rules.len();

        // Pre-compute profile gates.
        let skip_variant = !cfg.variant_normalization || zh_type == ChineseType::Simplified;
        let skip_ai = !cfg.ai_filler_detection;
        let skip_translationese = !cfg.translationese_detection;

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
                {
                    use crate::rules::ruleset::RuleType;
                    match compiled.rule_type {
                        RuleType::Variant if skip_variant => continue,
                        RuleType::AiFiller if skip_ai => continue,
                        RuleType::Translationese if skip_translationese => continue,
                        RuleType::PoliticalColoring
                            if !cfg
                                .political_stance
                                .allows_rule(&self.spelling_db.spelling_rules[idx].from) =>
                        {
                            continue
                        }
                        _ => {}
                    }
                }

                let class = self.spelling_db.rule_classes[idx];
                if !clue_index_built && (class == CLASS_CLUED || class == CLASS_FULL) {
                    rule_ir::build_clue_index_into(
                        self.spelling_db.clue_ac.as_ref(),
                        text,
                        clue_buf,
                    );
                    clue_index_built = true;
                }

                // Document-level fast path: if the clue index is empty and this
                // rule requires positive clue matches, it will always be
                // rejected. Skip MatchContext construction entirely.
                if clue_index_built
                    && clue_buf.is_empty()
                    && self.spelling_db.rule_pos_clue_ids[idx].is_some()
                {
                    continue;
                }

                if class == CLASS_TRULY_SIMPLE {
                    // Inline fast path: no MatchContext, no function call.
                    let start = $start;
                    let end = $end;

                    // Exclusion cursor (amortized O(1)).
                    while excl_cursor < excluded.len() && excluded[excl_cursor].end <= start {
                        excl_cursor += 1;
                    }
                    let is_excluded = excl_cursor < excluded.len()
                        && excluded[excl_cursor].start < end
                        && start < excluded[excl_cursor].end;
                    if is_excluded {
                        continue;
                    }

                    let straddle = if boundary_bitmap.is_empty() {
                        self.segmenter
                            .match_straddles_word_boundary(text, start, end)
                    } else {
                        boundary_bitmap.start_straddles(start)
                            || boundary_bitmap.end_straddles(end, start)
                    };
                    if !straddle {
                        issues.push(Issue::deferred_spelling(
                            start,
                            end - start,
                            IssueType::from(compiled.rule_type),
                            compiled.rule_type.default_severity(),
                            compiled.rule_idx,
                        ));
                    }
                } else if class == CLASS_SIMPLE {
                    // CLASS_SIMPLE: has superstring/exception/deletion but no
                    // clue checks. Avoids clue_index build and passes empty
                    // clue slice to skip clue-related branches.
                    let mut ctx = MatchContext {
                        text,
                        excluded,
                        excl_cursor: &mut excl_cursor,
                        cfg,
                        zh_type,
                        start: $start,
                        end: $end,
                        clue_index: &[],
                        boundary_bitmap,
                    };
                    if let Some(issue) = rule_ir::eval_predicates(
                        &self.spelling_db,
                        compiled,
                        &mut ctx,
                        &self.segmenter,
                    ) {
                        issues.push(issue);
                    }
                } else {
                    // CLASS_CLUED / CLASS_FULL: needs document-wide clue index.
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
                    if let Some(issue) = rule_ir::eval_predicates(
                        &self.spelling_db,
                        compiled,
                        &mut ctx,
                        &self.segmenter,
                    ) {
                        issues.push(issue);
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

/// Positional window BEFORE the match, clamped at paragraph/excluded
/// boundaries.
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
        assert_eq!(scanner.spelling_db.spelling_contexts.len(), n);
        assert_eq!(scanner.spelling_db.spelling_english.len(), n);
        assert_eq!(scanner.spelling_db.spelling_context_clues.len(), n);
        assert_eq!(scanner.spelling_db.spelling_context_suggestions.len(), n);
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
        // Sanity check: majority of rules should be CLASS_SIMPLE (the 79% from
        // PR #49 analysis). At least 60% to guard against drift.
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

    #[test]
    fn hong_macro_fires_with_explicit_clue() {
        // 宏 rule needs an explicit macro clue (e.g. #define, macro, 展開).
        let scanner = make_scanner();
        let issues = scanner.scan("這個宏是用 #define 展開的").issues;
        assert!(
            issues.iter().any(|i| i.found == "宏"),
            "宏 must fire when #define clue is nearby"
        );
    }

    #[test]
    fn coverage_report_populated() {
        let scanner = make_scanner();
        let output = scanner.scan("這是正確的繁體中文");
        let cov = output.coverage.expect("coverage must be present");
        assert!(cov.rules_checked > 100, "should have many rules checked");
        assert_eq!(cov.rules_matched, 0, "clean text should match 0 rules");

        let output2 = scanner.scan("軟件工程");
        let cov2 = output2.coverage.expect("coverage must be present");
        assert!(
            cov2.rules_matched > 0,
            "text with issues should match >0 rules"
        );
    }

    #[test]
    fn oral_density_no_double_count() {
        // "就是說" contains both the "就是" and "就是說" markers. The
        // merged-span approach must not double-count the overlap.
        let scanner = make_scanner();
        let text = "就是說就是說就是說就是說就是說就是說就是說就是說就是說就是說";
        let output = scanner.scan(text);
        let density = output.oral_density.expect("should compute density");
        assert!(
            density <= 1.0,
            "oral_density must not exceed 1.0, got {density}"
        );
    }

    #[test]
    fn quality_flag_asr_only_for_asr_confusables() {
        // "函數" is a non-ASR confusable: should NOT set "asr_artifacts".
        // "機體" near RAM clues is an ASR confusable: should set it.
        let scanner = make_scanner();

        let output1 = scanner.scan("函數在數學領域是 sin cos 的統稱");
        assert!(
            !output1.quality_flags.contains(&"asr_artifacts".to_string()),
            "non-ASR confusable should not trigger asr_artifacts flag"
        );
    }

    // -- math "函數" must NOT be rewritten to "函式" --------------------------

    #[test]
    fn math_function_not_rewritten_elementary() {
        let scanner = make_scanner();
        let text = "初等函數是由基本運算組合而成的函數";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math term '初等函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_trig() {
        let scanner = make_scanner();
        let text = "三角函數包含正弦與餘弦";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math term '三角函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_inverse() {
        let scanner = make_scanner();
        let text = "反函數的定義域和值域互換";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math term '反函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_composite() {
        let scanner = make_scanner();
        let text = "合成函數 f(g(x)) 的導函數可用鏈鎖律求得";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math terms '合成函數'/'導函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_exp_log() {
        let scanner = make_scanner();
        let text = "指數函數和對數函數互為反函數";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math terms '指數函數'/'對數函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_pdf_cdf() {
        let scanner = make_scanner();
        let text = "機率密度函數描述連續隨機變數的分佈函數";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math terms '機率密度函數'/'分佈函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_functional_analysis() {
        let scanner = make_scanner();
        let text = "函數分析是數學的一個分支";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math term '函數分析' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn math_function_not_rewritten_continuous() {
        let scanner = make_scanner();
        let text = "設 f 為一連續函數，其定義域為實數";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "math term '連續函數' must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn programming_function_still_flagged() {
        let scanner = make_scanner();
        let text = "編譯器會呼叫這個函數來處理程式碼";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|i| i.found == "函數"),
            "programming context '函數' must still be flagged: {issues:?}"
        );
    }

    // -- "函式" in programming context must NOT be rewritten to "函數" --------

    #[test]
    fn hanshi_not_rewritten_when_programming_clues_present() {
        // "函式" is correct for programming; math clues nearby must not
        // override when programming clues also appear.
        let scanner = make_scanner();
        let text = "在程式碼中計算三角函數的函式";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函式"),
            "'函式' in programming context must not be rewritten: {issues:?}"
        );
    }

    #[test]
    fn hanshi_not_rewritten_with_return_value() {
        let scanner = make_scanner();
        let text = "此函式計算 sin cos 值後回傳結果";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函式"),
            "'函式' with '回傳' must not be rewritten to 函數: {issues:?}"
        );
    }

    #[test]
    fn hanshi_not_rewritten_with_declaration() {
        let scanner = make_scanner();
        let text = "在 C 語言中宣告一個計算三角函數的函式";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函式"),
            "'函式' with '宣告' must not be rewritten to 函數: {issues:?}"
        );
    }

    #[test]
    fn math_hanshi_still_rewritten_with_parameters_and_variables() {
        let scanner = make_scanner();
        let text = "在數學中，函式的參數與變數可以表示為 x";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|i| i.found == "函式"),
            "math '函式' must still be rewritten near shared math terms: {issues:?}"
        );
    }

    // -- mixed context: math compound "函數" must survive near programming
    // clues

    #[test]
    fn math_compound_survives_mixed_context() {
        // "三角函數" is a math proper noun even when programming clues exist.
        let scanner = make_scanner();
        let text = "在程式碼中計算三角函數的函式";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "'三角函數' must not be rewritten even near programming clues: {issues:?}"
        );
    }

    #[test]
    fn hanshu_diaoyon_fires_in_programming() {
        // "函數調用→函式呼叫" only in programming context. Math evaluates
        // (代入/求值), never calls (呼叫).
        let scanner = make_scanner();
        let text = "編譯器的函數調用機制很重要";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|i| i.found == "函數調用"),
            "'函數調用' in programming context must be flagged: {issues:?}"
        );
    }

    #[test]
    fn hanshu_diaoyon_silent_without_programming_clues() {
        // Without programming clues, "函數調用" should not fire.
        let scanner = make_scanner();
        let text = "函數調用的概念";
        let issues = scanner.scan(text).issues;
        assert!(
            !issues.iter().any(|i| i.found == "函數調用"),
            "'函數調用' without programming clues must not fire: {issues:?}"
        );
    }

    #[test]
    fn hanshu_diaoyon_silent_inside_math_compound() {
        let scanner = make_scanner();
        let text = "程式中的三角函數調用";
        let issues = scanner.scan(text).issues;
        assert!(
            !issues.iter().any(|i| i.found == "函數調用"),
            "math compound '三角函數調用' must not rewrite inner 函數調用: {issues:?}"
        );
    }

    #[test]
    fn ambiguous_context_math_vetoes_standalone_hanshu() {
        // When both programming and math clues coexist, negative clues veto the
        // 函數->函式 rule for standalone 函數.
        let scanner = make_scanner();
        let text = "用程式碼呼叫函數來求解微積分";
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|i| i.found != "函數"),
            "mixed context: math clue should veto 函數->函式: {issues:?}"
        );
    }
}
