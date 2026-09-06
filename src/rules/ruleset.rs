use std::sync::Arc;

use serde::{Deserialize, Serialize};

// The postcard wire-format types live in one file that build.rs include!s, so
// the serializer and the deserializer cannot drift. Re-exported here so every
// existing crate::rules::ruleset::* path keeps resolving.
pub use super::schema::{
    CaseRule, ContextSuggestion, EditorialConfidence, RuleType, Ruleset, SpellingRule,
    KNOWN_STRUCTURAL_GUARDS,
};

/// Linting profile controlling zh-TW norm enforcement strictness.
///
/// Two profiles on the strictness axis:
/// - Base: cross-strait vocabulary, political coloring, casing, basic
///   punctuation, grammar. No character variant normalization.
/// - Strict: full Ministry of Education enforcement including character
///   variant normalization (裏→裡, 台→臺).
///
/// Orthogonal capabilities (applied on top of any profile):
/// - `relaxed`: disables colon enforcement, dunhao detection, grammar
///   checks, and uses en-dash for ranges. For software UI strings.
/// - `detect_ai`: enables AI writing artifact detection (filler phrases,
///   semantic safety words, copula/passive checks, density patterns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Base,
    Strict,
}

/// What kind of sourcing the document is held to, for the one detector that
/// asks: an unsupported authority attribution.
///
/// Casual prose can drop the appeal; technical and financial prose must
/// preserve the claim and name its source. It never suppresses a finding and
/// never changes anything else.
///
/// Not to be confused with [`Register`], which this used to call itself. The
/// two answer different questions and are not interchangeable. This one is a
/// claim the caller makes about the subject matter, and it selects advice.
/// `Register` is a property of the prose, read off the text, and it decides
/// which detectors stay quiet. They are also not the same axis: a 公文 is
/// formal whatever it is about, and a casual blog post about tax law is not.
///
/// The CLI flag stays `--document-genre` and the MCP parameter stays
/// `document_genre`; only the Rust type carries the sharper name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionGenre {
    Casual,
    Technical,
    Financial,
}

impl AttributionGenre {
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "casual" => Some(Self::Casual),
            "technical" => Some(Self::Technical),
            "financial" => Some(Self::Financial),
            _ => None,
        }
    }
}

/// The register a document is actually written in, as the scan resolved it.
///
/// Distinct from `AttributionGenre`, which is a claim the caller makes about
/// the
/// subject matter and governs what to advise about an unsourced attribution.
/// This one is a property of the prose: a 公文 opens 敬啟者 and closes 謹啟
/// whatever it is about, and the forms a detector should stop objecting to are
/// the ones that register mandates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Register {
    Formal,
    Casual,
}

/// What the caller asked for, which is what a batch-wide config can carry.
///
/// `Auto` is the default and resolves per document, because the register is a
/// property of the text and one `ProfileConfig` serves a whole batch of files.
/// The two explicit values are the caller's recourse when the heuristic reads
/// a document wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterMode {
    Auto,
    Formal,
    Casual,
}

impl RegisterMode {
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "formal" => Some(Self::Formal),
            "casual" => Some(Self::Casual),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Formal => "formal",
            Self::Casual => "casual",
        }
    }

    /// The register to scan `text` with: the caller's answer when they gave
    /// one, the detector's otherwise.
    pub fn resolve(self, text: &str) -> Register {
        match self {
            Self::Auto => crate::engine::register::detect_register(text),
            Self::Formal => Register::Formal,
            Self::Casual => Register::Casual,
        }
    }
}

