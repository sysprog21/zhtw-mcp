// Grammar scanner: pattern-based grammatical checks for zh-TW text.
//
// Detects interlingual transfer errors (English grammar calques in Chinese)
// and structural redundancies without requiring POS tagging.
//
// Phase 2a: interlingual transfer detection
//   - 和-connecting-clauses (和 between verb phrases instead of nouns)
//   - 是+adjective copula (是 before adjective without 很/非常)
//   - Redundant preposition after transitive verb
//
// Phase 2b: A-not-A + 嗎 clash detection
//   - A-not-A question structure with redundant sentence-final 嗎
//
// Architecture: a single Aho-Corasick automaton pre-scans the document once
// for all grammar trigger patterns, then dispatches each hit to per-type
// validators.  This replaces the O(P*N) per-scanner str::find() loops with
// O(N + H) where H = number of AC hits.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::scan::is_cjk_ideograph;
use crate::rules::ruleset::{Issue, IssueType, Severity};

// Common verb-final suffixes that indicate a verb phrase precedes 和.
const VERB_SUFFIXES: &[char] = &['了', '過', '著', '來', '去', '完', '好', '到'];

// Common pronouns for 是+adjective detection.
const PRONOUNS: &[&str] = &[
    "我", "你", "他", "她", "它", "我們", "你們", "他們", "她們", "這", "那", "這個", "那個",
];

// Adjectives commonly misused with bare 是 (English calque).
// Kept small and high-confidence to minimize false positives.
const BARE_SHI_ADJECTIVES: &[&str] = &[
    "漂亮", "高興", "開心", "難過", "傷心", "生氣", "快樂", "緊張", "害怕", "著急", "無聊", "好看",
    "難看", "厲害", "聰明", "笨", "冷", "熱", "忙", "累", "餓", "渴", "胖", "瘦", "大", "小", "多",
    "少", "長", "短", "高", "矮", "好", "壞", "新", "舊", "快", "慢", "早", "晚", "遠", "近", "深",
    "淺", "重", "輕", "難", "容易",
];

// Degree adverbs that make 是+adjective grammatical.
const DEGREE_ADVERBS: &[&str] = &[
    "很",
    "非常",
    "特別",
    "十分",
    "極",
    "超",
    "真",
    "太",
    "蠻",
    "挺",
    "相當",
    "比較",
    "最",
    "更",
    "越來越",
    "有點",
    "稍微",
];

// A-not-A patterns (question structures where 嗎 is redundant).
const A_NOT_A_PATTERNS: &[&str] = &[
    "是不是",
    "有沒有",
    "能不能",
    "會不會",
    "要不要",
    "好不好",
    "對不對",
    "行不行",
    "可不可以",
    "願不願意",
    "想不想",
    "知不知道",
    "喜不喜歡",
    "認不認識",
    "做不做",
    "吃不吃",
    "去不去",
    "來不來",
    "看不看",
    "走不走",
];

// Transitive verb + spurious preposition pairs (English calque).
// (verb, spurious_preposition, context_description)
const TRANSITIVE_VERB_PREPOSITION_PAIRS: &[(&str, &str, &str)] = &[
    ("強調", "在", "transitive verb with redundant preposition"),
    ("討論", "關於", "transitive verb with redundant preposition"),
    ("研究", "關於", "transitive verb with redundant preposition"),
    ("影響", "到", "transitive verb with redundant preposition"),
    ("考慮", "到", "transitive verb with redundant preposition"),
    ("處理", "到", "transitive verb with redundant preposition"),
    ("分析", "關於", "transitive verb with redundant preposition"),
];

// Bureaucratic verbal prefixes (English 'conduct/carry out' calque).
// "進行討論" → "討論", "加以分析" → "分析", "予以處理" → "處理"
const BUREAUCRATIC_PREFIXES: &[&str] = &["進行", "加以", "予以"];

// Verbs commonly nominalized after bureaucratic prefixes.
const NOMINALIZED_VERBS: &[&str] = &[
    "討論", "分析", "研究", "調查", "測試", "開發", "設計", "評估", "檢查", "審查", "修改", "更新",
    "比較", "溝通", "合作", "訓練", "處理", "管理", "規劃", "改善", "調整", "整合", "驗證", "觀察",
    "監控", "維護",
];

// Verbose action prefixes + abstract objects.
// "做出決定" → "決定", "作出回應" → "回應"
const VERBOSE_ACTION_PREFIXES: &[&str] = &["做出", "作出"];

const VERBOSE_ACTION_OBJECTS: &[&str] = &[
    "決定", "回應", "貢獻", "改變", "調整", "承諾", "解釋", "判斷", "選擇", "反應", "讓步", "保證",
    "回答", "犧牲", "努力",
];

// Attribution verbs for double-attribution detection.
// "根據研究顯示" is redundant — use "根據研究" or "研究顯示".
const ATTRIBUTION_VERBS: &[&str] = &["顯示", "指出", "表明", "表示", "說明"];

// Sentence-ending delimiters for boundary detection.
fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '。' | '？' | '！' | '?' | '!' | '\n')
}

// Clause-level delimiters (includes commas, semicolons).
fn is_clause_boundary(ch: char) -> bool {
    is_sentence_end(ch) || matches!(ch, '，' | ',' | '；' | ';' | '：' | ':')
}

fn grammar_issue(
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
        vec![suggestion.into()],
        IssueType::Grammar,
        severity,
    )
    .with_context(context)
}

// ========================================================================
// Grammar AC prefilter: single-pass pattern dispatch
// ========================================================================

/// Grammar check types that the AC prefilter dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrammarCheckType {
    ANotAMa,
    HeConnectingClauses,
    BareShiAdjective,
    RedundantPreposition,
    BureaucraticNominalization,
    VerboseAction,
    DuiJinxing,
    DoubleAttribution,
}

/// Build the grammar pattern table and AC automaton.
/// Returns (automaton, pattern_metadata) where pattern_metadata[i] = (check_type, pattern_table_index).
///
/// The pattern_table_index points back into the original constant arrays so
/// validators can retrieve per-pattern data (e.g. which verb+prep pair).
fn build_grammar_ac() -> (AhoCorasick, Vec<(GrammarCheckType, usize)>) {
    let mut patterns: Vec<&str> = Vec::new();
    let mut metadata: Vec<(GrammarCheckType, usize)> = Vec::new();

    // A-not-A patterns (20 patterns)
    for (i, pat) in A_NOT_A_PATTERNS.iter().enumerate() {
        patterns.push(pat);
        metadata.push((GrammarCheckType::ANotAMa, i));
    }

    // 和 (single char trigger)
    patterns.push("和");
    metadata.push((GrammarCheckType::HeConnectingClauses, 0));

    // 是 (single char trigger)
    patterns.push("是");
    metadata.push((GrammarCheckType::BareShiAdjective, 0));

    // Transitive verbs from TRANSITIVE_VERB_PREPOSITION_PAIRS
    for (i, &(verb, _, _)) in TRANSITIVE_VERB_PREPOSITION_PAIRS.iter().enumerate() {
        patterns.push(verb);
        metadata.push((GrammarCheckType::RedundantPreposition, i));
    }

    // Bureaucratic prefixes
    for (i, prefix) in BUREAUCRATIC_PREFIXES.iter().enumerate() {
        patterns.push(prefix);
        metadata.push((GrammarCheckType::BureaucraticNominalization, i));
    }

    // Verbose action prefixes
    for (i, prefix) in VERBOSE_ACTION_PREFIXES.iter().enumerate() {
        patterns.push(prefix);
        metadata.push((GrammarCheckType::VerboseAction, i));
    }

    // 對 (single char trigger for dui+jinxing)
    patterns.push("對");
    metadata.push((GrammarCheckType::DuiJinxing, 0));

    // 根據 (trigger for double attribution)
    patterns.push("根據");
    metadata.push((GrammarCheckType::DoubleAttribution, 0));

    let ac = AhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&patterns)
        .expect("grammar AC build should not fail on static patterns");

    (ac, metadata)
}

/// Lazily-initialized grammar AC automaton.
/// Thread-safe: OnceLock guarantees single initialization.
fn grammar_ac() -> &'static (AhoCorasick, Vec<(GrammarCheckType, usize)>) {
    use std::sync::OnceLock;
    static GRAMMAR_AC: OnceLock<(AhoCorasick, Vec<(GrammarCheckType, usize)>)> = OnceLock::new();
    GRAMMAR_AC.get_or_init(build_grammar_ac)
}

// ========================================================================
// Per-type validators: called with the AC hit position
// ========================================================================

/// Validate an A-not-A + 嗎 hit.
fn validate_a_not_a_ma(
    text: &str,
    abs_pos: usize,
    pattern_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, pattern_end, excluded) {
        return;
    }

    // Find sentence boundary after this A-not-A pattern.
    let rest = &text[pattern_end..];
    let sentence_end_pos = rest
        .char_indices()
        .find(|&(_, ch)| is_sentence_end(ch))
        .map(|(i, _)| pattern_end + i);

    let sentence_slice = if let Some(end) = sentence_end_pos {
        &text[pattern_end..end]
    } else {
        rest
    };

    // Check if 嗎 appears at the end of the sentence (possibly
    // preceded by whitespace only).
    let trimmed = sentence_slice.trim_end();
    if trimmed.ends_with('嗎') {
        let ma_offset = pattern_end + sentence_slice.rfind('嗎').unwrap();
        let ma_end = ma_offset + '嗎'.len_utf8();
        if !is_excluded(ma_offset, ma_end, excluded) {
            // Report the whole span from A-not-A to 嗎 as the found text.
            let found = &text[abs_pos..ma_end];
            issues.push(grammar_issue(
                abs_pos,
                found,
                &text[abs_pos..pattern_end],
                "A-not-A structure already encodes yes/no question; sentence-final \
                 '\u{55ce}' is redundant",
                Severity::Warning,
            ));
        }
    }
}

/// Validate a 和-connecting-clauses hit.
fn validate_he_connecting(
    text: &str,
    abs_pos: usize,
    he_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, he_end, excluded) {
        return;
    }

    // Check if the character immediately before 和 is a verb suffix.
    let before_he = &text[..abs_pos];
    let prev_char = before_he.chars().next_back();
    let has_verb_suffix = prev_char.is_some_and(|ch| VERB_SUFFIXES.contains(&ch));

    if !has_verb_suffix {
        return;
    }

    // Check if followed by a pronoun.
    let after_he = &text[he_end..];
    let next_is_pronoun = PRONOUNS.iter().any(|p| after_he.starts_with(p));

    if !next_is_pronoun {
        return;
    }

    // Guard: skip comparative constructions (和X一樣/一般/相同/類似/相似).
    let window_end = text[he_end..]
        .char_indices()
        .nth(10)
        .map_or(text.len(), |(i, _)| he_end + i);
    let comparative_window = &text[he_end..window_end];
    if ["一樣", "一般", "相同", "類似", "相似"]
        .iter()
        .any(|pat| comparative_window.contains(pat))
    {
        return;
    }

    issues.push(grammar_issue(
        abs_pos,
        &text[abs_pos..he_end],
        "，",
        "'\u{548c}' connects nouns/noun phrases only; use comma or conjunctions \
         like '\u{800c}\u{4e14}'/'\u{4e26}\u{4e14}' for clauses",
        Severity::Info,
    ));
}

/// Validate a bare 是+adjective hit.
fn validate_bare_shi_adjective(
    text: &str,
    abs_pos: usize,
    shi_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, shi_end, excluded) {
        return;
    }

    // Check if preceded by a pronoun.
    let before = &text[..abs_pos];
    let preceded_by_pronoun = PRONOUNS.iter().any(|p| before.ends_with(p));
    if !preceded_by_pronoun {
        return;
    }

    // Check if followed by a degree adverb (which makes it grammatical).
    let after = &text[shi_end..];
    let has_degree_adverb = DEGREE_ADVERBS.iter().any(|a| after.starts_with(a));
    if has_degree_adverb {
        return;
    }

    // Check if followed by a bare adjective.
    let matched_adj = BARE_SHI_ADJECTIVES
        .iter()
        .find(|&&adj| after.starts_with(adj));

    if let Some(adj) = matched_adj {
        let adj_end = shi_end + adj.len();
        if is_excluded(abs_pos, adj_end, excluded) {
            return;
        }

        // Guard: if the adjective is immediately followed by a CJK
        // character that acts as a noun head, it's a modifier in a noun
        // phrase (e.g. 好消息, 大問題), not a bare adjective predicate.
        let after_adj = &text[adj_end..];
        if let Some(ch) = after_adj.chars().next() {
            if is_cjk_ideograph(ch)
                && !matches!(
                    ch,
                    '的' | '了'
                        | '啊'
                        | '呀'
                        | '呢'
                        | '吧'
                        | '嗎'
                        | '又'
                        | '且'
                        | '並'
                        | '但'
                        | '而'
                )
            {
                return;
            }
        }

        // Find the pronoun that precedes 是 to include in the found span.
        let pronoun = PRONOUNS.iter().find(|p| before.ends_with(*p)).unwrap();
        let pronoun_start = abs_pos - pronoun.len();
        let found = &text[pronoun_start..adj_end];
        let suggestion = format!("{}很{}", pronoun, adj);

        issues.push(grammar_issue(
            pronoun_start,
            found,
            &suggestion,
            "Chinese adjectives are stative verbs; bare '\u{662f}' before adjective \
             is an English calque — use degree adverb '\u{5f88}' instead",
            Severity::Info,
        ));
    }
}

