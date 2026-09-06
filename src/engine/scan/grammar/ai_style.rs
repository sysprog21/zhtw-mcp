//! AI-writing detectors that work on grammar and paragraph shape.
//!
//! Everything here reports IssueType::AiStyle rather than Grammar, and is
//! gated by the ai_* switches on ProfileConfig.

use super::*;

// AI writing detection: grammar-level patterns

// Helper to create an AI-style issue (IssueType::AiStyle instead of Grammar).
pub(super) fn ai_style_issue(
    offset: usize,
    found: &str,
    suggestion: &str,
    context: &str,
    severity: Severity,
) -> Issue {
    Issue::new(
        offset,
        found.len(),
        found,
        if suggestion.is_empty() {
            vec![]
        } else {
            vec![suggestion.into()]
        },
        IssueType::AiStyle,
        severity,
    )
    .with_context(context)
}

// Context clues for definition sense of 意味著 → 表示. 即 on its own is not
// here: it matches inside 即使, 即將 and 立即, so an ordinary sentence that
// merely opens on 即使 was read as a definition. 亦即 is the form that actually
// marks one and cannot be a prefix of those.
const YIWEIZHE_DEFINITION_CLUES: &[&str] =
    &["定義", "是指", "就是", "亦即", "所謂", "稱為", "指的是"];

// Context clues for consequence sense of 意味著 → 代表.
const YIWEIZHE_CONSEQUENCE_CLUES: &[&str] = &[
    "因此", "所以", "結果", "導致", "造成", "如果", "一旦", "將會", "可能",
];

// Context clues for explanation/paraphrase sense of 意味著 → 也就是說.
const YIWEIZHE_EXPLANATION_CLUES: &[&str] =
    &["換言之", "換句話說", "簡單來說", "簡言之", "具體來說"];

// Detect overuse of 意味著 where native zh-TW would use context-appropriate
// alternatives: 表示 (definition), 代表 (consequence), 也就是說 (explanation).
// Emits a single disambiguated suggestion per occurrence (required by fixer.rs
// which skips issues with >1 suggestion for non-orthographic types). When
// disambiguation confidence is low, emits advisory-only (no suggestions).
pub(super) fn scan_ai_semantic_safety(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let target = "意味著";
    let target_len = target.len();
    let mut search_start = 0;

    while let Some(pos) = text[search_start..].find(target) {
        let abs_pos = search_start + pos;
        let end = abs_pos + target_len;
        search_start = end;

        if is_excluded(abs_pos, end, excluded) {
            continue;
        }

        // Look at surrounding sentence for context clues to disambiguate. Use
        // sentence boundaries (not clause boundaries) so that clues in an
        // adjacent clause within the same sentence are still visible (e.g.
        // '換言之，這意味著': '換言之' is in the prior clause).
        let sentence_start = text[..abs_pos]
            .char_indices()
            .rev()
            .find(|&(_, ch)| is_sentence_end(ch))
            .map(|(i, ch)| i + ch.len_utf8())
            .unwrap_or(0);
        let sentence_end = text[end..]
            .find(|ch: char| is_sentence_end(ch))
            .map(|i| end + i)
            .unwrap_or(text.len());
        let context_window = &text[sentence_start..sentence_end];

        // Try disambiguation: definition > consequence > explanation.
        let suggestion = if YIWEIZHE_DEFINITION_CLUES
            .iter()
            .any(|c| context_window.contains(c))
        {
            "表示"
        } else if YIWEIZHE_CONSEQUENCE_CLUES
            .iter()
            .any(|c| context_window.contains(c))
        {
            "代表"
        } else if YIWEIZHE_EXPLANATION_CLUES
            .iter()
            .any(|c| context_window.contains(c))
        {
            "也就是說"
        } else {
            // Low confidence: no clear context → advisory only (empty
            // suggestion).
            ""
        };

        issues.push(ai_style_issue(
            abs_pos,
            target,
            suggestion,
            "AI semantic safety word; native zh-TW prefers \
             context-specific alternatives (\u{8868}\u{793a}/\u{4ee3}\u{8868}/\u{4e5f}\u{5c31}\u{662f}\u{8aaa})",
            Severity::Info,
        ));
    }
}

// Copula-avoidance patterns: AI replaces simple 是/有 with inflated
// alternatives. (inflated_form, simple_copula)
const COPULA_AVOIDANCE_PATTERNS: &[(&str, &str)] = &[
    ("作為", "是"),
    ("標誌著", "是"),
    ("充當", "是"),
    ("擁有", "有"),
    ("設有", "有"),
];

// Characters that, when adjacent to a copula pattern, indicate a compound word
// rather than an inflated copula. Matching these suppresses the issue. 作為:
// preceded by 所 (有所作為) or 大 (大作為). 擁有: followed by 權/者/感/量
// (擁有權, 擁有者, 擁有感, 擁有量).
fn is_copula_compound(text: &str, abs_pos: usize, end: usize, inflated: &str) -> bool {
    if inflated == "作為" {
        // Check preceding char.
        if abs_pos >= 3 {
            let prev_start = text.floor_char_boundary(abs_pos - 3);
            let prev = &text[prev_start..abs_pos];
            if prev.ends_with('所') || prev.ends_with('大') {
                return true;
            }
        }
    }
    if inflated == "擁有" {
        // Check following char.
        if end < text.len() {
            let next_end = text.ceil_char_boundary(end + 1);
            let next = &text[end..next_end];
            if next.starts_with('權')
                || next.starts_with('者')
                || next.starts_with('感')
                || next.starts_with('量')
            {
                return true;
            }
        }
    }
    false
}

// Context clues for technical prose (where copula avoidance is most
// suspicious).
const COPULA_TECH_CONTEXT: &[&str] = &[
    "系統",
    "程式",
    "函式",
    "API",
    "介面",
    "模組",
    "元件",
    "架構",
    "伺服器",
    "資料庫",
];

// Detect AI copula avoidance: 作為/標誌著/充當 replacing 是, and 擁有/設有
// replacing 有, in technical prose context.
pub(super) fn scan_ai_copula_avoidance(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for &(inflated, simple) in COPULA_AVOIDANCE_PATTERNS {
        let inflated_len = inflated.len();
        let mut search_start = 0;

        while let Some(pos) = text[search_start..].find(inflated) {
            let abs_pos = search_start + pos;
            let end = abs_pos + inflated_len;
            search_start = end;

            if is_excluded(abs_pos, end, excluded) {
                continue;
            }

            // Skip compound words where the pattern is part of a larger term.
            if is_copula_compound(text, abs_pos, end, inflated) {
                continue;
            }

            // Only flag in technical prose context to avoid false positives on
            // literary/formal writing where these forms are natural.
            let window_start = abs_pos.saturating_sub(80);
            let window_end = text.len().min(end + 80);
            let window =
                &text[text.floor_char_boundary(window_start)..text.ceil_char_boundary(window_end)];

            let in_tech_context = COPULA_TECH_CONTEXT.iter().any(|c| window.contains(c));
            if !in_tech_context {
                continue;
            }

            // Advisory only: no token-level suggestion. Direct replacement
            // (e.g. 作為→是) produces broken sentences because the surrounding
            // syntax must change too. The user must restructure manually.
            let ctx = format!(
                "AI copula avoidance: consider restructuring with '{simple}' \
                 instead of '{inflated}'"
            );
            issues.push(ai_style_issue(abs_pos, inflated, "", &ctx, Severity::Info));
        }
    }
}

// Passive 被-constructions that have obvious active rewrites. (被-pattern,
// active_rewrite) Only patterns where dropping 被 is universally safe (adverb +
// verb). Excluded: 被認為是/被稱為 (flips meaning with animate subject),
// 被設計為/被用來/被用於 (changes construction when subject is affected
// entity).
const PASSIVE_REWRITE_PATTERNS: &[(&str, &str)] = &[
    ("被廣泛使用", "廣泛使用"),
    ("被廣泛採用", "廣泛採用"),
    ("被廣泛應用", "廣泛應用"),
];

// Detect passive voice overuse: 被 + verb where active voice is more natural.
// Only flags patterns from a curated list to minimize false positives.
pub(super) fn scan_ai_passive(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for &(pattern, rewrite) in PASSIVE_REWRITE_PATTERNS {
        let pattern_len = pattern.len();
        let mut search_start = 0;

        while let Some(pos) = text[search_start..].find(pattern) {
            let abs_pos = search_start + pos;
            let end = abs_pos + pattern_len;
            search_start = end;

            if is_excluded(abs_pos, end, excluded) {
                continue;
            }

            issues.push(ai_style_issue(
                abs_pos,
                pattern,
                rewrite,
                "AI passive voice overuse; active voice is more natural in zh-TW",
                Severity::Info,
            ));
        }
    }
}

