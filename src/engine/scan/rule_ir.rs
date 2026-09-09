// Intermediate representation for compiled spelling rule predicates.
//
// Each SpellingRule compiles into a CompiledRule: an ordered chain of
// MatchPredicate values that the evaluator walks sequentially, short-
// circuiting on the first rejection. This IR sits between the raw SpellingRule
// struct (declarative) and the evaluation loop (imperative), enabling future
// optimizations (predicate reordering, dead-predicate elimination) without
// touching the evaluator.
//
// Scope: per-match predicates only. Overlap resolution, anchor confirmation,
// and TM suppression operate at different granularity and are explicitly
// excluded.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use daachorse::{CharwiseDoubleArrayAhoCorasickBuilder, MatchKind as DaacMatchKind};

use crate::engine::excluded::ByteRange;
use crate::engine::segment::BoundaryBitmap;
use crate::engine::zhtype::ChineseType;
use crate::rules::ruleset::{Issue, IssueType, ProfileConfig, RuleType, Severity, SpellingRule};

use super::spelling;
use super::PositionalClue;

// Predicates

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

    /// Rule fires only when translationese detection is enabled.
    /// Maps to RuleType::Translationese gate.
    RequireTranslationeseDetection,

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

    // -- Context-clue gate (fused positive + negative, one window + one AC
    // pass) --
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

// Compiled rule

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
    /// Severity after applying the rule-level override, if any.
    pub severity: Severity,
}

/// A rule's context-selected suggestion groups, compiled for one pass.
///
/// The clues of every group go into a single Aho-Corasick, with `group_of`
/// mapping pattern id back to the group that owns it.  Selection then scans
/// the window once and takes the lowest matching group index, instead of
/// running `str::contains` over the whole window once per clue: on the shipped
/// seven-clue rule that was seven full-window passes per issue.
///
/// Lowest index, not earliest match, because ruleset order is the documented
/// precedence order and has nothing to do with where in the window a clue
/// happens to sit.
pub struct CompiledContextSelector {
    /// All clues of all groups, in group order.
    clue_ac: AhoCorasick,
    /// `group_of[pattern_id]` is the group that clue belongs to.
    group_of: Vec<usize>,
    /// Replacements per group, pre-interned so selection costs a refcount bump.
    to: Vec<Arc<[String]>>,
}

impl CompiledContextSelector {
    /// Replacements for the first group with a clue in `window`, if any.
    fn select(&self, window: &str) -> Option<&Arc<[String]>> {
        let mut best: Option<usize> = None;
        for m in self.clue_ac.find_overlapping_iter(window) {
            let g = self.group_of[m.pattern().as_usize()];
            if best.is_none_or(|b| g < b) {
                // Group 0 wins outright; nothing later can beat it.
                if g == 0 {
                    return self.to.first();
                }
                best = Some(g);
            }
        }
        best.map(|g| &self.to[g])
    }
}

// Compiled spelling database

