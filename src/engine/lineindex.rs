// Line/column position mapping for byte offsets.
//
// Pre-computes newline positions to efficiently convert byte offsets to (line,
// col) coordinates. Column values use UTF-16 code units by default (matching
// LSP spec), with optional UTF-32 (char index) mode.

/// Column encoding mode for position reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnEncoding {
    /// UTF-16 code units (LSP default). Surrogate pairs count as 2.
    Utf16,
    /// Unicode scalar values (char count). Each char counts as 1.
    Utf32,
}

/// Pre-computed newline index for fast byte-offset to (line, col) conversion.
pub struct LineIndex<'a> {
    /// Byte offsets of each line start. line_starts[0] is always 0.
    line_starts: Vec<usize>,
    /// The source text (borrowed for column computation).
    text: &'a str,
}

impl<'a> LineIndex<'a> {
    /// Build a line index from source text.
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts, text }
    }

    /// Build a `LineIndex` from pre-computed line starts (used by the
    /// merged detect+lineindex single-pass builder).
    pub fn from_parts(text: &'a str, line_starts: Vec<usize>) -> Self {
        Self { line_starts, text }
    }

    /// Fill `line` and `col` fields on a batch of issues whose offsets are
    /// already sorted ascending.  Single linear pass over the line-start
    /// table, which avoids an O(log n) binary search per issue.
    pub fn fill_line_col_sorted(
        &self,
        issues: &mut [crate::rules::ruleset::Issue],
        encoding: ColumnEncoding,
    ) {
        let mut line_idx = 0;

        // Incremental column cursor: (byte_offset, col_count) from the last
        // issue on the same line. When the next issue is on the same line and
        // at a later offset, we resume counting from the cursor instead of
        // re-scanning from line start.
        let mut cursor_byte: usize = 0;
        let mut cursor_col: usize = 0;

        for issue in issues.iter_mut() {
            // Advance line_idx forward.
            while line_idx + 1 < self.line_starts.len()
                && self.line_starts[line_idx + 1] <= issue.offset
            {
                line_idx += 1;
            }
            let line_byte_start = self.line_starts[line_idx];
            let offset = issue.offset.min(self.text.len());

            // If cursor is on the same line and at or before this offset, count
            // incrementally from cursor. Otherwise reset from line start.
            let (scan_from, base_col) = if cursor_byte >= line_byte_start && cursor_byte <= offset {
                (cursor_byte, cursor_col)
            } else {
                (line_byte_start, 0)
            };

            let delta_slice = &self.text[scan_from..offset];
            let delta_col = match encoding {
                ColumnEncoding::Utf16 => delta_slice.encode_utf16().count(),
                ColumnEncoding::Utf32 => delta_slice.chars().count(),
            };
            let col = base_col + delta_col;

            issue.line = line_idx + 1;
            issue.col = col + 1;

            // Update cursor for next issue.
            cursor_byte = offset;
            cursor_col = col;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ruleset::{Issue, IssueType, Severity};

    /// Positions for a batch of ascending offsets, which is the only shape
    /// this index is ever asked for: the scanner hands it a document's issues
    /// at once. Written against the batch call rather than a per-offset one
    /// so the column arithmetic is tested where it actually runs, including
    /// the cursor that carries a partial count between issues on one line.
    fn positions(text: &str, offsets: &[usize], encoding: ColumnEncoding) -> Vec<(usize, usize)> {
        let mut issues: Vec<Issue> = offsets
            .iter()
            .map(|&offset| {
                Issue::new(
                    offset,
                    0,
                    "",
                    Vec::new(),
                    IssueType::Grammar,
                    Severity::Warning,
                )
            })
            .collect();
        LineIndex::new(text).fill_line_col_sorted(&mut issues, encoding);
        issues.iter().map(|issue| (issue.line, issue.col)).collect()
    }

    #[test]
    fn single_line_ascii() {
        assert_eq!(
            positions("hello world", &[0, 5, 11], ColumnEncoding::Utf16),
            [(1, 1), (1, 6), (1, 12)]
        );
    }

    #[test]
    fn multi_line_ascii() {
        // 'a', the newline ending line 1, 'd', and 'g'.
        assert_eq!(
            positions("abc\ndef\nghi", &[0, 3, 4, 8], ColumnEncoding::Utf16),
            [(1, 1), (1, 4), (2, 1), (3, 1)]
        );
    }

    #[test]
    fn cjk_columns_utf16() {
        // CJK chars are in the BMP: one UTF-16 code unit each, three UTF-8
        // bytes each, so the columns advance by one where the offsets advance
        // by three.
        assert_eq!(
            positions("你好世界", &[0, 3, 6, 9], ColumnEncoding::Utf16),
            [(1, 1), (1, 2), (1, 3), (1, 4)]
        );
    }

    #[test]
    fn emoji_utf16_surrogate_pair() {
        // U+1F600 is outside the BMP: four UTF-8 bytes, two UTF-16 code units,
        // so the char after it sits at column 4 rather than 3.
        assert_eq!(
            positions("a😀b", &[0, 1, 5], ColumnEncoding::Utf16),
            [(1, 1), (1, 2), (1, 4)]
        );
    }

    #[test]
    fn emoji_utf32() {
        // The same offsets counted in scalar values, where the emoji is one.
        assert_eq!(
            positions("a😀b", &[0, 1, 5], ColumnEncoding::Utf32),
            [(1, 1), (1, 2), (1, 3)]
        );
    }

    #[test]
    fn mixed_ascii_cjk_multiline() {
        // Line 1 is Hello 你好 with 你 at byte 6 and 好 at byte 9; line 2
        // starts at byte 13, after the newline at 12.
        assert_eq!(
            positions("Hello 你好\nWorld 世界", &[6, 9, 13], ColumnEncoding::Utf16),
            [(1, 7), (1, 8), (2, 1)]
        );
    }

    #[test]
    fn offset_at_end() {
        assert_eq!(positions("abc", &[3], ColumnEncoding::Utf16), [(1, 4)]);
    }

    #[test]
    fn empty_text() {
        assert_eq!(positions("", &[0], ColumnEncoding::Utf16), [(1, 1)]);
    }

    #[test]
    fn a_line_start_resets_the_running_column() {
        // The cursor carries a partial column count from one issue to the next,
        // and only while both sit on the same line. A batch that crosses a line
        // boundary and then reports again on the new line is what catches a
        // reset that did not happen.
        assert_eq!(
            positions("你好\n世界", &[0, 3, 7, 10], ColumnEncoding::Utf16),
            [(1, 1), (1, 2), (2, 1), (2, 2)]
        );
    }
}