/// Processing chain configuration for a profile.
///
/// Each profile is a combination of enabled rule stages rather than a
/// subset of rules. More specific profiles (strict) add extra stages;
/// they do not replace earlier ones.
#[derive(Debug, Clone, Copy)]
pub struct ProfileConfig {
    /// Register governing advice for unsupported authority attributions.
    pub document_genre: AttributionGenre,
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
    /// Enable translationese (翻譯腔 / 歐化) detection: lexical patterns
    /// from the dewesternise checklist.  Orthogonal to `ai_filler_detection`:
    /// a translated manual is 歐化 but not AI-generated.
    pub translationese_detection: bool,
    /// Domain calibration for translationese scoring thresholds.
    /// `General` uses balanced thresholds; `Technical`/`Literary`/`News`
    /// shift the per-signal thresholds to match domain norms.
    pub translationese_domain: crate::engine::translationese_score::TranslationeseDomain,
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
    /// When true (default for Markdown content), boost severity by one level
    /// for issues whose span is fully contained inside a heading.  Headings
    /// are higher-visibility than body prose and warrant stricter treatment.
    pub heading_severity_boost: bool,
    /// Political stance sub-profile. Controls which PoliticalColoring rules
    /// fire.
    pub political_stance: PoliticalStance,
    /// When true, skip line/col computation (byte offsets only).
    /// Used by MCP tool which consumes offsets directly.
    pub offset_only: bool,
    /// When true (Markdown content only), exclude pulldown-cmark
    /// `Tag::BlockQuote` ranges from scanning.  Off by default, adopted
    /// blockquote prose is real content.  Opt-in via `--exempt-blockquotes`
    /// or `[markdown] exempt_blockquotes = true`.
    pub exempt_blockquotes: bool,
    /// Register policy. `Auto` (the default and, for now, the only value any
    /// caller sets) resolves per document from the text; the explicit values
    /// wait on a flag to select them.
    pub register: RegisterMode,
    /// Enable the rhythm (氣口) advisory axis: over-long sentences,
    /// sentence-ending monotony, and a relaxed 定語堆疊 gate.  Off by
    /// default and never fixable, because rhythm is taste.  Composes with
    /// any profile rather than being one, so a strict run can ask for it.
    pub rhythm: bool,
}

impl ProfileConfig {
    /// Return a copy with the political stance overridden.
    pub fn with_stance(mut self, stance: PoliticalStance) -> Self {
        self.political_stance = stance;
        self
    }

    /// Apply the 'relaxed' capability: disable colon enforcement, dunhao
    /// detection, and grammar checks; use en-dash for ranges. Designed for
    /// software UI strings where strict punctuation rules are too noisy.
    pub fn with_relaxed(mut self) -> Self {
        self.colon_enforcement = false;
        self.dunhao_detection = false;
        self.grammar_checks = false;
        self.range_en_dash = true;
        self
    }

    /// Override the register policy, which is `Auto` unless a caller says so.
    pub fn with_register(mut self, mode: RegisterMode) -> Self {
        self.register = mode;
        self
    }

    /// Apply the 'rhythm' capability: enable the advisory rhythm axis.
    pub fn with_rhythm(mut self, on: bool) -> Self {
        self.rhythm = on;
        self
    }

    /// Mark blockquote prose as excluded from scanning.  Useful when a
    /// document contains long mainland-Chinese citations the author
    /// cannot rewrite.
    pub fn with_exempt_blockquotes(mut self, on: bool) -> Self {
        self.exempt_blockquotes = on;
        self
    }
}

impl Profile {
    /// All defined profiles.
    pub const ALL: &'static [Profile] = &[Profile::Base, Profile::Strict];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Base => "base",
            Profile::Strict => "strict",
        }
    }

    /// Short description.
    pub fn description(self) -> &'static str {
        match self {
            Profile::Base => "Base zh-TW rules: cross-strait vocabulary, political coloring, casing, basic punctuation, grammar",
            Profile::Strict => "Full MoE enforcement: all punctuation, character variants, 臺 normalization, grammar",
        }
    }

    /// Processing chain stages enabled by this profile.
    pub fn config(self) -> ProfileConfig {
        match self {
            Profile::Base => ProfileConfig {
                document_genre: AttributionGenre::Casual,
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
                translationese_detection: true,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                translationese_domain:
                    crate::engine::translationese_score::TranslationeseDomain::General,
                heading_severity_boost: true,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
                exempt_blockquotes: false,
                register: RegisterMode::Auto,
                rhythm: false,
            },
            Profile::Strict => ProfileConfig {
                document_genre: AttributionGenre::Casual,
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
                ai_filler_detection: true,
                translationese_detection: true,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                translationese_domain:
                    crate::engine::translationese_score::TranslationeseDomain::General,
                heading_severity_boost: true,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
                exempt_blockquotes: false,
                register: RegisterMode::Auto,
                rhythm: false,
            },
        }
    }

    /// Strict parse from string. Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "base" => Some(Profile::Base),
            "strict" => Some(Profile::Strict),
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

    /// Whether a specific political_coloring rule should fire under this
    /// stance.
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

