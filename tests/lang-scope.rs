// End-to-end checks for the lang attribute honored by the scanner.
//
// Two seams meet here. Markdown derives the exclusion itself, from the HTML
// tags the author wrote. The browser extension cannot: it flattens a page into
// one string, so it hands the ranges over and the engine has to honor them on
// both the fast path and the one that rebuilds after NFC normalization.

use zhtw_mcp::engine::excluded::ByteRange;
use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::ruleset::{Profile, RuleType, SpellingRule};

fn cross_strait(from: &str, to: &str) -> SpellingRule {
    SpellingRule::new(from, vec![to.into()], RuleType::CrossStrait)
}

fn scanner() -> Scanner {
    Scanner::new(vec![cross_strait("软件", "軟體")], vec![])
}

fn markdown_hits(text: &str) -> Vec<String> {
    scanner()
        .scan_for_content_type(text, ContentType::Markdown, Profile::Base)
        .issues
        .into_iter()
        .map(|issue| issue.found)
        .collect()
}

/// What a plain-text scan reports with the caller holding some of the text
/// back. The seam the browser extension uses.
fn plain_hits(text: &str, excluded: &[ByteRange]) -> Vec<String> {
    scanner()
        .scan_for_content_type_with_extra_excluded(
            text,
            ContentType::Plain,
            Profile::Base.config(),
            excluded,
        )
        .issues
        .into_iter()
        .map(|issue| issue.found)
        .collect()
}

/// The byte range covering the fixture term and everything after it.
fn from_term(text: &str) -> ByteRange {
    ByteRange {
        start: text.find("软件").expect("fixture contains the term"),
        end: text.len(),
    }
}

/// The same Markdown with the declaration taken off the tag that carries it.
///
/// A test that asserts silence has to show the declaration is what caused it.
/// Two of these fixtures were written from the report that opened the issue,
/// where the flagged mark was an ASCII comma, and a later change stopped
/// flagging that inside an embedded English clause: the tests still passed and
/// had stopped proving anything. Each one now checks its own control.
fn without_lang(md: &str) -> String {
    md.replace(" lang=\"en\"", "")
}

// Markdown

#[test]
fn a_block_marked_english_is_not_scanned() {
    let md = "<div lang=\"en\">\n\nWe ship 软件, 對吧。\n\n</div>\n";
    assert!(
        markdown_hits(md).is_empty(),
        "prose inside a lang=en block was scanned: {:?}",
        markdown_hits(md)
    );
    assert!(
        !markdown_hits(&without_lang(md)).is_empty(),
        "the fixture says nothing without the declaration, so the test proves nothing"
    );
}

#[test]
fn an_inline_span_marked_english_is_not_scanned() {
    let md = "他說<span lang=\"en\">we ship 软件, 但</span>結束\n";
    assert!(
        markdown_hits(md).is_empty(),
        "prose inside a lang=en span was scanned: {:?}",
        markdown_hits(md)
    );
    assert!(
        !markdown_hits(&without_lang(md)).is_empty(),
        "the fixture says nothing without the declaration, so the test proves nothing"
    );
}

#[test]
fn a_span_marked_zh_tw_is_still_scanned() {
    let md = "他說<span lang=\"zh-TW\">用 软件, 對</span>結束\n";
    assert_eq!(markdown_hits(md), vec!["软件".to_owned(), ",".to_owned()]);
}

#[test]
fn an_english_scope_does_not_reach_past_its_closing_tag() {
    let md = "<span lang=\"en\">software</span>用 软件, 對\n";
    assert_eq!(markdown_hits(md), vec!["软件".to_owned(), ",".to_owned()]);
}

// Caller-supplied ranges, the seam the browser extension uses

#[test]
fn caller_ranges_take_text_out_of_the_scan() {
    let text = "用 软件, 對";
    let found = plain_hits(text, &[from_term(text)]);
    assert!(found.is_empty(), "caller range was ignored: {found:?}");
}

#[test]
fn caller_ranges_survive_nfc_normalization() {
    // The decomposed "é" is two chars before normalization and one after, so
    // every offset after it moves. A range measured against the text the caller
    // handed in has to be mapped forward, not used as-is.
    let text = "cafe\u{301} 用 软件, 對";
    let found = plain_hits(text, &[from_term(text)]);
    assert!(
        found.is_empty(),
        "caller range was not remapped through NFC: {found:?}"
    );
}

#[test]
fn a_range_reaching_the_end_survives_a_large_nfc_shrink() {
    // The end of a caller range is clipped before it is mapped, and the bound
    // has to be the original length rather than the normalized one. Seven
    // decomposed characters shrink the text by more than the term after them is
    // long, so a bound taken from the normalized text would push the end below
    // the range's own start and drop the range entirely.
    //
    // The earlier NFC test does not catch this: its range ends on a character
    // nothing flags, so truncating it still overlaps the term it protects.
    let text = format!("{} 软件", "e\u{301}".repeat(7));
    let found = plain_hits(&text, &[from_term(&text)]);
    assert!(
        found.is_empty(),
        "caller range was clipped against the normalized length: {found:?}"
    );
}

#[test]
fn a_range_past_the_end_of_the_text_silences_nothing() {
    let text = "用 软件, 對";
    let past_end = ByteRange {
        start: text.len() + 10,
        end: text.len() + 20,
    };
    assert_eq!(
        plain_hits(text, &[past_end]),
        vec!["软件".to_owned(), ",".to_owned()]
    );
}

#[test]
fn no_caller_ranges_scans_everything() {
    let text = "用 软件, 對";
    assert_eq!(
        plain_hits(text, &[]),
        vec!["软件".to_owned(), ",".to_owned()]
    );
}
