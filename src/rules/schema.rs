// The ruleset wire format: the exact types build.rs serializes to postcard and
// the runtime deserializes back.
//
// This file is the single definition of that format. build.rs pulls it in with
// include! rather than keeping its own copy, because postcard is not
// self-describing: field order and field count ARE the encoding. A field added
// to one copy and not the other, or added in a different position, silently
// corrupts every rule in the blob rather than failing to compile. Two copies
// had already drifted that way once, with editorial_confidence unasserted in
// the parity test for several releases.
//
// Three rules keep the include! working:
//
//   - Definitions only.  No impl blocks, no use of anything outside serde.
//     Inherent impls live in ruleset.rs, which is a normal module and can
//     reference Severity and the rest of the crate.
//   - No attribute that makes Serialize and Deserialize disagree.
//     skip_serializing_if is the one that already bit us: it would make
//     build.rs omit None fields from the postcard stream while the
//     deserializer still expects them, shifting every following field.  The
//     cost of dropping it is cosmetic, since the only runtime serializer of
//     these types is OverrideStore writing overrides.json, which now emits
//     explicit nulls for absent fields.  scripts/check-ruleset.py --lint
//     rejects that attribute and its relatives rather than trusting this
//     comment.
//   - Field order is the wire format within a build.  Both sides move together
//     now, and the blob is regenerated from assets/ruleset.json on every build
//     and include_bytes!d, never read from disk, so there are no older blobs to
//     stay compatible with.  Order still has to be internally consistent, which
//     it is by construction.
//
// ruleset.rs re-exports everything here, so crate::rules::ruleset::SpellingRule
// keeps resolving.

use serde::{Deserialize, Serialize};

/// Severity assigned to an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Rule types for spelling/terminology rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleType {
    /// Mainland China political coloring
    PoliticalColoring,
    /// Cross-strait usage difference
    CrossStrait,
    /// Typo / spelling correction
    Typo,
    /// Confusable term
    Confusable,
    /// Character variant: MoE standard form differs from non-standard glyph
    /// (e.g. 裏->裡, 綫->線). Curated from OpenCC TWVariants.txt.
    Variant,
    /// AI filler phrase: zero-information hedging/emphasis inserted by LLMs.
    /// Fixed-string AC matches; deletions or simple substitutions.
    AiFiller,
    /// Translationese (翻譯腔 / 歐化): Europeanized Chinese syntax and
    /// vocabulary.  Orthogonal to AI detection -- a translated manual is
    /// 歐化 but not AI-generated.  Sourced from the dewesternise checklist
    /// (余光中《論中文之西化》 and related literature).
    Translationese,
}

/// Structural guards the engine implements.
///
/// A rule naming one of these is owned by a procedural detector rather than by
/// the lexical pass. `scripts/check-ruleset.py` validates `structural_guard`
/// against this list, so a typo fails the ruleset lint instead of silently
/// removing a rule from both paths.
pub const KNOWN_STRUCTURAL_GUARDS: &[&str] = &["uncited_attribution"];