/// Tier 2 disambiguation outcome stored on each Issue.
/// Used by Tier 3 (sampling) to determine eligibility without
/// fragile context-string parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier2Outcome {
    /// Not processed by Tier 2 (deterministic types, no english/clues).
    #[default]
    NotEligible,
    /// Resolved locally by Tier 2 (hard anchor, collocation, clue, prior).
    Resolved,
    /// Suppressed by Tier 2 (score below ambiguous threshold).
    Suppressed,
    /// Gray zone: forwarded to Tier 3 for LLM judgment.
    GrayZone,
}

/// Which tier authored the resolution of an issue.
/// Injected per-issue in JSON output when `include_stats` is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionTier {
    /// Pure rule match: no disambiguation needed (punctuation, case,
    /// variant, grammar, unambiguous spelling).
    Deterministic,
    /// Resolved by Tier 2 local heuristics (context clues, profile
    /// priors, collocations, combined evidence).
    Heuristic,
    /// Resolved by Tier 3 LLM sampling or judgment cache.
    LlmJudged,
    /// Not conclusively resolved: suppressed as likely FP, skipped
    /// by budget exhaustion, or left in gray zone without LLM.
    Unresolved,
}

impl ResolutionTier {
    /// Derive the resolution tier from the issue's tier2_outcome and
    /// whether LLM sampling produced a judgment (indicated by context
    /// annotation).
    pub fn classify(issue: &Issue) -> Self {
        match issue.tier2_outcome {
            Tier2Outcome::NotEligible => ResolutionTier::Deterministic,
            Tier2Outcome::Resolved => ResolutionTier::Heuristic,
            Tier2Outcome::Suppressed => ResolutionTier::Unresolved,
            Tier2Outcome::GrayZone => {
                if issue.llm_judged {
                    ResolutionTier::LlmJudged
                } else {
                    ResolutionTier::Unresolved
                }
            }
        }
    }
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
    /// True when issues from this rule type land in the fixer's orthographic
    /// tier.  Delegates to `IssueType::is_orthographic` through the same
    /// mapping the scanner uses, so the two cannot disagree.
    pub fn is_orthographic(self) -> bool {
        IssueType::from(self).is_orthographic()
    }

    pub fn default_severity(self) -> Severity {
        match self {
            RuleType::PoliticalColoring | RuleType::Typo => Severity::Error,
            RuleType::CrossStrait | RuleType::Confusable | RuleType::Variant => Severity::Warning,
            RuleType::AiFiller | RuleType::Translationese => Severity::Info,
        }
    }
}

impl SpellingRule {
    /// True when this rule is an AiFiller deletion (`to: [""]`): the matched
    /// phrase should be removed entirely, with the empty string as the fix.
    pub fn is_deletion_rule(&self) -> bool {
        self.rule_type == RuleType::AiFiller && self.to.len() == 1 && self.to[0].is_empty()
    }

    /// True when this rule's own `to` is the deletion sentinel, whatever its
    /// type: empty, or leading with an empty string.
    ///
    /// Wider than [`Self::is_deletion_rule`], which also requires `AiFiller`.
    /// Inflation derives the reported span from this shape, and the group
    /// compiler refuses `context_suggestions` on it for that reason, so the
    /// two have to agree; naming it once is what makes them.
    pub fn has_deletion_sentinel(&self) -> bool {
        self.to.first().is_some_and(|t| t.is_empty()) || self.to.is_empty()
    }

    /// Create a spelling rule with required fields; optional fields default to
    /// None.  Combine with struct-update syntax to set one of them:
    ///
    /// ```ignore
    /// SpellingRule {
    ///     exceptions: Some(vec!["下著".into()]),
    ///     ..SpellingRule::new("著", vec!["着".into()], RuleType::Variant)
    /// }
    /// ```
    ///
    /// Not `#[cfg(test)]`, though tests are the main caller: that gate made it
    /// invisible to the integration tests in `tests/`, which link the library
    /// built without `cfg(test)`, so those files spelled out all thirteen
    /// fields and every new optional field cost an edit in each of them.
    pub fn new(from: impl Into<String>, to: Vec<String>, rule_type: RuleType) -> Self {
        Self {
            from: from.into(),
            to,
            rule_type,
            disabled: false,
            context: None,
            english: None,
            source: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            context_suggestions: None,
            tags: None,
            editorial_confidence: None,
            structural_guard: None,
        }
    }
}

