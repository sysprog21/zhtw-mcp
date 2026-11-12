// Fix application: apply suggested corrections to source text.
//
// Four tiers (strict superset hierarchy):
//   - None: lint only, no fixes applied.
//   - Orthographic: punctuation, spacing, character forms, case, variant,
//     ellipsis, grammar only.  Lexical term substitutions are skipped.
//   - LexicalSafe: orthographic + deterministic term substitutions
//     (exactly one suggestion, no context_clues, not annotated
//     editorial_confidence low).  When --verify calibration has run,
//     issues with anchor_match == Some(false) are skipped;
//     anchor_match == None applies unconditionally.
//   - LexicalContextual: all above + context-clue-gated terms and terms
//     annotated editorial_confidence low (both are judgment calls this
//     tier opts into).  For rules with context_clues, apply only when a
//     segmenter confirms enough clue words in surrounding text.  Non-clue
//     lexical issues use the same single-suggestion constraint as LexicalSafe.
//     Anchor rejection (Some(false)) is respected for non-clue issues
//     but overridden for clue-gated issues (segmenter provides
//     independent confirmation).
//
// Fixes are applied in a single forward pass (ascending offset order).

#[cfg(test)]
use std::sync::Arc;

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::segment::Segmenter;
use crate::rules::ruleset::{EditorialConfidence, Issue, IssueType, Tier2Outcome};

/// Fix mode controlling which issue types are eligible for automatic
/// correction.
///
/// Each tier is a strict superset: None < Orthographic < LexicalSafe <
/// LexicalContextual.
/// The variants are declared in that order and derive Ord, so tier tests read
/// as
/// comparisons ("mode < LexicalContextual") instead of negated variant
/// equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FixMode {
    /// Lint only -- no fixes applied.
    None,
    /// Orthographic fixes only: punctuation, spacing, character forms, case,
    /// variant, ellipsis, grammar.  Lexical term substitutions are skipped.
    Orthographic,
    /// Orthographic + deterministic term substitutions (exactly one suggestion,
    /// no context_clues, not annotated editorial_confidence low).  Equivalent
    /// to old 'safe' mode.
    LexicalSafe,
    /// All above + context-clue-gated terms and terms annotated
    /// editorial_confidence low.  For rules with context_clues, apply only when
    /// segmenter confirms enough clue words nearby.
    LexicalContextual,
}

/// Record of a single fix applied to the text.
#[derive(Debug, Clone)]
pub struct AppliedFix {
    /// Byte offset in the original text where the replacement was written.
    pub offset: usize,
    /// Byte length of the original span that was replaced.
    pub old_len: usize,
    /// The replacement string that was written.
    pub replacement: String,
}

/// Result of applying fixes to text.
#[derive(Debug, Clone)]
pub struct FixResult {
    /// The corrected text.
    pub text: String,
    /// Number of fixes applied.
    pub applied: usize,
    /// Number of issues skipped (ineligible for the chosen fix tier, or in
    /// excluded regions).
    pub skipped: usize,
    /// Subset of `skipped` the fixer judged on the issue's own merits: tier-2
    /// suppression, anchor rejection, an unconfirmed clue gate, a
    /// low-confidence annotation, or several candidate replacements.
    ///
    /// Separate from `skipped` because the two answer different questions. A
    /// lexical issue under `--fix=orthographic` was never in scope, and so are
    /// issues dropped for overlapping an earlier fix or landing in an excluded
    /// region; lumping those in makes `--fix=orthographic` on ordinary prose
    /// report every cross-strait term as "declined", which reads as a verdict
    /// the fixer never reached.
    pub declined: usize,
    /// Detailed record of each applied fix, stored in ascending offset
    /// order (forward pass). Used for position-based convergence
    /// suppression and exact offset remapping after re-scan.
    pub applied_fixes: Vec<AppliedFix>,
}

/// Minimum context clue words for aggressive fixer: confusable rules need
/// higher confidence (2 clues) because both forms are valid in different
/// contexts. Cross-strait and other rule types need only 1 clue because
/// the match itself is already a strong signal of incorrect regional usage.
const MIN_CLUE_MATCHES_CONFUSABLE: usize = 2;
const MIN_CLUE_MATCHES_DEFAULT: usize = 1;

