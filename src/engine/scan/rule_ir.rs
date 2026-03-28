// Intermediate representation for compiled spelling rule predicates.
//
// Each SpellingRule compiles into a CompiledRule: an ordered chain of
// MatchPredicate values that the evaluator walks sequentially, short-
// circuiting on the first rejection.  This IR sits between the raw
// SpellingRule struct (declarative) and the evaluation loop (imperative),
// enabling future optimizations (predicate reordering, dead-predicate
// elimination) without touching the evaluator.
//
// Scope: per-match predicates only.  Overlap resolution, anchor
// confirmation, and TM suppression operate at different granularity
// and are explicitly excluded.

use std::collections::{HashMap, HashSet};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use daachorse::{CharwiseDoubleArrayAhoCorasickBuilder, MatchKind as DaacMatchKind};

use crate::engine::excluded::ByteRange;
use crate::engine::segment::BoundaryBitmap;
use crate::engine::zhtype::ChineseType;
use crate::rules::ruleset::{Issue, IssueType, ProfileConfig, RuleType, SpellingRule};

use super::spelling;
use super::PositionalClue;

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// A single predicate in a compiled rule's filter chain.
///
/// Evaluation order matters: cheapest checks first, most-selective last.
/// The evaluator short-circuits on the first `Reject*` that fires or
/// the first `Require*` that fails.
#[derive(Debug, Clone)]
pub enum MatchPredicate {
    // -- Profile / config gates (const-eval candidates) --
    /// Rule fires only when variant normalization is enabled AND input
    /// is not Simplified Chinese.  Maps to RuleType::Variant gate.
    RequireVariantConfig,

    /// Rule fires only when AI filler detection is enabled.
    /// Maps to RuleType::AiFiller gate.
    RequireAiFillerDetection,

    /// Rule fires only when the political stance config allows it.
    /// Maps to RuleType::PoliticalColoring gate.
    RequireStanceAllow,

    // -- Exclusion / overlap filters --
    /// Reject if the match byte range overlaps any exclusion zone.
    /// Uses an advancing cursor for amortized O(1).
    RejectIfExcluded,

    /// Reject if the matched text already equals one of the rule's
    /// suggested replacements (superstring absorption).
    /// Each entry: (replacement_string, precomputed_from_offsets).
    RejectIfSuperstring { forms: Vec<(String, Vec<usize>)> },

    /// Reject if the match straddles an MMSEG word boundary at either
    /// the start or end offset.
    RejectIfWordBoundaryStraddles,

    /// Reject if the match falls inside an exception phrase.
    /// Each entry: (exception_phrase, precomputed_from_offsets).
    RejectIfInExceptionPhrase {
        exceptions: Vec<(String, Vec<usize>)>,
    },

    // -- Context-clue gate (fused positive + negative, one window + one AC pass) --
    /// Evaluate context clues in a single windowed AC scan.
    /// Rejects if positive clue count < min_matches OR any negative clue fires.
    /// Either pos_ids or neg_ids (or both) may be empty.
    CheckClues {
        pos_ids: Vec<u16>,
        neg_ids: Vec<u16>,
        min_pos_matches: u32,
    },

    // -- Positional-clue gate (dead-code-eliminated for CLASS_SIMPLE/CLUED) --
    /// Require all positional clues to match (AND semantics).
    /// Stores pre-parsed PositionalClue values to avoid per-eval allocation.
    RequirePositionalClues { clues: Vec<PositionalClue> },

    // -- Span mutation --
    /// For deletion rules: extend the match span to consume a trailing
    /// fullwidth comma or colon if present and not excluded.
    MayExtendDeletionSpan,
}

// ---------------------------------------------------------------------------
// Emit template
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Compiled rule
// ---------------------------------------------------------------------------

/// A fully compiled spelling rule: an ordered predicate chain.
/// The evaluator walks `predicates` in order, short-circuiting on
/// rejection, then constructs the Issue from `db.spelling_rules[rule_idx]`.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// The original rule index in the spelling_rules array.
    pub rule_idx: usize,
    /// Ordered predicate chain.  Evaluated left-to-right; first
    /// rejection aborts.
    pub predicates: Vec<MatchPredicate>,
    /// Cached rule type (avoids pointer chase through spelling_rules).
    pub rule_type: RuleType,
    /// Cached filter flags (avoids pointer chase through rule_filter_flags).
    pub filter_flags: u8,
}

// ---------------------------------------------------------------------------
// Compiled spelling database
// ---------------------------------------------------------------------------