/// Which structural detector produced a finding.
///
/// The document score weighs how many distinct detectors fired, not how many
/// times, so it needs the detector's identity. Reading that back out of the
/// display context does not work: several messages open with a runtime count,
/// so dash overuse and paragraph endings both reduced to the paragraph total
/// and merged or split by document length. It is also the pattern this crate
/// removed for the translationese phases, for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StructuralFamily {
    BinaryContrast,
    ParagraphEndings,
    DashOveruse,
    FormulaicHeadings,
    ListDensity,
    Tricolon,
    NegativeParallel,
    FormulaicClosing,
    SignificanceStamp,
    EraOpener,
    FourCharBulletLabels,
    MechanicalBullets,
    BoldInSentence,
    BoldInParagraph,
    AbstractLineMetaphor,
    RepeatedSlogan,
    RhetoricalSelfQa,
    EmDashOveruse,
    FormulaicDespite,
    FalseRanges,
    MixedReaderAddress,
    StackedPoliteness,
}

impl StructuralFamily {
    /// Whether the finding is evidence about who wrote the document.
    ///
    /// Most of these families are. Two are not: a Taiwanese procedure that
    /// opens every step with 請 is the standard house form, and mixing 你 with
    /// 您 is an editing slip a human makes as readily as a model. They are
    /// reported because they are defects, and they carry a family so the
    /// per-occurrence density signal skips them, but they add nothing to the
    /// authorship score.
    pub fn is_authorship_evidence(self) -> bool {
        !matches!(self, Self::MixedReaderAddress | Self::StackedPoliteness)
    }

    /// Whether the detector measures layout rather than writing.
    ///
    /// Bold runs, list shape and heading form describe the Markdown house
    /// style. They are capped apart from the prose families so that a heavily
    /// formatted document does not outvote what it actually says.
    pub fn is_formatting(self) -> bool {
        matches!(
            self,
            Self::FormulaicHeadings
                | Self::ListDensity
                | Self::FourCharBulletLabels
                | Self::MechanicalBullets
                | Self::BoldInSentence
                | Self::BoldInParagraph
        )
    }
}

/// A translationese detector family. Several exist in two passes that can
/// report the same span, and the overlap pass keeps one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseFamily {
    /// 之一 superlative calque.
    YiZhi,
    /// Redundant paired connective.
    Connective,
    /// Nominalization chain.
    Nominalization,
    /// False-friend adverbial.
    FalseFriend,
    /// Stacked pre-modifier.
    LongPremodifier,
    /// Rhythm (氣口): a sentence that runs on without a breath.
    RhythmLongSentence,
    /// Rhythm (氣口): consecutive sentences closing on the same particle.
    RhythmMonotony,
}

impl PhaseFamily {
    /// Whether the finding comes from an opt-in advisory axis: excluded from
    /// calibrated scores, and left alone by every fix tier.
    ///
    /// Rhythm findings carry `IssueType::Translationese` because that is what
    /// they are, and the triage list should show them beside ZY5. The property
    /// a caller needs is not that they are rhythm, though, but that a flag the
    /// user opted into must not move a number calibrated without it.
    pub fn is_advisory(self) -> bool {
        matches!(self, Self::RhythmLongSentence | Self::RhythmMonotony)
    }
}

