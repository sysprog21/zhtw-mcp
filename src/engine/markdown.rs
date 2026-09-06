// Markdown-aware text extraction for zhtw-mcp linting.
//
// Uses pulldown-cmark to parse Markdown and identify regions that should be
// excluded from linting: code blocks, inline code, HTML blocks, and YAML
// frontmatter.
//
// Returns byte ranges to exclude before scanning.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use super::excluded::{merge_ranges_pub, ByteRange};
use super::html_lang::LangScopes;

/// Build excluded byte ranges from Markdown structure.
///
/// Excludes: fenced/indented code blocks, inline code, HTML blocks/tags,
/// and YAML frontmatter (leading --- fences).
///
/// The returned ranges are sorted by start position and non-overlapping.
/// Options controlling Markdown structural exclusion.  Defaults match
/// the historical behavior of [build_markdown_excluded_ranges]: code
/// blocks excluded, blockquotes scanned.
#[derive(Debug, Clone, Copy, Default)]
pub struct MdScanOptions {
    /// When true, fenced/indented code blocks are NOT excluded (i.e. the
    /// scanner sees code-block prose).  Used by the
    /// `MarkdownScanCode` content type.
    pub scan_code_blocks: bool,
    /// When true, the byte ranges of pulldown-cmark `Tag::BlockQuote`
    /// events are excluded.  Implemented via cmark events so that nested
    /// blockquotes (`> >`), lazy continuation lines, and blockquotes
    /// inside list items behave correctly.  Off by default: adopted
    /// blockquote prose is real content.
    pub exempt_blockquotes: bool,
}

impl MdScanOptions {
    /// Construct options for a Markdown scan: pass `scan_code_blocks=true`
    /// when running with the `MarkdownScanCode` content type, and propagate
    /// the caller's `--exempt-blockquotes` flag.  Centralizes the literal
    /// previously copy-pasted across the CLI and MCP entry points.
    pub fn new(scan_code_blocks: bool, exempt_blockquotes: bool) -> Self {
        Self {
            scan_code_blocks,
            exempt_blockquotes,
        }
    }
}

pub fn build_markdown_excluded_ranges(text: &str) -> Vec<ByteRange> {
    build_markdown_excluded_ranges_with_options(text, MdScanOptions::default())
}

/// Return line starts for Markdown blocks that end the section before them.
///
/// Only headings and thematic breaks qualify. Those are the constructs that
/// actually close a section; a list, blockquote, or code fence sits *inside*
/// one, and the sentence before it is the lead-in that introduces it, which is
/// the standard shape of technical zh-TW. Treating those as boundaries turned
/// every "…，如下所示。" before a fence into a formulaic closer.
///
/// The value of asking the parser rather than matching a line prefix is that
/// it knows a heading-shaped line inside a fence is not a heading, and that a
/// setext underline makes the line above it one. Parser event ranges can start
/// after indentation, so normalize each range to its physical source-line
/// start before handing it to the line-oriented scanner.
pub(crate) fn block_boundary_starts(text: &str) -> Vec<usize> {
    // YAML frontmatter is metadata, not a section. The parser has no notion of
    // it: the opening fence reads as a thematic break and the closing one turns
    // the last key line into a setext heading, so both would land here as
    // boundaries. Nothing can precede the first of them, which makes this
    // unreachable for the closer detector today, but the list is supposed to
    // hold blocks that end a section and neither of these does.
    let frontmatter_end = detect_frontmatter(text).unwrap_or(0);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;

    // Depth tracks container nesting. A heading or a break inside a quote or a
    // list item belongs to that container, not to the document: "> # 標題" does
    // not end the section the quote sits in, and counting it flagged the
    // sentence above the quote as a formulaic closer.
    let mut depth = 0usize;
    let mut starts = Vec::new();
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match event {
            // Item is not counted: it only ever occurs inside a List, which has
            // already made the depth non-zero.
            Event::Start(Tag::BlockQuote(_) | Tag::List(_)) => depth += 1,
            Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_)) => {
                depth = depth.saturating_sub(1);
            }
            Event::Rule | Event::Start(Tag::Heading { .. })
                if depth == 0 && range.start >= frontmatter_end =>
            {
                starts.push(text[..range.start].rfind(['\n', '\r']).map_or(0, |i| i + 1));
            }
            _ => {}
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Like [build_markdown_excluded_ranges], but fenced/indented code blocks are
/// NOT excluded.  Only inline code (`backtick`), HTML, and YAML frontmatter
/// are excluded.  This allows linting Chinese prose inside code blocks
/// (comments, translated output, etc.) while still protecting inline code
/// and HTML from false positives.
pub fn build_markdown_excluded_ranges_no_code(text: &str) -> Vec<ByteRange> {
    build_markdown_excluded_ranges_with_options(
        text,
        MdScanOptions {
            scan_code_blocks: true,
            ..MdScanOptions::default()
        },
    )
}