/// The compiled spelling rule database.  Owns the AC automata and
/// all per-rule compiled data. Constructed by
/// `compile_spelling_rules_filtered()`.
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

    // -- Per-rule parallel arrays (indexed by rule position) -- These arrays
    // are populated during compile_spelling_rules_filtered() and used by
    // structural validation tests (filter_flags_match_rule_properties,
    // filter_vecs_aligned, rule_classes_match_filter_flags).
    /// The spelling rules themselves (filtered and deduplicated).
    ///
    /// Retained whole, not narrowed to the fields the scan path reads.  The
    /// Arc'd arrays below do duplicate roughly 100 KB of context, english and
    /// clue strings, but these rules also back `Scanner::spelling_rules()`,
    /// which the MCP ambiguous-terms resource reads, and the structural
    /// validation tests that re-derive the filter flags from raw fields.
    /// Dropping them would mean rebuilding the filtered list on demand for a
    /// cold resource read, which is more moving parts than 100 KB is worth.
    pub spelling_rules: Vec<SpellingRule>,
    /// Precomputed suggestions per rule.  Arc avoids per-issue clone
    /// during inflation: only a reference count bump per survivor.
    pub spelling_suggestions: Vec<Arc<[String]>>,
    /// Pre-interned context strings per rule.  Arc bump during inflation.
    pub spelling_contexts: Vec<Option<Arc<str>>>,
    /// Pre-interned english anchors per rule.  Arc bump during inflation.
    pub spelling_english: Vec<Option<Arc<str>>>,
    /// Pre-interned context clues per rule.  Arc bump during inflation.
    pub spelling_context_clues: Vec<Option<Arc<[String]>>>,
    /// Per-rule context-selected suggestion groups, pre-interned.  None for
    /// the overwhelming majority of rules, which pay only an Option check at
    /// inflation time.  A rule that has groups pays one window computation plus
    /// a single pass of that window through the group automaton, per issue; see
    /// `CompiledContextSelector` for why the clues are indexed rather than
    /// searched one at a time.  One rule in 1882 carries groups, and building
    /// its automaton is a handful of short words, so construction stays far
    /// under the cold-start budget.
    pub spelling_context_suggestions: Vec<Option<CompiledContextSelector>>,
    /// Per-rule editorial confidence.  Plain copy at inflation
    /// time: `EditorialConfidence` is `Copy`, so no Arc needed.
    pub spelling_editorial_confidence: Vec<Option<crate::rules::ruleset::EditorialConfidence>>,
    /// Per-rule positive clue IDs into the clue AC pattern list.
    pub rule_pos_clue_ids: Vec<Option<Vec<u16>>>,
    /// Per-rule negative clue IDs into the clue AC pattern list.
    #[allow(dead_code)] // folded into rule_filter_flags at compile time; read by tests
    pub rule_neg_clue_ids: Vec<Option<Vec<u16>>>,
    /// Per-rule parsed positional clues (checked after context-clue gate).
    #[allow(dead_code)] // folded into rule_filter_flags at compile time; read by tests
    pub rule_positional_clues: Vec<Option<Vec<PositionalClue>>>,
    /// Per-rule bitflags gating optional filter stages (spelling::FILTER_*).
    /// Used at compilation time to derive rule_classes;
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

// Match context (borrowed evaluation state)

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

    // find_overlapping_iter with MatchKind::Standard does not guarantee
    // left-to-right start-offset order for overlapping patterns. Sort to
    // satisfy the binary-search contract in lookup_clues_in_window. Nearly
    // sorted in practice, so the sort is close to O(n).
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
            if let Ok(pos) = pos_ids.binary_search(&clue_id) {
                let bit = 1u32 << pos;
                if pos_seen & bit == 0 {
                    pos_seen |= bit;
                    pos_found += 1;
                }
            }
        }

        if let Some(neg_ids) = neg_ids {
            if neg_ids.binary_search(&clue_id).is_ok() {
                return (pos_found, true);
            }
        }
    }

    (pos_found, false)
}

