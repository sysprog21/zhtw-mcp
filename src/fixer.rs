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

/// Apply fixes to text based on the given issues.
///
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
    // so the arity test below would decline them anyway; this says it at the
    // top so that adding a suggestion to one later cannot quietly make it
    // writable. Skip rather than Decline: an advisory the fixer was never meant
    // to act on is out of scope, not a judgment call it lost.
    if issue
        .phase_family
        .is_some_and(|(family, _)| family.is_rhythm())
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
            let clue_strs: Vec<&str> = issue
                .context_clues
                .as_ref()
                .unwrap()
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
mod tests {
    use super::*;
    use crate::engine::scan::surrounding_window;
    use crate::rules::ruleset::{IssueType, PhaseFamily, PhasePass, Severity};

    fn make_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::CrossStrait,
            Severity::Warning,
        )
    }

    fn make_issue_with_clues(
        offset: usize,
        found: &str,
        suggestions: Vec<&str>,
        clues: Vec<&str>,
    ) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::Confusable,
            Severity::Warning,
        )
        .with_english("program")
        .with_context_clues(clues.into_iter().map(String::from).collect())
    }

    fn make_punctuation_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::Punctuation,
            Severity::Warning,
        )
    }

    /// A caller-supplied span that splits a character is skipped, not sliced.
    ///
    /// The scanner never emits one, so this covers the guard rather than the
    /// scan: apply_fixes is public, and both edges of the span reach a slice.
    #[test]
    fn a_span_off_a_character_boundary_is_skipped() {
        let text = "這個軟件很好用";
        for offset in [7, 8] {
            let issues = vec![make_issue(offset, "軟件", vec!["軟體"])];
            let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
            assert_eq!(result.text, text, "text was rewritten at offset {offset}");
            assert_eq!(result.applied, 0);
        }

        // The same span on its real boundary still applies, so the guard is
        // rejecting the misalignment rather than the issue.
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        assert_eq!(
            apply_fixes(text, &issues, FixMode::LexicalSafe, &[]).applied,
            1
        );
    }

    /// An end that splits a character is rejected for the same reason a start
    /// is: the tail copy after the last fix slices there.
    #[test]
    fn a_span_ending_off_a_character_boundary_is_skipped() {
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.length = 5;
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text);
        assert_eq!(result.applied, 0);
    }

    /// A rhythm (氣口) finding as the scanner emits it, identified by its
    /// family rather than by the empty suggestion list that follows from it.
    /// `rhythm_findings_carry_no_suggestion` in the grammar scanner proves the
    /// real detectors produce this shape; here is the fixer's side.
    fn make_rhythm_issue(offset: usize, found: &str, family: PhaseFamily) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            Vec::new(),
            IssueType::Translationese,
            Severity::Info,
        )
        .with_phase_family(family, PhasePass::Indexed)
    }

    #[test]
    fn rhythm_issues_are_never_fixed_at_any_tier() {
        let text = "這份報告詳細說明了整個系統在過去一年之中所有功能的演進過程。";
        for family in [PhaseFamily::RhythmLongSentence, PhaseFamily::RhythmMonotony] {
            // The second issue carries a lone suggestion, which is the write
            // condition for every other issue type. The family has to be what
            // stops it, or a future detector that offers a rewrite hint would
            // start editing prose on taste alone.
            let issues = vec![
                make_rhythm_issue(0, "這份報告", family),
                Issue::new(
                    text.find("演進過程").unwrap(),
                    "演進過程".len(),
                    "演進過程",
                    vec!["演進".to_string()],
                    IssueType::Translationese,
                    Severity::Info,
                )
                .with_phase_family(family, PhasePass::Indexed),
            ];
            for mode in [
                FixMode::None,
                FixMode::Orthographic,
                FixMode::LexicalSafe,
                FixMode::LexicalContextual,
            ] {
                let result = apply_fixes(text, &issues, mode, &[]);
                assert_eq!(result.text, text, "rhythm was rewritten at {mode:?}");
                assert_eq!(result.applied, 0, "rhythm was applied at {mode:?}");
                assert_eq!(
                    result.declined, 0,
                    "an advisory the fixer never acts on is out of scope, not a judgment call, at {mode:?}"
                );
            }
        }
    }

    #[test]
    fn lexical_safe_single_suggestion() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn lexical_safe_multiple_suggestions_skipped() {
        let text = "這個視頻很好看";
        let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged
        assert_eq!(result.applied, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_skips_multi_suggestion_non_clue() {
        // Multi-suggestion lexical issue without context_clues: both
        // LexicalSafe and LexicalContextual skip it (no disambiguation).
        let text = "這個視頻很好看";
        let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- ambiguous, no clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn multiple_fixes() {
        let text = "這個軟件的內存";
        let issues = vec![
            make_issue(6, "軟件", vec!["軟體"]),
            make_issue(15, "內存", vec!["記憶體"]),
        ];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體的記憶體");
        assert_eq!(result.applied, 2);
    }

    #[test]
    fn excluded_offset_skipped() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(
            text,
            &issues,
            FixMode::LexicalSafe,
            &[ByteRange { start: 0, end: 21 }],
        );
        assert_eq!(result.text, text);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn unsorted_excluded_ranges_still_protect() {
        // Past ten ranges is_excluded binary-searches, which needs sorted,
        // non-overlapping input. A caller that hands over ranges in any other
        // order must still get its protected bytes back untouched rather than a
        // silent rewrite.
        let text = "這個軟件很好用";
        let offset = text.find("軟件").unwrap();

        // Twelve ranges, descending, with the one covering the issue last so a
        // binary search over the unsorted slice cannot find it.
        let mut excluded: Vec<ByteRange> = (0..11)
            .map(|i| ByteRange {
                start: 100 + i * 10,
                end: 100 + i * 10 + 5,
            })
            .rev()
            .collect();
        excluded.push(ByteRange {
            start: offset,
            end: offset + "軟件".len(),
        });

        let issues = vec![make_issue(offset, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &excluded);
        assert_eq!(result.text, text, "protected span was rewritten");
        assert_eq!(result.skipped, 1);

        // Sorted but overlapping is the other way the binary search breaks: it
        // assumes only the immediately preceding range can overlap, so a wide
        // early range that covers the issue is missed once narrower ranges sort
        // between them.
        let mut nested = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        nested.extend((0..11).map(|i| ByteRange {
            start: 100 + i * 10,
            end: 100 + i * 10 + 5,
        }));
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &nested);
        assert_eq!(
            result.text, text,
            "range covering the whole text was missed"
        );
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn zero_length_insertion_inside_excluded_is_skipped() {
        // Spacing rules emit zero-length insertions (missing_space_issue in
        // src/engine/scan/spacing.rs). A zero-width span overlaps nothing, so
        // the generic overlap test reports it as outside every range. The mask
        // still has to stop it: writing a space into the middle of a code span
        // corrupts the code exactly as a replacement would.
        let text = "這是 `中文abc` 的說明";
        let code_start = text.find('`').unwrap();
        let code_end = text[code_start + 1..].find('`').unwrap() + code_start + 2;
        let boundary = text.find("abc").unwrap();
        assert!(boundary > code_start && boundary < code_end);

        let insertion = Issue::new(
            boundary,
            0,
            "",
            vec![" ".into()],
            IssueType::Punctuation,
            Severity::Info,
        );
        let result = apply_fixes(
            text,
            &[insertion],
            FixMode::Orthographic,
            &[ByteRange {
                start: code_start,
                end: code_end,
            }],
        );
        assert_eq!(result.text, text, "must not write inside the code span");
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn declined_excluded_grammar_span_does_not_block_lexical_fix() {
        let text = "我們對`x`進行軟件處理。";
        let outer = text.find('對').unwrap();
        let inner = text.find("進行").unwrap();
        let lexical = text.find("軟件").unwrap();
        let code = text.find('`').unwrap();
        let code_end = text[code + 1..].find('`').unwrap() + code + 2;
        let issues = vec![
            Issue::new(
                outer,
                "對`x`進行軟件處理".len(),
                "對`x`進行軟件處理",
                vec!["處理`x`".into()],
                IssueType::Grammar,
                Severity::Info,
            ),
            Issue::new(
                inner,
                "進行軟件處理".len(),
                "進行軟件處理",
                vec!["軟件處理".into()],
                IssueType::Grammar,
                Severity::Info,
            ),
            make_issue(lexical, "軟件", vec!["軟體"]),
        ];

        let result = apply_fixes(
            text,
            &issues,
            FixMode::LexicalContextual,
            &[ByteRange {
                start: code,
                end: code_end,
            }],
        );

        assert_eq!(result.text, "我們對`x`進行軟體處理。");
        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 2);
    }

    #[test]
    fn empty_issues() {
        let text = "hello";
        let result = apply_fixes(text, &[], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "hello");
        assert_eq!(result.applied, 0);
    }

    // -- Orthographic tier tests --

    #[test]
    fn orthographic_fixes_punctuation() {
        let text = "你好,世界";
        let issues = vec![make_punctuation_issue(6, ",", vec!["，"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, "你好，世界");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn orthographic_skips_lexical_issues() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, text); // unchanged -- orthographic skips CrossStrait
        assert_eq!(result.skipped, 1);
    }

    // -- Anchor-match gating tests --

    #[test]
    fn lexical_safe_skips_anchor_rejected() {
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(false); // calibration rejected
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- anchor rejected
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_safe_applies_anchor_confirmed() {
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(true); // calibration confirmed
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_safe_applies_anchor_none() {
        let text = "這個軟件很好用";
        let issue = make_issue(6, "軟件", vec!["軟體"]);
        // anchor_match == None (no calibration) -- should apply unconditionally
        assert!(issue.anchor_match.is_none());
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn fix_modes_are_ordered_by_tier() {
        // The tier gate compares with "<", so its meaning rides on variant
        // declaration order. Reordering the enum compiles clean and passes
        // clippy while silently inverting the gate; this pins it.
        assert!(FixMode::None < FixMode::Orthographic);
        assert!(FixMode::Orthographic < FixMode::LexicalSafe);
        assert!(FixMode::LexicalSafe < FixMode::LexicalContextual);
    }

    #[test]
    fn lexical_safe_skips_low_editorial_confidence() {
        let text = "需要優化性能";
        let mut issue = make_issue(6, "優化", vec!["最佳化"]);
        issue.editorial_confidence = Some(EditorialConfidence::Low);

        let safe = apply_fixes(text, &[issue.clone()], FixMode::LexicalSafe, &[]);
        assert_eq!(safe.text, text);
        assert_eq!(safe.skipped, 1);
        assert_eq!(safe.declined, 1, "the annotation is a judgment call");

        let contextual = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
        assert_eq!(contextual.text, "需要最佳化性能");
        assert_eq!(contextual.applied, 1);
    }

    #[test]
    fn out_of_tier_issues_are_skipped_but_not_declined() {
        // "declined" is what the CLI prints, so it has to mean the fixer
        // weighed the issue and said no. A lexical issue under
        // --fix=orthographic was never in scope; counting it would make
        // orthographic runs on ordinary prose report every cross-strait term as
        // a verdict the fixer never reached.
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];

        let ortho = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(ortho.text, text);
        assert_eq!(ortho.skipped, 1);
        assert_eq!(ortho.declined, 0);

        // Same issue, same decision to leave it alone, but now on its merits.
        let ambiguous = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
        let safe = apply_fixes("這個視頻很好看", &ambiguous, FixMode::LexicalSafe, &[]);
        assert_eq!(safe.skipped, 1);
        assert_eq!(safe.declined, 1);
    }

    #[test]
    fn orthographic_ignores_low_editorial_confidence() {
        // The gate is guarded by !orthographic: editorial confidence is a
        // lexical-judgment signal, so an orthographic issue carrying the
        // annotation is still fixed at every tier.
        let text = "他說,好";
        let mut issue = make_punctuation_issue(6, ",", vec!["，"]);
        issue.editorial_confidence = Some(EditorialConfidence::Low);

        let fixed = apply_fixes(text, &[issue], FixMode::Orthographic, &[]);
        assert_eq!(fixed.text, "他說，好");
        assert_eq!(fixed.applied, 1);
        assert_eq!(fixed.skipped, 0);
    }

    #[test]
    fn lexical_contextual_respects_anchor_rejection_for_non_clue() {
        // Non-clue lexical issue with anchor rejection: LexicalContextual
        // respects it because there is no independent disambiguation signal.
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(false);
        let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- anchor rejected, no clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_skips_tier2_suppressed_issue() {
        let text = "學習的進程需要耐心和毅力";
        let offset = text.find("進程").unwrap();
        let mut issue = make_issue(offset, "進程", vec!["行程"]);
        issue.tier2_outcome = Tier2Outcome::Suppressed;
        issue.severity = Severity::Info;
        let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text);
        assert_eq!(result.skipped, 1);
    }

    // -- Combined anchor_match + context_clues tests --

    #[test]
    fn lexical_safe_skips_clue_rule_even_with_anchor_confirmed() {
        // anchor_match == Some(true) but has context_clues → LexicalSafe still
        // refuses because context-clue rules need LexicalContextual.
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let mut issue = make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        );
        issue.anchor_match = Some(true);
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- context_clues gate takes precedence
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_applies_clue_rule_despite_anchor_rejection() {
        // anchor_match == Some(false) + context_clues present.
        // LexicalContextual overrides anchor rejection and applies if segmenter
        // confirms clues.
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let mut issue = make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        );
        issue.anchor_match = Some(false);
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &[issue], FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, "我需要編寫一個程式來執行");
        assert_eq!(result.applied, 1);
    }

    // -- Context clue tests --

    #[test]
    fn lexical_safe_skips_issues_with_context_clues() {
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- lexical_safe refuses context-clue rules
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_with_segmenter_applies_when_clues_match() {
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, "我需要編寫一個程式來執行");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_contextual_with_segmenter_skips_when_clues_insufficient() {
        let text = "這個程序很重要";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, text); // unchanged -- insufficient clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_without_segmenter_skips_clue_rules() {
        let text = "這個程序很重要";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- no segmenter, cannot verify clues
        assert_eq!(result.skipped, 1);
    }

    // -- AiStyle tier exclusion tests --

    fn make_ai_style_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::AiStyle,
            Severity::Info,
        )
    }

    #[test]
    fn orthographic_skips_ai_style_issues() {
        let text = "這個系統作為核心元件";
        let offset = text.find("作為").unwrap();
        let issues = vec![make_ai_style_issue(offset, "作為", vec!["是"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, text); // unchanged: AiStyle not orthographic
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_safe_applies_single_suggestion_ai_style() {
        // Semantic safety words (意味著→表示) have a single suggestion and are
        // eligible for lexical_safe auto-fix.
        let text = "這個定義意味著所有值";
        let offset = text.find("意味著").unwrap();
        let issues = vec![make_ai_style_issue(offset, "意味著", vec!["表示"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個定義表示所有值");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_safe_skips_ai_style_no_suggestions() {
        let text = "這意味著很多事情";
        let offset = text.find("意味著").unwrap();
        let issues = vec![make_ai_style_issue(offset, "意味著", vec![])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged: no suggestion
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn surrounding_window_basic() {
        let text = "AABBCCDDEE";
        let window = surrounding_window(text, 4, 6);
        // Window should include chars around the CC range
        assert!(window.contains('A'));
        assert!(window.contains('E'));
    }

    #[test]
    fn surrounding_window_cjk() {
        let text = "我需要編寫一個程序來執行這個任務";
        let offset = text.find("程序").unwrap();
        let end = offset + "程序".len();
        let window = surrounding_window(text, offset, end);
        assert!(window.contains("編寫"));
        assert!(window.contains("執行"));
    }

    #[test]
    fn surrounding_window_empty_text() {
        let window = surrounding_window("", 0, 0);
        assert_eq!(window, "");
    }

    #[test]
    fn surrounding_window_at_boundaries() {
        // Match spans entire text -- window should return the whole string.
        let text = "程序";
        let window = surrounding_window(text, 0, text.len());
        assert_eq!(window, "程序");
    }

    // -- suppress_convergent_issues O(n) equivalence tests --

    #[test]
    fn suppress_convergent_o_n_matches_o_n2() {
        // Verify the O(n) forward-pass remap produces identical fix_ranges to
        // the old per-fix remap_to_post_fix approach.
        let cases: Vec<Vec<AppliedFix>> = vec![
            // Empty
            vec![],
            // Single fix, same length (no shift)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "軟體".into(),
            }],
            // Single fix, expansion (6 bytes -> 9 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "記憶體".into(),
            }],
            // Single fix, contraction (9 bytes -> 6 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 9,
                replacement: "軟體".into(),
            }],
            // Single fix, deletion (6 bytes -> 0 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: String::new(),
            }],
            // Two fixes, both same length
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: "軟體".into(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "記憶".into(),
                },
            ],
            // Two fixes, first expands
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: "記憶體".into(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "軟體".into(),
                },
            ],
            // Two fixes, first contracts
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 9,
                    replacement: "AB".into(),
                },
                AppliedFix {
                    offset: 20,
                    old_len: 6,
                    replacement: "CD".into(),
                },
            ],
            // Two fixes, first is deletion
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: String::new(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "XY".into(),
                },
            ],
            // Three fixes with mixed shifts
            vec![
                AppliedFix {
                    offset: 0,
                    old_len: 3,
                    replacement: "ABCDE".into(),
                },
                AppliedFix {
                    offset: 10,
                    old_len: 6,
                    replacement: "X".into(),
                },
                AppliedFix {
                    offset: 20,
                    old_len: 3,
                    replacement: "YZW".into(),
                },
            ],
        ];

        for (i, fixes) in cases.iter().enumerate() {
            // O(n^2) reference: call remap_to_post_fix per fix
            let expected: Vec<(usize, usize)> = fixes
                .iter()
                .map(|fix| {
                    let post = remap_to_post_fix(fix.offset, fixes);
                    (post, post + fix.replacement.len())
                })
                .collect();

            // O(n) forward pass
            let mut delta: isize = 0;
            let actual: Vec<(usize, usize)> = fixes
                .iter()
                .map(|fix| {
                    let post = (fix.offset as isize + delta).max(0) as usize;
                    delta += fix.replacement.len() as isize - fix.old_len as isize;
                    (post, post + fix.replacement.len())
                })
                .collect();

            assert_eq!(expected, actual, "case {i} mismatch: fixes={fixes:?}");
        }
    }

    #[test]
    fn suppress_convergent_deletion_suppresses_touching_issue() {
        // A deletion (replacement is empty) should suppress issues that touch
        // the deletion point.
        let fixes = vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: String::new(),
        }];
        // Issue at post-fix offset 6 (the deletion point) should be suppressed.
        let mut issues = vec![make_issue(6, "XX", vec!["YY"])];
        suppress_convergent_issues(&mut issues, &fixes);
        assert!(
            issues.is_empty(),
            "issue touching deletion point should be suppressed"
        );
    }

    #[test]
    fn suppress_convergent_preserves_non_overlapping_issue() {
        let fixes = vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: "軟體".into(),
        }];
        // Issue at offset 20, well past the fix range -- should survive.
        let mut issues = vec![make_issue(20, "內存", vec!["記憶體"])];
        suppress_convergent_issues(&mut issues, &fixes);
        assert_eq!(issues.len(), 1, "non-overlapping issue should be preserved");
    }

    #[test]
    fn empty_context_clues_vec_treated_as_no_clues() {
        // Issue with context_clues: Some(vec![]) should NOT be skipped in
        // lexical_safe because the empty vec means no ambiguity.
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.context_clues = Some(Arc::from(Vec::<String>::new()));
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    // remap_exclusions tests

    use crate::engine::excluded::ByteRange;

    fn br(start: usize, end: usize) -> ByteRange {
        ByteRange { start, end }
    }

    #[test]
    fn remap_exclusions_no_fixes() {
        let excl = vec![br(10, 20), br(30, 40)];
        let result = remap_exclusions(&excl, &[]);
        assert_eq!(result, vec![br(10, 20), br(30, 40)]);
    }

    #[test]
    fn remap_exclusions_fix_before_exclusion_grows() {
        // Fix at offset 5 replaces 2 bytes with 4 bytes (+2 delta). Exclusion
        // at (10, 20) should shift to (12, 22).
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 5,
            old_len: 2,
            replacement: "abcd".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(12, 22)]);
    }

    #[test]
    fn remap_exclusions_fix_before_exclusion_shrinks() {
        // Fix at offset 2 replaces 4 bytes with 1 byte (-3 delta). Exclusion at
        // (10, 20) should shift to (7, 17).
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 2,
            old_len: 4,
            replacement: "x".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(7, 17)]);
    }

    #[test]
    fn remap_exclusions_fix_after_exclusion() {
        // Fix at offset 25 is after the exclusion at (10, 20) -- no shift.
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 25,
            old_len: 3,
            replacement: "abcdef".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(10, 20)]);
    }

    #[test]
    fn remap_exclusions_multiple_fixes_multiple_zones() {
        // Fix at 5: 2->4 (+2), fix at 25: 3->1 (-2). Exclusion (10,20) shifts
        // by +2 -> (12,22). Exclusion (30,40) shifts by +2-2=0 -> (30,40).
        let excl = vec![br(10, 20), br(30, 40)];
        let fixes = vec![
            AppliedFix {
                offset: 5,
                old_len: 2,
                replacement: "abcd".to_string(),
            },
            AppliedFix {
                offset: 25,
                old_len: 3,
                replacement: "x".to_string(),
            },
        ];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(12, 22), br(30, 40)]);
    }

    #[test]
    fn remap_exclusions_zero_length_insertion_at_boundary() {
        // Spacing fix: zero-length insertion (old_len=0) at offset 10, which is
        // exactly the exclusion start. The insertion should shift the exclusion
        // right by the replacement length.
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 10,
            old_len: 0,
            replacement: " ".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(11, 21)]);
    }
}
