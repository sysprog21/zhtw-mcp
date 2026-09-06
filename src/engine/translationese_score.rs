// Document-level translationese (翻譯腔/歐化) scoring.
//
// Orthogonal to AI signature scoring. A translated technical manual is 歐化 but
// not AI-generated. Separate output struct, separate threshold.
//
// Composite score from:
// 1. Passive voice density (被 per 1000 chars)
// 2. 的-chain depth (max consecutive 的 without comma)
// 3. Weak-verb decomposition count (進行/加以/予以 + nominalized verb)
// 4. Pronoun density (他/她/它/他們 per 1000 chars)
// 5. Translationese issue density (from per-occurrence detectors)

use serde::{Deserialize, Serialize};

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, IssueType};

/// A single translationese signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationeseMarker {
    pub signal: String,
    pub count: usize,
    pub density: f32,
    pub threshold: f32,
}

/// Aggregated translationese scoring report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationeseReport {
    /// Composite score: 0.0 = natural zh-TW, 1.0 = heavily westernized.
    pub score: f32,
    /// Individual signal breakdown.
    pub markers: Vec<TranslationeseMarker>,
    /// Top contributing signal descriptions.
    pub top_signals: Vec<String>,
    /// Maximum consecutive 的 count found in any clause.
    pub max_de_chain: usize,
    /// Domain calibration profile applied.  Defaults to `General` when
    /// deserializing reports from older cache entries that predate the
    /// per-domain calibration feature, so a single missing field does not
    /// invalidate the entire cache file.
    #[serde(default)]
    pub domain: TranslationeseDomain,
}

/// Per-domain calibration profile for translationese scoring.
///
/// Different document genres tolerate different rates of westernized
/// constructions: technical writing accepts more passive voice and weak-verb
/// nominalization than literary prose; news writing falls between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TranslationeseDomain {
    /// Balanced thresholds suitable for general prose.
    #[default]
    General,
    /// Technical writing: looser thresholds for passive voice and weak verbs.
    Technical,
    /// Literary writing: tighter thresholds, especially for de-chains.
    Literary,
    /// News writing: moderate thresholds, favors active voice.
    News,
}

impl TranslationeseDomain {
    /// Human-readable name (matches the CLI flag value).
    pub fn name(self) -> &'static str {
        match self {
            TranslationeseDomain::General => "general",
            TranslationeseDomain::Technical => "technical",
            TranslationeseDomain::Literary => "literary",
            TranslationeseDomain::News => "news",
        }
    }

    /// Strict parse from string.  Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "general" => Some(TranslationeseDomain::General),
            "technical" => Some(TranslationeseDomain::Technical),
            "literary" => Some(TranslationeseDomain::Literary),
            "news" => Some(TranslationeseDomain::News),
            _ => None,
        }
    }

    /// Per-domain threshold table.
    pub fn thresholds(self) -> DomainThresholds {
        match self {
            TranslationeseDomain::General => DomainThresholds {
                passive: 3.0,
                weak_verb: 2.0,
                pronoun: 8.0,
                de_chain: 4,
                issue_density: 5.0,
                zy1b_per_200: 2.0,
                zy3b_chain_min: 3,
                zy5_min_chars: 15,
                zy5_min_de_count: 2,
            },
            TranslationeseDomain::Technical => DomainThresholds {
                // Technical prose tolerates more passive voice (specs commonly
                // use "被定義為", "被觀察到") and weak-verb nominalization
                // ("進行測試", "加以分析" are idiomatic in lab reports).
                passive: 6.0,
                weak_verb: 4.0,
                pronoun: 6.0,
                de_chain: 5,
                issue_density: 7.0,

                // Register crosswalk: external literal style ↔ technical_docs.
                // Looser ZY1b density (technical writing accepts repeated
                // "...之一" enumerations); higher ZY3b chain threshold (chained
                // nominalization tolerated more in technical text).
                zy1b_per_200: 3.0,
                zy3b_chain_min: 4,
                zy5_min_chars: 18,
                zy5_min_de_count: 2,
            },
            TranslationeseDomain::Literary => DomainThresholds {
                // Literary prose should be lean: tighter thresholds catch the
                // patterns 余光中 specifically warned against.
                passive: 1.5,
                weak_verb: 1.0,
                pronoun: 10.0,
                de_chain: 3,
                issue_density: 3.0,
                zy1b_per_200: 1.0,
                zy3b_chain_min: 3,
                zy5_min_chars: 12,
                zy5_min_de_count: 2,
            },
            TranslationeseDomain::News => DomainThresholds {
                // News writing favors active voice and concise sentences.
                // Register crosswalk: external storytelling style ↔ newsroom.
                passive: 2.5,
                weak_verb: 2.0,
                pronoun: 7.0,
                de_chain: 4,
                issue_density: 4.0,
                zy1b_per_200: 2.5,
                zy3b_chain_min: 3,
                zy5_min_chars: 15,
                zy5_min_de_count: 2,
            },
        }
    }
}

