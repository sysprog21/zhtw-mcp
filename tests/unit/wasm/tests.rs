use super::*;

#[test]
fn structural_symbol_exclusions_are_validated_in_utf8_bytes() {
    // "...中文" is nine bytes: three for the run and three for each ideograph.
    let options = ScanOptions {
        excluded_spans: vec![
            ExcludedSpan { start: 0, end: 3 },
            // Both endpoints land inside 中, so neither can be honoured.
            ExcludedSpan { start: 3, end: 5 },
            ExcludedSpan { start: 4, end: 6 },
            // Entirely past the end, so nothing is left after clipping.
            ExcludedSpan { start: 9, end: 12 },
            // Inverted, which no coordinate can be recovered from.
            ExcludedSpan { start: 6, end: 3 },
        ],
        ..ScanOptions::default()
    };

    assert_eq!(
        extra_excluded("...中文", &options),
        vec![ByteRange { start: 0, end: 3 }],
    );
}

#[test]
fn a_structural_symbol_exclusion_running_past_the_text_is_clipped() {
    // The scanner clips a caller range to the length of the text rather than
    // discarding it, and this side has to agree: a lang run reported one byte
    // long was honoured before excluded_spans existed and still has to be.
    let options = ScanOptions {
        excluded_spans: vec![ExcludedSpan { start: 0, end: 64 }],
        ..ScanOptions::default()
    };

    assert_eq!(
        extra_excluded("...中文", &options),
        vec![ByteRange { start: 0, end: 9 }],
    );
}

#[test]
fn structural_symbol_exclusion_filters_an_adjacent_symbol_node() {
    let text = "詳見第三章...";
    let options = ScanOptions {
        excluded_spans: vec![ExcludedSpan {
            start: text.len() - 3,
            end: text.len(),
        }],
        ..ScanOptions::default()
    };
    let scanner = scanner().expect("load browser scanner");
    let config = Profile::Base.config();

    let unexcluded =
        scanner.scan_for_content_type_with_extra_excluded(text, ContentType::Plain, config, &[]);
    assert!(unexcluded.issues.iter().any(|issue| issue.found == "..."));

    let excluded = scanner.scan_for_content_type_with_extra_excluded(
        text,
        ContentType::Plain,
        config,
        &extra_excluded(text, &options),
    );
    assert!(excluded.issues.iter().all(|issue| issue.found != "..."));
}
