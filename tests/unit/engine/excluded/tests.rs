use super::*;

// URL exclusion

#[test]
fn url_exclusion() {
    let text = "text https://example.com text";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "https://example.com");
}

#[test]
fn url_with_custom_scheme() {
    let text = "open vscode+ssh://remote/path now";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(
        &text[ranges[0].start..ranges[0].end],
        "vscode+ssh://remote/path"
    );
}

// File path exclusion

#[test]
fn relative_path_dot_slash() {
    let text = "text ./image.png text";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "./image.png");
}

#[test]
fn relative_path_dot_dot_slash() {
    let text = "text ../config.json text";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "../config.json");
}

#[test]
fn absolute_path() {
    let text = "text /asset/icon.svg text";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "/asset/icon.svg");
}

// Path inside URL not double-excluded

#[test]
fn path_inside_url_not_duplicated() {
    // The URL pattern covers the whole thing; the path pattern should not
    // create a second range for "/user/repo".
    let text = "https://github.com/user/repo";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(
        &text[ranges[0].start..ranges[0].end],
        "https://github.com/user/repo"
    );
}

// @mentions

#[test]
fn mention_exclusion() {
    let text = "text @test_user text";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "@test_user");
}

#[test]
fn mention_inside_url_not_duplicated() {
    // @user embedded in a URL should not produce a second range.
    let text = "https://example.com/@user/profile";
    let ranges = build_excluded_ranges(text);
    assert_eq!(ranges.len(), 1);
}

#[test]
fn metalinguistic_quotes_exclude_only_the_named_term() {
    let text =
        "「軟件」是中國用語；『硬件』是中國大陸的說法；「代碼」是大陸用語；『網絡』這個詞也常見。";
    let ranges = build_excluded_ranges(text);
    let terms: Vec<&str> = ranges
        .iter()
        .map(|range| &text[range.start..range.end])
        .collect();
    assert_eq!(terms, ["軟件", "硬件", "代碼", "網絡"]);
}

#[test]
fn bare_or_later_sentence_quote_is_not_a_term_mention() {
    for text in [
        "我們用「軟件」開發服務。",
        "「軟件」。這個詞是中國用語。",
        "「軟件』是中國用語。",
        "「軟件」是產品名稱而非舊說法。",
        "「安裝軟件後，請按下確定按鈕」這個詞需要說明。",
    ] {
        assert!(build_excluded_ranges(text).is_empty(), "{text}");
    }
}

#[test]
fn metalinguistic_term_cap_is_twelve_characters() {
    assert_eq!(
        build_excluded_ranges("「一二三四五六七八九十甲乙」一詞可見。\n").len(),
        1
    );
    assert!(build_excluded_ranges("「一二三四五六七八九十甲乙丙」一詞可見。\n").is_empty());
}

// Merge overlapping ranges

#[test]
fn multiple_overlapping_ranges_merge() {
    // Fabricate ranges that overlap and verify merge.
    let raw = vec![
        ByteRange { start: 0, end: 5 },
        ByteRange { start: 3, end: 8 },
        ByteRange { start: 10, end: 15 },
        ByteRange { start: 14, end: 20 },
        ByteRange { start: 25, end: 30 },
    ];
    let merged = merge_ranges_pub(raw);
    assert_eq!(
        merged,
        vec![
            ByteRange { start: 0, end: 8 },
            ByteRange { start: 10, end: 20 },
            ByteRange { start: 25, end: 30 },
        ]
    );
}

#[test]
fn adjacent_ranges_merge() {
    let raw = vec![
        ByteRange { start: 0, end: 5 },
        ByteRange { start: 5, end: 10 },
    ];
    let merged = merge_ranges_pub(raw);
    assert_eq!(merged, vec![ByteRange { start: 0, end: 10 }]);
}

// is_excluded (linear scan path, <= 10 ranges)

#[test]
fn is_excluded_linear() {
    let ranges = vec![
        ByteRange { start: 5, end: 10 },
        ByteRange { start: 20, end: 25 },
    ];
    // Point-like spans (1-byte)
    assert!(!is_excluded(0, 1, &ranges));
    assert!(!is_excluded(4, 5, &ranges));
    assert!(is_excluded(5, 6, &ranges));
    assert!(is_excluded(9, 10, &ranges));
    assert!(!is_excluded(10, 11, &ranges));
    assert!(is_excluded(20, 21, &ranges));
    assert!(!is_excluded(25, 26, &ranges));
    // Span that starts before and ends inside excluded range
    assert!(is_excluded(3, 7, &ranges));
    // Span that starts inside and ends after excluded range
    assert!(is_excluded(8, 12, &ranges));
    // Span that fully contains excluded range
    assert!(is_excluded(0, 30, &ranges));
}