/// Per-signal threshold values for a given domain calibration.
///
/// Register-aware fields (zy1b/zy3b/zy5) flip with the
/// `--translationese-domain` flag (CLI) or `translationese_domain` MCP
/// argument; threshold values are committed to source as the per-domain
/// `thresholds()` table.
#[derive(Debug, Clone, Copy)]
pub struct DomainThresholds {
    pub passive: f32,
    pub weak_verb: f32,
    pub pronoun: f32,
    pub de_chain: usize,
    pub issue_density: f32,
    /// ZY1b: 之一 occurrences per 200 chars in a paragraph above which
    /// the density check fires.
    pub zy1b_per_200: f32,
    /// ZY3b: minimum nominalization-head chain length (e.g.
    /// `<head>的<head>的<head>` = 3) within one sentence required to fire.
    pub zy3b_chain_min: usize,
    /// ZY5: minimum char length of the comma-free pre-modifier span.
    pub zy5_min_chars: usize,
    /// ZY5: minimum count of `的` particles inside the span.
    pub zy5_min_de_count: usize,
}

// Per-signal weights: kept constant across domains; only thresholds shift.
const PASSIVE_WEIGHT: f32 = 1.0;
const WEAK_VERB_WEIGHT: f32 = 0.8;
const PRONOUN_WEIGHT: f32 = 0.6;
const DE_CHAIN_WEIGHT: f32 = 0.7;
const ISSUE_DENSITY_WEIGHT: f32 = 0.5;

// Weak-verb prefixes that signal bureaucratic nominalization.
const WEAK_VERB_PREFIXES: &[&str] = &["進行", "加以", "予以", "展開", "作出", "給予", "提供"];

// Objects that, when following a weak-verb prefix, confirm the pattern is a
// real bureaucratic nominalization ("進行討論" → "討論") rather than a literal
// standalone use of the prefix ("進行" alone = "in progress"). Kept in sync
// with src/engine/scan/grammar.rs NOMINALIZED_VERBS / VERBOSE_ACTION_OBJECTS so
// the scoring signal aligns with per-issue flagging.
const WEAK_VERB_OBJECTS: &[&str] = &[
    "討論", "分析", "研究", "調查", "測試", "開發", "設計", "評估", "檢查", "審查", "修改", "更新",
    "比較", "溝通", "合作", "訓練", "處理", "管理", "規劃", "改善", "調整", "整合", "驗證", "觀察",
    "監控", "維護", "決定", "回應", "貢獻", "改變", "承諾", "解釋", "判斷", "選擇", "反應", "讓步",
    "保證", "回答", "犧牲", "努力", "支援", "協助", "檢討", "投票", "改革", "發表", "發展",
];

// Pronouns to count for density. Multi-character forms come first so the
// longest-match scan does not double-count "他們" as "他" + "他們".
const PRONOUNS: &[&str] = &["他們", "她們", "他", "她", "它"];

/// Compute translationese report from text and post-scan issues using the
/// general-purpose threshold table.  Returns None for texts too short
/// (< 200 chars).  Convenience wrapper for callers that don't need
/// domain-specific calibration.
pub fn compute_translationese_score(
    text: &str,
    issues: &[Issue],
    excluded: &[ByteRange],
) -> Option<TranslationeseReport> {
    compute_translationese_score_with_domain(text, issues, excluded, TranslationeseDomain::General)
}

