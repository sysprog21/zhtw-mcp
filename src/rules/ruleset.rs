use serde::{Deserialize, Serialize};

/// Linting profile that controls which rules are active and how strict they are.
///
/// Default is the baseline (current behavior). StrictMoe enables the full
/// Ministry of Education standard (variants, colon, 臺). UiStrings is
/// relaxed for software UI contexts (half-width : allowed). Editorial
/// enables AI writing artifact detection (filler phrases, semantic safety
/// words, copula avoidance, passive voice overuse) on top of base rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Default,
    StrictMoe,
    UiStrings,
    /// Editorial review: base rules + AI writing artifact detection.
    /// Flags discourse-level patterns statistically overrepresented in
    /// LLM-generated zh-TW text (filler phrases, semantic safety words,
    /// inflated copulas, passive voice overuse).
    Editorial,
}

/// Processing chain configuration for a profile.
///
/// Each profile is a combination of enabled rule stages rather than a
/// subset of rules. More specific profiles (strict_moe) add extra stages;
/// they do not replace earlier ones.
#[derive(Debug, Clone, Copy)]
pub struct ProfileConfig {
    /// Enable spelling rules (cross-strait, political, typo, confusable).
    pub spelling: bool,
    /// Enable case rules (proper noun casing).
    pub casing: bool,
    /// Enable basic punctuation: comma, period, !, ?, ;, (, ).
    pub basic_punctuation: bool,
    /// Enable full-width colon enforcement (: -> ：).
    pub colon_enforcement: bool,
    /// Enable enumeration comma (dunhao) detection.
    pub dunhao_detection: bool,
    /// Enable range indicator normalization (~, -).
    pub range_normalization: bool,
    /// Enable character variant normalization (裏->裡, 綫->線).
    pub variant_normalization: bool,
    /// Enable ellipsis normalization: ... → ……, 。。。 → …….
    pub ellipsis_normalization: bool,
    /// Range indicator style: true = en dash (–), false = wave dash (～).
    pub range_en_dash: bool,
    /// Enable grammar checks (interlingual transfer, A-not-A + 嗎 clash).
    pub grammar_checks: bool,
    /// Enable AI filler phrase detection (值得注意的是, 在這種情況下, etc.).
    pub ai_filler_detection: bool,
    /// Enable AI semantic safety word detection (意味著 disambiguation,
    /// copula avoidance, passive voice overuse).
    pub ai_semantic_safety: bool,
    /// Enable density-based AI phrase detection.  Counts tracked phrases
    /// across the full document and flags when density exceeds per-phrase
    /// thresholds (count per thousand characters).
    pub ai_density_detection: bool,
    /// Enable structural AI pattern detection: binary contrast density,
    /// paragraph-ending declarations, dash overuse, formulaic headings.
    pub ai_structural_patterns: bool,
    /// AI detection threshold multiplier: <1.0 = sensitive (catches more),
    /// 1.0 = balanced (default), >1.0 = conservative (fewer false positives).
    /// Maps to ai_threshold levels: low=0.5, medium=1.0, high=1.5.
    pub ai_threshold_multiplier: f32,
    /// Political stance sub-profile. Controls which PoliticalColoring rules fire.
    pub political_stance: PoliticalStance,
    /// When true, skip line/col computation (byte offsets only).
    /// Used by MCP tool which consumes offsets directly.
    pub offset_only: bool,
}

impl ProfileConfig {
    /// Return a copy with the political stance overridden.
    pub fn with_stance(mut self, stance: PoliticalStance) -> Self {
        self.political_stance = stance;
        self
    }
}