/// The compiled spelling rule database.  Owns the AC automata and
/// all per-rule compiled data.  Constructed by `compile_spelling_rules()`.
pub struct CompiledSpellingDb {
    /// Charwise double-array Aho-Corasick (primary).
    /// Not Debug because daachorse types don't implement it.
    pub ac_charwise: Option<daachorse::CharwiseDoubleArrayAhoCorasick<usize>>,
    /// Bytewise Aho-Corasick (fallback when charwise build fails).
    pub ac_bytewise: Option<AhoCorasick>,
    /// Compiled rules indexed by pattern ID (same order as AC patterns).
    pub rules: Vec<CompiledRule>,
    /// Interned clue strings for windowed AC lookup.
    pub clue_ac: Option<AhoCorasick>,
    /// Absorber strings (exception phrases + superstring `to` forms).
    pub absorber_strings: Vec<String>,

    // -- Per-rule parallel arrays (indexed by rule position) --
    // These arrays are populated during compile_spelling_rules() and used
    // by structural validation tests (filter_flags_match_rule_properties,
    // filter_vecs_aligned, rule_classes_match_filter_flags).
    /// The spelling rules themselves (filtered and deduplicated).
    pub spelling_rules: Vec<SpellingRule>,
    /// Precomputed suggestions per rule (avoids per-match allocation).
    pub spelling_suggestions: Vec<Vec<String>>,
    /// Per-rule positive clue IDs into the clue AC pattern list.
    #[allow(dead_code)]
    pub rule_pos_clue_ids: Vec<Option<Vec<u16>>>,
    /// Per-rule negative clue IDs into the clue AC pattern list.
    #[allow(dead_code)]
    pub rule_neg_clue_ids: Vec<Option<Vec<u16>>>,
    /// Per-rule parsed positional clues (checked after context-clue gate).
    #[allow(dead_code)]
    pub rule_positional_clues: Vec<Option<Vec<PositionalClue>>>,
    /// Per-rule bitflags gating optional filter stages (spelling::FILTER_*).
    /// Runtime-dead after compilation (cached in CompiledRule::filter_flags);
    /// kept for test inspection.
    #[allow(dead_code)]
    pub rule_filter_flags: Vec<u8>,
    /// Per-rule dispatch class for monomorphic fast paths (spelling::CLASS_*).
    pub rule_classes: Vec<u8>,
}

impl std::fmt::Debug for CompiledSpellingDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSpellingDb")
            .field("ac_charwise", &self.ac_charwise.is_some())
            .field("ac_bytewise", &self.ac_bytewise.is_some())
            .field("rules", &self.rules.len())
            .field("clue_ac", &self.clue_ac.is_some())
            .field("absorber_strings", &self.absorber_strings.len())
            .field("spelling_rules", &self.spelling_rules.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Match context (borrowed evaluation state)
// ---------------------------------------------------------------------------

/// Borrowed state needed to evaluate a predicate chain against a single
/// AC match.  All fields are references into caller-owned data, so the
/// evaluator allocates nothing.
pub struct MatchContext<'a> {
    pub text: &'a str,
    pub excluded: &'a [ByteRange],
    pub excl_cursor: &'a mut usize,
    pub cfg: &'a ProfileConfig,
    pub zh_type: ChineseType,
    /// Byte start of the AC match.
    pub start: usize,
    /// Byte end of the AC match (may be extended by MayExtendDeletionSpan).
    pub end: usize,
    /// Pre-computed clue hit index from document-wide clue AC scan.
    /// Sorted by byte offset for binary-search lookup.
    pub clue_index: &'a [(usize, u16)],
    /// Pre-computed boundary bitmap for word-straddle fast-path filtering.
    /// Provides both `start_crossed` and `end_needs_segmenter` lookups.
    pub boundary_bitmap: &'a BoundaryBitmap,
}

/// Build a document-wide clue hit index by running the clue AC once
/// over the full text, appending into a caller-provided buffer.
/// Avoids allocation when the buffer has been pre-allocated / reused.
pub fn build_clue_index_into(
    clue_ac: Option<&AhoCorasick>,
    text: &str,
    out: &mut Vec<(usize, u16)>,
) {
    let Some(ac) = clue_ac else {
        return;
    };
    out.extend(
        ac.find_overlapping_iter(text)
            .map(|m| (m.start(), m.pattern().as_usize() as u16)),
    );
    // Already sorted by start offset (AC iterates left-to-right),
    // but sort to guarantee for binary search.
    out.sort_unstable_by_key(|&(off, _)| off);
}