/// Build Markdown exclusion ranges with explicit options.  Backbone for the
/// two named wrappers above, plus opt-in features like `exempt_blockquotes`.
pub fn build_markdown_excluded_ranges_with_options(
    text: &str,
    opts: MdScanOptions,
) -> Vec<ByteRange> {
    let mut ranges = Vec::new();

    // Pre-pass: detect YAML frontmatter (leading --- fence). Exclude only the
    // structural tokens (--- fences, key+colon spans, ASCII quote delimiters),
    // leaving value prose scannable so that linting catches issues in
    // title/description.
    if let Some(fm_end) = detect_frontmatter(text) {
        collect_frontmatter_structural_ranges(text, fm_end, &mut ranges);
    }

    collect_container_fence_ranges(text, &mut ranges);

    let parser_opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(text, parser_opts);
    let mut in_code_block = false;
    let mut code_block_start = 0usize;
    let mut blockquote_depth: usize = 0;
    let mut blockquote_start: usize = 0;
    let mut lang_scopes = LangScopes::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            // Fenced or indented code blocks: exclude entire block (default) or
            // scan their prose (when scan_code_blocks is set).
            Event::Start(Tag::CodeBlock(_)) if !opts.scan_code_blocks => {
                in_code_block = true;
                code_block_start = range.start;
            }
            Event::End(TagEnd::CodeBlock) if in_code_block => {
                ranges.push(ByteRange {
                    start: code_block_start,
                    end: range.end,
                });
                in_code_block = false;
            }

            Event::Start(Tag::BlockQuote(_)) if opts.exempt_blockquotes => {
                if blockquote_depth == 0 {
                    blockquote_start = range.start;
                }
                blockquote_depth = blockquote_depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) if opts.exempt_blockquotes => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
                if blockquote_depth == 0 {
                    ranges.push(ByteRange {
                        start: blockquote_start,
                        end: range.end,
                    });
                }
            }

            // Inline code: exclude the span including backticks.
            Event::Code(_) => {
                ranges.push(ByteRange {
                    start: range.start,
                    end: range.end,
                });
            }

            // HTML: the tags themselves are never prose, and a tag carrying a
            // non-Chinese lang also takes the prose it wraps out of the scan.
            // The event text is read from the source rather than from the
            // event's own string so the offsets fed to the tracker are the
            // document's.
            Event::Html(_) | Event::InlineHtml(_) => {
                ranges.push(ByteRange {
                    start: range.start,
                    end: range.end,
                });
                lang_scopes.feed(&text[range.start..range.end], range.start);
            }

            _ => {}
        }
    }

    ranges.extend(lang_scopes.finish(text.len()));

    // Sort and merge (frontmatter + parser ranges may overlap).
    merge_ranges_pub(ranges)
}

/// Build excluded byte ranges for YAML structural tokens.
///
/// Excludes YAML key tokens (the key name + colon) so that bare ASCII colons
/// in key-value separators do not trigger false-positive colon warnings.
/// Only the key portion is excluded; values after the colon are prose and
/// remain scannable.
///
/// Pattern matched: /^\s*\w[\w-]*\s*:/ on each line.
/// Examples excluded: title:, key-name:, summary  :.
/// Not excluded: list items (- value), comments (# text), values.
pub fn build_yaml_excluded_ranges(text: &str) -> Vec<ByteRange> {
    let mut ranges = Vec::new();
    let mut pos = 0usize;

    for raw_line in text.split('\n') {
        let line_len = raw_line.len();

        if let Some(colon_pos) = yaml_key_colon_pos(raw_line) {
            // Exclude from the start of the line through the ':' (inclusive).
            ranges.push(ByteRange {
                start: pos,
                end: pos + colon_pos + 1,
            });
        }

        pos += line_len + 1; // +1 for the '\n'
    }

    ranges
}