/// Compute translationese report with a specified domain calibration.
///
/// Different domains use different threshold tables: see
/// [`TranslationeseDomain::thresholds`] for the values.
pub fn compute_translationese_score_with_domain(
    text: &str,
    issues: &[Issue],
    excluded: &[ByteRange],
    domain: TranslationeseDomain,
) -> Option<TranslationeseReport> {
    let char_count = {
        let mut count = 0usize;
        let mut byte_offset = 0usize;
        for ch in text.chars() {
            let ch_len = ch.len_utf8();
            if !is_excluded(byte_offset, byte_offset + ch_len, excluded) {
                count += 1;
            }
            byte_offset += ch_len;
        }
        count
    };
    if char_count < 200 {
        return None;
    }
    let text_k = char_count as f32 / 1000.0;
    let t = domain.thresholds();

    let mut markers = Vec::new();
    let mut weighted_sum: f32 = 0.0;
    let mut total_weight: f32 = 0.0;

    // Record a signal: push its marker, and add its excess contribution to the
    // weighted sum when over_threshold is true. Excess is clamped at 2x the
    // threshold to prevent a single runaway signal from dominating.
    let mut record = |signal: &str,
                      count: usize,
                      density: f32,
                      threshold: f32,
                      weight: f32,
                      over_threshold: bool| {
        markers.push(TranslationeseMarker {
            signal: signal.into(),
            count,
            density,
            threshold,
        });
        if over_threshold {
            // Excess is capped at 2.0; floor at 0.1 so an exact threshold hit
            // still contributes (matches the >= semantics for de-chain).
            let raw_excess = ((density - threshold) / threshold).min(2.0);
            let excess = raw_excess.max(0.1);
            weighted_sum += excess * weight;
        }
        total_weight += weight;
    };

    // Signal 1: passive voice density (被 count).
    let passive_count = count_pattern(text, "被", excluded);
    let passive_density = passive_count as f32 / text_k;
    record(
        "被動語態",
        passive_count,
        passive_density,
        t.passive,
        PASSIVE_WEIGHT,
        passive_density > t.passive,
    );

    // Signal 2: 的-chain depth. Uses >= so that exactly hitting the threshold
    // still contributes.
    let max_de_chain = compute_max_de_chain(text, excluded);
    record(
        "的字鏈",
        max_de_chain,
        max_de_chain as f32,
        t.de_chain as f32,
        DE_CHAIN_WEIGHT,
        max_de_chain >= t.de_chain,
    );

    // Signal 3: weak-verb decomposition density.
    let weak_verb_count = count_weak_verbs(text, excluded);
    let weak_verb_density = weak_verb_count as f32 / text_k;
    record(
        "弱動詞分解",
        weak_verb_count,
        weak_verb_density,
        t.weak_verb,
        WEAK_VERB_WEIGHT,
        weak_verb_density > t.weak_verb,
    );

    // Signal 4: pronoun density. Use longest-match scan to avoid counting 他們
    // as both 他 and 他們.
    let pronoun_count = count_longest_match(text, PRONOUNS, excluded);
    let pronoun_density = pronoun_count as f32 / text_k;
    record(
        "代詞密度",
        pronoun_count,
        pronoun_density,
        t.pronoun,
        PRONOUN_WEIGHT,
        pronoun_density > t.pronoun,
    );

    // Signal 5: translationese issue density from per-occurrence detectors.
    // Rhythm findings are Translationese too, but they only exist when the
    // opt-in --rhythm axis is on, and this threshold was calibrated without
    // them. Counting them would let a taste flag move the score.
    let trans_issue_count = issues
        .iter()
        .filter(|i| i.rule_type == IssueType::Translationese)
        .filter(|i| !i.phase_family.is_some_and(|(family, _)| family.is_rhythm()))
        .count();
    let trans_density = trans_issue_count as f32 / text_k;
    record(
        "翻譯腔偵測",
        trans_issue_count,
        trans_density,
        t.issue_density,
        ISSUE_DENSITY_WEIGHT,
        trans_density > t.issue_density,
    );

    // Composite score: weighted average of excess ratios, clamped to [0, 1].
    let score = if total_weight > 0.0 {
        (weighted_sum / total_weight).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Top signals: sorted by contribution. Use >= so an exact threshold hit
    // (e.g. exactly 4 consecutive 的) appears in the listing.
    let mut top: Vec<(String, f32)> = markers
        .iter()
        .filter(|m| m.density >= m.threshold)
        .map(|m| {
            let excess = ((m.density - m.threshold) / m.threshold).clamp(0.1, 2.0);
            (
                format!(
                    "{}: {:.1}/千字 (閾值 {:.1})",
                    m.signal, m.density, m.threshold
                ),
                excess,
            )
        })
        .collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_signals: Vec<String> = top.into_iter().take(3).map(|(s, _)| s).collect();

    Some(TranslationeseReport {
        score,
        markers,
        top_signals,
        max_de_chain,
        domain,
    })
}

/// Count occurrences of any pattern in the list, using longest-match-first
/// semantics so that "他們" does not get counted as "他" + "他們".
/// Patterns are tried in input order at each position; callers should
/// pre-sort longest-first.
fn count_longest_match(text: &str, patterns: &[&str], excluded: &[ByteRange]) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        let mut matched_len = 0;
        for pat in patterns {
            let plen = pat.len();
            if i + plen <= bytes.len() && &bytes[i..i + plen] == pat.as_bytes() {
                matched_len = plen;
                break;
            }
        }
        if matched_len > 0 {
            if !is_excluded(i, i + matched_len, excluded) {
                count += 1;
            }
            i += matched_len;
        } else {
            // Advance by one codepoint to keep i on a char boundary.
            let ch_len = text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            i += ch_len;
        }
    }
    count
}