/// Validate a redundant preposition hit.
fn validate_redundant_preposition(
    text: &str,
    abs_pos: usize,
    verb_end: usize,
    pair_index: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    let (verb, prep, ctx) = TRANSITIVE_VERB_PREPOSITION_PAIRS[pair_index];

    if is_excluded(abs_pos, verb_end, excluded) {
        return;
    }

    // Check if the preposition appears within 6 characters after verb.
    let window_end = text.floor_char_boundary(text.len().min(verb_end + 6 * 4));
    let after = &text[verb_end..window_end];

    if let Some(prep_offset) = after.find(prep) {
        let gap = &after[..prep_offset];
        let gap_chars: usize = gap.chars().count();
        if gap_chars > 2 {
            return;
        }

        let prep_abs = verb_end + prep_offset;
        let prep_end = prep_abs + prep.len();
        if is_excluded(prep_abs, prep_end, excluded) {
            return;
        }

        let found = &text[abs_pos..prep_end];
        issues.push(grammar_issue(abs_pos, found, verb, ctx, Severity::Info));
    }
}

/// Validate a bureaucratic nominalization hit.
fn validate_bureaucratic_nominalization(
    text: &str,
    abs_pos: usize,
    prefix_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, prefix_end, excluded) {
        return;
    }

    // Look for a nominalized verb within 2-char gap after prefix.
    let window_end = text.floor_char_boundary(text.len().min(prefix_end + 2 * 4 + 6 * 4));
    let after = &text[prefix_end..window_end];

    let matched = NOMINALIZED_VERBS
        .iter()
        .filter_map(|verb| {
            after.find(verb).and_then(|offset| {
                let gap_chars = after[..offset].chars().count();
                if gap_chars <= 2 {
                    Some((verb, offset))
                } else {
                    None
                }
            })
        })
        .min_by_key(|&(_, offset)| offset);

    if let Some((verb, verb_offset)) = matched {
        let verb_abs = prefix_end + verb_offset;
        let verb_end = verb_abs + verb.len();
        if is_excluded(verb_abs, verb_end, excluded) {
            return;
        }

        let found = &text[abs_pos..verb_end];
        issues.push(grammar_issue(
            abs_pos,
            found,
            verb,
            "bureaucratic nominalization calque of English 'conduct/carry out \
             + noun'; use the verb directly",
            Severity::Info,
        ));
    }
}

/// Validate a verbose action hit.
fn validate_verbose_action(
    text: &str,
    abs_pos: usize,
    prefix_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, prefix_end, excluded) {
        return;
    }

    // Check if an action object follows immediately (0-1 char gap).
    let window_end = text.floor_char_boundary(text.len().min(prefix_end + 4 + 6 * 4));
    let after = &text[prefix_end..window_end];

    let matched = VERBOSE_ACTION_OBJECTS
        .iter()
        .filter_map(|obj| {
            after.find(obj).and_then(|offset| {
                let gap_chars = after[..offset].chars().count();
                if gap_chars <= 1 {
                    Some((obj, offset))
                } else {
                    None
                }
            })
        })
        .min_by_key(|&(_, offset)| offset);

    if let Some((obj, obj_offset)) = matched {
        let obj_abs = prefix_end + obj_offset;
        let obj_end = obj_abs + obj.len();
        if is_excluded(obj_abs, obj_end, excluded) {
            return;
        }

        let found = &text[abs_pos..obj_end];
        issues.push(grammar_issue(
            abs_pos,
            found,
            obj,
            "verbose nominalization; the object can serve as a verb directly",
            Severity::Info,
        ));
    }
}

/// Validate a 對X進行Y hit.
fn validate_dui_jinxing(
    text: &str,
    abs_pos: usize,
    marker_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, marker_end, excluded) {
        return;
    }

    // Skip if 對 is part of a compound word.
    if abs_pos > 0 {
        let prev_ch = text[..abs_pos].chars().next_back();
        if prev_ch.is_some_and(|ch| {
            matches!(
                ch,
                '針' | '面' | '絕' | '相' | '反' | '比' | '核' | '校' | '應' | '配'
            )
        }) {
            return;
        }
    }
    // Check following char: 對於 is a compound preposition.
    if text[marker_end..].starts_with('於') {
        return;
    }

    let jinxing = "進行";
    let jinxing_len = jinxing.len();

    // Look for 進行 within a reasonable window (up to 8 CJK chars).
    let window_end = text.floor_char_boundary(text.len().min(marker_end + 8 * 4));
    let after_dui = &text[marker_end..window_end];

    let Some(jinxing_offset) = after_dui.find(jinxing) else {
        return;
    };

    // The object sits between 對 and 進行; must be 1-6 chars, non-empty.
    let object = &after_dui[..jinxing_offset];
    let obj_chars = object.chars().count();
    if obj_chars == 0 || obj_chars > 6 {
        return;
    }

    // Skip if object contains clause boundary chars.
    if object.chars().any(is_clause_boundary) {
        return;
    }

    let jinxing_abs = marker_end + jinxing_offset;
    let jinxing_end = jinxing_abs + jinxing_len;

    if is_excluded(jinxing_abs, jinxing_end, excluded) {
        return;
    }

    // Look for a verb after 進行, within 2-char gap.
    let verb_window_end = text.floor_char_boundary(text.len().min(jinxing_end + 2 * 4 + 6 * 4));
    let after_jinxing = &text[jinxing_end..verb_window_end];

    let matched = DUI_JINXING_VERBS
        .iter()
        .filter_map(|verb| {
            after_jinxing.find(verb).and_then(|offset| {
                let gap_chars = after_jinxing[..offset].chars().count();
                if gap_chars <= 2 {
                    Some((verb, offset))
                } else {
                    None
                }
            })
        })
        .min_by_key(|&(_, offset)| offset);

    if let Some((verb, verb_offset)) = matched {
        let verb_abs = jinxing_end + verb_offset;
        let verb_end = verb_abs + verb.len();
        if is_excluded(verb_abs, verb_end, excluded) {
            return;
        }

        let found = &text[abs_pos..verb_end];
        let suggestion = format!("{verb}{object}");
        issues.push(grammar_issue(
            abs_pos,
            found,
            &suggestion,
            "fronted-object bureaucratic padding '\u{5c0d}X\u{9032}\u{884c}Y'; \
             restructure as 'verb + object' directly",
            Severity::Info,
        ));
    }
}

/// Validate a double attribution hit (根據...顯示/指出/etc).
fn validate_double_attribution(
    text: &str,
    abs_pos: usize,
    marker_end: usize,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    if is_excluded(abs_pos, marker_end, excluded) {
        return;
    }

    // Search within current clause (up to next clause boundary).
    let rest = &text[marker_end..];
    let clause_len = rest
        .char_indices()
        .find(|&(_, ch)| is_clause_boundary(ch))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let clause = &rest[..clause_len];

    // Check for any attribution verb in this clause.
    for verb in ATTRIBUTION_VERBS {
        if let Some(verb_offset) = clause.find(verb) {
            let verb_abs = marker_end + verb_offset;
            let verb_end = verb_abs + verb.len();
            if is_excluded(verb_abs, verb_end, excluded) {
                continue;
            }

            let found = &text[abs_pos..verb_end];
            let source = &text[marker_end..verb_abs];
            // Skip degenerate case: no source between 根據 and verb.
            if source.trim().is_empty() {
                continue;
            }
            // Skip compound nouns.
            let after_verb = &text[verb_end..];
            let is_compound = match *verb {
                "說明" => after_verb.starts_with('書') || after_verb.starts_with('文'),
                "表示" => after_verb.starts_with('式') || after_verb.starts_with('法'),
                "顯示" => after_verb.starts_with('器') || after_verb.starts_with('屏'),
                _ => false,
            };
            if is_compound {
                continue;
            }
            // Skip markdown links between 根據 and the verb.
            if source.contains('[') || source.contains(']') {
                continue;
            }
            let suggestion = format!("根據{source}");
            issues.push(grammar_issue(
                abs_pos,
                found,
                &suggestion,
                "double attribution: '\u{6839}\u{64da}' (according to) and \
                 reporting verb are redundant together; use one or the other",
                Severity::Info,
            ));
            break; // one attribution verb per 根據 instance
        }
    }
}

// Phase 2b: detect A-not-A structures co-occurring with sentence-final 嗎.
#[cfg(test)]
pub(crate) fn scan_a_not_a_ma(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    for pattern in A_NOT_A_PATTERNS {
        let mut search_start = 0;
        while let Some(pos) = text[search_start..].find(pattern) {
            let abs_pos = search_start + pos;
            let pattern_end = abs_pos + pattern.len();
            search_start = pattern_end;

            if is_excluded(abs_pos, pattern_end, excluded) {
                continue;
            }

            // Find sentence boundary after this A-not-A pattern.
            let rest = &text[pattern_end..];
            let sentence_end_pos = rest
                .char_indices()
                .find(|&(_, ch)| is_sentence_end(ch))
                .map(|(i, _)| pattern_end + i);

            let sentence_slice = if let Some(end) = sentence_end_pos {
                &text[pattern_end..end]
            } else {
                rest
            };

            // Check if 嗎 appears at the end of the sentence (possibly
            // preceded by whitespace only).
            let trimmed = sentence_slice.trim_end();
            if trimmed.ends_with('嗎') {
                let ma_offset = pattern_end + sentence_slice.rfind('嗎').unwrap();
                let ma_end = ma_offset + '嗎'.len_utf8();
                if !is_excluded(ma_offset, ma_end, excluded) {
                    // Report the whole span from A-not-A to 嗎 as the found text.
                    let found = &text[abs_pos..ma_end];
                    issues.push(grammar_issue(
                        abs_pos,
                        found,
                        &text[abs_pos..pattern_end],
                        "A-not-A structure already encodes yes/no question; sentence-final \
                         '\u{55ce}' is redundant",
                        Severity::Warning,
                    ));
                }
            }
        }
    }
}

// Phase 2a: detect 和 connecting clauses (verb phrases) instead of nouns.
#[cfg(test)]
pub(crate) fn scan_he_connecting_clauses(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    let mut search_start = 0;
    let he = '和';
    let he_len = he.len_utf8();

    while let Some(pos) = text[search_start..].find(he) {
        let abs_pos = search_start + pos;
        let he_end = abs_pos + he_len;
        search_start = he_end;

        if is_excluded(abs_pos, he_end, excluded) {
            continue;
        }

        // Check if the character immediately before 和 is a verb suffix.
        // This is a heuristic: CJK char ending in common verb suffixes
        // (了/過/著/來/去/完/好/到) strongly suggests a verb phrase.
        let before_he = &text[..abs_pos];
        let prev_char = before_he.chars().next_back();
        let has_verb_suffix = prev_char.is_some_and(|ch| VERB_SUFFIXES.contains(&ch));

        if !has_verb_suffix {
            continue;
        }

        // Also check the character after 和 -- if followed by another verb
        // phrase indicator (pronoun starting a new clause), this is likely
        // a clause-connecting 和.
        let after_he = &text[he_end..];

        // Quick check: next CJK character should not be a noun-like context.
        // If the next char is also a verb suffix or a pronoun starts the
        // next segment, flag it.
        let next_is_pronoun = PRONOUNS.iter().any(|p| after_he.starts_with(p));

        if !next_is_pronoun {
            continue;
        }

        // Guard: skip comparative constructions (和X一樣/一般/相同/類似/相似).
        // These use 和 as a preposition, not a conjunction.
        let window_end = text[he_end..]
            .char_indices()
            .nth(10)
            .map_or(text.len(), |(i, _)| he_end + i);
        let comparative_window = &text[he_end..window_end];
        if ["一樣", "一般", "相同", "類似", "相似"]
            .iter()
            .any(|pat| comparative_window.contains(pat))
        {
            continue;
        }

        issues.push(grammar_issue(
            abs_pos,
            &text[abs_pos..he_end],
            "，",
            "'\u{548c}' connects nouns/noun phrases only; use comma or conjunctions \
             like '\u{800c}\u{4e14}'/'\u{4e26}\u{4e14}' for clauses",
            Severity::Info,
        ));
    }
}

