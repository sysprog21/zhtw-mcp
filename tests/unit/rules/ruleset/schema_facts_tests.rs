use super::*;

/// Facts about the ruleset schema that `scripts/check-ruleset.py` needs.
///
/// Written to `scripts/schema-facts.json`, which is checked in and read by
/// the script.  Both sides used to hand-parse the other's source: Python
/// regexed `schema.rs` for field and variant names, and a Rust test string-
/// parsed the Python for its rule-type sets.  Two parsers, four fail-open
/// holes between them, and a patch on the first hole that added a second
/// parse of the same file.
///
/// Serde already knows every one of these facts, so ask it and write the
/// answer down.  The generated file is data, so the Python side is a
/// `json.load` and the Rust side is this test, which fails when the file on
/// disk no longer matches what the types say.
#[test]
fn schema_facts_file_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/schema-facts.json");
    let current = serde_json::to_string_pretty(&schema_facts()).expect("serialize facts") + "\n";

    if std::env::var_os("UPDATE_SCHEMA_FACTS").is_some() {
        std::fs::write(path, &current).expect("write schema-facts.json");
        return;
    }
    let on_disk = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        on_disk, current,
        "scripts/schema-facts.json is stale; regenerate with \
         UPDATE_SCHEMA_FACTS=1 cargo test schema_facts_file_is_current"
    );
}

/// Every fact the lint script would otherwise copy by hand.
///
/// Field names come from serializing a real rule, so renames are whatever
/// serde actually does rather than whatever a regex believed.  The
/// rule-type lists come from the enum itself.
///
/// The emitted lists are sorted, not in declaration order: serde_json's
/// Map is a BTreeMap without the preserve_order feature.  That is fine
/// because check_schema_parity compares sets, but do not derive the
/// postcard wire order from this file.  Wire order is declaration order in
/// schema.rs, and the round-trip test in loader.rs is what guards it.
fn schema_facts() -> serde_json::Value {
    let sample = SpellingRule {
        context: Some(String::new()),
        english: Some(String::new()),
        source: Some(String::new()),
        exceptions: Some(Vec::new()),
        context_clues: Some(Vec::new()),
        negative_context_clues: Some(Vec::new()),
        positional_clues: Some(Vec::new()),
        context_suggestions: Some(Vec::new()),
        tags: Some(Vec::new()),
        severity: Some(Severity::Info),
        editorial_confidence: Some(EditorialConfidence::Low),
        structural_guard: Some(String::new()),
        ..SpellingRule::new("x", vec!["y".into()], RuleType::CrossStrait)
    };
    let case = CaseRule {
        term: "X".into(),
        alternatives: Some(Vec::new()),
        disabled: false,
    };

    // Every variant, exhaustively: the match makes a new one fail to compile
    // here rather than silently drop out of the generated lists.
    let all = [
        RuleType::PoliticalColoring,
        RuleType::CrossStrait,
        RuleType::Typo,
        RuleType::Confusable,
        RuleType::Variant,
        RuleType::AiFiller,
        RuleType::Translationese,
    ];
    for rt in all {
        match rt {
            RuleType::PoliticalColoring
            | RuleType::CrossStrait
            | RuleType::Typo
            | RuleType::Confusable
            | RuleType::Variant
            | RuleType::AiFiller
            | RuleType::Translationese => (),
        }
    }

    // Through serde, never a hand-written name: the linter validates values the
    // deserializer produced, so a second spelling here could accept what the
    // loader rejects.
    fn wire_name<T: serde::Serialize>(value: T) -> String {
        serde_json::to_value(value)
            .expect("enum serializes")
            .as_str()
            .expect("string enum")
            .to_string()
    }

    let confidences = [
        EditorialConfidence::High,
        EditorialConfidence::Medium,
        EditorialConfidence::Low,
    ];
    let severities = [Severity::Info, Severity::Warning, Severity::Error];
    for ec in confidences {
        match ec {
            EditorialConfidence::High | EditorialConfidence::Medium | EditorialConfidence::Low => {}
        }
    }
    for severity in severities {
        match severity {
            Severity::Info | Severity::Warning | Severity::Error => {}
        }
    }

    serde_json::json!({
        "_comment": concat!(
            "Generated from the Rust types by schema_facts_file_is_current in ",
            "src/rules/ruleset.rs. Do not edit; regenerate with ",
            "UPDATE_SCHEMA_FACTS=1 cargo test schema_facts_file_is_current."
        ),
        "spelling_fields": keys(&sample),
        "structural_guards": KNOWN_STRUCTURAL_GUARDS,
        "case_fields": keys(&case),
        "rule_types": all.iter().map(wire_name).collect::<Vec<_>>(),
        "orthographic_rule_types": all
            .iter()
            .filter(|rt| rt.is_orthographic())
            .map(wire_name)
            .collect::<Vec<_>>(),
        "editorial_confidence": confidences.iter().map(wire_name).collect::<Vec<_>>(),
        "severities": severities.iter().map(wire_name).collect::<Vec<_>>(),
    })
}

/// JSON keys of a value, in serialization order.
fn keys<T: Serialize>(v: &T) -> Vec<String> {
    serde_json::to_value(v)
        .expect("serializes")
        .as_object()
        .expect("struct is an object")
        .keys()
        .cloned()
        .collect()
}