/// [apply_fixes_with_context] after dropping the fixes the content type cannot
/// take verbatim (see
/// [ContentType::admits_fix](crate::engine::scan::ContentType::admits_fix)).
/// Every front end goes through this, so none can forget the filter. A dropped
/// issue is filtered before the fixer runs, so no FixResult counter sees it.
pub fn apply_fixes_for_content_type(
    text: &str,
    issues: &[Issue],
    mode: FixMode,
    excluded: &[ByteRange],
    segmenter: Option<&Segmenter>,
    content_type: crate::engine::scan::ContentType,
) -> FixResult {
    if issues.iter().all(|i| content_type.admits_fix(i)) {
        return apply_fixes_with_context(text, issues, mode, excluded, segmenter);
    }
    let kept: Vec<Issue> = issues
        .iter()
        .filter(|i| content_type.admits_fix(i))
        .cloned()
        .collect();
    apply_fixes_with_context(text, &kept, mode, excluded, segmenter)
}

/// Convenience wrapper that calls [apply_fixes_with_context] without a
/// segmenter.  Context-clue-dependent rules are treated as ambiguous.
pub fn apply_fixes(
    text: &str,
    issues: &[Issue],
    mode: FixMode,
    excluded: &[ByteRange],
) -> FixResult {
    apply_fixes_with_context(text, issues, mode, excluded, None)
}

/// What the fixer decided about one issue.
enum Verdict<'a> {
    /// Write this in place of the issue's span.
    Apply(&'a String),
    /// Out of scope at this tier. Nothing about the issue was weighed, so it
    /// is not a decline: the count the CLI prints would otherwise read as
    /// "wrong tier" on ordinary prose.
    Skip,
    /// Weighed and turned down.
    Decline,
}