/// Find the byte offset of the YAML key-separator colon in a line, if present.
///
/// Per the YAML spec, a block-mapping key separator is a : followed by a
/// space, tab, or end-of-line.  This handles all common block-mapping forms:
///
/// - key: value          , simple key
/// - key-name: value     , hyphenated key
/// -   indented: value   , indented key
/// - - key: value        , key inside a list item
/// - "quoted-key": value , quoted key (quote skipped; no escape handling)
///
/// Known limitations (acceptable for prose documentation YAML):
/// - Flow mappings without whitespace after : (e.g. {key:"val"}) are not
///   detected; those are rare in documentation YAML and equivalent to JSON.
/// - Only the first key-colon per line is excluded; additional key-colons in
///   flow sequences ({a: 1, b: 2}) on the same line are not excluded.
/// - Escaped quotes inside quoted keys (e.g. "key\"name": v) may confuse
///   the quote-tracking state.
///
/// Returns the byte offset of the : within the line, or None.
/// Colons inside single- or double-quoted strings are skipped.
fn yaml_key_colon_pos(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_quote = false;
    let mut quote_char = b'\0';

    while i < len {
        let b = bytes[i];
        if in_quote {
            if b == quote_char {
                in_quote = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = true;
            quote_char = b;
        } else if b == b':' {
            // YAML key separator: colon must be followed by whitespace or be at
            // EOL.
            let next = bytes.get(i + 1).copied().unwrap_or(b' ');
            if next == b' ' || next == b'\t' || next == b'\r' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// A Markdown table cell span with `(row, column)` coordinates.
#[derive(Debug, Clone, Copy)]
pub struct TableCellSpan {
    pub start: usize,
    pub end: usize,
    pub row: usize,
    pub col: usize,
}

/// Extract byte ranges of all Markdown table cells with their (row, column).
///
/// Row 0 is the header row (or row 1 if no separator).  Column index is the
/// 0-based position within the row.  Returns empty if the document has no
/// tables.
pub fn extract_table_cell_spans(text: &str) -> Vec<TableCellSpan> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(text, opts);
    let mut spans = Vec::new();
    let mut in_table = false;
    let mut current_row = 0usize;
    let mut current_col = 0usize;
    let mut cell_active = false;
    let mut cell_start = 0usize;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                current_row = 0;
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) if in_table => {
                current_col = 0;
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) if in_table => {
                current_row += 1;
            }
            Event::Start(Tag::TableCell) if in_table => {
                cell_active = true;
                cell_start = range.start;
            }
            Event::End(TagEnd::TableCell) if in_table && cell_active => {
                spans.push(TableCellSpan {
                    start: cell_start,
                    end: range.end,
                    row: current_row,
                    col: current_col,
                });
                current_col += 1;
                cell_active = false;
            }
            _ => {}
        }
    }
    spans
}

/// Extract byte ranges of heading text inline content in Markdown.
///
/// Returns ranges covering only the inline text inside heading containers
/// (H1-H6), excluding the leading `#` markers and trailing whitespace.
/// Used by the scan pipeline to apply severity boosting on heading text.
///
/// Issues outside the inline text (e.g. inside ATX `#` markers) are not
/// boosted, since they are unlikely to represent real heading prose hits.
///
/// YAML frontmatter is excluded: pulldown-cmark would otherwise interpret
/// the closing `---` as a setext H2 underline, falsely treating frontmatter
/// values as heading text.
pub fn extract_heading_ranges(text: &str) -> Vec<ByteRange> {
    let frontmatter_end = detect_frontmatter(text).unwrap_or(0);

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(text, opts);
    let mut ranges = Vec::new();
    let mut in_heading = false;
    let mut current_inline_start: Option<usize> = None;
    let mut current_inline_end: Option<usize> = None;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                in_heading = true;
                current_inline_start = None;
                current_inline_end = None;
            }
            Event::End(TagEnd::Heading(_)) if in_heading => {
                if let (Some(start), Some(end)) = (current_inline_start, current_inline_end) {
                    // Skip false-positive headings synthesised from frontmatter
                    // content + closing "---".
                    if start >= frontmatter_end {
                        ranges.push(ByteRange { start, end });
                    }
                }
                in_heading = false;
            }
            Event::Text(_) | Event::Code(_) | Event::Html(_) | Event::InlineHtml(_)
                if in_heading =>
            {
                if current_inline_start.is_none() {
                    current_inline_start = Some(range.start);
                }
                current_inline_end = Some(range.end);
            }
            _ => {}
        }
    }
    ranges
}