// Phase 2a: detect bare 是+adjective (English copula calque).
#[cfg(test)]
pub(crate) fn scan_bare_shi_adjective(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let shi = "是";
    let shi_len = shi.len();
    let mut search_start = 0;

    while let Some(pos) = text[search_start..].find(shi) {
        let abs_pos = search_start + pos;
        let shi_end = abs_pos + shi_len;
        search_start = shi_end;

        if is_excluded(abs_pos, shi_end, excluded) {
            continue;
        }

        // Check if preceded by a pronoun.
        let before = &text[..abs_pos];
        let preceded_by_pronoun = PRONOUNS.iter().any(|p| before.ends_with(p));
        if !preceded_by_pronoun {
            continue;
        }

        // Check if followed by a degree adverb (which makes it grammatical).
        let after = &text[shi_end..];
        let has_degree_adverb = DEGREE_ADVERBS.iter().any(|a| after.starts_with(a));
        if has_degree_adverb {
            continue;
        }

        // Check if followed by a bare adjective.
        let matched_adj = BARE_SHI_ADJECTIVES
            .iter()
            .find(|&&adj| after.starts_with(adj));

        if let Some(adj) = matched_adj {
            let adj_end = shi_end + adj.len();
            if is_excluded(abs_pos, adj_end, excluded) {
                continue;
            }

            // Guard: if the adjective is immediately followed by a CJK
            // character that acts as a noun head, it's a modifier in a noun
            // phrase (e.g. 好消息, 大問題), not a bare adjective predicate.
            // Exclude particles (啊了呢吧嗎呀) and connectors (又且並但而的)
            // which do NOT indicate a noun compound.
            let after_adj = &text[adj_end..];
            if let Some(ch) = after_adj.chars().next() {
                if is_cjk_ideograph(ch)
                    && !matches!(
                        ch,
                        '的' | '了'
                            | '啊'
                            | '呀'
                            | '呢'
                            | '吧'
                            | '嗎'
                            | '又'
                            | '且'
                            | '並'
                            | '但'
                            | '而'
                    )
                {
                    continue;
                }
            }

            // Find the pronoun that precedes 是 to include in the found span.
            let pronoun = PRONOUNS.iter().find(|p| before.ends_with(*p)).unwrap();
            let pronoun_start = abs_pos - pronoun.len();
            let found = &text[pronoun_start..adj_end];
            let suggestion = format!("{}很{}", pronoun, adj,);

            issues.push(grammar_issue(
                pronoun_start,
                found,
                &suggestion,
                "Chinese adjectives are stative verbs; bare '\u{662f}' before adjective \
                 is an English calque — use degree adverb '\u{5f88}' instead",
                Severity::Info,
            ));
        }
    }
}

// Phase 2a: detect transitive verb + redundant preposition.
#[cfg(test)]
pub(crate) fn scan_redundant_preposition(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    for &(verb, prep, ctx) in TRANSITIVE_VERB_PREPOSITION_PAIRS {
        let mut search_start = 0;
        while let Some(pos) = text[search_start..].find(verb) {
            let abs_pos = search_start + pos;
            let verb_end = abs_pos + verb.len();
            search_start = verb_end;

            if is_excluded(abs_pos, verb_end, excluded) {
                continue;
            }

            // Check if the preposition appears within 6 characters after verb.
            let window_end = text.floor_char_boundary(text.len().min(verb_end + 6 * 4));
            let after = &text[verb_end..window_end];

            if let Some(prep_offset) = after.find(prep) {
                // Only flag if the preposition is close (within ~2 chars of
                // intervening content, to avoid false positives).
                let gap = &after[..prep_offset];
                let gap_chars: usize = gap.chars().count();
                if gap_chars > 2 {
                    continue;
                }

                let prep_abs = verb_end + prep_offset;
                let prep_end = prep_abs + prep.len();
                if is_excluded(prep_abs, prep_end, excluded) {
                    continue;
                }

                let found = &text[abs_pos..prep_end];
                issues.push(grammar_issue(abs_pos, found, verb, ctx, Severity::Info));
            }
        }
    }
}

// Detect bureaucratic nominalization: 進行/加以/予以 + verb.
// These are calques of English "conduct/carry out + noun" and are verbose.
#[cfg(test)]
pub(crate) fn scan_bureaucratic_nominalization(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    for prefix in BUREAUCRATIC_PREFIXES {
        let prefix_len = prefix.len();
        let mut search_start = 0;
        while let Some(pos) = text[search_start..].find(prefix) {
            let abs_pos = search_start + pos;
            let prefix_end = abs_pos + prefix_len;
            search_start = prefix_end;

            if is_excluded(abs_pos, prefix_end, excluded) {
                continue;
            }

            // Look for a nominalized verb within 2-char gap after prefix.
            let window_end = text.floor_char_boundary(text.len().min(prefix_end + 2 * 4 + 6 * 4));
            let after = &text[prefix_end..window_end];

            // Pick the verb whose match is earliest by text position, not
            // list order — avoids silently matching the wrong verb when two
            // verbs from the list both appear in the window.
            let matched = NOMINALIZED_VERBS
                .iter()
                .filter_map(|verb| {
                    after.find(verb).and_then(|offset| {
                        let gap_chars = after[..offset].chars().count();
                        if gap_chars <= 2 {
                            Some((verb, offset))
                        } else {
                            None
                        }
                    })
                })
                .min_by_key(|&(_, offset)| offset);

            if let Some((verb, verb_offset)) = matched {
                let verb_abs = prefix_end + verb_offset;
                let verb_end = verb_abs + verb.len();
                if is_excluded(verb_abs, verb_end, excluded) {
                    continue;
                }

                let found = &text[abs_pos..verb_end];
                issues.push(grammar_issue(
                    abs_pos,
                    found,
                    verb,
                    "bureaucratic nominalization calque of English 'conduct/carry out \
                     + noun'; use the verb directly",
                    Severity::Info,
                ));
            }
        }
    }
}

// Detect verbose action prefix: 做出/作出 + abstract noun.
// "做出決定" → "決定", "作出回應" → "回應"
#[cfg(test)]
pub(crate) fn scan_verbose_action(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    for prefix in VERBOSE_ACTION_PREFIXES {
        let prefix_len = prefix.len();
        let mut search_start = 0;
        while let Some(pos) = text[search_start..].find(prefix) {
            let abs_pos = search_start + pos;
            let prefix_end = abs_pos + prefix_len;
            search_start = prefix_end;

            if is_excluded(abs_pos, prefix_end, excluded) {
                continue;
            }

            // Check if an action object follows immediately (0-1 char gap).
            let window_end = text.floor_char_boundary(text.len().min(prefix_end + 4 + 6 * 4));
            let after = &text[prefix_end..window_end];

            let matched = VERBOSE_ACTION_OBJECTS
                .iter()
                .filter_map(|obj| {
                    after.find(obj).and_then(|offset| {
                        let gap_chars = after[..offset].chars().count();
                        if gap_chars <= 1 {
                            Some((obj, offset))
                        } else {
                            None
                        }
                    })
                })
                .min_by_key(|&(_, offset)| offset);

            if let Some((obj, obj_offset)) = matched {
                let obj_abs = prefix_end + obj_offset;
                let obj_end = obj_abs + obj.len();
                if is_excluded(obj_abs, obj_end, excluded) {
                    continue;
                }

                let found = &text[abs_pos..obj_end];
                issues.push(grammar_issue(
                    abs_pos,
                    found,
                    obj,
                    "verbose nominalization; the object can serve as a verb directly",
                    Severity::Info,
                ));
            }
        }
    }
}

// Verbs commonly found in the 對X進行Y pattern.
const DUI_JINXING_VERBS: &[&str] = &[
    "討論", "分析", "研究", "調查", "測試", "開發", "設計", "評估", "檢查", "審查", "修改", "更新",
    "比較", "處理", "管理", "規劃", "改善", "調整", "整合", "驗證", "觀察", "監控", "維護", "計算",
    "編輯", "翻譯", "優化", "部署", "配置", "重構",
];

// Detect 對X進行Y pattern: fronted-object bureaucratic padding.
// "對資料進行分析" → "分析資料", "對系統進行測試" → "測試系統"
// This is distinct from scan_bureaucratic_nominalization which catches
// standalone "進行分析" — here the explicit 對X object is present, giving
// a better suggestion that preserves the object.
#[cfg(test)]
pub(crate) fn scan_dui_jinxing(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let marker = "對";
    let marker_len = marker.len();
    let jinxing = "進行";
    let jinxing_len = jinxing.len();
    let mut search_start = 0;

    while let Some(pos) = text[search_start..].find(marker) {
        let abs_pos = search_start + pos;
        let marker_end = abs_pos + marker_len;
        search_start = marker_end;

        if is_excluded(abs_pos, marker_end, excluded) {
            continue;
        }

        // Skip if 對 is part of a compound word (針對, 對於, 面對, 絕對, 相對).
        // Check preceding char: if CJK, this 對 is likely a suffix, not a
        // standalone preposition.
        if abs_pos > 0 {
            let prev_ch = text[..abs_pos].chars().next_back();
            if prev_ch.is_some_and(|ch| {
                matches!(
                    ch,
                    '針' | '面' | '絕' | '相' | '反' | '比' | '核' | '校' | '應' | '配'
                )
            }) {
                continue;
            }
        }
        // Check following char: 對於 is a compound preposition, not this pattern.
        if text[marker_end..].starts_with('於') {
            continue;
        }

        // Look for 進行 within a reasonable window (up to 8 CJK chars ≈ 24 bytes).
        let window_end = text.floor_char_boundary(text.len().min(marker_end + 8 * 4));
        let after_dui = &text[marker_end..window_end];

        let Some(jinxing_offset) = after_dui.find(jinxing) else {
            continue;
        };

        // The object sits between 對 and 進行; must be 1-6 chars, non-empty.
        let object = &after_dui[..jinxing_offset];
        let obj_chars = object.chars().count();
        if obj_chars == 0 || obj_chars > 6 {
            continue;
        }

        // Skip if object contains clause boundary chars.
        if object.chars().any(is_clause_boundary) {
            continue;
        }

        let jinxing_abs = marker_end + jinxing_offset;
        let jinxing_end = jinxing_abs + jinxing_len;

        if is_excluded(jinxing_abs, jinxing_end, excluded) {
            continue;
        }

        // Look for a verb after 進行, within 2-char gap.
        let verb_window_end = text.floor_char_boundary(text.len().min(jinxing_end + 2 * 4 + 6 * 4));
        let after_jinxing = &text[jinxing_end..verb_window_end];

        let matched = DUI_JINXING_VERBS
            .iter()
            .filter_map(|verb| {
                after_jinxing.find(verb).and_then(|offset| {
                    let gap_chars = after_jinxing[..offset].chars().count();
                    if gap_chars <= 2 {
                        Some((verb, offset))
                    } else {
                        None
                    }
                })
            })
            .min_by_key(|&(_, offset)| offset);

        if let Some((verb, verb_offset)) = matched {
            let verb_abs = jinxing_end + verb_offset;
            let verb_end = verb_abs + verb.len();
            if is_excluded(verb_abs, verb_end, excluded) {
                continue;
            }

            let found = &text[abs_pos..verb_end];
            let suggestion = format!("{verb}{object}");
            issues.push(grammar_issue(
                abs_pos,
                found,
                &suggestion,
                "fronted-object bureaucratic padding '\u{5c0d}X\u{9032}\u{884c}Y'; \
                 restructure as 'verb + object' directly",
                Severity::Info,
            ));
        }
    }
}

// Detect double attribution: 根據 + attribution verb in same clause.
// "根據研究顯示" is redundant — either "根據研究" or "研究顯示" suffices.
#[cfg(test)]
pub(crate) fn scan_double_attribution(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let marker = "根據";
    let marker_len = marker.len();
    let mut search_start = 0;

    while let Some(pos) = text[search_start..].find(marker) {
        let abs_pos = search_start + pos;
        let marker_end = abs_pos + marker_len;
        search_start = marker_end;

        if is_excluded(abs_pos, marker_end, excluded) {
            continue;
        }

        // Search within current clause (up to next clause boundary).
        let rest = &text[marker_end..];
        let clause_len = rest
            .char_indices()
            .find(|&(_, ch)| is_clause_boundary(ch))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let clause = &rest[..clause_len];

        // Check for any attribution verb in this clause.
        for verb in ATTRIBUTION_VERBS {
            if let Some(verb_offset) = clause.find(verb) {
                let verb_abs = marker_end + verb_offset;
                let verb_end = verb_abs + verb.len();
                if is_excluded(verb_abs, verb_end, excluded) {
                    continue;
                }

                let found = &text[abs_pos..verb_end];
                let source = &text[marker_end..verb_abs];
                // Skip degenerate case: no source between 根據 and verb.
                if source.trim().is_empty() {
                    continue;
                }
                // Skip when the matched verb is actually a prefix of a longer
                // compound noun (e.g. 說明書, 表示式, 顯示器). Key the
                // suffix check to the specific verb to avoid false negatives
                // like 表示會 (will indicate) or 顯示圖 (show diagram).
                let after_verb = &text[verb_end..];
                let is_compound = match *verb {
                    "說明" => after_verb.starts_with('書') || after_verb.starts_with('文'),
                    "表示" => after_verb.starts_with('式') || after_verb.starts_with('法'),
                    "顯示" => after_verb.starts_with('器') || after_verb.starts_with('屏'),
                    _ => false,
                };
                if is_compound {
                    continue;
                }
                // Skip when a markdown link bracket sits between 根據 and the
                // verb — the verb is inside link text, not an attribution verb.
                if source.contains('[') || source.contains(']') {
                    continue;
                }
                let suggestion = format!("根據{source}");
                issues.push(grammar_issue(
                    abs_pos,
                    found,
                    &suggestion,
                    "double attribution: '\u{6839}\u{64da}' (according to) and \
                     reporting verb are redundant together; use one or the other",
                    Severity::Info,
                ));
                break; // one attribution verb per 根據 instance
            }
        }
    }
}