/// Decide one issue's fate, given the tier and what the ruleset says about it.
///
/// Separate from the write loop because it is pure: the loop owns the cursor
/// and the barrier state, this owns the judgment, and neither can corrupt the
/// other's half.
fn fix_verdict<'a>(
    issue: &'a Issue,
    end: usize,
    text: &str,
    excluded: &[ByteRange],
    mode: FixMode,
    segmenter: Option<&Segmenter>,
) -> Verdict<'a> {
    // Tier-based fix eligibility.
    //
    // Orthographic issue types can be fixed mechanically (no lexical
    // ambiguity). Lexical types (CrossStrait, Typo, PoliticalColoring,
    // Confusable) need progressively higher fix tiers. AiStyle zero-width
    // artifact removal (empty suggestion on invisible chars only) is safe for
    // orthographic tier: it deletes invisible junk. The found-content check
    // prevents future AiStyle rules with empty suggestions from being
    // misclassified as orthographic. Narrower than
    // ai_score::is_suspicious_zero_width_at, which weighs each codepoint
    // against its neighbors: only ZWSP (U+200B) and mid-text BOM (U+FEFF) are
    // pure tokenizer junk safe to strip unconditionally. A ZWJ or ZWNJ that the
    // detector judged stray is still worth a human's attention rather than an
    // automatic deletion, since misreading its context corrupts a glyph or a
    // spelling.
    let deletes_invisible = issue.rule_type == IssueType::AiStyle
        && crate::rules::ruleset::is_delete_suggestion(&issue.suggestions)
        && !issue.found.is_empty()
        && issue
            .found
            .chars()
            .all(crate::engine::ai_score::is_zero_width_candidate);
    let ai_zero_width_removal = deletes_invisible
        && issue.found.chars().all(|ch| {
            ch == '\u{200B}' || (ch == '\u{FEFF}' && issue.offset > 0) // preserve file-start BOM
        });
    let orthographic = issue.rule_type.is_orthographic() || ai_zero_width_removal;

    // The narrow set is the write condition, not only the tier gate. Without
    // this the arity test below applied the deletion at every tier from
    // LexicalSafe up, which is where "--fix" and "convert" run, so the
    // narrowing only ever protected the tier least likely to reach it. A
    // word-final Malayalam chillu (ZWJ before a space), a doubled Persian ZWNJ
    // and an ideographic variation selector all read as stray to a neighbour
    // test, and deleting them corrupts a glyph or a spelling.
    if deletes_invisible && !ai_zero_width_removal {
        return Verdict::Decline;
    }

    // Rhythm is taste, and the fixer is not. The findings carry no suggestion,
    // so the arity test below would skip them anyway; this says it at the top
    // so that adding a suggestion to one later cannot quietly make it writable.
    // Skip rather than Decline: an advisory the fixer was never meant to act on
    // is out of scope, not a judgment call it lost.
    if issue
        .phase_family
        .is_some_and(|(family, _)| family.is_advisory())
    {
        return Verdict::Skip;
    }

    // Orthographic tier: skip all lexical issues.
    if mode == FixMode::Orthographic && !orthographic {
        return Verdict::Skip;
    }

    // Tier 2 can suppress lexical issues as likely false positives. Respect
    // that suppression during auto-fix so we do not rewrite general prose like
    // "學習的進程" into OS terminology.
    if !orthographic && issue.tier2_outcome == Tier2Outcome::Suppressed {
        return Verdict::Decline;
    }

    // Pre-compute context-clue presence for gating decisions below.
    let has_clues = issue.context_clues.as_ref().is_some_and(|c| !c.is_empty());

    // Judgment calls belong to the top tier. A clue-gated term needs the
    // segmenter to confirm its domain, and a rule the ruleset annotates
    // editorial_confidence low stays valid zh-TW in some senses, so every tier
    // below LexicalContextual leaves both alone.
    //
    // Only the explicit annotation counts here. The MCP explain path
    // (heuristic_editorial_confidence in mcp/tools.rs) falls back to a
    // heuristic that calls every Translationese, AiStyle, Grammar,
    // Severity::Info and anchor-rejected issue low. That fallback exists to
    // decide what to tell a human reviewer, not what to write to a file:
    // applying it here would key the write path on a severity field that
    // suppression mutates, and would duplicate the anchor gate below without
    // its clue-gated escape hatch.
    if !orthographic && mode < FixMode::LexicalContextual {
        // A clue-gated term below the top tier is out of scope, not turned
        // down: the segmenter never ran, so nothing about this issue was
        // weighed, and the tier that handles the class exists one step up. 349
        // shipped rules carry context_clues, so calling these declines would
        // make the count the CLI prints mean "wrong tier" again on ordinary
        // technical prose.
        if has_clues {
            return Verdict::Skip;
        }

        // A low-confidence annotation is the opposite: the ruleset already
        // reached a verdict on the term, and this tier is honoring it.
        if issue.editorial_confidence == Some(EditorialConfidence::Low) {
            return Verdict::Decline;
        }
    }

    // Anchor-match gating for lexical issues: when calibration has run
    // (--verify), anchor_match carries the verdict. If calibration explicitly
    // rejected the term (Some(false)), skip the fix: both LexicalSafe and
    // LexicalContextual respect anchor rejection for non-clue issues (no
    // independent disambiguation available). Context-clue-gated issues in
    // LexicalContextual can override rejection because the segmenter provides
    // independent confirmation. When anchor_match is None (no calibration),
    // apply unconditionally.
    if !orthographic && issue.anchor_match == Some(false) && !has_clues {
        return Verdict::Decline;
    }

    // Context-clue gating for lexical issues. Only LexicalContextual reaches
    // here with clues; the merged tier gate above skipped the rest.
    if has_clues && !orthographic {
        // Threshold is type-aware: confusable rules (both forms valid in
        // different contexts) need 2 clues for confidence; cross-strait and
        // other rules need only 1 (the match itself is a strong regional
        // signal, one nearby clue is sufficient to confirm domain).
        let min_clues = if issue.rule_type == IssueType::Confusable {
            MIN_CLUE_MATCHES_CONFUSABLE
        } else {
            MIN_CLUE_MATCHES_DEFAULT
        };
        let confirmed = segmenter.is_some_and(|seg| {
            let window =
                crate::engine::scan::surrounding_window_bounded(text, issue.offset, end, excluded);

            // Iterating the Option rather than unwrapping it: has_clues above
            // already proved it is Some, and an empty list counts zero clues,
            // which declines exactly as a missing one would.
            let clue_strs: Vec<&str> = issue
                .context_clues
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|s| s.as_str())
                .collect();
            seg.count_context_clues(window, &clue_strs) >= min_clues
        });
        if !confirmed {
            return Verdict::Decline;
        }
    }

    // Suggestion selection: exactly one candidate, for every issue type.
    //
    // Orthographic issues used to take the first of however many were offered,
    // on the reasoning that punctuation and case are mechanical. The premise is
    // true of the issues the engine builds (punctuation, grammar and case all
    // construct a single-element vec) but not of the rules a user can load: a
    // variant rule with "to": ["a", "b"] in a pack or an overrides file reached
    // that arm and wrote "a" at --fix=orthographic, the most conservative tier
    // there is.
    //
    // So the arity test is the write condition and the orthographic split
    // governs only tier eligibility, which is what it was ever about. One
    // candidate means the answer is determined; more than one is a judgment
    // call regardless of which pass produced it.
    if issue.suggestions.len() == 1 {
        return Verdict::Apply(&issue.suggestions[0]);
    }

    // Several candidates and no way to choose: a judgment call left to the
    // author, not an out-of-scope issue.
    //
    // An empty suggestion list is the other way to land here, and it is not a
    // judgment call: the rule had nothing to offer, so there was no verdict to
    // reach. Counting it would report a decline for a malformed rule, which
    // only a pack can carry since check-ruleset.py rejects the shape in the
    // shipped ruleset.
    if issue.suggestions.len() > 1 {
        Verdict::Decline
    } else {
        Verdict::Skip
    }
}