/// Collect YAML frontmatter structural ranges: opening `---` line, closing
/// `---` line, per-line key+colon spans, and bare ASCII `"` / `'` quote
/// bytes used as scalar delimiters.  Values remain scannable; only the
/// 1-byte delimiters are masked from the punctuation scanner so that
/// downstream YAML parsers continue to see ASCII quotes.
fn collect_frontmatter_structural_ranges(text: &str, fm_end: usize, ranges: &mut Vec<ByteRange>) {
    let fm = &text[..fm_end];
    let mut pos = 0usize;

    for raw_line in fm.split('\n') {
        let line_len = raw_line.len();
        let trimmed = raw_line.trim_end_matches('\r');

        if trimmed == "---" {
            // Exclude the entire fence line (and the trailing \n if present).
            let end = (pos + line_len + 1).min(fm_end);
            ranges.push(ByteRange { start: pos, end });
        } else if let Some(colon_pos) = yaml_key_colon_pos(raw_line) {
            // Exclude the key+colon prefix; value text remains scannable.
            ranges.push(ByteRange {
                start: pos,
                end: pos + colon_pos + 1,
            });
        }

        // Preserve ASCII '"' and "'" bytes used as YAML scalar delimiters by
        // excluding them from the punctuation scanner. Without this, the
        // scanner converts '"' to "「"/"」" inside frontmatter values and
        // breaks downstream YAML parsers (regression observed in ai-muninn.com
        // calque blindspot sweep, 2026-05).
        for (i, b) in raw_line.bytes().enumerate() {
            if b == b'"' || b == b'\'' {
                ranges.push(ByteRange {
                    start: pos + i,
                    end: pos + i + 1,
                });
            }
        }

        pos += line_len + 1; // +1 for the '\n'
        if pos >= fm_end {
            break;
        }
    }
}

/// Collect container-block fence lines (:::keyword / :::) as excluded ranges.
/// Used by HackMD and Docusaurus for admonitions.  Only the fence lines
/// themselves are excluded; the prose content between them is still scanned.
fn collect_container_fence_ranges(text: &str, ranges: &mut Vec<ByteRange>) {
    let mut pos = 0usize;
    for raw_line in text.split('\n') {
        let line_len = raw_line.len();
        let trimmed = raw_line.trim_start_matches([' ', '\t']);
        if trimmed.starts_with(":::") {
            ranges.push(ByteRange {
                start: pos,
                end: pos + line_len,
            });
        }
        pos += line_len + 1; // +1 for the '\n'
    }
}

