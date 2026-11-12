use anyhow::{Context, Result};

use super::ruleset::Ruleset;

/// Load the embedded ruleset from pre-serialized postcard binary.
/// The binary is generated at build time from assets/ruleset.json by build.rs.
/// Postcard deserialization is ~10x faster than serde_json and zero-alloc for
/// the parse step itself (allocations come from owned String fields).
pub fn load_embedded_ruleset() -> Result<Ruleset> {
    let started = std::time::Instant::now();
    let _span = tracing::info_span!("load_ruleset").entered();
    static RULESET_POSTCARD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ruleset.postcard"));
    let ruleset: Ruleset =
        postcard::from_bytes(RULESET_POSTCARD).context("parse embedded ruleset (postcard)")?;
    tracing::info!(
        spelling_rule_count = ruleset.spelling_rules.len() as u64,
        case_rule_count = ruleset.case_rules.len() as u64,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "load_ruleset completed"
    );
    Ok(ruleset)
}

/// Compute a combined hash of all rules (spelling + case) for reproducibility
/// tracking.
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
#[path = "../../tests/unit/rules/loader/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/unit/rules/loader/register_gate_overlap.rs"]
mod register_gate_overlap;