/// Apply fixes to text using an optional segmenter for context-clue analysis.
///
/// Issues must be sorted by offset (ascending) and non-overlapping
/// (guaranteed by the scanner's resolve_overlaps pass).  Fixes are
/// applied in a single forward pass (ascending offset order): chunks of
/// unchanged text are copied between replacement spans, yielding O(N).
///
/// Fix tiers control which issues are eligible:
///   - Orthographic: only Punctuation/Case/Variant/Grammar issues.
///   - LexicalSafe: above + lexical issues without context_clues,
///     single suggestion only.  When `--verify` calibration has run,
///     issues with `anchor_match == Some(false)` are skipped (calibration
///     rejected the term).  `anchor_match == None` (no calibration)
///     applies unconditionally.
///   - LexicalContextual: all above + context-clue-gated lexical issues,
///     verified by segmenter when available.  For non-clue issues, respects
///     anchor rejections (no independent disambiguation).  For clue-gated
///     issues, the segmenter overrides anchor rejection.
pub fn apply_fixes_with_context(
    text: &str,
    issues: &[Issue],
    mode: FixMode,
    excluded: &[ByteRange],
    segmenter: Option<&Segmenter>,
) -> FixResult {
    let started = std::time::Instant::now();
    let _span = tracing::info_span!(
        "fix",
        content_length = text.len() as u64,
        issue_count = issues.len() as u64,
        mode = ?mode
    )
    .entered();
    // Lint-only mode: no fixes attempted, nothing to skip.
    if mode == FixMode::None {
        tracing::info!(
            fix_count = 0_u64,
            skipped_count = 0_u64,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "fix completed"
        );
        return FixResult {
            text: text.to_string(),
            applied: 0,
            skipped: 0,
            declined: 0,
            applied_fixes: Vec::new(),
        };
    }

    let mut out = String::with_capacity(text.len());
    let mut applied = 0usize;

    // Only the interesting counter is kept. Every path through the loop ends in
    // exactly one of applied or a skip, so the skip total is arithmetic, and a
    // site that forgets to bump it cannot exist.
    let mut declined = 0usize;

    // "is_excluded" switches to binary search past ten ranges, which assumes
    // the slice is sorted by start and non-overlapping. Every in-tree caller
    // satisfies that, because the builders all end in "merge_ranges_pub", but
    // this is a public entry point on a write path: an unsorted slice would
    // silently let a fix through into bytes the caller marked protected. The
    // check is one linear pass, and the normalization it guards runs only for
    // callers that got it wrong.
    let normalized;
    let excluded = if excluded.windows(2).all(|w| w[0].end <= w[1].start) {
        excluded
    } else {
        normalized = crate::engine::excluded::merge_ranges_pub(excluded.to_vec());
        &normalized[..]
    };

    let mut applied_fixes = Vec::new();
    // Byte position up to which we have already copied into out.
    let mut cursor: usize = 0;

    // Byte position up to which grammar issues are declined because an
    // enclosing grammar span was declined. See the barrier check below.
    let mut skip_until: usize = 0;

    // Issues are already sorted ascending by offset and non-overlapping
    // (scanner's resolve_overlaps guarantees this). Iterate forward, copying
    // unchanged gaps and appending replacements.
    for issue in issues {
        // Reject an unusable span before anything else looks at it. Two reasons
        // it has to be first, not merely early. It is not a judgment, so it
        // must not reach a gate that records a decline. And the clue gate below
        // slices surrounding_window, whose forward walk stops at text.len()
        // without clamping byte_end, so an out-of-range end reaching it panics
        // on a public entry point.
        //
        // In range is not the same as usable. Both edges are sliced further
        // down, and a slice that splits a character panics exactly as an
        // out-of-range one does, so a span landing inside a multi-byte
        // character has to fall out here too. The scanner's own offsets are
        // character aligned, which is what makes this a guard on the entry
        // point rather than a check the scan needs.
        let Some(end) = issue
            .offset
            .checked_add(issue.length)
            .filter(|e| *e <= text.len())
            .filter(|e| text.is_char_boundary(issue.offset) && text.is_char_boundary(*e))
        else {
            tracing::warn!(
                "skipping malformed issue at offset {}: span past end of text \
                 or off a character boundary",
                issue.offset
            );
            continue;
        };

        // Skip overlapping issues: grammar issues are appended after overlap
        // resolution and may overlap each other (e.g. 對X進行Y overlaps the
        // inner 進行Y). The fixer must not apply both.
        //
        // skip_until extends the same barrier to a span that was declined
        // rather than applied. Without it, declining the outer 對X進行Y because
        // it crosses an excluded region still lets the inner 進行Y fire, which
        // strips 進行 and leaves the fronted 對 dangling: prose nobody wrote,
        // from a span the mask said not to touch.
        if issue.offset < cursor
            || (issue.rule_type == IssueType::Grammar && issue.offset < skip_until)
        {
            continue;
        }

        // Skip if the issue writes into any excluded region. For a non-empty
        // span that is the scanner's own overlap test, including its
        // binary-search path once the range list grows past a handful.
        //
        // Zero-length insertions need their own check: a zero-width span
        // overlaps nothing, so the generic test reports it as outside every
        // range. Spacing rules emit exactly that shape, and an insertion
        // strictly inside a range corrupts protected bytes just as a
        // replacement would. The bounds are strict on purpose: inserting at a
        // range edge writes outside it, which is how a missing space before an
        // inline code span gets fixed.
        let writes_into_excluded = if issue.length == 0 {
            excluded
                .iter()
                .any(|r| issue.offset > r.start && issue.offset < r.end)
        } else {
            is_excluded(issue.offset, end, excluded)
        };
        if writes_into_excluded {
            skip_until = skip_until.max(end);
            continue;
        }

        let rep = match fix_verdict(issue, end, text, excluded, mode, segmenter) {
            Verdict::Apply(rep) => rep,
            Verdict::Skip => continue,
            Verdict::Decline => {
                declined += 1;
                continue;
            }
        };

        out.push_str(&text[cursor..issue.offset]);
        out.push_str(rep);
        cursor = end;
        applied_fixes.push(AppliedFix {
            offset: issue.offset,
            old_len: issue.length,
            replacement: rep.clone(),
        });
        applied += 1;
    }

    // Copy the remaining tail after the last fix (or the entire text if no
    // fixes were applied).
    out.push_str(&text[cursor..]);

    // Every issue is either applied or skipped, and the loop has no early exit,
    // so this is the total rather than a running tally nobody can forget to
    // keep.
    let skipped = issues.len() - applied;

    tracing::info!(
        fix_count = applied as u64,
        skipped_count = skipped as u64,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "fix completed"
    );
    FixResult {
        text: out,
        applied,
        skipped,
        declined,
        applied_fixes,
    }
}

