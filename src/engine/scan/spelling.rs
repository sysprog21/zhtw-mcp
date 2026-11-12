// Spelling rule scan using Aho-Corasick (charwise daachorse primary, bytewise
// fallback). Context-clue checking uses a windowed AC scan over bounded slices
// rather than full-document pre-scan.

use super::emit::Emitter;
use super::punctuation::digits_both_sides;
use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::segment::BoundaryBitmap;
use crate::engine::sentence::is_sentence_terminator;
use crate::engine::zhtype::ChineseType;
use crate::rules::ruleset::{Issue, IssueType, ProfileConfig, Register};

use super::rule_ir::{self, MatchContext};
use super::{
    clamp_at_excluded, is_cjk_ideograph, resolve_register, PositionalClue, Scanner,
    CONTEXT_WINDOW_CHARS, POSITIONAL_WINDOW_CHARS,
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

/// What a spelling pass does with a hit of a register-gated rule.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GateMode {
    /// Stop at the first hit the document's register rejects.
    Stop,
    /// Skip every such hit.
    Skip,
    /// Gates are not consulted: the automaton holds no rejected rule.
    Off,
}

/// State a spelling pass carries beside the emitter.
struct PassState<'a> {
    register: &'a std::cell::OnceCell<Register>,
    zh_type: ChineseType,
    cfg: &'a ProfileConfig,
    clue_buf: &'a mut Vec<(usize, u16)>,
    clue_index_built: bool,
    boundary_bitmap: &'a BoundaryBitmap,
}

impl Scanner {
    /// Single-pass spelling scan with BoundaryBitmap fast-path.
    ///
    /// Uses the IR-based evaluation path: each AC hit is evaluated against
    /// its precompiled predicate chain via `eval_predicates()`.
    ///
    /// A register-gated rule the document's register rejects must not take
    /// part in longest-match resolution, or it would hide a shorter rule
    /// inside it. The first pass runs the full automaton and stops at the
    /// first such hit; the scan then discards its issues and makes one pass
    /// over the automaton without the rejected rules. If that automaton could
    /// not be built, the pass runs over the full one and skips rejected hits.
    pub(crate) fn scan_spelling(
        &self,
        em: &mut Emitter<'_>,
        zh_type: ChineseType,
        cfg: &ProfileConfig,
        clue_buf: &mut Vec<(usize, u16)>,
        boundary_bitmap: &BoundaryBitmap,
        register: &std::cell::OnceCell<Register>,
    ) {
        let db = &self.spelling_db;
        let base_len = em.issues.len();

        // Lazy clue index.
        clue_buf.clear();
        let mut st = PassState {
            register,
            zh_type,
            cfg,
            clue_buf,
            clue_index_built: false,
            boundary_bitmap,
        };

        let first_mode = if db.has_register_gates {
            GateMode::Stop
        } else {
            GateMode::Off
        };
        let Some(reg) = self.spelling_pass(em, &mut st, db.patterns.hits(em.text), first_mode)
        else {
            return;
        };

        em.issues.truncate(base_len);
        let subset = db.register_subset(reg);
        if subset.is_usable() {
            self.spelling_pass(em, &mut st, subset.hits(em.text), GateMode::Off);
        } else {
            self.spelling_pass(em, &mut st, db.patterns.hits(em.text), GateMode::Skip);
        }
    }