// compile_rule_predicates()

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
    if rule.rule_type == RuleType::Translationese {
        preds.push(MatchPredicate::RequireTranslationeseDetection);
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
    let pos = pos_clue_ids
        .filter(|ids| !ids.is_empty())
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    let neg = neg_clue_ids
        .filter(|ids| !ids.is_empty())
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    if !pos.is_empty() || !neg.is_empty() {
        let min_pos_matches = if pos.is_empty() {
            0
        } else {
            super::MIN_SCAN_CLUE_MATCHES as u32
        };
        preds.push(MatchPredicate::CheckClues {
            pos_ids: pos,
            neg_ids: neg,
            min_pos_matches,
        });
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

/// True when one of `forms` sits in the text so that it covers the match at
/// `start`, each form being paired with the offsets at which the match may
/// appear inside it.
///
/// Shared by the superstring and exception-phrase predicates, which differ
/// only in what the covering form means: an already-correct longer word in one
/// case, a phrase the rule does not apply to in the other.
fn covered_by_form(text: &str, start: usize, forms: &[(String, Vec<usize>)]) -> bool {
    forms.iter().any(|(form, offsets)| {
        offsets.iter().any(|&offset| {
            let Some(form_start) = start.checked_sub(offset) else {
                return false;
            };
            let form_end = form_start.saturating_add(form.len()).min(text.len());
            text.get(form_start..form_end) == Some(form.as_str())
        })
    })
}

/// Widen a deletion span over a trailing full-width comma or colon, unless
/// that would reach into an excluded range.
fn extended_deletion_span(ctx: &MatchContext<'_>, end: usize) -> usize {
    if end > ctx.text.len() {
        return end;
    }
    let Some(c) = ctx.text.get(end..).and_then(|s| s.chars().next()) else {
        return end;
    };
    if !matches!(c, '\u{FF0C}' | '\u{FF1A}') {
        return end;
    }
    let extended = end.saturating_add(c.len_utf8());
    if extended > ctx.text.len() {
        return end;
    }
    let excluded_overlap = *ctx.excl_cursor < ctx.excluded.len()
        && ctx.excluded[*ctx.excl_cursor].start < extended
        && end < ctx.excluded[*ctx.excl_cursor].end;
    if excluded_overlap {
        end
    } else {
        extended
    }
}

// eval_predicates() -- generic path for CLASS_CLUED and CLASS_FULL

/// Evaluate a compiled rule's predicate chain against a match context.
///
/// Returns Some(Issue) when all predicates pass, None on first rejection.
#[inline]
pub fn eval_predicates(
    db: &CompiledSpellingDb,
    rule: &CompiledRule,
    ctx: &mut MatchContext<'_>,
    segmenter: &super::super::segment::Segmenter,
) -> Option<Issue> {
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
            MatchPredicate::RequireTranslationeseDetection => {
                if !ctx.cfg.translationese_detection {
                    return None;
                }
            }
            MatchPredicate::RequireStanceAllow => {
                // The one predicate that needs the rule itself. Read here
                // rather than up front: this is a random index into ~1958
                // rules, and only political_coloring rules compile it, so the
                // common path never pays for the lookup.
                let sr = &db.spelling_rules[rule.rule_idx];
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
                // The match sits inside an already-correct longer form.
                if covered_by_form(ctx.text, ctx.start, forms) {
                    return None;
                }
            }
            MatchPredicate::RejectIfWordBoundaryStraddles => {
                let straddles = if ctx.boundary_bitmap.is_empty() {
                    segmenter.match_straddles_word_boundary(ctx.text, ctx.start, end)
                } else {
                    ctx.boundary_bitmap.start_straddles(ctx.start)
                        || ctx.boundary_bitmap.end_straddles(end, ctx.start)
                };
                if straddles {
                    return None;
                }
            }
            MatchPredicate::RejectIfInExceptionPhrase { exceptions } => {
                if covered_by_form(ctx.text, ctx.start, exceptions) {
                    return None;
                }
            }
            MatchPredicate::CheckClues {
                pos_ids,
                neg_ids,
                min_pos_matches,
            } => {
                // Fused context-clue gate using pre-computed document-wide
                // index.
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
                end = extended_deletion_span(ctx, end);
            }
        }
    }

    Some(Issue::deferred_spelling(
        ctx.start,
        end - ctx.start,
        IssueType::from(rule.rule_type),
        rule.severity,
        rule.rule_idx,
    ))
}

// inflate_spelling_issues()

/// Inflate deferred spelling issues after overlap resolution.
///
/// Issues with `spelling_rule_idx = Some(idx)` have empty found,
/// suggestions, context, english, and context_clues.  This function
/// fills them in from the compiled DB and the original text.
/// Non-spelling issues are left untouched.
pub fn inflate_spelling_issues(
    db: &CompiledSpellingDb,
    text: &str,
    excluded: &[ByteRange],
    issues: &mut [Issue],
) {
    inflate_spelling_issues_inner(db, text, excluded, issues, false);
}

/// Like `inflate_spelling_issues` but skips context/english/context_clues
/// when `offset_only` is true (MCP compact output path).  Saves ~3 Arc
/// clones per surviving issue.
pub fn inflate_spelling_issues_compact(
    db: &CompiledSpellingDb,
    text: &str,
    excluded: &[ByteRange],
    issues: &mut [Issue],
) {
    inflate_spelling_issues_inner(db, text, excluded, issues, true);
}

#[inline]
fn inflate_spelling_issues_inner(
    db: &CompiledSpellingDb,
    text: &str,
    excluded: &[ByteRange],
    issues: &mut [Issue],
    offset_only: bool,
) {
    for issue in issues.iter_mut() {
        if let Some(idx) = issue.spelling_rule_idx.take() {
            let sr = &db.spelling_rules[idx];

            // For deletion rules, the span may have been extended to absorb
            // trailing punctuation. Use rule.from.len() for found so users see
            // the phrase to delete, not the absorbed punctuation.
            let found_len = if sr.has_deletion_sentinel() {
                sr.from.len()
            } else {
                issue.length
            };
            let end = issue.offset.saturating_add(found_len).min(text.len());
            if let Some(s) = text.get(issue.offset..end) {
                issue.found = s.to_string();
            }

            // Context-selected replacements override the rule default when a
            // group's clues appear nearby. First matching group wins, so
            // ruleset order is the precedence order. The window comes from the
            // same context_byte_window the clue gate uses, so it stops at
            // paragraph breaks and excluded ranges: a clue in the next
            // paragraph or inside a fenced code block must not swap the
            // replacement set, which would also silently disable auto-fix.
            issue.suggestions = db.spelling_context_suggestions[idx]
                .as_ref()
                .and_then(|sel| {
                    let (ws, we) =
                        spelling::context_byte_window(text, issue.offset.min(end), end, excluded);
                    sel.select(&text[ws..we]).cloned()
                })
                .unwrap_or_else(|| db.spelling_suggestions[idx].clone());
            issue.refresh_suggested_rewrite();
            issue.editorial_confidence = db.spelling_editorial_confidence[idx];
            issue.configured_severity = sr.severity;
            if !offset_only {
                issue.context.clone_from(&db.spelling_contexts[idx]);
                issue.english.clone_from(&db.spelling_english[idx]);
                issue
                    .context_clues
                    .clone_from(&db.spelling_context_clues[idx]);
            }
        }
    }
}