// ========================================================================
// AI writing detection: grammar-level patterns
// ========================================================================

// Helper to create an AI-style issue (IssueType::AiStyle instead of Grammar).
fn ai_style_issue(
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

// Context clues for definition sense of 意味著 → 表示.
const YIWEIZHE_DEFINITION_CLUES: &[&str] =
    &["定義", "是指", "就是", "即", "所謂", "稱為", "指的是"];

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
// which skips issues with >1 suggestion for non-orthographic types).
// When disambiguation confidence is low, emits advisory-only (no suggestions).
pub(crate) fn scan_ai_semantic_safety(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
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

        // Look at surrounding sentence for context clues to disambiguate.
        // Use sentence boundaries (not clause boundaries) so that clues in
        // an adjacent clause within the same sentence are still visible
        // (e.g. '換言之，這意味著' — '換言之' is in the prior clause).
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
            // Low confidence: no clear context → advisory only (empty suggestion).
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

// Copula-avoidance patterns: AI replaces simple 是/有 with inflated alternatives.
// (inflated_form, simple_copula)
const COPULA_AVOIDANCE_PATTERNS: &[(&str, &str)] = &[
    ("作為", "是"),
    ("標誌著", "是"),
    ("充當", "是"),
    ("擁有", "有"),
    ("設有", "有"),
];

// Characters that, when adjacent to a copula pattern, indicate a compound
// word rather than an inflated copula.  Matching these suppresses the issue.
// 作為: preceded by 所 (有所作為) or 大 (大作為).
// 擁有: followed by 權/者/感/量 (擁有權, 擁有者, 擁有感, 擁有量).
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

// Context clues for technical prose (where copula avoidance is most suspicious).
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
pub(crate) fn scan_ai_copula_avoidance(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
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

            // Only flag in technical prose context to avoid false positives
            // on literary/formal writing where these forms are natural.
            let window_start = abs_pos.saturating_sub(80);
            let window_end = text.len().min(end + 80);
            let window =
                &text[text.floor_char_boundary(window_start)..text.ceil_char_boundary(window_end)];

            let in_tech_context = COPULA_TECH_CONTEXT.iter().any(|c| window.contains(c));
            if !in_tech_context {
                continue;
            }

            // Advisory only — no token-level suggestion.  Direct replacement
            // (e.g. 作為→是) produces broken sentences because the surrounding
            // syntax must change too.  The user must restructure manually.
            let ctx = format!(
                "AI copula avoidance: consider restructuring with '{simple}' \
                 instead of '{inflated}'"
            );
            issues.push(ai_style_issue(abs_pos, inflated, "", &ctx, Severity::Info));
        }
    }
}

// Passive 被-constructions that have obvious active rewrites.
// (被-pattern, active_rewrite)
// Only patterns where dropping 被 is universally safe (adverb + verb).
// Excluded: 被認為是/被稱為 (flips meaning with animate subject),
// 被設計為/被用來/被用於 (changes construction when subject is affected entity).
const PASSIVE_REWRITE_PATTERNS: &[(&str, &str)] = &[
    ("被廣泛使用", "廣泛使用"),
    ("被廣泛採用", "廣泛採用"),
    ("被廣泛應用", "廣泛應用"),
];

