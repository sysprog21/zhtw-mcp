use super::*;

#[test]
fn build_clue_index_sorts_overlapping_hits_by_start_offset() {
    let ac = AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .build(["aba", "ba", "a"])
        .expect("build clue AC");
    let mut index = Vec::new();

    build_clue_index_into(Some(&ac), "aba", &mut index);

    assert!(
        index.windows(2).all(|w| w[0].0 <= w[1].0),
        "clue hits must be sorted by start offset: {index:?}"
    );
}

#[test]
fn lookup_clues_counts_distinct_positive_ids_across_overlaps() {
    let clue_index = vec![(0, 2), (0, 2), (1, 3), (2, 5)];
    let pos_ids = vec![2, 3, 5];

    let (pos_found, any_neg) = lookup_clues_in_window(&clue_index, 0, 3, Some(&pos_ids), None);

    assert_eq!(pos_found, 3, "should count distinct positive clue ids");
    assert!(!any_neg, "no negative clues should be reported");
}

#[test]
fn lookup_clues_negative_hit_vetoes_same_offset_window() {
    let clue_index = vec![(0, 2), (0, 7), (1, 3)];
    let pos_ids = vec![2, 3];
    let neg_ids = vec![7];

    let (pos_found, any_neg) =
        lookup_clues_in_window(&clue_index, 0, 2, Some(&pos_ids), Some(&neg_ids));

    assert_eq!(
        pos_found, 1,
        "positive clue before veto should still be counted"
    );
    assert!(any_neg, "negative clue at same offset must veto");
}

/// Whether the first occurrence of the rule pattern in text is covered by any
/// of the exception phrases, stored the way the compiler stores them: the
/// phrase plus the byte offsets of the pattern inside it.
fn spared(text: &str, phrases: &[&str]) -> bool {
    const PATTERN: &str = "茲將";
    let forms: Vec<(String, Vec<usize>)> = phrases
        .iter()
        .map(|p| {
            let offsets = p.match_indices(PATTERN).map(|(at, _)| at).collect();
            (p.to_string(), offsets)
        })
        .collect();
    let start = text.find(PATTERN).expect("pattern occurs in the text");
    covered_by_form(text, start, &forms)
}

#[test]
fn the_exception_phrase_decides_what_is_covered() {
    let phrases = ["公告，茲將"];
    for (text, want, why) in [
        (
            "區公所昨天發布公告，茲將下週三調整。",
            true,
            "inside the phrase",
        ),
        (
            "小公告，茲將名單附後。",
            true,
            "no left boundary, a known limit",
        ),
        ("公告：茲將名單附後。", false, "another separator"),
        (
            "公告很多，我認為茲將重點整理如下。",
            false,
            "too far from it",
        ),
        (
            "茲將重點整理如下。",
            false,
            "the phrase would start before the text",
        ),
    ] {
        assert_eq!(spared(text, &phrases), want, "{why}: {text}");
    }
}

#[test]
fn a_phrase_running_past_the_end_of_the_text_is_not_covered() {
    assert!(!spared("茲將公", &["茲將公告"]));
}

#[test]
fn an_offset_inside_a_multibyte_character_is_not_covered() {
    // The phrase starts one byte before the match, in the middle of the
    // preceding three-byte character; that must read as no match, not panic.
    assert!(!spared("茲茲將", &["x茲將"]));
}

#[test]
fn every_occurrence_of_the_pattern_in_the_phrase_counts() {
    // 茲將 occurs twice in the phrase; a match at either position is covered.
    let forms = [("茲將，茲將".to_string(), vec![0, "茲將，".len()])];
    assert!(covered_by_form("茲將，茲將", 0, &forms));
    assert!(covered_by_form("茲將，茲將", "茲將，".len(), &forms));
}

#[test]
fn a_later_form_can_cover_when_an_earlier_one_does_not() {
    assert!(spared(
        "昨天公告，茲將名單附後。",
        &["本部公告，茲將", "公告，茲將"]
    ));
}

#[test]
fn no_forms_cover_nothing() {
    assert!(!spared("公告，茲將名單附後。", &[]));
    let no_offsets = [("公告，茲將".to_string(), Vec::new())];
    assert!(!covered_by_form(
        "公告，茲將名單附後。",
        "公告，".len(),
        &no_offsets
    ));
}
