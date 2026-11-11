use serde::Deserialize;
use zhtw_mcp::engine::s2t::S2TConverter;
use zhtw_mcp::engine::scan::{build_exclusions_for_content_type, ContentType, Scanner};
use zhtw_mcp::engine::segment::Segmenter;
use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
use zhtw_mcp::rules::ruleset::{Issue, IssueType, Profile, ProfileConfig, Ruleset};

#[derive(Debug, Deserialize)]
struct CorpusSpec {
    id: String,
    label: String,
    profile: String,
    #[serde(default)]
    detect_ai: bool,
    mode: String,
    min_bytes: usize,
    cases: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize)]
struct CorpusCase {
    id: String,
    repeat: usize,
    input: String,
    #[serde(default)]
    scan_text: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    /// Per-case override of the spec's "detect_ai". The native corpus runs
    /// with the AI filter off, which left the AI-only detectors with no
    /// false-positive coverage at all: a detector that fires on ordinary
    /// zh-TW could not move "fp_rate" because it never ran. Cases that name a
    /// specific AI detector set this to measure it.
    #[serde(default)]
    detect_ai: Option<bool>,
    /// A corpus case that documents a human-writing pattern the scanner must
    /// leave alone. Native rates stay aggregate, but these named regressions
    /// fail immediately rather than consuming up to five percent of the budget.
    #[serde(default)]
    must_be_clean: bool,
    expected_fixed: String,
    expected_issues: Vec<ExpectedIssue>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedIssue {
    found: String,
    replace: String,
    rule_type: String,
    #[serde(default = "default_occurrence")]
    occurrence: usize,
}

fn default_occurrence() -> usize {
    1
}

fn case_content_type(case: &CorpusCase) -> ContentType {
    match case.content_type.as_deref() {
        None => ContentType::Plain,
        Some(name) => ContentType::from_name(name)
            .unwrap_or_else(|| panic!("unknown content_type {name:?} in corpus case {}", case.id)),
    }
}

#[derive(Debug, Clone)]
struct ResolvedExpectedIssue {
    offset: usize,
    found: String,
    replace: String,
    rule_type: IssueType,
}

#[derive(Debug, Default, Clone)]
struct ScoreCounts {
    tp: usize,
    fp: usize,
    fn_: usize,
}

#[derive(Debug, Default, Clone)]
struct FixCounts {
    exact_docs: usize,
    total_docs: usize,
}

#[derive(Debug, Default)]
struct NativeCounts {
    /// Repeat-weighted document counts.  These describe the byte volume the
    /// scanner saw, which is what `min_bytes` is about.
    flagged_docs: usize,
    total_docs: usize,
    /// Distinct fixture counts, ignoring `repeat`. Gated alongside the
    /// repeat-weighted pair above:
    /// a case authored with `repeat: 50` must not weigh fifty times more than
    /// one authored with `repeat: 1`, or a real regression on a single sentence
    /// disappears into the denominator.
    flagged_cases: usize,
    total_cases: usize,
    total_fp_issues: usize,
}

fn load_scanner() -> (Scanner, Segmenter) {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    (scanner, segmenter)
}

fn build_config(spec: &CorpusSpec, detect_ai: bool) -> ProfileConfig {
    let profile = Profile::from_str_strict(&spec.profile)
        .unwrap_or_else(|| panic!("unknown profile: {}", spec.profile));
    let mut cfg = profile.config();
    if detect_ai {
        cfg.ai_filler_detection = true;
        cfg.ai_semantic_safety = true;
        cfg.ai_density_detection = true;
        cfg.ai_structural_patterns = true;
    }
    cfg
}

fn parse_issue_type(name: &str) -> IssueType {
    match name {
        "political_coloring" => IssueType::PoliticalColoring,
        "cross_strait" => IssueType::CrossStrait,
        "typo" => IssueType::Typo,
        "confusable" => IssueType::Confusable,
        "case" => IssueType::Case,
        "punctuation" => IssueType::Punctuation,
        "variant" => IssueType::Variant,
        "grammar" => IssueType::Grammar,
        "ai_style" => IssueType::AiStyle,
        "repetition" => IssueType::Repetition,
        "translationese" => IssueType::Translationese,
        _ => panic!("unknown issue type: {name}"),
    }
}

fn nth_offset(text: &str, needle: &str, occurrence: usize) -> Option<usize> {
    assert!(occurrence > 0, "occurrence must be >= 1 (1-based)");
    text.match_indices(needle)
        .nth(occurrence - 1)
        .map(|(idx, _)| idx)
}

fn resolve_expected_issues(text: &str, expected: &[ExpectedIssue]) -> Vec<ResolvedExpectedIssue> {
    expected
        .iter()
        .map(|issue| {
            let offset = nth_offset(text, &issue.found, issue.occurrence).unwrap_or_else(|| {
                panic!(
                    "could not resolve occurrence {} of {:?} in text {:?}",
                    issue.occurrence, issue.found, text
                )
            });
            ResolvedExpectedIssue {
                offset,
                found: issue.found.clone(),
                replace: issue.replace.clone(),
                rule_type: parse_issue_type(&issue.rule_type),
            }
        })
        .collect()
}

fn matches_expected(actual: &Issue, expected: &ResolvedExpectedIssue) -> bool {
    actual.offset == expected.offset
        && actual.found == expected.found
        && actual.rule_type == expected.rule_type
        && (actual.suggestions.iter().any(|s| s == &expected.replace)
            || (expected.replace.is_empty() && actual.suggestions.is_empty()))
}

fn score_document(actual: &[Issue], expected: &[ResolvedExpectedIssue]) -> ScoreCounts {
    let mut used = vec![false; actual.len()];
    let mut counts = ScoreCounts::default();

    for exp in expected {
        if let Some(idx) = actual
            .iter()
            .enumerate()
            .find_map(|(idx, issue)| (!used[idx] && matches_expected(issue, exp)).then_some(idx))
        {
            used[idx] = true;
            counts.tp += 1;
        } else {
            counts.fn_ += 1;
        }
    }

    counts.fp = used.iter().filter(|matched| !**matched).count();
    counts
}

fn pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn precision(counts: &ScoreCounts) -> f64 {
    let denom = counts.tp + counts.fp;
    if denom == 0 {
        100.0
    } else {
        pct(counts.tp, denom)
    }
}

fn recall(counts: &ScoreCounts) -> f64 {
    let denom = counts.tp + counts.fn_;
    if denom == 0 {
        100.0
    } else {
        pct(counts.tp, denom)
    }
}

fn fix_success_rate(counts: &FixCounts) -> f64 {
    pct(counts.exact_docs, counts.total_docs)
}

/// Repeat-weighted rate. Gated alongside the per-fixture rate.
fn native_fp_rate(counts: &NativeCounts) -> f64 {
    pct(counts.flagged_docs, counts.total_docs)
}

/// Per-fixture rate.  This is the gated number.
fn native_case_fp_rate(counts: &NativeCounts) -> f64 {
    pct(counts.flagged_cases, counts.total_cases)
}

fn evaluate_positive_corpus(
    spec: &CorpusSpec,
    scanner: &Scanner,
    segmenter: &Segmenter,
    converter: &S2TConverter,
) -> (ScoreCounts, FixCounts, usize) {
    let mut score = ScoreCounts::default();
    let mut fix = FixCounts::default();
    let mut total_bytes = 0usize;

    for case in &spec.cases {
        let scan_text = if spec.mode == "s2t" {
            let converted = converter.convert(&case.input);
            let expected_scan_text = case
                .scan_text
                .as_ref()
                .unwrap_or_else(|| panic!("{}:{} missing scan_text", spec.id, case.id));
            assert_eq!(
                converted, *expected_scan_text,
                "{}:{} built-in S2T output drifted",
                spec.id, case.id
            );
            expected_scan_text.as_str()
        } else {
            case.input.as_str()
        };

        let expected = resolve_expected_issues(scan_text, &case.expected_issues);
        let content_type = case_content_type(case);
        let cfg = build_config(spec, case.detect_ai.unwrap_or(spec.detect_ai));
        let issues = scanner
            .scan_for_content_type_with_config(scan_text, content_type, cfg)
            .issues;
        let doc_score = score_document(&issues, &expected);
        let fixed = apply_fixes_with_context(
            scan_text,
            &issues,
            FixMode::LexicalSafe,
            &build_exclusions_for_content_type(scan_text, content_type),
            Some(segmenter),
        );

        total_bytes += scan_text.len() * case.repeat;
        score.tp += doc_score.tp * case.repeat;
        score.fp += doc_score.fp * case.repeat;
        score.fn_ += doc_score.fn_ * case.repeat;
        fix.total_docs += case.repeat;
        if fixed.text == case.expected_fixed {
            fix.exact_docs += case.repeat;
        }
    }

    (score, fix, total_bytes)
}

fn evaluate_native_corpus(
    spec: &CorpusSpec,
    scanner: &Scanner,
    segmenter: &Segmenter,
) -> (NativeCounts, FixCounts, usize) {
    let mut native = NativeCounts::default();
    let mut fix = FixCounts::default();
    let mut total_bytes = 0usize;

    for case in &spec.cases {
        let content_type = case_content_type(case);
        let cfg = build_config(spec, case.detect_ai.unwrap_or(spec.detect_ai));
        let issues = scanner
            .scan_for_content_type_with_config(&case.input, content_type, cfg)
            .issues;
        if case.must_be_clean {
            assert!(
                issues.is_empty(),
                "native case {} must stay clean: {:?}",
                case.id,
                issues.iter().map(|issue| &issue.found).collect::<Vec<_>>()
            );
        }
        let fixed = apply_fixes_with_context(
            &case.input,
            &issues,
            FixMode::LexicalSafe,
            &build_exclusions_for_content_type(&case.input, content_type),
            Some(segmenter),
        );

        total_bytes += case.input.len() * case.repeat;
        native.total_docs += case.repeat;
        native.total_cases += 1;
        native.total_fp_issues += issues.len() * case.repeat;
        if !issues.is_empty() {
            native.flagged_docs += case.repeat;
            native.flagged_cases += 1;
        }
        fix.total_docs += case.repeat;
        if fixed.text == case.expected_fixed {
            fix.exact_docs += case.repeat;
        }
    }

    (native, fix, total_bytes)
}

fn print_positive_report(spec: &CorpusSpec, score: &ScoreCounts, fix: &FixCounts, bytes: usize) {
    println!(
        "{:<24} bytes={:>6}  precision={:>5.1}%  recall={:>5.1}%  safe_fix={:>5.1}%  tp={} fp={} fn={}  {}",
        spec.id,
        bytes,
        precision(score),
        recall(score),
        fix_success_rate(fix),
        score.tp,
        score.fp,
        score.fn_,
        spec.label,
    );
}

fn print_native_report(spec: &CorpusSpec, native: &NativeCounts, fix: &FixCounts, bytes: usize) {
    println!(
        "{:<24} bytes={:>6}  fp_rate(cases)={:>5.1}% ({}/{})  fp_rate(weighted)={:>5.1}% ({}/{})  fp_issues={}  safe_fix={:>5.1}%  {}",
        spec.id,
        bytes,
        native_case_fp_rate(native),
        native.flagged_cases,
        native.total_cases,
        native_fp_rate(native),
        native.flagged_docs,
        native.total_docs,
        native.total_fp_issues,
        fix_success_rate(fix),
        spec.label,
    );
}

fn load_corpus(path: &str) -> CorpusSpec {
    serde_json::from_str(path).unwrap()
}

#[test]
fn corpus_evaluation_suite() {
    let (scanner, segmenter) = load_scanner();
    let converter = S2TConverter::new();

    let ai = load_corpus(include_str!("corpus/ai-generated.json"));
    let native = load_corpus(include_str!("corpus/native-zh-tw.json"));
    let cn = load_corpus(include_str!("corpus/cn-to-tw-conversion.json"));

    let (ai_score, ai_fix, ai_bytes) =
        evaluate_positive_corpus(&ai, &scanner, &segmenter, &converter);
    let (native_counts, native_fix, native_bytes) =
        evaluate_native_corpus(&native, &scanner, &segmenter);
    let (cn_score, cn_fix, cn_bytes) =
        evaluate_positive_corpus(&cn, &scanner, &segmenter, &converter);

    assert!(
        ai_bytes >= ai.min_bytes,
        "{} corpus too small: {} < {} bytes",
        ai.id,
        ai_bytes,
        ai.min_bytes
    );
    assert!(
        native_bytes >= native.min_bytes,
        "{} corpus too small: {} < {} bytes",
        native.id,
        native_bytes,
        native.min_bytes
    );
    assert!(
        cn_bytes >= cn.min_bytes,
        "{} corpus too small: {} < {} bytes",
        cn.id,
        cn_bytes,
        cn.min_bytes
    );

    let aggregate = ScoreCounts {
        tp: ai_score.tp + cn_score.tp,
        fp: ai_score.fp + cn_score.fp,
        fn_: ai_score.fn_ + cn_score.fn_,
    };

    println!();
    println!("=== Corpus Evaluation Suite ===");
    println!();
    println!("{:<24} {:<}", "corpus", "metrics");
    println!("{}", "-".repeat(112));
    print_positive_report(&ai, &ai_score, &ai_fix, ai_bytes);
    print_native_report(&native, &native_counts, &native_fix, native_bytes);
    print_positive_report(&cn, &cn_score, &cn_fix, cn_bytes);
    println!("{}", "-".repeat(112));
    println!(
        "{:<24} precision={:>5.1}%  recall={:>5.1}%",
        "aggregate_dirty",
        precision(&aggregate),
        recall(&aggregate),
    );
    println!();
    // Printed before the gates so the caveats appear even when one fails.
    println!("Caveats: the corpora are synthetic and small. Precision is per matched issue,");
    println!("not per document. The native false-positive rate covers only the checked-in cases.");
    println!();

    assert!(
        precision(&aggregate) >= 90.0,
        "aggregate precision gate failed: {:.1}%",
        precision(&aggregate)
    );

    // Both rates are gated, because each is blind to what the other catches.
    // The weighted rate misses a regression that lands on a rarely repeated
    // fixture: a batch of rules once introduced nine reproducible
    // false-positive classes and moved it by zero. The per-fixture rate misses
    // a regression concentrated in a heavily repeated one: a single repeat-50
    // fixture is 7% of the byte volume but under 3% of the fixtures.
    assert!(
        native_case_fp_rate(&native_counts) <= 5.0,
        "native zh-TW false-positive gate failed: {:.1}% of fixtures ({}/{})",
        native_case_fp_rate(&native_counts),
        native_counts.flagged_cases,
        native_counts.total_cases
    );
    assert!(
        native_fp_rate(&native_counts) <= 5.0,
        "native zh-TW false-positive gate failed: {:.1}% by repeat weight ({}/{})",
        native_fp_rate(&native_counts),
        native_counts.flagged_docs,
        native_counts.total_docs
    );

    // All three safe-fix rates are gated. Leaving cn_fix and native_fix
    // computed but unasserted meant a fix-tier behavior change could sit in the
    // printed output indefinitely: gating editorial_confidence low terms once
    // drove cn_to_tw to exactly 85.0% while the suite still passed.
    //
    // The two rates added here gate at 99.0, not 85.0, because 85.0 is too
    // loose to catch what it is meant to catch. cn_to_tw carries 900 weighted
    // docs, so 85.0% tolerates 135 of them regressing, roughly two whole
    // fixture cases, and ">= 85.0" admits the exact 85.0% case cited above.
    //
    // 99.0 is a suite-level tripwire, not a per-fixture one: it catches a heavy
    // fixture, or a systematic change spread across several light ones, but not
    // a single light fixture on its own. Per-fixture coverage is the job of the
    // fixtures' own expected_fixed assertions.
    //
    // ai_fix stays at the historical 85.0 on purpose: it is the gate CLAUDE.md
    // documents as the project contract. Raising it to 99.0 is worth doing and
    // would cost nothing today, but that is a deliberate contract change, not a
    // side effect of adding two sibling gates.
    assert!(
        fix_success_rate(&ai_fix) >= 85.0,
        "AI-generated safe-fix gate failed: {:.1}%",
        fix_success_rate(&ai_fix)
    );
    assert!(
        fix_success_rate(&cn_fix) >= 99.0,
        "zh-CN conversion safe-fix gate failed: {:.1}% (expected 100.0%)",
        fix_success_rate(&cn_fix)
    );
    assert!(
        fix_success_rate(&native_fix) >= 99.0,
        "native zh-TW safe-fix gate failed: {:.1}% (expected 100.0%)",
        fix_success_rate(&native_fix)
    );

    // Recall was computed and printed on every run and asserted nowhere, so a
    // change that halved AI detection passed the suite. That is not
    // hypothetical: two commits reworked AI detection while this gate did not
    // exist, and the only thing that would have noticed was a human reading the
    // printed number.
    //
    // Per corpus, not aggregate, because the aggregate is dominated by
    // cn_to_tw_conversion: it carries roughly twice the true positives of
    // ai_generated, so AI recall could fall a long way with the aggregate
    // barely moving.
    //
    // Floors sit two points below what each corpus measured when the gate was
    // added. One point is too tight to survive adding a fixture, which shifts
    // the denominator; five is wide enough to hide a detector going quiet.
    for (label, score, min_recall, min_precision) in [
        ("AI-generated", &ai_score, 94.0, 91.0),
        ("zh-CN conversion", &cn_score, 98.0, 96.0),
    ] {
        assert!(
            recall(score) >= min_recall,
            "{label} recall gate failed: {:.1}% (floor {min_recall}%)",
            recall(score)
        );
        assert!(
            precision(score) >= min_precision,
            "{label} precision gate failed: {:.1}% (floor {min_precision}%)",
            precision(score)
        );
    }
}