// Didactic sentence patterns: AI-typical moralizing constructions that are
// nearly 100% AI-generated in technical articles.
// Pattern: 的(故事|案例|經驗|教訓|歷史)(告訴|提醒|啟示)(我們|後人|世人)
pub(super) fn scan_ai_didactic(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Use a simple multi-step search: find each 告訴我們/提醒我們/啟示我們
    // then look backward for 的(故事|案例|經驗|教訓|歷史)
    // This is more efficient than regex for CJK text.

    const VERBS: &[&str] = &["告訴", "提醒", "啟示"];
    const OBJECTS: &[&str] = &["我們", "後人", "世人"];
    const NOUNS: &[&str] = &["故事", "案例", "經驗", "教訓", "歷史"];

    for verb in VERBS {
        for obj in OBJECTS {
            let pattern = format!("{verb}{obj}");
            let pattern_len = pattern.len();
            let mut search_start = 0;

            while let Some(pos) = text[search_start..].find(&pattern) {
                let abs_pos = search_start + pos;
                let end = abs_pos + pattern_len;
                search_start = end;

                if is_excluded(abs_pos, end, excluded) {
                    continue;
                }

                // Look backward up to 30 bytes for 的 + noun
                let lookback_start = abs_pos.saturating_sub(30);
                let lookback_start = text.floor_char_boundary(lookback_start);
                let lookback = &text[lookback_start..abs_pos];

                let has_didactic_noun = NOUNS.iter().any(|noun| {
                    let prefix = format!("的{noun}");
                    lookback.contains(&prefix)
                });

                if has_didactic_noun {
                    // Find the full span: from 的noun to verb+obj
                    let full_start = NOUNS
                        .iter()
                        .filter_map(|noun| {
                            let prefix = format!("的{noun}");
                            lookback.rfind(&prefix).map(|i| lookback_start + i)
                        })
                        .min()
                        .unwrap_or(abs_pos);

                    let full_text = &text[full_start..end];

                    issues.push(ai_style_issue(
                        full_start,
                        full_text,
                        "",
                        "AI didactic pattern; technical articles rarely use moralizing conclusions",
                        Severity::Info,
                    ));
                }
            }
        }
    }
}

// Vague exaggeration patterns: AI-typical claims like "領先時代 N 年" without
// technical substance.
// Pattern: (領先|超前|超越)(時代|業界|同期)...N年
pub(super) fn scan_ai_vague_exaggeration(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const VERBS: &[&str] = &["領先", "超前", "超越"];
    const OBJECTS: &[&str] = &["時代", "業界", "同期"];

    for verb in VERBS {
        let verb_len = verb.len();
        let mut search_start = 0;

        while let Some(pos) = text[search_start..].find(verb) {
            let abs_pos = search_start + pos;
            let verb_end = abs_pos + verb_len;
            search_start = verb_end;

            if is_excluded(abs_pos, verb_end, excluded) {
                continue;
            }

            // Look forward up to 30 chars for object + optional gap + digits +
            // 年
            let lookahead_end = text.len().min(verb_end + 60);
            let lookahead_end = text.ceil_char_boundary(lookahead_end);
            let lookahead = &text[verb_end..lookahead_end];

            // The claim is a lead measured in years, so the digits have to run
            // into the 年 itself. Any digit plus any 年 in the window also
            // matched a sentence that merely mentioned a calendar year, and the
            // span then ended at whichever 年 came first rather than at the one
            // that matched. A duration carries one or two digits; four is a
            // year, not a lead.
            let duration_end = OBJECTS.iter().find_map(|obj| {
                let obj_pos = lookahead.find(obj)?;
                let after_obj_start = obj_pos + obj.len();
                let after_obj = &lookahead[after_obj_start..];
                let win_end = after_obj.floor_char_boundary(after_obj.len().min(20));
                let mut digits = 0usize;
                for (i, c) in after_obj[..win_end].char_indices() {
                    if c == '年' && (1..=2).contains(&digits) {
                        return Some(verb_end + after_obj_start + i + '年'.len_utf8());
                    }
                    digits = if c.is_ascii_digit() { digits + 1 } else { 0 };
                }
                None
            });

            if let Some(pattern_end) = duration_end {
                let full_text = &text[abs_pos..pattern_end];

                issues.push(ai_style_issue(
                    abs_pos,
                    full_text,
                    "",
                    "AI vague exaggeration; replace with concrete technical comparison",
                    Severity::Info,
                ));
            }
        }
    }
}

// Density thresholds for AI phrase detection. Each entry: (phrase, threshold
// per 1000 chars, max_acceptable count suggestion). Calibrated from x86.md
// field review data.
const DENSITY_TRACKED_PHRASES: &[(&str, f32, u32)] = &[
    ("更重要的是", 0.5, 5),
    ("值得注意的是", 0.3, 3),
    ("這意味著", 0.5, 5),
    ("不容忽視", 0.2, 2),
    ("深刻影響", 0.3, 3),
    ("從某種意義上", 0.2, 2),
    ("從某種程度上", 0.2, 2),
    ("需要注意的是", 0.3, 3),
    ("在某種程度上", 0.2, 2),
    ("在這個過程中", 0.3, 3),
];

// Post-scan density pass: count tracked phrases across the full document. When
// density (count / text_len_chars * 1000) exceeds the per-phrase threshold,
// emit a single summary AiStyle issue at the first occurrence with density
// stats. Does NOT duplicate per-occurrence ai_filler detection: this catches
// the statistical signature that only becomes visible at document level.
pub(crate) fn scan_ai_density(em: &mut Emitter<'_>, threshold_multiplier: f32) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let char_count = text.chars().count();

    // Skip density analysis on short texts (< 500 chars): not enough
    // statistical signal to distinguish AI from human.
    if char_count < 500 {
        return;
    }
    let text_k = char_count as f32 / 1000.0;

    for &(phrase, threshold, max_acceptable) in DENSITY_TRACKED_PHRASES {
        let phrase_len = phrase.len();
        let mut count: u32 = 0;
        let mut first_offset: Option<usize> = None;
        let mut search_start = 0;

        while let Some(pos) = text[search_start..].find(phrase) {
            let abs_pos = search_start + pos;
            search_start = abs_pos + phrase_len;

            if is_excluded(abs_pos, abs_pos + phrase_len, excluded) {
                continue;
            }
            count += 1;
            first_offset.get_or_insert(abs_pos);
        }

        if count == 0 {
            continue;
        }

        let density = count as f32 / text_k;
        let effective_threshold = threshold * threshold_multiplier;
        if density > effective_threshold {
            // Set on the first hit, and this branch needs count > 0.
            let Some(offset) = first_offset else {
                continue;
            };
            let ctx = format!(
                "AI density: \u{300C}{phrase}\u{300D} 在本文出現 {count} 次 \
                 ({density:.1}次/千字，閾值 {effective_threshold:.1})，\
                 疑似 AI 生成的轉折公式。建議減至 {max_acceptable} 次以內。"
            );
            issues.push(ai_style_issue(offset, phrase, "", &ctx, Severity::Warning));
        }
    }
}

// Structural AI pattern detectors

/// Returns true if the byte range [start, end) is entirely within an exclusion
/// zone.
pub(super) fn is_para_excluded(start: usize, end: usize, excluded: &[ByteRange]) -> bool {
    excluded.iter().any(|r| r.start <= start && end <= r.end)
}

// Binary contrast density: AI overuses paired transition patterns. Counts
// intra-sentence double turns, progressive, and concessive patterns. Threshold:
// >5 per 1000 chars is AI-typical (human baseline: 2-3).
/// Offset of a binary-contrast construction in `sentence`: a start word with
/// one of its turn words somewhere after it.
///
/// Only the first start word that occurs at all is considered, whether or not
/// a turn follows it, which is what keeps one sentence from being counted
/// twice for the same construction.
fn contrast_hit(sentence: &str, starts: &[&str], turns: &[&str]) -> Option<usize> {
    for &start_word in starts {
        let Some(pos) = sentence.find(start_word) else {
            continue;
        };
        let after = &sentence[pos + start_word.len()..];
        return turns.iter().any(|turn| after.contains(turn)).then_some(pos);
    }
    None
}

