// Sentence and paragraph boundary index.
//
// Provides reusable boundary detection for grammar checks, structural AI
// pattern detection, and translationese scoring. Computed once per scan, shared
// across consumers.
//
// Chinese sentence boundaries: 。？！；and blank-line paragraph breaks. Mixed
// CJK/Latin: also split on .?! followed by whitespace + uppercase. Abbreviation
// deny-list prevents false splits (Mr., P.S., etc.).

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::scan::is_cjk_ideograph;

/// A sentence span identified by byte offsets into the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentenceBound {
    pub byte_start: usize,
    pub byte_end: usize,
}

/// A paragraph span identified by byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParagraphBound {
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Pre-computed sentence and paragraph boundary index for a document.
#[derive(Debug)]
pub struct BoundaryIndex {
    pub sentences: Vec<SentenceBound>,
    pub paragraphs: Vec<ParagraphBound>,
}

// CJK terminal punctuation that always ends a sentence.
const CJK_TERMINATORS: &[char] = &['。', '！', '？'];

// Semicolons act as sentence boundaries in Chinese text.
const CJK_SOFT_TERMINATORS: &[char] = &['；'];

// Latin terminal punctuation, which only triggers a split when followed by
// whitespace + uppercase letter (to avoid splitting on abbreviations).
const LATIN_TERMINATORS: &[char] = &['.', '?', '!'];

// Abbreviation patterns that end with '.' but are NOT sentence boundaries.
// Checked by looking at the text preceding a Latin period.
const ABBREVIATION_SUFFIXES: &[&str] = &[
    "Mr", "Mrs", "Ms", "Dr", "Prof", "Jr", "Sr", "vs", "etc", "i.e", "e.g", "P.S", "p.s", "cf",
    "al", "Vol", "No", "Fig", "Eq", "Rev",
];

/// Whether the character "ch" at byte "start" ends a sentence. CJK
/// terminators and the semicolon always do; a Latin one only when followed by
/// whitespace and a capital or CJK, and not after a listed abbreviation.
pub(crate) fn is_sentence_terminator(text: &str, start: usize, ch: char) -> bool {
    CJK_TERMINATORS.contains(&ch)
        || CJK_SOFT_TERMINATORS.contains(&ch)
        || LATIN_TERMINATORS.contains(&ch)
            && is_latin_sentence_end(text, start, start + ch.len_utf8())
}

impl BoundaryIndex {
    /// Build a boundary index for the given text, respecting exclusion zones.
    ///
    /// Exclusion zones (code blocks, URLs, etc.) are treated as opaque:
    /// boundaries inside them are ignored, and entering/leaving an exclusion
    /// zone acts as a sentence break.
    pub fn build(text: &str, excluded: &[ByteRange]) -> Self {
        crate::engine::index_guard::note_build(crate::engine::index_guard::DocIndex::Boundary);
        let sentences = build_sentences(text, excluded);
        let paragraphs = build_paragraphs(text);
        BoundaryIndex {
            sentences,
            paragraphs,
        }
    }

    /// Find the sentence containing byte offset `pos`.
    /// Returns None if pos is outside all sentences (e.g. inside an exclusion
    /// zone).
    pub fn sentence_at(&self, pos: usize) -> Option<&SentenceBound> {
        self.sentences
            .iter()
            .find(|s| s.byte_start <= pos && pos < s.byte_end)
    }

    /// Find the paragraph containing byte offset `pos`.
    pub fn paragraph_at(&self, pos: usize) -> Option<&ParagraphBound> {
        self.paragraphs
            .iter()
            .find(|p| p.byte_start <= pos && pos < p.byte_end)
    }

    /// Return all sentences within a given paragraph.
    ///
    /// Both lists are built in offset order, so the sentences of a paragraph
    /// are a contiguous run and two binary searches find it. The filter this
    /// replaced walked every sentence and allocated a Vec per paragraph, which
    /// is quadratic in the document: on a 200 KB file with 970 paragraphs and
    /// 3,395 sentences it cost 2.2 ms, most of the closing-phrase pass.
    pub fn sentence_slice(&self, para: &ParagraphBound) -> &[SentenceBound] {
        let start = self
            .sentences
            .partition_point(|s| s.byte_start < para.byte_start);
        let end = self
            .sentences
            .partition_point(|s| s.byte_end <= para.byte_end);
        self.sentences.get(start..end).unwrap_or(&[])
    }

