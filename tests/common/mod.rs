//! Test helpers shared by more than one integration target.

use std::collections::HashSet;

/// The largest pair of lengths (in characters) the diff aligns; beyond it every
/// source character is reported as dropped. A caller that needs a real answer
/// checks its inputs against this first.
pub const MAX_DIFF_CELLS: usize = 4_000_000;

/// One edit from "src" to "dst": dropping source character i is "(i, None)",
/// inserting character c before source character i is "(i, Some(c))".
pub type Edit = (usize, Option<char>);

/// The edits between "src" and "dst", from a longest common subsequence
/// alignment. When several alignments are equally long the one that drops the
/// earlier character is taken, so equal inputs always give equal edit sets.
pub fn changed_positions(src: &[char], dst: &[char]) -> HashSet<Edit> {
    if src == dst {
        return HashSet::new();
    }
    let (n, m) = (src.len(), dst.len());

    // Pairs this long never occur in UI strings; treat every source character
    // as dropped rather than build a quadratic table.
    if n.saturating_mul(m) > MAX_DIFF_CELLS {
        return (0..n).map(|i| (i, None)).collect();
    }
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if src[i] == dst[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let mut changed = HashSet::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && src[i] == dst[j] {
            i += 1;
            j += 1;
        } else if j == m || (i < n && lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            changed.insert((i, None));
            i += 1;
        } else {
            changed.insert((i, Some(dst[j])));
            j += 1;
        }
    }
    changed
}