pub(super) fn scan_ai_binary_contrast(em: &mut Emitter<'_>, threshold_multiplier: f32) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let char_count = text.chars().count();
    if char_count < 500 {
        return;
    }

    // Split into sentences (approximate: split on sentence-ending punctuation).
    let mut count: u32 = 0;
    let mut first_offset: Option<usize> = None;

    // Concessive: 雖然/儘管/即便 ... 但/卻/然而
    let concessive_starts: &[&str] = &["雖然", "儘管", "即便", "即使"];
    let concessive_turns: &[&str] = &["但", "卻", "然而", "不過"];

    // Progressive: 不僅/不只/不單 ... 更/還/也/亦
    let progressive_starts: &[&str] = &["不僅", "不只", "不單"];
    let progressive_turns: &[&str] = &["更", "還", "也", "亦"];

    // Scan paragraphs (split on double newline).
    for (_, para) in crate::engine::scan::split_paragraphs(text) {
        let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
        if is_para_excluded(para_start, para_start + para.len(), excluded) {
            continue;
        }
        // Scan sentences within paragraph (split on 。！？).
        for sentence in para.split(['。', '！', '？']) {
            let sent_start = sentence.as_ptr() as usize - text.as_ptr() as usize;
            let patterns = [
                (concessive_starts, concessive_turns),
                (progressive_starts, progressive_turns),
            ];
            for (starts, turns) in patterns {
                let Some(pos) = contrast_hit(sentence, starts, turns) else {
                    continue;
                };

                // Per match, not per paragraph: the gate above only skips a
                // paragraph that is wholly excluded, so a contrast pair inside
                // an inline code span in prose still reached the count.
                let abs = sent_start + pos;
                if is_excluded(abs, abs + 1, excluded) {
                    continue;
                }
                count += 1;
                first_offset.get_or_insert(abs);
            }
        }
    }

    let text_k = char_count as f32 / 1000.0;
    let density = count as f32 / text_k;
    let effective_threshold = 5.0 * threshold_multiplier;
    if density > effective_threshold && count >= 3 {
        let offset = first_offset.unwrap_or(0);
        let ctx = format!(
            "AI structural: 二元對比句式出現 {count} 次 ({density:.1}次/千字，\
             閾值 {effective_threshold:.1})，疑似 AI 慣用的對立轉折模式。"
        );
        issues.push(
            ai_style_issue(offset, "", "", &ctx, Severity::Info)
                .with_structural_family(StructuralFamily::BinaryContrast),
        );
    }
}

// Paragraph-ending formulaic declarations. AI closes paragraphs with stock
// phrases like:
//   這...證明/揭示...
//   這...成為...的基礎/基石/起點
//   正是這...讓...
// Flag when 3+ paragraphs end with such patterns.
pub(super) fn scan_ai_paragraph_endings(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = crate::engine::scan::split_paragraphs(text)
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| {
            if p.trim().is_empty() {
                return false;
            }
            let start = p.as_ptr() as usize - text.as_ptr() as usize;
            !is_para_excluded(start, start + p.len(), excluded)
        })
        .collect();
    if paragraphs.len() < 5 {
        return;
    }

    let ending_patterns: &[&str] = &[
        "的基礎",
        "的基石",
        "的起點",
        "的關鍵",
        "的核心",
        "證明了",
        "揭示了",
        "展示了",
        "體現了",
        "由此可見",
        "這說明了",
        "不難發現",
        "這提示我們",
        "這也印證了",
        "這反映了",
    ];
    let prefix_patterns: &[&str] = &["正是這", "正是在", "這也正是"];

    let mut match_count = 0;
    let mut first_offset: Option<usize> = None;

    for para in &paragraphs {
        let trimmed = para.trim();
        // Check last ~30 chars of paragraph (approximate ending).
        let check_len = trimmed.len().min(90); // ~30 CJK chars
        let tail_start = trimmed.len().saturating_sub(check_len);
        let tail = &trimmed[trimmed.floor_char_boundary(tail_start)..];

        let mut matched = false;
        for &pat in ending_patterns {
            if tail.contains(pat) {
                matched = true;
                break;
            }
        }
        if !matched {
            for &pat in prefix_patterns {
                if tail.contains(pat) {
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            match_count += 1;
            if first_offset.is_none() {
                let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
                first_offset = Some(para_start);
            }
        }
    }

    if match_count >= 3 {
        let total = paragraphs.len();
        let offset = first_offset.unwrap_or(0);
        let ctx = format!(
            "AI structural: {total} 個段落中 {match_count} 個以公式化宣言結尾 \
             (的基礎/證明了/正是這...)，疑似 AI 總結模式。"
        );
        issues.push(
            ai_style_issue(offset, "", "", &ctx, Severity::Info)
                .with_structural_family(StructuralFamily::ParagraphEndings),
        );
    }
}

// Dash overuse: flag when many paragraphs contain ≥3 em-dashes. AI writing
// overuses parenthetical dashes for elaboration.
pub(super) fn scan_ai_dash_overuse(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = crate::engine::scan::split_paragraphs(text)
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| {
            if p.trim().is_empty() {
                return false;
            }
            let start = p.as_ptr() as usize - text.as_ptr() as usize;
            !is_para_excluded(start, start + p.len(), excluded)
        })
        .collect();
    if paragraphs.len() < 3 {
        return;
    }

    let mut heavy_dash_count = 0;
    let mut first_offset: Option<usize> = None;

    for para in &paragraphs {
        let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
        let dash_count = count_non_excluded_matches(para, para_start, "—", excluded).0;
        if dash_count >= 3 {
            heavy_dash_count += 1;
            if first_offset.is_none() {
                first_offset = Some(para_start);
            }
        }
    }

    // Flag when ≥3 paragraphs have heavy dash usage.
    if heavy_dash_count >= 3 {
        let total = paragraphs.len();
        let offset = first_offset.unwrap_or(0);
        let ctx = format!(
            "AI structural: {total} 個段落中 {heavy_dash_count} 個含 ≥3 個破折號，\
             疑似 AI 過度使用插入說明。"
        );
        issues.push(
            ai_style_issue(offset, "", "", &ctx, Severity::Info)
                .with_structural_family(StructuralFamily::DashOveruse),
        );
    }
}

// Formulaic section headings: AI generates stereotyped heading patterns. These
// are only meaningful in Markdown/structured text where headings are explicit.
// Detects patterns in lines starting with # or ##.
const FORMULAIC_HEADINGS: &[&str] = &[
    "挑戰與未來展望",
    "結論與展望",
    "挑戰與機遇",
    "問題與挑戰",
    "優勢與劣勢",
    "現狀與未來",
    "回顧與展望",
    "總結與展望",
    "影響與意義",
    "發展與演變",
];

pub(super) fn scan_ai_formulaic_headings(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let mut match_count = 0;
    let mut first_offset: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        // Check lines that look like Markdown headings.
        if !trimmed.starts_with('#') {
            continue;
        }
        let line_start = line.as_ptr() as usize - text.as_ptr() as usize;
        if is_para_excluded(line_start, line_start + line.len(), excluded) {
            continue;
        }
        // Strip leading # and whitespace.
        let heading_text = trimmed.trim_start_matches('#').trim();
        for &pattern in FORMULAIC_HEADINGS {
            if heading_text.contains(pattern) {
                match_count += 1;
                if first_offset.is_none() {
                    let line_start = line.as_ptr() as usize - text.as_ptr() as usize;
                    first_offset = Some(line_start);
                }
                break;
            }
        }
    }

    // A single formulaic heading might be legitimate; flag ≥2.
    if match_count >= 2 {
        let offset = first_offset.unwrap_or(0);
        let ctx = format!(
            "AI structural: 發現 {match_count} 個公式化標題 \
             (挑戰與展望/結論與展望...)，疑似 AI 生成的章節結構。"
        );
        issues.push(
            ai_style_issue(offset, "", "", &ctx, Severity::Info)
                .with_structural_family(StructuralFamily::FormulaicHeadings),
        );
    }
}