/// Look up clue hits within a byte window from the pre-computed index.
/// Returns `(positive_match_count, any_negative_match)`.
pub fn lookup_clues_in_window(
    clue_index: &[(usize, u16)],
    win_start: usize,
    win_end: usize,
    pos_ids: Option<&[u16]>,
    neg_ids: Option<&[u16]>,
) -> (usize, bool) {
    if clue_index.is_empty() || (pos_ids.is_none() && neg_ids.is_none()) {
        return (0, false);
    }

    // Binary search for the first hit at or after win_start.
    let lo = clue_index.partition_point(|&(off, _)| off < win_start);

    let mut pos_seen: u32 = 0;
    let mut pos_found = 0usize;

    for &(off, clue_id) in &clue_index[lo..] {
        if off >= win_end {
            break;
        }

        if let Some(pos_ids) = pos_ids {
            if let Some(pos) = pos_ids.iter().position(|&id| id == clue_id) {
                let bit = 1u32 << pos;
                if pos_seen & bit == 0 {
                    pos_seen |= bit;
                    pos_found += 1;
                }
            }
        }

        if let Some(neg_ids) = neg_ids {
            if neg_ids.contains(&clue_id) {
                return (pos_found, true);
            }
        }
    }

    (pos_found, false)
}

// ---------------------------------------------------------------------------
// compile_rule_predicates()
// ---------------------------------------------------------------------------

/// Build the ordered predicate chain for a single rule.
///
/// The chain order mirrors the evaluation order in the old
/// process_match_dispatch: config gates first, then exclusion, superstring,
/// word-boundary, exception, context clues, positional clues, deletion span.
pub fn compile_rule_predicates(
    rule: &SpellingRule,
    flags: u8,
    pos_clue_ids: Option<&[u16]>,
    neg_clue_ids: Option<&[u16]>,
    positional_clues: Option<&[PositionalClue]>,
) -> Vec<MatchPredicate> {
    let mut preds = Vec::with_capacity(8);

    // -- Config gates (cheapest, evaluated first) --
    if rule.rule_type == RuleType::Variant {
        preds.push(MatchPredicate::RequireVariantConfig);
    }
    if rule.rule_type == RuleType::AiFiller {
        preds.push(MatchPredicate::RequireAiFillerDetection);
    }
    if rule.rule_type == RuleType::PoliticalColoring {
        preds.push(MatchPredicate::RequireStanceAllow);
    }

    // -- Exclusion check (amortized O(1), high reject rate) --
    preds.push(MatchPredicate::RejectIfExcluded);

    // -- Superstring absorption (precomputed from-offsets) --
    if flags & spelling::FILTER_HAS_SUPERSTRING != 0 {
        let forms: Vec<(String, Vec<usize>)> = rule
            .to
            .iter()
            .filter(|t| t.contains(&rule.from))
            .map(|t| {
                let offsets: Vec<usize> = t.match_indices(&rule.from).map(|(pos, _)| pos).collect();
                (t.clone(), offsets)
            })
            .collect();
        if !forms.is_empty() {
            preds.push(MatchPredicate::RejectIfSuperstring { forms });
        }
    }

    // -- Context clues BEFORE boundary (clue window scan is cheaper than
    //    segmenter and has high reject rate for clue-gated rules) --
    {
        let pos = pos_clue_ids
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.to_vec())
            .unwrap_or_default();
        let neg = neg_clue_ids
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.to_vec())
            .unwrap_or_default();
        if !pos.is_empty() || !neg.is_empty() {
            preds.push(MatchPredicate::CheckClues {
                pos_ids: pos,
                neg_ids: neg,
                min_pos_matches: if pos_clue_ids.is_some_and(|ids| !ids.is_empty()) {
                    super::MIN_SCAN_CLUE_MATCHES as u32
                } else {
                    0
                },
            });
        }
    }

    // -- Word boundary straddle (expensive: segmenter call) --
    preds.push(MatchPredicate::RejectIfWordBoundaryStraddles);

    // -- Exception phrases (precomputed from-offsets) --
    if flags & spelling::FILTER_HAS_EXCEPTIONS != 0 {
        if let Some(ref exc_list) = rule.exceptions {
            let exceptions: Vec<(String, Vec<usize>)> = exc_list
                .iter()
                .map(|exc| {
                    let offsets: Vec<usize> =
                        exc.match_indices(&rule.from).map(|(pos, _)| pos).collect();
                    (exc.clone(), offsets)
                })
                .collect();
            if !exceptions.is_empty() {
                preds.push(MatchPredicate::RejectIfInExceptionPhrase { exceptions });
            }
        }
    }

    // -- Positional clue gate --
    if let Some(clues) = positional_clues {
        if !clues.is_empty() {
            preds.push(MatchPredicate::RequirePositionalClues {
                clues: clues.to_vec(),
            });
        }
    }

    // -- Deletion span extension (last, mutates the match end) --
    if flags & spelling::FILTER_IS_DELETION != 0 {
        preds.push(MatchPredicate::MayExtendDeletionSpan);
    }

    preds
}