impl Profile {
    /// All defined profiles.
    pub const ALL: &'static [Profile] = &[
        Profile::Default,
        Profile::StrictMoe,
        Profile::UiStrings,
        Profile::Editorial,
    ];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Default => "default",
            Profile::StrictMoe => "strict_moe",
            Profile::UiStrings => "ui_strings",
            Profile::Editorial => "editorial",
        }
    }

    /// Short description.
    pub fn description(self) -> &'static str {
        match self {
            Profile::Default => "Base zh-TW rules: cross-strait vocabulary, political coloring, casing, basic punctuation, grammar",
            Profile::StrictMoe => "Full MoE enforcement: all punctuation, character variants, 臺 normalization, grammar",
            Profile::UiStrings => "Relaxed for software UI: half-width colon allowed, en dash for ranges, strict vocabulary, no grammar",
            Profile::Editorial => "AI writing review: base rules + filler phrase detection, semantic safety words, copula/passive checks",
        }
    }

    /// Processing chain stages enabled by this profile.
    pub fn config(self) -> ProfileConfig {
        match self {
            Profile::Default => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: true,
                dunhao_detection: true,
                range_normalization: true,
                variant_normalization: false,
                ellipsis_normalization: true,
                range_en_dash: false,
                grammar_checks: true,
                ai_filler_detection: false,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
            },
            Profile::StrictMoe => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: true,
                dunhao_detection: true,
                range_normalization: true,
                variant_normalization: true,
                ellipsis_normalization: true,
                range_en_dash: false,
                grammar_checks: true,
                ai_filler_detection: false,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
            },
            Profile::UiStrings => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: false,
                dunhao_detection: false,
                range_normalization: true,
                variant_normalization: false,
                ellipsis_normalization: true,
                range_en_dash: true,
                grammar_checks: false,
                ai_filler_detection: false,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
            },
            // Editorial: base rules + all AI writing artifact detection.
            // Targets discourse-level patterns overrepresented in LLM output.
            Profile::Editorial => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: true,
                dunhao_detection: true,
                range_normalization: true,
                variant_normalization: false,
                ellipsis_normalization: true,
                range_en_dash: false,
                grammar_checks: true,
                ai_filler_detection: true,
                ai_semantic_safety: true,
                ai_density_detection: true,
                ai_structural_patterns: true,
                ai_threshold_multiplier: 1.0,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
            },
        }
    }

    /// Parse from string, defaulting to Default on unrecognized input.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "strict_moe" => Profile::StrictMoe,
            "ui_strings" => Profile::UiStrings,
            "editorial" => Profile::Editorial,
            _ => Profile::Default,
        }
    }

    /// Strict parse from string. Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Profile::Default),
            "strict_moe" => Some(Profile::StrictMoe),
            "ui_strings" => Some(Profile::UiStrings),
            "editorial" => Some(Profile::Editorial),
            _ => None,
        }
    }
}

/// Political stance sub-profile controlling which PoliticalColoring rules fire.
///
/// Orthogonal to the main Profile enum. When None (or RocCentric), all
/// political_coloring rules apply (current default). International keeps only
/// organization/entity name normalization (東盟→東協). Neutral suppresses all
/// political_coloring rules entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoliticalStance {
    /// Apply all political_coloring rules (default behavior).
    RocCentric,
    /// Only apply organization/entity name rules; skip identity-loaded terms
    /// (內地, 祖國, 大陸同胞).
    International,
    /// Suppress all political_coloring rules.
    Neutral,
}

impl PoliticalStance {
    /// All defined stances.
    pub const ALL: &'static [PoliticalStance] = &[
        PoliticalStance::RocCentric,
        PoliticalStance::International,
        PoliticalStance::Neutral,
    ];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            PoliticalStance::RocCentric => "roc_centric",
            PoliticalStance::International => "international",
            PoliticalStance::Neutral => "neutral",
        }
    }

    /// Short description.
    pub fn description(self) -> &'static str {
        match self {
            PoliticalStance::RocCentric => {
                "Apply all political/regional normalization rules (default)"
            }
            PoliticalStance::International => {
                "Only normalize international organization names (東盟→東協); skip identity terms"
            }
            PoliticalStance::Neutral => "Suppress all political coloring rules",
        }
    }

    /// Parse from string, defaulting to RocCentric on unrecognized input.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "international" => PoliticalStance::International,
            "neutral" => PoliticalStance::Neutral,
            _ => PoliticalStance::RocCentric,
        }
    }

    /// Strict parse from string. Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "roc_centric" => Some(PoliticalStance::RocCentric),
            "international" => Some(PoliticalStance::International),
            "neutral" => Some(PoliticalStance::Neutral),
            _ => None,
        }
    }

    /// Whether a specific political_coloring rule should fire under this stance.
    ///
    /// Identity-loaded terms (內地, 大陸同胞, 祖國) are suppressed under
    /// International. All terms suppressed under Neutral.
    pub fn allows_rule(self, from: &str) -> bool {
        match self {
            PoliticalStance::RocCentric => true,
            PoliticalStance::Neutral => false,
            PoliticalStance::International => {
                // Suppress identity-loaded terms; keep organization names.
                !matches!(from, "內地" | "大陸同胞" | "祖國")
            }
        }
    }
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// Human-readable lowercase name.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    /// Single-letter severity for compact/grep-style output.
    pub fn letter(self) -> &'static str {
        match self {
            Severity::Info => "I",
            Severity::Warning => "W",
            Severity::Error => "E",
        }
    }
}

