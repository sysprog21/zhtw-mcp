// Unicode normalization for consistent scanning.
//
// Applies NFC normalization before scanning so that identical-looking text with
// different byte representations produces identical diagnostics.
// Returns the normalized text and a byte-offset mapping from normalized
// positions back to original positions.

use std::borrow::Cow;

use unicode_normalization::{IsNormalized, UnicodeNormalization};

/// Result of NFC normalization with offset mapping.
pub struct Normalized<'a> {
    /// NFC-normalized text. Borrows the original when already NFC.
    pub text: Cow<'a, str>,
    /// Maps each byte index in text to the corresponding byte index in the
    /// original input. Length equals text.len() + 1 (the extra entry maps
    /// the end-of-string position).
    ///
    /// Empty when text is already NFC (identity mapping). Use map_offset
    /// which handles the empty case by returning the input offset unchanged.
    pub offset_map: Vec<usize>,
}

/// Normalize input to NFC, returning the normalized text and a byte-offset
/// mapping back to the original.
///
/// If the input is already in NFC, the mapping is an identity (each index
/// maps to itself). When normalization changes character boundaries, the
/// mapping tracks how each normalized byte relates to the original.
pub fn normalize_nfc(input: &str) -> Normalized<'_> {
    // Fast path: quick-check first so common clean zh-TW input avoids the full
    // normalization walk and any allocation. Fall back to the exact check only
    // for the indeterminate Maybe case; skip it entirely when the answer is
    // already definitive (Yes or No).
    match unicode_normalization::is_nfc_quick(input.chars()) {
        IsNormalized::Yes => {
            return Normalized {
                text: Cow::Borrowed(input),
                offset_map: Vec::new(),
            };
        }
        IsNormalized::Maybe if unicode_normalization::is_nfc(input) => {
            return Normalized {
                text: Cow::Borrowed(input),
                offset_map: Vec::new(),
            };
        }
        _ => {} // No or Maybe-and-not-NFC: proceed to normalize below
    }

    // Build the offset mapping one normalization segment at a time.
    //
    // A segment is a starter plus the combining marks that follow it, which is
    // the largest unit NFC can rewrite: composition never joins two starters
    // (Hangul jamo excepted, handled below) and canonical reordering never
    // moves a mark past one. Normalizing each segment separately therefore
    // concatenates to the same string as normalizing the whole input, and every
    // byte it produces belongs to that one segment.
    //
    // The previous version walked output chars and skipped any original
    // combining mark that did not equal the current one, assuming it had been
    // absorbed. A mark can also expand: U+0344 becomes U+0308 U+0301, one char
    // into two. The skip then consumed the following base character, and every
    // offset after it pointed at the wrong place, so a fix wrote its
    // replacement over neighbouring text. The normalized text is assembled from
    // the same segments the map is built from, so it is produced once rather
    // than normalized whole and then again per segment.
    let mut nfc_text = String::with_capacity(input.len());
    let mut offset_map = Vec::with_capacity(input.len() + 1);
    let mut segment_start = 0usize;
    let mut prev: Option<char> = None;

    for (i, ch) in input.char_indices() {
        if i > 0 && starts_segment(ch, prev) {
            map_segment(
                &input[segment_start..i],
                segment_start,
                &mut nfc_text,
                &mut offset_map,
            );
            segment_start = i;
        }
        prev = Some(ch);
    }
    map_segment(
        &input[segment_start..],
        segment_start,
        &mut nfc_text,
        &mut offset_map,
    );

    // End-of-string sentinel.
    offset_map.push(input.len());

    Normalized {
        text: Cow::Owned(nfc_text),
        offset_map,
    }
}