/// A spelling/terminology rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpellingRule {
    /// The term to match (source form to be flagged).
    pub from: String,
    /// One or more replacement suggestions (target forms).
    pub to: Vec<String>,
    /// Classification of this rule.
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    /// Optional per-rule severity. When omitted, the rule type's established
    /// default applies. This lets a character-form preference remain advisory
    /// without weakening other rules that share the Variant type.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// If true, this rule is disabled and will not be used for scanning.
    #[serde(default)]
    pub disabled: bool,
    /// Usage context that helps the AI agent pick the right suggestion
    /// when multiple correct forms exist or when the term is ambiguous.
    #[serde(default)]
    pub context: Option<String>,
    /// English original term -- serves as an unambiguous anchor when the
    /// same Chinese term means different things across the strait.
    /// E.g. 並行 = concurrency (TW) vs parallelism (CN).
    #[serde(default)]
    pub english: Option<String>,
    /// Evidence for adding this rule: either a fixture path under "tests/" or
    /// the id of a corpus case that demonstrates the regression. Existing
    /// embedded rules predate this field; the ruleset linter requires it only
    /// for rules newly added relative to HEAD.
    #[serde(default)]
    pub source: Option<String>,
    /// Exception phrases where the matched form should not be flagged.
    /// Applies to all rule types (variant, cross_strait, typo, confusable).
    /// E.g. chess term 下著 keeps 着; 分類 keeps 類 from firing as a class
    /// warning.  An empty or absent list means no exceptions.
    #[serde(default)]
    pub exceptions: Option<Vec<String>>,
    /// Surrounding words that suggest the intended meaning for ambiguous terms.
    /// When present, the fixer uses segmentation to check if these clue words
    /// appear near the match. E.g. 程序 with clues ["編寫", "代碼", "執行"]
    /// suggests the "program" sense rather than "procedure".
    #[serde(default)]
    pub context_clues: Option<Vec<String>>,
    /// Words that, when present in the surrounding window, indicate the term is
    /// being used correctly in context and should NOT be flagged.  Acts as a
    /// veto: if any negative clue matches, the rule is skipped regardless of
    /// positive context_clues.  E.g. 項目 should not fire when 的 or 等
    /// precede it (list-item grammatical usage vs. project/IT usage).
    #[serde(default)]
    pub negative_context_clues: Option<Vec<String>>,
    /// Positional conditions that constrain WHERE a context term must appear
    /// relative to the match.  More expressive than flat context_clues (which
    /// check presence anywhere in +-40-char window).  Syntax:
    ///
    /// - `before:TERM` -- TERM must appear within 20 chars AFTER the match
    /// - `after:TERM` -- TERM must appear within 20 chars BEFORE the match
    /// - `adjacent:TERM` -- TERM must be immediately adjacent (no gap)
    /// - `not_before:TERM` -- TERM must NOT appear within 20 chars after
    /// - `not_after:TERM` -- TERM must NOT appear within 20 chars before
    ///
    /// All positive conditions must pass (AND). Any negative condition vetoes.
    /// When both context_clues and positional_clues are present, both must
    /// match (AND).
    #[serde(default)]
    pub positional_clues: Option<Vec<String>>,
    /// Replacement sets selected by surrounding context.  Groups are tried in
    /// order and the first whose clues appear in the +-40-char window wins,
    /// replacing the rule's default "to" for that match only.
    ///
    /// Exists because one source term can need different corrections in
    /// different domains.  「優化」takes「最佳化」for IT optimize, but where
    /// the text means improve rather than make-optimal the right word is
    /// 「改善」or「提升」, and offering「最佳化」there would be wrong. Encoding
    /// that as a flat multi-entry "to" would instead stop auto-fix in the
    /// primary sense, since the fixer only applies single-suggestion rules.
    ///
    /// A group carrying several "to" entries is deliberately not auto-fixable:
    /// picking between them is a judgment call, so the issue is reported and
    /// left to the author.
    #[serde(default)]
    pub context_suggestions: Option<Vec<ContextSuggestion>>,
    /// Optional tags for categorization and filtering in rule packs.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Per-rule editorial confidence.  Distinguishes binary
    /// corrections from style-preference suggestions.  When set to
    /// `Low`, the explain pipeline marks issues from this rule as
    /// `auto_fix_safe = false` AND `needs_review = true` -- these are
    /// terms whose Mainland/Taiwan distinction is genuine but where
    /// the calque form is also valid zh-TW vocabulary in some senses
    /// (e.g. 場景, 比如).  A term that is simply wrong in zh-TW carries no
    /// annotation, even when its surface form has a valid unrelated sense:
    /// 優化 and 算法 are handled with context_suggestions and exceptions
    /// instead.  Defaults to `None` (heuristic derivation in
    /// `derive_explain_meta`).
    #[serde(default)]
    pub editorial_confidence: Option<EditorialConfidence>,
    /// Names a procedural detector that owns this phrase, instead of letting
    /// the lexical pass emit it.
    ///
    /// Some tells need a guard the schema cannot express. An uncited
    /// attribution is the case this exists for: 研究顯示 is ordinary zh-TW
    /// whenever a citation follows it in the same sentence, and deciding that
    /// means looking for numbered brackets, footnote markers, Markdown links
    /// and bare URLs, honoring the exclusion ranges, and stopping at the
    /// sentence boundary. `negative_context_clues` holds substrings and can
    /// express none of it.
    ///
    /// Before this field the phrases lived in a `const` inside the scanner,
    /// which meant no user override, no `disabled` flag and no provenance
    /// gate, while every other rule had all three. Carrying them here as
    /// ordinary rules restores that, and the guard name tells the lexical pass
    /// to skip them so the detector remains the only thing that can emit one.
    ///
    /// The value must name a guard the engine implements; the ruleset linter
    /// rejects unknown names rather than silently disabling a rule.
    #[serde(default)]
    pub structural_guard: Option<String>,
}

/// A replacement set selected by surrounding context.
///
/// See "SpellingRule::context_suggestions" for why this exists and when a
/// group is auto-fixable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextSuggestion {
    /// Words that select this group when any of them appears in the window.
    pub clues: Vec<String>,
    /// Replacements offered instead of the rule's default "to".
    pub to: Vec<String>,
}

/// Editorial confidence tier surfaced in explain output.
///
/// Per-issue field that distinguishes binary corrections (`線程` ->
/// `執行緒`, high) from editorial-judgment terms (`場景` is correct zh-TW
/// for a stage scene, so rewriting it to `情境` is an IT-context judgment
/// call -- low).  Distinct from `summary_metrics.confidence_distribution`,
/// which tracks resolution-tier confidence across the document.
///
/// Invariant enforced downstream: `Low` => `auto_fix_safe = false`
/// AND "needs_review = true".  Two subsystems enforce it with different
/// scope: the MCP explain path applies it to a derived confidence that
/// falls back to a heuristic, while the fixer applies it only to this
/// explicit annotation and only at or below "lexical_safe", since
/// "--fix=lexical_contextual" deliberately writes these terms.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditorialConfidence {
    High,
    Medium,
    Low,
}

/// A proper noun casing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRule {
    /// The canonical correct casing (e.g. "JavaScript").
    pub term: String,
    /// Other accepted casings (e.g. ["javascript", "JAVASCRIPT"]).
    #[serde(default)]
    pub alternatives: Option<Vec<String>>,
    /// If true, this rule is disabled and will not be used for scanning.
    #[serde(default)]
    pub disabled: bool,
}

/// Top-level ruleset container -- the JSON source format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ruleset {
    pub spelling_rules: Vec<SpellingRule>,
    pub case_rules: Vec<CaseRule>,
}