    /// One pass over the hits. Returns the register that stopped it, in
    /// GateMode::Stop only.
    fn spelling_pass(
        &self,
        em: &mut Emitter<'_>,
        st: &mut PassState<'_>,
        hits: impl Iterator<Item = (usize, usize, usize)>,
        gate_mode: GateMode,
    ) -> Option<Register> {
        let Emitter {
            text,
            excluded,
            issues,
            ..
        } = em;
        let register = st.register;
        let (text, excluded) = (*text, *excluded);
        let (cfg, zh_type, boundary_bitmap) = (st.cfg, st.zh_type, st.boundary_bitmap);
        let n_rules = self.spelling_db.spelling_rules.len();
        let mut excl_cursor: usize = 0;

        // Pre-compute profile gates. Variant, ai filler and translationese are
        // public families of their own, so the remaining rule types are what
        // the spelling family owns and skip_spelling gates.
        let skip_spelling = !cfg.spelling;
        let skip_variant = !cfg.variant_normalization || zh_type == ChineseType::Simplified;
        let skip_ai = !cfg.ai_filler_detection;
        let skip_translationese = !cfg.translationese_detection;

        for (start, end, idx) in hits {
            if idx >= n_rules {
                continue;
            }
            let compiled = &self.spelling_db.rules[idx];

            if gate_mode != GateMode::Off {
                if let Some(gate) = compiled.register {
                    let reg =
                        *register.get_or_init(|| resolve_register(text, excluded, cfg.register));
                    if !gate.admits(reg) {
                        if gate_mode == GateMode::Stop {
                            return Some(reg);
                        }
                        continue;
                    }
                }
            }

            // Fast-reject profile-gated rules.
            {
                use crate::rules::ruleset::RuleType;
                match compiled.rule_type {
                    RuleType::Variant if skip_variant => continue,
                    RuleType::AiFiller if skip_ai => continue,
                    RuleType::Translationese if skip_translationese => continue,
                    other if skip_spelling && other.in_spelling_family() => continue,
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
            if !st.clue_index_built && (class == CLASS_CLUED || class == CLASS_FULL) {
                rule_ir::build_clue_index_into(
                    self.spelling_db.clue_ac.as_ref(),
                    text,
                    st.clue_buf,
                );
                st.clue_index_built = true;
            }

            // Document-level fast path: if the clue index is empty and this
            // rule requires positive clue matches, it will always be rejected.
            // Skip MatchContext construction entirely.
            if st.clue_index_built
                && st.clue_buf.is_empty()
                && self.spelling_db.rule_pos_clue_ids[idx].is_some()
            {
                continue;
            }

            if class == CLASS_TRULY_SIMPLE {
                // Inline fast path: no MatchContext, no function call.
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
                        compiled.severity,
                        compiled.rule_idx,
                    ));
                }
            } else {
                // CLASS_SIMPLE has superstring/exception/deletion but no clue
                // checks, so it passes an empty clue slice. CLASS_CLUED and
                // CLASS_FULL need the document-wide clue index.
                let clue_index: &[(usize, u16)] = if class == CLASS_SIMPLE {
                    &[]
                } else {
                    st.clue_buf.as_slice()
                };
                let mut ctx = MatchContext {
                    text,
                    excluded,
                    excl_cursor: &mut excl_cursor,
                    cfg,
                    zh_type,
                    start,
                    end,
                    clue_index,
                    boundary_bitmap,
                };
                if let Some(issue) =
                    rule_ir::eval_predicates(&self.spelling_db, compiled, &mut ctx, &self.segmenter)
                {
                    issues.push(issue);
                }
            }
        }
        None
    }
}

