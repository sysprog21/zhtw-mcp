// Build script: pre-serialize assets/ruleset.json to postcard binary format.
// At runtime, postcard::from_bytes is ~10x faster than serde_json::from_str.

use std::path::Path;

// Mirror the ruleset types needed for deserialization.
// These must match the runtime types in src/rules/ruleset.rs exactly.
#[derive(serde::Serialize, serde::Deserialize)]
struct Ruleset {
    spelling_rules: Vec<SpellingRule>,
    case_rules: Vec<CaseRule>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SpellingRule {
    from: String,
    to: Vec<String>,
    #[serde(rename = "type")]
    rule_type: RuleType,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    english: Option<String>,
    #[serde(default)]
    exceptions: Option<Vec<String>>,
    #[serde(default)]
    context_clues: Option<Vec<String>>,
    #[serde(default)]
    negative_context_clues: Option<Vec<String>>,
    #[serde(default)]
    positional_clues: Option<Vec<String>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuleType {
    PoliticalColoring,
    CrossStrait,
    Typo,
    Confusable,
    Variant,
    AiFiller,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CaseRule {
    term: String,
    #[serde(default)]
    alternatives: Option<Vec<String>>,
    #[serde(default)]
    disabled: bool,
}

fn main() {
    let ruleset_path = Path::new("assets/ruleset.json");
    println!("cargo:rerun-if-changed={}", ruleset_path.display());

    let json = std::fs::read_to_string(ruleset_path).expect("read assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(&json).expect("parse ruleset.json");

    let bytes = postcard::to_allocvec(&ruleset).expect("postcard serialize");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let out_path = Path::new(&out_dir).join("ruleset.postcard");
    std::fs::write(&out_path, &bytes).expect("write ruleset.postcard");
}