// ---------------------------------------------------------------------------
// eval_simple() -- fast path for CLASS_SIMPLE (no clues, no positional)
// ---------------------------------------------------------------------------

/// Inlined fast path for CLASS_SIMPLE rules (83% of all rules).
///
/// Evaluates the fixed predicate sequence without Vec iteration or enum
/// matching.  Predicates: config gate → excluded → superstring → boundary →
/// exception → deletion.  No clue or positional checks.
#[inline]
pub fn eval_simple(
    db: &CompiledSpellingDb,
    rule: &CompiledRule,
    ctx: &mut MatchContext<'_>,
    segmenter: &super::super::segment::Segmenter,
) -> Option<Issue> {
    let flags = rule.filter_flags;
    let mut end = ctx.end;

    // Config gates (using cached rule_type, no pointer chase).
    match rule.rule_type {
        RuleType::Variant => {
            if !ctx.cfg.variant_normalization || ctx.zh_type == ChineseType::Simplified {
                return None;
            }
        }
        RuleType::AiFiller => {
            if !ctx.cfg.ai_filler_detection {
                return None;
            }
        }
        RuleType::PoliticalColoring => {
            let sr = &db.spelling_rules[rule.rule_idx];
            if !ctx.cfg.political_stance.allows_rule(&sr.from) {
                return None;
            }
        }
        _ => {}
    }

    // Exclusion check (amortized O(1)).
    while *ctx.excl_cursor < ctx.excluded.len() && ctx.excluded[*ctx.excl_cursor].end <= ctx.start {
        *ctx.excl_cursor += 1;
    }
    if *ctx.excl_cursor < ctx.excluded.len()
        && ctx.excluded[*ctx.excl_cursor].start < end
        && ctx.start < ctx.excluded[*ctx.excl_cursor].end
    {
        return None;
    }

    // Superstring absorption & Exception phrases (using precomputed predicates).
    if flags & (spelling::FILTER_HAS_SUPERSTRING | spelling::FILTER_HAS_EXCEPTIONS) != 0 {
        for pred in &rule.predicates {
            match pred {
                MatchPredicate::RejectIfSuperstring { forms } => {
                    let absorbed = forms.iter().any(|(correct, offsets)| {
                        offsets.iter().any(|&wrong_pos| {
                            if let Some(correct_start) = ctx.start.checked_sub(wrong_pos) {
                                let correct_end = correct_start
                                    .saturating_add(correct.len())
                                    .min(ctx.text.len());
                                ctx.text.get(correct_start..correct_end) == Some(correct.as_str())
                            } else {
                                false
                            }
                        })
                    });
                    if absorbed {
                        return None;
                    }
                }
                MatchPredicate::RejectIfInExceptionPhrase { exceptions } => {
                    let in_exception = exceptions.iter().any(|(exc, offsets)| {
                        offsets.iter().any(|&pos| {
                            if let Some(exc_start) = ctx.start.checked_sub(pos) {
                                let exc_end =
                                    exc_start.saturating_add(exc.len()).min(ctx.text.len());
                                ctx.text.get(exc_start..exc_end) == Some(exc.as_str())
                            } else {
                                false
                            }
                        })
                    });
                    if in_exception {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }

    // Word boundary straddle.
    if ctx.boundary_bitmap.is_empty() {
        if segmenter.match_straddles_word_boundary(ctx.text, ctx.start, end) {
            return None;
        }
    } else {
        if ctx.boundary_bitmap.start_straddles(ctx.start) {
            return None;
        }
        if ctx.boundary_bitmap.end_straddles(end, ctx.start) {
            return None;
        }
    }

    // Deletion span extension (cursor-aware: excl_cursor is already positioned
    // past the match start from the earlier exclusion check).
    if flags & spelling::FILTER_IS_DELETION != 0 && end <= ctx.text.len() {
        if let Some(c) = ctx.text.get(end..).and_then(|s| s.chars().next()) {
            if matches!(c, '\u{FF0C}' | '\u{FF1A}') {
                let extended = end.saturating_add(c.len_utf8());
                if extended <= ctx.text.len() {
                    // Use cursor: the exclusion zone at excl_cursor (if any)
                    // starts at or after ctx.start.  If it overlaps [end, extended),
                    // the extension is inside an excluded range.
                    let excluded_overlap = *ctx.excl_cursor < ctx.excluded.len()
                        && ctx.excluded[*ctx.excl_cursor].start < extended
                        && end < ctx.excluded[*ctx.excl_cursor].end;
                    if !excluded_overlap {
                        end = extended;
                    }
                }
            }
        }
    }

    // Emit lightweight issue using cached rule_type (no sr dereference).
    let mut issue = Issue::new(
        ctx.start,
        end - ctx.start,
        "",
        Vec::new(),
        IssueType::from(rule.rule_type),
        rule.rule_type.default_severity(),
    );
    issue.spelling_rule_idx = Some(rule.rule_idx);
    Some(issue)
}

// ---------------------------------------------------------------------------
// eval_predicates() -- generic path for CLASS_CLUED and CLASS_FULL
// ---------------------------------------------------------------------------

/// Evaluate a compiled rule's predicate chain against a match context.
///
/// Returns Some(Issue) when all predicates pass, None on first rejection.
pub fn eval_predicates(
    db: &CompiledSpellingDb,
    rule: &CompiledRule,
    ctx: &mut MatchContext<'_>,
    segmenter: &super::super::segment::Segmenter,
) -> Option<Issue> {
    let sr = &db.spelling_rules[rule.rule_idx];
    let mut end = ctx.end;

    for pred in &rule.predicates {
        match pred {
            MatchPredicate::RequireVariantConfig => {
                if !ctx.cfg.variant_normalization || ctx.zh_type == ChineseType::Simplified {
                    return None;
                }
            }
            MatchPredicate::RequireAiFillerDetection => {
                if !ctx.cfg.ai_filler_detection {
                    return None;
                }
            }
            MatchPredicate::RequireStanceAllow => {
                if !ctx.cfg.political_stance.allows_rule(&sr.from) {
                    return None;
                }
            }
            MatchPredicate::RejectIfExcluded => {
                // Advancing-cursor exclusion check (amortized O(1)).
                while *ctx.excl_cursor < ctx.excluded.len()
                    && ctx.excluded[*ctx.excl_cursor].end <= ctx.start
                {
                    *ctx.excl_cursor += 1;
                }
                if *ctx.excl_cursor < ctx.excluded.len()
                    && ctx.excluded[*ctx.excl_cursor].start < end
                    && ctx.start < ctx.excluded[*ctx.excl_cursor].end
                {
                    return None;
                }
            }
            MatchPredicate::RejectIfSuperstring { forms } => {
                let absorbed = forms.iter().any(|(correct, offsets)| {
                    offsets.iter().any(|&wrong_pos| {
                        if let Some(correct_start) = ctx.start.checked_sub(wrong_pos) {
                            let correct_end = correct_start
                                .saturating_add(correct.len())
                                .min(ctx.text.len());
                            ctx.text.get(correct_start..correct_end) == Some(correct.as_str())
                        } else {
                            false
                        }
                    })
                });
                if absorbed {
                    return None;
                }
            }
            MatchPredicate::RejectIfWordBoundaryStraddles => {
                if ctx.boundary_bitmap.is_empty() {
                    if segmenter.match_straddles_word_boundary(ctx.text, ctx.start, end) {
                        return None;
                    }
                } else {
                    if ctx.boundary_bitmap.start_straddles(ctx.start) {
                        return None;
                    }
                    if ctx.boundary_bitmap.end_straddles(end, ctx.start) {
                        return None;
                    }
                }
            }
            MatchPredicate::RejectIfInExceptionPhrase { exceptions } => {
                let in_exception = exceptions.iter().any(|(exc, offsets)| {
                    offsets.iter().any(|&pos| {
                        if let Some(exc_start) = ctx.start.checked_sub(pos) {
                            let exc_end = exc_start.saturating_add(exc.len()).min(ctx.text.len());
                            ctx.text.get(exc_start..exc_end) == Some(exc.as_str())
                        } else {
                            false
                        }
                    })
                });
                if in_exception {
                    return None;
                }
            }
            MatchPredicate::CheckClues {
                pos_ids,
                neg_ids,
                min_pos_matches,
            } => {
                // Fused context-clue gate using pre-computed document-wide index.
                let (win_start, win_end) =
                    spelling::context_byte_window(ctx.text, ctx.start, end, ctx.excluded);
                let pos_slice = if pos_ids.is_empty() {
                    None
                } else {
                    Some(pos_ids.as_slice())
                };
                let neg_slice = if neg_ids.is_empty() {
                    None
                } else {
                    Some(neg_ids.as_slice())
                };
                let (pos_matches, any_neg) = lookup_clues_in_window(
                    ctx.clue_index,
                    win_start,
                    win_end,
                    pos_slice,
                    neg_slice,
                );
                if !pos_ids.is_empty() && pos_matches < *min_pos_matches as usize {
                    return None;
                }
                if any_neg {
                    return None;
                }
            }
            MatchPredicate::RequirePositionalClues { clues } => {
                if !spelling::check_positional_clues(ctx.text, ctx.start, end, ctx.excluded, clues)
                {
                    return None;
                }
            }
            MatchPredicate::MayExtendDeletionSpan => {
                if end <= ctx.text.len() {
                    if let Some(c) = ctx.text.get(end..).and_then(|s| s.chars().next()) {
                        if matches!(c, '\u{FF0C}' | '\u{FF1A}') {
                            let extended = end.saturating_add(c.len_utf8());
                            if extended <= ctx.text.len() {
                                let excluded_overlap = *ctx.excl_cursor < ctx.excluded.len()
                                    && ctx.excluded[*ctx.excl_cursor].start < extended
                                    && end < ctx.excluded[*ctx.excl_cursor].end;
                                if !excluded_overlap {
                                    end = extended;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Emit lightweight issue (deferred: found/suggestions/context/english/
    // context_clues are empty and will be inflated after overlap resolution).
    let mut issue = Issue::new(
        ctx.start,
        end - ctx.start,
        "",
        Vec::new(),
        IssueType::from(sr.rule_type),
        sr.rule_type.default_severity(),
    );
    issue.spelling_rule_idx = Some(rule.rule_idx);
    Some(issue)
}

// ---------------------------------------------------------------------------
// inflate_spelling_issues()
// ---------------------------------------------------------------------------

/// Inflate deferred spelling issues after overlap resolution.
///
/// Issues with `spelling_rule_idx = Some(idx)` have empty found,
/// suggestions, context, english, and context_clues.  This function
/// fills them in from the compiled DB and the original text.
/// Non-spelling issues are left untouched.
pub fn inflate_spelling_issues(db: &CompiledSpellingDb, text: &str, issues: &mut [Issue]) {
    for issue in issues.iter_mut() {
        if let Some(idx) = issue.spelling_rule_idx.take() {
            let sr = &db.spelling_rules[idx];
            // For deletion rules, the span may have been extended to absorb
            // trailing punctuation.  Use `rule.from.len()` for `found` so
            // users see the phrase to delete, not the absorbed punctuation.
            let found_len = if sr.to.first().is_some_and(|t| t.is_empty()) || sr.to.is_empty() {
                sr.from.len()
            } else {
                issue.length
            };
            let end = issue.offset.saturating_add(found_len).min(text.len());
            if let Some(s) = text.get(issue.offset..end) {
                issue.found = s.to_string();
            }
            issue.suggestions = db.spelling_suggestions[idx].clone();
            issue.context.clone_from(&sr.context);
            issue.english.clone_from(&sr.english);
            issue.context_clues.clone_from(&sr.context_clues);
        }
    }
}

// ---------------------------------------------------------------------------
// compile_spelling_rules()
// ---------------------------------------------------------------------------

/// Compile a set of spelling rules into a `CompiledSpellingDb`.
///
/// Filters disabled rules, deduplicates by `from` key (last wins),
/// builds AC automata (charwise primary, bytewise fallback), interns
/// context clues, and computes per-rule filter flags and dispatch classes.
pub fn compile_spelling_rules(
    spelling_rules: Vec<SpellingRule>,
) -> anyhow::Result<CompiledSpellingDb> {
    let mut spelling_rules: Vec<SpellingRule> =
        spelling_rules.into_iter().filter(|r| !r.disabled).collect();

    // Deduplicate by `from` key (last wins; overrides come after embedded).
    {
        let mut seen = HashSet::new();
        let mut i = spelling_rules.len();
        while i > 0 {
            i -= 1;
            if !seen.insert(spelling_rules[i].from.clone()) {
                spelling_rules.remove(i);
            }
        }
    }

    // Deduplicate context clues within each rule.
    for rule in &mut spelling_rules {
        if let Some(ref mut clues) = rule.context_clues {
            let mut seen = HashSet::new();
            clues.retain(|c| seen.insert(c.clone()));
        }
        if let Some(ref mut clues) = rule.negative_context_clues {
            let mut seen = HashSet::new();
            clues.retain(|c| seen.insert(c.clone()));
        }
    }

    let spelling_suggestions: Vec<Vec<String>> = spelling_rules
        .iter()
        .map(super::effective_suggestions)
        .collect();

    // Build clue AC: intern all unique clue strings, map per-rule clue
    // lists to indices, build a bytewise AC for windowed lookups.
    let (clue_ac, mut rule_pos_clue_ids, mut rule_neg_clue_ids) = {
        let mut clue_map: HashMap<String, u16> = HashMap::new();
        let mut clue_vec: Vec<String> = Vec::new();

        let mut intern_clue = |s: &String| -> Option<u16> {
            if let Some(&idx) = clue_map.get(s) {
                Some(idx)
            } else {
                let idx = match u16::try_from(clue_vec.len()) {
                    Ok(i) => i,
                    Err(_) => {
                        eprintln!(
                            "[zhtw-mcp] clue index overflow (>{} unique clues); \
                             remaining clues will be ignored",
                            u16::MAX
                        );
                        return None;
                    }
                };
                clue_map.insert(s.clone(), idx);
                clue_vec.push(s.clone());
                Some(idx)
            }
        };

        let mut pos_ids: Vec<Option<Vec<u16>>> = Vec::with_capacity(spelling_rules.len());
        let mut neg_ids: Vec<Option<Vec<u16>>> = Vec::with_capacity(spelling_rules.len());

        let mut intern_clues = |clues: &Option<Vec<String>>| -> Option<Vec<u16>> {
            let clues = clues.as_ref().filter(|c| !c.is_empty())?;
            let ids: Vec<u16> = clues.iter().filter_map(&mut intern_clue).collect();
            if ids.is_empty() {
                None
            } else {
                Some(ids)
            }
        };

        for rule in &spelling_rules {
            pos_ids.push(intern_clues(&rule.context_clues));
            neg_ids.push(intern_clues(&rule.negative_context_clues));
        }

        let ac = if clue_vec.is_empty() {
            None
        } else {
            match AhoCorasickBuilder::new()
                .match_kind(MatchKind::Standard)
                .build(&clue_vec)
            {
                Ok(ac) => Some(ac),
                Err(e) => {
                    eprintln!("[zhtw-mcp] clue AC build failed: {e}");
                    None
                }
            }
        };

        (ac, pos_ids, neg_ids)
    };

    // Validate clue-ID counts fit the fixed bitset (capacity 32).
    // Truncate rather than panic on malformed rulesets.
    let truncate_clue_ids = |ids_vec: &mut Vec<Option<Vec<u16>>>, label: &str| {
        for (i, slot) in ids_vec.iter_mut().enumerate() {
            if let Some(ids) = slot {
                if ids.len() > 32 {
                    eprintln!(
                        "[zhtw-mcp] rule '{}' has {} {label} clues, \
                         exceeds bitset capacity 32; truncating",
                        spelling_rules[i].from,
                        ids.len(),
                    );
                    ids.truncate(32);
                }
            }
        }
    };
    truncate_clue_ids(&mut rule_pos_clue_ids, "positive");
    truncate_clue_ids(&mut rule_neg_clue_ids, "negative");

    let rule_positional_clues: Vec<Option<Vec<PositionalClue>>> = spelling_rules
        .iter()
        .map(|rule| {
            rule.positional_clues.as_ref().and_then(|raw| {
                let parsed: Vec<PositionalClue> = raw
                    .iter()
                    .filter_map(|s| {
                        let clue = PositionalClue::parse(s);
                        if clue.is_none() {
                            eprintln!(
                                "[zhtw-mcp] rule '{}': unrecognized positional clue '{}'",
                                rule.from, s
                            );
                        }
                        clue
                    })
                    .collect();
                if parsed.is_empty() {
                    None
                } else {
                    Some(parsed)
                }
            })
        })
        .collect();

    let spelling_patterns: Vec<&str> = spelling_rules.iter().map(|r| r.from.as_str()).collect();

    // Absorption patterns: exception phrases and superstring `to` forms
    // injected into the AC so LeftmostLongest suppresses shorter `from`
    // matches.  Indices >= spelling_rules.len() act as sentinels.
    let absorber_strings: Vec<String> = {
        let from_set: HashSet<&str> = spelling_patterns.iter().copied().collect();
        let mut candidates: Vec<(String, &str)> = Vec::new();
        let mut dedup = HashSet::new();
        for rule in &spelling_rules {
            if let Some(ref exceptions) = rule.exceptions {
                for exc in exceptions {
                    if exc.contains(&rule.from)
                        && !from_set.contains(exc.as_str())
                        && dedup.insert(exc.clone())
                    {
                        candidates.push((exc.clone(), rule.from.as_str()));
                    }
                }
            }
            for to in &rule.to {
                if to.contains(&rule.from)
                    && to != &rule.from
                    && !from_set.contains(to.as_str())
                    && dedup.insert(to.clone())
                {
                    candidates.push((to.clone(), rule.from.as_str()));
                }
            }
        }
        // Reject absorbers that would shadow a different rule's `from`.
        candidates
            .into_iter()
            .filter(|(absorber, orig_from)| {
                !spelling_patterns.iter().any(|&f| {
                    if f == *orig_from {
                        return false;
                    }
                    if absorber.contains(f) {
                        return true;
                    }
                    // Right-boundary overlap: proper suffix of absorber
                    // is a prefix of f.
                    let mut chars = absorber.char_indices();
                    chars.next(); // skip position 0 (full string)
                    for (byte_idx, _) in chars {
                        let suffix = &absorber[byte_idx..];
                        if f.starts_with(suffix) {
                            return true;
                        }
                    }
                    false
                })
            })
            .map(|(s, _)| s)
            .collect()
    };
    let all_patterns: Vec<&str> = spelling_patterns
        .iter()
        .copied()
        .chain(absorber_strings.iter().map(|s| s.as_str()))
        .collect();

    let ac_charwise = {
        let patvals: Vec<(&str, usize)> = all_patterns
            .iter()
            .enumerate()
            .map(|(i, &p)| (p, i))
            .collect();
        match CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(DaacMatchKind::LeftmostLongest)
            .build_with_values(patvals)
        {
            Ok(ac) => Some(ac),
            Err(e) => {
                eprintln!("[zhtw-mcp] charwise AC build failed, using bytewise fallback: {e}");
                None
            }
        }
    };

    let ac_bytewise = if ac_charwise.is_none() {
        match AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&all_patterns)
        {
            Ok(ac) => Some(ac),
            Err(e) => {
                eprintln!("[zhtw-mcp] bytewise spelling AC build failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let rule_filter_flags: Vec<u8> = spelling_rules
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            let mut f: u8 = 0;
            if rule.to.iter().any(|t| t.contains(&rule.from)) {
                f |= spelling::FILTER_HAS_SUPERSTRING;
            }
            if rule.exceptions.as_ref().is_some_and(|v| !v.is_empty()) {
                f |= spelling::FILTER_HAS_EXCEPTIONS;
            }
            if rule_pos_clue_ids[i].is_some() {
                f |= spelling::FILTER_HAS_POS_CLUES;
            }
            if rule_neg_clue_ids[i].is_some() {
                f |= spelling::FILTER_HAS_NEG_CLUES;
            }
            if rule_positional_clues[i].is_some() {
                f |= spelling::FILTER_HAS_POSITIONAL;
            }
            if rule.is_deletion_rule() {
                f |= spelling::FILTER_IS_DELETION;
            }
            f
        })
        .collect();

    // Dispatch class per rule: determines which monomorphic fast path
    // handles each AC hit.  Positional implies FULL; context clues
    // without positional implies CLUED; everything else is SIMPLE.
    let rule_classes: Vec<u8> = rule_filter_flags
        .iter()
        .map(|&f| {
            if f & spelling::FILTER_HAS_POSITIONAL != 0 {
                spelling::CLASS_FULL
            } else if f & (spelling::FILTER_HAS_POS_CLUES | spelling::FILTER_HAS_NEG_CLUES) != 0 {
                spelling::CLASS_CLUED
            } else if f == 0 {
                spelling::CLASS_TRULY_SIMPLE
            } else {
                spelling::CLASS_SIMPLE
            }
        })
        .collect();

    // Build CompiledRule for each rule (IR predicate chains).
    let compiled_rules: Vec<CompiledRule> = (0..spelling_rules.len())
        .map(|i| {
            let rule = &spelling_rules[i];
            let predicates = compile_rule_predicates(
                rule,
                rule_filter_flags[i],
                rule_pos_clue_ids[i].as_deref(),
                rule_neg_clue_ids[i].as_deref(),
                rule_positional_clues[i].as_deref(),
            );
            CompiledRule {
                rule_idx: i,
                predicates,
                rule_type: rule.rule_type,
                filter_flags: rule_filter_flags[i],
            }
        })
        .collect();

    Ok(CompiledSpellingDb {
        ac_charwise,
        ac_bytewise,
        rules: compiled_rules,
        clue_ac,
        absorber_strings,
        spelling_rules,
        spelling_suggestions,
        rule_pos_clue_ids,
        rule_neg_clue_ids,
        rule_positional_clues,
        rule_filter_flags,
        rule_classes,
    })
}