/// Map one segment's normalized bytes back to original offsets.
///
/// Everything composed into the base maps to the segment start, which is the
/// position a reader wants for a span. Trailing combining marks that survive
/// normalization are paired against the input's trailing marks counted from
/// the end, so a mark NFC could not absorb points into the mark run rather
/// than at the base.
///
/// Pairing by position, not by identity, and that is a deliberate ceiling. The
/// map has to be non-decreasing, because a span is translated by indexing it,
/// while canonical reordering is by definition a permutation: in
/// "a\u{0344}\u{0316}" the surviving U+0301 comes from the mark at byte 1 but
/// NFC emits it after the mark from byte 3. No monotonic map can say that, so
/// this one points each surviving mark at the input mark in the same position
/// from the end.
///
/// The base's end boundary inherits that: in "e\u{0316}\u{0301}" the base
/// composes with the mark at byte 3 while the mark at byte 1 survives, so the
/// boundary after the base names byte 3 and a replacement of the base alone
/// would cover a mark that is still in the output. There is no monotonic
/// assignment that avoids this, because the consumed mark sits after the
/// surviving one in the input and before it in the output. The error stays
/// inside one combining run, and no rule in this linter targets a bare
/// combining sequence, so it costs nothing today. Fixing it means per-span
/// provenance instead of a byte map, and a different lookup everywhere.
fn map_segment(segment: &str, start: usize, nfc_text: &mut String, offset_map: &mut Vec<usize>) {
    let before = nfc_text.len();
    nfc_text.extend(segment.nfc());
    let normalized = &nfc_text[before..];

    // Both tails run backward from the end, so zip pairs the marks that
    // correspond. Everything composed into the base maps to the segment start.
    let origins: Vec<usize> = segment
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_combining_mark(c))
        .map(|(i, _)| start + i)
        .collect();
    let tail: Vec<usize> = normalized
        .chars()
        .rev()
        .take_while(|&c| is_combining_mark(c))
        .map(char::len_utf8)
        .collect();

    let tail_bytes: usize = tail.iter().sum();
    offset_map.resize(offset_map.len() + normalized.len() - tail_bytes, start);
    for (i, &n) in tail.iter().enumerate().rev() {
        // NFC can expand one input mark into several output marks (U+0344),
        // leaving more output marks than input ones. The shift pairs the two
        // runs from the end, so the excess falls on the last input mark, which
        // is right when that is the mark that expanded and keeps the excess
        // inside the mark run either way. Never on the segment's base: an
        // output mark did not come from there.
        let origin = origins
            .get(i.saturating_sub(tail.len().saturating_sub(origins.len())))
            .copied()
            .unwrap_or(start);
        offset_map.resize(offset_map.len() + n, origin);
    }
}

/// Whether "ch" begins a new normalization segment, given the character
/// before it.
///
/// A combining mark never does. Neither does a Hangul jamo that composes with
/// what precedes it, which is the one case where two starters combine.
///
/// The pair has to be one Hangul composition actually forms, not merely two
/// Hangul characters. Refusing to split any adjacent pair swallowed a run of
/// complete syllables into one segment, and a segment maps every byte that is
/// not a trailing mark to its start, so 각각각 reported all three syllables at
/// the offset of the first and a fix would have rewritten the wrong one.
fn starts_segment(ch: char, prev: Option<char>) -> bool {
    if is_combining_mark(ch) {
        return false;
    }
    let Some(prev) = prev else {
        return true;
    };

    // UAX #15 Hangul composition, the only case where a starter joins another:
    // a leading jamo takes a vowel, and an LV syllable takes a trailing jamo.
    // Two complete syllables never compose, so they are separate segments.
    const L: std::ops::RangeInclusive<u32> = 0x1100..=0x1112;
    const V: std::ops::RangeInclusive<u32> = 0x1161..=0x1175;
    const T: std::ops::RangeInclusive<u32> = 0x11A8..=0x11C2;
    const S: std::ops::RangeInclusive<u32> = 0xAC00..=0xD7A3;
    let (prev, cur) = (u32::from(prev), u32::from(ch));

    // The trailing jamo follows a vowel as well as an LV syllable, because the
    // walk sees the source characters: in L V T the character before T is the
    // vowel, and the L+V it composed with is not written down anywhere.
    let composes = (L.contains(&prev) && V.contains(&cur))
        || (V.contains(&prev) && T.contains(&cur))
        || (S.contains(&prev) && (prev - S.start()).is_multiple_of(28) && T.contains(&cur));
    !composes
}

/// Whether the character is a Unicode combining mark (General_Category=Mark).
///
/// From the normalization tables that are already linked for NFC, not a
/// hand-listed set of blocks. The block list covered five ranges and missed
/// Arabic, Hebrew, Devanagari and Cyrillic marks, which made those characters
/// look like segment starters: an Arabic alef followed by a maddah then
/// composes inside "nfc()" but not across the split segments, and the offset
/// map ends up longer than the string it indexes.
fn is_combining_mark(ch: char) -> bool {
    unicode_normalization::char::is_combining_mark(ch)
}