    /// Extract the text slice for a sentence bound.
    pub fn sentence_text<'a>(&self, text: &'a str, s: &SentenceBound) -> &'a str {
        &text[s.byte_start..s.byte_end]
    }

    /// Extract the text slice for a paragraph bound.
    pub fn paragraph_text<'a>(&self, text: &'a str, p: &ParagraphBound) -> &'a str {
        &text[p.byte_start..p.byte_end]
    }
}

/// Build sentence boundaries from text.
fn build_sentences(text: &str, excluded: &[ByteRange]) -> Vec<SentenceBound> {
    let mut sentences = Vec::new();
    let mut sent_start: usize = 0;
    let mut in_excluded = false;
    let mut last_was_content = false;

    let bytes = text.as_bytes();
    let mut byte_offset = 0;

    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        let ch_end = byte_offset + ch_len;

        // Handle exclusion zone transitions.
        let currently_excluded = is_excluded(byte_offset, ch_end, excluded);
        if currently_excluded && !in_excluded {
            // Entering exclusion: flush current sentence if non-empty.
            if last_was_content && byte_offset > sent_start {
                push_sentence(&mut sentences, text, sent_start, byte_offset);
            }
            in_excluded = true;
            last_was_content = false;
        } else if !currently_excluded && in_excluded {
            // Leaving exclusion: start new sentence.
            sent_start = byte_offset;
            in_excluded = false;
        }

        if currently_excluded {
            byte_offset = ch_end;
            continue;
        }

        // The same blank-line rule the paragraph splitters use: a newline, an
        // optional "\r", another newline. Recognising only the two pure forms
        // left a mixed "\n\r\n" document with paragraph bounds that no sentence
        // bound matched, so "sentence_slice" returned nothing for either
        // paragraph and every paragraph-scoped detector skipped the content.
        let paragraph_break_len = match ch {
            // Locate the "\n" of the first terminator, whichever form it takes.
            '\r' if bytes.get(ch_end) == Some(&b'\n') => blank_line_end(bytes, ch_end),
            '\n' => blank_line_end(bytes, byte_offset),
            _ => None,
        }
        .map(|end| end - byte_offset);

        // Paragraph break, in any combination of terminators.
        if let Some(break_len) = paragraph_break_len {
            if last_was_content && byte_offset > sent_start {
                push_sentence(&mut sentences, text, sent_start, byte_offset);
            }
            last_was_content = false;
            sent_start = byte_offset + break_len;
            byte_offset = ch_end;
            continue;
        }

        if is_sentence_terminator(text, byte_offset, ch) {
            push_sentence(&mut sentences, text, sent_start, ch_end);
            sent_start = ch_end;
            last_was_content = false;
            byte_offset = ch_end;
            continue;
        }

        if !ch.is_whitespace() {
            last_was_content = true;
        }

        byte_offset = ch_end;
    }

    // Flush trailing sentence.
    if sent_start < text.len() && last_was_content {
        push_sentence(&mut sentences, text, sent_start, text.len());
    }

    sentences
}