// Enumerated list density: count list-containing paragraphs relative to total.
// AI writing overuses bullet/numbered lists for organization. Flag when
// list-paragraph ratio exceeds 40%.
pub(super) fn scan_ai_list_density(em: &mut Emitter<'_>, threshold_multiplier: f32) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = crate::engine::scan::split_paragraphs(text)
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| {
            if p.trim().is_empty() {
                return false;
            }
            let start = p.as_ptr() as usize - text.as_ptr() as usize;
            !is_para_excluded(start, start + p.len(), excluded)
        })
        .collect();
    if paragraphs.len() < 5 {
        return;
    }

    let mut list_para_count = 0;
    let mut first_offset: Option<usize> = None;

    for para in &paragraphs {
        let has_list = para.lines().any(|line| {
            let t = line.trim();
            // Markdown unordered list items.
            t.starts_with("- ") || t.starts_with("* ")
            // Markdown ordered list items.
            || (t.len() > 2
                && t.as_bytes()[0].is_ascii_digit()
                && (t.contains(". ") && t.find(". ").unwrap() < 4))
        });
        if has_list {
            list_para_count += 1;
            if first_offset.is_none() {
                let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
                first_offset = Some(para_start);
            }
        }
    }

    let total = paragraphs.len();
    let ratio = list_para_count as f32 / total as f32;
    let effective_threshold = 0.4 * threshold_multiplier;
    if ratio > effective_threshold && list_para_count >= 3 {
        let pct = (ratio * 100.0) as u32;
        let offset = first_offset.unwrap_or(0);
        let ctx = format!(
            "AI structural: 全文 {total} 段落中 {list_para_count} 個含列表 \
             ({pct}%)，疑似 AI 結構化傾向。"
        );
        issues.push(
            ai_style_issue(offset, "", "", &ctx, Severity::Info)
                .with_structural_family(StructuralFamily::ListDensity),
        );
    }
}

// Detect invisible-character residue and emit per-occurrence AiStyle issues.
// Which code points count, and the context filtering that spares valid emoji
// ZWJ sequences, ideographic variation selectors, file-start BOMs and bidi
// marks, both live in ai_score so the scanner and the document-level score
// cannot disagree about what needs rewriting. Suggestion is empty string so the
// fixer strips them automatically.
pub(crate) fn scan_ai_zero_width(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if !crate::engine::ai_score::has_zero_width(text) {
        return;
    }
    let mut byte_offset = 0;
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().copied().enumerate() {
        let ch_len = ch.len_utf8();

        // Gate on the cheap candidate test first, as count_zero_width does: the
        // neighbour analysis costs 2.6 ms per 2 MB unprefiltered against 0.9 ms
        // with it, and ordinary prose falls through the catch-all arm.
        if crate::engine::ai_score::is_zero_width_candidate(ch)
            && crate::engine::ai_score::is_suspicious_zero_width_at(&chars, index)
            && !is_excluded(byte_offset, byte_offset + ch_len, excluded)
        {
            let label = crate::engine::ai_score::describe_zero_width(ch);
            let ctx = format!("AI token: 隱形字元 {label}，疑似 LLM 分詞器或複製貼上殘留。");
            let found: String = ch.into();
            issues.push(
                Issue::new(
                    byte_offset,
                    ch_len,
                    &found,
                    vec![String::new()],
                    IssueType::AiStyle,
                    Severity::Info,
                )
                .with_context(&ctx),
            );
        }
        byte_offset += ch_len;
    }
}

// Structural AI detectors (require BoundaryIndex)

// Tricolon detection: three 、-separated spans with identical char length, or
// identical sentence-final particles.
pub(super) fn scan_ai_tricolon(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];

        // Strip sentence-final punctuation so the trailing span's char count
        // matches its peers (團結、奮鬥、創新。 should be three 2-char spans).
        let stripped_end = s
            .trim_end_matches(['。', '！', '？', '；', '.', '!', '?'])
            .len();
        let s = &s[..stripped_end];

        // Build (byte_start, byte_end, char_count) for each 、-separated span.
        // Tracking offsets explicitly avoids the s.find(span) hazard where
        // repeated spans (e.g. 乙、甲、甲) collapse to the first occurrence.
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        let mut span_start = 0usize;
        for (idx_byte, _) in s.match_indices('、') {
            let char_count = s[span_start..idx_byte].chars().count();
            spans.push((span_start, idx_byte, char_count));
            span_start = idx_byte + '、'.len_utf8();
        }
        // Final span after the last 、.
        if span_start <= s.len() {
            let char_count = s[span_start..].chars().count();
            spans.push((span_start, s.len(), char_count));
        }
        if spans.len() < 3 {
            continue;
        }
        // Check consecutive triples for identical char-count pattern.
        for window in spans.windows(3) {
            let (s0_start, _, len0) = window[0];
            let (_, _, len1) = window[1];
            let (_, s2_end, len2) = window[2];
            if len0 == len1 && len1 == len2 && len0 > 0 && len0 <= 8 {
                let abs_start = sent.byte_start + s0_start;
                let abs_end = sent.byte_start + s2_end;
                if !is_excluded(abs_start, abs_end, excluded) {
                    issues.push(
                        Issue::new(
                            abs_start,
                            abs_end - abs_start,
                            &text[abs_start..abs_end],
                            vec![],
                            IssueType::AiStyle,
                            Severity::Info,
                        )
                        .with_context(
                            "AI structural: 三連排比（tricolon）— \
                             三個等長的、分隔片段，常見於 AI 生成文本",
                        )
                        .with_structural_family(StructuralFamily::Tricolon),
                    );
                }
                break; // One tricolon per sentence is enough.
            }
        }
    }
}

// Negative parallel: 不只是/不僅是 plus 而是/更是 within 30 chars.
pub(super) fn scan_ai_negative_parallel(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const OPENERS: &[&str] = &["不只是", "不僅是", "不僅僅是"];
    const CLOSERS: &[&str] = &["而是", "更是"];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        for opener in OPENERS {
            let Some(pos) = s.find(opener) else {
                continue;
            };
            let after_opener = pos + opener.len();
            // 30-char lookahead, char-boundary safe (not byte-truncated).
            let search_end = char_bounded_end(s, after_opener, 30);
            let window = &s[after_opener..search_end];

            // The first closer that appears decides the span; a second one in
            // the same window describes the same construction.
            let hit = CLOSERS
                .iter()
                .find_map(|closer| window.find(closer).map(|cpos| (cpos, closer)));
            let Some((cpos, closer)) = hit else {
                continue;
            };

            let abs_start = sent.byte_start + pos;
            let abs_end = sent.byte_start + after_opener + cpos + closer.len();
            if is_excluded(abs_start, abs_end, excluded) {
                continue;
            }
            issues.push(
                ai_style_issue(
                    abs_start,
                    &text[abs_start..abs_end],
                    "",
                    "AI structural: 否定平行結構（不只是…而是/更是），AI 常用公式",
                    Severity::Info,
                )
                .with_structural_family(StructuralFamily::NegativeParallel),
            );
        }
    }
}

/// What follows byte offset `pos`, as far as section boundaries are concerned.
///
/// This is what makes the detector below match its name. Without it, "section
/// ending" phrases fire anywhere they appear, and 展望未來 in the middle of a
/// paragraph in the middle of a document is ordinary prose, not a formulaic
/// closer.
///
enum SectionBoundary {
    /// The next non-blank line opens a new section.
    Heading,
    /// Nothing follows.
    DocEnd,
    /// More body text: not a boundary.
    Body,
}

/// Per-document answers for the two questions a closing-phrase check asks of
/// every sentence: where its line ends, and whether anything before that is
/// not closing punctuation.
///
/// Built once and searched, because both were forward walks from each
/// sentence. An unwrapped paragraph has no line break to find, and a run of
/// full stops passes the tail test at every position, so each was quadratic
/// on a document that is otherwise ordinary.
pub(super) struct CloserTailIndex {
    line_breaks: Vec<usize>,
    non_tail: Vec<usize>,
    /// Positions of 「（」 and 「註」, the only characters an accepted tail may
    /// hold beyond whitespace and closing punctuation, and only as part of a
    /// 「（註）」 prefix.
    annotation: Vec<usize>,
}

impl CloserTailIndex {
    pub(super) fn build(text: &str) -> Self {
        crate::engine::index_guard::note_build(crate::engine::index_guard::DocIndex::CloserTail);
        let mut line_breaks = Vec::new();
        let mut non_tail = Vec::new();
        let mut annotation = Vec::new();
        for (i, ch) in text.char_indices() {
            if ch == '\n' || ch == '\r' {
                line_breaks.push(i);
            } else if ch == '（' || ch == '註' {
                annotation.push(i);
            } else if !may_open_closer_tail(ch) {
                non_tail.push(i);
            }
        }
        Self {
            line_breaks,
            non_tail,
            annotation,
        }
    }