// is_excluded (binary search path, > 10 ranges)

#[test]
fn is_excluded_binary_search() {
    // Build 12 non-overlapping ranges so we exercise the binary-search path
    // (threshold is >10).
    let ranges: Vec<ByteRange> = (0..12)
        .map(|i| ByteRange {
            start: i * 10,
            end: i * 10 + 5,
        })
        .collect();
    assert_eq!(ranges.len(), 12);

    // Inside first range
    assert!(is_excluded(0, 1, &ranges));
    assert!(is_excluded(4, 5, &ranges));
    assert!(!is_excluded(5, 6, &ranges));

    // Inside last range (110..115)
    assert!(is_excluded(110, 111, &ranges));
    assert!(is_excluded(114, 115, &ranges));
    assert!(!is_excluded(115, 116, &ranges));

    // Gap between ranges
    assert!(!is_excluded(7, 8, &ranges));
    assert!(!is_excluded(55, 56, &ranges));
    assert!(!is_excluded(99, 100, &ranges));

    // Inside middle range (60..65)
    assert!(is_excluded(60, 61, &ranges));
    assert!(is_excluded(64, 65, &ranges));

    // Span overlapping boundary (straddles gap and range)
    assert!(is_excluded(3, 7, &ranges));
    assert!(is_excluded(58, 62, &ranges));
}

#[test]
fn is_excluded_empty() {
    assert!(!is_excluded(0, 1, &[]));
    assert!(!is_excluded(100, 101, &[]));
}

// build_excluded_ranges does NOT cover backticks (code block exclusion is
// handled by pulldown-cmark in markdown.rs)

#[test]
fn backticks_not_excluded_by_content_ranges() {
    // build_excluded_ranges handles URLs/paths/mentions only. Backtick-based
    // code exclusion is now handled by pulldown-cmark.
    let text = "text `code` more ```block``` end";
    let ranges = build_excluded_ranges(text);

    // Empty is the claim. Checking only that each range avoids the words would
    // pass a range covering the backticks themselves.
    assert!(ranges.is_empty(), "got {ranges:?}");
    for r in &ranges {
        let matched = &text[r.start..r.end];
        assert!(
            !matched.contains("code") && !matched.contains("block"),
            "backtick content should not be excluded by content ranges: got {:?}",
            matched
        );
    }
}

#[test]
fn mask_excluded_blanks_ranges_and_keeps_every_offset() {
    let text = "前文 `受試者` 後文";
    let start = text.find('`').unwrap();
    let end = text.rfind('`').unwrap() + 1;
    let masked = mask_excluded(text, &[ByteRange { start, end }]);
    assert_eq!(masked.len(), text.len());
    assert!(!masked.contains("受試者"));
    assert!(masked.starts_with("前文 "));
    assert!(masked.ends_with(" 後文"));
    assert_eq!(&masked[start..end], " ".repeat(end - start));
}

#[test]
fn mask_excluded_keeps_line_breaks_inside_a_range() {
    let text = "a `b\n「\nc` d";
    let end = text.rfind('`').unwrap() + 1;
    let masked = mask_excluded(text, &[ByteRange { start: 2, end }]);
    assert_eq!(masked.matches('\n').count(), 2);
    assert_eq!(masked.len(), text.len());
    assert!(!masked.contains('「'));
}

#[test]
fn mask_excluded_with_no_ranges_is_the_text() {
    assert_eq!(mask_excluded("原文", &[]), "原文");
}

#[test]
fn mask_excluded_clamps_a_range_past_the_end() {
    let masked = mask_excluded("ab", &[ByteRange { start: 1, end: 99 }]);
    assert_eq!(masked, "a ");
}

#[test]
fn mask_excluded_widens_a_range_that_splits_a_character() {
    // 茲 is three bytes; a range covering only its first byte still blanks the
    // whole character, and the result is valid text of the same length.
    let text = "茲將";
    let masked = mask_excluded(text, &[ByteRange { start: 1, end: 2 }]);
    assert_eq!(masked.len(), text.len());
    assert_eq!(masked, "   將");
}

#[test]
fn mask_excluded_handles_overlapping_unsorted_and_inverted_ranges() {
    let text = "abcdef";
    let masked = mask_excluded(
        text,
        &[
            ByteRange { start: 4, end: 6 },
            ByteRange { start: 1, end: 3 },
            ByteRange { start: 2, end: 5 },
            ByteRange { start: 4, end: 2 },
        ],
    );
    assert_eq!(masked, "a     ");
}