// compile_spelling_rules_filtered()

/// Rule types to exclude from the compiled AC automaton.
///
/// When the target profile is known at Scanner construction time, rule types
/// that the profile would always fast-reject can be excluded entirely from
/// the DAAC, shrinking it by ~5% under the default profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfileFilter {
    pub exclude_variant: bool,
    pub exclude_ai_filler: bool,
    pub exclude_translationese: bool,
    pub exclude_spelling: bool,
}

impl ProfileFilter {
    /// No filtering: include all rule types.
    pub fn none() -> Self {
        Self {
            exclude_variant: false,
            exclude_ai_filler: false,
            exclude_translationese: false,
            exclude_spelling: false,
        }
    }

    /// True when at least one rule type is excluded, so the retain pass is
    /// worth running at all.
    pub fn excludes_anything(&self) -> bool {
        self.exclude_variant
            || self.exclude_ai_filler
            || self.exclude_translationese
            || self.exclude_spelling
    }

    /// Build a filter from a ProfileConfig: exclude rule types that the
    /// profile would always fast-reject in the scan loop.
    pub fn from_config(cfg: &crate::rules::ruleset::ProfileConfig) -> Self {
        Self {
            exclude_variant: !cfg.variant_normalization,
            exclude_ai_filler: !cfg.ai_filler_detection,
            exclude_translationese: !cfg.translationese_detection,
            exclude_spelling: !cfg.spelling,
        }
    }
}

/// Phrases a procedural detector owns, with the automaton to find them.
///
/// Built from the ruleset rather than a `const`, which is the point: these
/// phrases now carry `disabled`, user overrides and the provenance gate like
/// every other rule, while the detector keeps the structural guard the schema
/// cannot express.
pub struct StructuralGuard {
    phrases: Vec<GuardPhrase>,
    ac: AhoCorasick,
}

/// A phrase that a structural detector owns, retaining the policy attached to
/// its source rule instead of reconstructing it from the detector type.
///
/// Every guarded rule shipped today is an ai_filler with no override, so the
/// severity here is Info for all of them, which is the constant the detector
/// used to hardcode. It is carried per phrase anyway so that a guarded rule
/// that does set one is honored, rather than being silently ignored on the
/// one path that reconstructs severity from the type.
///
/// The declared severity travels beside the effective one because the two
/// answer different questions. The effective level is what the finding reports
/// at; the declared level is what `Issue::is_pinned_advisory` reads, so a
/// guarded rule that pins Info is exempt from the heading boost exactly like
/// the same pin on a lexical rule. Carrying only the effective level would let
/// a pin of Info be boosted back to Warning inside a heading, which is the
/// silent divergence this type exists to prevent.
pub(crate) struct GuardPhrase {
    pub(crate) text: String,
    pub(crate) severity: Severity,
    pub(crate) configured_severity: Option<Severity>,
}

impl GuardPhrase {
    fn from_rule(rule: &SpellingRule) -> Self {
        Self {
            text: rule.from.clone(),
            severity: rule.effective_severity(),
            configured_severity: rule.severity,
        }
    }
}

impl StructuralGuard {
    /// Build a guard from the rules that name it.
    ///
    /// Returns `None` when the automaton refuses the phrase set, which is the
    /// same outcome `GuardRules::build` gives that guard: no detector rather
    /// than a half-built one.
    fn from_guard_phrases(mut phrases: Vec<GuardPhrase>) -> Option<Self> {
        // Sorted so the automaton, and therefore pattern indices and match
        // order, do not depend on the caller's or a hash map's ordering.
        phrases.sort_unstable_by(|a, b| a.text.cmp(&b.text));
        let ac = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(phrases.iter().map(|phrase| phrase.text.as_str()))
            .ok()?;
        Some(Self { phrases, ac })
    }