/// Which pass of a paired detector produced a finding. The indexed pass knows
/// sentence and paragraph boundaries; the lexical one only matches substrings,
/// so it yields when both cover the same span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhasePass {
    Lexical,
    Indexed,
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
    /// Suggested replacements.  Arc avoids per-issue allocation during
    /// inflate: most issues share suggestions with their source rule.
    pub suggestions: Arc<[String]>,
    /// Manual rewrite hint for a style rewrite. AI-style hints are present
    /// only for one determined non-empty replacement; competing alternatives
    /// need an AI (or human) to rewrite the surrounding clause. Translationese
    /// retains its ranked first candidate because disambiguation can promote a
    /// context-selected candidate to that position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_rewrite: Option<String>,
    /// Classification of the triggering rule.
    pub rule_type: IssueType,
    /// Severity level.
    pub severity: Severity,
    /// Usage context from the triggering rule, helping the AI agent
    /// choose the right suggestion or understand the nuance.
    /// Arc-interned during inflation to avoid per-issue String clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Arc<str>>,
    /// English original term: unambiguous anchor for cross-strait terms.
    /// Arc-interned during inflation to avoid per-issue String clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<Arc<str>>,
    /// Context clues from the triggering rule. Fixer uses these with a
    /// segmenter to decide whether an ambiguous term should be corrected.
    /// Arc-interned during inflation to avoid per-issue Vec clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_clues: Option<Arc<[String]>>,
    /// Calibration result from translation verification.
    /// `Some(true)`: anchor found in translation (confirmed).
    /// `Some(false)`: anchor absent in translation (unconfirmed).
    /// `None`: calibration not attempted or API failure (no signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_match: Option<bool>,
    /// Internal flag for project-glossary banned-term precedence.
    /// When true, TM must not downgrade the issue, but the marker
    /// stays out of user-facing `context` metadata.
    #[serde(skip)]
    pub glossary_banned: bool,
    /// Tier 2 disambiguation outcome.  Set by `disambiguate_batch` to
    /// indicate whether the issue was resolved locally, suppressed, or
    /// left in the gray zone for Tier 3.  Internal: not serialized.
    #[serde(skip)]
    pub tier2_outcome: Tier2Outcome,
    /// Which translationese detector produced this, and which pass it ran in.
    ///
    /// The overlap pass needs to know that the lexical and the
    /// boundary-aware halves of a pair are the same finding. It used to learn
    /// that by searching the display context for a scheme code such as
    /// "ZY1b", which made a human-readable string load-bearing: rewording the
    /// message silently changed which issues were deduplicated. Internal, not
    /// serialized.
    #[serde(skip)]
    pub phase_family: Option<(PhaseFamily, PhasePass)>,
    /// Which structural AI detector produced this. Internal, not serialized.
    #[serde(skip)]
    pub structural_family: Option<StructuralFamily>,
    /// Whether Tier 3 LLM sampling produced a judgment for this issue.
    /// Set by `refine_issues_with_sampling` (or judgment cache hit).
    /// Used by `ResolutionTier::classify` to distinguish LLM-judged from
    /// unresolved gray-zone issues without fragile string parsing.
    #[serde(skip)]
    pub llm_judged: bool,
    /// Internal: deferred spelling rule index for lazy issue inflation.
    /// When Some, suggestions/context/english/context_clues are empty
    /// placeholders that must be inflated from the compiled DB after
    /// overlap resolution.
    #[serde(skip)]
    pub(crate) spelling_rule_idx: Option<usize>,
    /// Markdown table cell coordinates `(row, column)` (0-based) when the
    /// issue falls inside a Markdown table cell.  Useful for editor
    /// integrations and SARIF region output.  `None` when not in a table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_cell: Option<TableCell>,
    /// Per-issue editorial confidence.  Copied from the source
    /// `SpellingRule` during inflation; surfaces in MCP explain output
    /// via `derive_explain_meta`.  `None` means heuristic derivation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editorial_confidence: Option<EditorialConfidence>,
}

/// Markdown table cell coordinates: `(row, column)` are 0-based, with row 0
/// being the header row (or row 1 if the table has no separator).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
}