impl RuleType {
    pub fn default_severity(self) -> Severity {
        match self {
            RuleType::PoliticalColoring | RuleType::Typo => Severity::Error,
            RuleType::CrossStrait | RuleType::Confusable | RuleType::Variant => Severity::Warning,
            RuleType::AiFiller => Severity::Info,
        }
    }
}

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
    /// If true, this rule is disabled and will not be used for scanning.
    #[serde(default)]
    pub disabled: bool,
    /// Usage context that helps the AI agent pick the right suggestion
    /// when multiple correct forms exist or when the term is ambiguous.
    #[serde(default)]
    pub context: Option<String>,
    /// English original term — serves as an unambiguous anchor when the
    /// same Chinese term means different things across the strait.
    /// E.g. 並行 = concurrency (TW) vs parallelism (CN).
    #[serde(default)]
    pub english: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_context_clues: Option<Vec<String>>,
    /// Positional conditions that constrain WHERE a context term must appear
    /// relative to the match.  More expressive than flat context_clues (which
    /// check presence anywhere in +-40-char window).  Syntax:
    ///
    /// - `before:TERM` — TERM must appear within 20 chars AFTER the match
    /// - `after:TERM` — TERM must appear within 20 chars BEFORE the match
    /// - `adjacent:TERM` — TERM must be immediately adjacent (no gap)
    /// - `not_before:TERM` — TERM must NOT appear within 20 chars after
    /// - `not_after:TERM` — TERM must NOT appear within 20 chars before
    ///
    /// All positive conditions must pass (AND). Any negative condition vetoes.
    /// When both context_clues and positional_clues are present, both must
    /// match (AND).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positional_clues: Option<Vec<String>>,
    /// Optional tags for categorization and filtering in rule packs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl SpellingRule {
    /// True when this rule is an AiFiller deletion (`to: [""]`): the matched
    /// phrase should be removed entirely, with the empty string as the fix.
    pub fn is_deletion_rule(&self) -> bool {
        self.rule_type == RuleType::AiFiller && self.to.len() == 1 && self.to[0].is_empty()
    }

    /// Create a spelling rule with required fields; optional fields default to None.
    #[cfg(test)]
    pub fn new(from: impl Into<String>, to: Vec<String>, rule_type: RuleType) -> Self {
        Self {
            from: from.into(),
            to,
            rule_type,
            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
        }
    }
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

/// Top-level ruleset container — the JSON source format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ruleset {
    pub spelling_rules: Vec<SpellingRule>,
    pub case_rules: Vec<CaseRule>,
}

/// An issue found by the scanner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Byte offset in the original text.
    pub offset: usize,
    /// Byte length of the matched span.
    pub length: usize,
    /// 1-based line number in the original text.
    pub line: usize,
    /// 1-based column number (UTF-16 code units by default, matching LSP spec).
    pub col: usize,
    /// The matched (wrong) text.
    pub found: String,
    /// Suggested replacements.
    pub suggestions: Vec<String>,
    /// Classification of the triggering rule.
    pub rule_type: IssueType,
    /// Severity level.
    pub severity: Severity,
    /// Usage context from the triggering rule, helping the AI agent
    /// choose the right suggestion or understand the nuance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// English original term — unambiguous anchor for cross-strait terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<String>,
    /// Context clues from the triggering rule. Fixer uses these with a
    /// segmenter to decide whether an ambiguous term should be corrected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_clues: Option<Vec<String>>,
    /// Calibration result from translation verification.
    /// `Some(true)`: anchor found in translation (confirmed).
    /// `Some(false)`: anchor absent in translation (unconfirmed).
    /// `None`: calibration not attempted or API failure (no signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_match: Option<bool>,
    /// Internal: deferred spelling rule index for lazy issue inflation.
    /// When Some, suggestions/context/english/context_clues are empty
    /// placeholders that must be inflated from the compiled DB after
    /// overlap resolution.
    #[serde(skip)]
    pub(crate) spelling_rule_idx: Option<usize>,
}

