use anyhow::{Context, Result};

use super::ruleset::Ruleset;

/// Load the embedded ruleset from pre-serialized postcard binary.
/// The binary is generated at build time from assets/ruleset.json by build.rs.
/// Postcard deserialization is ~10x faster than serde_json and zero-alloc for
/// the parse step itself (allocations come from owned String fields).
pub fn load_embedded_ruleset() -> Result<Ruleset> {
    static RULESET_POSTCARD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ruleset.postcard"));
    postcard::from_bytes(RULESET_POSTCARD).context("parse embedded ruleset (postcard)")
}

/// Compute a combined hash of all rules (spelling + case) for reproducibility tracking.
/// This hash changes whenever base rules or overrides change.
pub fn compute_ruleset_hash(
    spelling_rules: &[super::ruleset::SpellingRule],
    case_rules: &[super::ruleset::CaseRule],
) -> String {
    let canonical = serde_json::json!({
        "spelling": spelling_rules,
        "case": case_rules,
    });
    let bytes = serde_json::to_vec(&canonical).expect("Value serialization is infallible");
    blake3::hash(&bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ruleset::{CaseRule, RuleType, SpellingRule};

    #[test]
    fn hash_deterministic() {
        let rules = vec![SpellingRule {
            from: "軟件".into(),
            to: vec!["軟體".into()],
            rule_type: RuleType::CrossStrait,

            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }];
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
        let rules_a = vec![SpellingRule {
            from: "軟件".into(),
            to: vec!["軟體".into()],
            rule_type: RuleType::CrossStrait,

            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }];
        let rules_b = vec![SpellingRule {
            from: "內存".into(),
            to: vec!["記憶體".into()],
            rule_type: RuleType::CrossStrait,

            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }];
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
        let source = include_str!("../../assets/ruleset.json");
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
        // Full field-by-field parity: catches postcard schema drift
        // (e.g. field reorder, enum variant reorder between build.rs and
        // runtime types).
        for (i, (j, p)) in json_ruleset
            .spelling_rules
            .iter()
            .zip(postcard_ruleset.spelling_rules.iter())
            .enumerate()
        {
            assert_eq!(j.from, p.from, "spelling rule {i}: from mismatch");
            assert_eq!(j.to, p.to, "spelling rule {i}: to mismatch");
            assert_eq!(
                j.rule_type, p.rule_type,
                "spelling rule {i}: rule_type mismatch"
            );
            assert_eq!(
                j.disabled, p.disabled,
                "spelling rule {i}: disabled mismatch"
            );
            assert_eq!(j.context, p.context, "spelling rule {i}: context mismatch");
            assert_eq!(j.english, p.english, "spelling rule {i}: english mismatch");
            assert_eq!(
                j.exceptions, p.exceptions,
                "spelling rule {i}: exceptions mismatch"
            );
            assert_eq!(
                j.context_clues, p.context_clues,
                "spelling rule {i}: context_clues mismatch"
            );
            assert_eq!(
                j.negative_context_clues, p.negative_context_clues,
                "spelling rule {i}: negative_context_clues mismatch"
            );
            assert_eq!(
                j.positional_clues, p.positional_clues,
                "spelling rule {i}: positional_clues mismatch"
            );
            assert_eq!(j.tags, p.tags, "spelling rule {i}: tags mismatch");
        }
        for (i, (j, p)) in json_ruleset
            .case_rules
            .iter()
            .zip(postcard_ruleset.case_rules.iter())
            .enumerate()
        {
            assert_eq!(j.term, p.term, "case rule {i}: term mismatch");
            assert_eq!(
                j.alternatives, p.alternatives,
                "case rule {i}: alternatives mismatch"
            );
            assert_eq!(j.disabled, p.disabled, "case rule {i}: disabled mismatch");
        }
    }
}
