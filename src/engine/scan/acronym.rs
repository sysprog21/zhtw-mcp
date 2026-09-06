// Spaced-acronym rejoining for ASR transcripts.
//
// Detects sequences of single uppercase ASCII letters separated by spaces (e.g.
// "C P U", "F P G A") and suggests the joined form ("CPU", "FPGA"). Only flags
// sequences of 2+ letters that form a known acronym or any sequence of 3+
// letters (high confidence that spacing is an ASR artifact).

use super::super::excluded::is_excluded;
use super::emit::Emitter;
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// Known 2-letter acronyms that should be rejoined.
/// Without this list, 2-letter sequences like "I O" would false-positive
/// on normal English.
static KNOWN_TWO_LETTER: &[&str] = &[
    "AI", "IO", "OS", "IP", "UI", "VM", "GC", "PC", "DB", "PR", "RD", "QA", "ML", "FE", "BE",
];

/// Scan for spaced single-letter uppercase sequences.
pub(crate) fn scan_spaced_acronyms(em: &mut Emitter<'_>) {
    let text = em.text;
    let excluded = em.excluded;
    let issues = &mut *em.issues;

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for an uppercase ASCII letter.
        if !bytes[i].is_ascii_uppercase() {
            i += 1;
            continue;
        }

        // Check that this letter is isolated (preceded by non-alpha or SOL).
        if i > 0 && bytes[i - 1].is_ascii_alphabetic() {
            i += 1;
            continue;
        }

        // Collect sequence of "X " or "X<end>".
        let seq_start = i;
        let mut letters = Vec::new();
        let mut j = i;

        loop {
            if j >= len || !bytes[j].is_ascii_uppercase() {
                break;
            }
            // Ensure this is a single letter (next is space or non-alpha).
            let after = j + 1;
            if after < len && bytes[after].is_ascii_alphabetic() {
                break;
            }
            letters.push(bytes[j] as char);
            // Skip the space after the letter.
            if after < len && bytes[after] == b' ' {
                j = after + 1;
            } else {
                j = after;
                break;
            }
        }

        if letters.len() < 2 {
            i += 1;
            continue;
        }

        let joined: String = letters.iter().collect();
        // Trim trailing space: the matched range should end at the last letter.
        let seq_end = if j > seq_start && j <= len && bytes[j - 1] == b' ' {
            j - 1
        } else {
            j
        };

        // For 2-letter sequences, require a known acronym match.
        if letters.len() == 2 && !KNOWN_TWO_LETTER.contains(&joined.as_str()) {
            i = seq_end;
            continue;
        }

        if is_excluded(seq_start, seq_end, excluded) {
            i = seq_end;
            continue;
        }

        let matched = &text[seq_start..seq_end];
        // Don't flag if it already matches the joined form (no spaces).
        if matched == joined {
            i = seq_end;
            continue;
        }

        issues.push(Issue::new(
            seq_start,
            seq_end - seq_start,
            matched.to_string(),
            vec![joined],
            IssueType::Repetition, // Reuse Repetition for ASR artifacts
            Severity::Info,
        ));
        i = seq_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::excluded::ByteRange;

    fn scan(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_spaced_acronyms(&mut Emitter::new(text, &[], &mut issues));
        issues
    }

    #[test]
    fn catches_three_letter_acronym() {
        let issues = scan("使用 C P U 架構");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "C P U");
        assert_eq!(issues[0].suggestions.as_ref(), ["CPU"]);
    }

    #[test]
    fn catches_four_letter_acronym() {
        let issues = scan("F P G A 開發板");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions.as_ref(), ["FPGA"]);
    }

    #[test]
    fn catches_known_two_letter() {
        let issues = scan("A I 技術");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions.as_ref(), ["AI"]);
    }

    #[test]
    fn skips_unknown_two_letter() {
        // "X Y" is not a known acronym.
        let issues = scan("座標 X Y 軸");
        assert!(issues.is_empty());
    }

    #[test]
    fn skips_normal_words() {
        let issues = scan("Hello World from Linux");
        assert!(issues.is_empty());
    }

    #[test]
    fn skips_excluded() {
        let excluded = vec![ByteRange { start: 0, end: 50 }];
        let mut issues = Vec::new();
        scan_spaced_acronyms(&mut Emitter::new("C P U 架構", &excluded, &mut issues));
        assert!(issues.is_empty());
    }

    #[test]
    fn acronym_at_end_of_string() {
        let issues = scan("使用 C P U");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "C P U");
        assert_eq!(issues[0].suggestions.as_ref(), ["CPU"]);
    }

    #[test]
    fn multiple_acronyms_in_one_text() {
        let issues = scan("C P U 和 G P U 的效能");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].suggestions.as_ref(), ["CPU"]);
        assert_eq!(issues[1].suggestions.as_ref(), ["GPU"]);
    }

    #[test]
    fn skips_lowercase_letters() {
        // Lowercase single letters are not ASR artifacts.
        let issues = scan("a b c d e");
        assert!(issues.is_empty());
    }

    #[test]
    fn skips_mixed_case_letters() {
        // 'A b C' has mixed case; not a valid spaced acronym.
        let issues = scan("A b C");
        assert!(issues.is_empty());
    }

    #[test]
    fn known_two_letter_at_end() {
        let issues = scan("使用 A I");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions.as_ref(), ["AI"]);
    }

    #[test]
    fn five_letter_acronym() {
        let issues = scan("R I S C V 架構");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions.as_ref(), ["RISCV"]);
    }

    #[test]
    fn does_not_fire_on_already_joined() {
        // If text already says CPU (no spaces), nothing to flag.
        let issues = scan("使用CPU架構");
        assert!(issues.is_empty());
    }

    #[test]
    fn adjacent_to_word_boundary() {
        // 'CPU' as part of a word should not be split-detected.
        let issues = scan("myCPU is fast");
        assert!(issues.is_empty());
    }
}