impl Issue {
    /// Construct an issue with all semantic fields; line/col are set to 0
    /// (filled in later by the line-index pass).
    pub fn new(
        offset: usize,
        length: usize,
        found: impl Into<String>,
        suggestions: Vec<String>,
        rule_type: IssueType,
        severity: Severity,
    ) -> Self {
        Self {
            offset,
            length,
            line: 0,
            col: 0,
            found: found.into(),
            suggestions,
            rule_type,
            severity,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            spelling_rule_idx: None,
        }
    }

    /// Builder: attach context string.
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context = Some(ctx.into());
        self
    }

    /// Builder: attach english anchor.
    pub fn with_english(mut self, eng: impl Into<String>) -> Self {
        self.english = Some(eng.into());
        self
    }

    /// Builder: attach context clues.
    pub fn with_context_clues(mut self, clues: Vec<String>) -> Self {
        self.context_clues = Some(clues);
        self
    }
}

/// Issue classification — covers spelling, case, punctuation, grammar, and AI style checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    PoliticalColoring,
    CrossStrait,
    Typo,
    Confusable,
    Case,
    Punctuation,
    Variant,
    Grammar,
    /// AI writing artifact: filler phrases, semantic safety words, copula
    /// avoidance, passive voice overuse.  NOT eligible for orthographic-tier
    /// fixes — requires lexical_contextual or none.
    AiStyle,
}

impl IssueType {
    /// Stable ordering key for deterministic output (used by scan sort).
    pub fn sort_order(self) -> u8 {
        match self {
            IssueType::PoliticalColoring => 0,
            IssueType::CrossStrait => 1,
            IssueType::Typo => 2,
            IssueType::Confusable => 3,
            IssueType::Case => 4,
            IssueType::Punctuation => 5,
            IssueType::Variant => 6,
            IssueType::Grammar => 7,
            IssueType::AiStyle => 8,
        }
    }

    /// Snake_case name matching serde serialization.
    pub fn name(self) -> &'static str {
        match self {
            IssueType::PoliticalColoring => "political_coloring",
            IssueType::CrossStrait => "cross_strait",
            IssueType::Typo => "typo",
            IssueType::Confusable => "confusable",
            IssueType::Case => "case",
            IssueType::Punctuation => "punctuation",
            IssueType::Variant => "variant",
            IssueType::Grammar => "grammar",
            IssueType::AiStyle => "ai_style",
        }
    }
}

impl Issue {
    /// Compact suggestion string: first suggestion only, `+N` suffix for alternatives.
    /// Falls back to `english` field when no suggestions exist.
    pub fn compact_suggestion(&self) -> String {
        if self.suggestions.is_empty() {
            self.english.as_deref().unwrap_or("?").to_string()
        } else if self.suggestions.len() == 1 && self.suggestions[0].is_empty() {
            "(delete)".to_string()
        } else if self.suggestions.len() == 1 {
            self.suggestions[0].clone()
        } else {
            format!("{}+{}", self.suggestions[0], self.suggestions.len() - 1)
        }
    }

    /// Grouping key for deduplication in compact output.
    /// Issues with identical (found, rule_type, suggestions, severity) are collapsible.
    /// Uses full suggestion list (joined) rather than compact display form to avoid
    /// merging issues with different alternative sets.
    pub fn compact_dedup_key(&self) -> (&str, &'static str, String, &'static str) {
        (
            &self.found,
            self.rule_type.name(),
            self.suggestions.join("|"),
            self.severity.letter(),
        )
    }
}

impl From<RuleType> for IssueType {
    fn from(rt: RuleType) -> Self {
        match rt {
            RuleType::PoliticalColoring => IssueType::PoliticalColoring,
            RuleType::CrossStrait => IssueType::CrossStrait,
            RuleType::Typo => IssueType::Typo,
            RuleType::Confusable => IssueType::Confusable,
            RuleType::Variant => IssueType::Variant,
            RuleType::AiFiller => IssueType::AiStyle,
        }
    }
}