    /// Offset of the first line break at or after `pos`.
    pub(super) fn next_line_break(&self, pos: usize) -> Option<usize> {
        self.line_breaks
            .get(self.line_breaks.partition_point(|&b| b < pos))
            .copied()
    }

    /// Whether `text[start..end)` holds a character that no accepted closing
    /// tail can contain.
    pub(super) fn has_non_tail_char(&self, start: usize, end: usize) -> bool {
        any_position_in(&self.non_tail, start, end)
    }

    /// Whether `text[start..end)` holds a 「（註）」 character, which is the
    /// only
    /// case the exact predicate has to decide.
    fn has_annotation(&self, start: usize, end: usize) -> bool {
        any_position_in(&self.annotation, start, end)
    }
}

fn section_boundary_after(
    text: &str,
    pos: usize,
    idx: &CloserTailIndex,
    markdown_blocks: Option<&[usize]>,
) -> SectionBoundary {
    if pos > text.len() {
        return SectionBoundary::DocEnd;
    }

    // A sentence can end before harmless trailing punctuation such as （註）.
    // It still has to be the last prose on its physical line: otherwise a later
    // heading would make a mid-paragraph phrase look like a closer.
    //
    // Both questions are answered from indexes built once for the document,
    // because this runs per sentence and both walks were unbounded. Searching
    // for the line end rescanned an unwrapped paragraph to its end every time,
    // and walking the tail rescanned a run of sentence punctuation: 60,000 full
    // stops in 176 KB cost 9.4 seconds. A byte cap on either walk trades the
    // blowup for a tail length that is silently not a closer, so index both
    // instead and keep the answer exact.
    let line_end = idx.next_line_break(pos).unwrap_or(text.len());
    let after_line = &text[line_end..];

    // The index decides outright when the tail is only whitespace and closing
    // punctuation, which is every real case and the one that was quadratic.
    if idx.has_non_tail_char(pos, line_end) {
        return SectionBoundary::Body;
    }

    // Only a 「（註）」 annotation needs the exact predicate, and the index
    // says whether one is present, so a long uniform run never reaches the walk
    // that reading the tail would cost.
    if idx.has_annotation(pos, line_end) && !is_allowed_section_closer_tail(&text[pos..line_end]) {
        return SectionBoundary::Body;
    }
    let line_base = line_end;

    // split_inclusive rather than lines, which does not treat a bare CR as a
    // terminator and would fold a CR-delimited file into one apparent line.
    let next_line = after_line
        .split_inclusive(['\n', '\r'])
        .scan(line_base, |offset, raw| {
            let start = *offset;
            *offset += raw.len();
            Some((start, raw.trim_end_matches(['\n', '\r'])))
        })
        .find(|(_, line)| !line.trim().is_empty());

    // Deliberately no exclusion test here. An earlier revision rejected a
    // heading-shaped line overlapping an exclusion range, meaning to skip a
    // shell comment inside a code block. is_excluded tests overlap rather than
    // containment, and the ranges cover inline code, paths, URLs and mentions,
    // so a heading naming a file in backticks stopped being a boundary and the
    // closer before it went unreported. On technical Markdown, where headings
    // routinely name files and config keys, the detector went silent
    // altogether.
    //
    // It bought nothing either: a comment inside a fenced block is already
    // unreachable, because the fence opener is the first non-blank line and is
    // not heading-shaped, and an indented block needs four spaces, which the
    // indent rule rejects. The parser index only ever *adds* boundaries it
    // alone can see, such as a setext underline. The line-shape test stays as
    // the fallback because most Markdown never arrives labelled: "ContentType"
    // defaults to "Plain", so an MCP caller passing Markdown text with no
    // filename, or a ".txt" file of Markdown, would otherwise lose the detector
    // everywhere but the last sentence of the document.
    let Some((start, line)) = next_line else {
        return SectionBoundary::DocEnd;
    };
    let opens_section = match markdown_blocks {
        // The parser is authoritative when we have it. Or-ing the line shape in
        // would undo its main advantage: under MarkdownScanCode a shell comment
        // inside a fence is scanned prose, and the parser refuses to call it a
        // heading.
        Some(blocks) => blocks.binary_search(&start).is_ok(),

        // Plain text has no parser index, and nothing in this crate sniffs an
        // unlabelled document, so ContentType defaults to Plain and the
        // line-shape test is all a bare .txt or an MCP call without a filename
        // ever gets.
        None => is_markdown_heading_line(line),
    };
    if opens_section {
        SectionBoundary::Heading
    } else {
        SectionBoundary::Body
    }
}

/// Whether text after a sentence-ending closer contains only allowed adornment.
///
/// The sentence index ends at its first terminal punctuation mark, leaving
/// repeated punctuation and a conventional "（註）" note in the line tail.  Do
/// not accept arbitrary parentheticals here: they are prose, not a boundary.
/// Whether a character can stand alone in an accepted closing tail.
///
/// Deliberately excludes 「（」 and 「註」, which are only legal as part of a
/// 「（註）」 prefix. A tail containing either falls through to
/// "is_allowed_section_closer_tail", which owns that rule; a tail without one
/// is decided by the index alone, which is what keeps a long run of full
/// stops from being rewalked per sentence.
fn may_open_closer_tail(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '。' | '！' | '？' | '!' | '?' | '…' | '、' | '，' | ')' | '）' | '】' | '」' | '』'
        )
}

fn is_allowed_section_closer_tail(tail: &str) -> bool {
    let mut rest = tail.trim();
    while let Some(after_note) = rest.strip_prefix("（註）") {
        rest = after_note.trim_start();
    }
    rest.chars().all(|ch| {
        matches!(
            ch,
            '。' | '！' | '？' | '!' | '?' | '…' | '、' | '，' | ')' | '）' | '】' | '」' | '』'
        )
    })
}

/// An ATX heading, per CommonMark: at most three spaces of indentation, then
/// one to six hashes, then a space, a tab or the end of the line.
///
/// The indent limit is load-bearing rather than pedantic. Four spaces starts an
/// indented code block, so a commented line inside one (`    # setup`) would
/// otherwise read as a section boundary and let the detector fire on the prose
/// before a code sample. A tab reaches column four on its own, so it cannot
/// introduce a heading either.
pub(super) fn is_markdown_heading_line(line: &str) -> bool {
    let indent = line.bytes().take_while(|&b| b == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    if rest.starts_with('\t') {
        return false;
    }
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && matches!(rest.as_bytes().get(hashes), None | Some(b' ' | b'\t'))
}

fn is_markdown_heading_sentence(text: &str) -> bool {
    !text.contains(['\n', '\r']) && is_markdown_heading_line(text)
}

fn is_in_markdown_heading_line(text: &str, pos: usize) -> bool {
    let line_start = text[..pos].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let line_end = text[pos..]
        .find(['\n', '\r'])
        .map_or(text.len(), |i| pos + i);
    is_markdown_heading_line(&text[line_start..line_end])
}

// Formulaic section endings: the last sentence of a section-closing paragraph,
// matching formulaic closing phrases.
/// Closing formulas. Hoisted to module scope so the prefilter automaton and
/// the per-sentence scan share one list.
const FORMULAIC_ENDINGS: &[&str] = &[
    "展望未來",
    "拭目以待",
    "值得期待",
    "我們有理由相信",
    "具有重要意義",
    "具有重要戰略意義",
    "攜手共進",
    "值得深思",
    "任重道遠",
    "值得持續觀察",
    "值得持續關注",
    "機會與風險並存",
    "未來可期",
    "前景可期",
    "添磚加瓦",
    "開啟新篇章",
    "讓我們共同期待",
    "讓我們一起見證",
    "讓我們並肩前行",
    "感謝您的閱讀",
    "希望這篇文章對您有所幫助",
    "希望這些資訊對你有幫助",
    "如有不足之處",
    "歡迎在留言區",
    "歡迎在評論區",
];

/// One automaton over [`FORMULAIC_ENDINGS`].
///
/// Doubles as the prefilter that decides whether the closing-tail index is
/// worth building, and replaces the per-phrase substring scans that ran once
/// per closing sentence.
fn formulaic_ending_ac() -> &'static AhoCorasick {
    use std::sync::OnceLock;
    static AC: OnceLock<AhoCorasick> = OnceLock::new();
    AC.get_or_init(|| {
        AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(FORMULAIC_ENDINGS)
            .expect("formulaic ending patterns are valid")
    })
}

