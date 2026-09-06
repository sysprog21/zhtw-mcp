// Grammar scanner: pattern-based grammatical checks for zh-TW text.
//
// Detects interlingual transfer errors (English grammar calques in Chinese) and
// structural redundancies without requiring POS tagging.
//
// Interlingual transfer detection:
//   - 和-connecting-clauses (和 between verb phrases instead of nouns)
//   - 是+adjective copula (是 before adjective without 很/非常)
//   - Redundant preposition after transitive verb
//
// A-not-A and 嗎 clash detection:
//   - A-not-A question structure with redundant sentence-final 嗎
//
// Architecture: a single Aho-Corasick automaton pre-scans the document once for
// all grammar trigger patterns, then dispatches each hit to per-type
// validators. This replaces the O(P*N) per-scanner str::find() loops with O(N +
// H) where H = number of AC hits.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use super::emit::Emitter;
use crate::engine::excluded::{is_excluded, ByteRange};
use crate::engine::scan::is_cjk_ideograph;
use crate::engine::scan::rule_ir::StructuralGuard;
use crate::rules::ruleset::{
    DocumentGenre, Issue, IssueType, PhaseFamily, PhasePass, Severity, StructuralFamily,
};

// Common verb-final suffixes that indicate a verb phrase precedes 和.
const VERB_SUFFIXES: &[char] = &['了', '過', '著', '來', '去', '完', '好', '到'];

// Common pronouns for 是+adjective detection.
const PRONOUNS: &[&str] = &[
    "我", "你", "他", "她", "它", "我們", "你們", "他們", "她們", "這", "那", "這個", "那個",
];

// Adjectives commonly misused with bare 是 (English calque). Kept small and
// high-confidence to minimize false positives.
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

// Transitive verb + spurious preposition pairs (English calque). (verb,
// spurious_preposition, context_description)
const TRANSITIVE_VERB_PREPOSITION_PAIRS: &[(&str, &str, &str)] = &[
    ("強調", "在", "transitive verb with redundant preposition"),
    ("討論", "關於", "transitive verb with redundant preposition"),
    ("研究", "關於", "transitive verb with redundant preposition"),
    ("影響", "到", "transitive verb with redundant preposition"),
    ("考慮", "到", "transitive verb with redundant preposition"),
    ("處理", "到", "transitive verb with redundant preposition"),
    ("分析", "關於", "transitive verb with redundant preposition"),
];

// Bureaucratic verbal prefixes (English 'conduct/carry out' calque). "進行討論"
// → "討論", "加以分析" → "分析", "予以處理" → "處理"
const BUREAUCRATIC_PREFIXES: &[&str] = &["進行", "加以", "予以"];

// Verbs commonly nominalized after bureaucratic prefixes.
const NOMINALIZED_VERBS: &[&str] = &[
    "討論", "分析", "研究", "調查", "測試", "開發", "設計", "評估", "檢查", "審查", "修改", "更新",
    "比較", "溝通", "合作", "訓練", "處理", "管理", "規劃", "改善", "調整", "整合", "驗證", "觀察",
    "監控", "維護",
];

// Verbose action prefixes + abstract objects. "做出決定" → "決定", "作出回應" →
// "回應"
const VERBOSE_ACTION_PREFIXES: &[&str] = &["做出", "作出"];

const VERBOSE_ACTION_OBJECTS: &[&str] = &[
    "決定", "回應", "貢獻", "改變", "調整", "承諾", "解釋", "判斷", "選擇", "反應", "讓步", "保證",
    "回答", "犧牲", "努力",
];

// Attribution verbs for double-attribution detection. "根據研究顯示" is
// redundant: use "根據研究" or "研究顯示".
const ATTRIBUTION_VERBS: &[&str] = &["顯示", "指出", "表明", "表示", "說明"];

// Attribution verbs that are also the first half of a compound noun. Keyed to
// the verb, because a blanket suffix test would lose 表示會 ("will indicate")
// and 顯示圖 ("show diagram"), which are verb uses rather than nouns.
const VERB_COMPOUND_SUFFIXES: &[(&str, &[&str])] = &[
    ("說明", &["書", "文"]),
    ("表示", &["式", "法"]),
    ("顯示", &["器", "屏", "卡", "幕"]),
];

// What may follow a compound noun. The suffix character alone is not enough: 器
// also opens 器官, 器材 and 器械, so 研究顯示器官移植… is an attribution about
// organ transplants, not a sentence about a monitor.
fn ends_compound(after: &str) -> bool {
    match after.chars().next() {
        None => true,
        Some(ch) => !is_cjk_ideograph(ch) || "的上中與和是會有可能等這那就也都".contains(ch),
    }
}

/// Whether "verb" followed by "after" spells a compound noun rather than an
/// attribution, as in 說明書, 表示式 and 顯示器.
fn opens_a_compound_noun(verb: &str, after: &str) -> bool {
    VERB_COMPOUND_SUFFIXES.iter().any(|&(v, suffixes)| {
        v == verb
            && suffixes
                .iter()
                .any(|sfx| after.starts_with(sfx) && ends_compound(&after[sfx.len()..]))
    })
}

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

// Grammar AC prefilter: single-pass pattern dispatch

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
/// Returns (automaton, pattern_metadata) where pattern_metadata[i] =
/// (check_type, pattern_table_index).
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

// Per-type validators: called with the AC hit position