/// Map an original-text byte offset to its position in the fixed text.
///
/// Accumulates byte deltas (replacement.len() - old_len) from all applied
/// fixes whose original offset is strictly before orig_offset.  All fix
/// offsets are in original-text coordinates and non-overlapping.
pub fn remap_to_post_fix(orig_offset: usize, applied_fixes: &[AppliedFix]) -> usize {
    let mut delta: isize = 0;
    for fix in applied_fixes {
        if fix.offset < orig_offset {
            delta += fix.replacement.len() as isize - fix.old_len as isize;
        }
    }
    let result = orig_offset as isize + delta;
    debug_assert!(result >= 0, "remap produced negative offset");
    result.max(0) as usize
}

/// Remap exclusion zones from original-text coordinates to post-fix
/// coordinates.
///
/// The fixer never applies fixes inside excluded regions, so exclusion zones
/// remain structurally intact -- only their byte offsets shift due to
/// earlier replacements having different lengths than the originals.
///
/// Uses a merge-style single forward pass over both sorted sequences
/// (applied_fixes and exclusions), accumulating deltas in O(E + F) time.
pub fn remap_exclusions(
    exclusions: &[crate::engine::excluded::ByteRange],
    applied_fixes: &[AppliedFix],
) -> Vec<crate::engine::excluded::ByteRange> {
    use crate::engine::excluded::ByteRange;

    if applied_fixes.is_empty() {
        return exclusions.to_vec();
    }

    let mut delta: isize = 0;
    let mut fix_idx = 0;
    exclusions
        .iter()
        .map(|&ByteRange { start, end }| {
            // Advance past all fixes whose span ends at or before this
            // exclusion zone. The end-of-span check (offset + old_len) is
            // critical for zero-length insertions (e.g. spacing fixes with
            // old_len == 0): an insertion at the exclusion boundary must shift
            // the zone right.
            while fix_idx < applied_fixes.len() {
                let fix = &applied_fixes[fix_idx];
                let fix_end = fix.offset.saturating_add(fix.old_len);
                if fix_end > start {
                    break;
                }
                delta += fix.replacement.len() as isize - fix.old_len as isize;
                fix_idx += 1;
            }
            let new_start = (start as isize + delta).max(0) as usize;
            let new_end = (end as isize + delta).max(0) as usize;
            ByteRange {
                start: new_start,
                end: new_end,
            }
        })
        .collect()
}