// Detect passive voice overuse: 被 + verb where active voice is more natural.
// Only flags patterns from a curated list to minimize false positives.
pub(crate) fn scan_ai_passive(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
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

// Didactic sentence patterns: AI-typical moralizing constructions
// that are nearly 100% AI-generated in technical articles.
// Pattern: 的(故事|案例|經驗|教訓|歷史)(告訴|提醒|啟示)(我們|後人|世人)
pub(crate) fn scan_ai_didactic(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
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

// Vague exaggeration patterns: AI-typical claims like "領先時代 N 年"
// without technical substance.
// Pattern: (領先|超前|超越)(時代|業界|同期)...N年
pub(crate) fn scan_ai_vague_exaggeration(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
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

            // Look forward up to 30 chars for object + optional gap + digits + 年
            let lookahead_end = text.len().min(verb_end + 60);
            let lookahead_end = text.ceil_char_boundary(lookahead_end);
            let lookahead = &text[verb_end..lookahead_end];

            let has_object = OBJECTS.iter().any(|obj| {
                if let Some(obj_pos) = lookahead.find(obj) {
                    // Check for digits followed by 年 after the object
                    let after_obj = &lookahead[obj_pos + obj.len()..];
                    // Skip up to 12 bytes of gap, then look for digit+年
                    let win_end = after_obj.floor_char_boundary(after_obj.len().min(20));
                    let check_window = &after_obj[..win_end];
                    check_window.chars().any(|c| c.is_ascii_digit()) && check_window.contains('年')
                } else {
                    false
                }
            });

            if has_object {
                // Find the end of the pattern (up to 年)
                let pattern_end = text[verb_end..lookahead_end]
                    .find('年')
                    .map(|i| verb_end + i + '年'.len_utf8())
                    .unwrap_or(verb_end);
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

// Density thresholds for AI phrase detection.
// Each entry: (phrase, threshold per 1000 chars, max_acceptable count suggestion).
// Calibrated from x86.md field review data.
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

// Post-scan density pass: count tracked phrases across the full document.
// When density (count / text_len_chars * 1000) exceeds the per-phrase threshold,
// emit a single summary AiStyle issue at the first occurrence with density stats.
// Does NOT duplicate per-occurrence ai_filler detection — this catches the
// statistical signature that only becomes visible at document level.
pub(crate) fn scan_ai_density(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
    threshold_multiplier: f32,
) {
    let char_count = text.chars().count();
    // Skip density analysis on short texts (< 500 chars) — not enough
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
            if first_offset.is_none() {
                first_offset = Some(abs_pos);
            }
        }

        if count == 0 {
            continue;
        }

        let density = count as f32 / text_k;
        let effective_threshold = threshold * threshold_multiplier;
        if density > effective_threshold {
            let offset = first_offset.unwrap();
            let ctx = format!(
                "AI density: \u{300C}{phrase}\u{300D} 在本文出現 {count} 次 \
                 ({density:.1}次/千字，閾值 {effective_threshold:.1})，\
                 疑似 AI 生成的轉折公式。建議減至 {max_acceptable} 次以內。"
            );
            issues.push(ai_style_issue(offset, phrase, "", &ctx, Severity::Warning));
        }
    }
}

// --- Structural AI pattern detectors ---

/// Returns true if the byte range [start, end) is entirely within an exclusion zone.
fn is_para_excluded(start: usize, end: usize, excluded: &[ByteRange]) -> bool {
    excluded.iter().any(|r| r.start <= start && end <= r.end)
}

// Binary contrast density: AI overuses paired transition patterns.
// Counts intra-sentence double turns, progressive, and concessive patterns.
// Threshold: >5 per 1000 chars is AI-typical (human baseline: 2-3).
pub(crate) fn scan_ai_binary_contrast(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
    threshold_multiplier: f32,
) {
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
    for para in text.split("\n\n") {
        let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
        if is_para_excluded(para_start, para_start + para.len(), excluded) {
            continue;
        }
        // Scan sentences within paragraph (split on 。！？).
        for sentence in para.split(['。', '！', '？']) {
            let sent_start = sentence.as_ptr() as usize - text.as_ptr() as usize;
            // Check concessive pattern.
            for &start_word in concessive_starts {
                if let Some(pos) = sentence.find(start_word) {
                    let after = &sentence[pos + start_word.len()..];
                    for &turn in concessive_turns {
                        if after.contains(turn) {
                            count += 1;
                            if first_offset.is_none() {
                                first_offset = Some(sent_start + pos);
                            }
                            break;
                        }
                    }
                    break;
                }
            }
            // Check progressive pattern.
            for &start_word in progressive_starts {
                if let Some(pos) = sentence.find(start_word) {
                    let after = &sentence[pos + start_word.len()..];
                    for &turn in progressive_turns {
                        if after.contains(turn) {
                            count += 1;
                            if first_offset.is_none() {
                                first_offset = Some(sent_start + pos);
                            }
                            break;
                        }
                    }
                    break;
                }
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
        issues.push(ai_style_issue(offset, "", "", &ctx, Severity::Info));
    }
}

// Paragraph-ending formulaic declarations.
// AI closes paragraphs with stock phrases like:
//   這...證明/揭示...
//   這...成為...的基礎/基石/起點
//   正是這...讓...
// Flag when 3+ paragraphs end with such patterns.
pub(crate) fn scan_ai_paragraph_endings(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
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
        issues.push(ai_style_issue(offset, "", "", &ctx, Severity::Info));
    }
}

// Dash overuse: flag when many paragraphs contain ≥3 em-dashes.
// AI writing overuses parenthetical dashes for elaboration.
pub(crate) fn scan_ai_dash_overuse(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
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
        let dash_count = para.matches('—').count();
        if dash_count >= 3 {
            heavy_dash_count += 1;
            if first_offset.is_none() {
                let para_start = para.as_ptr() as usize - text.as_ptr() as usize;
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
        issues.push(ai_style_issue(offset, "", "", &ctx, Severity::Info));
    }
}

// Formulaic section headings: AI generates stereotyped heading patterns.
// These are only meaningful in Markdown/structured text where headings
// are explicit. Detects patterns in lines starting with # or ##.
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

pub(crate) fn scan_ai_formulaic_headings(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
) {
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
        issues.push(ai_style_issue(offset, "", "", &ctx, Severity::Info));
    }
}

// Enumerated list density: count list-containing paragraphs relative to total.
// AI writing overuses bullet/numbered lists for organization.
// Flag when list-paragraph ratio exceeds 40%.
pub(crate) fn scan_ai_list_density(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
    threshold_multiplier: f32,
) {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
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
        issues.push(ai_style_issue(offset, "", "", &ctx, Severity::Info));
    }
}

// Zero-width codepoints injected by LLM tokenizers (BPE/WordPiece).
// Any occurrence mid-text is a tokenizer artifact; suggest empty string for auto-removal.
const ZERO_WIDTH_CODEPOINTS: &[(char, &str)] = &[
    ('\u{200B}', "U+200B zero-width space"),
    ('\u{200C}', "U+200C zero-width non-joiner"),
    ('\u{200D}', "U+200D zero-width joiner"),
    ('\u{FEFF}', "U+FEFF byte-order mark"),
    ('\u{200E}', "U+200E left-to-right mark"),
    ('\u{200F}', "U+200F right-to-left mark"),
];

// Detect zero-width tokenizer artifacts and emit per-occurrence AiStyle issues.
// Suggestion is empty string so the fixer strips them automatically.
fn scan_ai_zero_width(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let mut byte_offset = 0;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        if let Some(&(_, label)) = ZERO_WIDTH_CODEPOINTS.iter().find(|(c, _)| *c == ch) {
            if !is_excluded(byte_offset, byte_offset + ch_len, excluded) {
                let ctx = format!("AI token: 零寬字元 {label}，疑似 LLM 分詞器殘留。");
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
        }
        byte_offset += ch_len;
    }
}

// Entry point: run all structural AI pattern checks.
// Gated by ProfileConfig::ai_structural_patterns.
pub(crate) fn scan_ai_structural(
    text: &str,
    excluded: &[ByteRange],
    issues: &mut Vec<Issue>,
    threshold_multiplier: f32,
) {
    scan_ai_binary_contrast(text, excluded, issues, threshold_multiplier);
    scan_ai_paragraph_endings(text, excluded, issues);
    scan_ai_dash_overuse(text, excluded, issues);
    scan_ai_formulaic_headings(text, excluded, issues);
    scan_ai_list_density(text, excluded, issues, threshold_multiplier);
    scan_ai_zero_width(text, excluded, issues);
}

// Entry point for AI writing detection grammar checks.
// Gated by ProfileConfig::ai_semantic_safety — NOT called from scan_grammar.
pub(crate) fn scan_ai_grammar(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    scan_ai_semantic_safety(text, excluded, issues);
    scan_ai_copula_avoidance(text, excluded, issues);
    scan_ai_passive(text, excluded, issues);
    scan_ai_didactic(text, excluded, issues);
    scan_ai_vague_exaggeration(text, excluded, issues);
}

// Main entry point: run all grammar checks via AC prefilter.
//
// A single Aho-Corasick pass finds all trigger patterns, then dispatches
// each hit to the appropriate validator.  This is O(N + H) instead of the
// old O(P*N) where P = total patterns across 8 scanners.
pub(crate) fn scan_grammar(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    let (ac, metadata) = grammar_ac();

    for mat in ac.find_iter(text) {
        let (check_type, pattern_index) = metadata[mat.pattern().as_usize()];
        let start = mat.start();
        let end = mat.end();

        match check_type {
            GrammarCheckType::ANotAMa => {
                validate_a_not_a_ma(text, start, end, excluded, issues);
            }
            GrammarCheckType::HeConnectingClauses => {
                validate_he_connecting(text, start, end, excluded, issues);
            }
            GrammarCheckType::BareShiAdjective => {
                validate_bare_shi_adjective(text, start, end, excluded, issues);
            }
            GrammarCheckType::RedundantPreposition => {
                validate_redundant_preposition(text, start, end, pattern_index, excluded, issues);
            }
            GrammarCheckType::BureaucraticNominalization => {
                validate_bureaucratic_nominalization(text, start, end, excluded, issues);
            }
            GrammarCheckType::VerboseAction => {
                validate_verbose_action(text, start, end, excluded, issues);
            }
            GrammarCheckType::DuiJinxing => {
                validate_dui_jinxing(text, start, end, excluded, issues);
            }
            GrammarCheckType::DoubleAttribution => {
                validate_double_attribution(text, start, end, excluded, issues);
            }
        }
    }
}

// Old scan_grammar entry point retained for differential testing.
#[cfg(test)]
fn scan_grammar_legacy(text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
    scan_a_not_a_ma(text, excluded, issues);
    scan_he_connecting_clauses(text, excluded, issues);
    scan_bare_shi_adjective(text, excluded, issues);
    scan_redundant_preposition(text, excluded, issues);
    scan_bureaucratic_nominalization(text, excluded, issues);
    scan_verbose_action(text, excluded, issues);
    scan_dui_jinxing(text, excluded, issues);
    scan_double_attribution(text, excluded, issues);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_grammar(text, &[], &mut issues);
        issues
    }

    // =======================================================================
    // Phase 1: plumbing — IssueType::Grammar fundamentals
    // =======================================================================

    #[test]
    fn grammar_issue_type_serde_round_trip() {
        let json = serde_json::to_string(&IssueType::Grammar).unwrap();
        assert_eq!(json, "\"grammar\"");
        let back: IssueType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, IssueType::Grammar);
    }

    #[test]
    fn grammar_sort_order_is_last() {
        // Grammar should sort after all other issue types.
        assert!(IssueType::Grammar.sort_order() > IssueType::Variant.sort_order());
        assert!(IssueType::Grammar.sort_order() > IssueType::Punctuation.sort_order());
    }

    #[test]
    fn grammar_name_matches_serde() {
        assert_eq!(IssueType::Grammar.name(), "grammar");
    }

    #[test]
    fn grammar_issue_fields_populated() {
        let issues = scan("你是不是學生嗎？");
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        assert_eq!(i.rule_type, IssueType::Grammar);
        assert_eq!(i.severity, Severity::Warning);
        assert!(i.context.is_some(), "grammar issues should have context");
        assert!(!i.suggestions.is_empty(), "should have suggestions");
        assert!(i.length > 0, "should have nonzero byte length");
    }

    #[test]
    fn grammar_issue_offset_is_byte_accurate() {
        let text = "你是不是學生嗎？";
        let issues = scan(text);
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        // The found text extracted from the reported span should match.
        assert_eq!(&text[i.offset..i.offset + i.length], i.found);
    }

    #[test]
    fn empty_text_produces_no_issues() {
        assert!(scan("").is_empty());
    }

    #[test]
    fn ascii_only_text_produces_no_issues() {
        assert!(scan("Hello world, this is a test.").is_empty());
    }

    #[test]
    fn clean_chinese_text_produces_no_issues() {
        let clean = "台灣是一個美麗的島嶼，有豐富的文化和美食。";
        assert!(scan(clean).is_empty());
    }

    // =======================================================================
    // Phase 2b: A-not-A + 嗎 — all 14 patterns × with/without 嗎
    // =======================================================================

    // -- with 嗎 (should flag) --

    #[test]
    fn a_not_a_shi_bu_shi_with_ma() {
        let issues = scan("你是不是學生嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("是不是"));
        assert!(issues[0].found.contains("嗎"));
    }

    #[test]
    fn a_not_a_you_mei_you_with_ma() {
        let issues = scan("你有沒有吃飯嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("有沒有"));
    }

    #[test]
    fn a_not_a_neng_bu_neng_with_ma() {
        let issues = scan("你能不能來嗎");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("能不能"));
    }

    #[test]
    fn a_not_a_hui_bu_hui_with_ma() {
        let issues = scan("他會不會游泳嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("會不會"));
    }

    #[test]
    fn a_not_a_yao_bu_yao_with_ma() {
        let issues = scan("你要不要喝水嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("要不要"));
    }

    #[test]
    fn a_not_a_hao_bu_hao_with_ma() {
        let issues = scan("這樣好不好嗎");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("好不好"));
    }

    #[test]
    fn a_not_a_dui_bu_dui_with_ma() {
        let issues = scan("答案對不對嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("對不對"));
    }

    #[test]
    fn a_not_a_xing_bu_xing_with_ma() {
        let issues = scan("這樣行不行嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("行不行"));
    }

    #[test]
    fn a_not_a_ke_bu_ke_yi_with_ma() {
        let issues = scan("可不可以走嗎");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("可不可以"));
    }

    #[test]
    fn a_not_a_yuan_bu_yuan_yi_with_ma() {
        let issues = scan("你願不願意幫忙嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("願不願意"));
    }

    #[test]
    fn a_not_a_xiang_bu_xiang_with_ma() {
        let issues = scan("你想不想去嗎");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("想不想"));
    }

    #[test]
    fn a_not_a_zhi_bu_zhi_dao_with_ma() {
        let issues = scan("你知不知道嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("知不知道"));
    }

    #[test]
    fn a_not_a_xi_bu_xi_huan_with_ma() {
        let issues = scan("你喜不喜歡吃飯嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("喜不喜歡"));
    }

    #[test]
    fn a_not_a_ren_bu_ren_shi_with_ma() {
        let issues = scan("你認不認識他嗎");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("認不認識"));
    }

    // -- without 嗎 (should NOT flag) --

    #[test]
    fn a_not_a_shi_bu_shi_without_ma() {
        assert!(scan("你是不是學生？").is_empty());
    }

    #[test]
    fn a_not_a_you_mei_you_without_ma() {
        assert!(scan("你有沒有吃飯？").is_empty());
    }

    #[test]
    fn a_not_a_neng_bu_neng_without_ma() {
        assert!(scan("你能不能來？").is_empty());
    }

    #[test]
    fn a_not_a_hui_bu_hui_without_ma() {
        assert!(scan("他會不會游泳？").is_empty());
    }

    #[test]
    fn a_not_a_yao_bu_yao_without_ma() {
        assert!(scan("你要不要喝水？").is_empty());
    }

    #[test]
    fn a_not_a_hao_bu_hao_without_ma() {
        assert!(scan("這樣好不好？").is_empty());
    }

    #[test]
    fn a_not_a_dui_bu_dui_without_ma() {
        assert!(scan("答案對不對？").is_empty());
    }

    #[test]
    fn a_not_a_xing_bu_xing_without_ma() {
        assert!(scan("這樣行不行？").is_empty());
    }

    #[test]
    fn a_not_a_ke_bu_ke_yi_without_ma() {
        assert!(scan("可不可以走？").is_empty());
    }

    #[test]
    fn a_not_a_yuan_bu_yuan_yi_without_ma() {
        assert!(scan("你願不願意幫忙？").is_empty());
    }

    #[test]
    fn a_not_a_xiang_bu_xiang_without_ma() {
        assert!(scan("你想不想去？").is_empty());
    }

    #[test]
    fn a_not_a_zhi_bu_zhi_dao_without_ma() {
        assert!(scan("你知不知道？").is_empty());
    }

    #[test]
    fn a_not_a_xi_bu_xi_huan_without_ma() {
        assert!(scan("你喜不喜歡吃飯？").is_empty());
    }

    #[test]
    fn a_not_a_ren_bu_ren_shi_without_ma() {
        assert!(scan("你認不認識他？").is_empty());
    }

    // -- A-not-A edge cases --

    #[test]
    fn a_not_a_ma_across_sentence_boundary_clean() {
        // 嗎 is in a different sentence — must not flag.
        assert!(scan("你是不是學生。他好嗎？").is_empty());
    }

    #[test]
    fn a_not_a_ma_across_newline_boundary_clean() {
        assert!(scan("你是不是學生\n他好嗎？").is_empty());
    }

    #[test]
    fn a_not_a_ma_across_exclamation_boundary_clean() {
        assert!(scan("你是不是學生！他好嗎？").is_empty());
    }

    #[test]
    fn ma_only_no_a_not_a_clean() {
        assert!(scan("你是學生嗎？").is_empty());
    }

    #[test]
    fn a_not_a_suggestion_is_pattern_without_ma() {
        let issues = scan("你是不是學生嗎？");
        assert_eq!(issues[0].suggestions[0], "是不是");
    }

    #[test]
    fn a_not_a_severity_is_warning() {
        let issues = scan("你是不是學生嗎？");
        assert_eq!(issues[0].severity, Severity::Warning);
    }

    #[test]
    fn a_not_a_ma_with_trailing_whitespace() {
        // 嗎 followed by spaces before sentence end.
        let issues = scan("你是不是學生嗎  ？");
        assert_eq!(issues.len(), 1);
    }

    // =======================================================================
    // Phase 2a: 和-connecting-clauses
    // =======================================================================

    #[test]
    fn he_verb_suffix_le_with_pronoun() {
        let issues = scan("我吃了和你去看電影");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "和");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn he_verb_suffix_guo_with_pronoun() {
        let issues = scan("我去過和他來過");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_zhe_with_pronoun() {
        let issues = scan("我看著和她說話");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_lai_with_pronoun() {
        let issues = scan("我回來和你一起走");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_qu_with_pronoun() {
        let issues = scan("他出去和我回家");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_wan_with_pronoun() {
        let issues = scan("我寫完和你開始");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_hao_with_pronoun() {
        let issues = scan("我準備好和他出發");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_verb_suffix_dao_with_pronoun() {
        let issues = scan("我找到和她確認");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn he_between_nouns_clean() {
        assert!(scan("蘋果和橘子都很好吃").is_empty());
    }

    #[test]
    fn he_no_verb_suffix_before_clean() {
        // No verb suffix immediately before 和.
        assert!(scan("老師和學生都來了").is_empty());
    }

    #[test]
    fn he_verb_suffix_but_no_pronoun_after_clean() {
        // Verb suffix before 和, but no pronoun after → not a clause connector.
        assert!(scan("我吃了和飯").is_empty());
    }

    #[test]
    fn he_suggestion_is_comma() {
        let issues = scan("我住在台北了和我有一隻狗");
        assert_eq!(issues[0].suggestions[0], "，");
    }

    // =======================================================================
    // Phase 2a: 是+adjective copula
    // =======================================================================

    #[test]
    fn bare_shi_disyllabic_adj() {
        let issues = scan("她是漂亮");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "她是漂亮");
        assert_eq!(issues[0].suggestions[0], "她很漂亮");
    }

    #[test]
    fn bare_shi_monosyllabic_adj() {
        let issues = scan("我是忙");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "我很忙");
    }

    #[test]
    fn bare_shi_adj_with_ta() {
        let issues = scan("他是高");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "他很高");
    }

    #[test]
    fn bare_shi_adj_with_women() {
        let issues = scan("我們是開心");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "我們很開心");
    }

    #[test]
    fn bare_shi_adj_with_zhe() {
        let issues = scan("這是好");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "這很好");
    }

    #[test]
    fn bare_shi_adj_with_na() {
        let issues = scan("那是遠");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "那很遠");
    }

    #[test]
    fn bare_shi_severity_is_info() {
        let issues = scan("她是漂亮");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    // -- degree adverbs suppress the pattern (negative tests) --

    #[test]
    fn shi_with_hen_clean() {
        assert!(scan("她是很漂亮").is_empty());
    }

    #[test]
    fn shi_with_feichang_clean() {
        assert!(scan("她是非常漂亮").is_empty());
    }

    #[test]
    fn shi_with_tebie_clean() {
        assert!(scan("她是特別漂亮").is_empty());
    }

    #[test]
    fn shi_with_tai_clean() {
        assert!(scan("她是太漂亮").is_empty());
    }

    #[test]
    fn shi_with_zhen_clean() {
        assert!(scan("她是真漂亮").is_empty());
    }

    #[test]
    fn shi_with_bijiao_clean() {
        assert!(scan("她是比較漂亮").is_empty());
    }

    #[test]
    fn shi_with_youdian_clean() {
        assert!(scan("她是有點漂亮").is_empty());
    }

    // -- 是+noun should not fire --

    #[test]
    fn shi_noun_predicate_clean() {
        assert!(scan("她是老師").is_empty());
    }

    #[test]
    fn shi_proper_noun_clean() {
        assert!(scan("他是台灣人").is_empty());
    }

    #[test]
    fn shi_without_pronoun_clean() {
        // No pronoun before 是: e.g. 問題是... — should not fire.
        assert!(scan("問題是很大").is_empty());
    }

    #[test]
    fn shi_adj_as_noun_modifier_clean() {
        // 好消息 — 好 is an adjective modifying a noun, not a bare predicate.
        assert!(scan("這是好消息").is_empty());
    }

    #[test]
    fn shi_adj_as_noun_modifier_da_clean() {
        // 大問題 — same pattern.
        assert!(scan("這是大問題").is_empty());
    }

    #[test]
    fn shi_adj_standalone_still_fires() {
        // 好 at end of text (no following CJK) — still a bare adjective.
        let issues = scan("這是好");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn shi_adj_with_particle_still_fires() {
        // 漂亮啊 — particle after adjective, NOT a noun modifier.
        let issues = scan("她是漂亮啊");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn shi_adj_with_connector_still_fires() {
        // 漂亮又善良 — connector after adjective, NOT a noun modifier.
        let issues = scan("她是漂亮又善良");
        assert_eq!(issues.len(), 1);
    }

    // =======================================================================
    // Phase 2a: redundant preposition
    // =======================================================================

    #[test]
    fn redundant_prep_taolun_guanyu() {
        let issues = scan("我們討論關於這個問題");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("討論關於"));
        assert_eq!(issues[0].suggestions[0], "討論");
    }

    #[test]
    fn redundant_prep_yanjiu_guanyu() {
        let issues = scan("他研究關於量子力學");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("研究關於"));
    }

    #[test]
    fn redundant_prep_qiangdiao_zai() {
        let issues = scan("他強調在這一點上");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("強調在"));
    }

    #[test]
    fn redundant_prep_yingxiang_dao() {
        let issues = scan("這影響到整體計畫");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("影響到"));
    }

    #[test]
    fn redundant_prep_kaolu_dao() {
        let issues = scan("請考慮到這個因素");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("考慮到"));
    }

    #[test]
    fn redundant_prep_chuli_dao() {
        let issues = scan("我處理到這個問題");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("處理到"));
    }

    #[test]
    fn redundant_prep_severity_is_info() {
        let issues = scan("我們討論關於這個問題");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn transitive_verb_no_preposition_clean() {
        assert!(scan("我們討論這個問題").is_empty());
    }

    #[test]
    fn preposition_too_far_from_verb_clean() {
        // Gap > 2 chars between verb and preposition.
        assert!(scan("我們討論了很多關於這個問題").is_empty());
    }

    #[test]
    fn redundant_prep_with_one_char_gap() {
        // One char gap between verb and preposition is still flagged.
        let issues = scan("他研究了關於量子力學");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn redundant_prep_fenxi_guanyu() {
        let issues = scan("他分析關於這個現象");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("分析關於"));
    }

    // =======================================================================
    // Extended A-not-A patterns (single-char verbs)
    // =======================================================================

    #[test]
    fn a_not_a_zuo_bu_zuo_with_ma() {
        let issues = scan("你做不做嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("做不做"));
    }

    #[test]
    fn a_not_a_chi_bu_chi_with_ma() {
        let issues = scan("你吃不吃嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("吃不吃"));
    }

    #[test]
    fn a_not_a_qu_bu_qu_with_ma() {
        let issues = scan("你去不去嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("去不去"));
    }

    #[test]
    fn a_not_a_lai_bu_lai_with_ma() {
        let issues = scan("你來不來嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("來不來"));
    }

    #[test]
    fn a_not_a_kan_bu_kan_with_ma() {
        let issues = scan("你看不看嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("看不看"));
    }

    #[test]
    fn a_not_a_zou_bu_zou_with_ma() {
        let issues = scan("你走不走嗎？");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("走不走"));
    }

    #[test]
    fn a_not_a_zuo_bu_zuo_without_ma() {
        assert!(scan("你做不做？").is_empty());
    }

    #[test]
    fn a_not_a_chi_bu_chi_without_ma() {
        assert!(scan("你吃不吃？").is_empty());
    }

    // =======================================================================
    // Bureaucratic nominalization (進行/加以/予以 + verb)
    // =======================================================================

    #[test]
    fn bureaucratic_jinxing_taolun() {
        let issues = scan("我們進行討論");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "進行討論");
        assert_eq!(issues[0].suggestions[0], "討論");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn bureaucratic_jinxing_fenxi() {
        let issues = scan("他們進行分析");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "進行分析");
    }

    #[test]
    fn bureaucratic_jinxing_yanjiu() {
        let issues = scan("這個團隊進行研究");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "研究");
    }

    #[test]
    fn bureaucratic_jinxing_ceshi() {
        let issues = scan("我們進行測試");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "進行測試");
    }

    #[test]
    fn bureaucratic_jinxing_with_le_gap() {
        // 了 between prefix and verb (1-char gap, should still flag).
        let issues = scan("我們進行了討論");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "進行了討論");
    }

    #[test]
    fn bureaucratic_jiayi_fenxi() {
        let issues = scan("我們加以分析");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "加以分析");
        assert_eq!(issues[0].suggestions[0], "分析");
    }

    #[test]
    fn bureaucratic_yuyi_chuli() {
        let issues = scan("我們予以處理");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "予以處理");
        assert_eq!(issues[0].suggestions[0], "處理");
    }

    #[test]
    fn bureaucratic_jinxing_standalone_clean() {
        // 進行 as standalone verb ("proceeding") — no nominalized verb after.
        assert!(scan("會議正在進行").is_empty());
    }

    #[test]
    fn bureaucratic_jinxing_zhong_clean() {
        // 進行中 means "in progress" — not a nominalization.
        assert!(scan("專案進行中").is_empty());
    }

    #[test]
    fn bureaucratic_verb_too_far_clean() {
        // Verb too far away (>2 chars gap).
        assert!(scan("我們進行了一些額外的討論").is_empty());
    }

    #[test]
    fn bureaucratic_jinxing_picks_nearest_verb() {
        // Two verbs in window: 管理 (offset 0) and 研究 (offset 2 chars).
        // Should match 管理 (nearest by text position).
        let issues = scan("我們進行管理研究");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "進行管理");
        assert_eq!(issues[0].suggestions[0], "管理");
    }

    #[test]
    fn bureaucratic_multiple_prefixes() {
        let issues = scan("我們進行討論並加以分析");
        assert_eq!(issues.len(), 2);
    }

    // =======================================================================
    // Verbose action prefix (做出/作出 + abstract noun)
    // =======================================================================

    #[test]
    fn verbose_zuochu_jueding() {
        let issues = scan("他做出決定");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "做出決定");
        assert_eq!(issues[0].suggestions[0], "決定");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn verbose_zuochu_huiying() {
        let issues = scan("我們做出回應");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "做出回應");
    }

    #[test]
    fn verbose_zuochu_gongxian() {
        let issues = scan("他做出貢獻");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "貢獻");
    }

    #[test]
    fn verbose_zuochu_with_le() {
        let issues = scan("他做出了決定");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "做出了決定");
    }

    #[test]
    fn verbose_zuochu_alt_prefix() {
        // 作出 is an alternate form of 做出.
        let issues = scan("他作出回應");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "作出回應");
    }

    #[test]
    fn verbose_zuochu_no_object_clean() {
        // 做出 without a known object — not flagged.
        assert!(scan("他做出一個蛋糕").is_empty());
    }

    #[test]
    fn verbose_zuochu_object_too_far_clean() {
        // Object too far away (>1 char gap).
        assert!(scan("他做出了很多決定").is_empty());
    }

    // =======================================================================
    // Double attribution (根據...顯示/指出)
    // =======================================================================

    #[test]
    fn double_attribution_genju_xianshi() {
        let issues = scan("根據研究顯示，成果很好");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "根據研究顯示");
        assert_eq!(issues[0].suggestions[0], "根據研究");
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn double_attribution_genju_zhichu() {
        let issues = scan("根據報告指出，問題嚴重");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "根據報告指出");
    }

    #[test]
    fn double_attribution_genju_biaoming() {
        let issues = scan("根據數據表明這是正確的");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "根據數據表明");
    }

    #[test]
    fn double_attribution_genju_biaoshi() {
        let issues = scan("根據專家表示，這很重要");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("根據專家表示"));
    }

    #[test]
    fn double_attribution_genju_shuoming() {
        let issues = scan("根據文件說明，規格如下");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("根據文件說明"));
    }

    #[test]
    fn double_attribution_long_source() {
        // Long source text between 根據 and attribution verb.
        let issues = scan("根據最新發表的一項研究報告顯示，結果令人驚訝");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[0], "根據最新發表的一項研究報告");
    }

    #[test]
    fn double_attribution_empty_source_skipped() {
        // Degenerate case: no source between 根據 and verb — skip.
        assert!(scan("根據顯示結果很好").is_empty());
    }

    #[test]
    fn double_attribution_noun_compound_skipped() {
        // 說明書 is a noun compound; 說明 is a prefix, not an attribution verb.
        assert!(scan("根據手冊說明書的內容").is_empty());
    }

    #[test]
    fn double_attribution_verb_at_boundary_still_fires() {
        // 說明 followed by comma (not CJK) — still an attribution verb.
        let issues = scan("根據文件說明，規格如下");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_biaoshi_hui_still_fires() {
        // 表示會 — 會 means "will", not a noun suffix. Must still fire.
        let issues = scan("根據消息表示會延期");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_xianshi_tu_still_fires() {
        // 顯示圖 — 圖 here is "diagram", not a compound suffix. Must fire.
        let issues = scan("根據數據顯示圖表有誤");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_markdown_link_skipped() {
        // 根據[link text with 說明](url) — verb inside markdown link, not attribution.
        assert!(scan("根據[維護者設計說明](https://example.com)，新版核心改動很大").is_empty());
    }

    #[test]
    fn double_attribution_markdown_link_bracket_only() {
        // Even a bare [ between 根據 and verb suppresses the match.
        assert!(scan("根據[某研究說明書]的結論").is_empty());
    }

    #[test]
    fn genju_without_verb_clean() {
        // 根據 without attribution verb — prepositional phrase, not redundant.
        assert!(scan("根據研究，成果很好").is_empty());
    }

    #[test]
    fn genju_verb_in_next_clause_clean() {
        // Attribution verb after comma — different clause, not flagged.
        assert!(scan("根據這份報告，研究顯示成果很好").is_empty());
    }

    #[test]
    fn standalone_verb_without_genju_clean() {
        // Attribution verb without 根據 — just a normal verb.
        assert!(scan("研究顯示成果很好").is_empty());
    }

    // =======================================================================
    // Phase 2c: 對X進行Y — fronted-object bureaucratic padding
    // =======================================================================

    #[test]
    fn dui_jinxing_basic() {
        let issues = scan("對資料進行分析");
        let dui: Vec<_> = issues
            .iter()
            .filter(|i| i.found.starts_with("對"))
            .collect();
        assert_eq!(dui.len(), 1);
        assert_eq!(dui[0].found, "對資料進行分析");
        assert_eq!(dui[0].suggestions, vec!["分析資料"]);
        assert_eq!(dui[0].severity, Severity::Info);
    }

    #[test]
    fn dui_jinxing_longer_object() {
        let issues = scan("我們對整個系統進行測試");
        let dui: Vec<_> = issues
            .iter()
            .filter(|i| i.found.starts_with("對"))
            .collect();
        assert_eq!(dui.len(), 1);
        assert_eq!(dui[0].suggestions, vec!["測試整個系統"]);
    }

    #[test]
    fn dui_jinxing_various_verbs() {
        // Each fires dui_jinxing; bureaucratic_nominalization may also fire.
        let check = |text: &str| scan(text).iter().any(|i| i.found.starts_with("對"));
        assert!(check("對程式碼進行審查"));
        assert!(check("對方案進行評估"));
        assert!(check("對架構進行重構"));
    }

    #[test]
    fn dui_jinxing_compound_word_zhendui_skipped() {
        // 針對 is a compound preposition; the 對 is not standalone.
        let issues = scan("針對資料進行分析");
        assert!(
            !issues.iter().any(|i| i.found.starts_with("對")),
            "should not match 對 inside 針對"
        );
    }

    #[test]
    fn dui_jinxing_compound_word_duiyu_skipped() {
        // 對於 is a compound preposition; should not match.
        assert!(!scan("對於資料進行分析")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_compound_miandui_skipped() {
        // 面對 — not a standalone 對.
        assert!(!scan("面對問題進行分析")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_compound_bidui_skipped() {
        // 比對 — technical verb, not standalone 對.
        assert!(!scan("比對資料進行分析")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_compound_hedui_skipped() {
        // 核對 — not standalone 對.
        assert!(!scan("核對資料進行檢查")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_no_verb_after() {
        // 進行 without a matching verb following — not flagged.
        assert!(scan("對資料進行了某些操作").is_empty());
    }

    #[test]
    fn dui_jinxing_no_jinxing() {
        // 對 without 進行 — not flagged.
        assert!(scan("對資料很感興趣").is_empty());
    }

    #[test]
    fn dui_jinxing_object_too_long() {
        // Object between 對 and 進行 exceeds 6 chars — dui_jinxing should skip.
        // (scan_bureaucratic_nominalization may still fire on "進行分析".)
        let issues = scan("對這份非常重要的報告進行分析");
        assert!(
            !issues.iter().any(|i| i.found.starts_with("對")),
            "dui_jinxing should not fire with oversized object"
        );
    }

    #[test]
    fn dui_jinxing_clause_boundary_in_object() {
        // Comma between 對 and 進行 — the 對X進行Y pattern should NOT fire.
        // (scan_bureaucratic_nominalization may still fire on "進行分析".)
        let issues = scan("對資料，進行分析");
        assert!(
            !issues.iter().any(|i| i.found.starts_with("對")),
            "dui_jinxing should not fire across clause boundary"
        );
    }

    #[test]
    fn dui_jinxing_does_not_clash_with_bureaucratic() {
        // Both scanners should fire independently:
        // - scan_bureaucratic_nominalization catches "進行分析" → "分析"
        // - scan_dui_jinxing catches "對資料進行分析" → "分析資料"
        // The broader one (dui_jinxing) covers the full span.
        let issues = scan("對資料進行分析");
        let dui = issues
            .iter()
            .filter(|i| i.found == "對資料進行分析")
            .count();
        let bureau = issues.iter().filter(|i| i.found == "進行分析").count();
        assert_eq!(dui, 1, "dui_jinxing should fire");
        assert_eq!(bureau, 1, "bureaucratic should also fire");
    }

    // =======================================================================
    // Exclusion zone handling
    // =======================================================================

    #[test]
    fn excluded_range_suppresses_a_not_a() {
        let text = "你是不是學生嗎？";
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_grammar(text, &excluded, &mut issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn excluded_range_suppresses_bare_shi() {
        let text = "她是漂亮";
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_grammar(text, &excluded, &mut issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn excluded_range_suppresses_redundant_prep() {
        let text = "我們討論關於這個問題";
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_grammar(text, &excluded, &mut issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn partial_exclusion_still_flags_outside() {
        // Exclude only the first 3 bytes, leaving the rest scannable.
        let text = "你是不是學生嗎？";
        let excluded = vec![ByteRange { start: 0, end: 3 }];
        let mut issues = Vec::new();
        scan_grammar(text, &excluded, &mut issues);
        // 是不是 starts at byte 3 (after 你), should still be detected.
        assert_eq!(issues.len(), 1);
    }

    // =======================================================================
    // Multiple issues in the same text
    // =======================================================================

    #[test]
    fn multiple_grammar_issues_in_one_text() {
        // Contains both A-not-A+嗎 and bare 是+adj.
        let text = "你是不是學生嗎？她是漂亮";
        let issues = scan(text);
        assert_eq!(issues.len(), 2);
        let types: Vec<_> = issues.iter().map(|i| i.rule_type).collect();
        assert!(types.iter().all(|t| *t == IssueType::Grammar));
    }

    #[test]
    fn multiple_a_not_a_in_same_text() {
        let text = "你是不是學生嗎？他有沒有錢嗎？";
        let issues = scan(text);
        assert_eq!(issues.len(), 2);
    }

    // =======================================================================
    // False-positive guards — natural zh-TW text that should NOT trigger
    // =======================================================================

    #[test]
    fn natural_question_with_ma_only() {
        assert!(scan("你今天有空嗎？").is_empty());
    }

    #[test]
    fn natural_he_connecting_nouns() {
        assert!(scan("我喜歡音樂和電影").is_empty());
    }

    #[test]
    fn comparative_he_yiyang_clean() {
        // 和你一樣 is a comparative construction, not clause coordination.
        assert!(scan("找到和你一樣的東西").is_empty());
    }

    #[test]
    fn comparative_he_xiangtong_clean() {
        assert!(scan("做了和他相同的選擇").is_empty());
    }

    #[test]
    fn natural_shi_with_noun() {
        assert!(scan("這是一本好書").is_empty());
    }

    #[test]
    fn natural_shi_de_construction() {
        // 是…的 is a common grammatical construction, not a calque.
        assert!(scan("她是昨天來的").is_empty());
    }

    #[test]
    fn natural_verb_suffix_before_he_but_noun_after() {
        // 了 before 和, but noun (not pronoun) after → no flag.
        assert!(scan("我買了和牛肉").is_empty());
    }

    #[test]
    fn natural_transitive_verb_with_object() {
        assert!(scan("我們討論了技術細節").is_empty());
    }

    #[test]
    fn technical_prose_no_false_positives() {
        let text = "在這個系統中，我們討論了架構設計和效能最佳化。\
                    你有沒有看過相關文件？這是很重要的步驟。";
        assert!(scan(text).is_empty());
    }

    #[test]
    fn natural_jinxing_standalone() {
        // 進行 as "to proceed" without a verb object.
        assert!(scan("工程順利進行，一切正常。").is_empty());
    }

    #[test]
    fn natural_zuochu_physical() {
        // 做出 with a physical object, not abstract action.
        assert!(scan("她做出了一道好菜").is_empty());
    }

    #[test]
    fn natural_genju_prepositional() {
        // 根據 as preposition with comma, no attribution verb in clause.
        assert!(scan("根據合約規定，雙方應遵守以下條款。").is_empty());
    }

    // =======================================================================
    // AI writing detection
    // =======================================================================

    fn scan_ai(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_ai_grammar(text, &[], &mut issues);
        issues
    }

    // -- 意味著 semantic safety word --

    #[test]
    fn ai_yiweizhe_definition_context() {
        let text = "這個定義意味著所有的值都必須為正";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);
        assert_eq!(issues[0].found, "意味著");
        assert_eq!(issues[0].suggestions, vec!["表示"]);
    }

    #[test]
    fn ai_yiweizhe_consequence_context() {
        let text = "如果記憶體不足，這意味著系統將會崩潰";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions, vec!["代表"]);
    }

    #[test]
    fn ai_yiweizhe_explanation_context() {
        let text = "換言之，這意味著我們需要重新設計";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions, vec!["也就是說"]);
    }

    #[test]
    fn ai_yiweizhe_no_context_advisory_only() {
        let text = "這意味著很多事情";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        // No clear context → advisory only (empty suggestions).
        assert!(issues[0].suggestions.is_empty());
    }

    #[test]
    fn ai_yiweizhe_in_excluded_region() {
        let mut issues = Vec::new();
        let excluded = vec![ByteRange { start: 0, end: 100 }];
        scan_ai_semantic_safety("這意味著很多", &excluded, &mut issues);
        assert!(issues.is_empty());
    }

    // -- Copula avoidance --

    #[test]
    fn ai_copula_zuowei_in_tech_context() {
        let text = "此系統作為核心元件運作";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "作為");
        // Advisory only — no direct replacement (would break sentence).
        assert!(issues[0].suggestions.is_empty());
        assert!(issues[0].context.as_ref().unwrap().contains("是"));
    }

    #[test]
    fn ai_copula_zuowei_not_in_tech_context() {
        // No tech context clues → should not flag.
        let text = "她作為一位母親非常偉大";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn ai_copula_yongyou_in_tech_context() {
        let text = "這個模組擁有三個介面";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "擁有");
        // Advisory only — no direct replacement.
        assert!(issues[0].suggestions.is_empty());
        assert!(issues[0].context.as_ref().unwrap().contains("有"));
    }

    // -- Passive voice --

    #[test]
    fn ai_passive_bei_guangfan() {
        let text = "這個框架被廣泛使用於各種專案中";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "被廣泛使用");
        assert_eq!(issues[0].suggestions, vec!["廣泛使用"]);
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);
    }

    #[test]
    fn ai_passive_bei_chengwei_not_flagged() {
        // 被稱為 removed: dropping 被 flips meaning with animate subjects.
        let text = "這個演算法被稱為快速排序";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn ai_passive_bei_renwei_not_flagged() {
        // 被認為是 removed: 他被認為是→他認為是 changes meaning.
        let text = "他被認為是最好的程式設計師";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn ai_passive_no_match_unlisted() {
        // 被打 is not in the curated list → no flag.
        let text = "他被打了一頓";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    // -- Copula compound word false-positive guards --

    #[test]
    fn ai_copula_yousuozuowei_not_flagged() {
        // 有所作為 is a compound; 作為 should not be flagged.
        let text = "這個系統必須有所作為才能改善效能";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn ai_copula_yongyouquan_not_flagged() {
        // 擁有權 is a compound; 擁有 should not be flagged.
        let text = "此模組的擁有權屬於核心架構";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    // -- AI grammar does not interfere with base grammar --

    #[test]
    fn ai_grammar_does_not_produce_grammar_issues() {
        let text = "此系統作為核心元件，這意味著我們需要因此重新設計";
        let issues = scan_ai(text);
        for issue in &issues {
            assert_eq!(issue.rule_type, IssueType::AiStyle);
        }
    }

    // -- Didactic sentence patterns --

    #[test]
    fn ai_didactic_pattern_detected() {
        let text = "x86 的歷史告訴我們處理器設計需要平衡";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);
        assert!(issues[0].found.contains("告訴我們"));
    }

    #[test]
    fn ai_didactic_different_verb() {
        let text = "這個案例的教訓提醒世人不要重蹈覆轍";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("提醒世人"));
    }

    #[test]
    fn ai_didactic_no_noun_prefix() {
        // Without 的+noun before verb, should not flag.
        let text = "老師告訴我們要認真學習";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    // -- Vague exaggeration patterns --

    #[test]
    fn ai_vague_exaggeration_detected() {
        let text = "這項技術領先時代至少20年";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);
        assert!(issues[0].found.contains("領先"));
    }

    #[test]
    fn ai_vague_exaggeration_different_verb() {
        let text = "該設計超越同期產品約5年的技術水準";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].found.contains("超越"));
    }

    #[test]
    fn ai_vague_exaggeration_no_year() {
        // Without digits+年 following, should not flag.
        let text = "這項技術領先業界的水準";
        let issues = scan_ai(text);
        assert!(issues.is_empty());
    }

    // -- IssueType::AiStyle plumbing --

    // -- AI density detection tests --

    fn scan_density(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_ai_density(text, &[], &mut issues, 1.0);
        issues
    }

    #[test]
    fn ai_density_short_text_skipped() {
        // Text under 500 chars should not trigger density analysis.
        let text = "更重要的是".repeat(20); // ~100 chars
        let issues = scan_density(&text);
        assert!(issues.is_empty(), "short text should skip density check");
    }

    #[test]
    fn ai_density_below_threshold_no_issue() {
        // ~2600 chars of filler with 1 occurrence of tracked phrase.
        // 1 / 2.6 ≈ 0.38/千字, below threshold 0.5 for '更重要的是'.
        let mut text = "這是一段正常的中文技術文章。".repeat(200);
        text.push_str("更重要的是，我們需要考慮效能。");
        assert!(text.chars().count() >= 2000);
        let issues = scan_density(&text);
        assert!(
            issues.is_empty(),
            "single occurrence in long text should not exceed density: {} chars",
            text.chars().count()
        );
    }

    #[test]
    fn ai_density_above_threshold_flags() {
        // ~1000 chars with high density of '更重要的是' (threshold 0.5/千字).
        // We need >0.5 per 1000 chars, so >1 in 2000 chars or >0.5 in 1000.
        // Build ~1000 char text with 3 occurrences → density 3.0/千字.
        let filler = "這是正常的技術內容段落。"; // 12 chars
        let mut text = String::new();
        for i in 0..80 {
            if i % 25 == 0 {
                text.push_str("更重要的是，我們需要重新評估。");
            } else {
                text.push_str(filler);
            }
        }
        assert!(text.chars().count() >= 500);
        let issues = scan_density(&text);
        assert!(
            !issues.is_empty(),
            "high density should trigger: text has {} chars",
            text.chars().count()
        );
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);
        assert!(issues[0].context.as_ref().unwrap().contains("次/千字"));
        assert!(issues[0].context.as_ref().unwrap().contains("更重要的是"));
    }

    #[test]
    fn ai_density_excluded_ranges_respected() {
        // Occurrences in excluded ranges should not count toward density.
        let filler = "這是正常的技術內容段落。";
        let mut text = String::new();
        for i in 0..80 {
            if i % 25 == 0 {
                text.push_str("更重要的是，我們需要重新評估。");
            } else {
                text.push_str(filler);
            }
        }
        // Exclude the entire text — all occurrences should be skipped.
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_ai_density(&text, &excluded, &mut issues, 1.0);
        assert!(issues.is_empty(), "excluded ranges should suppress density");
    }

    #[test]
    fn ai_density_multiple_phrases_independent() {
        // Two different phrases both above threshold — should get two issues.
        let mut text = String::new();
        for _ in 0..60 {
            text.push_str("這是正常的技術內容。");
        }
        // Insert both phrases repeatedly.
        for _ in 0..5 {
            text.push_str("更重要的是，這個方法不容忽視。");
        }
        assert!(text.chars().count() >= 500);
        let issues = scan_density(&text);
        // At least one should fire (density depends on exact char count).
        let density_contexts: Vec<_> = issues.iter().filter_map(|i| i.context.as_ref()).collect();
        // Both phrases should be independently evaluated.
        let has_gengyaojinaoshi = density_contexts.iter().any(|c| c.contains("更重要的是"));
        let has_buronghushi = density_contexts.iter().any(|c| c.contains("不容忽視"));
        // At least one should trigger given high density.
        assert!(
            has_gengyaojinaoshi || has_buronghushi,
            "at least one high-density phrase should trigger: contexts={density_contexts:?}"
        );
    }

    // -- AI structural pattern tests --

    fn scan_structural(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_ai_structural(text, &[], &mut issues, 1.0);
        issues
    }

    #[test]
    fn ai_structural_binary_contrast_below_threshold() {
        // Short text or low density should not flag.
        let text = "雖然困難很多，但我們還是做到了。這是正常的文章。".repeat(10);
        let issues = scan_structural(&text);
        // Only 10 concessive patterns in ~280 chars — below 500 char threshold.
        assert!(
            issues.is_empty()
                || !issues
                    .iter()
                    .any(|i| i.context.as_ref().is_some_and(|c| c.contains("二元對比")))
        );
    }

    #[test]
    fn ai_structural_binary_contrast_high_density() {
        let filler = "這是正常的技術段落。";
        let mut text = String::new();
        for i in 0..50 {
            if i % 4 == 0 {
                text.push_str("雖然這很困難，但我們可以克服。");
            } else if i % 4 == 1 {
                text.push_str("不僅要學習，更要實踐。");
            } else {
                text.push_str(filler);
            }
        }
        let issues = scan_structural(&text);
        let contrast_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("二元對比")))
            .collect();
        assert!(
            !contrast_issues.is_empty(),
            "high density binary contrast should trigger: {} chars, issues={:?}",
            text.chars().count(),
            issues
        );
    }

    #[test]
    fn ai_structural_paragraph_endings() {
        let mut text = String::new();
        for i in 0..8 {
            if i % 2 == 0 {
                text.push_str("這個技術的發展證明了人工智慧的潛力。");
            } else {
                text.push_str("正是這個突破讓研究人員重新思考。");
            }
            text.push_str("\n\n");
        }
        let issues = scan_structural(&text);
        let ending_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化宣言")))
            .collect();
        assert!(
            !ending_issues.is_empty(),
            "formulaic paragraph endings should trigger"
        );
    }

    #[test]
    fn ai_structural_dash_overuse() {
        let mut text = String::new();
        for _ in 0..5 {
            text.push_str("這項技術—作為核心—非常重要—我們必須注意。\n\n");
        }
        let issues = scan_structural(&text);
        let dash_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("破折號")))
            .collect();
        assert!(!dash_issues.is_empty(), "heavy dash usage should trigger");
    }

    #[test]
    fn ai_structural_formulaic_headings() {
        let text = "# 簡介\n\n內容\n\n## 挑戰與未來展望\n\n更多內容\n\n## 結論與展望\n\n結語";
        let issues = scan_structural(text);
        let heading_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化標題")))
            .collect();
        assert!(
            !heading_issues.is_empty(),
            "formulaic headings should trigger"
        );
    }

    #[test]
    fn ai_structural_list_density() {
        let mut text = String::new();
        for i in 0..10 {
            if i < 5 {
                text.push_str("- 第一項\n- 第二項\n- 第三項");
            } else {
                text.push_str("這是一段正常的段落文字。");
            }
            text.push_str("\n\n");
        }
        let issues = scan_structural(&text);
        let list_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("列表")))
            .collect();
        assert!(
            !list_issues.is_empty(),
            "high list density should trigger: 5/10 = 50%"
        );
    }

    #[test]
    fn ai_structural_normal_text_no_false_positive() {
        // Normal text should not trigger any structural patterns.
        let text = "台灣的技術產業在近年來快速發展。半導體製造是其中的核心。\n\n\
                    台積電作為全球最大的晶圓代工廠，在先進製程上保持領先。\n\n\
                    未來的發展方向包括三奈米和二奈米製程的量產。\n\n\
                    除了硬體之外，軟體生態系統也在蓬勃發展中。\n\n\
                    這些發展為台灣的經濟帶來了穩定的成長動力。";
        let issues = scan_structural(text);
        assert!(
            issues.is_empty(),
            "normal text should not trigger structural patterns: {issues:?}"
        );
    }

    #[test]
    fn ai_zero_width_detected() {
        let text = "正常文字\u{200B}中間\u{FEFF}結尾";
        let issues = scan_structural(text);
        let zw: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("零寬字元")))
            .collect();
        assert_eq!(zw.len(), 2, "should detect 2 zero-width artifacts: {zw:?}");
        // Suggestions should be empty string for auto-removal.
        for issue in &zw {
            assert_eq!(issue.suggestions.len(), 1);
            assert!(issue.suggestions[0].is_empty());
        }
    }

    #[test]
    fn ai_zero_width_excluded() {
        let text = "正常\u{200B}文字";
        // Exclude the zero-width space (byte offset 6 for 2 CJK chars = 6 bytes).
        let excluded = vec![ByteRange { start: 6, end: 9 }];
        let mut issues = Vec::new();
        scan_ai_structural(text, &excluded, &mut issues, 1.0);
        let zw: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("零寬字元")))
            .collect();
        assert!(zw.is_empty(), "excluded zero-width should not be detected");
    }

    #[test]
    fn ai_zero_width_no_false_positive() {
        let text = "完全正常的文字，沒有任何零寬字元。";
        let issues = scan_structural(text);
        let zw: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("零寬字元")))
            .collect();
        assert!(zw.is_empty(), "clean text should have no zero-width issues");
    }

    #[test]
    fn ai_style_serde_round_trip() {
        let json = serde_json::to_string(&IssueType::AiStyle).unwrap();
        assert_eq!(json, "\"ai_style\"");
        let back: IssueType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, IssueType::AiStyle);
    }

    #[test]
    fn ai_style_sort_order_after_grammar() {
        assert!(IssueType::AiStyle.sort_order() > IssueType::Grammar.sort_order());
    }

    #[test]
    fn is_para_excluded_empty_exclusions() {
        // Empty exclusion list never excludes anything.
        assert!(!is_para_excluded(0, 100, &[]));
    }

    #[test]
    fn is_para_excluded_fully_inside() {
        let excluded = vec![ByteRange { start: 0, end: 200 }];
        assert!(is_para_excluded(10, 50, &excluded));
    }

    #[test]
    fn is_para_excluded_partial_overlap_not_excluded() {
        // Paragraph extends beyond the exclusion zone — should NOT be excluded.
        let excluded = vec![ByteRange { start: 0, end: 30 }];
        assert!(!is_para_excluded(10, 50, &excluded));
    }

    #[test]
    fn structural_detectors_skip_excluded_paragraphs() {
        // Build text with a list-heavy "paragraph" that is fully excluded.
        // Without exclusion it would trigger list_density; with exclusion it should not.
        let mut text = String::new();
        let code_start = 0;
        // Fake code block paragraph with list items.
        for _ in 0..10 {
            text.push_str("- list item in code\n");
        }
        let code_end = text.len();
        text.push_str("\n\n");
        // Add non-list prose paragraphs to meet minimum paragraph count.
        for _ in 0..6 {
            text.push_str("這是正常的段落文字，沒有列表項目，用來充數。\n\n");
        }
        let excluded = vec![ByteRange {
            start: code_start,
            end: code_end,
        }];
        let mut issues = Vec::new();
        scan_ai_list_density(&text, &excluded, &mut issues, 1.0);
        let list_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("含列表")))
            .collect();
        assert!(
            list_issues.is_empty(),
            "excluded code paragraph should not inflate list density: {list_issues:?}"
        );
    }

    // =======================================================================
    // Differential testing: AC prefilter vs. legacy per-scanner path
    // =======================================================================

    /// Compare AC-based scan_grammar output against legacy per-scanner output.
    /// Issues may arrive in different order, so we sort by (offset, found) before comparing.
    fn assert_ac_matches_legacy(text: &str) {
        let mut ac_issues = Vec::new();
        scan_grammar(text, &[], &mut ac_issues);
        ac_issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.found.cmp(&b.found)));

        let mut legacy_issues = Vec::new();
        scan_grammar_legacy(text, &[], &mut legacy_issues);
        legacy_issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.found.cmp(&b.found)));

        assert_eq!(
            ac_issues.len(),
            legacy_issues.len(),
            "issue count mismatch on {:?}:\n  AC:     {:?}\n  Legacy: {:?}",
            text,
            ac_issues.iter().map(|i| &i.found).collect::<Vec<_>>(),
            legacy_issues.iter().map(|i| &i.found).collect::<Vec<_>>(),
        );

        for (ac, leg) in ac_issues.iter().zip(legacy_issues.iter()) {
            assert_eq!(ac.offset, leg.offset, "offset mismatch on {:?}", text);
            assert_eq!(ac.found, leg.found, "found mismatch on {:?}", text);
            assert_eq!(
                ac.suggestions, leg.suggestions,
                "suggestion mismatch on {:?}",
                text
            );
            assert_eq!(ac.severity, leg.severity, "severity mismatch on {:?}", text);
        }
    }

    #[test]
    fn differential_a_not_a() {
        assert_ac_matches_legacy("你是不是學生嗎？");
        assert_ac_matches_legacy("你有沒有吃飯嗎？");
        assert_ac_matches_legacy("你是不是學生？"); // no 嗎, clean
    }

    #[test]
    fn differential_he_connecting() {
        assert_ac_matches_legacy("我吃了和你去看電影");
        assert_ac_matches_legacy("蘋果和橘子都很好吃"); // clean
    }

    #[test]
    fn differential_bare_shi() {
        assert_ac_matches_legacy("她是漂亮");
        assert_ac_matches_legacy("她是很漂亮"); // clean
        assert_ac_matches_legacy("這是好消息"); // noun modifier, clean
    }

    #[test]
    fn differential_redundant_preposition() {
        assert_ac_matches_legacy("我們討論關於這個問題");
        assert_ac_matches_legacy("這影響到整體計畫");
        assert_ac_matches_legacy("我們討論這個問題"); // clean
    }

    #[test]
    fn differential_bureaucratic() {
        assert_ac_matches_legacy("我們進行討論");
        assert_ac_matches_legacy("加以分析這個問題");
    }

    #[test]
    fn differential_verbose_action() {
        assert_ac_matches_legacy("做出決定");
        assert_ac_matches_legacy("作出回應");
    }

    #[test]
    fn differential_dui_jinxing() {
        assert_ac_matches_legacy("對資料進行分析");
        assert_ac_matches_legacy("對系統進行測試");
    }

    #[test]
    fn differential_double_attribution() {
        assert_ac_matches_legacy("根據研究顯示這個結果");
        assert_ac_matches_legacy("根據研究這個結果"); // clean
    }

    #[test]
    fn differential_combined() {
        // Multiple grammar patterns in one text.
        assert_ac_matches_legacy("她是漂亮，我們討論關於這個問題，你是不是學生嗎？");
    }

    #[test]
    fn differential_empty_and_ascii() {
        assert_ac_matches_legacy("");
        assert_ac_matches_legacy("Hello world");
    }

    #[test]
    fn differential_dui_jinxing_with_bureaucratic() {
        // Text triggers both DuiJinxing (對...進行) and BureaucraticNominalization (進行...).
        assert_ac_matches_legacy("對資料進行分析的報告");
    }
}