pub(super) fn scan_ai_formulaic_section_endings(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
    markdown_blocks: Option<&[usize]>,
) {
    let text = em.text;

    // Once for the document, and only when the document has a closing phrase in
    // it at all. flag_closing_phrases runs per paragraph, so building the index
    // there scanned and allocated over the whole text per paragraph: 0.77s to
    // 1.63s on 4,000 paragraphs, growing with their product. Building it here
    // regardless still walked every character of every document, including the
    // majority that carry none of these phrases, so the prefilter decides and
    // the index follows.
    let tail_index = formulaic_ending_ac()
        .find_iter(text)
        .next()
        .map(|_| CloserTailIndex::build(text));
    for para in &idx.paragraphs {
        let sents = idx.sentence_slice(para);
        if let Some(tail_index) = &tail_index {
            flag_closing_phrases(em, sents, tail_index, markdown_blocks);
        }
        flag_significance_stamps(em, sents);
    }
}

/// Closing phrases, which are only a tell where a section actually closes.
///
/// The position gate applies to these and not to the patterns in
/// [`flag_significance_stamps`]: 展望未來 is a closer, so it is a tell at a
/// close and ordinary prose in the middle of a paragraph. A significance stamp
/// and 隨著…不斷發展 are tells wherever they sit, so gating those would have
/// deleted them.
fn flag_closing_phrases(
    em: &mut Emitter<'_>,
    sents: &[crate::engine::sentence::SentenceBound],
    tail_index: &CloserTailIndex,
    markdown_blocks: Option<&[usize]>,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // A sentence followed by a heading is a close even when body text follows
    // that heading inside the same paragraph, so the walk runs forwards over
    // every sentence. Searching backwards for a single candidate found the
    // document's last sentence instead and missed the real close.
    let mut body = sents
        .iter()
        .copied()
        .filter(|s| !is_markdown_heading_sentence(&text[s.byte_start..s.byte_end]))
        .peekable();

    // The document's final sentence is a close too, which is what the DocEnd
    // arm covers: peek is what makes that sentence the final one.
    while let Some(sent) = body.next() {
        let closes = match section_boundary_after(text, sent.byte_end, tail_index, markdown_blocks)
        {
            SectionBoundary::Heading => true,
            SectionBoundary::DocEnd => body.peek().is_none(),
            SectionBoundary::Body => false,
        };
        if !closes {
            continue;
        }
        let s = &text[sent.byte_start..sent.byte_end];
        for &phrase in FORMULAIC_ENDINGS {
            // First occurrence that is neither on a heading line nor excluded;
            // later ones in the same sentence add nothing.
            let hit = s
                .match_indices(phrase)
                .map(|(pos, _)| sent.byte_start + pos)
                .find(|&abs| {
                    !is_in_markdown_heading_line(text, abs)
                        && !is_excluded(abs, abs + phrase.len(), excluded)
                });
            let Some(abs) = hit else {
                continue;
            };
            issues.push(
                ai_style_issue(
                    abs,
                    phrase,
                    "",
                    "AI structural: 公式化用語，常見於 AI 生成文本",
                    Severity::Info,
                )
                .with_structural_family(StructuralFamily::FormulaicClosing),
            );
        }
    }
}

/// Significance stamps and 隨著…不斷發展, which are tells at any position.
fn flag_significance_stamps(
    em: &mut Emitter<'_>,
    sents: &[crate::engine::sentence::SentenceBound],
) {
    let (text, excluded) = (em.text, em.excluded);

    const FORMULAIC_PAIRS: &[(&str, &str)] = &[
        ("奠定", "理論基礎"),
        ("提供", "重要框架"),
        ("發揮", "關鍵作用"),
        ("印證", "重要性"),
    ];

    for sent in sents {
        let s = &text[sent.byte_start..sent.byte_end];
        for &(start_phrase, end_phrase) in FORMULAIC_PAIRS {
            let Some(start) = s.find(start_phrase) else {
                continue;
            };
            let Some(end_pos) = s[start + start_phrase.len()..].find(end_phrase) else {
                continue;
            };
            let end = start + start_phrase.len() + end_pos + end_phrase.len();
            let abs = sent.byte_start + start;
            let abs_end = sent.byte_start + end;
            if is_excluded(abs, abs_end, excluded) {
                continue;
            }
            em.issues.push(
                ai_style_issue(
                    abs,
                    &text[abs..abs_end],
                    "",
                    "AI structural: 意義蓋章式收尾，常見於 AI 生成文本",
                    Severity::Info,
                )
                .with_structural_family(StructuralFamily::SignificanceStamp),
            );
        }

        flag_gradual_development(em, sent);
    }
}

/// 隨著...不斷發展 with a gap of at most 40 characters, which may be zero.
fn flag_gradual_development(em: &mut Emitter<'_>, sent: &crate::engine::sentence::SentenceBound) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let s = &text[sent.byte_start..sent.byte_end];

    // Two bindings rather than one tuple: a tuple evaluates both arms, so the
    // second search would run over every sentence even though 隨著 is rare.
    let Some(start) = s.find("隨著") else {
        return;
    };
    let Some(end_pos) = s.find("不斷發展") else {
        return;
    };
    let after_kw = start + "隨著".len();
    // Out of order is not the pattern.
    if end_pos < after_kw || s[after_kw..end_pos].chars().count() > 40 {
        return;
    }
    let abs = sent.byte_start + start;
    let abs_end = sent.byte_start + end_pos + "不斷發展".len();
    if is_excluded(abs, abs_end, excluded) {
        return;
    }
    issues.push(
        ai_style_issue(
            abs,
            &text[abs..abs_end],
            "",
            "AI structural: 公式化用語（隨著…不斷發展）",
            Severity::Info,
        )
        .with_structural_family(StructuralFamily::EraOpener),
    );
}

// Mechanical bullet lists: every item starts with **keyword**.
pub(super) fn scan_ai_mechanical_bullets(
    em: &mut Emitter<'_>,
    _idx: &crate::engine::sentence::BoundaryIndex,
) {
    let text = em.text;

    // Scan for Markdown list items where every item starts with **bold**.
    let mut list_start: Option<usize> = None;
    let mut bold_count = 0;
    let mut four_char_label_count = 0;
    let mut item_count = 0;
    let mut first_item_offset = 0;

    for (line_offset, line) in line_iter(text) {
        let trimmed = line.trim_start();

        // Numbered list items: one or more ASCII digits followed by '.' or ')'
        // and whitespace. Matches 1., 10., 123), etc.
        let numbered_marker_len = numbered_list_marker_len(trimmed);
        let is_list_item =
            trimmed.starts_with("- ") || trimmed.starts_with("* ") || numbered_marker_len.is_some();

        if is_list_item {
            if list_start.is_none() {
                list_start = Some(line_offset);
                // Point at the list marker itself, not the leading indentation.
                first_item_offset = line_offset + (line.len() - trimmed.len());
                bold_count = 0;
                four_char_label_count = 0;
                item_count = 0;
            }
            item_count += 1;
            // Check for leading **bold**
            let content = if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                &trimmed[2..]
            } else if let Some(marker_len) = numbered_marker_len {
                trimmed[marker_len..].trim_start()
            } else {
                ""
            };
            if content.starts_with("**") {
                bold_count += 1;
                if bold_label_is_four_chars_before_colon(content) {
                    four_char_label_count += 1;
                }
            }
        } else if list_start.is_some() {
            // End of list.
            emit_ai_mechanical_bullet_issue(
                em,
                first_item_offset,
                item_count,
                bold_count,
                four_char_label_count,
            );
            list_start = None;
        }
    }
    // Flush trailing list.
    if list_start.is_some() {
        emit_ai_mechanical_bullet_issue(
            em,
            first_item_offset,
            item_count,
            bold_count,
            four_char_label_count,
        );
    }
}