    /// Build a guard straight from rules, for tests that exercise the detector
    /// without going through a whole ruleset.
    ///
    /// It takes rules rather than phrases and severities so a fixture cannot
    /// state a policy the rule does not: the mapping from rule to phrase is the
    /// one `GuardRules::build` uses, not a second spelling of it.
    #[cfg(test)]
    pub fn from_rules(rules: &[SpellingRule]) -> Self {
        Self::from_guard_phrases(rules.iter().map(GuardPhrase::from_rule).collect())
            .expect("test guard patterns are valid")
    }

    /// The phrase and policy behind a match, by pattern index.
    pub(crate) fn matched_rule(&self, index: usize) -> &GuardPhrase {
        &self.phrases[index]
    }

    /// Non-overlapping leftmost-longest matches over `text`.
    pub fn find_iter<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = aho_corasick::Match> + 'a {
        self.ac.find_iter(text)
    }

    pub fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }
}

/// Every structural guard that has at least one live phrase.
#[derive(Default)]
pub struct GuardRules {
    guards: FxHashMap<String, StructuralGuard>,
}

impl GuardRules {
    /// Collect guarded phrases from the full rule set.
    ///
    /// Takes the rules before profile filtering, like the segmenter does: a
    /// profile that drops `ai_filler` from the lexical automaton must not also
    /// silently empty a detector that has its own gate.
    pub fn build(rules: &[SpellingRule]) -> Self {
        // Same order as prepare_rules: drop disabled, then last-wins by from.
        // Both sides have to agree on which duplicate survives, or a phrase
        // could be dropped from the lexical automaton by one rule and picked up
        // here by another, or by neither.
        let mut winner: FxHashMap<&str, &SpellingRule> = FxHashMap::default();
        for rule in rules.iter().filter(|r| !r.disabled) {
            winner.insert(rule.from.as_str(), rule);
        }

        let mut by_guard: FxHashMap<String, Vec<GuardPhrase>> = FxHashMap::default();
        for rule in winner.into_values() {
            let Some(guard) = rule.structural_guard.as_deref() else {
                continue;
            };

            // An unknown name means the ruleset asked for a detector this build
            // does not have. Dropping the phrase silently would turn a typo
            // into missing coverage, so leave it out of the lexical pass (that
            // already happened) and out of here, and let the ruleset linter be
            // the thing that reports it.
            if !crate::rules::ruleset::KNOWN_STRUCTURAL_GUARDS.contains(&guard) {
                tracing::warn!(guard, phrase = %rule.from, "unknown structural_guard, rule inert");
                continue;
            }
            by_guard
                .entry(guard.to_string())
                .or_default()
                .push(GuardPhrase::from_rule(rule));
        }
        let guards = by_guard
            .into_iter()
            // The sort that keeps pattern indices independent of hash-map
            // iteration order lives in from_guard_phrases, shared with the test
            // constructor so the two cannot order phrases differently.
            .filter_map(|(name, phrases)| {
                Some((name, StructuralGuard::from_guard_phrases(phrases)?))
            })
            .collect();
        Self { guards }
    }

    /// The guard registered under `name`, if any phrase still carries it.
    pub fn get(&self, name: &str) -> Option<&StructuralGuard> {
        self.guards.get(name)
    }
}

