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
use crate::engine::scan::rule_ir::StructuralGuard;
use crate::engine::scan::{char_bounded_end, is_cjk_ideograph};
use crate::rules::ruleset::{
    AttributionGenre, Issue, IssueType, PhaseFamily, PhasePass, Register, Severity,
    StructuralFamily,
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
// → "討論", "加以分析" → "分析", "予以處理" → "處理" The flag is the formal
// register's licence: 予以核准 is what a 公文 writes, and telling its author to
// write 核准 is telling them to stop writing 公文. The other two carry no such
// licence, because 進行討論 is padding in a letter as much as in a blog post.
const BUREAUCRATIC_PREFIXES: &[(&str, bool)] = &[("進行", false), ("加以", false), ("予以", true)];

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
    for (i, &(prefix, _)) in BUREAUCRATIC_PREFIXES.iter().enumerate() {
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
fn validate_bureaucratic_nominalization(
    em: &mut Emitter<'_>,
    abs_pos: usize,
    prefix_end: usize,
    pattern_index: usize,
    register: Register,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    if is_excluded(abs_pos, prefix_end, excluded) {
        return;
    }
    if register == Register::Formal && BUREAUCRATIC_PREFIXES[pattern_index].1 {
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
    genre: AttributionGenre,
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
    genre: AttributionGenre,
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
        AttributionGenre::Casual => {
            "vague authority attribution; name the source or rewrite the clause without it"
        }
        AttributionGenre::Technical | AttributionGenre::Financial => {
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
fn scan_bureaucratic_nominalization(em: &mut Emitter<'_>, register: Register) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    for &(prefix, formal_licenses_prefix) in BUREAUCRATIC_PREFIXES {
        if register == Register::Formal && formal_licenses_prefix {
            continue;
        }
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

mod ai_style;
use ai_style::*;
pub(crate) use ai_style::{scan_ai_density, scan_ai_zero_width};

mod translationese;
pub(crate) use translationese::numbered_list_marker_len;
use translationese::*;

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
pub(crate) fn scan_translationese_lexical(em: &mut Emitter<'_>, register: Register) {
    scan_zy1a_superlative_yi_zhi(em);
    scan_zy2a_connective_calques(em, register);
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
    rhythm: bool,
    register: Register,
) {
    scan_zy1b_yi_zhi_density(em, boundary_index, domain);
    if register != Register::Formal {
        scan_zy2b_sentence_bounded_connectives(em, boundary_index);
    }
    scan_zy3b_nominalization_chain(em, boundary_index, domain);
    scan_zy5_long_premodifier(em, boundary_index, domain, rhythm);
}

mod rhythm;
pub(crate) use rhythm::scan_rhythm;
use rhythm::*;

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
pub(crate) fn scan_grammar(em: &mut Emitter<'_>, register: Register) {
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
                validate_bureaucratic_nominalization(em, start, end, pattern_index, register);
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
fn scan_grammar_legacy(em: &mut Emitter<'_>, register: Register) {
    scan_a_not_a_ma(em);
    scan_he_connecting_clauses(em);
    scan_bare_shi_adjective(em);
    scan_redundant_preposition(em);
    scan_bureaucratic_nominalization(em, register);
    scan_verbose_action(em);
    scan_dui_jinxing(em);
    scan_double_attribution(em);
}

#[cfg(test)]
mod tests;
