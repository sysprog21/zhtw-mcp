use super::*;
use crate::rules::ruleset::{CaseRule, RuleType, SpellingRule};

#[test]
fn hash_deterministic() {
    let rules = vec![SpellingRule::new(
        "軟件",
        vec!["軟體".into()],
        RuleType::CrossStrait,
    )];
    let case_rules = vec![CaseRule {
        term: "JavaScript".into(),
        alternatives: None,
        disabled: false,
    }];

    let h1 = compute_ruleset_hash(&rules, &case_rules);
    let h2 = compute_ruleset_hash(&rules, &case_rules);
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 64);
}

#[test]
fn hash_changes_with_rules() {
    let rules_a = vec![SpellingRule::new(
        "軟件",
        vec!["軟體".into()],
        RuleType::CrossStrait,
    )];
    let rules_b = vec![SpellingRule::new(
        "內存",
        vec!["記憶體".into()],
        RuleType::CrossStrait,
    )];
    let case_rules: Vec<CaseRule> = vec![];

    let h1 = compute_ruleset_hash(&rules_a, &case_rules);
    let h2 = compute_ruleset_hash(&rules_b, &case_rules);
    assert_ne!(h1, h2);
}

#[test]
fn embedded_ruleset_parses() {
    let ruleset = load_embedded_ruleset().unwrap();
    assert!(!ruleset.spelling_rules.is_empty());
    assert!(!ruleset.case_rules.is_empty());
}

#[test]
fn embedded_ruleset_matches_json() {
    // Verify postcard binary matches original JSON source.
    let source = include_str!("../../../../assets/ruleset.json");
    let json_ruleset: Ruleset = serde_json::from_str(source).unwrap();
    let postcard_ruleset = load_embedded_ruleset().unwrap();
    assert_eq!(
        json_ruleset.spelling_rules.len(),
        postcard_ruleset.spelling_rules.len()
    );
    assert_eq!(
        json_ruleset.case_rules.len(),
        postcard_ruleset.case_rules.len()
    );

    // Full field-by-field parity: the JSON the ruleset ships as must survive
    // the postcard round trip unchanged.
    //
    // build.rs no longer mirrors these types; it include!s src/rules/schema.rs,
    // so a field cannot exist on one side and not the other. What remains is
    // the asymmetry inside serde itself: postcard is not self-describing, so
    // any attribute that changes what Serialize emits without changing what
    // Deserialize expects (skip_serializing_if is the one that already bit us)
    // silently shortens the stream and shifts every following field. That is
    // what this test still catches.
    //
    // The destructuring is load-bearing, not style. Listing fields by hand let
    // two of them (editorial_confidence, context_suggestions) go unchecked for
    // several releases. Exhaustive destructuring makes the compiler refuse to
    // build this test until a new field is handled here, so the coverage cannot
    // silently rot again.
    for (i, (j, p)) in json_ruleset
        .spelling_rules
        .iter()
        .zip(postcard_ruleset.spelling_rules.iter())
        .enumerate()
    {
        let SpellingRule {
            from,
            to,
            rule_type,
            severity,
            disabled,
            context,
            english,
            source,
            exceptions,
            context_clues,
            negative_context_clues,
            positional_clues,
            context_suggestions,
            tags,
            editorial_confidence,
            structural_guard,
        } = j;
        assert_eq!(*from, p.from, "spelling rule {i}: from mismatch");
        assert_eq!(*to, p.to, "spelling rule {i}: to mismatch");
        assert_eq!(
            *rule_type, p.rule_type,
            "spelling rule {i}: rule_type mismatch"
        );
        assert_eq!(
            *severity, p.severity,
            "spelling rule {i}: severity mismatch"
        );
        assert_eq!(
            *disabled, p.disabled,
            "spelling rule {i}: disabled mismatch"
        );
        assert_eq!(*context, p.context, "spelling rule {i}: context mismatch");
        assert_eq!(*english, p.english, "spelling rule {i}: english mismatch");
        assert_eq!(*source, p.source, "spelling rule {i}: source mismatch");
        assert_eq!(
            *structural_guard, p.structural_guard,
            "spelling rule {i}: structural_guard mismatch"
        );
        assert_eq!(
            *exceptions, p.exceptions,
            "spelling rule {i}: exceptions mismatch"
        );
        assert_eq!(
            *context_clues, p.context_clues,
            "spelling rule {i}: context_clues mismatch"
        );
        assert_eq!(
            *negative_context_clues, p.negative_context_clues,
            "spelling rule {i}: negative_context_clues mismatch"
        );
        assert_eq!(
            *positional_clues, p.positional_clues,
            "spelling rule {i}: positional_clues mismatch"
        );
        assert_eq!(
            *context_suggestions, p.context_suggestions,
            "spelling rule {i}: context_suggestions mismatch"
        );
        assert_eq!(*tags, p.tags, "spelling rule {i}: tags mismatch");
        assert_eq!(
            *editorial_confidence, p.editorial_confidence,
            "spelling rule {i}: editorial_confidence mismatch"
        );
    }
    for (i, (j, p)) in json_ruleset
        .case_rules
        .iter()
        .zip(postcard_ruleset.case_rules.iter())
        .enumerate()
    {
        // Destructured for the same reason as the spelling loop above.
        let CaseRule {
            term,
            alternatives,
            disabled,
        } = j;
        assert_eq!(*term, p.term, "case rule {i}: term mismatch");
        assert_eq!(
            *alternatives, p.alternatives,
            "case rule {i}: alternatives mismatch"
        );
        assert_eq!(*disabled, p.disabled, "case rule {i}: disabled mismatch");
    }
}