impl Issue {
    /// Derive a manual rewrite hint without silently choosing among
    /// alternatives. AI-style findings are often handed to an assistant for a
    /// clause-level rewrite, where a single replacement is useful anchor text
    /// but the first item in an alternatives list is not. Translationese
    /// keeps its ranked first candidate: local or LLM disambiguation promotes
    /// the selected contextual form to index zero before this is refreshed.
    ///
    /// This is presentation metadata for the explain output. It does not
    /// govern whether the fixer applies anything: that is decided separately
    /// by the tier logic in "fixer::fix_verdict", which today applies any
    /// issue carrying exactly one suggestion. A detector that must never be
    /// applied mechanically has to emit no suggestion at all.
    pub fn derive_suggested_rewrite(
        rule_type: IssueType,
        suggestions: &[String],
    ) -> Option<String> {
        match rule_type {
            IssueType::Translationese => suggestions.iter().find(|s| !s.is_empty()).cloned(),
            IssueType::AiStyle => match suggestions {
                [suggestion] if !suggestion.is_empty() => Some(suggestion.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn refresh_suggested_rewrite(&mut self) {
        self.suggested_rewrite = Self::derive_suggested_rewrite(self.rule_type, &self.suggestions);
    }

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
        let suggested_rewrite = Self::derive_suggested_rewrite(rule_type, &suggestions);
        Self {
            offset,
            length,
            line: 0,
            col: 0,
            found: found.into(),
            suggestions: suggestions.into(),
            suggested_rewrite,
            rule_type,
            severity,
            phase_family: None,
            structural_family: None,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            glossary_banned: false,
            tier2_outcome: Tier2Outcome::NotEligible,
            llm_judged: false,
            spelling_rule_idx: None,
            table_cell: None,
            editorial_confidence: None,
        }
    }

    /// Lightweight constructor for deferred spelling issues.
    ///
    /// Skips the `found` and `suggestions` allocations: those are filled
    /// during inflation after overlap resolution.  Uses a static empty
    /// Arc to avoid per-issue heap allocation.
    pub(crate) fn deferred_spelling(
        offset: usize,
        length: usize,
        rule_type: IssueType,
        severity: Severity,
        rule_idx: usize,
    ) -> Self {
        static EMPTY_SUGGESTIONS: std::sync::OnceLock<Arc<[String]>> = std::sync::OnceLock::new();
        Self {
            offset,
            length,
            line: 0,
            col: 0,
            found: String::new(),
            suggestions: EMPTY_SUGGESTIONS
                .get_or_init(|| Arc::from(Vec::<String>::new()))
                .clone(),
            suggested_rewrite: None,
            rule_type,
            severity,
            phase_family: None,
            structural_family: None,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            glossary_banned: false,
            tier2_outcome: Tier2Outcome::NotEligible,
            llm_judged: false,
            spelling_rule_idx: Some(rule_idx),
            table_cell: None,
            editorial_confidence: None,
        }
    }

    /// Builder: attach context string.
    /// Tag the finding with the paired detector it came from, so the overlap
    /// pass can recognise the two halves without reading the message.
    pub(crate) fn with_phase_family(mut self, family: PhaseFamily, pass: PhasePass) -> Self {
        self.phase_family = Some((family, pass));
        self
    }

    /// Record which structural detector produced this finding.
    pub(crate) fn with_structural_family(mut self, family: StructuralFamily) -> Self {
        self.structural_family = Some(family);
        self
    }

    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context = Some(Arc::from(ctx.into()));
        self
    }

    /// Builder: attach english anchor.
    pub fn with_english(mut self, eng: impl Into<String>) -> Self {
        self.english = Some(Arc::from(eng.into()));
        self
    }

    /// Builder: attach context clues.
    pub fn with_context_clues(mut self, clues: Vec<String>) -> Self {
        self.context_clues = Some(Arc::from(clues));
        self
    }
}

/// Issue classification: covers spelling, case, punctuation, grammar, and AI
/// style checks.
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
    /// fixes: requires lexical_contextual or none.
    AiStyle,
    /// Consecutive duplicate word or character (e.g. '去去', 'cache cache').
    Repetition,
    /// Translationese (翻譯腔 / 歐化): Europeanized Chinese syntax/vocabulary.
    /// Orthogonal to AiStyle: separate score, separate CLI/MCP surface.
    Translationese,
}

impl IssueType {
    /// True for issue types the fixer treats as mechanical: applied at every
    /// `--fix` tier, taking the first suggestion without the single-candidate
    /// check that keeps judgment calls out of the write path.
    ///
    /// The definition lives here rather than inline in the fixer because three
    /// places need the same answer and were drifting: the fixer's tier gate,
    /// the compile step that refuses `context_suggestions` on orthographic
    /// rules, and `scripts/check-ruleset.py`.  A rule type added to one and not
    /// the others quietly auto-applies a term nobody meant to auto-apply.
    pub fn is_orthographic(self) -> bool {
        matches!(
            self,
            IssueType::Punctuation | IssueType::Case | IssueType::Variant | IssueType::Grammar
        )
    }

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
            IssueType::Repetition => 9,
            IssueType::Translationese => 10,
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
            IssueType::Repetition => "repetition",
            IssueType::Translationese => "translationese",
        }
    }
}

/// Text shown for a rule that deletes its match rather than replacing it.
pub const DELETE_SUGGESTION: &str = "(delete)";

/// True when a suggestion list means "delete the match": exactly one entry,
/// and that entry empty. Every output format has to special-case this, so the
/// predicate lives next to the issue rather than in each formatter.
pub fn is_delete_suggestion(suggestions: &[String]) -> bool {
    suggestions.len() == 1 && suggestions[0].is_empty()
}