/// Remove re-scan issues whose byte range overlaps a region written by the
/// fixer.
///
/// After applying fixes and re-scanning, the fixer may have introduced new
/// text that triggers rules (convergent chain).  These are noise: the fixer
/// already chose the best replacement.  This function suppresses them by
/// checking each re-scan issue against the post-fix byte ranges of applied
/// fixes.
pub fn suppress_convergent_issues(issues: &mut Vec<Issue>, applied_fixes: &[AppliedFix]) {
    if applied_fixes.is_empty() {
        return;
    }

    // Build post-fix ranges in a single forward pass (O(n)) instead of calling
    // remap_to_post_fix per fix (O(n) each, O(n^2) total). Applied fixes are
    // sorted by offset and non-overlapping, so a running delta accumulator
    // gives the correct remapped position for each fix.
    let mut delta: isize = 0;
    let fix_ranges: Vec<(usize, usize)> = applied_fixes
        .iter()
        .map(|fix| {
            let post = (fix.offset as isize + delta).max(0) as usize;
            delta += fix.replacement.len() as isize - fix.old_len as isize;
            (post, post + fix.replacement.len())
        })
        .collect();
    issues.retain(|issue| {
        let issue_end = issue.offset + issue.length;
        !fix_ranges.iter().any(|&(start, end)| {
            if start == end {
                // Zero-length deletion: suppress issues touching this offset.
                issue.offset <= start && issue_end > start
            } else {
                issue.offset < end && issue_end > start
            }
        })
    });
}

#[cfg(test)]
#[path = "../tests/unit/fixer/tests.rs"]
mod tests;
