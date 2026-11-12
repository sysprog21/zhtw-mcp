//! Scan for rewrite goals in text an agent reads while deciding how hard to
//! rewrite. A number to reach ("no more than 40%", "lower the score") rewards
//! editing for its own sake, so measurement stays in the harness and the
//! instruction layer carries reasons not to edit instead.
//!
//! Caller-supplied gate settings (ai_threshold, max_errors) are the caller's
//! own limits and not goals the agent should chase, so lines naming them are
//! skipped.

use regex::Regex;

const PHRASES: &[&str] = &[
    "不超過",
    "不應超過",
    "至少",
    "低於",
    "at most",
    "at least",
    "no more than",
    "below",
];

const GATE_SETTINGS: &[&str] = &["ai_threshold", "max_errors"];

/// The lines of text that state a rewrite goal, as "line: pattern" strings.
pub(crate) fn rewrite_goal_hits(text: &str) -> Vec<String> {
    let score =
        Regex::new(r"(?i)(reduce|lower|降低).{0,20}score|score.{0,20}(reduce|lower|降低)").unwrap();
    let mut hits = Vec::new();
    for line in text.lines() {
        if GATE_SETTINGS.iter().any(|g| line.contains(g)) {
            continue;
        }
        let lower = line.to_lowercase();
        for p in PHRASES {
            if lower.contains(p) {
                hits.push(format!("{p:?} in {line:?}"));
            }
        }
        if score.is_match(line) {
            hits.push(format!("score pattern in {line:?}"));
        }
    }
    hits
}

#[test]
fn the_scan_catches_each_goal_pattern() {
    for bad in [
        "列表不超過 40%",
        "列表不應超過全文",
        "至少改三處",
        "低於 2 個",
        "At most 2 per paragraph",
        "Edit at least three places",
        "no more than 40% of the text",
        "keep it below 3",
        "Reduce the AI score",
        "lower your score",
        "降低 AI 分數 score",
        "score 要降低",
    ] {
        assert!(!rewrite_goal_hits(bad).is_empty(), "missed: {bad}");
    }
}

#[test]
fn the_scan_skips_gate_settings_and_plain_text() {
    assert!(rewrite_goal_hits("reject if below 3 errors (max_errors)").is_empty());
    assert!(rewrite_goal_hits("ai_threshold: low at most").is_empty());
    assert!(rewrite_goal_hits("單獨一個破折號不必改").is_empty());
}