impl Issue {
    /// Compact suggestion string: first suggestion only, `+N` suffix for
    /// alternatives.
    /// Falls back to `english` field when no suggestions exist.
    pub fn compact_suggestion(&self) -> String {
        if self.suggestions.is_empty() {
            self.english.as_deref().unwrap_or("?").to_string()
        } else if is_delete_suggestion(&self.suggestions) {
            DELETE_SUGGESTION.to_string()
        } else if self.suggestions.len() == 1 {
            self.suggestions[0].clone()
        } else {
            format!("{}+{}", self.suggestions[0], self.suggestions.len() - 1)
        }
    }

    /// Grouping key for deduplication in compact output.
    /// Issues with identical (found, rule_type, suggestions, severity) are
    /// collapsible.
    /// Uses full suggestion list (joined) rather than compact display form to
    /// avoid
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
            RuleType::Translationese => IssueType::Translationese,
            RuleType::Typo => IssueType::Typo,
            RuleType::Confusable => IssueType::Confusable,
            RuleType::Variant => IssueType::Variant,
            RuleType::AiFiller => IssueType::AiStyle,
        }
    }
}

#[cfg(test)]
mod delete_suggestion_tests {
    use super::*;

    fn sugs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn only_a_lone_empty_suggestion_means_delete() {
        assert!(is_delete_suggestion(&sugs(&[""])));
        // An empty list means "no suggestion", not "delete".
        assert!(!is_delete_suggestion(&[]));
        assert!(!is_delete_suggestion(&sugs(&["軟體"])));
        // Two entries, one empty, is an alternatives list.
        assert!(!is_delete_suggestion(&sugs(&["", "軟體"])));
    }

    #[test]
    fn compact_suggestion_renders_the_delete_sentinel() {
        let issue = Issue::new(
            0,
            3,
            "\u{200B}",
            sugs(&[""]),
            IssueType::AiStyle,
            Severity::Info,
        );
        assert_eq!(issue.compact_suggestion(), DELETE_SUGGESTION);
    }
}

#[cfg(test)]
mod suggested_rewrite_tests {
    use super::*;

    #[test]
    fn style_rewrite_hint_requires_one_determined_replacement() {
        assert_eq!(
            Issue::derive_suggested_rewrite(IssueType::AiStyle, &["廣泛使用".into()]),
            Some("廣泛使用".into())
        );
        assert_eq!(
            Issue::derive_suggested_rewrite(IssueType::AiStyle, &["藉由".into(), "經由".into()]),
            None,
            "a rewrite assistant must not be handed an arbitrary first alternative"
        );
        assert_eq!(
            Issue::derive_suggested_rewrite(IssueType::AiStyle, &["".into()]),
            None,
            "a deletion is an instruction, not replacement anchor text"
        );
    }
}

#[cfg(test)]
mod schema_facts_tests {
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
        let current =
            serde_json::to_string_pretty(&schema_facts()).expect("serialize facts") + "\n";

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
            editorial_confidence: Some(EditorialConfidence::Low),
            structural_guard: Some(String::new()),
            ..SpellingRule::new("x", vec!["y".into()], RuleType::CrossStrait)
        };
        let case = CaseRule {
            term: "X".into(),
            alternatives: Some(Vec::new()),
            disabled: false,
        };

        // Every variant, exhaustively: the match makes a new one fail to
        // compile here rather than silently drop out of the generated lists.
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
        let name = |rt: RuleType| {
            serde_json::to_value(rt)
                .expect("RuleType serializes")
                .as_str()
                .expect("string enum")
                .to_string()
        };

        let confidences = [
            EditorialConfidence::High,
            EditorialConfidence::Medium,
            EditorialConfidence::Low,
        ];
        for ec in confidences {
            match ec {
                EditorialConfidence::High
                | EditorialConfidence::Medium
                | EditorialConfidence::Low => (),
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
            "rule_types": all.iter().map(|rt| name(*rt)).collect::<Vec<_>>(),
            "orthographic_rule_types": all
                .iter()
                .filter(|rt| rt.is_orthographic())
                .map(|rt| name(*rt))
                .collect::<Vec<_>>(),
            "editorial_confidence": confidences
                .iter()
                .map(|ec| {
                    serde_json::to_value(ec)
                        .expect("EditorialConfidence serializes")
                        .as_str()
                        .expect("string enum")
                        .to_string()
                })
                .collect::<Vec<_>>(),
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
}
