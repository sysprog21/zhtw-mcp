//! Test helpers shared by more than one integration target.

use std::collections::HashSet;

/// The largest pair of lengths (in characters) the diff aligns; a longer pair
/// fails the test rather than return a misleading answer.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// One edit from "src" to "dst": dropping source character i is "(i, None, 0)",
/// inserting character c before source character i is "(i, Some(c), k)", where
/// k counts the insertions already made before i, so inserting two characters
/// at one place is two edits and does not collapse into one.
pub type Edit = (usize, Option<char>, usize);

/// The edits between "src" and "dst", from a longest common subsequence
/// alignment. When several alignments are equally long the one that drops the
/// earlier character is taken, so equal inputs always give equal edit sets.
pub fn changed_positions(src: &[char], dst: &[char]) -> HashSet<Edit> {
    if src == dst {
        return HashSet::new();
    }
    let (n, m) = (src.len(), dst.len());

    // Pairs this long never occur in UI strings. A drop-all answer would look
    // like a real diff and inflate every count built on it, so fail instead.
    assert!(
        n.saturating_mul(m) <= MAX_DIFF_CELLS,
        "a pair is too long for the character diff: split it"
    );
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

    // Insertions already made before the current position; only an insertion
    // leaves i unchanged, so a run of them is consecutive and the count resets
    // when i advances.
    let mut run = 0;
    while i < n || j < m {
        if i < n && j < m && src[i] == dst[j] {
            i += 1;
            j += 1;
            run = 0;
        } else if j == m || (i < n && lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            changed.insert((i, None, 0));
            i += 1;
            run = 0;
        } else {
            changed.insert((i, Some(dst[j]), run));
            run += 1;
            j += 1;
        }
    }
    changed
}