/// Count non-overlapping occurrences of a pattern, excluding exclusion zones.
fn count_pattern(text: &str, pattern: &str, excluded: &[ByteRange]) -> usize {
    let pattern_len = pattern.len();
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = text[start..].find(pattern) {
        let abs = start + pos;
        if !is_excluded(abs, abs + pattern_len, excluded) {
            count += 1;
        }
        start = abs + pattern_len;
    }
    count
}

/// Find the maximum consecutive 的 count in any clause (split on commas).
fn compute_max_de_chain(text: &str, excluded: &[ByteRange]) -> usize {
    let mut max_chain = 0;
    let mut current_chain = 0;
    let mut byte_offset = 0;

    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        let in_excluded = is_excluded(byte_offset, byte_offset + ch_len, excluded);
        byte_offset += ch_len;

        if in_excluded {
            current_chain = 0;
            continue;
        }

        if ch == '的' {
            current_chain += 1;
            max_chain = max_chain.max(current_chain);
        } else if matches!(ch, '，' | ',' | '。' | '！' | '？' | '；' | '\n') {
            current_chain = 0;
        }

        // Non-的 CJK characters don't reset the chain: we're counting 的 in
        // patterns like X的Y的Z的W.
    }
    max_chain
}

/// Count weak-verb + nominalized-verb compounds (e.g. "進行討論", "加以分析").
///
/// Bare prefix hits without a known object do not count: 進行 alone means
/// "in progress" and is fine on its own; the translationese signal fires
/// only when the prefix precedes a nominalized verb.
fn count_weak_verbs(text: &str, excluded: &[ByteRange]) -> usize {
    let mut count = 0;
    for &prefix in WEAK_VERB_PREFIXES {
        let prefix_len = prefix.len();
        let mut start = 0;
        while let Some(pos) = text[start..].find(prefix) {
            let abs = start + pos;
            let after = abs + prefix_len;
            start = after;
            if is_excluded(abs, after, excluded) {
                continue;
            }

            // Require a known weak-verb object starting at after. Look ahead up
            // to 4 chars (handles optional 了/的 between prefix and object,
            // e.g. "進行了討論").
            let lookahead_end = text[after..]
                .char_indices()
                .nth(4)
                .map(|(i, _)| after + i)
                .unwrap_or(text.len());
            let window = &text[after..lookahead_end];

            // Both prefix and object must be outside excluded zones; an object
            // span buried inside a code fence does not count.
            if WEAK_VERB_OBJECTS.iter().any(|obj| {
                window.find(obj).is_some_and(|obj_pos| {
                    let obj_start = after + obj_pos;
                    !is_excluded(obj_start, obj_start + obj.len(), excluded)
                })
            }) {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(text: &str) -> Option<TranslationeseReport> {
        compute_translationese_score(text, &[], &[])
    }

    #[test]
    fn short_text_returns_none() {
        assert!(score("短文").is_none());
    }

    #[test]
    fn clean_text_scores_low() {
        // Natural zh-TW prose, no translationese patterns (>200 chars).
        let text = "台灣是一個美麗的島嶼。\
                    這裡有豐富的自然景觀和人文風情。\
                    山脈縱貫全島，海岸線變化多端。\
                    人民友善熱情，文化多元而包容。\
                    教育普及，科技產業蓬勃發展。\
                    美食種類繁多，從夜市小吃到精緻料理。\
                    四季分明的氣候適合各種戶外活動。\
                    歷史悠久的寺廟和現代建築並存。\
                    交通便利，鐵路和公路網路完善。\
                    醫療水準在亞洲名列前茅。\
                    城市規劃相當完善，公共運輸覆蓋率極高。\
                    都市農業開始萌芽，屋頂菜園數量逐年增加。\
                    博物館藏品豐富，展覽內容定期更新。\
                    圖書館遍布各區，閱讀風氣盛行。\
                    社區活動中心提供多樣化課程。\
                    夜市文化獨樹一幟，吸引各國觀光客。\
                    傳統節慶慶典保留完整的民俗活動。\
                    志工服務精神深入民間組織運作。\
                    環保意識逐年提升，垃圾分類成效顯著。\
                    全民健保制度獲得國際社會高度肯定。";
        let report = score(text);
        assert!(report.is_some());
        let r = report.unwrap();
        assert!(
            r.score < 0.3,
            "Clean text should score low, got {}",
            r.score
        );
    }

    #[test]
    fn westernized_text_scores_higher() {
        // Text with heavy translationese markers (>200 chars).
        let text = "在這個過程中，問題被充分地討論了。\
                    她被認為是最優秀的人選。\
                    政府進行了全面的調查和分析。\
                    他們對這個問題加以研究和討論。\
                    這個方案被廣泛認為是最好的選擇。\
                    她被授予了最高榮譽的獎項。\
                    他們進行了長時間的討論和辯論。\
                    結果被公布在最新的報告中。\
                    這些措施被視為非常必要的步驟。\
                    整個計劃被認為是成功的典範。\
                    他們的努力被證明是值得的。\
                    她被選為年度最佳員工的候選人。\
                    這項政策被認為對經濟發展有重大影響。\
                    他們進行了深入的市場分析和評估。\
                    這個決定被視為具有里程碑意義的轉折。\
                    她被指派負責整個專案的執行工作。\
                    他們加以整合並予以重新規劃。\
                    這些成果被廣泛報導和討論。\
                    她的表現被評價為出色的領導典範。\
                    他們對問題進行了全面的檢討和改善。";
        let report = score(text);
        assert!(report.is_some());
        let r = report.unwrap();
        assert!(
            r.score > 0.0,
            "Westernized text should score higher, got {}",
            r.score
        );
    }

    #[test]
    fn technical_domain_scores_lower_than_literary_on_passive_text() {
        // Identical text scored under technical (lenient) vs literary (strict)
        // domains: literary should always score >= technical for the same
        // passive-heavy input.
        let text = "他被認為是優秀的學者。她被視為傑出領袖。整個項目被認為是成功的。\
                    她被授予了榮譽。她被選為主席。他們進行了討論。\
                    她被廣泛認為是優秀的人選。他被任命為主管。\
                    他被指派負責這個專案。他們進行了完整的評估。\
                    這個方案被認為是最好的選擇。他們予以支援。\
                    這些成果被報導出來。她被評為年度典範。\
                    他被推舉為代表。整體計畫被視為一大進展。\
                    研究結果被廣泛發表並被多次引用。這個提案被多方採納。\
                    他們的努力被證明是值得的。她被選為年度最佳員工。\
                    這項政策被認為對發展有重大影響。\
                    她被授予最高榮譽。他被廣泛認可為傑出領袖。"; // >200 chars
        let r_tech = compute_translationese_score_with_domain(
            text,
            &[],
            &[],
            TranslationeseDomain::Technical,
        )
        .expect("technical score");
        let r_lit = compute_translationese_score_with_domain(
            text,
            &[],
            &[],
            TranslationeseDomain::Literary,
        )
        .expect("literary score");
        assert!(
            r_lit.score >= r_tech.score,
            "literary ({}) should score >= technical ({}) on passive-heavy text",
            r_lit.score,
            r_tech.score
        );
        assert_eq!(r_tech.domain, TranslationeseDomain::Technical);
        assert_eq!(r_lit.domain, TranslationeseDomain::Literary);
    }

    #[test]
    fn domain_from_str_round_trips() {
        for d in [
            TranslationeseDomain::General,
            TranslationeseDomain::Technical,
            TranslationeseDomain::Literary,
            TranslationeseDomain::News,
        ] {
            assert_eq!(TranslationeseDomain::from_str_strict(d.name()), Some(d));
        }
        assert_eq!(TranslationeseDomain::from_str_strict("invalid"), None);
    }

    #[test]
    fn de_chain_detection() {
        let chain = compute_max_de_chain("這是我的朋友的妹妹的同學的書", &[]);
        assert_eq!(chain, 4);
    }

    #[test]
    fn passive_count_basic() {
        let count = count_pattern("他被打了，她也被罵了", "被", &[]);
        assert_eq!(count, 2);
    }

    #[test]
    fn weak_verb_count_basic() {
        let count = count_weak_verbs("我們進行討論並加以分析", &[]);
        assert_eq!(count, 2);
    }

    #[test]
    fn weak_verb_count_rejects_bare_prefix() {
        // "進行" alone = "in progress" (legitimate standalone use); must not
        // inflate the weak-verb signal.
        assert_eq!(count_weak_verbs("專案正在進行，尚未完成。", &[]), 0);
        // "進行中" likewise has no nominalized object.
        assert_eq!(count_weak_verbs("工作進行中，請稍候。", &[]), 0);
    }

    #[test]
    fn weak_verb_count_allows_intervening_particles() {
        // "進行了討論": the 了 between prefix and object should still count.
        assert_eq!(count_weak_verbs("他們進行了討論並加以了分析", &[]), 2);
    }

    #[test]
    fn weak_verb_count_skips_excluded_object() {
        // Codex round 4: an object span inside an exclusion zone (e.g. inline
        // code) must not count toward the weak-verb signal even when the prefix
        // itself is in clean prose.
        let text = "他們進行討論之後，我們進行討論再次回顧。";
        // Mark the second "討論" (bytes 33..39 in this string) as excluded.
        let second_obj_start = text.rfind("討論").unwrap();
        let excluded = vec![ByteRange {
            start: second_obj_start,
            end: second_obj_start + "討論".len(),
        }];

        // Without the fix: count = 2. With the fix: only the unexcluded first
        // occurrence counts → 1.
        assert_eq!(count_weak_verbs(text, &excluded), 1);
    }

    #[test]
    fn report_deserializes_without_domain_field() {
        // Codex round 4: pre-domain cache entries must still load. Without
        // serde(default) on domain, a single old entry would cause
        // load_entries() to discard the whole cache file.
        let json = r#"{
            "score": 0.5,
            "markers": [],
            "top_signals": [],
            "max_de_chain": 0
        }"#;
        let r: TranslationeseReport =
            serde_json::from_str(json).expect("missing domain field must default, not fail");
        assert_eq!(r.domain, TranslationeseDomain::General);
    }
}