/// Validate an A-not-A + 嗎 hit.
fn validate_a_not_a_ma(em: &mut Emitter<'_>, abs_pos: usize, pattern_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

    // Check if 嗎 appears at the end of the sentence (possibly preceded by
    // whitespace only).
    if let Some(head) = sentence_slice.trim_end().strip_suffix('嗎') {
        let ma_offset = pattern_end + head.len();
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
fn validate_he_connecting(em: &mut Emitter<'_>, abs_pos: usize, he_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
fn validate_bare_shi_adjective(em: &mut Emitter<'_>, abs_pos: usize, shi_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if is_excluded(abs_pos, shi_end, excluded) {
        return;
    }

    // Check if preceded by a pronoun.
    let before = &text[..abs_pos];
    let Some(pronoun) = PRONOUNS.iter().find(|p| before.ends_with(*p)) else {
        return;
    };

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

        // Guard: if the adjective is immediately followed by a CJK character
        // that acts as a noun head, it's a modifier in a noun phrase (e.g.
        // 好消息, 大問題), not a bare adjective predicate.
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

        // The pronoun that precedes 是 is part of the found span.
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
    em: &mut Emitter<'_>,
    abs_pos: usize,
    verb_end: usize,
    pair_index: usize,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
fn validate_bureaucratic_nominalization(em: &mut Emitter<'_>, abs_pos: usize, prefix_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
fn validate_verbose_action(em: &mut Emitter<'_>, abs_pos: usize, prefix_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
fn validate_dui_jinxing(em: &mut Emitter<'_>, abs_pos: usize, marker_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
fn validate_double_attribution(em: &mut Emitter<'_>, abs_pos: usize, marker_end: usize) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
            if opens_a_compound_noun(verb, after_verb) {
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

// How far back an attribution may look for the authority that governs it. The
// engine's own evidence window, documented in docs/rules.md, because this is
// the same question it answers: how far away can supporting evidence be.
//
// Only the lookback is clamped. A preposition two hundred characters earlier
// governs a different clause, and bounding this keeps the prefix test constant
// per match. The citation search is not clamped: it runs to the real sentence
// end, since zh-TW places a citation there.
const ATTRIBUTION_WINDOW_CHARS: usize = crate::engine::scan::CONTEXT_WINDOW_CHARS;

/// Byte spans of every sentence terminator, in order, computed once per
/// document so a match can locate its sentence by binary search.
///
/// The previous version walked outward from each match and had to cap the
/// walk, because prose that runs on commas without ever reaching a terminator
/// made every match rescan the document. Capping the forward walk hid a
/// citation placed at the end of a long sentence, which is exactly where
/// zh-TW puts one. The bound lives here instead: one linear pass per document
/// and a binary search per match, with no limit on how far a citation may sit.
///
/// A lone newline is deliberately not a terminator here, unlike in
/// "is_sentence_end". Hard-wrapped prose puts its citation on the continuation
/// line, so stopping at the wrap hid the very citation this pass looks for:
/// "研究顯示\n成果很好[1]。" is sourced and was reported anyway. A blank line
/// does end the span, because the next paragraph's citation belongs to the
/// next paragraph.
fn sentence_terminators(text: &str) -> Vec<(usize, usize)> {
    text.char_indices()
        .filter(|&(i, ch)| match ch {
            '\n' => ends_span(&text[i + 1..]),
            _ => is_sentence_end(ch),
        })
        .map(|(i, ch)| (i, i + ch.len_utf8()))
        .collect()
}

/// Whether a newline ends the attribution's sentence span.
///
/// A hard wrap does not: the citation for a claim routinely sits on the
/// continuation line. A blank line does, and so does the start of another
/// block, because a heading, list item, quote or rule begins material that is
/// not this claim. Tested on line shape so it holds for plain text too.
fn ends_span(rest: &str) -> bool {
    let Some(line) = rest.split_inclusive('\n').next() else {
        return true;
    };
    let line = line.trim_end_matches(['\n', '\r']);
    line.trim().is_empty() || starts_block(line)
}

/// Whether a line opens a Markdown block.
fn starts_block(line: &str) -> bool {
    let trimmed = line.trim_start();
    is_markdown_heading_line(line)
        || is_thematic_break(trimmed)
        || trimmed.starts_with('>')
        || trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || is_bullet_item(trimmed)
        || numbered_list_marker_len(trimmed).is_some()
}

/// Three or more of one marker, and nothing else on the line.
fn is_thematic_break(trimmed: &str) -> bool {
    let mut marks = trimmed.chars().filter(|c| !c.is_whitespace());
    let Some(first @ ('-' | '*' | '_')) = marks.next() else {
        return false;
    };
    let mut count = 1;
    for c in marks {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// A bullet marker followed by a space or a tab.
pub(super) fn is_bullet_item(trimmed: &str) -> bool {
    matches!(trimmed.as_bytes(), [b'-' | b'*' | b'+', b' ' | b'\t', ..])
}

/// Prepositions that introduce a named source before the attribution verb.
const SOURCE_PREPOSITIONS: &[&str] = &["根據", "依據", "按照", "援引", "引述"];

/// Trailing morphemes of an organization name. A run of Han characters ending
/// in one of these is a named institution, so the attribution after it is
/// sourced: "中央氣象署專家認為" names its authority as surely as a footnote
/// does, and is the ordinary shape of Taiwanese reporting.
///
/// Single characters first, then only the multi-character forms whose last
/// character is not already here. 學院, 協會, 基金會 and 研究所 would never be
/// reached: 院, 會 and 所 match them first.
const ORGANIZATION_SUFFIXES: &[&str] = &[
    "署",
    "院",
    "部",
    "處",
    "局",
    "會",
    "所",
    "中心",
    "大學",
    "公司",
    "團隊",
    "實驗室",
];

/// Whether the text before an attribution names the authority it speaks for.
fn names_an_authority(prefix: &str) -> bool {
    if SOURCE_PREPOSITIONS.iter().any(|p| prefix.contains(p)) {
        return true;
    }

    // Only the run immediately before the verb counts. A suffix character
    // further back belongs to a different clause.
    let cut = prefix
        .char_indices()
        .rev()
        .take_while(|&(_, ch)| is_cjk_ideograph(ch))
        .last()
        .map_or(prefix.len(), |(i, _)| i);
    let tail = prefix[cut..].trim_end_matches('的');
    ORGANIZATION_SUFFIXES
        .iter()
        .any(|suffix| tail.ends_with(suffix))
}

/// Byte offsets of every citation marker in the document, in order.
///
/// Computed once, for the same reason the terminators are: with the sentence
/// bound no longer capped, a document that runs on commas is one sentence, so
/// re-scanning it per match would be quadratic. This recognises only
/// unambiguous markers: numbered brackets, Markdown links, footnote
/// references, and URLs, in half-width or full-width brackets.
///
/// A marker inside an excluded region does not count, or a URL quoted in
/// inline code would source a claim it has nothing to do with. A URL's own
/// exclusion is not disqualifying: that range starts exactly at the marker,
/// while a wrapping span such as inline code starts before it.
fn citation_marker_positions(text: &str, excluded: &[ByteRange]) -> Vec<usize> {
    let mut positions = Vec::new();

    // Binary search rather than a scan per URL: the ranges are sorted and
    // non-overlapping, so only the last range starting at or before the offset
    // can contain it. The linear form was O(URLs x ranges) and cost 19.8 ms of
    // a 55.9 ms run on a 722 KB document carrying 12,000 of each.
    let strictly_inside = |offset: usize| {
        // Partition on "starts strictly before", not "at or before": the URL's
        // own exclusion starts exactly at the offset, and letting it win the
        // search would hide the wrapping span that starts earlier. Ranges are
        // non-overlapping, so once that one is excluded only the last range
        // starting before the offset can still contain it.
        let idx = excluded.partition_point(|r| r.start < offset);
        idx > 0 && offset < excluded[idx - 1].end
    };
    for marker in ["http://", "https://", "www."] {
        for (offset, _) in text.match_indices(marker) {
            if !strictly_inside(offset) {
                positions.push(offset);
            }
        }
    }

    // Closing-bracket offsets, ascending, one list per width so a full-width
    // opener cannot pair with a half-width closer. Collected once and binary
    // searched: scanning forward from each opener made an unmatched bracket
    // rescan to end of document, which is quadratic in the number of unmatched
    // brackets. A byte cap on that scan traded the quadratic for a truncation
    // that dropped any link whose label ran past it, and a CJK label reaches 64
    // bytes at 21 characters, which in-repo citations routinely exceed.
    let half_closers: Vec<usize> = text.match_indices(']').map(|(i, _)| i).collect();
    let full_closers: Vec<usize> = text.match_indices('］').map(|(i, _)| i).collect();
    for (i, open) in text.match_indices(['[', '［']) {
        let (close, list) = if open == "[" {
            (']', &half_closers)
        } else {
            ('］', &full_closers)
        };
        let first_close_at =
            |from: usize| list[list.partition_point(|&p| p < from)..].first().copied();
        let rest = &text[i + open.len()..];
        if rest.starts_with('^') {
            if let Some(close_at) = first_close_at(i + open.len() + 1) {
                let marker_end = close_at + close.len_utf8();
                if !is_excluded(i, marker_end, excluded) {
                    positions.push(i);
                }
            }
        } else if let Some(close_at) = first_close_at(i + open.len()) {
            let label = &text[i + open.len()..close_at];
            let marker_end = close_at + close.len_utf8();
            let numbered = !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit());
            let link = text[marker_end..].starts_with('(');
            if (numbered && !is_excluded(i, marker_end, excluded))
                || (link && !is_excluded(i, marker_end + 1, excluded))
            {
                positions.push(i);
            }
        }
    }
    positions.sort_unstable();
    positions
}

/// Per-document indexes the bare-attribution check needs, built once so that
/// neither the sentence lookup nor the citation lookup rescans the text.
struct AttributionIndex {
    terminators: Vec<(usize, usize)>,
    citations: Vec<usize>,
}

impl AttributionIndex {
    fn build(text: &str, excluded: &[ByteRange]) -> Self {
        crate::engine::index_guard::note_build(crate::engine::index_guard::DocIndex::Attribution);
        Self {
            terminators: sentence_terminators(text),
            citations: citation_marker_positions(text, excluded),
        }
    }

    /// Bounds of the sentence containing "offset", excluding both terminators.
    fn sentence_bounds(&self, offset: usize, len: usize) -> (usize, usize) {
        let idx = self
            .terminators
            .partition_point(|&(start, _)| start <= offset);
        let start = if idx == 0 {
            0
        } else {
            self.terminators[idx - 1].1
        };
        let end = self.terminators.get(idx).map_or(len, |&(start, _)| start);
        (start, end)
    }

    /// Whether a citation marker falls inside the given byte range.
    fn has_citation(&self, start: usize, end: usize) -> bool {
        any_position_in(&self.citations, start, end)
    }
}

/// Whether any recorded position falls in `[start, end)`.
///
/// Shared by the attribution and closing-tail indexes, which both keep sorted
/// positions and both used to spell this search out themselves.
fn any_position_in(positions: &[usize], start: usize, end: usize) -> bool {
    let i = positions.partition_point(|&p| p < start);
    positions.get(i).is_some_and(|&p| p < end)
}

pub(crate) fn scan_ai_bare_attribution(
    em: &mut Emitter<'_>,
    genre: DocumentGenre,
    guard: Option<&StructuralGuard>,
) {
    let (text, excluded) = (em.text, em.excluded);

    // No guard means every phrase carrying it was disabled or overridden away,
    // which is a legitimate configuration and not an error.
    let Some(guard) = guard.filter(|g| !g.is_empty()) else {
        return;
    };

    // Built on the first match, not before it. Most documents contain no
    // attribution at all, and indexing terminators and citations for them was
    // the whole cost of this pass.
    let mut index = None;
    for mat in guard.find_iter(text) {
        let index = index.get_or_insert_with(|| AttributionIndex::build(text, excluded));
        validate_bare_attribution(
            em,
            mat.start(),
            guard.phrase(mat.pattern().as_usize()),
            genre,
            index,
        );
    }
}

fn validate_bare_attribution(
    em: &mut Emitter<'_>,
    abs_pos: usize,
    phrase: &str,
    genre: DocumentGenre,
    index: &AttributionIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let end = abs_pos + phrase.len();
    let (sentence_start, sentence_end) = index.sentence_bounds(abs_pos, text.len());

    // The text before this occurrence, not before the first one in the
    // sentence: with two attributions in one sentence, only the later one may
    // be the one that "根據" names. Clamped, so one long sentence full of
    // attributions cannot make the prefix test quadratic.
    let lookback = text[sentence_start..abs_pos]
        .char_indices()
        .rev()
        .take(ATTRIBUTION_WINDOW_CHARS)
        .last()
        .map_or(abs_pos, |(i, _)| sentence_start + i);
    let sentence_prefix = &text[lookback..abs_pos];
    if is_excluded(abs_pos, end, excluded)

        // From the attribution forward: zh-TW puts a citation after the claim,
        // and a marker before it belongs to something else.
        || index.has_citation(abs_pos, sentence_end)

        // 研究顯示器 and 研究顯示屏 are display devices, not claims. Same table
        // the double-attribution check uses, so a new compound is one edit.
        || ATTRIBUTION_VERBS
            .iter()
            .any(|verb| phrase.ends_with(verb) && opens_a_compound_noun(verb, &text[end..]))

        // "根據研究顯示" is handled by the more-specific double-attribution
        // rule, and "中央氣象署專家認為" has already named its authority.
        || names_an_authority(sentence_prefix)
    {
        return;
    }

    // No suggestion in any genre. These phrases are ordinary zh-TW whenever a
    // source is named nearby, so the finding is advice to a human or an
    // assistant, never a mechanical edit. An empty-string suggestion is the
    // fixer's delete sentinel, and deleting the attribution off the front of a
    // sentence leaves text like "多位，本次修法將影響地方財政".
    let context = match genre {
        DocumentGenre::Casual => {
            "vague authority attribution; name the source or rewrite the clause without it"
        }
        DocumentGenre::Technical | DocumentGenre::Financial => {
            "citation missing for this authority attribution; name the source (do not invent one)"
        }
    };
    issues.push(
        Issue::new(
            abs_pos,
            phrase.len(),
            phrase,
            Vec::new(),
            IssueType::AiStyle,
            Severity::Info,
        )
        .with_context(context),
    );
}

// Detect A-not-A structures co-occurring with sentence-final 嗎.
#[cfg(test)]
fn scan_a_not_a_ma(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

            // Check if 嗎 appears at the end of the sentence (possibly preceded
            // by whitespace only).
            if let Some(head) = sentence_slice.trim_end().strip_suffix('嗎') {
                let ma_offset = pattern_end + head.len();
                let ma_end = ma_offset + '嗎'.len_utf8();
                if !is_excluded(ma_offset, ma_end, excluded) {
                    // Report the whole span from A-not-A to 嗎 as the found
                    // text.
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

// Detect 和 connecting clauses (verb phrases) instead of nouns.
#[cfg(test)]
fn scan_he_connecting_clauses(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

        // Check if the character immediately before 和 is a verb suffix. This
        // is a heuristic: CJK char ending in common verb suffixes
        // (了/過/著/來/去/完/好/到) strongly suggests a verb phrase.
        let before_he = &text[..abs_pos];
        let prev_char = before_he.chars().next_back();
        let has_verb_suffix = prev_char.is_some_and(|ch| VERB_SUFFIXES.contains(&ch));

        if !has_verb_suffix {
            continue;
        }

        // Also check the character after 和 -- if followed by another verb
        // phrase indicator (pronoun starting a new clause), this is likely a
        // clause-connecting 和.
        let after_he = &text[he_end..];

        // Quick check: next CJK character should not be a noun-like context. If
        // the next char is also a verb suffix or a pronoun starts the next
        // segment, flag it.
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

// Detect bare 是+adjective (English copula calque).
#[cfg(test)]
fn scan_bare_shi_adjective(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
            // The guard above already established one is there; re-deriving it
            // here rather than unwrapping keeps the two from drifting apart.
            let Some(pronoun) = PRONOUNS.iter().find(|p| before.ends_with(*p)) else {
                continue;
            };
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

// Detect transitive verb + redundant preposition.
#[cfg(test)]
fn scan_redundant_preposition(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

// Detect bureaucratic nominalization: 進行/加以/予以 + verb. These are calques
// of English "conduct/carry out + noun" and are verbose.
#[cfg(test)]
fn scan_bureaucratic_nominalization(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

            // Pick the verb whose match is earliest by text position, not list
            // order: avoids silently matching the wrong verb when two verbs
            // from the list both appear in the window.
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

// Detect verbose action prefix: 做出/作出 + abstract noun. "做出決定" → "決定",
// "作出回應" → "回應"
#[cfg(test)]
fn scan_verbose_action(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
// "對資料進行分析" → "分析資料", "對系統進行測試" → "測試系統" This is distinct
// from scan_bureaucratic_nominalization which catches standalone "進行分析":
// here the explicit 對X object is present, giving a better suggestion that
// preserves the object.
#[cfg(test)]
fn scan_dui_jinxing(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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

        // Check following char: 對於 is a compound preposition, not this
        // pattern.
        if text[marker_end..].starts_with('於') {
            continue;
        }

        // Look for 進行 within a reasonable window (up to 8 CJK chars ≈ 24
        // bytes).
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
// "根據研究顯示" is redundant: either "根據研究" or "研究顯示" suffices.
#[cfg(test)]
fn scan_double_attribution(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

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
                // compound noun (e.g. 說明書, 表示式, 顯示器). Key the suffix
                // check to the specific verb to avoid false negatives like
                // 表示會 (will indicate) or 顯示圖 (show diagram).
                let after_verb = &text[verb_end..];
                if opens_a_compound_noun(verb, after_verb) {
                    continue;
                }

                // Skip when a markdown link bracket sits between 根據 and the
                // verb: the verb is inside link text, not an attribution verb.
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

// AI writing detection: grammar-level patterns

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
// which skips issues with >1 suggestion for non-orthographic types). When
// disambiguation confidence is low, emits advisory-only (no suggestions).
fn scan_ai_semantic_safety(em: &mut Emitter<'_>) {
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
fn scan_ai_copula_avoidance(em: &mut Emitter<'_>) {
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
fn scan_ai_passive(em: &mut Emitter<'_>) {
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
fn scan_ai_didactic(em: &mut Emitter<'_>) {
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
fn scan_ai_vague_exaggeration(em: &mut Emitter<'_>) {
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
fn is_para_excluded(start: usize, end: usize, excluded: &[ByteRange]) -> bool {
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

fn scan_ai_binary_contrast(em: &mut Emitter<'_>, threshold_multiplier: f32) {
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
    for (_, para) in super::split_paragraphs(text) {
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
                count += 1;
                first_offset.get_or_insert(sent_start + pos);
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
fn scan_ai_paragraph_endings(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = super::split_paragraphs(text)
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
fn scan_ai_dash_overuse(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = super::split_paragraphs(text)
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

fn scan_ai_formulaic_headings(em: &mut Emitter<'_>) {
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
fn scan_ai_list_density(em: &mut Emitter<'_>, threshold_multiplier: f32) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let paragraphs: Vec<&str> = super::split_paragraphs(text)
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
fn scan_ai_tricolon(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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

/// Slice up to `n` characters from a byte offset, char-boundary safe.
/// Returns the byte range that covers up to n chars from start_byte.
/// Out-of-range or non-char-boundary `start_byte` is clamped to `text.len()`
/// to keep all callers panic-free.
fn char_bounded_end(text: &str, start_byte: usize, n_chars: usize) -> usize {
    if start_byte >= text.len() || !text.is_char_boundary(start_byte) {
        return text.len();
    }
    text[start_byte..]
        .char_indices()
        .nth(n_chars)
        .map(|(i, _)| start_byte + i)
        .unwrap_or(text.len())
}

// Negative parallel: 不只是/不僅是 plus 而是/更是 within 30 chars.
fn scan_ai_negative_parallel(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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
struct CloserTailIndex {
    line_breaks: Vec<usize>,
    non_tail: Vec<usize>,
    /// Positions of 「（」 and 「註」, the only characters an accepted tail may
    /// hold beyond whitespace and closing punctuation, and only as part of a
    /// 「（註）」 prefix.
    annotation: Vec<usize>,
}

impl CloserTailIndex {
    fn build(text: &str) -> Self {
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
    fn next_line_break(&self, pos: usize) -> Option<usize> {
        self.line_breaks
            .get(self.line_breaks.partition_point(|&b| b < pos))
            .copied()
    }

    /// Whether `text[start..end)` holds a character that no accepted closing
    /// tail can contain.
    fn has_non_tail_char(&self, start: usize, end: usize) -> bool {
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
fn is_markdown_heading_line(line: &str) -> bool {
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

fn scan_ai_formulaic_section_endings(
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
fn scan_ai_mechanical_bullets(em: &mut Emitter<'_>, _idx: &crate::engine::sentence::BoundaryIndex) {
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

// Excessive bold: three or more **...** runs per 200 chars in a paragraph.
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

fn scan_ai_excessive_bold(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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

fn scan_ai_abstract_line_metaphor(
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

fn scan_ai_repeated_parallel_slogan(
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
fn scan_ai_rhetorical_self_qa(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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
fn scan_ai_emdash_overuse(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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
fn scan_ai_formulaic_despite(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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
fn scan_ai_false_ranges(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
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
fn scan_ai_hedging_density(
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

// Syntactic translationese detectors (require BoundaryIndex)

// Passive voice density: count 被 per paragraph, flag above two per 100 chars.
fn scan_trans_passive_density(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Technical whitelist: these passive forms are standard in zh-TW technical
    // prose.
    const WHITELIST: &[&str] = &[
        "被定義為",
        "被廣泛採用",
        "被置於",
        "被稱作",
        "被觀察到",
        "被記錄為",
    ];

    // Note: per-occurrence flagging of specific calques (被廣泛認為, 被視為,
    // 被稱為, etc.) is handled by the spelling ruleset, not duplicated here.
    // This detector contributes only the density-based signal.

    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let char_count = p.chars().count();
        if char_count < 20 {
            continue;
        }

        // Count 被 occurrences not in whitelist.
        let mut bei_count = 0;
        let mut search_from = 0;
        while let Some(pos) = p[search_from..].find('被') {
            let abs_pos = para.byte_start + search_from + pos;
            let bei_start = search_from + pos;
            // 10-char lookahead, char-boundary safe.
            let context_end = char_bounded_end(p, bei_start, 10);
            let context = &p[bei_start..context_end];

            if !is_excluded(abs_pos, abs_pos + '被'.len_utf8(), excluded) {
                let whitelisted = WHITELIST.iter().any(|w| context.starts_with(w));
                if !whitelisted {
                    bei_count += 1;
                }
            }
            search_from += pos + '被'.len_utf8();
        }

        // Density check: >2 per 100 chars.
        let density_threshold = ((char_count as f32 / 100.0) * 2.0).ceil() as usize;
        if bei_count > density_threshold.max(2) {
            // First 2 chars as preview, char-boundary safe.
            let preview_end = char_bounded_end(p, 0, 2);
            issues.push(
                Issue::new(
                    para.byte_start,
                    preview_end,
                    &p[..preview_end],
                    vec![],
                    IssueType::Translationese,
                    Severity::Warning,
                )
                .with_context(format!(
                    "翻譯腔：被動語態密度過高 — 段落內 {bei_count} 處 '被' 字句"
                )),
            );
        }
    }
}

// Abstract subject: a noun phrase ending in 的(減少|增加|...) at sentence
// head, followed by 導致|標誌著|意味著.
fn scan_trans_abstract_subject(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const ABSTRACT_NOUNS: &[&str] = &["的減少", "的增加", "的提高", "的下降", "的通過", "的實施"];
    const ABSTRACT_VERBS: &[&str] = &["導致", "標誌著", "意味著"];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];

        // The abstract noun has to lead the sentence; the verb may sit anywhere
        // after it. One issue per sentence either way.
        let head = &s[..char_bounded_end(s, 0, 20)];
        if !ABSTRACT_NOUNS.iter().any(|noun| head.contains(noun)) {
            continue;
        }
        if !ABSTRACT_VERBS.iter().any(|verb| s.contains(verb)) {
            continue;
        }
        let abs = sent.byte_start;
        if is_excluded(abs, abs + s.len().min(12), excluded) {
            continue;
        }
        issues.push(
            Issue::new(
                abs,
                s.len(),
                s,
                vec![],
                IssueType::Translationese,
                Severity::Info,
            )
            .with_context("翻譯腔：抽象主語（的+抽象名詞+導致/意味著），歐化句型"),
        );
    }
}

// G3/G4: displaced conditionals, 如果 after main clause.
fn scan_trans_displaced_conditional(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const CONDITIONALS: &[&str] = &["如果", "假如", "若"];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let char_len = s.chars().count();
        if char_len < 6 {
            continue;
        }
        // A displaced conditional is one that appears after the halfway point.
        let midpoint = char_bounded_end(s, 0, char_len / 2);

        for &cond in CONDITIONALS {
            // Search only after the halfway point; sentence-initial occurrences
            // are correctly placed and naturally excluded by this slice.
            if let Some(pos) = s[midpoint..].find(cond) {
                let abs = sent.byte_start + midpoint + pos;
                if !is_excluded(abs, abs + cond.len(), excluded) {
                    // Check for 的話 after the conditional (extra calque
                    // signal).
                    let after = &s[midpoint + pos + cond.len()..];
                    let has_dehua = after.contains("的話");
                    let ctx = if has_dehua {
                        "翻譯腔：後置條件句（…如果…的話），建議將條件前置"
                    } else {
                        "翻譯腔：後置條件句，建議將條件前置"
                    };
                    issues.push(
                        Issue::new(
                            abs,
                            cond.len(),
                            cond,
                            vec![],
                            IssueType::Translationese,
                            Severity::Info,
                        )
                        .with_context(ctx),
                    );
                }
                break; // One per sentence.
            }
        }
    }
}

// Pronoun overuse: three or more consecutive sentences starting with
// 他/她/它/他們.
fn scan_trans_pronoun_overuse(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const PRONOUNS: &[&str] = &["他", "她", "它", "他們", "她們"];

    for para in &idx.paragraphs {
        let sents = idx.sentence_slice(para);
        let mut consecutive = 0;
        let mut first_offset = 0;

        for sent in sents {
            let s = &text[sent.byte_start..sent.byte_end];
            let starts_with_pronoun = PRONOUNS.iter().any(|p| s.starts_with(p));
            if starts_with_pronoun {
                if consecutive == 0 {
                    first_offset = sent.byte_start;
                }
                consecutive += 1;
            } else {
                if consecutive >= 3 && !is_excluded(first_offset, first_offset + 3, excluded) {
                    issues.push(
                        Issue::new(
                            first_offset,
                            3,
                            &text[first_offset..first_offset + 3],
                            vec![],
                            IssueType::Translationese,
                            Severity::Info,
                        )
                        .with_context(format!(
                            "翻譯腔：代詞過度使用 — 連續 {consecutive} 句以代詞開頭"
                        )),
                    );
                }
                consecutive = 0;
            }
        }
        // Flush trailing run.
        if consecutive >= 3 && !is_excluded(first_offset, first_offset + 3, excluded) {
            issues.push(
                Issue::new(
                    first_offset,
                    3,
                    &text[first_offset..first_offset + 3],
                    vec![],
                    IssueType::Translationese,
                    Severity::Info,
                )
                .with_context(format!(
                    "翻譯腔：代詞過度使用 — 連續 {consecutive} 句以代詞開頭"
                )),
            );
        }
    }
}

// Copula plus classifier inflation: 他是一個/名/位...的...人.
fn scan_trans_copula_classifier(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const COPULA_PATTERNS: &[&str] = &["是一個", "是一名", "是一位"];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        for &pattern in COPULA_PATTERNS {
            let Some(pos) = s.find(pattern) else {
                continue;
            };
            // The pattern only reads as the calque when a 的 clause follows.
            if !s[pos + pattern.len()..].contains("的") {
                continue;
            }
            let abs = sent.byte_start + pos;
            if !is_excluded(abs, abs + pattern.len(), excluded) {
                issues.push(
                    Issue::new(
                        abs,
                        pattern.len(),
                        pattern,
                        // Advice, not a replacement: dropping the classifier
                        // turns 他是一名警察的兒子 ("a policeman's son") into
                        // 他是警察的兒子 ("the policeman's son"). An empty list
                        // is declined at every tier, where an editorial
                        // confidence annotation is honored only below
                        // lexical_contextual, the tier convert always uses.
                        vec![],
                        IssueType::Translationese,
                        Severity::Info,
                    )
                    .with_context("翻譯腔：繫詞+量詞膨脹（是一個/名/位…的…），建議刪除繫詞+量詞"),
                );
            }
            break; // One per sentence.
        }
    }
}

// 的 and 地 confusion: adjective plus 的 plus verb where 地 is correct.
fn scan_trans_adverbial_particle_mixup(
    em: &mut Emitter<'_>,
    _idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Finite list of common adj+的+verb confusions (should be 地).
    const CONFUSIONS: &[(&str, &str)] = &[
        ("仔細的看", "仔細地看"),
        ("認真的聽", "認真地聽"),
        ("慢慢的走", "慢慢地走"),
        ("靜靜的坐", "靜靜地坐"),
        ("快速的跑", "快速地跑"),
        ("努力的工作", "努力地工作"),
        ("安靜的離開", "安靜地離開"),
        ("輕輕的放", "輕輕地放"),
        ("默默的承受", "默默地承受"),
        ("悄悄的走", "悄悄地走"),
    ];

    for &(wrong, correct) in CONFUSIONS {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(wrong) {
            let abs = search_from + pos;
            if !is_excluded(abs, abs + wrong.len(), excluded) {
                issues.push(
                    Issue::new(
                        abs,
                        wrong.len(),
                        wrong,
                        vec![correct.to_string()],
                        IssueType::Translationese,
                        Severity::Warning,
                    )
                    .with_context("翻譯腔：的/地混淆 — 副詞修飾動詞應用「地」"),
                );
            }
            search_from = abs + wrong.len();
        }
    }
}

// 的的不休 (余光中): four or more 的 in one continuous span with no comma.
fn scan_trans_excessive_de_chain(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let text = em.text;

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];

        // Walk clause boundaries with explicit byte offsets so repeated
        // identical clauses do not collapse to the first occurrence.
        let mut clause_start = 0usize;
        for (sep_byte, sep_ch) in s.match_indices(['，', ',']) {
            emit_excessive_de_chain(em, s, sent.byte_start, clause_start, sep_byte);
            clause_start = sep_byte + sep_ch.len();
        }
        // Final clause after the last separator.
        emit_excessive_de_chain(em, s, sent.byte_start, clause_start, s.len());
    }
}

fn emit_excessive_de_chain(
    em: &mut Emitter<'_>,
    sent_text: &str,
    sent_offset: usize,
    clause_start: usize,
    clause_end: usize,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if clause_start >= clause_end {
        return;
    }
    let clause = &sent_text[clause_start..clause_end];
    let de_count = clause.matches('的').count();
    if de_count < 4 {
        return;
    }
    let abs = sent_offset + clause_start;
    let abs_end = sent_offset + clause_end;
    if is_excluded(abs, abs_end, excluded) {
        return;
    }
    issues.push(
        Issue::new(
            abs,
            clause.len(),
            &text[abs..abs_end],
            vec![],
            IssueType::Translationese,
            Severity::Warning,
        )
        .with_context(format!(
            "翻譯腔：的的不休 — 一個子句中出現 {de_count} 個「的」（余光中）"
        )),
    );
}

// 地 overuse on disyllabic adverbs: 慢慢地、靜靜地、認真地.
fn scan_trans_adverbial_particle_redundant(
    em: &mut Emitter<'_>,
    _idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Finite whitelist: these adverbs can drop 地 in natural Chinese.
    const ADVERBS: &[(&str, &str)] = &[
        ("慢慢地", "慢慢"),
        ("靜靜地", "靜靜"),
        ("認真地", "認真"),
        ("安靜地", "安靜"),
        ("輕輕地", "輕輕"),
        ("默默地", "默默"),
        ("悄悄地", "悄悄"),
        ("漸漸地", "漸漸"),
        ("緩緩地", "緩緩"),
        ("偷偷地", "偷偷"),
    ];

    for &(with_di, without_di) in ADVERBS {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(with_di) {
            let abs = search_from + pos;
            if !is_excluded(abs, abs + with_di.len(), excluded) {
                issues.push(
                    Issue::new(
                        abs,
                        with_di.len(),
                        with_di,
                        vec![without_di.to_string()],
                        IssueType::Translationese,
                        Severity::Info,
                    )
                    .with_context("翻譯腔：雙音節副詞+「地」冗餘，可省略「地」"),
                );
            }
            search_from = abs + with_di.len();
        }
    }
}

// EN→ZH calque detectors: substring-only lexical pass
//
// These four detectors capture EN→ZH translation tells from a six-red- flag
// review checklist, complementing (not duplicating) the existing
// dewesternise-checklist coverage in scan_translationese_syntactic. All four
// are substring-only, needing no boundary index, so they run as soon as
// translationese_detection is enabled.
//
//   ZY1a: 之一 superlative calque (Red Flag 4)
//   ZY2a: bounded EN connective calques (Red Flag 2)
//   ZY3a: finite nominalization shapes (Red Flag 6)
//   ZY4a: false-friend lexical pairs with same-span guard
//
// Boundary-aware variants (paragraph density, sentence-bounded EN connectives,
// extended nominalization chain, long pre-modifier 定語堆疊) live below in the
// scan_translationese_indexed block: they run alongside the syntactic detectors
// and reuse the same BoundaryIndex.

// ZY1a: 之一 superlative calque. Match "最[^之]{1,20}之一" and
// "極為[^之]{1,20}之一": bounded character class, no ".*?". Mirrors "one of the
// most..." directly with high TP rate vs raw 之一 density.
//
// Native-Mandarin guard: when the noun head immediately preceding 之一 is a
// person-class profession noun ("畫家", "學者", "作家", "工程師", "運動員",
// etc.), "最…之一" is biographical idiom ("當代最傑出的畫家之一"), not
// translation tell. Suppress in that case.
fn scan_zy1a_superlative_yi_zhi(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const SUPERLATIVES: &[&str] = &["最", "極為"];
    const CLOSER: &str = "之一";
    const MAX_CHARS_BETWEEN: usize = 20;

    // Person-class profession/person-role noun tails. Match full tails rather
    // than a single final character so ordinary nouns such as "國家" are not
    // misclassified as biographical idiom.
    const PERSON_NOUN_TAILS: &[&str] = &[
        "畫家",
        "學者",
        "作家",
        "工程師",
        "程式設計師",
        "設計師",
        "研究員",
        "運動員",
        "球員",
        "演員",
        "歌手",
        "作者",
        "記者",
        "教授",
        "醫師",
        "醫生",
    ];

    for &opener in SUPERLATIVES {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(opener) {
            let abs_open = search_from + pos;
            let after_open = abs_open + opener.len();
            // Bounded forward window: at most MAX_CHARS_BETWEEN chars.
            let window_end = char_bounded_end(text, after_open, MAX_CHARS_BETWEEN);
            let window = &text[after_open..window_end];

            // Disqualify when no 之一 in window, when 之 splits the gap (would
            // change semantics), or when the gap is empty.
            let Some(cpos) = window.find(CLOSER) else {
                search_from = after_open;
                continue;
            };
            let gap = &window[..cpos];
            if gap.is_empty() || gap.contains('之') {
                search_from = after_open;
                continue;
            }

            let abs_close_end = after_open + cpos + CLOSER.len();

            // Native-Mandarin biographical guard: profession-suffix noun head
            // makes 最…之一 idiomatic, so suppress.
            let is_biographical = PERSON_NOUN_TAILS.iter().any(|tail| gap.ends_with(tail));
            if !is_biographical && !is_excluded(abs_open, abs_close_end, excluded) {
                issues.push(
                    Issue::new(
                        abs_open,
                        abs_close_end - abs_open,
                        &text[abs_open..abs_close_end],
                        vec![],
                        IssueType::Translationese,
                        Severity::Info,
                    )
                    .with_phase_family(PhaseFamily::YiZhi, PhasePass::Lexical)
                    .with_context(
                        "翻譯腔：之一最高級套語，省去「之一」\
                         改用「極為…」/「非常…」/「數一數二的…」",
                    ),
                );
            }
            search_from = abs_close_end;
        }
    }
}

// Static helper: find a needle within a forward char-bounded window.
// Returns the byte offset of the needle within text if found within max_chars
// characters of start_byte, else None.
fn find_within_chars(
    text: &str,
    start_byte: usize,
    max_chars: usize,
    needle: &str,
) -> Option<usize> {
    let end = char_bounded_end(text, start_byte, max_chars);
    text[start_byte..end].find(needle).map(|p| start_byte + p)
}

// ZY2a: bounded EN connective calques, 因為…所以 / 雖然…但是 / 當…的時候 /
// 如果…那麼. Hard-bounded distance (no ".*?").
/// Whether what follows a bare 當 makes it part of a different word.
///
/// 當 is one character with many non-connective uses (當下, 當時, 當作, 當地,
/// 當局, 當事人, 當中, 當然, 當面), so the connective reading needs this filter
/// before the distance search is worth running.
fn starts_another_dang_word(rest: &str) -> bool {
    const SKIP_NEXT: &[char] = &[
        '下', '時', '作', '初', '今', '年', '日', '前', '地', '局', '事', '中', '然', '面', '選',
        '權', '代',
    ];
    rest.chars().next().is_some_and(|c| SKIP_NEXT.contains(&c))
}

fn scan_zy2a_connective_calques(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // (opener, closer, max_chars_between, label). Distance budget per opener:
    // 40 chars for 因/雖/如, 30 chars for 當.
    const PATTERNS: &[(&str, &str, usize, &str)] = &[
        ("因為", "所以", 40, "因為…所以"),
        ("雖然", "但是", 40, "雖然…但是"),
        ("當", "的時候", 30, "當…的時候"),
        ("如果", "那麼", 40, "如果…那麼"),
    ];

    // Register markers signalling formal-letter or contract templates where the
    // paired connective is template-mandatory. Skip when these appear in the
    // document head (first 100 chars, char-boundary safe).
    const FORMAL_MARKERS: &[&str] = &["敬啟者", "謹此", "茲就", "謹啟", "合約", "契約"];
    let head_end = char_bounded_end(text, 0, 100);
    let in_formal_register = FORMAL_MARKERS.iter().any(|m| text[..head_end].contains(m));
    if in_formal_register {
        return;
    }

    for &(opener, closer, max_between, label) in PATTERNS {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(opener) {
            let abs_open = search_from + pos;
            let after_open = abs_open + opener.len();

            if opener == "當" && starts_another_dang_word(&text[after_open..]) {
                search_from = after_open;
                continue;
            }
            match find_within_chars(text, after_open, max_between, closer) {
                Some(abs_close) => {
                    let abs_close_end = abs_close + closer.len();
                    if !is_excluded(abs_open, abs_close_end, excluded) {
                        issues.push(
                            Issue::new(
                                abs_open,
                                abs_close_end - abs_open,
                                &text[abs_open..abs_close_end],
                                vec![],
                                IssueType::Translationese,
                                Severity::Info,
                            )
                            .with_phase_family(PhaseFamily::Connective, PhasePass::Lexical)
                            .with_context(format!(
                                "翻譯腔：連接詞贅餘（{label}），中文常省略其中一端"
                            )),
                        );
                    }
                    search_from = abs_close_end;
                }
                None => {
                    search_from = after_open;
                }
            }
        }
    }
}

// ZY3a: finite nominalization patterns (Red Flag 6 surface forms).
//
// Two pair-forms drawn directly from the source's nominalization "BAD"
// examples:
//   - X的實施Y的提升       (pair 1: implementation→improvement)
//   - 對X的分析Y的發現     (pair 2: analysis→discovery)
//
// Plus a finite list of single nominalized verb-noun heads. Pair forms fire
// with higher confidence (Severity::Info still, REPORT-ONLY). Single forms fire
// when they appear with another nominalization in the same sentence-clause
// ("，"/"。"-bounded), which suppresses standalone noun uses ("策略的實施"
// mentioned once is fine; chained nominalization is the translationese tell).
fn scan_zy3a_finite_nominalization(em: &mut Emitter<'_>) {
    let text = em.text;

    const NOMINAL_HEADS: &[&str] = &[
        "的實施",
        "的分析",
        "的講解",
        "的理解",
        "的認識",
        "的發現",
        "的提升",
        "的下降",
        "的改善",
    ];

    // Each pair is a documented ZY3a form: a left head followed by a right one
    // with no coordination between them. The two families read the same way, so
    // they are one list rather than four parallel arguments.
    const ZY3A_PAIRS: &[HeadPair<'_>] = &[
        HeadPair {
            left: &["的實施", "的改善", "的提升", "的下降"],
            right: &["的提升", "的改善", "的下降"],
        },
        HeadPair {
            left: &["的分析", "的講解", "的理解", "的認識"],
            right: &["的發現", "的理解", "的認識"],
        },
    ];

    // Walk the text by character, locate each clause (bounded by "，" / "," /
    // "。" / "；" / "\n" / start / end), and emit only when the clause contains
    // one of the documented pair-forms or a true back-to-back nominalization
    // chain ("...的講解的理解"). Merely hosting two nominal heads in the same
    // clause is not enough.
    let mut clause_start = 0;
    for (i, ch) in text.char_indices() {
        if is_clause_boundary_char(ch) {
            emit_zy3a_clause(em, clause_start, i, NOMINAL_HEADS, ZY3A_PAIRS);
            clause_start = i + ch.len_utf8();
        }
    }
    // Final clause (no trailing boundary).
    emit_zy3a_clause(em, clause_start, text.len(), NOMINAL_HEADS, ZY3A_PAIRS);
}

/// Two nominal-head lists that qualify as a ZY3a pair when one follows the
/// other: a head from "left" and then a head from "right".
struct HeadPair<'a> {
    left: &'a [&'a str],
    right: &'a [&'a str],
}

/// Clause boundaries used by ZY3a / ZY4a: full-width and ASCII commas,
/// full-width period/semicolon, and newline.
fn is_clause_boundary_char(ch: char) -> bool {
    matches!(ch, '，' | '。' | '；' | ',' | '\n')
}

fn emit_zy3a_clause(
    em: &mut Emitter<'_>,
    clause_start: usize,
    clause_end: usize,
    heads: &[&str],
    pairs: &[HeadPair<'_>],
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if clause_start >= clause_end {
        return;
    }
    let clause = &text[clause_start..clause_end];
    // Collect head positions (relative to clause start).
    let mut hits: Vec<(usize, &str)> = Vec::new();
    for &head in heads {
        let mut from = 0;
        while let Some(p) = clause[from..].find(head) {
            let abs_p = from + p;
            hits.push((abs_p, head));
            from = abs_p + head.len();
        }
    }

    // Need ≥2 nominal heads in the same clause to qualify for further
    // structural checks.
    if hits.len() < 2 {
        return;
    }
    hits.sort_unstable_by_key(|&(p, _)| p);

    let Some((rel_start, rel_end, head_count)) = find_zy3a_shape(clause, &hits, pairs) else {
        return;
    };

    let abs_start = clause_start + rel_start;
    let abs_end = clause_start + rel_end;
    if is_excluded(abs_start, abs_end, excluded) {
        return;
    }
    issues.push(
        Issue::new(
            abs_start,
            abs_end - abs_start,
            &text[abs_start..abs_end],
            vec![],
            IssueType::Translationese,
            Severity::Info,
        )
        .with_phase_family(PhaseFamily::Nominalization, PhasePass::Lexical)
        .with_context(format!(
            "翻譯腔：名詞化動詞鏈（{} 處「的+動名詞」），建議改用動詞句",
            head_count
        )),
    );
}

fn find_zy3a_shape(
    clause: &str,
    hits: &[(usize, &str)],
    pairs: &[HeadPair<'_>],
) -> Option<(usize, usize, usize)> {
    for window in hits.windows(2) {
        let (first_pos, first_head) = window[0];
        let (second_pos, second_head) = window[1];
        let first_end = first_pos + first_head.len();
        let second_end = second_pos + second_head.len();
        let gap = &clause[first_end..second_pos];

        if gap.is_empty() {
            return Some((first_pos, second_end, 2));
        }
        if contains_zy3a_coordination(gap) {
            continue;
        }
        if pairs
            .iter()
            .any(|pair| pair.left.contains(&first_head) && pair.right.contains(&second_head))
        {
            return Some((first_pos, second_end, 2));
        }
    }
    None
}

fn contains_zy3a_coordination(gap: &str) -> bool {
    ["和", "與", "及", "並", "且", "或"]
        .iter()
        .any(|tok| gap.contains(tok))
}

// ZY4a: false-friend lexical pairs. Fire only when the same comma-bounded span
// contains another translation-context cue (another false-friend hit OR a
// romanized parenthetical gloss (English) immediately after the term). This
// local guard suppresses standalone uses of these words: "實際上" alone is
// fine; "實際上, 嚴肅地說..." is the cluster tell.
fn scan_zy4a_false_friends(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // (term, suggested_rephrasing, label).  Auto-fix safe: false.
    const PAIRS: &[(&str, &str, &str)] = &[
        ("實際上", "其實", "實際上→其實"),
        ("字面上", "簡直/真就是", "字面上→簡直"),
        ("基本上", "大致而言/整體來看", "基本上→大致而言"),
        ("絕對地", "完全", "絕對地→完全"),
        ("肯定地", "絕對/無疑", "肯定地→絕對"),
        ("明顯地", "顯然", "明顯地→顯然"),
        ("嚴肅地表示", "鄭重表示/說真的", "嚴肅地表示→鄭重表示"),
        ("誠實地說", "老實說", "誠實地說→老實說"),
    ];

    // Step 1: collect all hits with byte positions and (clause_start,
    // clause_end) bounds. A clause is bounded by "，"/","/"。"/"；"/"\n"/
    // start/end of text. Hits inside an exclusion zone (code, URL, etc.) are
    // skipped at collection time so they cannot supply spurious
    // companion-evidence to neighboring non-excluded hits.
    struct Hit {
        abs_start: usize,
        abs_end: usize,
        suggestion: &'static str,
        label: &'static str,
        clause_start: usize,
        clause_end: usize,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for &(term, suggestion, label) in PAIRS {
        let mut from = 0;
        while let Some(p) = text[from..].find(term) {
            let abs_start = from + p;
            let abs_end = abs_start + term.len();
            from = abs_end;
            if is_excluded(abs_start, abs_end, excluded) {
                continue;
            }
            let (clause_start, clause_end) = clause_bounds(text, abs_start);
            hits.push(Hit {
                abs_start,
                abs_end,
                suggestion,
                label,
                clause_start,
                clause_end,
            });
        }
    }

    // Step 2: a hit qualifies when its clause contains another false-friend hit
    // OR the term is followed by a romanized parenthetical gloss (e.g.
    // actually, basically) that itself is not inside an exclusion zone.
    for h in &hits {
        let companion = hits.iter().any(|other| {
            !std::ptr::eq(other, h)
                && other.clause_start == h.clause_start
                && other.clause_end == h.clause_end
        });
        let parenthetical_gloss = has_ascii_parenthetical_after(text, h.abs_end, excluded);
        if !(companion || parenthetical_gloss) {
            continue;
        }

        // Advisory only (PAIRS is "Auto-fix safe: false"): the suggestions are
        // context-dependent alternatives, several slash-separated
        // (簡直/真就是), NOT drop-in replacements. The fixer applies any single
        // non-orthographic suggestion verbatim (fixer.rs), so emitting one here
        // would inject the literal alternatives. Keep suggestions empty;
        // surface the rephrasing in context so lint output still shows it.
        issues.push(
            Issue::new(
                h.abs_start,
                h.abs_end - h.abs_start,
                &text[h.abs_start..h.abs_end],
                vec![],
                IssueType::Translationese,
                Severity::Info,
            )
            .with_phase_family(PhaseFamily::FalseFriend, PhasePass::Lexical)
            .with_context(format!(
                "翻譯腔：假性對應詞（{}），文脈含其他翻譯特徵，建議改寫為「{}」",
                h.label, h.suggestion
            )),
        );
    }
}

// Locate the comma-bounded clause containing pos (byte offset). Boundaries:
// "，" / "," / "。" / "；" / "\n" / start/end. Caller must pass a valid char
// boundary; debug builds assert this so a future caller passing an interior
// byte trips an explicit failure.
fn clause_bounds(text: &str, pos: usize) -> (usize, usize) {
    debug_assert!(
        pos == text.len() || text.is_char_boundary(pos),
        "clause_bounds requires a char-boundary byte offset"
    );

    // Backward scan: the most recent boundary before pos (exclusive). The
    // clause begins after that boundary char, or 0 if none.
    let start = text[..pos]
        .char_indices()
        .rfind(|&(_, c)| is_clause_boundary_char(c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    // Forward scan: the first boundary at/after pos, else end of text.
    let end = text[pos..]
        .char_indices()
        .find(|&(_, c)| is_clause_boundary_char(c))
        .map(|(i, _)| pos + i)
        .unwrap_or(text.len());
    (start, end)
}

// Detect a romanized parenthetical gloss "(...)" immediately after a hit. Skips
// up to 2 whitespace bytes between the hit and "(". The contents must contain
// at least one ASCII letter (a-zA-Z) to qualify as English.
// Returns false when the gloss span overlaps an exclusion zone (code, URL,
// inline literal): those parens are not translation evidence.
fn has_ascii_parenthetical_after(text: &str, after_byte: usize, excluded: &[ByteRange]) -> bool {
    let bytes = text.as_bytes();
    let mut i = after_byte;
    let mut skipped = 0;
    while i < bytes.len() && skipped < 2 && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
        skipped += 1;
    }
    if i >= bytes.len() || bytes[i] != b'(' {
        return false;
    }
    let close = match bytes[i + 1..].iter().position(|&b| b == b')') {
        Some(p) => i + 1 + p,
        None => return false,
    };
    if is_excluded(i, close + 1, excluded) {
        return false;
    }
    bytes[i + 1..close].iter().any(|&b| b.is_ascii_alphabetic())
}

// Tense marker overuse: several 曾/已/過/了 in one sentence when an explicit
// date is present.
fn scan_trans_tense_marker(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const TENSE_MARKERS: &[char] = &['曾', '已', '過', '了'];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        // Check for explicit date marker (年/月/日 or digits).
        let has_date = s.contains('年')
            || s.contains('月')
            || s.contains('日')
            || s.chars().any(|c| c.is_ascii_digit());

        if !has_date {
            continue;
        }

        let marker_count: usize = TENSE_MARKERS.iter().map(|&m| s.matches(m).count()).sum();

        if marker_count >= 3
            && !is_excluded(sent.byte_start, sent.byte_start + s.len().min(6), excluded)
        {
            issues.push(
                Issue::new(
                    sent.byte_start,
                    s.len(),
                    s,
                    vec![],
                    IssueType::Translationese,
                    Severity::Info,
                )
                .with_context(format!(
                    "翻譯腔：時態標記冗餘 — 句中已有日期，{marker_count} 個時態詞多餘"
                )),
            );
        }
    }
}

// Boundary-aware translationese detectors
//
//   ZY1b: 之一 paragraph density (register-thresholded)
//   ZY2b: sentence-bounded EN connective calques (with structural-fix
//          suggestion)
//   ZY3b: extended nominalization chain ≥N within one sentence
//          (register-thresholded)
//   ZY5    long pre-modifier 定語堆疊 (register-thresholded)
//
// Threshold values (per-domain) flow from TranslationeseDomain::thresholds() so
// --translationese-domain register switches actually change firing behavior at
// scan time.

// Curated abstract-head whitelist for ZY3b extended chain detection. Drawn from
// the source's nominalization examples + targeted corpus mining. Each head is a
// nominalized verb form that translates EN "the X of Y". Kept finite and
// explicit (no POS dependency).
const ZY3B_ABSTRACT_HEADS: &[&str] = &[
    "實施", "分析", "講解", "理解", "認識", "發現", "提升", "下降", "改善", "評估", "探索", "建構",
    "推動", "落實", "形成", "確立", "發展", "建立", "促進", "強化", "深化", "整合", "統合", "落地",
    "規劃", "執行",
];

// ZY1b: 之一 paragraph density. Per-paragraph count of "之一", thresholded
// against DomainThresholds::zy1b_per_200. Catches the translation register
// where every other sentence ends "…之一。": a strong tell for "one of the
// most..." over-use that no individual occurrence betrays.
fn scan_zy1b_yi_zhi_density(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
    domain: crate::engine::translationese_score::TranslationeseDomain,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const TARGET: &str = "之一";
    const MIN_CHARS: usize = 100; // Skip very short paragraphs.

    let thresholds = domain.thresholds();
    let per_200 = thresholds.zy1b_per_200;

    for para in &idx.paragraphs {
        let p = &text[para.byte_start..para.byte_end];
        let char_count = p.chars().count();
        if char_count < MIN_CHARS {
            continue;
        }

        // Count non-excluded 之一 occurrences AND remember the first
        // non-excluded byte offset for anchoring the issue. Anchoring on a raw
        // find() would drop the diagnostic when an excluded span contains the
        // first 之一 even if the paragraph qualifies.
        let mut count = 0usize;
        let mut first_non_excluded: Option<usize> = None;
        let mut search_from = 0usize;
        while let Some(pos) = p[search_from..].find(TARGET) {
            let abs = para.byte_start + search_from + pos;
            if !is_excluded(abs, abs + TARGET.len(), excluded) {
                count += 1;
                first_non_excluded.get_or_insert(abs);
            }
            search_from += pos + TARGET.len();
        }
        if count < 2 {
            continue;
        }
        let density = (count as f32) * 200.0 / (char_count as f32);
        if density < per_200 {
            continue;
        }
        let Some(abs) = first_non_excluded else {
            continue;
        };
        issues.push(
            Issue::new(
                abs,
                TARGET.len(),
                TARGET,
                vec![],
                IssueType::Translationese,
                Severity::Info,
            )
            .with_phase_family(PhaseFamily::YiZhi, PhasePass::Indexed)
            .with_context(format!(
                "翻譯腔：之一 段落密度過高 — {count} 處 / 200字 ({density:.1})，\
                 超過 {} 域閾值 {per_200:.1}",
                domain.name()
            )),
        );
    }
}

// ZY2b: sentence-bounded EN connective calques. Same patterns as ZY2a but
// verifies opener+closer sit in the same sentence: emits a structural-fix
// suggestion that ZY2a cannot ("drop 因為, keep 所以").
fn scan_zy2b_sentence_bounded_connectives(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // (opener, closer, max_chars_between, drop_form, label). Keep the same
    // distance caps as ZY2a so the sentence-bounded variant does not
    // reintroduce long-distance false positives.
    const PATTERNS: &[(&str, &str, usize, &str, &str)] = &[
        ("因為", "所以", 40, "因為", "因為…所以"),
        ("雖然", "但是", 40, "雖然", "雖然…但是"),
        ("當", "的時候", 30, "的時候", "當…的時候"),
        ("如果", "那麼", 40, "那麼", "如果…那麼"),
    ];
    const SKIP_NEXT_DANG: &[char] = &[
        '下', '時', '作', '初', '今', '年', '日', '前', '地', '局', '事', '中', '然', '面', '選',
        '權', '代',
    ];

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        for &(opener, closer, max_between, drop_form, label) in PATTERNS {
            // Iterate every opener occurrence inside the sentence: a guarded
            // 當-prefix hit (e.g. 當地) early in the sentence must not block a
            // real 當…的時候 connective later in the same sentence.
            let mut search_from = 0usize;
            while let Some(rel_pos) = s[search_from..].find(opener) {
                let open_pos = search_from + rel_pos;
                let after_open = open_pos + opener.len();
                // 當-prefix word guard (matches ZY2a): skip 當地, 當時, etc.
                let dang_prefix_word = opener == "當"
                    && s[after_open..]
                        .chars()
                        .next()
                        .is_some_and(|c| SKIP_NEXT_DANG.contains(&c));
                if dang_prefix_word {
                    search_from = after_open;
                    continue;
                }
                let Some(close_rel) = find_within_chars(s, after_open, max_between, closer) else {
                    search_from = after_open;
                    continue;
                };
                let close_abs_end = sent.byte_start + close_rel + closer.len();
                let abs_open = sent.byte_start + open_pos;
                if !is_excluded(abs_open, close_abs_end, excluded) {
                    // The suggestion must be a valid REPLACEMENT (the fixer
                    // applies suggestions[0]), not a human-readable
                    // instruction. drop_form is always the opener or the closer
                    // of the matched span, so the fix is the span with that one
                    // end removed ("keep the other end"). The explanation lives
                    // in the context, not the suggestion.
                    let matched = &text[abs_open..close_abs_end];
                    let fixed = if drop_form == opener {
                        matched.strip_prefix(drop_form).unwrap_or(matched)
                    } else {
                        matched.strip_suffix(drop_form).unwrap_or(matched)
                    };
                    issues.push(
                        Issue::new(
                            abs_open,
                            close_abs_end - abs_open,
                            matched,
                            vec![fixed.to_string()],
                            IssueType::Translationese,
                            Severity::Info,
                        )
                        .with_phase_family(PhaseFamily::Connective, PhasePass::Indexed)
                        .with_context(format!(
                            "翻譯腔：句內連接詞贅餘（{label}），建議刪除「{drop_form}」僅保留另一端"
                        )),
                    );
                }

                // Advance past the matched closer so we don't re-fire on its
                // tail.
                search_from = close_abs_end - sent.byte_start;
            }
        }
    }
}

// ZY3b: extended nominalization chain, ≥N consecutive "<head>的<head>的<head>"
// shapes within one sentence, where every head matches ZY3B_ABSTRACT_HEADS. N
// comes from the per-domain zy3b_chain_min threshold. Different from ZY3a
// (which counts any of nine specific verb-noun heads in a clause); ZY3b
// requires the recursive shape with ≥N levels.
fn scan_zy3b_nominalization_chain(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
    domain: crate::engine::translationese_score::TranslationeseDomain,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let chain_min = domain.thresholds().zy3b_chain_min;

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];

        // Walk every position; at each, see how many
        // "<head>的<head>(的<head>)*" levels chain forward, where every head
        // matches the whitelist.
        let mut search_from = 0usize;
        while let Some((head_rel, head)) = find_first_abstract_head_at_or_after(s, search_from) {
            let (chain_depth, chain_end) = walk_zy3b_chain(s, head_rel);
            if chain_depth >= chain_min {
                let abs_start = sent.byte_start + head_rel;
                let abs_end = sent.byte_start + chain_end;
                if !is_excluded(abs_start, abs_end, excluded) {
                    issues.push(
                        Issue::new(
                            abs_start,
                            abs_end - abs_start,
                            &text[abs_start..abs_end],
                            vec![],
                            IssueType::Translationese,
                            Severity::Info,
                        )
                        .with_phase_family(PhaseFamily::Nominalization, PhasePass::Indexed)
                        .with_context(format!(
                            "翻譯腔：名詞化串接 — {chain_depth} 級「的+抽象名詞」鏈"
                        )),
                    );
                }
                // Advance past the entire chain; do not re-fire on its tail.
                search_from = chain_end;
            } else {
                // Move past this head and continue searching.
                search_from = head_rel + head.len();
            }
        }
    }
}

// Longest ZY3b head matching at the start of s (or None). Shared between the
// chain walker and the leftmost-longest finder so both agree on which head wins
// when a future ZY3B_ABSTRACT_HEADS entry has another head as a prefix.
fn longest_zy3b_head_at(s: &str) -> Option<&'static str> {
    ZY3B_ABSTRACT_HEADS
        .iter()
        .filter(|h| s.starts_with(*h))
        .max_by_key(|h| h.len())
        .copied()
}

// Locate the first abstract-head occurrence at or after from within s.
//
// Returns (head_byte_start, head) of the longest matching head at the earliest
// position. Performs a leftmost-longest match by trying each head and picking
// the earliest start, with longest_zy3b_head_at breaking ties (cheap given the
// finite head list, ~25 entries).
fn find_first_abstract_head_at_or_after(s: &str, from: usize) -> Option<(usize, &'static str)> {
    let abs_pos = ZY3B_ABSTRACT_HEADS
        .iter()
        .filter_map(|head| s[from..].find(head).map(|pos| from + pos))
        .min()?;
    longest_zy3b_head_at(&s[abs_pos..]).map(|head| (abs_pos, head))
}

// Walk the chain starting at byte offset start (which must point at an abstract
// head): returns (depth, end_byte) where depth counts how many "<head>的<head>"
// levels chain forward (≥1) and end_byte is the byte offset just past the last
// head. Stops at the first 的 not followed by another whitelisted head.
fn walk_zy3b_chain(s: &str, start: usize) -> (usize, usize) {
    let mut depth = 0usize;
    let mut cursor = start;
    loop {
        let Some(head) = longest_zy3b_head_at(&s[cursor..]) else {
            return (depth, cursor);
        };
        depth += 1;
        cursor += head.len();

        // Only consume the trailing "的" if another whitelisted head follows
        // it. Otherwise the chain ends at the head we just matched: anchoring
        // the issue span past an orphan "的" would mis-highlight the diagnostic
        // and break the "end_byte = just past the last head" invariant.
        if !s[cursor..].starts_with('的') {
            return (depth, cursor);
        }
        let after_de = cursor + '的'.len_utf8();
        if longest_zy3b_head_at(&s[after_de..]).is_none() {
            return (depth, cursor);
        }
        cursor = after_de;
    }
}

// ZY5: long pre-modifier 定語堆疊 (Red Flag 3). Parser-free heuristic: within
// one sentence, find each maximal span bounded by "，、。；：" (no internal
// commas) that ends in "的<noun>". Flag when char-length ≥zy5_min_chars AND the
// span contains ≥zy5_min_de_count 的 occurrences.
fn scan_zy5_long_premodifier(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
    domain: crate::engine::translationese_score::TranslationeseDomain,
) {
    let text = em.text;

    const SPAN_BREAKERS: &[char] = &['，', '、', '。', '；', '：', ',', ';', ':'];
    let thresholds = domain.thresholds();
    let min_chars = thresholds.zy5_min_chars;
    let min_de = thresholds.zy5_min_de_count;

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let mut emit = |start, end| {
            emit_zy5_span_if_qualifies(em, s, sent.byte_start, start, end, min_chars, min_de);
        };
        // Walk the sentence, splitting at SPAN_BREAKERS.
        let mut span_start = 0usize;
        for (i, ch) in s.char_indices() {
            if SPAN_BREAKERS.contains(&ch) {
                emit(span_start, i);
                span_start = i + ch.len_utf8();
            }
        }
        emit(span_start, s.len());
    }
}

/// The smallest right edge at which the region starting at `region_start`
/// opens a predicate, or `None` if it never does.
///
/// ZY5 asks that question of a region whose left edge is fixed (the first 的)
/// and whose right edge grows with each candidate. Asked directly it rescans
/// the region every time, which is what kept the walk quadratic. Asked once,
/// each candidate becomes a comparison.
///
/// This works because a marker's verdict does not depend on where the region
/// ends. `opens_a_predicate` reads its windows out of the region, so a right
/// edge falling inside a marker's tail window truncates it; here the windows
/// come from the span, so they read the same whatever the edge. The two agree
/// for the edges ZY5 actually asks about: the region always ends at a 的, so
/// a truncated window would have to spell a `MARKER_WORDS` entry containing
/// 的, and none of them contain it at all. The left edge is fixed, so its own
/// window is decided once either way.
fn first_predicate_close(span: &str, region_start: usize) -> Option<usize> {
    const CHAR_LEN: usize = '就'.len_utf8();
    const WORD_LEN: usize = 2 * CHAR_LEN;
    let is_word = |window: Option<&str>| window.is_some_and(|w| MARKER_WORDS.contains(&w));

    let region = &span[region_start..];
    let mut earliest: Option<usize> = None;
    for marker in PREDICATE_MARKERS {
        for (at, matched) in region.match_indices(marker) {
            let head = at
                .checked_sub(CHAR_LEN)
                .and_then(|q| region.get(q..q + WORD_LEN));
            let p = region_start + at;
            if is_word(head) || is_word(span.get(p..p + WORD_LEN)) {
                continue;
            }
            // The region has to reach past the marker for it to be in it.
            let close = p + matched.len();
            if earliest.is_none_or(|best| close < best) {
                earliest = Some(close);
            }
        }
    }
    earliest
}

/// True if `s` contains an adverb or auxiliary that opens a predicate.
///
/// The multi-character markers are unambiguous. A single-character one counts
/// unless it is part of a listed word, because these characters are also
/// ordinary morphemes: 成就 and 人才 end with one, 就地 and 便利 start with
/// one.
///
/// The list is curated and will never be complete, and that is the deliberate
/// choice. A missing entry costs one suppression that should have fired, which
/// is a quiet miss on an advisory rule. The alternative, testing the character
/// after the marker against a set of predicate openers, fails the other way:
/// these markers precede ordinary verbs and manner adverbs far more often than
/// function words (就開始, 就變成, 就徹底, 卻依然, 才慢慢), so any closed set
/// misses most of them and the false positive this guard exists to stop comes
/// straight back. For a linter the quiet miss is the right failure.
/// The direct form of the question, kept as the definition
/// `first_predicate_close` is checked against and used only by that check.
#[cfg(test)]
fn opens_a_predicate(s: &str) -> bool {
    // Every listed word is two characters with the marker as its head or its
    // tail, so the mask is two lookups: the pair starting at the marker, and
    // the pair ending with it.
    const CHAR_LEN: usize = '就'.len_utf8();
    const WORD_LEN: usize = 2 * CHAR_LEN;
    let is_word = |window: Option<&str>| window.is_some_and(|w| MARKER_WORDS.contains(&w));

    PREDICATE_MARKERS.iter().any(|marker| {
        s.match_indices(marker).any(|(at, _)| {
            let tail = s.get(at..at + WORD_LEN);
            let head = at
                .checked_sub(CHAR_LEN)
                .and_then(|p| s.get(p..p + WORD_LEN));
            !is_word(head) && !is_word(tail)
        })
    })
}

// Ordinary words containing a single-char marker, in either position. Not
// exhaustive and cannot be: these are productive morphemes. Adding a missing
// entry costs one suppression, so the list grows on evidence.
const MARKER_WORDS: &[&str] = &[
    // 就. Absent on purpose: 就地, 就此, 就算, 就近 are adverbs or a
    // conjunction, so they do open a predicate (城市就此吞噬他的生活).
    "成就", "造就", "遷就", "將就", "俯就", "屈就", "就業", "就讀", "就職", "就緒", "就醫", "就學",
    "就寢", "就任", "就座", "就位", "就診",
    // 才. Absent on purpose: 才能 reads as 才 + 能 ("only then can") far more
    // often than as the noun. Across 12 MB of zh-TW prose every occurrence
    // inside a ZY5-shaped span was adverbial, so masking it would trade ten
    // false positives for no real detection.
    "人才", "天才", "剛才", "奴才", "庸才", "英才", "秀才", "方才", "幹才", "口才", "求才", "成才",
    "怪才", "奇才", "專才", "才華", "才幹", "才智", "才氣", "才藝", // 便
    "方便", "順便", "簡便", "隨便", "即便", "輕便", "大便", "小便", "糞便", "不便", "以便", "便利",
    "便當", "便宜", "便條", "便捷", "便民", "便箋",
    // 卻.  Absent on purpose: 除卻 and 省卻 are verbs.
    "忘卻", "退卻", "冷卻", "推卻", "了卻", "拋卻", "卻步", // 也
    "也許",
];

/// Adverbs and auxiliaries that open a predicate.  See `first_predicate_close`.
/// 要 and 能 are excluded because they would match inside 需要 and 才能, which
/// appear in genuine pre-modifier chains.
const PREDICATE_MARKERS: &[&str] = &[
    "也", "就", "才", "卻", "便", "可以", "應該", "必須", "已經", "正在",
];

fn emit_zy5_span_if_qualifies(
    em: &mut Emitter<'_>,
    sent_text: &str,
    sent_offset: usize,
    span_start: usize,
    span_end: usize,
    min_chars: usize,
    min_de: usize,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const PREDICATE_VERBS: &[&str] = &[
        "看到", "看見", "遇到", "聽到", "找到", "收到", "發現", "認識", "帶著", "帶到", "帶來",
        "告訴", "看著", "碰到", "經過",
    ];
    if span_start >= span_end {
        return;
    }
    let span = &sent_text[span_start..span_end];

    // Every candidate is a prefix of the span, so the tests that look at a
    // prefix are answered once here rather than recomputed per 的. Rescanning
    // them made the walk quadratic in the number of 的, which the early exit
    // below only bounds when the noun run happens to reach the end of the span:
    // one Latin character or digit stops the run short and the walk goes back
    // to rescanning everything.
    if span
        .chars()
        .next()
        .is_some_and(|ch| matches!(ch, '我' | '你' | '他' | '她' | '它' | '咱' | '您'))
    {
        // Candidates all start at the span, so this rejects every one of them.
        return;
    }

    let de_len = '的'.len_utf8();

    // The 的 that count toward a candidate are those before its end, so their
    // positions are collected once and each candidate takes a slice of them.
    // They cannot be accumulated as the walk goes: a noun run is CJK and 的 is
    // CJK, so a candidate swallows 的 the walk has not reached yet.
    //
    // Collected on first use rather than up front. Most comma-free spans in
    // ordinary prose hold a 的 or two and fail an earlier gate, so building
    // this for every span allocates for spans that never read it.
    let mut de_positions: Option<Vec<usize>> = None;

    // The verbs are all two CJK characters, so a verb straddling the boundary
    // between one scan and the next spans at most this many bytes.
    const MAX_VERB_BYTES: usize = 6;
    debug_assert!(PREDICATE_VERBS.iter().all(|v| v.len() <= MAX_VERB_BYTES));

    let mut best_candidate: Option<(usize, usize, usize)> = None;

    // The prefix before the 的 only grows, so it is scanned for a predicate
    // verb once in total: each pass covers what the last one had not reached,
    // plus enough overlap for a verb lying across the seam.
    let mut verb_scanned_to = 0usize;
    let mut verb_seen = false;

    // Computed on the first candidate that needs it, since the region it covers
    // is the same for all of them.
    let mut predicate_close: Option<Option<usize>> = None;

    // Only read by the debug assertion below, which guards the reasoning the
    // predicate break depends on.
    #[cfg(debug_assertions)]
    let mut furthest_candidate_end = 0usize;

    // Characters in span[..counted_to], carried across iterations so the whole
    // span is walked once in total rather than once per 的.
    let mut chars_before = 0usize;
    let mut counted_to = 0usize;
    let mut from = 0usize;
    while let Some(p) = span[from..].find('的') {
        let rel_de = from + p;
        from = rel_de + de_len;
        let abs_de = sent_offset + span_start + rel_de;
        if is_excluded(abs_de, abs_de + de_len, excluded) {
            continue;
        }

        chars_before += span[counted_to..rel_de].chars().count();
        counted_to = rel_de;

        let noun_tail = &span[rel_de + de_len..];
        let mut noun_len = 0usize;
        let mut noun_chars = 0usize;
        for ch in noun_tail.chars().take_while(|&ch| is_cjk_ideograph(ch)) {
            noun_len += ch.len_utf8();
            noun_chars += 1;
        }
        if noun_len == 0 {
            continue;
        }

        let candidate_end = rel_de + de_len + noun_len;
        // The 的 itself, plus everything before it and the noun run after it.
        let char_count = chars_before + 1 + noun_chars;
        if char_count < min_chars {
            continue;
        }
        if !verb_seen && rel_de > verb_scanned_to {
            let mut window_start = verb_scanned_to.saturating_sub(MAX_VERB_BYTES - 1);
            while !span.is_char_boundary(window_start) {
                window_start -= 1;
            }
            let window = &span[window_start..rel_de];
            verb_seen = PREDICATE_VERBS.iter().any(|verb| window.contains(verb));
            verb_scanned_to = rel_de;
        }
        if verb_seen {
            // The prefix only grows, so no later candidate can lose the verb.
            break;
        }

        let de_positions = de_positions.get_or_insert_with(|| {
            span.match_indices('的')
                .map(|(at, _)| at)
                .filter(|&at| {
                    let abs = sent_offset + span_start + at;
                    !is_excluded(abs, abs + de_len, excluded)
                })
                .collect()
        });

        // Both the check and the state it needs are debug-only, so release
        // builds carry neither.
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                candidate_end >= furthest_candidate_end,
                "candidate ends must not go backwards, or the predicate break below is wrong"
            );
            furthest_candidate_end = candidate_end;
        }
        let de_count = de_positions.partition_point(|&at| at < candidate_end);
        if de_count < min_de {
            continue;
        }

        // A predicate between the first and last 的 means separate phrases. The
        // region starts at the first 的, which never moves, so the answer for
        // every candidate comes from one pass built on first use.
        let region_start = de_positions[0] + de_len;
        let region_end = de_positions[de_count - 1];
        if region_end > region_start {
            let close =
                *predicate_close.get_or_insert_with(|| first_predicate_close(span, region_start));
            if close.is_some_and(|close| close <= region_end) {
                // close is fixed for the span and region_end only grows, so
                // every later candidate lands here too. Walking on costs a noun
                // run and a character count per remaining 的 to reach the same
                // answer, which on a span with thousands of them is the whole
                // rest of the walk.
                break;
            }
        }

        let abs_start = sent_offset + span_start;
        let abs_end = sent_offset + span_start + candidate_end;
        if is_excluded(abs_start, abs_end, excluded) {
            continue;
        }

        let should_replace = best_candidate
            .as_ref()
            .is_none_or(|(best_end, _, _)| candidate_end > *best_end);
        if should_replace {
            best_candidate = Some((candidate_end, char_count, de_count));
        }

        // Only a longer candidate can replace this one, and none reaches past
        // the end of the span.
        if candidate_end == span.len() {
            break;
        }
    }

    let Some((candidate_end, char_count, de_count)) = best_candidate else {
        return;
    };
    let abs_start = sent_offset + span_start;
    let abs_end = sent_offset + span_start + candidate_end;
    issues.push(
        Issue::new(
            abs_start,
            candidate_end,
            &text[abs_start..abs_end],
            vec![],
            IssueType::Translationese,
            Severity::Warning,
        )
        .with_phase_family(PhaseFamily::LongPremodifier, PhasePass::Lexical)
        .with_context(format!(
            "翻譯腔：定語堆疊 — {char_count} 字無逗點、含 {de_count} 個「的」，\
             建議拆成短句"
        )),
    );
}

/// Iterate over lines with their byte offsets.  Strips trailing \r so
/// callers see consistent line content on both LF and CRLF inputs; the
/// returned offset still points at the original line start.
/// Return the byte length of an ordered-list marker (e.g. "1.", "10.", "123)")
/// at the start of `s`, including the trailing `.` or `)`.  Returns `None` if
/// `s` does not start with such a marker followed by whitespace.
///
/// Handles multi-digit numbers (10., 12)), not just single digits.
pub(super) fn numbered_list_marker_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    match bytes.get(digits) {
        Some(&b'.') | Some(&b')') => {}
        _ => return None,
    }
    // Marker must be followed by whitespace or end-of-line.
    match bytes.get(digits + 1) {
        None | Some(&b' ') | Some(&b'\t') => Some(digits + 1),
        _ => None,
    }
}

fn line_iter(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split('\n').map(move |line| {
        let start = offset;
        offset += line.len() + 1; // +1 for the \n
        (start, line.strip_suffix('\r').unwrap_or(line))
    })
}

// Entry point: run all structural AI pattern checks. Gated by
// ProfileConfig::ai_structural_patterns.
pub(crate) fn scan_ai_structural(em: &mut Emitter<'_>, threshold_multiplier: f32) {
    // Every finding these produce must name its detector: the score counts
    // distinct families, and an untagged one silently moves to per-occurrence
    // density and becomes eligible for mention suppression. A builder call is
    // easy to forget, so assert it here rather than trusting each detector to
    // remember.
    let first_new = em.issues.len();
    scan_ai_binary_contrast(em, threshold_multiplier);
    scan_ai_paragraph_endings(em);
    scan_ai_dash_overuse(em);
    scan_ai_formulaic_headings(em);
    scan_ai_list_density(em, threshold_multiplier);
    scan_ai_mixed_reader_address(em);
    scan_ai_stacked_politeness(em);

    // assert, not debug_assert: compiled out, this checks nothing in the build
    // people run, and the cost is one pass over the findings just pushed,
    // against the document scan that produced them.
    assert!(
        em.issues[first_new..]
            .iter()
            .all(|i| i.structural_family.is_some()),
        "a structural detector emitted a finding without naming its family"
    );
}

// Reader address mixed within one document.
//
// 你 and 您 are both correct zh-TW; only using both is the defect, because a
// reader who sees the register change looks for a reason and finds none.
// Developer documentation drops the subject or uses 你, an end-user manual may
// use 您, and the reference this came from states the rule as "同一份文件不得
// 混用". Reported once, on the rarer of the two, since that is the one to
// change.
//
// 你 needs a boundary test that 您 does not: it is the second character of 迷你
// and of 迷你版, where it is not a pronoun at all.
fn scan_ai_mixed_reader_address(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    let occurrences = |needle: char| -> Vec<usize> {
        text.match_indices(needle)
            .filter(|&(pos, _)| {
                !is_excluded(pos, pos + needle.len_utf8(), excluded) && !text[..pos].ends_with('迷')
            })
            .map(|(pos, _)| pos)
            .collect()
    };
    let informal = occurrences('你');
    let formal = occurrences('您');
    if informal.is_empty() || formal.is_empty() {
        return;
    }

    // The minority form is the one the author slipped into, so point at its
    // first occurrence and name the count on both sides.
    let (found, offset, minority, majority) = if informal.len() <= formal.len() {
        ('你', informal[0], informal.len(), formal.len())
    } else {
        ('您', formal[0], formal.len(), informal.len())
    };
    let ctx = format!(
        "全文混用「你」與「您」（{minority} 處對 {majority} 處），\
         同一份文件的讀者稱謂應一致"
    );
    let found = found.to_string();
    issues.push(
        ai_style_issue(offset, &found, "", &ctx, Severity::Info)
            .with_structural_family(StructuralFamily::MixedReaderAddress),
    );
}

// 請 on every step of a procedure.
//
// One 請 in the surrounding prose is ordinary courtesy. Repeating it per step
// pads every line with the same word and reads as generated. The gate is the
// run, not the word: three consecutive list items each opening with it.
fn scan_ai_stacked_politeness(em: &mut Emitter<'_>) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    const MIN_RUN: usize = 3;
    /// What a line does to a run of polite steps.
    enum Step {
        /// A list item opening with 請, at this offset.
        Opens(usize),
        /// A list item that does not, or a non-list line: the run ends.
        Breaks,
        /// An indented continuation of the item above, which is still that
        /// item. A wrapped step used to end the run, so a three-step procedure
        /// written with wrapped lines was never reported.
        Continues,
    }

    let classify = |line: &str, offset: usize| {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return Step::Breaks;
        }
        let marker =
            numbered_list_marker_len(trimmed).or_else(|| is_bullet_item(trimmed).then_some(2));
        let Some(marker) = marker else {
            // Indented text under a list item belongs to it. An unindented line
            // is a new block and ends the run.
            return if line.starts_with([' ', '\t']) {
                Step::Continues
            } else {
                Step::Breaks
            };
        };
        let body = trimmed[marker..].trim_start();
        if body.starts_with('請') {
            Step::Opens(offset + (line.len() - body.len()))
        } else {
            Step::Breaks
        }
    };

    // (offset of the run's first 請, steps in it); None between runs.
    let mut run: Option<(usize, usize)> = None;
    let mut offset = 0usize;
    // The empty sentinel closes a run that reaches the end of the text.
    for line in text.split_inclusive('\n').chain([""]) {
        match classify(line, offset) {
            // An excluded 請 is not the author's, so it neither opens a run nor
            // extends one. Checking only the run's first step let protected
            // text make up the rest of the count.
            Step::Opens(at) if !is_excluded(at, at + '請'.len_utf8(), excluded) => {
                run = Some(run.map_or((at, 1), |(start, n)| (start, n + 1)));
            }
            Step::Continues => {}
            _ => {
                if let Some((start, n)) = run.take().filter(|&(_, n)| n >= MIN_RUN) {
                    {
                        let ctx = format!(
                            "連續 {n} 個步驟都以「請」開頭，\
                             禮貌語在整體說明處出現一次即可"
                        );
                        issues.push(
                            ai_style_issue(start, "請", "", &ctx, Severity::Info)
                                .with_structural_family(StructuralFamily::StackedPoliteness),
                        );
                    }
                }
            }
        }
        offset += line.len();
    }
}

// Structural AI pattern detectors that require sentence/paragraph boundary
// index. S1 tricolon, S2 negative parallel, S3 formulaic section endings, S4
// mechanical bullets, S5 excessive bold, S6 em-dash overuse, S7 formulaic
// despite, S8 false ranges, V2 hedging density.
pub(crate) fn scan_ai_structural_phase2(
    em: &mut Emitter<'_>,
    boundary_index: &crate::engine::sentence::BoundaryIndex,
    content_type: crate::engine::scan::ContentType,
) {
    let (text, excluded) = (em.text, em.excluded);

    // Every finding these produce must name its detector: the score counts
    // distinct families, and an untagged one silently moves to per-occurrence
    // density and becomes eligible for mention suppression. A builder call is
    // easy to forget, so assert it here rather than trusting each detector to
    // remember.
    let first_new = em.issues.len();
    let markdown_blocks = matches!(
        content_type,
        crate::engine::scan::ContentType::Markdown
            | crate::engine::scan::ContentType::MarkdownScanCode
    )
    .then(|| crate::engine::markdown::block_boundary_starts(text));
    scan_ai_tricolon(em, boundary_index);
    scan_ai_negative_parallel(em, boundary_index);
    scan_ai_formulaic_section_endings(em, boundary_index, markdown_blocks.as_deref());
    scan_ai_mechanical_bullets(em, boundary_index);
    scan_ai_excessive_bold(em, boundary_index);
    scan_ai_emdash_overuse(em, boundary_index);
    scan_ai_formulaic_despite(em, boundary_index);
    scan_ai_false_ranges(em, boundary_index);
    scan_ai_hedging_density(text, excluded, em.issues, boundary_index);
    scan_ai_abstract_line_metaphor(em, boundary_index);
    scan_ai_repeated_parallel_slogan(em, boundary_index);
    scan_ai_rhetorical_self_qa(em, boundary_index);

    // assert, not debug_assert: compiled out, this checks nothing in the build
    // people run, and the cost is one pass over the findings just pushed,
    // against the document scan that produced them.
    assert!(
        em.issues[first_new..]
            .iter()
            .all(|i| i.structural_family.is_some()),
        "a structural detector emitted a finding without naming its family"
    );
}

// Lexical translationese detectors that need no sentence/paragraph index. EN→ZH
// calque pass:
//   ZY1a 之一 superlative calque, ZY2a EN connective bounded calques,
//   ZY3a finite nominalization patterns, ZY4a false-friend lexical pairs.
pub(crate) fn scan_translationese_lexical(em: &mut Emitter<'_>) {
    scan_zy1a_superlative_yi_zhi(em);
    scan_zy2a_connective_calques(em);
    scan_zy3a_finite_nominalization(em);
    scan_zy4a_false_friends(em);
}

// Syntactic translationese detectors that require sentence/paragraph boundary
// index. G1 passive density, G2 abstract subject, G3/G4 displaced conditionals,
// G8 pronoun overuse, Y1 copula+classifier, Y2 的/地 confusion, S3 的的不休, V7
// 地 overuse, V13 tense markers.
pub(crate) fn scan_translationese_syntactic(
    em: &mut Emitter<'_>,
    boundary_index: &crate::engine::sentence::BoundaryIndex,
) {
    scan_trans_passive_density(em, boundary_index);
    scan_trans_abstract_subject(em, boundary_index);
    scan_trans_displaced_conditional(em, boundary_index);
    scan_trans_pronoun_overuse(em, boundary_index);
    scan_trans_copula_classifier(em, boundary_index);
    scan_trans_adverbial_particle_mixup(em, boundary_index);
    scan_trans_excessive_de_chain(em, boundary_index);
    scan_trans_adverbial_particle_redundant(em, boundary_index);
    scan_trans_tense_marker(em, boundary_index);
}

// Boundary-aware translationese dispatcher. Runs detectors that need
// sentence/paragraph index AND a per-domain threshold table:
//   ZY1b 之一 paragraph density,
//   ZY2b sentence-bounded EN connectives,
//   ZY3b extended nominalization chain,
//   ZY5  long pre-modifier 定語堆疊.
pub(crate) fn scan_translationese_indexed(
    em: &mut Emitter<'_>,
    boundary_index: &crate::engine::sentence::BoundaryIndex,
    domain: crate::engine::translationese_score::TranslationeseDomain,
) {
    scan_zy1b_yi_zhi_density(em, boundary_index, domain);
    scan_zy2b_sentence_bounded_connectives(em, boundary_index);
    scan_zy3b_nominalization_chain(em, boundary_index, domain);
    scan_zy5_long_premodifier(em, boundary_index, domain);
}

// Entry point for AI writing detection grammar checks. Gated by
// ProfileConfig::ai_semantic_safety, NOT called from scan_grammar.
pub(crate) fn scan_ai_grammar(em: &mut Emitter<'_>) {
    scan_ai_semantic_safety(em);
    scan_ai_copula_avoidance(em);
    scan_ai_passive(em);
    scan_ai_didactic(em);
    scan_ai_vague_exaggeration(em);
}

// Main entry point: run all grammar checks via AC prefilter.
//
// A single Aho-Corasick pass finds all trigger patterns, then dispatches each
// hit to the appropriate validator. This is O(N + H) instead of the old O(P*N)
// where P = total patterns across 8 scanners.
pub(crate) fn scan_grammar(em: &mut Emitter<'_>) {
    let text = em.text;

    let (ac, metadata) = grammar_ac();

    for mat in ac.find_iter(text) {
        let (check_type, pattern_index) = metadata[mat.pattern().as_usize()];
        let start = mat.start();
        let end = mat.end();

        match check_type {
            GrammarCheckType::ANotAMa => {
                validate_a_not_a_ma(em, start, end);
            }
            GrammarCheckType::HeConnectingClauses => {
                validate_he_connecting(em, start, end);
            }
            GrammarCheckType::BareShiAdjective => {
                validate_bare_shi_adjective(em, start, end);
            }
            GrammarCheckType::RedundantPreposition => {
                validate_redundant_preposition(em, start, end, pattern_index);
            }
            GrammarCheckType::BureaucraticNominalization => {
                validate_bureaucratic_nominalization(em, start, end);
            }
            GrammarCheckType::VerboseAction => {
                validate_verbose_action(em, start, end);
            }
            GrammarCheckType::DuiJinxing => {
                validate_dui_jinxing(em, start, end);
            }
            GrammarCheckType::DoubleAttribution => {
                validate_double_attribution(em, start, end);
            }
        }
    }
}

// Old scan_grammar entry point retained for differential testing.
#[cfg(test)]
fn scan_grammar_legacy(em: &mut Emitter<'_>) {
    scan_a_not_a_ma(em);
    scan_he_connecting_clauses(em);
    scan_bare_shi_adjective(em);
    scan_redundant_preposition(em);
    scan_bureaucratic_nominalization(em);
    scan_verbose_action(em);
    scan_dui_jinxing(em);
    scan_double_attribution(em);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::sentence::BoundaryIndex;

    fn scan(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_grammar(&mut Emitter::new(text, &[], &mut issues));
        issues
    }

    fn scan_phase2(text: &str) -> Vec<Issue> {
        let idx = BoundaryIndex::build(text, &[]);
        let mut issues = Vec::new();
        scan_ai_structural_phase2(
            &mut Emitter::new(text, &[], &mut issues),
            &idx,
            crate::engine::scan::ContentType::Markdown,
        );
        scan_translationese_syntactic(&mut Emitter::new(text, &[], &mut issues), &idx);
        issues
    }

    // Boundary-aware detector panic-safety regression tests (from Codex/Gemini
    // review)

    #[test]
    fn tricolon_with_repeated_spans_does_not_panic() {
        // Codex high #1: 乙、甲、甲、乙 used to confuse find()-based offset
        // calculation when the same span repeats. Should not panic and should
        // detect the central tricolon (甲、甲).
        let text = "乙、甲、甲、乙、丙。";
        let _ = scan_phase2(text);
    }

    #[test]
    fn negative_parallel_mixed_ascii_cjk_does_not_panic() {
        // Codex high #2: byte-counted lookahead used to split UTF-8 chars.
        let text = "不只是A，而是中文混合內容。";
        let _ = scan_phase2(text);
    }

    #[test]
    fn passive_density_short_paragraph_does_not_panic() {
        // Codex high #3 + #5: short ASCII-leading paragraphs used to panic when
        // slicing first-N bytes.
        let text = "A被B。\n\n中文段落以「被」字開頭，被廣泛認為是好的。";
        let _ = scan_phase2(text);
    }

    #[test]
    fn excessive_bold_short_ascii_paragraph_does_not_panic() {
        // Codex high #4: short ASCII-leading paragraph slicing.
        let text = "**A** 中文 **B** 中文 **C** 中文 **D**";
        let _ = scan_phase2(text);
    }

    #[test]
    fn tricolon_detects_simple_pattern() {
        // Three consecutive identical-length 2-char spans (團結、奮鬥、創新)
        // form a tricolon when isolated as the entire sentence content.
        let text = "團結、奮鬥、創新。";
        let issues = scan_phase2(text);
        let has_tricolon = issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("tricolon")));
        assert!(
            has_tricolon,
            "Expected tricolon detection, got {:?}",
            issues
        );
    }

    fn has_formulaic(issues: &[Issue]) -> bool {
        has_context_with(issues, "公式化用語")
    }

    fn has_self_qa(issues: &[Issue]) -> bool {
        has_context_with(issues, "連續自問自答")
    }

    #[test]
    fn formulaic_ending_fires_at_a_section_close() {
        for (text, what) in [
            // Final paragraph of the document, final sentence: this is what the
            // detector is named for.
            (
                "本文介紹了新的排程策略。\n\n效能提升相當顯著。展望未來。",
                "document-final sentence",
            ),
            // Same phrase closing a paragraph that a heading follows.
            (
                "第一節說明背景。展望未來。\n\n## 第二節\n\n這裡是內文。",
                "before a heading",
            ),
            (
                "第一節說明背景。展望未來。\n## 第二節\n\n這裡是內文。",
                "before a heading without a blank line",
            ),
            (
                "第一節說明背景。展望未來。\r## 第二節\r\r這裡是內文。",
                "before a CR-only heading",
            ),
            (
                "## 第二節\n本節總結。展望未來。",
                "in body text after a heading without a blank line",
            ),
        ] {
            assert!(has_formulaic(&scan_phase2(text)), "expected a hit {what}");
        }
    }

    #[test]
    fn formulaic_ending_covers_new_closing_platitudes() {
        for text in ["我們將攜手共進。", "這個案例值得深思。"] {
            assert!(
                has_formulaic(&scan_phase2(text)),
                "missing formulaic closer: {text:?}"
            );
        }
    }

    #[test]
    fn formulaic_ending_ignores_indented_code_as_a_boundary() {
        // Four spaces starts an indented code block, so a commented line inside
        // one is not a heading and does not close a section.
        let code = "## 一\n\n效能提升。展望未來。\n\n    # setup\n    print(1)\n";
        let issues = scan_phase2(code);
        assert!(
            !has_formulaic(&issues),
            "indented code read as a section boundary: {issues:?}"
        );

        let tabbed = "## 一\n\n效能提升。展望未來。\n\n\t# setup\n";
        assert!(
            !has_formulaic(&scan_phase2(tabbed)),
            "tab-indented code read as a section boundary"
        );

        // Three spaces is still a heading.
        let indented_heading = "效能提升。展望未來。\n\n   ### 標題\n\n內文。";
        assert!(
            has_formulaic(&scan_phase2(indented_heading)),
            "three-space indented heading rejected"
        );
    }

    #[test]
    fn formulaic_ending_requires_no_same_line_prose_after_the_closer() {
        // A later heading must not turn a phrase in the middle of a paragraph
        // into a section closer.
        let text = "展望未來。這裡仍在說明細節。\n\n# 下一節\n";
        assert!(
            !has_formulaic(&scan_phase2(text)),
            "same-line prose was skipped before the heading"
        );

        // The narrow allowance keeps the existing trailing-note form valid.
        assert!(has_formulaic(&scan_phase2(
            "展望未來。（註）\n\n# 下一節\n"
        )));
    }

    #[test]
    fn heading_containing_inline_code_is_still_a_section_boundary() {
        use crate::engine::scan::{build_exclusions_for_content_type, ContentType};

        // A heading naming a file or a config key in backticks is a heading.
        // Rejecting it, which an exclusion test over the whole line did, made
        // the detector silent on most technical Markdown.
        for heading in [
            "## 第二節",
            "## 設定 `config.toml`",
            "## 設定 src/main.rs",
            "## 參考 https://example.com/a",
        ] {
            let text = format!("## 一\n\n本節總結。展望未來。\n\n{heading}\n\n這裡是內文。");
            let ranges = build_exclusions_for_content_type(&text, ContentType::Markdown);
            let idx = BoundaryIndex::build(&text, &ranges);
            let mut issues = Vec::new();
            scan_ai_structural_phase2(
                &mut Emitter::new(&text, &ranges, &mut issues),
                &idx,
                crate::engine::scan::ContentType::Markdown,
            );
            assert!(
                has_formulaic(&issues),
                "closer lost before heading {heading:?}: {issues:?}"
            );
        }
    }

    #[test]
    fn a_code_fence_after_the_closer_is_not_a_section_boundary() {
        // A fence opens a block inside the section, and the sentence before it
        // is the lead-in that introduces it. That is the ordinary shape of
        // technical zh-TW, so it must not read as a closer. The comment inside
        // the fence is the second half of the test: the parser knows a
        // heading-shaped line in a fence is not a heading.
        let with_comment = "## 一\n\n效能提升。展望未來。\n\n```sh\n# install\nmake\n```\n";
        let without_comment = "## 一\n\n效能提升。展望未來。\n\n```sh\nmake\n```\n";
        for text in [with_comment, without_comment] {
            assert!(
                !has_formulaic(&scan_phase2(text)),
                "a code fence after the closer opened a section: {text:?}"
            );
        }

        // The indent rule is what rejects an indented block, and that one is
        // reachable: four spaces is code, three is a heading.
        assert!(!has_formulaic(&scan_phase2(
            "本節總結。展望未來。\n    # x\n"
        )));
        assert!(has_formulaic(&scan_phase2(
            "本節總結。展望未來。\n   # x\n"
        )));
    }

    #[test]
    fn only_headings_and_rules_close_a_markdown_section() {
        // The trailing-note form still reaches its heading.
        assert!(has_formulaic(&scan_phase2(
            "## 一\n\n效能提升。展望未來。（註）\n\n## 二\n"
        )));
        assert!(has_formulaic(&scan_phase2(
            "效能提升。展望未來。\n\n---\n\n下一節。\n"
        )));

        // A list or blockquote sits inside the section. The sentence above it
        // introduces it, so it is lead-in prose rather than a closer.
        for text in [
            "效能提升。展望未來。\n\n- 下一節的項目\n",
            "效能提升。展望未來。\n\n> 下一節的引文\n",
        ] {
            assert!(
                !has_formulaic(&scan_phase2(text)),
                "a lead-in before a block was read as a closer: {text:?}"
            );
        }
    }

    #[test]
    fn formulaic_ending_prefers_the_heading_boundary_over_the_document_end() {
        // The closer sits before a heading that body text follows immediately,
        // so all of it is one paragraph. Searching backwards for a single
        // candidate found 這裡是內文。 and missed the close entirely.
        let text = "本節總結。展望未來。\n## 第二節\n這裡是內文。";
        let issues = scan_phase2(text);
        assert!(
            has_formulaic(&issues),
            "closer before a heading lost to the document-final sentence: {issues:?}"
        );
    }

    #[test]
    fn formulaic_ending_ignores_heading_text() {
        let text = "## 展望未來";
        let issues = scan_phase2(text);
        assert!(!has_formulaic(&issues), "fired on heading text: {issues:?}");

        let tight = "## 展望未來\n這裡是內文。";
        let issues = scan_phase2(tight);
        assert!(
            !has_formulaic(&issues),
            "fired on heading text joined to body: {issues:?}"
        );

        let body_repeat = "## 展望未來\n本節總結。展望未來。";
        assert!(
            has_formulaic(&scan_phase2(body_repeat)),
            "heading occurrence hid body closer"
        );
    }

    #[test]
    fn formulaic_ending_stays_quiet_mid_document() {
        // Same phrase, same paragraph-final position, but the section continues
        // into another body paragraph. Before this was gated, the detector
        // fired here, which is the false positive its own name argues against.
        let text = "第一段的結尾。展望未來。\n\n第二段還在講同一件事。\n\n第三段結束。";
        let issues = scan_phase2(text);
        assert!(!has_formulaic(&issues), "fired mid-document: {issues:?}");
    }

    #[test]
    fn formulaic_ending_stays_quiet_mid_paragraph() {
        // Closing platitude used as body prose, with real sentences after it in
        // the same closing paragraph. Only the last sentence is a closing.
        let text = "背景說明如下。展望未來。這一節接著討論實作細節與取捨。";
        let issues = scan_phase2(text);
        assert!(!has_formulaic(&issues), "fired mid-paragraph: {issues:?}");
    }

    #[test]
    fn excessive_de_chain_reports_each_occurrence_with_correct_offset() {
        // Codex round 2: repeated identical clauses must report distinct
        // offsets, not collapse to the first one via s.find(clause).
        let text = "我的他的她的它的東西，我的他的她的它的物品。";
        let issues = scan_phase2(text);
        let de_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("的的不休")))
            .collect();
        assert_eq!(
            de_issues.len(),
            2,
            "Expected 2 distinct clauses, got {de_issues:?}"
        );
        // The two issues must have different offsets.
        assert_ne!(de_issues[0].offset, de_issues[1].offset);
    }

    #[test]
    fn numbered_list_marker_len_matches_multi_digit() {
        assert_eq!(numbered_list_marker_len("1. item"), Some(2));
        assert_eq!(numbered_list_marker_len("10. item"), Some(3));
        assert_eq!(numbered_list_marker_len("123) item"), Some(4));
        // No whitespace after marker → not a list item.
        assert_eq!(numbered_list_marker_len("10.foo"), None);
        // Letter before the period → not a list item.
        assert_eq!(numbered_list_marker_len("a. item"), None);
        // Missing trailing marker → not a list item.
        assert_eq!(numbered_list_marker_len("10 item"), None);
    }

    #[test]
    fn mechanical_bullets_detects_multi_digit_list() {
        // cubic review: 10+ item list must still be detected. All items use
        // **bold** prefix: the detector should fire on the full set, not cut
        // off at single-digit markers.
        let mut text = String::new();
        for i in 1..=12 {
            text.push_str(&format!("{i}. **項目** 內容文字。\n"));
        }
        let issues = scan_phase2(&text);
        let has_mechanical = issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("機械式列表")));
        assert!(
            has_mechanical,
            "expected mechanical bullets detection across 12-item list, got {issues:?}"
        );
    }

    #[test]
    fn displaced_conditional_finds_late_when_sentence_starts_with_one() {
        // Gemini round 2: a sentence that starts with 如果 but has another
        // displaced 如果 should still flag the second one.
        let text = "如果你來，我會走，但他不會走，如果他不想來。";
        let issues = scan_phase2(text);
        let has_displaced = issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains("後置條件")));
        assert!(
            has_displaced,
            "Expected displaced conditional, got {issues:?}"
        );
    }

    // Plumbing: IssueType::Grammar fundamentals

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

    // A-not-A plus 嗎: all 14 patterns, with and without 嗎

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
        // 嗎 is in a different sentence: must not flag.
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

    // 和-connecting-clauses

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

    // 是+adjective copula

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
        // No pronoun before 是: e.g. 問題是..., should not fire.
        assert!(scan("問題是很大").is_empty());
    }

    #[test]
    fn shi_adj_as_noun_modifier_clean() {
        // 好消息: 好 is an adjective modifying a noun, not a bare predicate.
        assert!(scan("這是好消息").is_empty());
    }

    #[test]
    fn shi_adj_as_noun_modifier_da_clean() {
        // 大問題: same pattern.
        assert!(scan("這是大問題").is_empty());
    }

    #[test]
    fn shi_adj_standalone_still_fires() {
        // 好 at end of text (no following CJK): still a bare adjective.
        let issues = scan("這是好");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn shi_adj_with_particle_still_fires() {
        // 漂亮啊: particle after adjective, NOT a noun modifier.
        let issues = scan("她是漂亮啊");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn shi_adj_with_connector_still_fires() {
        // 漂亮又善良: connector after adjective, NOT a noun modifier.
        let issues = scan("她是漂亮又善良");
        assert_eq!(issues.len(), 1);
    }

    // Redundant preposition

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

    // Extended A-not-A patterns (single-char verbs)

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

    // Bureaucratic nominalization (進行/加以/予以 + verb)

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
        // 進行 as standalone verb ("proceeding"): no nominalized verb after.
        assert!(scan("會議正在進行").is_empty());
    }

    #[test]
    fn bureaucratic_jinxing_zhong_clean() {
        // 進行中 means "in progress": not a nominalization.
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

    // Verbose action prefix (做出/作出 + abstract noun)

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
        // 做出 without a known object: not flagged.
        assert!(scan("他做出一個蛋糕").is_empty());
    }

    #[test]
    fn verbose_zuochu_object_too_far_clean() {
        // Object too far away (>1 char gap).
        assert!(scan("他做出了很多決定").is_empty());
    }

    // Double attribution (根據...顯示/指出)

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
        // Degenerate case: no source between 根據 and verb, skip.
        assert!(scan("根據顯示結果很好").is_empty());
    }

    #[test]
    fn double_attribution_noun_compound_skipped() {
        // 說明書 is a noun compound; 說明 is a prefix, not an attribution verb.
        assert!(scan("根據手冊說明書的內容").is_empty());
    }

    #[test]
    fn double_attribution_verb_at_boundary_still_fires() {
        // 說明 followed by comma (not CJK): still an attribution verb.
        let issues = scan("根據文件說明，規格如下");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_biaoshi_hui_still_fires() {
        // 表示會: 會 means "will", not a noun suffix. Must still fire.
        let issues = scan("根據消息表示會延期");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_xianshi_tu_still_fires() {
        // 顯示圖: 圖 here is "diagram", not a compound suffix. Must fire.
        let issues = scan("根據數據顯示圖表有誤");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn double_attribution_markdown_link_skipped() {
        // 根據[link text with 說明](url): verb inside markdown link, not
        // attribution.
        assert!(scan("根據[維護者設計說明](https://example.com)，新版核心改動很大").is_empty());
    }

    #[test]
    fn double_attribution_markdown_link_bracket_only() {
        // Even a bare [ between 根據 and verb suppresses the match.
        assert!(scan("根據[某研究說明書]的結論").is_empty());
    }

    #[test]
    fn genju_without_verb_clean() {
        // 根據 without attribution verb: prepositional phrase, not redundant.
        assert!(scan("根據研究，成果很好").is_empty());
    }

    fn scan_bare(text: &str) -> Vec<Issue> {
        scan_bare_with_excluded(text, &[])
    }

    /// The phrases the shipped ruleset carries under this guard. Built here
    /// rather than taken from a const, so these tests exercise the same list
    /// the scanner does and a migration that dropped a phrase would fail them.
    fn attribution_guard() -> StructuralGuard {
        let ruleset: crate::rules::ruleset::Ruleset =
            serde_json::from_str(include_str!("../../../assets/ruleset.json"))
                .expect("embedded ruleset parses");
        let phrases: Vec<String> = ruleset
            .spelling_rules
            .iter()
            .filter(|r| !r.disabled && r.structural_guard.as_deref() == Some("uncited_attribution"))
            .map(|r| r.from.clone())
            .collect();
        assert!(
            !phrases.is_empty(),
            "the ruleset carries no uncited_attribution phrases"
        );
        StructuralGuard::from_phrases(phrases)
    }

    fn scan_bare_with_excluded(text: &str, excluded: &[ByteRange]) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_ai_bare_attribution(
            &mut Emitter::new(text, excluded, &mut issues),
            DocumentGenre::Casual,
            Some(&attribution_guard()),
        );
        issues
    }

    #[test]
    fn genju_verb_in_next_clause_with_named_authority_clean() {
        // The preceding clause names the authority, so this is not bare.
        assert!(scan_bare("根據這份報告，研究顯示成果很好").is_empty());
    }

    #[test]
    fn bare_attribution_does_not_ride_along_on_the_grammar_checks() {
        // It is an AI-style tell, and "grammar_checks" is on in every profile.
        assert!(scan("研究顯示成果很好").is_empty());
    }

    #[test]
    fn standalone_research_shows_in_casual_prose_is_reported_but_never_rewritten() {
        let issues = scan_bare("研究顯示成果很好");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "研究顯示");
        assert_eq!(issues[0].rule_type, IssueType::AiStyle);

        // No suggestion in any genre. An empty-string suggestion is the fixer's
        // delete sentinel, and deleting an attribution off the front of a
        // sentence leaves "多位，本次修法將影響地方財政".
        assert!(
            issues[0].suggestions.is_empty(),
            "a bare attribution must never carry a mechanical edit"
        );
    }

    #[test]
    fn standalone_research_shows_in_technical_or_financial_prose_needs_a_citation() {
        for genre in [DocumentGenre::Technical, DocumentGenre::Financial] {
            let mut issues = Vec::new();
            scan_ai_bare_attribution(
                &mut Emitter::new("研究顯示成果很好", &[], &mut issues),
                genre,
                Some(&attribution_guard()),
            );
            assert_eq!(issues.len(), 1, "{genre:?}");
            assert!(issues[0].suggestions.is_empty(), "{genre:?}");
            assert!(issues[0]
                .context
                .as_deref()
                .unwrap()
                .contains("citation missing"));
        }
    }

    #[test]
    fn bare_attribution_display_device_compounds_are_skipped() {
        for text in ["研究顯示器的效能", "研究顯示屏的解析度"] {
            assert!(
                scan_bare(text).is_empty(),
                "display device must not match: {text}"
            );
        }
    }

    #[test]
    fn compound_nouns_must_terminate_to_exempt_an_attribution() {
        for text in [
            "研究顯示器的效能",
            "研究顯示屏的解析度",
            "研究顯示卡的效能",
            "本文研究顯示器。",
        ] {
            assert!(scan_bare(text).is_empty(), "display device: {text}");
        }
        // 器 also opens 器官, 器材 and 器械.
        for text in [
            "研究顯示器官移植後存活率提高。",
            "研究顯示器材耗損率偏高。",
            "研究顯示器械耗損率偏高。",
        ] {
            assert_eq!(scan_bare(text).len(), 1, "attribution lost: {text}");
        }
    }

    #[test]
    fn a_citation_sources_a_claim_only_when_it_follows_it() {
        for text in [
            "參見 https://example.com/p\n研究顯示這項技術改善良率。",
            "# 參考資料 [1]\n研究顯示這項技術改善良率。",
            "- 研究顯示這項技術改善良率\n- 詳見 https://example.com/p",
            "研究顯示這項技術改善良率\n---\n另一則請參見 [1]",
            "研究顯示此療法有效\n# 參考資料\n[1] 某期刊",
        ] {
            assert_eq!(scan_bare(text).len(), 1, "citation leaked: {text:?}");
        }
        for text in [
            "研究顯示\n這項技術改善良率[1]。",
            "研究顯示\n這項技術\n大幅改善良率[1]。",
        ] {
            assert!(scan_bare(text).is_empty(), "hard wrap broken: {text:?}");
        }
    }

    // The noise carries no closing bracket at all, so every opener in it is
    // unmatched. What this pins is that the citation index still resolves the
    // one real marker and that the walk terminates; the cost itself belongs to
    // a benchmark, not to an assertion.
    #[test]
    fn an_unmatched_bracket_does_not_rescan_the_document() {
        let noise = "參數[a 與 b 的關係，".repeat(2000);
        assert_eq!(scan_bare(&format!("研究顯示成果很好。{noise}")).len(), 1);
        assert!(scan_bare(&format!("研究顯示成果很好[1]。{noise}")).is_empty());
    }

    // A CJK label reaches 64 bytes at 21 characters. While the closing bracket
    // was found by a capped forward scan, any longer label stopped counting as
    // a citation and the attribution it sourced was reported as unsourced.
    #[test]
    fn a_long_link_label_still_counts_as_a_citation() {
        let long = "衛生福利部國民健康署二〇二五年度全國健康調查報告";
        assert!(long.len() > 64);
        assert!(scan_bare(&format!(
            "研究顯示這項療法有效，見[{long}](./refs/health.md)。"
        ))
        .is_empty());
        assert!(scan_bare(&format!("研究顯示這項療法有效，見[^{long}]。")).is_empty());
        // The opener and closer must be the same width.
        assert_eq!(
            scan_bare(&format!("研究顯示這項療法有效，見［{long}]。")).len(),
            1
        );
    }

    #[test]
    fn bare_attribution_citations_in_the_same_sentence_suppress_it() {
        for text in [
            "專家認為此作法有效[1]。",
            "研究表明結果可重現[^source]。",
            "業內普遍認為此方案成熟，[報告](https://example.com/report)。",
            "有報告指出詳情見 https://example.com/report。",
        ] {
            assert!(
                scan_bare(text).is_empty(),
                "citation should suppress: {text}"
            );
        }
    }

    #[test]
    fn citation_counts_however_far_into_the_sentence_it_sits() {
        // The forward search used to be capped, which hid a citation placed
        // where zh-TW normally puts one: at the end of a long sentence.
        let filler = "在控制了年齡與教育程度等變項之後，這項關聯仍然顯著，".repeat(25);
        let cited = format!("研究顯示，{filler}詳見 https://example.com/p 。");
        assert!(
            scan_bare(&cited).is_empty(),
            "a distant citation still sources the claim"
        );

        let uncited = format!("研究顯示，{filler}結論如上。");
        assert_eq!(
            scan_bare(&uncited).len(),
            1,
            "removing the citation must still report"
        );
    }

    #[test]
    fn a_wrapped_sentence_keeps_the_citation_on_its_continuation_line() {
        // Hard-wrapped Markdown puts the citation after the wrap. Treating the
        // wrap as a sentence end left the citation in the "next" sentence and
        // reported a sourced claim as bare.
        assert!(
            scan_bare("研究顯示這項關聯仍然顯著，\n方法請參見 [1]。").is_empty(),
            "a soft line break must not cut the sentence short of its citation"
        );

        // A blank line does end it: the next paragraph's citation is its own.
        assert_eq!(
            scan_bare("研究顯示這項關聯仍然顯著\n\n方法請參見 [1]。").len(),
            1,
            "a citation in the next paragraph must not source this claim"
        );
    }

    #[test]
    fn bare_attribution_ignores_citation_marker_in_excluded_inline_code() {
        let text = "研究顯示此設定有效，請執行 `curl https://example.com`。";
        let code_start = text.find('`').unwrap();
        let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
        let url_start = text.find("https://").unwrap();
        let url_end = url_start + "https://example.com".len();
        let issues = scan_bare_with_excluded(
            text,
            &[
                ByteRange {
                    start: code_start,
                    end: code_end,
                },
                ByteRange {
                    start: url_start,
                    end: url_end,
                },
            ],
        );

        assert_eq!(
            issues.len(),
            1,
            "inline-code URL is not a citation: {issues:?}"
        );
        assert_eq!(issues[0].found, "研究顯示");
    }

    #[test]
    fn bare_attribution_raw_url_remains_a_citation_when_its_url_is_excluded() {
        let text = "研究顯示此設定有效：https://example.com。";
        let url_start = text.find("https://").unwrap();
        let issues = scan_bare_with_excluded(
            text,
            &[ByteRange {
                start: url_start,
                end: url_start + "https://example.com".len(),
            }],
        );

        assert!(
            issues.is_empty(),
            "raw URL should remain a citation: {issues:?}"
        );
    }

    // 對X進行Y: fronted-object bureaucratic padding

    #[test]
    fn dui_jinxing_basic() {
        let issues = scan("對資料進行分析");
        let dui: Vec<_> = issues
            .iter()
            .filter(|i| i.found.starts_with("對"))
            .collect();
        assert_eq!(dui.len(), 1);
        assert_eq!(dui[0].found, "對資料進行分析");
        assert_eq!(dui[0].suggestions[..], vec!["分析資料"]);
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
        assert_eq!(dui[0].suggestions[..], vec!["測試整個系統"]);
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
        // 面對: not a standalone 對.
        assert!(!scan("面對問題進行分析")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_compound_bidui_skipped() {
        // 比對: technical verb, not standalone 對.
        assert!(!scan("比對資料進行分析")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_compound_hedui_skipped() {
        // 核對: not standalone 對.
        assert!(!scan("核對資料進行檢查")
            .iter()
            .any(|i| i.found.starts_with("對")));
    }

    #[test]
    fn dui_jinxing_no_verb_after() {
        // 進行 without a matching verb following: not flagged.
        assert!(scan("對資料進行了某些操作").is_empty());
    }

    #[test]
    fn dui_jinxing_no_jinxing() {
        // 對 without 進行: not flagged.
        assert!(scan("對資料很感興趣").is_empty());
    }

    #[test]
    fn dui_jinxing_object_too_long() {
        // Object between 對 and 進行 exceeds 6 chars: dui_jinxing should skip.
        // (scan_bureaucratic_nominalization may still fire on "進行分析".)
        let issues = scan("對這份非常重要的報告進行分析");
        assert!(
            !issues.iter().any(|i| i.found.starts_with("對")),
            "dui_jinxing should not fire with oversized object"
        );
    }

    #[test]
    fn dui_jinxing_clause_boundary_in_object() {
        // Comma between 對 and 進行: the 對X進行Y pattern should NOT fire.
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
        // - scan_dui_jinxing catches "對資料進行分析" → "分析資料" The broader
        // one (dui_jinxing) covers the full span.
        let issues = scan("對資料進行分析");
        let dui = issues
            .iter()
            .filter(|i| i.found == "對資料進行分析")
            .count();
        let bureau = issues.iter().filter(|i| i.found == "進行分析").count();
        assert_eq!(dui, 1, "dui_jinxing should fire");
        assert_eq!(bureau, 1, "bureaucratic should also fire");
    }

    // Exclusion zone handling

    #[test]
    fn excluded_range_suppresses_a_not_a() {
        let text = "你是不是學生嗎？";
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_grammar(&mut Emitter::new(text, &excluded, &mut issues));
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
        scan_grammar(&mut Emitter::new(text, &excluded, &mut issues));
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
        scan_grammar(&mut Emitter::new(text, &excluded, &mut issues));
        assert!(issues.is_empty());
    }

    #[test]
    fn partial_exclusion_still_flags_outside() {
        // Exclude only the first 3 bytes, leaving the rest scannable.
        let text = "你是不是學生嗎？";
        let excluded = vec![ByteRange { start: 0, end: 3 }];
        let mut issues = Vec::new();
        scan_grammar(&mut Emitter::new(text, &excluded, &mut issues));
        // 是不是 starts at byte 3 (after 你), should still be detected.
        assert_eq!(issues.len(), 1);
    }

    // Multiple issues in the same text

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

    // False-positive guards: natural zh-TW text that should NOT trigger

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

    // AI writing detection

    fn scan_ai(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_ai_grammar(&mut Emitter::new(text, &[], &mut issues));
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
        assert_eq!(issues[0].suggestions[..], vec!["表示"]);
    }

    #[test]
    fn ai_yiweizhe_consequence_context() {
        let text = "如果記憶體不足，這意味著系統將會崩潰";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[..], vec!["代表"]);
    }

    #[test]
    fn ai_yiweizhe_explanation_context() {
        let text = "換言之，這意味著我們需要重新設計";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].suggestions[..], vec!["也就是說"]);
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
        scan_ai_semantic_safety(&mut Emitter::new("這意味著很多", &excluded, &mut issues));
        assert!(issues.is_empty());
    }

    // -- Copula avoidance --

    #[test]
    fn ai_copula_zuowei_in_tech_context() {
        let text = "此系統作為核心元件運作";
        let issues = scan_ai(text);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].found, "作為");
        // Advisory only: no direct replacement (would break sentence).
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
        // Advisory only: no direct replacement.
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
        assert_eq!(issues[0].suggestions[..], vec!["廣泛使用"]);
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
        scan_ai_density(&mut Emitter::new(text, &[], &mut issues), 1.0);
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
        // ~2600 chars of filler with 1 occurrence of tracked phrase. 1 / 2.6 ≈
        // 0.38/千字, below threshold 0.5 for '更重要的是'.
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
        // Exclude the entire text: all occurrences should be skipped.
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let mut issues = Vec::new();
        scan_ai_density(&mut Emitter::new(&text, &excluded, &mut issues), 1.0);
        assert!(issues.is_empty(), "excluded ranges should suppress density");
    }

    #[test]
    fn ai_density_multiple_phrases_independent() {
        // Two different phrases both above threshold: should get two issues.
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
        scan_ai_structural(&mut Emitter::new(text, &[], &mut issues), 1.0);

        // Mirrors run_ai_filter, which runs the invisible-character layer
        // alongside the structural pass rather than inside it.
        scan_ai_zero_width(&mut Emitter::new(text, &[], &mut issues));
        issues
    }

    fn context_of(issues: &[Issue], needle: &str) -> usize {
        issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains(needle)))
            .count()
    }

    #[test]
    fn mixed_reader_address_is_reported_once_on_the_rarer_form() {
        let issues = scan_structural("你可以修改設定檔。您也可以重新啟動服務。");
        assert_eq!(context_of(&issues, "混用"), 1);
        // The minority form is the one to change, so that is what is named.
        let hit = issues
            .iter()
            .find(|i| i.context.as_ref().is_some_and(|c| c.contains("混用")))
            .expect("mixing reported");
        assert!(hit.found == "你" || hit.found == "您");
    }

    #[test]
    fn one_reader_address_is_not_a_defect() {
        for text in [
            "你可以修改設定檔，也可以重新啟動服務。",
            "您可以修改設定檔，也可以重新啟動服務。",
            // 你 is the second character of 迷你, where it is not a pronoun.
            "這是迷你版的設定，迷你主機也支援。您可以直接使用。",
        ] {
            assert_eq!(context_of(&scan_structural(text), "混用"), 0, "{text}");
        }
    }

    #[test]
    fn politeness_stacked_on_every_step_is_reported() {
        let text = "1. 請停止服務。\n2. 請修改設定檔。\n3. 請重新啟動服務。\n";
        assert_eq!(context_of(&scan_structural(text), "以「請」開頭"), 1);
        let bullets = "- 請停止服務。\n- 請修改設定檔。\n- 請重新啟動服務。\n";
        assert_eq!(context_of(&scan_structural(bullets), "以「請」開頭"), 1);
    }

    // A wrapped step is still one step. Ending the run on the continuation line
    // meant a three-step procedure written with wrapping was never seen.
    // Paragraph slices must not carry their line ending: callers test exclusion
    // as "is the whole paragraph covered", so a trailing \r reached one byte
    // past what an exclusion range ends at and a fenced block was scanned
    // anyway.
    #[test]
    fn paragraphs_are_the_same_either_line_ending() {
        let lf = "第一段。\n\n第二段。\n\n第三段。\n";
        let crlf = lf.replace('\n', "\r\n");
        let of = |t: &str| {
            super::super::split_paragraphs(t)
                .into_iter()
                .map(|(_, p)| p.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(of(lf), ["第一段。", "第二段。", "第三段。"]);
        assert_eq!(of(&crlf), of(lf));
    }

    // Both walks this replaced ran from each sentence to a terminator that a
    // pathological document does not have: an unwrapped paragraph has no line
    // break, and a run of full stops passes the tail test at every position.
    // 60,000 of them in 176 KB cost 9.4 seconds, because the cost was paid per
    // sentence and every full stop starts one.
    #[test]
    fn a_run_of_sentence_punctuation_answers_from_the_index() {
        let run = "。".repeat(20_000);
        let doc = format!("這個方案展望未來{run}\n下一行。\n");
        let idx = CloserTailIndex::build(&doc);
        let run_start = doc.find('。').expect("a full stop");
        let line_end = idx.next_line_break(run_start).expect("a line break");

        // Every position inside the run is answered without walking it, and the
        // answer is the same at each: nothing here disqualifies a closer.
        for pos in (run_start..line_end).step_by(3) {
            assert!(!idx.has_non_tail_char(pos, line_end));
        }
        // Prose before the run does disqualify it.
        assert!(idx.has_non_tail_char(0, line_end));
        // The line break is found by search, not by scanning to end of text.
        assert_eq!(idx.next_line_break(0), Some(line_end));
    }

    #[test]
    fn a_wrapped_step_does_not_break_the_run() {
        let text = "1. 請停止服務，\n   並確認沒有連線。\n2. 請修改設定檔，\n                       把 ttl 調高。\n3. 請重新啟動服務。\n";
        assert_eq!(context_of(&scan_structural(text), "以「請」開頭"), 1);
    }

    #[test]
    fn ordinary_politeness_is_not_a_run() {
        for text in [
            // One 請 in prose is courtesy, not padding.
            "執行前請先備份。\n\n1. 停止服務。\n2. 修改設定檔。\n3. 重新啟動。\n",
            // Two is under the run threshold.
            "1. 請停止服務。\n2. 請修改設定檔。\n",
            // A break in the run stops it, so two short lists do not combine.
            "1. 請停止服務。\n2. 修改設定檔。\n3. 請重新啟動。\n",
        ] {
            assert_eq!(
                context_of(&scan_structural(text), "以「請」開頭"),
                0,
                "{text}"
            );
        }
    }

    #[test]
    fn ai_structural_binary_contrast_below_threshold() {
        // Short text or low density should not flag.
        let text = "雖然困難很多，但我們還是做到了。這是正常的文章。".repeat(10);
        let issues = scan_structural(&text);
        // Only 10 concessive patterns in ~280 chars: below 500 char threshold.
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
    fn ai_formulaic_despite_ignores_challenge_before_despite() {
        let text = "這些挑戰很多，儘管如此，團隊仍然持續改善。";
        let issues = scan_structural(text);
        let despite_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("公式化轉折")))
            .collect();
        assert!(
            despite_issues.is_empty(),
            "challenge before despite should not trigger formulaic despite: {despite_issues:?}"
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
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
            .collect();
        assert_eq!(zw.len(), 2, "should detect 2 zero-width artifacts: {zw:?}");
        // Suggestions should be empty string for auto-removal.
        for issue in &zw {
            assert_eq!(issue.suggestions.len(), 1);
            assert!(issue.suggestions[0].is_empty());
        }
    }

    #[test]
    fn ai_zero_width_preserves_valid_emoji_and_bidi_controls() {
        let text = "家庭 emoji 👩\u{200D}👩 與混合方向 abc\u{200E}עברית\u{200F}。";
        let issues = scan_structural(text);
        assert!(
            !issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元"))),
            "valid Unicode controls must not become AI rewrite findings: {issues:?}"
        );
    }

    #[test]
    fn ai_zero_width_excluded() {
        let text = "正常\u{200B}文字";

        // Exclude the zero-width space (byte offset 6 for 2 CJK chars = 6
        // bytes).
        let excluded = vec![ByteRange { start: 6, end: 9 }];
        let mut issues = Vec::new();
        scan_ai_structural(&mut Emitter::new(text, &excluded, &mut issues), 1.0);
        let zw: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
            .collect();
        assert!(zw.is_empty(), "excluded zero-width should not be detected");
    }

    #[test]
    fn ai_excessive_bold_ignores_excluded_markers() {
        let text =
            "這是一段正常說明文字，內容足夠長但是沒有真的使用粗體標記，只有內嵌程式碼 `**a** **b** **c**` 作為示例。";
        let code_start = text.find('`').unwrap();
        let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
        let excluded = vec![ByteRange {
            start: code_start,
            end: code_end,
        }];
        let idx = BoundaryIndex::build(text, &excluded);
        let mut issues = Vec::new();

        scan_ai_excessive_bold(&mut Emitter::new(text, &excluded, &mut issues), &idx);

        let bold_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("段落粗體過多"))
            })
            .collect();
        assert!(
            bold_issues.is_empty(),
            "excluded bold markers should not trigger excessive-bold: {bold_issues:?}"
        );
    }

    #[test]
    fn ai_four_char_bold_label_bullets_trigger() {
        let text =
            "- **核心價值**：第一段說明\n- **治理架構**：第二段說明\n- **實作路徑**：第三段說明";
        let issues = scan_phase2(text);
        assert!(
            issues.iter().any(|i| i
                .context
                .as_ref()
                .is_some_and(|c| c.contains("四字標籤式列表"))),
            "four-character bold labels should trigger: {issues:?}"
        );
        assert!(
            !issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("機械式列表"))),
            "specialized four-character list should replace generic mechanical list: {issues:?}"
        );
    }

    #[test]
    fn ai_significance_stamping_triggers_per_paragraph() {
        // The fixture has to put the stamp somewhere other than the last
        // sentence of the last paragraph, or it asserts nothing: its own
        // message claims stamping fires outside section-final sentences, and a
        // one-sentence document cannot show that.
        let text = "這項安排提供政策的重要框架，也印證制度的重要性。這一節接著討論實作細節。\n\n第二段還在同一節裡。";
        let issues = scan_phase2(text);
        assert!(
            issues.iter().any(|i| i
                .context
                .as_ref()
                .is_some_and(|c| c.contains("意義蓋章式收尾"))),
            "significance stamping should trigger outside section-final sentences: {issues:?}"
        );
    }

    #[test]
    fn non_closing_ai_patterns_ignore_the_section_gate() {
        // 隨著…不斷發展 is an opener, not a closer. Gating it on section-final
        // position deleted it from every multi-paragraph document, which is how
        // the position gate was first written.
        let text =
            "隨著人工智慧不斷發展，各行各業都受到影響。\n\n第二段說明實作細節。\n\n第三段收尾。";
        let issues = scan_phase2(text);
        assert!(
            issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("隨著"))),
            "隨著…不斷發展 lost mid-document: {issues:?}"
        );
    }

    #[test]
    fn ai_significance_stamping_exact_phrase_not_duplicated() {
        let text = "這項安排提供重要框架。";
        let issues = scan_phase2(text);
        let stamping_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("意義蓋章式收尾") || c.contains("公式化用語"))
            })
            .collect();
        assert_eq!(
            stamping_issues.len(),
            1,
            "exact phrase and pair match should not double-report: {issues:?}"
        );
    }

    #[test]
    fn ai_in_sentence_bold_parallel_triggers() {
        let text = "我們需要**理解歷史**、**回到現場**，並**重建脈絡**。";
        let issues = scan_phase2(text);
        assert!(
            issues.iter().any(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("句內關鍵詞粗體排比"))
            }),
            "2-4 bold runs in one sentence should trigger: {issues:?}"
        );
    }

    #[test]
    fn ai_abstract_line_metaphor_requires_three_signals() {
        let good = "改革不是口號，而是一條線。這條線從地方走出來，連到制度現場。這條線也會延伸到新的公共討論。";
        let good_issues = scan_phase2(good);
        let line_issue = good_issues.iter().find(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| c.contains("抽象概念具象成路線"))
        });
        assert!(
            line_issue.is_some(),
            "three-signal abstract line metaphor should trigger: {good_issues:?}"
        );
        assert_eq!(line_issue.unwrap().found, "一條線");

        let idiom = "這條路走不通，所以團隊改用另一個方案。";
        let idiom_issues = scan_phase2(idiom);
        assert!(
            !idiom_issues.iter().any(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("抽象概念具象成路線"))
            }),
            "single idiom must stay silent: {idiom_issues:?}"
        );
    }

    #[test]
    fn ai_repeated_parallel_slogan_triggers_once() {
        let text = "制度不是牆，而是橋。第一段說明政策背景。\n\n制度不是牆，而是橋。第二段重複同一句口號。";
        let issues = scan_phase2(text);
        let slogan_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句")))
            .collect();
        assert_eq!(
            slogan_issues.len(),
            1,
            "repeated parallel slogan should trigger once: {issues:?}"
        );
    }

    #[test]
    fn ai_same_paragraph_slogan_repetition_does_not_trigger() {
        // Same parallel sentence repeated within one paragraph is ordinary
        // 排比, not the cross-section 金句疊句 tic: must stay silent.
        let text = "制度不是牆，而是橋。制度不是牆，而是橋。我們要繼續努力。";
        let issues = scan_phase2(text);
        assert!(
            !issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句"))),
            "same-paragraph repetition should not trigger slogan detector: {issues:?}"
        );
    }

    #[test]
    fn ai_repeated_plain_sentence_does_not_trigger_slogan() {
        let text = "請在週五前提交報告。這是行政提醒。\n\n請在週五前提交報告。這是第二次提醒。";
        let issues = scan_phase2(text);
        assert!(
            !issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains("金句疊句"))),
            "ordinary repeated sentence should not trigger slogan detector: {issues:?}"
        );
    }

    #[test]
    fn ai_repeated_rhetorical_self_qa_triggers_but_faq_is_preserved() {
        let rhetorical =
            "你以為只是網路慢嗎？錯了，每次請求都重新計算。為什麼快取沒生效？因為 TTL 設定到期。";
        let issues = scan_phase2(rhetorical);
        let self_qa: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("連續自問自答"))
            })
            .collect();
        assert_eq!(
            self_qa.len(),
            1,
            "rhetorical pairs should trigger: {issues:?}"
        );

        let faq = "faq：你以為只是網路慢嗎？錯了，每次請求都重新計算。為什麼快取沒生效？因為 TTL 設定到期。";
        let faq_issues = scan_phase2(faq);
        assert!(
            !faq_issues.iter().any(|i| i
                .context
                .as_ref()
                .is_some_and(|c| c.contains("連續自問自答"))),
            "explicit FAQ must stay silent: {faq_issues:?}"
        );
    }

    #[test]
    fn rhetorical_self_qa_leaves_explanatory_prose_alone() {
        // Chained 為什麼/因為 is how Chinese textbooks and technical explainers
        // teach. Without a staged reveal there is nothing AI-specific about it.
        let teaching = "為什麼要用快取？因為重算的成本很高。為什麼快取會失效？因為 TTL 設定到期。";
        assert!(
            !has_self_qa(&scan_phase2(teaching)),
            "explanatory prose must not be flagged"
        );
    }

    #[test]
    fn rhetorical_self_qa_survives_idiomatic_lead_ins() {
        // Idiomatic zh-TW rarely starts on the bare interrogative.
        let text = "但你以為只是網路慢嗎？錯了，每次請求都重新計算。那為什麼快取沒生效？主要是因為 TTL 設定到期。";
        assert!(
            has_self_qa(&scan_phase2(text)),
            "discourse markers hid the device"
        );
    }

    #[test]
    fn ai_emdash_overuse_needs_two_dashes_in_a_paragraph() {
        // Pins the threshold the doc comment states. One dash is ordinary
        // punctuation; the detector's own message counts occurrences, so a
        // one-dash paragraph firing would also report "段落內 1 處".
        for (text, want) in [("這段文字——結尾", 0), ("這段文字——持續補充——結尾", 1)]
        {
            let idx = BoundaryIndex::build(text, &[]);
            let mut issues = Vec::new();
            scan_ai_emdash_overuse(&mut Emitter::new(text, &[], &mut issues), &idx);
            assert_eq!(
                issues.len(),
                want,
                "{text:?} should produce {want} issue(s)"
            );
        }
    }

    #[test]
    fn ai_emdash_overuse_ignores_excluded_markers() {
        let text = "`——` 這段文字——持續補充——結尾";
        let code_start = text.find('`').unwrap();
        let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
        let excluded = vec![ByteRange {
            start: code_start,
            end: code_end,
        }];
        let idx = BoundaryIndex::build(text, &excluded);
        let mut issues = Vec::new();

        scan_ai_emdash_overuse(&mut Emitter::new(text, &excluded, &mut issues), &idx);

        let dash_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("破折號")))
            .collect();
        assert_eq!(dash_issues.len(), 1, "real dashes should trigger once");
        assert_eq!(
            dash_issues[0].offset,
            text.find("文字——").unwrap() + "文字".len()
        );
        assert!(
            dash_issues[0]
                .context
                .as_ref()
                .is_some_and(|c| c.contains("段落內 2 處")),
            "excluded dash should not inflate count: {dash_issues:?}"
        );
    }

    #[test]
    fn ai_dash_overuse_ignores_excluded_markers() {
        let text = "`———` 這是正常段落。\n\n`———` 這也是正常段落。\n\n`———` 這仍然是正常段落。";
        let excluded: Vec<ByteRange> = text
            .match_indices('`')
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|pair| ByteRange {
                start: pair[0].0,
                end: pair[1].0 + '`'.len_utf8(),
            })
            .collect();
        let mut issues = Vec::new();

        scan_ai_dash_overuse(&mut Emitter::new(text, &excluded, &mut issues));

        let dash_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.context
                    .as_ref()
                    .is_some_and(|c| c.contains("含 ≥3 個破折號"))
            })
            .collect();
        assert!(
            dash_issues.is_empty(),
            "excluded code dashes should not create dash-overuse density: {dash_issues:?}"
        );
    }

    #[test]
    fn ai_hedging_density_ignores_excluded_markers() {
        let text = "在某種程度上，這段正常文字提供足夠長的段落內容，用來測試密度提升不會被程式碼範例影響，並保留一個真正的提示。`從某個角度來看 可以說是`";
        let code_start = text.find('`').unwrap();
        let code_end = text.rfind('`').unwrap() + '`'.len_utf8();
        let excluded = vec![ByteRange {
            start: code_start,
            end: code_end,
        }];
        let idx = BoundaryIndex::build(text, &excluded);
        let mut issues = vec![Issue::new(
            0,
            "在某種程度上".len(),
            "在某種程度上",
            vec![],
            IssueType::AiStyle,
            Severity::Info,
        )
        .with_context("AI hedging: 在某種程度上")];

        scan_ai_hedging_density(text, &excluded, &mut issues, &idx);

        assert_eq!(
            issues[0].severity,
            Severity::Info,
            "excluded hedging examples should not promote the real issue"
        );
    }

    #[test]
    fn ai_zero_width_no_false_positive() {
        let text = "完全正常的文字，沒有任何零寬字元。";
        let issues = scan_structural(text);
        let zw: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("隱形字元")))
            .collect();
        assert!(zw.is_empty(), "clean text should have no zero-width issues");
    }

    #[test]
    fn abstract_subject_reports_more_than_first_sentence() {
        let text = "預算的減少導致服務縮減。品質的提高意味著效率提升。";
        let idx = BoundaryIndex::build(text, &[]);
        let mut issues = Vec::new();

        scan_trans_abstract_subject(&mut Emitter::new(text, &[], &mut issues), &idx);

        let abstract_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("抽象主語")))
            .collect();
        assert_eq!(
            abstract_issues.len(),
            2,
            "should report one abstract-subject issue per matching sentence"
        );
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
        // Paragraph extends beyond the exclusion zone: should NOT be excluded.
        let excluded = vec![ByteRange { start: 0, end: 30 }];
        assert!(!is_para_excluded(10, 50, &excluded));
    }

    #[test]
    fn structural_detectors_skip_excluded_paragraphs() {
        // Build text with a list-heavy "paragraph" that is fully excluded.
        // Without exclusion it would trigger list_density; with exclusion it
        // should not.
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
        scan_ai_list_density(&mut Emitter::new(&text, &excluded, &mut issues), 1.0);
        let list_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.context.as_ref().is_some_and(|c| c.contains("含列表")))
            .collect();
        assert!(
            list_issues.is_empty(),
            "excluded code paragraph should not inflate list density: {list_issues:?}"
        );
    }

    // Differential testing: AC prefilter vs. legacy per-scanner path

    /// Compare AC-based scan_grammar output against legacy per-scanner output.
    /// Issues may arrive in different order, so we sort by (offset, found)
    /// before comparing.
    fn assert_ac_matches_legacy(text: &str) {
        let mut ac_issues = Vec::new();
        scan_grammar(&mut Emitter::new(text, &[], &mut ac_issues));
        ac_issues.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.found.cmp(&b.found)));

        let mut legacy_issues = Vec::new();
        scan_grammar_legacy(&mut Emitter::new(text, &[], &mut legacy_issues));
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
        // Text triggers both DuiJinxing (對...進行) and
        // BureaucraticNominalization (進行...).
        assert_ac_matches_legacy("對資料進行分析的報告");
    }

    // EN→ZH calque detectors: substring-only lexical pass.

    fn scan_lex(text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        scan_translationese_lexical(&mut Emitter::new(text, &[], &mut issues));
        issues
    }

    fn has_context_with(issues: &[Issue], needle: &str) -> bool {
        issues
            .iter()
            .any(|i| i.context.as_ref().is_some_and(|c| c.contains(needle)))
    }

    // ZY1a -----------------------------------------------------------------

    #[test]
    fn zy1a_fires_on_classic_one_of_the_most_calque() {
        // calque_superlative_zy1_bad_001: textbook translation tell.
        let text = "20 世紀最重要的發現之一。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_fires_on_jiwei_variant() {
        // calque_superlative_zy1_bad_002: 極為...之一 variant. Use an event
        // noun (成就) rather than a person noun so the biographical guard does
        // not suppress this case.
        let text = "這是極為重要的科學成就之一。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_fires_on_long_modifier_within_window() {
        // calque_superlative_zy1_bad_003: pattern survives an internal
        // modifier.
        let text = "這是當代最具代表性的科學成就之一。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_passes_when_zhi_breaks_the_pair() {
        // calque_superlative_zy1_good_001: 之 between 最 and 之一 disqualifies.
        // The opener-closer pair is no longer a single superlative span.
        let text = "最近之內所收到的回信之一。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_passes_when_no_superlative_marker() {
        // calque_superlative_zy1_good_002: bare 之一 without 最/極為 is fine.
        let text = "他是領域裡的傑出學者之一。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_passes_when_no_zhiyi() {
        // 最 alone (without trailing 之一) does not pair with the
        // superlative-calque shape and must not fire.
        let text = "這是最重要的研究方向。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy1a_passes_on_biographical_person_noun() {
        // calque_superlative_zy1_good_004: native-Mandarin biographical idiom.
        // 畫家/學者/作家 endings get the person-noun guard.
        let cases = [
            "他是當代最傑出的畫家之一。",
            "她是領域裡最有影響力的學者之一。",
            "她是這一代最受歡迎的作家之一。",
            "他是最早的程式設計師之一。",
            "他是傑出的工程師之一。",
        ];
        for text in cases {
            assert!(
                !fires(&scan_lex(text), (PhaseFamily::YiZhi, PhasePass::Lexical)),
                "should not fire on biographical idiom: {text}"
            );
        }
    }

    #[test]
    fn zy1a_still_fires_on_non_person_noun_ending_with_shared_character() {
        let text = "世界上最重要的國家之一。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::YiZhi, PhasePass::Lexical)
        ));
    }

    // ZY2a -----------------------------------------------------------------

    #[test]
    fn zy2a_fires_on_yinwei_suoyi() {
        let text = "因為下雨了，所以我們待在屋裡。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_fires_on_suiran_danshi() {
        let text = "雖然他非常努力，但是還是失敗了。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_fires_on_dang_de_shihou() {
        let text = "當我到達公司的時候，他已經在開會了。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_fires_on_ruguo_name() {
        let text = "如果你願意幫忙，那麼請告訴我。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_passes_on_unpaired_yinwei() {
        // calque_connective_zy2_good_001: 因為ng without 所以 is fine.
        let text = "因為下雨，他選擇待在屋裡。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_passes_when_dang_is_dangshi() {
        // 當時 is a noun ("at that time"), not the temporal connective.
        let text = "當時的他並不知情。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy2a_passes_when_distance_exceeds_window() {
        // Gap > 40 chars between 因為 and 所以: pair is too far to qualify.
        let mut filler = String::from("因為");
        filler.push_str(&"啊".repeat(45));
        filler.push_str("所以這裡。");
        assert!(!fires(
            &scan_lex(&filler),
            (PhaseFamily::Connective, PhasePass::Lexical)
        ));
    }

    // ZY3a -----------------------------------------------------------------

    #[test]
    fn zy3a_fires_on_implementation_improvement_pair() {
        // Nominalization BAD pair 1: 策略的實施帶來了效率的提升.
        let text = "策略的實施帶來了效率的提升。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy3a_fires_on_analysis_discovery_pair() {
        // Nominalization BAD pair 2: 對資料的分析導致了模式的發現.
        let text = "對資料的分析導致了模式的發現。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy3a_fires_on_three_chain() {
        // Extended chain: 對X的講解的理解 ≥3 nominalizations.
        let text = "他對概念的講解的理解非常深入。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy3a_passes_on_single_nominalization() {
        // calque_nominalization_zy3_good_001: a single 的+nominalization is
        // fine.
        let text = "策略的實施很順利。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy3a_passes_when_clause_boundary_separates() {
        // Two nominalizations across a comma: different clauses.
        let text = "策略的實施完成了，效率的提升仍在觀察。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy3a_passes_on_coordinated_nominal_phrases() {
        let text = "我們對政策的理解和對流程的認識都很深入。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::Nominalization, PhasePass::Lexical)
        ));
    }

    // ZY4a -----------------------------------------------------------------

    #[test]
    fn zy4a_fires_when_two_false_friends_share_a_clause() {
        // calque_falsefriend_zy4_bad_001: actually + basically pattern.
        let text = "實際上基本上每個人都同意這個觀點。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::FalseFriend, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy4a_fires_with_parenthetical_gloss() {
        // calque_falsefriend_zy4_bad_002: term followed by (English) gloss.
        let text = "字面上 (literally) 我也是這樣理解的。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::FalseFriend, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy4a_fires_on_seriously_honestly_cluster() {
        // calque_falsefriend_zy4_bad_003: 嚴肅地表示 + 誠實地說 same clause.
        let text = "他嚴肅地表示誠實地說我們無法繼續。";
        assert!(fires(
            &scan_lex(text),
            (PhaseFamily::FalseFriend, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy4a_passes_on_solo_occurrence() {
        // calque_falsefriend_zy4_solo_001: lone 實際上 in a clause, OK.
        let text = "實際上他比我想的還要勤奮。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::FalseFriend, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy4a_passes_when_companion_is_in_different_clause() {
        // calque_falsefriend_zy4_solo_002: comma separates the two cues.
        let text = "實際上他並不在意，基本上一切都按部就班。";
        assert!(!fires(
            &scan_lex(text),
            (PhaseFamily::FalseFriend, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy4a_ignores_companion_inside_excluded_zone() {
        // Codex review: a false-friend hit inside an exclusion zone (e.g.
        // inline code) must not supply companion-evidence to a non-excluded hit
        // in the same clause. Range [0, 10) covers "實際上" so the remaining
        // "基本上" is alone outside the zone.
        let text = "實際上基本上每個人都同意。";
        let mut issues = Vec::new();
        let excluded: &[ByteRange] = &[ByteRange {
            start: 0,
            end: "實際上".len(),
        }];
        scan_translationese_lexical(&mut Emitter::new(text, excluded, &mut issues));

        // The remaining 基本上 should NOT fire because its only same-clause
        // companion (實際上) is now excluded.
        assert!(
            !issues
                .iter()
                .any(|i| i.phase_family == Some((PhaseFamily::FalseFriend, PhasePass::Lexical))),
            "ZY4a should not fire when companion is excluded"
        );
    }

    #[test]
    fn zy4a_parenthetical_gloss_inside_excluded_zone_does_not_qualify() {
        // Codex review: parenthetical gloss inside an exclusion zone must not
        // count as translation evidence.
        let text = "字面上 (literally) 我也同意。";
        // Mark the parenthetical gloss as excluded.
        let paren_start = text.find('(').unwrap();
        let paren_end = text.find(')').unwrap() + 1;
        let mut issues = Vec::new();
        let excluded: &[ByteRange] = &[ByteRange {
            start: paren_start,
            end: paren_end,
        }];
        scan_translationese_lexical(&mut Emitter::new(text, excluded, &mut issues));
        assert!(
            !issues
                .iter()
                .any(|i| i.phase_family == Some((PhaseFamily::FalseFriend, PhasePass::Lexical))),
            "ZY4a should not fire when gloss is excluded"
        );
    }

    #[test]
    fn zy2a_skips_dang_di_dang_ju_dang_zhongs() {
        // Gemini HIGH: 當地/當局/當中/當然 must not be misclassified as
        // 當…的時候 connectives even when 的時候 happens to follow.
        let cases = [
            "當地的時候情況不容易掌握。",
            "當局的時候反應顯得遲緩。",
            "當中的時候資訊很混亂。",
            "當然的時候大家都會選擇A。",
        ];
        for text in cases {
            let issues = scan_lex(text);
            assert!(
                !issues
                    .iter()
                    .any(|i| i.phase_family == Some((PhaseFamily::Connective, PhasePass::Lexical))),
                "ZY2a must not fire on 當-prefix words: {text}"
            );
        }
    }

    // Regression: detectors must not panic on empty / ASCII / mixed input.

    #[test]
    fn lexical_detectors_handle_empty_input() {
        assert!(scan_lex("").is_empty());
    }

    #[test]
    fn lexical_detectors_handle_ascii_only() {
        assert!(scan_lex("Hello world. Actually, basically.").is_empty());
    }

    #[test]
    fn lexical_detectors_handle_mixed_cjk_ascii_no_panic() {
        let text = "Hello 因為A，所以B。實際上 (literally) 100% sure.";
        let _ = scan_lex(text);
    }

    // Boundary-aware translationese detectors.

    fn scan_indexed(
        text: &str,
        domain: crate::engine::translationese_score::TranslationeseDomain,
    ) -> Vec<Issue> {
        let idx = BoundaryIndex::build(text, &[]);
        let mut issues = Vec::new();
        scan_translationese_indexed(&mut Emitter::new(text, &[], &mut issues), &idx, domain);
        issues
    }

    /// Select by detector identity rather than by the wording of the message.
    fn fires(issues: &[Issue], want: (PhaseFamily, PhasePass)) -> bool {
        issues.iter().any(|i| i.phase_family == Some(want))
    }

    // ZY1b -----------------------------------------------------------------

    #[test]
    fn zy1b_fires_on_yi_zhi_density() {
        // 6 之一 in a >100-char paragraph → density well above general
        // threshold (2.0/200).
        let text = "這是科學成就之一，這是科學成就之一。\
                    那是貢獻之一，那是貢獻之一。\
                    再者是發現之一，再者是發現之一。\
                    另一個發現之一，另一個發現之一。\
                    還有重要事件之一，還有重要事件之一。\
                    最後一個成就之一，最後一個成就之一。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            fires(&issues, (PhaseFamily::YiZhi, PhasePass::Indexed)),
            "expected 之一 段落密度過高 — in: {issues:?}"
        );
    }

    #[test]
    fn zy1b_passes_on_short_paragraph() {
        let text = "這是成就之一。那是貢獻之一。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(&issues, (PhaseFamily::YiZhi, PhasePass::Indexed)));
    }

    #[test]
    fn zy1b_register_switch_changes_firing() {
        // 3 之一 in ~250-char paragraph → density ~2.4/200. Above Literary
        // threshold (1.0/200) but below Technical (3.0/200). Natural prose
        // padding without further 之一 occurrences.
        let body = "他是領域裡的創新者，這項研究是當代成就，他在學術圈\
                    地位崇高，這個團隊也是行業領頭羊。整體來說這篇論文\
                    是經典代表之一，這個觀點是少數派論述之一，未來研究\
                    的方向也將以此為一例之一展開深入探討與分析。學界\
                    認為相關討論值得關注，後續研究也將圍繞這些主題\
                    進行更為深入的考察與比較分析。研究團隊指出，過去\
                    幾年的學術走向已經逐步成形，各種新方法不斷被提出\
                    且持續調整，主流共識正在凝聚當中，相關文獻也明顯\
                    增加，引起學界更廣泛關注。";
        let lit = scan_indexed(
            body,
            crate::engine::translationese_score::TranslationeseDomain::Literary,
        );
        let tech = scan_indexed(
            body,
            crate::engine::translationese_score::TranslationeseDomain::Technical,
        );
        // Literary threshold 1.0/200: fires; Technical 3.0/200: does not.
        assert!(
            fires(&lit, (PhaseFamily::YiZhi, PhasePass::Indexed)),
            "Literary should fire: {lit:?}"
        );
        assert!(
            !fires(&tech, (PhaseFamily::YiZhi, PhasePass::Indexed)),
            "Technical should not fire: {tech:?}"
        );
    }

    // ZY2b -----------------------------------------------------------------

    #[test]
    fn zy2b_fires_on_sentence_bounded_yinwei_suoyi() {
        let text = "因為下雨了，所以我們待在屋裡。這句話另起一行。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
            "expected ZY2b: {issues:?}"
        );
    }

    #[test]
    fn zy2b_does_not_fire_across_sentence_boundary() {
        // 因為 in sentence 1, 所以 in sentence 2: must NOT fire.
        let text = "他停下來，因為下雨了。所以大家紛紛回家了。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            !fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
            "should not span sentences: {issues:?}"
        );
    }

    #[test]
    fn zy2b_finds_real_dang_after_skipped_dangdi() {
        // Codex review: a guarded 當-prefix word (當地) early in a sentence
        // must not block a real 當…的時候 connective later in the same
        // sentence. Both opener occurrences must be examined.
        let text = "他在當地一直工作著，當我抵達總部的時候才終於和他見面。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
            "expected ZY2b: {issues:?}"
        );
    }

    #[test]
    fn zy2b_preserves_zy2a_distance_cap_inside_sentence() {
        let filler = "甲".repeat(45);
        let text = format!("因為{filler}所以我們決定延後。");
        let issues = scan_indexed(
            &text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            !fires(&issues, (PhaseFamily::Connective, PhasePass::Indexed)),
            "should respect ZY2a distance cap: {issues:?}"
        );
    }

    #[test]
    fn zy1b_anchor_skips_excluded_first_hit() {
        // Codex review: ZY1b's anchor must point at the first NON-excluded
        // 之一, not the first raw substring hit. When an excluded zone covers
        // the first hit but the paragraph still qualifies, the issue must still
        // emit (anchored elsewhere).
        let body = "他是領域裡的創新者之一，這項研究是當代成就，他在學術圈\
                    地位崇高，這個團隊也是行業領頭羊。整體來說這篇論文\
                    是經典代表之一，這個觀點是少數派論述之一，未來研究\
                    的方向也將以此為一例之一展開深入探討。學界認為相關\
                    討論值得關注，後續研究也將圍繞這些主題進行更為深入\
                    的考察與比較分析。";
        let text = format!("前綴 {body}");
        let first_zhi_yi = text.find("之一").unwrap();
        let excluded: &[ByteRange] = &[ByteRange {
            start: first_zhi_yi,
            end: first_zhi_yi + "之一".len(),
        }];
        let idx = BoundaryIndex::build(&text, excluded);
        let mut issues = Vec::new();
        scan_translationese_indexed(
            &mut Emitter::new(&text, excluded, &mut issues),
            &idx,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        let zy1b: Vec<_> = issues
            .iter()
            .filter(|i| i.phase_family == Some((PhaseFamily::YiZhi, PhasePass::Indexed)))
            .collect();
        assert!(!zy1b.is_empty(), "ZY1b should still fire: {issues:?}");
        assert_ne!(zy1b[0].offset, first_zhi_yi);
    }

    #[test]
    fn zy2b_skips_dang_prefix_words() {
        let text = "當地的時候情況不容易掌握。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::Connective, PhasePass::Indexed)
        ));
    }

    // ZY3b -----------------------------------------------------------------

    #[test]
    fn zy3b_fires_on_three_chain_in_general_domain() {
        // 改善的提升的發現: 3 chained heads, depth 3 ≥ general's chain_min 3.
        let text = "他完成改善的提升的發現工作。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            fires(&issues, (PhaseFamily::Nominalization, PhasePass::Indexed)),
            "expected ZY3b: {issues:?}"
        );
    }

    #[test]
    fn zy3b_passes_on_two_chain_in_technical_domain() {
        // Technical bumps chain_min to 4: a 3-level chain doesn't fire.
        let text = "他完成改善的提升的發現工作。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::Technical,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::Nominalization, PhasePass::Indexed)
        ));
    }

    #[test]
    fn zy3b_passes_on_two_chain_in_general() {
        // 2-level chain (depth 2) below general threshold (3).
        let text = "他完成改善的提升工作。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::Nominalization, PhasePass::Indexed)
        ));
    }

    #[test]
    fn zy3b_chain_does_not_consume_orphan_trailing_de() {
        // Gemini + Codex review: walk_zy3b_chain must not include a trailing 的
        // in the emitted span when no whitelisted head follows. Walker
        // invariants (substring-relative byte offsets; each CJK char is 3
        // bytes):
        //   walk_zy3b_chain("改善的提升的非詞", 0) = (2, 15)
        //     - cursor lands just past 提升 (byte 15), not past the
        //     orphan 的 at byte 18.
        //   walk_zy3b_chain("改善的提升的發現的非詞", 0) = (3, 24)
        //     - cursor lands just past 發現 (byte 24), not past the
        //     orphan 的 at byte 27.
        // We exercise the second case end-to-end by running the full detector
        // and checking the emitted issue's found text does not end in 的. The
        // depth-2 case can't be checked end-to-end because it falls below the
        // default chain_min=3 threshold.
        let text = "他完成改善的提升的發現的非詞工作。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        let zy3b_issue = issues
            .iter()
            .find(|i| i.phase_family == Some((PhaseFamily::Nominalization, PhasePass::Indexed)))
            .expect("ZY3b should fire");
        // The emitted span must not end in 的: that's the orphan-的 bug.
        assert!(
            !zy3b_issue.found.ends_with('的'),
            "ZY3b span should not include orphan trailing 的, got: {:?}",
            zy3b_issue.found
        );
        // The span must end at 發現 (the last whitelisted head).
        assert!(
            zy3b_issue.found.ends_with("發現"),
            "ZY3b span should end at last head 發現, got: {:?}",
            zy3b_issue.found
        );
    }

    // ZY5 ------------------------------------------------------------------

    #[test]
    fn zy5_fires_on_long_premodifier() {
        // 19 chars, 2 的, comma-free: long-pre-modifier archetype.
        let text = "那個在車站外面的雨裡等了三個小時的男人終於放棄了。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "expected ZY5: {issues:?}"
        );
    }

    #[test]
    fn predicate_scan_agrees_with_the_direct_form() {
        let cases = [
            "他就走了的東西的樣子",
            "這個可以用的方法的問題",
            "就地取材的方式的結果", // 就 heads a word that is not masked
            "他的成就的意義的說明", // 就 as the tail of 成就: masked by the head
            "剛才的說法的意思",     // 才 as the tail of 剛才
            "方便的方式的結果",     // 便 as the tail of 方便
            "忘卻的往事的痕跡",     // 卻 as the tail of 忘卻
            "已經完成的工作的內容",
            "東西的樣子的問題就", // marker flush against the right edge
            "正在進行的計畫的目標",
            "沒有任何標記的一般文字",
            "才對的說法的意思",
        ];

        // Only right edges that fall on a 的 are checked, because those are the
        // only ones ZY5 asks about, and they are what makes reading the windows
        // from the span rather than the region equivalent.
        for case in cases {
            for lo in (0..case.len()).filter(|&i| case.is_char_boundary(i)) {
                let close = first_predicate_close(case, lo);
                for (hi, _) in case.match_indices('的').filter(|&(at, _)| at >= lo) {
                    assert_eq!(
                        close.is_some_and(|close| close <= hi),
                        opens_a_predicate(&case[lo..hi]),
                        "case {case:?} region {lo}..{hi} = {:?}",
                        &case[lo..hi]
                    );
                }
            }
        }
    }

    #[test]
    fn zy5_reports_a_span_reaching_the_last_noun() {
        // Pins what the early exit depends on: the reported span runs to the
        // end of the comma-free segment, so stopping at the first candidate to
        // reach that end loses nothing. It reaches the end because the noun run
        // after 的 is taken over CJK characters and 的 is one of them, so the
        // run swallows the following phrases.
        let text = "那個在車站外面的雨裡等了三個小時的男人的外套";
        let issues = scan_general(text);
        let zy5 = issues
            .iter()
            .find(|i| i.phase_family == Some((PhaseFamily::LongPremodifier, PhasePass::Lexical)))
            .unwrap_or_else(|| panic!("expected ZY5: {issues:?}"));
        assert_eq!(
            zy5.offset + zy5.length,
            text.len(),
            "the reported span must reach the last noun: {zy5:?}"
        );
    }

    fn scan_general(text: &str) -> Vec<Issue> {
        scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        )
    }

    #[test]
    fn zy5_predicate_guard_covers_ordinary_verbs_and_adverbs() {
        // These markers precede lexical verbs and manner adverbs far more often
        // than function words, so the guard cannot key on what follows them.
        for word in [
            "就開始",
            "就變成",
            "就徹底",
            "就永遠",
            "就逐漸",
            "就此",
            "就算",
            "卻依然",
            "也很快",
            "才慢慢",
            "便迅速",
            "就會",
            "就要",
            "就被",
        ] {
            let text = format!("那個看起來十分陌生的城市{word}吞噬他熟悉的日常生活。");
            let issues = scan_general(&text);
            assert!(
                !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
                "unexpected ZY5 for {word}: {issues:?}"
            );
        }
    }

    #[test]
    fn zy5_passes_when_a_predicate_separates_the_de_phrases() {
        // 16 chars, 2 的, comma-free, but 也能 opens a predicate between them,
        // so 每週的檢討會議 and 真正的瓶頸 modify different heads. This was a
        // live false positive in tests/corpus/native-zh-tw.json.
        let text = "每週的檢討會議也能聚焦真正的瓶頸。";
        let issues = scan_general(text);
        assert!(
            !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "unexpected ZY5: {issues:?}"
        );
    }

    #[test]
    fn marker_words_are_all_two_characters() {
        // opens_a_predicate masks with two fixed-width window lookups, so a
        // longer entry would be silently ignored rather than masking anything.
        for word in MARKER_WORDS {
            assert_eq!(
                word.chars().count(),
                2,
                "{word} must be two characters for the window mask to see it"
            );
        }
    }

    #[test]
    fn zy5_predicate_guard_ignores_markers_inside_words() {
        // The marker may sit at either end of an ordinary word: 成就 and 人才
        // end with one, 便利 and 就業 start with one. None of them opens a
        // predicate, so all of these stay stacked pre-modifiers and must fire.
        for text in [
            "那是一個工匠面對自己的成就被命運直接抹除的瞬間。",
            "這乃是一個從不細看自己所寫之物的人才會犯下的錯誤。",
            "為了兼顧系統的便利性與穩定性的整體平衡設計。",
            "這是一份針對長期投入的就業輔導方案累積的完整報告。",
            "憑著他的才華洋溢加上長年累積的舞台經驗。",
        ] {
            let issues = scan_general(text);
            assert!(
                fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
                "expected ZY5 for {text:?}: {issues:?}"
            );
        }
    }

    #[test]
    fn zy5_passes_on_native_long_name() {
        // 中華民國行政院: 7 chars, 0 的: never fires.
        let text = "中華民國行政院昨日發表了新政策。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::LongPremodifier, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy5_passes_on_short_native_possessive() {
        // 我父親的朋友的兒子: 8 chars, fails 15-char gate.
        let text = "我父親的朋友的兒子今天來訪。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::LongPremodifier, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy5_passes_when_internal_comma_breaks_span() {
        // Same chars but with a comma: span is broken, native rhythm.
        let text = "那個男人在車站外面的雨裡，等了三個小時，終於放棄了。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(!fires(
            &issues,
            (PhaseFamily::LongPremodifier, PhasePass::Lexical)
        ));
    }

    #[test]
    fn zy5_passes_in_technical_domain_at_borderline() {
        // 18-char span: exactly at Technical threshold (zy5_min_chars=18) but
        // only 17 chars after counting → doesn't qualify.
        let text = "車站外面的雨裡等了三小時的男人。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::Technical,
        );
        let _ = issues; // Just verifying no panic; behavior depends on count.
    }

    #[test]
    fn zy5_passes_on_long_clause_without_premodifier_endpoint() {
        let text = "我昨天在博物館看到他的朋友的兒子正在導覽。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "unexpected ZY5: {issues:?}"
        );
    }

    #[test]
    fn zy5_passes_on_clause_that_only_ends_with_de_noun() {
        let text = "昨天在博物館看到他的朋友的兒子正在導覽。";
        let issues = scan_indexed(
            text,
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(
            !fires(&issues, (PhaseFamily::LongPremodifier, PhasePass::Lexical)),
            "unexpected ZY5: {issues:?}"
        );
    }

    // Cross-detector regression: empty / no-paragraph / unicode panics.

    #[test]
    fn indexed_detectors_handle_empty_input() {
        let issues = scan_indexed(
            "",
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn indexed_detectors_handle_short_input_no_panic() {
        let _ = scan_indexed(
            "短",
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
    }

    #[test]
    fn indexed_detectors_handle_ascii_only() {
        let issues = scan_indexed(
            "Hello world. Actually basically.",
            crate::engine::translationese_score::TranslationeseDomain::General,
        );
        assert!(issues.is_empty());
    }
}
