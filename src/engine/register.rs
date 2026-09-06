//! Which register a document is written in.
//!
//! A peer of [`crate::engine::zhtype`]: character and phrase tables plus one
//! classifier that reads raw text before the scan and answers a single
//! question about the whole document.
//!
//! Evidence only, never a rule. This decides which detectors should hold their
//! tongue, never what to rewrite. A false formal reading costs a suppression;
//! no false reading costs an edit, so every gate below errs toward Casual.

use crate::engine::scan::{char_bounded_end, is_cjk_ideograph};
use crate::rules::ruleset::Register;

// Forms that occur in 公文 and formal correspondence and effectively nowhere
// else, so one anywhere in the document settles the register. A letter that
// opens on two paragraphs of context still signs off 謹啟.
const FORMAL_ANCHORS: &[&str] = &[
    "敬啟者",
    "謹啟",
    "謹此",
    "茲就",
    "鈞鑒",
    "台端",
    "惠請",
    "此致",
    "特此函達",
    "相應函復",
];

// How a contract refers to itself in its own opening. The determiner is the
// whole of the evidence: 合約 on its own is the subject of any article about
// contract law, and scoping the bare noun to the head was not enough, because a
// note short enough to be all head is exactly the casual writing that then lost
// its findings.
//
// 本 only. 該合約 and 此合約 are anaphoric, which is how an article refers to a
// contract it has just named, so they carry no evidence that the document in
// hand is the contract. Missing a contract whose opening never says 本合約 is
// the cheaper mistake, for the same reason the anchors take a boundary test.
const FORMAL_HEAD_MARKERS: &[&str] = &["本合約", "本契約"];

// How much of the document counts as the head for the weaker markers.
const FORMAL_HEAD_CHARS: usize = 100;

/// Whether `anchor` occurring at `at` starts a phrase rather than continuing a
/// word.
///
/// Chinese has no spaces to search between, so a bare substring test reads
/// 台端 out of 平台端 and 此致 out of 因此致使, and either one silently turns a
/// technical document formal. The tell is the character in front: an anchor
/// that opens a salutation follows a line break, punctuation or nothing at
/// all, while a false hit follows the ideograph that owns it.
///
/// Deliberately one-sided. 此致敬禮 and 敬啟者： continue into CJK on the right
/// and are exactly what this looks for, so only the left side is tested.
///
/// The cost of being wrong is asymmetric, which is why this errs strict: a
/// missed 公文 leaves the linter where it was before the register existed,
/// while a false formal reading silently drops real findings. That is the
/// trade that rejects 王大明謹啟, whose sign-off runs straight on from the
/// name, and it is the right way to be wrong.
fn starts_a_phrase(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|prev| !is_cjk_ideograph(prev))
}

fn has_formal_anchor(text: &str) -> bool {
    FORMAL_ANCHORS.iter().any(|anchor| {
        text.match_indices(anchor)
            .any(|(at, _)| starts_a_phrase(text, at))
    })
}

/// Decide whether `text` is written in a formal register.
pub(crate) fn detect_register(text: &str) -> Register {
    if has_formal_anchor(text) {
        return Register::Formal;
    }
    let head_end = char_bounded_end(text, 0, FORMAL_HEAD_CHARS);
    let head = &text[..head_end];
    if FORMAL_HEAD_MARKERS.iter().any(|m| {
        head.match_indices(m)
            .any(|(at, _)| starts_a_phrase(head, at))
    }) {
        return Register::Formal;
    }
    Register::Casual
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_is_casual() {
        assert_eq!(
            detect_register("我們今天要處理這個問題。"),
            Register::Casual
        );
    }

    #[test]
    fn empty_text_is_casual() {
        assert_eq!(detect_register(""), Register::Casual);
    }

    #[test]
    fn a_salutation_settles_the_register() {
        assert_eq!(detect_register("敬啟者：茲有一事相商。"), Register::Formal);
    }

    #[test]
    fn a_sign_off_past_the_head_still_settles_it() {
        // The whole point of reading the document rather than its first hundred
        // characters: the strongest evidence is at the bottom.
        let body = "說明事項。".repeat(60);
        let text = format!("{body}\n此致\n敬禮");
        assert!(text.chars().count() > FORMAL_HEAD_CHARS);
        assert_eq!(detect_register(&text), Register::Formal);
    }

    #[test]
    fn an_anchor_inside_a_word_is_not_an_anchor() {
        // 台端 sits inside 平台端, 前台端 and 後台端, which is ordinary
        // technical prose about the platform or front-end side of a system.
        for text in [
            "這個錯誤發生在平台端。",
            "前台端與後台端都要修。",
            "資料在後台端就已經遺失了。",
        ] {
            assert_eq!(detect_register(text), Register::Casual, "{text}");
        }
    }

    #[test]
    fn a_connective_is_not_a_sign_off() {
        // 此致 sits inside 因此致使, one of the commonest connectives there is.
        assert_eq!(detect_register("因此致使資料遺失。"), Register::Casual);
        assert_eq!(detect_register("因此致命的錯誤發生了。"), Register::Casual);
    }

    #[test]
    fn a_discount_is_not_a_request() {
        // 惠請 sits inside 優惠請洽, which is advertising copy.
        assert_eq!(detect_register("優惠請洽門市人員。"), Register::Casual);
    }

    #[test]
    fn an_anchor_after_punctuation_or_a_line_break_counts() {
        assert_eq!(detect_register("說明如上。\n此致\n敬禮"), Register::Formal);
        assert_eq!(detect_register("報告完畢，惠請查照。"), Register::Formal);
    }

    #[test]
    fn an_anchor_opening_the_document_counts() {
        // Nothing in front of it at all is the boundary case that matters most,
        // because it is what a salutation actually looks like.
        assert_eq!(detect_register("謹此陳報。"), Register::Formal);
    }

    #[test]
    fn contract_nouns_count_only_in_the_head() {
        assert_eq!(detect_register("本合約之當事人如下。"), Register::Formal);

        let padding = "這是一段說明文字。".repeat(20);
        let text = format!("{padding}本合約之當事人如下。");
        assert!(text.chars().count() > FORMAL_HEAD_CHARS);
        assert_eq!(detect_register(&text), Register::Casual);
    }

    #[test]
    fn an_anaphoric_contract_reference_is_not_self_reference() {
        // 該合約 is how an article refers to a contract it has just named, not
        // how a contract refers to itself.
        for text in [
            "該合約價值十億美元，我們予以處理。",
            "此合約的爭議點在於付款條件。",
            "這篇文章討論合約與契約的差異。",
        ] {
            assert_eq!(detect_register(text), Register::Casual, "{text}");
        }
    }

    #[test]
    fn a_place_name_ending_in_the_determiner_is_not_a_contract() {
        // 本合約 sits inside 日本合約, which is a contract with Japan rather
        // than a contract naming itself.
        assert_eq!(
            detect_register("日本合約的談判仍在進行。"),
            Register::Casual
        );
    }

    #[test]
    fn the_head_boundary_lands_on_a_character() {
        // A multi-byte head cut must not slice through a CJK character. Every
        // prefix length from nothing to past the window has to be safe.
        for n in 0..(FORMAL_HEAD_CHARS + 20) {
            let text = "說".repeat(n);
            assert_eq!(detect_register(&text), Register::Casual, "n={n}");
        }
    }

    #[test]
    fn an_anchor_split_across_a_latin_run_is_not_an_anchor() {
        assert_eq!(detect_register("平台 端點的設定"), Register::Casual);
    }
}