/// Reduce the incoming rules to the set that will actually be compiled.
///
/// Order matters and is load-bearing: disabled rules go first, then dedup by
/// `from` (last wins, so overrides beat the embedded ruleset), and only then
/// the profile filter.  Filtering before dedup could change which duplicate
/// survives.
fn prepare_rules(spelling_rules: Vec<SpellingRule>, filter: &ProfileFilter) -> Vec<SpellingRule> {
    // Filter disabled first, then deduplicate (last-wins), THEN apply profile
    // filter. Profile filtering must run after dedup so it cannot change which
    // duplicate survives for the same from key.
    let mut spelling_rules: Vec<SpellingRule> =
        spelling_rules.into_iter().filter(|r| !r.disabled).collect();

    // Deduplicate by from key (last wins; overrides come after embedded).
    // Borrow the keys instead of cloning ~1.8k Strings, and mark-then-retain
    // instead of repeated O(n) Vec::remove.
    {
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        let mut keep = vec![true; spelling_rules.len()];
        for i in (0..spelling_rules.len()).rev() {
            if !seen.insert(spelling_rules[i].from.as_str()) {
                keep[i] = false;
            }
        }
        let mut i = 0;
        spelling_rules.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }

    // A guarded rule is owned by a procedural detector, so drop it from the
    // lexical automaton: left in, the phrase would be reported with none of the
    // structural checks the guard exists to run.
    //
    // After dedup, for the same reason profile filtering is. An override that
    // redefines a phrase without a guard, or adds one to a phrase that had
    // none, only settles once last-wins has picked the surviving rule.
    // Filtering first would decide the question against a rule the ruleset had
    // already replaced.
    spelling_rules.retain(|r| r.structural_guard.is_none());

    // Profile-aware filtering: exclude rule types that the target profile would
    // always fast-reject. Runs after dedup to preserve last-wins semantics.
    if filter.excludes_anything() {
        spelling_rules.retain(|r| {
            if filter.exclude_variant && r.rule_type == RuleType::Variant {
                return false;
            }
            if filter.exclude_ai_filler && r.rule_type == RuleType::AiFiller {
                return false;
            }
            if filter.exclude_translationese && r.rule_type == RuleType::Translationese {
                return false;
            }
            if filter.exclude_spelling && r.rule_type.in_spelling_family() {
                return false;
            }
            true
        });
    }

    // Deduplicate context clues within each rule.
    for rule in &mut spelling_rules {
        if let Some(ref mut clues) = rule.context_clues {
            let mut seen = FxHashSet::default();
            clues.retain(|c| seen.insert(c.clone()));
        }
        if let Some(ref mut clues) = rule.negative_context_clues {
            let mut seen = FxHashSet::default();
            clues.retain(|c| seen.insert(c.clone()));
        }
    }

    spelling_rules
}

/// Intern every clue string once, map each rule's clue lists to ids, and build
/// the bytewise automaton the windowed lookups use.
///
/// Returns `(clue_ac, pos_ids, neg_ids)` with both id vectors truncated to the
/// 32-entry bitset capacity and sorted, which is what
/// `lookup_clues_in_window` binary-searches.
type ClueIndex = (
    Option<AhoCorasick>,
    Vec<Option<Vec<u16>>>,
    Vec<Option<Vec<u16>>>,
);

