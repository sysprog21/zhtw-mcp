use super::*;

fn bounds(text: &str) -> BoundaryIndex {
    BoundaryIndex::build(text, &[])
}

#[test]
fn cjk_sentence_split() {
    let text = "你好。世界！測試？完成";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 4);
    assert_eq!(
        &text[idx.sentences[0].byte_start..idx.sentences[0].byte_end],
        "你好。"
    );
    assert_eq!(
        &text[idx.sentences[1].byte_start..idx.sentences[1].byte_end],
        "世界！"
    );
    assert_eq!(
        &text[idx.sentences[2].byte_start..idx.sentences[2].byte_end],
        "測試？"
    );
    assert_eq!(
        &text[idx.sentences[3].byte_start..idx.sentences[3].byte_end],
        "完成"
    );
}

#[test]
fn semicolon_splits_sentence() {
    let text = "前半句；後半句。";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 2);
    assert_eq!(
        &text[idx.sentences[0].byte_start..idx.sentences[0].byte_end],
        "前半句；"
    );
    assert_eq!(
        &text[idx.sentences[1].byte_start..idx.sentences[1].byte_end],
        "後半句。"
    );
}

#[test]
fn paragraph_break_splits_sentence() {
    let text = "第一段\n\n第二段";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 2);
    assert_eq!(idx.paragraphs.len(), 2);
}

#[test]
fn latin_sentence_with_abbreviation() {
    let text = "Mr. Smith went home. He is here.";
    let idx = bounds(text);
    // "Mr." should NOT split. "home." should split. "here." should end.
    assert_eq!(idx.sentences.len(), 2);
    assert!(idx.sentences[0].byte_end <= text.find("He").unwrap());
}

#[test]
fn latin_sentence_splits_across_multiple_whitespaces() {
    // cubic review: must handle multiple spaces / tabs / newlines between the
    // terminator and the next capital letter.
    let text = "Alice went home.  Bob followed.";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 2);

    let text = "One ended.\n\nTwo started.";
    let idx = bounds(text);
    assert!(idx.sentences.len() >= 2);

    let text = "Foo.\tBar is next.";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 2);
}

#[test]
fn mixed_cjk_latin() {
    let text = "這是測試。This is a test. 第二句。";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 3);
}

#[test]
fn cjk_adjacent_abbreviation_not_a_sentence_end() {
    // Codex/Gemini review: abbreviation with CJK preceding char should still be
    // recognized as an abbreviation, not a sentence end.
    let text = "這由Mr. Smith處理過。";
    let idx = bounds(text);
    // "Mr." should NOT split. Whole thing is one sentence.
    assert_eq!(idx.sentences.len(), 1);
}

#[test]
fn exclusion_zone_breaks_sentence() {
    let text = "前面的文字`code`後面的文字。";

    // Simulate exclusion zone over code (bytes for the backtick-wrapped part).
    let code_start = text.find('`').unwrap();
    let code_end = text.rfind('`').unwrap() + 1;
    let excluded = vec![ByteRange {
        start: code_start,
        end: code_end,
    }];
    let idx = BoundaryIndex::build(text, &excluded);
    // Should have at least 2 sentence fragments.
    assert!(idx.sentences.len() >= 2);
}

#[test]
fn empty_text() {
    let idx = bounds("");
    assert!(idx.sentences.is_empty());
    assert!(idx.paragraphs.is_empty());
}

#[test]
fn whitespace_only() {
    let idx = bounds("   \n\n   ");
    assert!(idx.sentences.is_empty());
}

#[test]
fn sentence_at_lookup() {
    let text = "第一句。第二句。";
    let idx = bounds(text);
    let s1_mid = text.find('一').unwrap();
    let found = idx.sentence_at(s1_mid);
    assert!(found.is_some());
    assert_eq!(found.unwrap().byte_start, 0);
}

#[test]
fn paragraph_at_lookup() {
    let text = "段落一\n\n段落二";
    let idx = bounds(text);
    let p2_start = text.rfind('段').unwrap();
    let found = idx.paragraph_at(p2_start);
    assert!(found.is_some());
    assert!(found.unwrap().byte_start > 0);
}

#[test]
fn sentences_in_paragraph() {
    let text = "第一句。第二句。\n\n第三句。";
    let idx = bounds(text);
    assert_eq!(idx.paragraphs.len(), 2);
    let sents = idx.sentence_slice(&idx.paragraphs[0]);
    assert_eq!(sents.len(), 2);
    let sents2 = idx.sentence_slice(&idx.paragraphs[1]);
    assert_eq!(sents2.len(), 1);
}

#[test]
fn crlf_paragraph_break() {
    let text = "段落一\r\n\r\n段落二";
    let idx = bounds(text);
    assert_eq!(idx.paragraphs.len(), 2);
}

#[test]
fn crlf_paragraph_break_splits_sentence() {
    let text = "第一段\r\n\r\n展望未來";
    let idx = bounds(text);
    assert_eq!(idx.sentences.len(), 2);
    assert_eq!(idx.paragraphs.len(), 2);

    let first_para_sents = idx.sentence_slice(&idx.paragraphs[0]);
    let second_para_sents = idx.sentence_slice(&idx.paragraphs[1]);
    assert_eq!(first_para_sents.len(), 1);
    assert_eq!(second_para_sents.len(), 1);
}

// Paragraph bounds and sentence bounds have to agree on where a blank line is,
// or "sentence_slice" returns nothing for a paragraph that plainly has content.
// Three separate splitters answer this question, and each one has been wrong
// about a different terminator combination in turn, so the property is asserted
// across all four rather than the pair that happened to work.
#[test]
fn every_blank_line_form_agrees_across_paragraphs_and_sentences() {
    for sep in ["\n\n", "\r\n\r\n", "\n\r\n", "\r\n\n"] {
        for (a, b) in [
            ("第一段內容", "第二段內容"),
            ("第一句。第二句。", "第三句。"),
        ] {
            let doc = format!("{a}{sep}{b}");
            let idx = BoundaryIndex::build(&doc, &[]);
            assert_eq!(idx.paragraphs.len(), 2, "paragraphs for {sep:?} in {doc:?}");
            for para in &idx.paragraphs {
                assert!(
                    !idx.sentence_slice(para).is_empty(),
                    "no sentence in a paragraph with content, {sep:?} in {doc:?}"
                );
            }
        }
    }
}

#[test]
fn dot_before_extension_b_char_starts_sentence() {
    // 𠮷 (U+20BB7) is Ext B: the old BMP-only predicate missed it.
    let text = "Done. 𠮷野家";
    assert_eq!(bounds(text).sentences.len(), 2);
}

#[test]
fn dot_before_cjk_opener_starts_sentence() {
    let text = "Done. 「你好」";
    assert_eq!(bounds(text).sentences.len(), 2);
}

#[test]
fn dot_before_fullwidth_bang_does_not_start_sentence() {
    // is_cjk_context would admit this; the sentence predicate must not.
    // (Fullwidth Ａ is uppercase and splits either way, so it pins nothing.)
    assert_eq!(bounds("Done. ！").sentences.len(), 1);
}

#[test]
fn extension_b_char_before_abbreviation_is_not_a_sentence_end() {
    let text = "𠮷Mr. Smith 處理過。";
    assert_eq!(bounds(text).sentences.len(), 1);
}

#[test]
fn ideographic_comma_before_abbreviation_is_not_a_sentence_end() {
    let text = "甲、Mr. Smith 處理過。";
    assert_eq!(bounds(text).sentences.len(), 1);
}
