// Overlap resolution for detected issues.
//
// Priority-based greedy algorithm: longest match wins; on tie, higher
// severity wins.  Avoids the ghost-suppression flaw in forward greedy scans.

use crate::rules::ruleset::Issue;

/// Remove overlapping issues from a sorted (by offset) issue list.
///
/// Priority: longer match wins; on tie, higher severity wins.
///
/// Uses a priority-based greedy algorithm: issues are processed longest-first,
/// accepted only when non-overlapping with all already-accepted matches.
/// This avoids the ghost-suppression flaw in a forward greedy scan, where A
/// can be wrongly discarded because B beats A and then C beats B — even though
/// A and C would not have overlapped.
#[allow(dead_code)]
pub(crate) fn resolve_overlaps(issues: &mut Vec<Issue>) {
    let mut order = Vec::new();
    let mut keep = Vec::new();
    let mut accepted = Vec::new();
    resolve_overlaps_with_scratch(issues, &mut order, &mut keep, &mut accepted);
}

/// Like [`resolve_overlaps`] but reuses caller-provided scratch buffers.
pub(crate) fn resolve_overlaps_with_scratch(
    issues: &mut Vec<Issue>,
    order: &mut Vec<usize>,
    keep: &mut Vec<bool>,
    accepted: &mut Vec<(usize, usize)>,
) {
    if issues.len() <= 1 {
        return;
    }

    // Process in priority order: longest first; on tie, highest severity;
    // on further tie, earliest offset (deterministic).
    let n = issues.len();

    order.clear();
    order.extend(0..n);
    order.sort_by(|&a, &b| {
        issues[b]
            .length
            .cmp(&issues[a].length)
            .then_with(|| issues[b].severity.cmp(&issues[a].severity))
            .then_with(|| issues[a].offset.cmp(&issues[b].offset))
    });

    keep.clear();
    keep.resize(n, false);

    // Accepted byte intervals (start, end), kept sorted by start offset
    // for O(log n) overlap checks via binary search.
    accepted.clear();

    for &i in order.iter() {
        let start = issues[i].offset;
        let end = start.saturating_add(issues[i].length);

        // Binary search for the insertion point, then check neighbors.
        // Two intervals [s1,e1) and [s2,e2) overlap iff s1 < e2 && e1 > s2.
        let pos = accepted.partition_point(|&(s, _)| s < start);
        let overlaps =
            // Check the interval just before (if it extends past our start).
            (pos > 0 && accepted[pos - 1].1 > start)
            // Check the interval at pos (if our end extends past its start).
            || (pos < accepted.len() && end > accepted[pos].0);

        if !overlaps {
            keep[i] = true;
            accepted.insert(pos, (start, end));
        }
    }

    // Retain in original (offset-sorted) order.
    let mut i = 0;
    issues.retain(|_| {
        let k = keep[i];
        i += 1;
        k
    });
}