/// Map a byte offset in normalized text back to the original text.
///
/// When offset_map is empty (identity mapping from NFC fast path),
/// the offset is returned unchanged. Otherwise, out-of-bounds offsets
/// are clamped to the original text length.
pub(crate) fn map_offset(offset_map: &[usize], normalized_offset: usize) -> usize {
    if offset_map.is_empty() {
        return normalized_offset;
    }
    if normalized_offset >= offset_map.len() {
        *offset_map.last().unwrap_or(&0)
    } else {
        offset_map[normalized_offset]
    }
}

/// Map a byte range in the original text to the corresponding range in the
/// normalized text.
///
/// The inverse direction of map_offset, for ranges a caller computed against
/// the text it handed in. A normalized byte belongs to the answer when the
/// original offset it records falls inside the range, which is one binary
/// search at each end of a map that is non-decreasing.
///
/// The start needs one correction on top of that. A character NFC composed
/// away leaves no normalized byte recording its offset, so a range that opens
/// on such a character would start after the byte that absorbed it and leave
/// it scannable. When the search lands on a gap rather than on the offset
/// asked for, the start walks back over the whole composed scalar. Rounding
/// outward is the safe direction for an exclusion.
///
/// Returns None when the range maps to nothing: an empty or inverted input
/// range, or one that lies past the end of the text.
pub(crate) fn map_range_forward(
    offset_map: &[usize],
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if start >= end {
        return None;
    }
    if offset_map.is_empty() {
        return Some((start, end));
    }

    let mut mapped_start = offset_map.partition_point(|&origin| origin < start);
    let mapped_end = offset_map.partition_point(|&origin| origin < end);

    // A gap means the byte before the landing point absorbed the offset asked
    // for. Take that byte, and the rest of the scalar it belongs to, which is
    // the run of bytes sharing its origin.
    if mapped_start > 0 && offset_map.get(mapped_start).is_some_and(|&o| o > start) {
        let absorbed = offset_map[mapped_start - 1];
        mapped_start = offset_map.partition_point(|&origin| origin < absorbed);
    }

    (mapped_start < mapped_end).then_some((mapped_start, mapped_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A range mapped forward has to cover every normalized byte that carries
    /// any part of the original range, or an exclusion leaks.
    fn covers(input: &str, start: usize, end: usize) -> (usize, usize) {
        let norm = normalize_nfc(input);
        map_range_forward(&norm.offset_map, start, end).expect("range maps to something")
    }

    #[test]
    fn forward_mapping_is_identity_when_already_nfc() {
        let input = "Hello 你好世界";
        let norm = normalize_nfc(input);
        assert!(norm.offset_map.is_empty());
        assert_eq!(map_range_forward(&norm.offset_map, 2, 8), Some((2, 8)));
    }

    #[test]
    fn forward_mapping_rejects_an_empty_or_inverted_range() {
        let norm = normalize_nfc("cafe\u{301}");
        assert_eq!(map_range_forward(&norm.offset_map, 3, 3), None);
        assert_eq!(map_range_forward(&norm.offset_map, 5, 2), None);
    }

    #[test]
    fn forward_mapping_covers_the_bytes_after_a_composition() {
        // "cafe" plus a combining acute: five chars in, four out, so every
        // offset after the mark moves by two bytes.
        let input = "cafe\u{301} 用";
        let tail = input.find('用').expect("fixture contains the character");
        let (start, end) = covers(input, tail, input.len());
        let norm = normalize_nfc(input);
        assert_eq!(&norm.text[start..end], "用");
    }

    #[test]
    fn forward_mapping_keeps_a_range_that_opens_on_a_combining_mark() {
        // The mark composes into the base before it, so no normalized byte
        // records the mark's own offset. Rounding the start inward would drop
        // the composed character out of the range entirely.
        let input = "e\u{301}";
        let norm = normalize_nfc(input);
        let (start, end) = map_range_forward(&norm.offset_map, 1, input.len())
            .expect("a range over the mark maps to something");
        assert_eq!(&norm.text[start..end], "\u{e9}");
    }

    #[test]
    fn forward_mapping_of_every_prefix_covers_what_it_should() {
        // Exhaustive over one string that composes, reorders and expands, so a
        // boundary rule that works only on the fixtures above fails here.
        let input = "a\u{301}b e\u{344}\u{316}c 用字";
        let norm = normalize_nfc(input);
        assert!(
            !norm.offset_map.is_empty(),
            "fixture must not be NFC already"
        );
        for start in 0..input.len() {
            for end in start + 1..=input.len() {
                let Some((s, e)) = map_range_forward(&norm.offset_map, start, end) else {
                    continue;
                };
                assert!(
                    s < e && e <= norm.text.len(),
                    "{start}..{end} mapped to {s}..{e}"
                );

                // Every normalized byte whose recorded origin is inside the
                // range has to be inside the mapped range.
                for (i, &origin) in norm.offset_map.iter().enumerate().take(norm.text.len()) {
                    if (start..end).contains(&origin) {
                        assert!(
                            (s..e).contains(&i),
                            "byte {i} (origin {origin}) escaped {start}..{end} -> {s}..{e}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn already_nfc_identity() {
        let input = "Hello 你好世界";
        let norm = normalize_nfc(input);
        assert_eq!(&*norm.text, input);
        // Fast path: empty offset_map means identity mapping.
        assert!(norm.offset_map.is_empty());
        for i in 0..=input.len() {
            assert_eq!(map_offset(&norm.offset_map, i), i);
        }
    }

    #[test]
    fn nfc_composed_vs_decomposed() {
        // U+0065 U+0301 (e + combining acute) -> U+00E9 (é precomposed).
        let decomposed = "e\u{0301}";
        let norm = normalize_nfc(decomposed);
        assert_eq!(norm.text, "\u{00E9}"); // NFC form: é
        assert_eq!(norm.text.len(), 2); // é is 2 UTF-8 bytes
                                        // The normalized é maps back to byte 0
                                        // (the 'e' position).
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // End sentinel maps to original end.
        assert_eq!(
            map_offset(&norm.offset_map, norm.text.len()),
            decomposed.len()
        );
    }

    #[test]
    fn nfc_with_surrounding_text() {
        // "ae\u{0301}b" -> "aéb" after NFC.
        let input = "ae\u{0301}b";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, "a\u{00E9}b");
        // 'a' at norm byte 0 maps to orig byte 0.
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // 'é' at norm byte 1 maps to orig byte 1 (the 'e').
        assert_eq!(map_offset(&norm.offset_map, 1), 1);

        // 'b' at norm byte 3 maps to orig byte 4 (after e + combining = 3
        // bytes).
        assert_eq!(map_offset(&norm.offset_map, 3), 4);
    }

    #[test]
    fn cjk_text_unchanged() {
        let input = "繁體中文測試";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, input);
    }

    #[test]
    fn mixed_content() {
        // Mix of ASCII, CJK, and precomposed chars - all already NFC.
        let input = "Hello 你好 café";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, input);
    }

    #[test]
    fn map_offset_out_of_bounds() {
        // For NFC fast path, map_offset returns the input offset unchanged.
        let input = "abc";
        let norm = normalize_nfc(input);
        assert_eq!(map_offset(&norm.offset_map, 100), 100);
        // For non-NFC input, map_offset clamps to original length.
        let decomposed = "e\u{0301}";
        let norm2 = normalize_nfc(decomposed);
        assert_eq!(map_offset(&norm2.offset_map, 100), decomposed.len());
    }

    #[test]
    fn empty_input() {
        let norm = normalize_nfc("");
        assert_eq!(&*norm.text, "");
        // Empty string is NFC, so fast path: empty offset_map.
        assert!(norm.offset_map.is_empty());
    }

    #[test]
    fn stacked_combining_marks_offset() {
        // "a + U+0301 + U+0301" → NFC is "á + U+0301" (first mark absorbed).
        // The remaining U+0301 in NFC output must map to byte 3 in the original
        // (the second mark), not byte 1 (the first, absorbed mark).
        //
        // - Original bytes: a(0), U+0301(1-2), U+0301(3-4), 5 in total.
        // - NFC bytes: á(0-1), U+0301(2-3), 4 in total.
        let input = "a\u{0301}\u{0301}";
        assert_eq!(input.len(), 5); // a=1, U+0301=2, U+0301=2
        let norm = normalize_nfc(input);
        // NFC: á (U+00E9 = 2 bytes) + U+0301 (2 bytes)
        assert_eq!(norm.text.len(), 4);
        // The á at NFC byte 0 maps to orig byte 0 (the 'a').
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // The remaining U+0301 at NFC byte 2 maps to orig byte 3 (second mark).
        assert_eq!(map_offset(&norm.offset_map, 2), 3);
    }

    // Two complete Hangul syllables do not compose, so they are separate
    // segments. Coalescing them mapped every byte of the run to the offset of
    // the first syllable.
    #[test]
    fn adjacent_hangul_syllables_keep_their_own_offsets() {
        let norm = normalize_nfc("각각각 e\u{0301}");
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        assert_eq!(map_offset(&norm.offset_map, 3), 3);
        assert_eq!(map_offset(&norm.offset_map, 6), 6);
    }

    // The pairs that do compose still must not be split: the walk sees source
    // characters, so the trailing jamo follows the vowel, not the LV syllable
    // the two of them will become.
    #[test]
    fn decomposed_hangul_jamo_still_compose() {
        let norm = normalize_nfc("\u{1100}\u{1161}\u{11A8} e\u{0301}");
        assert_eq!(norm.text, "\u{AC01} \u{00E9}");
    }

    #[test]
    fn expanded_combining_mark_maps_to_its_source() {
        let input = "中\u{0344}b";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, "中\u{0308}\u{0301}b");
        assert_eq!(map_offset(&norm.offset_map, 3), 3);
        assert_eq!(map_offset(&norm.offset_map, 5), 3);
        assert_eq!(map_offset(&norm.offset_map, 7), 5);
    }
}

#[cfg(test)]
mod invariants {
    use super::*;
    use unicode_normalization::UnicodeNormalization;

    /// Deterministic xorshift, so a failure is reproducible from the seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick<T: Copy>(&mut self, from: &[T]) -> T {
            from[(self.next() % from.len() as u64) as usize]
        }
    }

    // The offset map is what every reported span is translated through, so a
    // single wrong entry writes a fix over neighbouring text. Four properties
    // have to hold for any input, and the alphabet below is chosen to exercise
    // the cases the segment walk reasons about: composing pairs, a mark that
    // expands into two (U+0344), canonical reordering across combining classes,
    // Hangul jamo that compose across starters, and scripts whose marks the old
    // block list missed.
    #[test]
    fn the_offset_map_holds_for_random_input() {
        const ALPHABET: &[char] = &[
            'a', 'e', '中', 'b', '\u{0301}', '\u{0300}', '\u{0308}', '\u{0344}', '\u{0315}',
            '\u{05B0}', '\u{0654}', '\u{093C}', '\u{094D}', '\u{0915}', '\u{1100}', '\u{1161}',
            '\u{11A8}', '\u{AC00}', 'ا', 'ب', ' ', '。',
        ];
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for case in 0..200_000u32 {
            let len = (rng.next() % 8) as usize;
            let input: String = (0..len).map(|_| rng.pick(ALPHABET)).collect();
            let norm = normalize_nfc(&input);

            let expected: String = input.nfc().collect();
            assert_eq!(&*norm.text, expected, "case {case}: {input:?}");

            if norm.offset_map.is_empty() {
                // Fast path: identity, only taken when the input is already
                // NFC.
                assert_eq!(&*norm.text, input, "case {case}: {input:?}");
                continue;
            }

            assert_eq!(
                norm.offset_map.len(),
                norm.text.len() + 1,
                "case {case}: map length must cover every byte plus the sentinel: {input:?}"
            );
            assert!(
                norm.offset_map.windows(2).all(|w| w[0] <= w[1]),
                "case {case}: map must not run backwards: {input:?} -> {:?}",
                norm.offset_map
            );
            for (i, &origin) in norm.offset_map.iter().enumerate() {
                assert!(
                    origin <= input.len() && input.is_char_boundary(origin),
                    "case {case}: entry {i} = {origin} is not a char boundary in {input:?}"
                );
            }
        }
    }
}