fn build_clue_index(spelling_rules: &[SpellingRule]) -> ClueIndex {
    // Build clue AC: intern all unique clue strings, map per-rule clue lists to
    // indices, build a bytewise AC for windowed lookups.
    let (clue_ac, mut rule_pos_clue_ids, mut rule_neg_clue_ids) = {
        let mut clue_map: FxHashMap<String, u16> = FxHashMap::default();
        let mut clue_vec: Vec<String> = Vec::new();

        let mut intern_clue = |s: &String| -> Option<u16> {
            if let Some(&idx) = clue_map.get(s) {
                Some(idx)
            } else {
                let idx = match u16::try_from(clue_vec.len()) {
                    Ok(i) => i,
                    Err(_) => {
                        tracing::warn!(
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

        for rule in spelling_rules {
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
                    tracing::warn!("clue AC build failed: {e}");
                    None
                }
            }
        };

        (ac, pos_ids, neg_ids)
    };

    // Validate clue-ID counts fit the fixed bitset (capacity 32). Truncate
    // rather than panic on malformed rulesets.
    let truncate_clue_ids = |ids_vec: &mut Vec<Option<Vec<u16>>>, label: &str| {
        for (i, slot) in ids_vec.iter_mut().enumerate() {
            if let Some(ids) = slot {
                if ids.len() > 32 {
                    tracing::warn!(
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

    // Sort clue IDs for binary-search membership in lookup_clues_in_window.
    for ids in rule_pos_clue_ids.iter_mut().flatten() {
        ids.sort_unstable();
    }
    for ids in rule_neg_clue_ids.iter_mut().flatten() {
        ids.sort_unstable();
    }

    (clue_ac, rule_pos_clue_ids, rule_neg_clue_ids)
}

/// Build the absorber strings and the pattern automata over them.
///
/// Absorbers are exception phrases and superstring `to` forms injected into the
/// automaton so LeftmostLongest suppresses the shorter `from` match they
/// contain.  Pattern indices at or beyond `spelling_rules.len()` are those
/// sentinels rather than real rules.
///
/// Charwise (daachorse) is the primary automaton; the bytewise one is built
/// only as a fallback when the charwise build fails, never both.
type Automata = (
    Vec<String>,
    Option<daachorse::CharwiseDoubleArrayAhoCorasick<usize>>,
    Option<AhoCorasick>,
);

fn build_automata(spelling_rules: &[SpellingRule]) -> Automata {
    let spelling_patterns: Vec<&str> = spelling_rules.iter().map(|r| r.from.as_str()).collect();

    // Absorption patterns: exception phrases and superstring to forms injected
    // into the AC so LeftmostLongest suppresses shorter from matches. Indices
    // >= spelling_rules.len() act as sentinels.
    let absorber_strings: Vec<String> = {
        let from_set: FxHashSet<&str> = spelling_patterns.iter().copied().collect();
        let mut candidates: Vec<(String, &str)> = Vec::new();
        let mut dedup = FxHashSet::default();
        for rule in spelling_rules {
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

        // Index froms by first char. Both shadow conditions below --
        // containment (absorber.contains(f)) and right-boundary overlap (a
        // suffix of the absorber is a prefix of f) -- require f's first char to
        // occur in the absorber. So only froms bucketed under a char the
        // absorber actually contains can shadow it; the rest are skipped. Exact
        // same result as scanning all patterns, ~50x fewer comparisons on the
        // ~1.8k-rule ruleset.
        let mut from_by_first: FxHashMap<char, Vec<&str>> = FxHashMap::default();
        let mut has_empty_from = false;
        for &f in &spelling_patterns {
            match f.chars().next() {
                Some(c) => from_by_first.entry(c).or_default().push(f),
                None => has_empty_from = true,
            }
        }

        // Reject absorbers that would shadow a different rule's from.
        candidates
            .into_iter()
            .filter(|(absorber, orig_from)| {
                // An empty from (deduped, so at most one) has no first char and
                // is thus unbucketed, but it is a substring of every absorber
                // -- so unless it *is* orig_from, it shadows unconditionally,
                // exactly as the old full scan did.
                if has_empty_from && !orig_from.is_empty() {
                    return false;
                }
                let mut chars_seen = FxHashSet::default();
                !absorber.chars().any(|c| {
                    if !chars_seen.insert(c) {
                        return false; // bucket already tested for this absorber
                    }
                    from_by_first.get(&c).into_iter().flatten().any(|&f| {
                        if f == *orig_from {
                            return false;
                        }
                        if absorber.contains(f) {
                            return true;
                        }

                        // Right-boundary overlap: proper suffix of absorber is
                        // a prefix of f.
                        let mut ci = absorber.char_indices();
                        ci.next(); // skip position 0 (full string)
                        for (byte_idx, _) in ci {
                            let suffix = &absorber[byte_idx..];
                            if f.starts_with(suffix) {
                                return true;
                            }
                        }
                        false
                    })
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
                tracing::warn!("charwise AC build failed, using bytewise fallback: {e}");
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
                tracing::warn!("bytewise spelling AC build failed: {e}");
                None
            }
        }
    } else {
        None
    };

    (absorber_strings, ac_charwise, ac_bytewise)
}

/// A group is kept only when it can do what the field promises. Malformed
/// shapes are dropped whole, never repaired, because a repaired group
/// changes auto-fix eligibility without saying so: filtering the empty entry
/// out of "["改善", ""]" would leave one candidate, and one candidate is
/// writable.
///
/// "usable" covers both lists: empty can never select or would erase the
/// rule default, and an empty string is worse than useless in either
/// position. As a clue it always matches, since "window.contains(\"\")" is
/// true for every window, so one stray entry makes the group the answer for
/// every match of the rule. As a replacement it is the deletion sentinel,
/// which would turn a substitution into a deletion.
///
/// Groups are refused outright on a rule whose own "to" is that sentinel:
/// inflation derives the reported span from the rule default, so a group
/// offering a real replacement would report a span shorter than the one it
/// rewrites.
///
/// A rule left with no usable group compiles to None so inflation skips the
/// window scan.
fn compile_context_selector(r: &SpellingRule) -> Option<CompiledContextSelector> {
    fn usable(v: &[String]) -> bool {
        !v.is_empty() && v.iter().all(|s| !s.is_empty())
    }

    if r.has_deletion_sentinel() {
        return None;
    }
    let groups: Vec<&crate::rules::ruleset::ContextSuggestion> = r
        .context_suggestions
        .iter()
        .flatten()
        .filter(|g| usable(&g.clues) && usable(&g.to))
        .collect();
    if groups.is_empty() {
        return None;
    }
    let mut patterns: Vec<&str> = Vec::new();
    let mut group_of: Vec<usize> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        for clue in g.clues.iter() {
            patterns.push(clue.as_str());
            group_of.push(gi);
        }
    }

    // Standard (not leftmost) match kind: selection needs every clue that is
    // present, not the one the automaton would prefer.
    //
    // Noncontiguous NFA because build cost is what matters here. This runs once
    // per scanner construction, inside a 50 ms cold-start budget, over a
    // handful of short words; the faster automata earn their construction back
    // on document-length haystacks, not on a 240-byte window.
    let clue_ac = AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .kind(Some(aho_corasick::AhoCorasickKind::NoncontiguousNFA))
        .build(&patterns)
        .ok()?;
    Some(CompiledContextSelector {
        clue_ac,
        group_of,
        to: groups.iter().map(|g| Arc::from(g.to.as_slice())).collect(),
    })
}

/// Compile a set of spelling rules into a `CompiledSpellingDb`.
///
/// Filters disabled rules, deduplicates by `from` key (last wins),
/// builds AC automata (charwise primary, bytewise fallback), interns
/// context clues, and computes per-rule filter flags and dispatch classes.
///
/// `filter` optionally excludes rule types that the target profile would always
/// reject, shrinking the DAAC by ~5% under the default profile; pass
/// `ProfileFilter::none()` to keep every rule.
pub fn compile_spelling_rules_filtered(
    spelling_rules: Vec<SpellingRule>,
    filter: &ProfileFilter,
) -> CompiledSpellingDb {
    let spelling_rules = prepare_rules(spelling_rules, filter);

    let spelling_suggestions: Vec<Arc<[String]>> = spelling_rules
        .iter()
        .map(super::effective_suggestions)
        .map(Arc::from)
        .collect();

    let spelling_contexts: Vec<Option<Arc<str>>> = spelling_rules
        .iter()
        .map(|r| r.context.as_deref().map(Arc::from))
        .collect();

    let spelling_english: Vec<Option<Arc<str>>> = spelling_rules
        .iter()
        .map(|r| r.english.as_deref().map(Arc::from))
        .collect();

    let spelling_context_clues: Vec<Option<Arc<[String]>>> = spelling_rules
        .iter()
        .map(|r| r.context_clues.as_ref().map(|v| Arc::from(v.as_slice())))
        .collect();

    let spelling_context_suggestions: Vec<Option<CompiledContextSelector>> = spelling_rules
        .iter()
        .map(compile_context_selector)
        .collect();

    let spelling_editorial_confidence: Vec<Option<crate::rules::ruleset::EditorialConfidence>> =
        spelling_rules
            .iter()
            .map(|r| r.editorial_confidence)
            .collect();

    let (clue_ac, rule_pos_clue_ids, rule_neg_clue_ids) = build_clue_index(&spelling_rules);

    let rule_positional_clues: Vec<Option<Vec<PositionalClue>>> = spelling_rules
        .iter()
        .map(|rule| {
            rule.positional_clues.as_ref().and_then(|raw| {
                let parsed: Vec<PositionalClue> = raw
                    .iter()
                    .filter_map(|s| {
                        let clue = PositionalClue::parse(s);
                        if clue.is_none() {
                            tracing::warn!(
                                "[zhtw-mcp] rule '{}': unrecognized positional clue '{}'",
                                rule.from,
                                s
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

    let (absorber_strings, ac_charwise, ac_bytewise) = build_automata(&spelling_rules);

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

    // Dispatch class per rule: determines which monomorphic fast path handles
    // each AC hit. Positional implies FULL; context clues without positional
    // implies CLUED; everything else is SIMPLE.
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
                severity: rule.effective_severity(),
            }
        })
        .collect();

    CompiledSpellingDb {
        ac_charwise,
        ac_bytewise,
        rules: compiled_rules,
        clue_ac,
        absorber_strings,
        spelling_rules,
        spelling_suggestions,
        spelling_contexts,
        spelling_english,
        spelling_context_clues,
        spelling_context_suggestions,
        spelling_editorial_confidence,
        rule_pos_clue_ids,
        rule_neg_clue_ids,
        rule_positional_clues,
        rule_filter_flags,
        rule_classes,
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/engine/scan/rule_ir/tests.rs"]
mod tests;
