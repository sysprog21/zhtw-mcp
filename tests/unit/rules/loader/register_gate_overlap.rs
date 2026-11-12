use super::load_embedded_ruleset;
use crate::engine::scan::Scanner;
use crate::rules::ruleset::{
    Profile, RegisterGate, RegisterMode, RuleType, Severity, SpellingRule,
};

// The register gate runs inside the spelling scan, before longest-match
// resolution, so a gated rule that does not apply in the document's register
// never competes with the rules around it. These tests pin that with a
// synthetic ruleset in which a gated pattern overlaps an ungated one.

fn gated(from: &str, gate: RegisterGate) -> SpellingRule {
    let mut rule = SpellingRule::new(from, vec![], RuleType::AiFiller);
    rule.severity = Some(Severity::Info);
    rule.register = Some(gate);
    rule
}

fn ungated(from: &str, to: &str) -> SpellingRule {
    SpellingRule::new(from, vec![to.into()], RuleType::CrossStrait)
}

fn found(scanner: &Scanner, text: &str, mode: RegisterMode) -> Vec<String> {
    let cfg = Profile::Base.config().with_register(mode);
    scanner
        .scan_with_config(text, &[], cfg)
        .issues
        .into_iter()
        .map(|i| i.found)
        .collect()
}

#[test]
fn dropped_gated_match_no_longer_hides_a_rule_inside_it() {
    let scanner = Scanner::new(
        vec![
            gated("特此公告", RegisterGate::Casual),
            ungated("公告", "佈告"),
        ],
        vec![],
    );
    let text = "我們特此公告，大家都知道了。";
    // Formal rejects the gated rule: the shorter ungated rule inside it fires.
    assert_eq!(found(&scanner, text, RegisterMode::Formal), ["公告"]);
    // Casual admits it: the longest match still wins, as before.
    assert_eq!(found(&scanner, text, RegisterMode::Casual), ["特此公告"]);
}

#[test]
fn dropped_gated_match_no_longer_hides_a_crossing_rule() {
    let scanner = Scanner::new(
        vec![
            gated("特此公告", RegisterGate::Casual),
            ungated("公告事項", "公佈事項"),
        ],
        vec![],
    );
    let text = "特此公告事項如下。";
    assert_eq!(found(&scanner, text, RegisterMode::Formal), ["公告事項"]);
    assert_eq!(found(&scanner, text, RegisterMode::Casual), ["特此公告"]);
}

#[test]
fn gate_of_the_other_register_is_dropped_too() {
    let scanner = Scanner::new(
        vec![
            gated("特此公告", RegisterGate::Formal),
            ungated("公告", "佈告"),
        ],
        vec![],
    );
    let text = "我們特此公告，大家都知道了。";
    assert_eq!(found(&scanner, text, RegisterMode::Casual), ["公告"]);
    assert_eq!(found(&scanner, text, RegisterMode::Formal), ["特此公告"]);
}

#[test]
fn scan_before_a_rejected_hit_is_kept_after_the_restart() {
    let scanner = Scanner::new(
        vec![
            gated("特此公告", RegisterGate::Casual),
            ungated("公告", "佈告"),
            ungated("軟件", "軟體"),
        ],
        vec![],
    );
    let text = "軟件更新。特此公告軟件。";
    assert_eq!(
        found(&scanner, text, RegisterMode::Formal),
        ["軟件", "公告", "軟件"]
    );
}

#[test]
fn embedded_ruleset_has_register_gated_rules() {
    let ruleset = load_embedded_ruleset().expect("embedded ruleset loads");
    let gated = ruleset
        .spelling_rules
        .iter()
        .filter(|r| r.register.is_some() && !r.disabled)
        .count();
    assert!(gated > 0, "expected register-gated rules in the ruleset");
}