/// Detect YAML frontmatter delimited by --- at the start of the document.
/// Returns the byte offset just past the closing ---\n (or end of closing ---).
fn detect_frontmatter(text: &str) -> Option<usize> {
    // Must start at the very beginning with --- followed by a newline.
    if !text.starts_with("---") {
        return None;
    }

    let after_open = if text.starts_with("---\n") {
        4
    } else if text.starts_with("---\r\n") {
        5
    } else {
        return None;
    };

    // Find the closing --- on its own line.
    let rest = &text[after_open..];
    for (line_start, line) in rest.split('\n').scan(0usize, |pos, line| {
        let start = *pos;
        *pos += line.len() + 1; // +1 for the \n
        Some((start, line))
    }) {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" {
            // End position is after the closing ---\n.
            let abs_end = after_open + line_start + line.len() + 1;
            return Some(abs_end.min(text.len()));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_boundaries_are_normalized_to_source_line_starts() {
        let md = "前言。\n\n   # 標題\n\n  - 項目\n\n > 引文\n\n```sh\necho ok\n```\n\n---\n";
        let starts = block_boundary_starts(md);
        for line in ["   # 標題", "---"] {
            let start = md.find(line).unwrap();
            assert!(
                starts.binary_search(&start).is_ok(),
                "missing {line:?}: {starts:?}"
            );
        }

        // A list, blockquote, or fence opens a block inside the section, not a
        // new section. The sentence before it is a lead-in, not a closer.
        for line in ["  - 項目", " > 引文", "```sh"] {
            let start = md.find(line).unwrap();
            assert!(
                starts.binary_search(&start).is_err(),
                "{line:?} must not end a section: {starts:?}"
            );
        }
        assert!(starts.binary_search(&0).is_err());
    }

    #[test]
    fn nested_headings_and_rules_are_not_section_boundaries() {
        for md in [
            "本節總結。展望未來。\n\n> # 引用裡的標題\n",
            "本節總結。展望未來。\n\n> ---\n",
            "本節總結。展望未來。\n\n- 項目\n\n  # 清單裡的標題\n",
        ] {
            let starts = block_boundary_starts(md);
            assert!(
                starts.is_empty(),
                "nested construct opened a section: {md:?} -> {starts:?}"
            );
        }
    }

    #[test]
    fn frontmatter_is_not_a_section_boundary() {
        let md = "---\ntitle: t\n---\n\n本節總結。展望未來。\n\n# 下一節\n";
        let starts = block_boundary_starts(md);
        let heading = md.find("# 下一節").unwrap();
        assert_eq!(
            starts,
            vec![heading],
            "only the real heading closes a section: {starts:?}"
        );
    }

    #[test]
    fn a_heading_shaped_line_inside_a_fence_is_not_a_boundary() {
        // The reason this index comes from the parser rather than a line prefix
        // test.
        let md = "前言。\n\n```sh\n# install\nmake\n```\n";
        let starts = block_boundary_starts(md);
        assert!(
            starts.is_empty(),
            "a comment inside a fence opened a section: {starts:?}"
        );
    }

    #[test]
    fn fenced_code_block_excluded() {
        let md = "前言\n```rust\nlet x = 1;\n```\n後語\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(!ranges.is_empty());
        let excluded_text: String = ranges
            .iter()
            .map(|r| &md[r.start..r.end])
            .collect::<Vec<_>>()
            .join("");
        assert!(excluded_text.contains("let x = 1;"));
        assert!(!excluded_text.contains("前言"));
        assert!(!excluded_text.contains("後語"));
    }

    #[test]
    fn inline_code_excluded() {
        let md = "使用 `println!` 來輸出\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(!ranges.is_empty());
        let any_covers_println = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains("println"));
        assert!(any_covers_println);
    }

    #[test]
    fn yaml_frontmatter_keys_excluded_values_scannable() {
        let md = "---\ntitle: 測試\ndate: 2024-01-01\n---\n正文開始\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(!ranges.is_empty());
        // Key+colon is excluded.
        let any_covers_title_key = ranges.iter().any(|r| md[r.start..r.end].contains("title:"));
        assert!(any_covers_title_key, "title: key should be excluded");
        // Value text is NOT excluded: it's prose to be scanned.
        let value_excluded = ranges.iter().any(|r| md[r.start..r.end].contains("測試"));
        assert!(
            !value_excluded,
            "frontmatter value 測試 should be scannable"
        );
        // Body is not excluded.
        let body_excluded = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains("正文開始"));
        assert!(!body_excluded);
        // Closing --- fence is excluded.
        let close_excluded = ranges.iter().any(|r| md[r.start..r.end].trim() == "---");
        assert!(close_excluded, "closing --- fence should be excluded");
    }

    #[test]
    fn extract_heading_ranges_skips_frontmatter_setext() {
        // pulldown-cmark synthesises a setext H2 from frontmatter content +
        // closing "---". We must skip that false-positive heading so the
        // severity boost does not apply to frontmatter values.
        let md = "---\ntitle: 軟件測試指南\ndate: 2026\n---\n# 真標題\n正文。\n";
        let ranges = extract_heading_ranges(md);

        // Should contain the real heading "真標題" but NOT the synthesised
        // frontmatter heading.
        for r in &ranges {
            let span = &md[r.start..r.end];
            assert!(
                !span.contains("title:"),
                "frontmatter content leaked into heading ranges: {span:?}"
            );
        }
        let real_heading_present = ranges.iter().any(|r| md[r.start..r.end].contains("真標題"));
        assert!(
            real_heading_present,
            "real heading '真標題' should still be detected: {ranges:?}"
        );
    }

    #[test]
    fn yaml_frontmatter_not_in_middle() {
        let md = "前言\n---\ntitle: 測試\n---\n後語\n";
        let ranges = build_markdown_excluded_ranges(md);
        // --- in the middle is not frontmatter (it's a thematic break).
        let any_covers_title = ranges.iter().any(|r| md[r.start..r.end].contains("title:"));
        assert!(!any_covers_title);
    }

    #[test]
    fn html_block_excluded() {
        let md = "前言\n<div>some html</div>\n後語\n";
        let ranges = build_markdown_excluded_ranges(md);
        let any_covers_html = ranges.iter().any(|r| md[r.start..r.end].contains("<div>"));
        assert!(any_covers_html);
    }

    #[test]
    fn nested_list_with_code() {
        let md = "- 項目一\n  - `code` 子項目\n- 項目二\n";
        let ranges = build_markdown_excluded_ranges(md);
        let any_covers_code = ranges.iter().any(|r| md[r.start..r.end].contains("code"));
        assert!(any_covers_code);
        // List text should not be excluded.
        let any_covers_item = ranges.iter().any(|r| md[r.start..r.end].contains("項目一"));
        assert!(!any_covers_item);
    }

    #[test]
    fn blockquote_with_code() {
        let md = "> 引用文字 `inline` 繼續\n";
        let ranges = build_markdown_excluded_ranges(md);
        let any_covers_inline = ranges.iter().any(|r| md[r.start..r.end].contains("inline"));
        assert!(any_covers_inline);
        let any_covers_quote = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains("引用文字"));
        assert!(!any_covers_quote);
    }

    #[test]
    fn empty_input() {
        let ranges = build_markdown_excluded_ranges("");
        assert!(ranges.is_empty());
    }

    #[test]
    fn plain_text_no_exclusions() {
        let md = "這是純文字，沒有任何 Markdown 語法。\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(ranges.is_empty());
    }

    #[test]
    fn code_block_with_language_tag() {
        let md = "```python\nprint('hello')\n```\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(!ranges.is_empty());
        let excluded: String = ranges
            .iter()
            .map(|r| &md[r.start..r.end])
            .collect::<Vec<_>>()
            .join("");
        assert!(excluded.contains("print('hello')"));
    }

    #[test]
    fn multiple_code_blocks() {
        let md = "文字\n```\nblock1\n```\n中間\n```\nblock2\n```\n結尾\n";
        let ranges = build_markdown_excluded_ranges(md);
        assert!(ranges.len() >= 2);
    }

    #[test]
    fn container_fence_lines_excluded() {
        // The :::warning and ::: fence lines must be excluded. The prose
        // content between them must NOT be excluded.
        let md = "前言\n:::warning\n這是警告內容，請注意：細節。\n:::\n後語\n";
        let ranges = build_markdown_excluded_ranges(md);
        let any_covers_open_fence = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains(":::warning"));
        assert!(
            any_covers_open_fence,
            "opening fence line should be excluded"
        );
        let any_covers_close_fence = ranges.iter().any(|r| {
            let s = &md[r.start..r.end];
            s.trim() == ":::"
        });
        assert!(
            any_covers_close_fence,
            "closing fence line should be excluded"
        );
        // Prose content between fences must remain scannable.
        let prose_excluded = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains("警告內容"));
        assert!(
            !prose_excluded,
            "prose content between fences must not be excluded"
        );
    }

    #[test]
    fn container_fence_four_colons() {
        // :::: (4-colon) fences must also be excluded.
        let md = "文字\n::::note\n注意事項\n::::\n後語\n";
        let ranges = build_markdown_excluded_ranges(md);
        let any_covers_open = ranges
            .iter()
            .any(|r| md[r.start..r.end].contains("::::note"));
        assert!(any_covers_open, "4-colon opening fence should be excluded");
    }

    // Tests for build_yaml_excluded_ranges

    #[test]
    fn yaml_key_colon_excluded() {
        let yaml = "title: 繁體中文文件\nsummary: 說明文字\n";
        let ranges = build_yaml_excluded_ranges(yaml);
        // "title:" should be excluded.
        let any_covers_title_colon = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("title:"));
        assert!(any_covers_title_colon, "YAML key colon must be excluded");
        // The value "繁體中文文件" must NOT be excluded.
        let value_excluded = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("繁體中文文件"));
        assert!(!value_excluded, "YAML value must remain scannable");
    }

    #[test]
    fn yaml_key_with_spaces_before_colon() {
        let yaml = "title  : 文字\n";
        let ranges = build_yaml_excluded_ranges(yaml);
        let covers_key = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("title  :"));
        assert!(covers_key, "key with spaces before colon must be excluded");
    }

    #[test]
    fn yaml_pure_list_items_not_excluded() {
        // Pure list items with no key-value colon are not excluded.
        let yaml = "- 項目一\n- 項目二\n";
        let ranges = build_yaml_excluded_ranges(yaml);
        assert!(
            ranges.is_empty(),
            "pure list items (no colon) must not be excluded"
        );
    }

    #[test]
    fn yaml_list_mapping_key_excluded() {
        // - key: value, the key colon inside a list item must be excluded.
        let yaml = "- name: 測試\n- label: 標籤\n";
        let ranges = build_yaml_excluded_ranges(yaml);

        // Each list mapping line should have one excluded range covering "-
        // name:" / "- label:".
        let covers_name = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("name:"));
        assert!(covers_name, "key colon inside list item must be excluded");
        // Values must remain scannable.
        let value_excluded = ranges.iter().any(|r| yaml[r.start..r.end].contains("測試"));
        assert!(!value_excluded, "list mapping value must remain scannable");
    }

    #[test]
    fn yaml_hyphenated_key_excluded() {
        let yaml = "key-name: 值\n";
        let ranges = build_yaml_excluded_ranges(yaml);
        let covers = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("key-name:"));
        assert!(covers, "hyphenated key must be excluded");
    }

    #[test]
    fn yaml_indented_key_excluded() {
        let yaml = "outer:\n  inner: 值\n";
        let ranges = build_yaml_excluded_ranges(yaml);
        // "outer:" at col 0 and "  inner:" (indented) both excluded.
        let covers_outer = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("outer:"));
        let covers_inner = ranges
            .iter()
            .any(|r| yaml[r.start..r.end].contains("inner:"));
        assert!(covers_outer, "top-level key must be excluded");
        assert!(covers_inner, "indented key must be excluded");
    }

    // lang-scoped exclusion. The tracker's own rules are covered in
    // html_lang.rs; what these check is this file's seam, that a pulldown-cmark
    // HTML event reaches it with the document offsets it needs.

    /// Whether one excluded range covers the given substring.
    fn excluded_covers(md: &str, needle: &str) -> bool {
        let ranges = build_markdown_excluded_ranges(md);
        let start = md.find(needle).expect("needle present in fixture");
        let end = start + needle.len();
        ranges.iter().any(|r| r.start <= start && r.end >= end)
    }

    #[test]
    fn block_level_lang_en_excludes_the_prose_between_the_tags() {
        let md = "<div lang=\"en\">\n\nDo one thing, and do it well, 對吧。\n\n</div>\n";
        assert!(excluded_covers(md, "Do one thing, and do it well, 對吧。"));
    }

    #[test]
    fn inline_lang_en_span_excludes_its_run() {
        let md = "他說<span lang=\"en\">I agree, 但</span>結束\n";
        assert!(excluded_covers(md, "I agree, 但"));
        assert!(!excluded_covers(md, "結束"));
    }

    #[test]
    fn a_zh_tw_span_is_still_scanned() {
        let md = "他說<span lang=\"zh-TW\">好, 對</span>結束\n";
        assert!(!excluded_covers(md, "好, 對"));
    }

    #[test]
    fn a_lang_tag_written_inside_a_fence_scopes_nothing() {
        let md = "```html\n<span lang=\"en\">\n```\n\n他說, 對吧。\n";
        assert!(!excluded_covers(md, "他說, 對吧。"));
    }
}