fn emit_ai_mechanical_bullet_issue(
    em: &mut Emitter<'_>,
    first_item_offset: usize,
    item_count: usize,
    bold_count: usize,
    four_char_label_count: usize,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if item_count < 3 || is_excluded(first_item_offset, first_item_offset + 1, excluded) {
        return;
    }
    let (context, family) = if four_char_label_count == item_count {
        (
            format!("AI structural: 四字標籤式列表 — {item_count} 項全部以四字粗體標籤加冒號開頭"),
            StructuralFamily::FourCharBulletLabels,
        )
    } else if bold_count == item_count {
        (
            format!("AI structural: 機械式列表 — {item_count} 項全部以粗體關鍵字開頭"),
            StructuralFamily::MechanicalBullets,
        )
    } else {
        return;
    };

    // List markers (- * digit) are ASCII, so first_item_offset + 1 is a char
    // boundary.
    let marker = &text[first_item_offset..first_item_offset + 1];
    issues.push(
        Issue::new(
            first_item_offset,
            1,
            marker,
            vec![],
            IssueType::AiStyle,
            Severity::Info,
        )
        .with_context(context)
        .with_structural_family(family),
    );
}

fn bold_label_is_four_chars_before_colon(content: &str) -> bool {
    let Some(rest) = content.strip_prefix("**") else {
        return false;
    };
    let Some(close) = rest.find("**") else {
        return false;
    };
    let label = &rest[..close];
    label.chars().count() == 4 && rest[close + 2..].trim_start().starts_with(['：', ':'])
}

fn count_non_excluded_bold_runs(text: &str, base_offset: usize, excluded: &[ByteRange]) -> usize {
    text.match_indices("**")
        .filter(|(offset, marker)| {
            let abs = base_offset + *offset;
            !is_excluded(abs, abs + marker.len(), excluded)
        })
        .count()
        / 2
}

// Excessive bold: three or more **...** runs per 200 chars in a paragraph. The
// sentence-level arm below stops at four on purpose: five or more in one
// sentence already clears this paragraph threshold, and reporting both would
// name the same prose twice.
pub(super) fn scan_ai_excessive_bold(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let bold_count = count_non_excluded_bold_runs(s, sent.byte_start, excluded);
        if (2..=4).contains(&bold_count) && !is_excluded(sent.byte_start, sent.byte_end, excluded) {
            let preview_end = char_bounded_end(s, 0, 2);
            issues.push(
                Issue::new(
                    sent.byte_start,
                    preview_end,
                    &s[..preview_end],
                    vec![],
                    IssueType::AiStyle,
                    Severity::Info,
                )
                .with_context(format!(
                    "AI structural: 句內關鍵詞粗體排比 — 單句內 {bold_count} 處粗體"
                ))
                .with_structural_family(StructuralFamily::BoldInSentence),
            );
        }
    }
    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let char_count = p.chars().count();
        if char_count < 30 {
            continue;
        }
        // Count **...** runs.
        let bold_count = count_non_excluded_bold_runs(p, para.byte_start, excluded);
        // Threshold: ≥3 per 200 chars.
        let threshold = ((char_count as f32 / 200.0) * 3.0).ceil() as usize;
        if bold_count >= 3
            && bold_count >= threshold
            && !is_excluded(para.byte_start, para.byte_start + 1, excluded)
        {
            // First 2 chars as preview, char-boundary safe.
            let preview_end = char_bounded_end(p, 0, 2);
            issues.push(
                Issue::new(
                    para.byte_start,
                    preview_end,
                    &p[..preview_end],
                    vec![],
                    IssueType::AiStyle,
                    Severity::Info,
                )
                .with_context(format!(
                    "AI structural: 段落粗體過多 — {bold_count} 處粗體標記（每 200 字 ≥3 處）"
                ))
                .with_structural_family(StructuralFamily::BoldInParagraph),
            );
        }
    }
}

pub(super) fn scan_ai_abstract_line_metaphor(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const ABSTRACT_TERMS: &[&str] = &["一條線", "路線", "軸線", "脈絡", "橋"];
    const GROWTH_VERBS: &[&str] = &["走出來", "長出來", "鋪出來", "延伸", "串起來", "拓寬"];

    // Scope per-paragraph so an abstract term in one section can't correlate
    // with anaphora in an unrelated one.
    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let Some((first_term, matched_term)) =
            first_non_excluded_any(p, para.byte_start, ABSTRACT_TERMS, excluded)
        else {
            continue;
        };
        // "這條路線" contains "這條路" as a prefix; count it once, not twice.
        let line = count_non_excluded_matches(p, para.byte_start, "這條線", excluded).0;
        let road_line = count_non_excluded_matches(p, para.byte_start, "這條路線", excluded).0;
        let road = count_non_excluded_matches(p, para.byte_start, "這條路", excluded).0;
        let anaphora_count = line + road_line + road.saturating_sub(road_line);
        if anaphora_count < 2
            || first_non_excluded_any(p, para.byte_start, GROWTH_VERBS, excluded).is_none()
        {
            continue;
        }

        issues.push(
            Issue::new(
                first_term,
                matched_term.len(),
                matched_term,
                vec![],
                IssueType::AiStyle,
                Severity::Info,
            )
            .with_context(format!(
                "AI structural: 抽象概念具象成路線並反覆回指 — 回指出現 {anaphora_count} 次"
            ))
            .with_structural_family(StructuralFamily::AbstractLineMetaphor),
        );
    }
}

pub(super) fn scan_ai_repeated_parallel_slogan(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // (slogan text, first byte offset, first paragraph index, emitted).
    let mut seen: Vec<(String, usize, usize, bool)> = Vec::new();
    for (para_idx, para) in idx.paragraphs.iter().enumerate() {
        for sent in idx.sentence_slice(para) {
            if is_excluded(sent.byte_start, sent.byte_end, excluded) {
                continue;
            }
            let raw = &text[sent.byte_start..sent.byte_end];
            let s = raw.trim();
            if !looks_like_parallel_slogan(s) {
                continue;
            }
            // Offset of the slogan itself, past any leading whitespace/newline.
            let slogan_start = sent.byte_start + (raw.len() - raw.trim_start().len());
            if let Some((_, first_offset, first_para, emitted)) =
                seen.iter_mut().find(|(prior, _, _, _)| prior == s)
            {
                // Only a slogan repeated across paragraphs is "金句疊句"; the
                // same parallel sentence repeated within one paragraph is
                // ordinary 排比.
                if *first_para != para_idx && !*emitted {
                    let offset = *first_offset;
                    let len = char_bounded_end(&text[offset..], 0, 8);
                    issues.push(
                        Issue::new(
                            offset,
                            len,
                            &text[offset..offset + len],
                            vec![],
                            IssueType::AiStyle,
                            Severity::Info,
                        )
                        .with_context("AI structural: 金句疊句 — 對仗句跨段重複出現")
                        .with_structural_family(StructuralFamily::RepeatedSlogan),
                    );
                    *emitted = true;
                }
            } else {
                seen.push((s.to_string(), slogan_start, para_idx, false));
            }
        }
    }
}

/// Discourse markers that can open a rhetorical question or its answer without
/// changing the device. Idiomatic zh-TW rarely starts the sentence on the bare
/// interrogative: "那為什麼會變慢？主要是因為…" is the same move as
/// "為什麼會變慢？因為…".
const QA_LEAD_INS: &[&str] = &[
    "那麼",
    "那",
    "但是",
    "但",
    "然而",
    "可是",
    "而",
    "所以",
    "究竟",
    "到底",
    "其實",
    "主要",
    "真正",
    "說穿了",
    "說白了",
];

/// Strip any run of leading discourse markers and the punctuation after them.
fn strip_qa_lead_in(sentence: &str) -> &str {
    let mut rest = sentence.trim();
    while let Some(next) = QA_LEAD_INS.iter().find_map(|lead| rest.strip_prefix(lead)) {
        rest = next.trim_start_matches(['，', ',', '、', ' ']);
    }
    rest
}