/// Check if a Latin period/question/exclamation at `dot_start..dot_end` is a
/// real sentence boundary (followed by whitespace + uppercase letter) and not
/// an abbreviation.
fn is_latin_sentence_end(text: &str, dot_start: usize, dot_end: usize) -> bool {
    let rest = &text[dot_end..];
    let mut chars = rest.chars().peekable();

    // Must be followed by at least one whitespace char; consume the full
    // whitespace run rather than the first, so a period followed by two spaces
    // and a capital still splits.
    match chars.peek() {
        Some(c) if c.is_whitespace() => {}
        _ => return false,
    }
    while let Some(c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }

    // Then an uppercase letter, or CJK text or a CJK opening mark such as 「
    // (U+3001..U+303F), which counts as a new sentence. Not is_cjk_context: it
    // would also admit fullwidth Latin and ！ here.
    match chars.next() {
        Some(c)
            if c.is_uppercase()
                || is_cjk_ideograph(c)
                || ('\u{3001}'..='\u{303F}').contains(&c) => {}
        _ => return false,
    }

    // Check abbreviation deny-list: look at text before the dot. Word boundary
    // = start of text, ASCII whitespace/punct, or CJK char (no ASCII letter
    // immediately before the abbreviation).
    let before = &text[..dot_start];
    for abbr in ABBREVIATION_SUFFIXES {
        if before.ends_with(abbr) {
            let prefix_start = before.len() - abbr.len();
            if prefix_start == 0 {
                return false;
            }
            let prev_char = before[..prefix_start].chars().next_back();
            match prev_char {
                None => return false,
                Some(c) if !c.is_alphanumeric() => return false,
                Some(c) if is_cjk_ideograph(c) => return false,
                _ => {}
            }
        }
    }

    true
}

/// Push a sentence if it contains any non-whitespace content.
fn push_sentence(sentences: &mut Vec<SentenceBound>, text: &str, start: usize, end: usize) {
    // Trim leading whitespace from the sentence start.
    let trimmed_start = text[start..end]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| start + i)
        .unwrap_or(end);

    if trimmed_start < end {
        // Trim trailing whitespace.
        let trimmed_end = text[trimmed_start..end]
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_whitespace())
            .map(|(i, c)| trimmed_start + i + c.len_utf8())
            .unwrap_or(trimmed_start);

        if trimmed_start < trimmed_end {
            sentences.push(SentenceBound {
                byte_start: trimmed_start,
                byte_end: trimmed_end,
            });
        }
    }
}

/// Build paragraph boundaries from text (split on \n\n or \r\n\r\n).
/// Byte offset just past the blank line that starts at `bytes[newline]`, or
/// None if that newline ends a single line rather than a paragraph.
///
/// A blank line is two terminators in a row, and either may be "\r\n" or "\n".
/// `bytes[newline]` must be the "\n" of the first terminator.
///
/// One definition, because four copies cost four rounds of defects: a bare
/// "\n\n" test dead on CRLF, a slice that carried its own "\r", a swap that
/// lost the mixed form, and paragraph bounds that no sentence bound matched.
/// Each was a copy of this rule somebody had already fixed elsewhere.
pub(crate) fn blank_line_end(bytes: &[u8], newline: usize) -> Option<usize> {
    let mut j = newline + 1;
    if bytes.get(j) == Some(&b'\r') {
        j += 1;
    }
    (bytes.get(j) == Some(&b'\n')).then_some(j + 1)
}

fn build_paragraphs(text: &str) -> Vec<ParagraphBound> {
    let mut result = Vec::new();
    let mut prev = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // A blank line is two terminators in a row, and either may be "\r\n" or
        // "\n". Matching only the two pure forms missed a document that mixes
        // them around one blank line, which is what a patch or a merge
        // produces, and the paragraph then swallowed the rest of the file.
        if bytes[i] == b'\n' {
            if let Some(next) = blank_line_end(bytes, i) {
                // Cut before the "\r" a CRLF terminator puts here, so the
                // paragraph does not carry its own line ending.
                let end = i - usize::from(text[..i].ends_with('\r'));
                push_paragraph(&mut result, text, prev, end);
                prev = next;
                i = prev;
                continue;
            }
        }
        i += 1;
    }

    // Trailing paragraph.
    if prev < text.len() {
        push_paragraph(&mut result, text, prev, text.len());
    }

    result
}

fn push_paragraph(paragraphs: &mut Vec<ParagraphBound>, text: &str, start: usize, end: usize) {
    let slice = &text[start..end];
    if slice.chars().any(|c| !c.is_whitespace()) {
        paragraphs.push(ParagraphBound {
            byte_start: start,
            byte_end: end,
        });
    }
}

#[cfg(test)]
#[path = "../../tests/unit/engine/sentence/tests.rs"]
mod tests;