/// Compute the byte-offset window for context-clue proximity checks,
/// clamped at sentence ends, paragraph breaks and excluded-range boundaries.
pub(crate) fn context_byte_window(
    text: &str,
    match_start: usize,
    match_end: usize,
    excluded: &[ByteRange],
) -> (usize, usize) {
    let bytes = text.as_bytes();
    let max_search = CONTEXT_WINDOW_CHARS * 4;
    let search_start = text.ceil_char_boundary(match_start.saturating_sub(max_search));
    let search_end = text.floor_char_boundary((match_end + max_search).min(text.len()));
    let para_start = find_last_paragraph_break(&bytes[search_start..match_start])
        .map_or(0, |pos| search_start + pos + 1);
    let para_end = find_first_paragraph_break(&bytes[match_end..search_end])
        .map_or(text.len(), |pos| match_end + pos);
    let para_start = last_mark_end(text, search_start, match_start, is_window_sentence_end)
        .map_or(para_start, |p| para_start.max(p));
    let para_end = first_mark(text, match_end, search_end, is_window_sentence_end)
        .map_or(para_end, |p| para_end.min(p));

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

/// Whether a positional clue term occurs in text[ws..we].
///
/// An ASCII term matches only as a whole word: an ASCII letter directly beside
/// it means it is part of a longer word, so CI is not found in ASCII or PCIe
/// and kg is not found in pkg. A digit beside it still counts, because 5kg is
/// the unit written against its number. Other terms match as substrings.
fn clue_in(text: &str, ws: usize, we: usize, term: &str) -> bool {
    if !term.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return text[ws..we].contains(term);
    }
    let bytes = text.as_bytes();
    text[ws..we].match_indices(term).any(|(i, _)| {
        let (s, e) = (ws + i, ws + i + term.len());
        !(s > 0 && bytes[s - 1].is_ascii_alphabetic())
            && !bytes.get(e).is_some_and(u8::is_ascii_alphabetic)
    })
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
                if !clue_in(text, ws, we, term) {
                    return false;
                }
            }
            PositionalClue::After(term) => {
                let (ws, we) = *before_win
                    .get_or_insert_with(|| positional_bounds_before(text, start, excluded));
                if !clue_in(text, ws, we, term) {
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
                if clue_in(text, ws, we, term) {
                    return false;
                }
            }
            PositionalClue::NotAfter(term) => {
                let (ws, we) = *before_win
                    .get_or_insert_with(|| positional_bounds_before(text, start, excluded));
                if clue_in(text, ws, we, term) {
                    return false;
                }
            }
            PositionalClue::NotBeforeClause(term) => {
                let (ws, we) =
                    *after_win.get_or_insert_with(|| positional_bounds_after(text, end, excluded));

                // A colon right after the match introduces its own value, as in
                // the spec-sheet label 產品質量：兩公斤, so it does not close
                // the clause; a colon further along does.
                let from = ws
                    + text[ws..we]
                        .chars()
                        .next()
                        .filter(|c| matches!(c, '：' | ':'))
                        .map_or(0, char::len_utf8);
                let we = first_mark(text, from, we, is_clause_mark).unwrap_or(we);
                if clue_in(text, ws, we, term) {
                    return false;
                }
            }
            PositionalClue::NotAfterClause(term) => {
                let (ws, we) = *before_win
                    .get_or_insert_with(|| positional_bounds_before(text, start, excluded));
                let ws = last_mark_end(text, ws, we, is_clause_mark).unwrap_or(ws);
                if clue_in(text, ws, we, term) {
                    return false;
                }
            }
        }
    }
    true
}

/// A sentence end for the clue windows: the shared splitter's answer, plus a
/// half-width period written straight against a Chinese character. The
/// splitter wants whitespace after a Latin period, which suits English, but
/// 他順利通過.系統稍後更新 is two sentences all the same. Kept local so the
/// detectors' sentence splits do not move.
fn is_window_sentence_end(text: &str, offset: usize, c: char) -> bool {
    is_sentence_terminator(text, offset, c)
        || c == '.'
            && text[offset + 1..]
                .chars()
                .next()
                .is_some_and(is_cjk_ideograph)
}

/// A positional clue names a neighbour in the same clause, which ends at a
/// sentence mark or clause punctuation. An ASCII comma or colon between digits
/// (1,000 and 12:30) is notation, not punctuation.
fn is_clause_mark(text: &str, offset: usize, c: char) -> bool {
    match c {
        '，' | '；' | ';' | '：' => true,
        ',' | ':' => !digits_both_sides(text.as_bytes(), offset),
        _ => is_window_sentence_end(text, offset, c),
    }
}

type MarkFn = fn(&str, usize, char) -> bool;

fn first_mark(text: &str, start: usize, end: usize, is_mark: MarkFn) -> Option<usize> {
    text[start..end]
        .char_indices()
        .find_map(|(i, c)| is_mark(text, start + i, c).then_some(start + i))
}

fn last_mark_end(text: &str, start: usize, end: usize, is_mark: MarkFn) -> Option<usize> {
    text[start..end]
        .char_indices()
        .rev()
        .find_map(|(i, c)| is_mark(text, start + i, c).then_some(start + i + c.len_utf8()))
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
#[path = "../../../tests/unit/engine/scan/spelling/tests.rs"]
mod tests;