/// Detect repeated rhetorical self-Q&A: an essay-like paragraph that asks its
/// own dramatic question and immediately unveils the answer.
///
/// The hard part is that chained "為什麼…？因為…" is also how Chinese textbooks
/// and technical explainers legitimately teach, so pair count alone cannot
/// separate the two. What does separate them is the staged-reveal framing:
/// "你以為…嗎？錯了" tells the reader they were wrong before saying anything,
/// which explanatory prose has no reason to do. So require both: at least two
/// pairs, and at least one of them dramatic.
pub(super) fn scan_ai_rhetorical_self_qa(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Lowercased before comparison, so a "faq:" heading suppresses too.
    const FAQ_LABELS: &[&str] = &["faq", "常見問題", "q：", "q:", "問：", "問:"];

    for para in &idx.paragraphs {
        let sentences = idx.sentence_slice(para);

        let mut pairs = 0usize;
        let mut first = None;
        let mut dramatic = false;
        for pair in sentences.windows(2) {
            if is_excluded(pair[0].byte_start, pair[0].byte_end, excluded)
                || is_excluded(pair[1].byte_start, pair[1].byte_end, excluded)
            {
                continue;
            }
            let question = strip_qa_lead_in(idx.sentence_text(text, &pair[0]));
            let answer = strip_qa_lead_in(idx.sentence_text(text, &pair[1]));
            let is_dramatic = (question.starts_with("你以為")
                || question.starts_with("大家都以為"))
                && question.contains('嗎')
                && (answer.starts_with("錯了") || answer.starts_with("錯，"));

            // "主要是因為" survives lead-in stripping as "是因為", because the
            // copula belongs to the answer rather than to the marker.
            let is_ordinary = (question.starts_with("為什麼")
                && ["因為", "是因為", "原因"]
                    .iter()
                    .any(|open| answer.starts_with(open)))
                || (question.starts_with("問題出在哪") && answer.starts_with("出在"));
            dramatic |= is_dramatic;
            if is_dramatic || is_ordinary {
                pairs += 1;
                first.get_or_insert(&pair[0]);
            }
        }

        // Two pairs alone is ordinary explanatory prose. The dramatic framing
        // is what makes the chain a tell.
        let Some(first) = first.filter(|_| pairs >= 2 && dramatic) else {
            continue;
        };

        // Checked last: it allocates a cased copy of the paragraph, and only a
        // paragraph that already looks like a hit is worth that.
        let paragraph = text[para.byte_start..para.byte_end].to_lowercase();
        if FAQ_LABELS.iter().any(|label| paragraph.contains(label)) {
            continue;
        }

        issues.push(
            Issue::new(
                first.byte_start,
                first.byte_end - first.byte_start,
                idx.sentence_text(text, first),
                vec![],
                IssueType::AiStyle,
                Severity::Info,
            )
            .with_context(format!(
                "AI structural: 連續自問自答 ×{}；刪除設問，直接陳述原因或主張",
                pairs
            ))
            .with_structural_family(StructuralFamily::RhetoricalSelfQa),
        );
    }
}

fn looks_like_parallel_slogan(sentence: &str) -> bool {
    let s = sentence
        .trim_end_matches(['。', '！', '？', '!', '?'])
        .trim();
    let char_count = s.chars().count();
    if !(8..=80).contains(&char_count) {
        return false;
    }
    if (s.contains("不是") && s.contains("而是"))
        || (s.contains("不只") && (s.contains("更是") || s.contains("也是")))
        || (s.contains('越') && s.matches('越').count() >= 2)
    {
        return true;
    }
    // Semicolon is the stronger parallel boundary; split on it before comma.
    let Some((left, right)) = s
        .split_once('；')
        .or_else(|| s.split_once('，'))
        .or_else(|| s.split_once(','))
    else {
        return false;
    };
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    left_len >= 3 && right_len >= 3 && left_len.abs_diff(right_len) <= 8
}

fn first_non_excluded_any<'a>(
    text: &str,
    base_offset: usize,
    needles: &'a [&'a str],
    excluded: &[ByteRange],
) -> Option<(usize, &'a str)> {
    needles
        .iter()
        .filter_map(|&needle| {
            count_non_excluded_matches(text, base_offset, needle, excluded)
                .1
                .map(|offset| (offset, needle))
        })
        .min_by_key(|(offset, _)| *offset)
}

fn count_non_excluded_matches(
    text: &str,
    base_offset: usize,
    needle: &str,
    excluded: &[ByteRange],
) -> (usize, Option<usize>) {
    let mut count = 0;
    let mut first_offset = None;
    let mut search_from = 0;

    while let Some(pos) = text[search_from..].find(needle) {
        let rel = search_from + pos;
        let abs = base_offset + rel;
        if !is_excluded(abs, abs + needle.len(), excluded) {
            count += 1;
            first_offset.get_or_insert(abs);
        }
        search_from = rel + needle.len();
    }

    (count, first_offset)
}

// Em-dash overuse: two or more '——' in one paragraph. A single one is ordinary
// punctuation and always has been.
pub(super) fn scan_ai_emdash_overuse(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let (count, first_offset) = count_non_excluded_matches(p, para.byte_start, "——", excluded);
        if count < 2 {
            continue;
        }
        if let Some(abs) = first_offset {
            issues.push(
                Issue::new(
                    abs,
                    "——".len(),
                    "——",
                    vec![],
                    IssueType::AiStyle,
                    Severity::Info,
                )
                .with_context(format!(
                    "AI structural: 破折號過度使用 — 段落內 {count} 處（AI 常見模式）"
                ))
                .with_structural_family(StructuralFamily::EmDashOveruse),
            );
        }
    }
}

// Formulaic 'despite': 儘管.*挑戰 plus a forward-looking verb within one
// sentence.
pub(super) fn scan_ai_formulaic_despite(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const FORWARD_VERBS: &[&str] = &["仍然", "持續", "蓬勃發展", "繼續"];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let Some(start) = s.find("儘管") else {
            continue;
        };
        let after_despite_start = start + "儘管".len();
        let Some(challenge_rel) = s[after_despite_start..].find("挑戰") else {
            continue;
        };
        let challenge = after_despite_start + challenge_rel;

        // Char-counted gap (<= 40 chars), encoding-independent.
        if s[after_despite_start..challenge].chars().count() > 40 {
            continue;
        }

        // The pattern needs a forward-looking verb after the challenge.
        let rest = &s[challenge + "挑戰".len()..];
        if !FORWARD_VERBS.iter().any(|verb| rest.contains(verb)) {
            continue;
        }

        let abs = sent.byte_start + start;
        let abs_end = sent.byte_end;
        if is_excluded(abs, abs_end, excluded) {
            continue;
        }
        issues.push(
            ai_style_issue(
                abs,
                &text[abs..abs_end],
                "",
                "AI structural: 公式化轉折（儘管…挑戰…仍然），AI 常見句型",
                Severity::Info,
            )
            .with_structural_family(StructuralFamily::FormulaicDespite),
        );
    }
}

// False ranges: 從...到...再到 chains.
pub(super) fn scan_ai_false_ranges(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let Some(cong) = s.find("從") else {
            continue;
        };
        let after_cong = cong + "從".len();
        let Some(dao) = s[after_cong..].find("到") else {
            continue;
        };
        let after_dao = after_cong + dao + "到".len();
        let Some(zaidao) = s[after_dao..].find("再到") else {
            continue;
        };
        let chain_end = after_dao + zaidao + "再到".len();

        // Only a chain of 10 or more characters reads as the AI pattern.
        if s[cong..chain_end].chars().count() < 10 {
            continue;
        }

        let abs = sent.byte_start + cong;
        let abs_end = sent.byte_start + chain_end;
        if is_excluded(abs, abs_end, excluded) {
            continue;
        }
        issues.push(
            ai_style_issue(
                abs,
                &text[abs..abs_end],
                "",
                "AI structural: 假範圍鏈（從…到…再到），AI 常見列舉模式",
                Severity::Info,
            )
            .with_structural_family(StructuralFamily::FalseRanges),
        );
    }
}

// Hedging density: promote Info to Warning at three or more hedging hits per
// 200 chars.
pub(super) fn scan_ai_hedging_density(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut [Issue],
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    const HEDGING_PHRASES: &[&str] = &["在某種程度上", "從某個角度來看", "可以說是", "相對而言"];

    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let char_count = p.chars().count();
        if char_count < 50 {
            continue;
        }
        let mut count = 0;
        for phrase in HEDGING_PHRASES {
            count += count_non_excluded_matches(p, para.byte_start, phrase, excluded).0;
        }
        // Threshold: ≥3 per 200 chars.
        let threshold = ((char_count as f32 / 200.0) * 3.0).ceil() as usize;
        if count >= 3 && count >= threshold {
            // Promote existing hedging Info issues in this paragraph to
            // Warning.
            for issue in issues.iter_mut() {
                if issue.offset >= para.byte_start
                    && issue.offset < para.byte_end
                    && issue.rule_type == IssueType::AiStyle
                    && issue.severity == Severity::Info
                {
                    if let Some(ref ctx) = issue.context {
                        if HEDGING_PHRASES
                            .iter()
                            .any(|h| ctx.contains(h) || issue.found.contains(h))
                        {
                            issue.severity = Severity::Warning;
                        }
                    }
                }
            }
        }
    }
}
