//! Translationese (翻譯腔 / 歐化) detectors.
//!
//! The syntactic scan_trans_* pass, the substring-only ZY1a-ZY4a lexical pass,
//! and the boundary-aware ZY1b-ZY5 indexed pass, which share a vocabulary.

use super::*;

// Syntactic translationese detectors (require BoundaryIndex)

// Passive voice density: count 被 per paragraph, flag above two per 100 chars.
pub(super) fn scan_trans_passive_density(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
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
pub(super) fn scan_trans_abstract_subject(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
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
pub(super) fn scan_trans_displaced_conditional(
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
pub(super) fn scan_trans_pronoun_overuse(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
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
pub(super) fn scan_trans_copula_classifier(
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
pub(super) fn scan_trans_adverbial_particle_mixup(
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
pub(super) fn scan_trans_excessive_de_chain(
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
pub(super) fn scan_trans_adverbial_particle_redundant(
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
pub(super) fn scan_zy1a_superlative_yi_zhi(em: &mut Emitter<'_>) {
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

pub(super) fn scan_zy2a_connective_calques(em: &mut Emitter<'_>, register: Register) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // (opener, closer, max_chars_between, label). Distance budget per opener:
    // 40 chars for 因/雖/如, 30 chars for 當.
    const PATTERNS: &[(&str, &str, usize, &str)] = &[
        ("因為", "所以", 40, "因為…所以"),
        ("雖然", "但是", 40, "雖然…但是"),
        ("當", "的時候", 30, "當…的時候"),
        ("如果", "那麼", 40, "如果…那麼"),
    ];

    // A formal letter or contract template mandates the paired connective, so
    // reporting it there is reporting the form itself. The marker list this
    // used to read for itself now lives in engine::register, where the
    // bureaucratic detector can read it too.
    if register == Register::Formal {
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
pub(super) fn scan_zy3a_finite_nominalization(em: &mut Emitter<'_>) {
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
pub(super) fn scan_zy4a_false_friends(em: &mut Emitter<'_>) {
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

// Locate the clause containing pos (byte offset), where a clause ends at any of
// the marks is_clause_boundary_char names, or at the start or end of the text.
// Kept in step with that function rather than restating its list, which is how
// this comment came to describe a narrower set than the code. Caller must pass
// a valid char boundary; debug builds assert this so a future caller passing an
// interior byte trips an explicit failure.
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
pub(super) fn scan_trans_tense_marker(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
) {
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
pub(super) fn scan_zy1b_yi_zhi_density(
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
pub(super) fn scan_zy2b_sentence_bounded_connectives(
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
pub(super) fn scan_zy3b_nominalization_chain(
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
//
// The rhythm flag relaxes the second condition to what the span already
// guarantees. The 的-count gate is the only thing between this detector and a
// general 氣口 check: a 25-character run with one 的 is the same unbroken
// breath as one with three, and the count is there to keep the default pass
// conservative rather than because a single 的 makes the span acceptable.
//
// Removing a gate needs a gate put back, or a Latin identifier run and a
// parenthetical aside both start counting as breathless prose. The rhythm
// axis's own fragment test takes over: it counts CJK ideographs only and treats
// brackets and dashes as the pauses they are, neither of which ZY5's own
// character count and SPAN_BREAKERS list do.
//
// The relaxation reaches ZY5 only when the translationese pass is running,
// because a threshold cannot relax a detector that is switched off. Both
// shipped profiles run it, so in practice the flag alone is enough.
pub(super) fn scan_zy5_long_premodifier(
    em: &mut Emitter<'_>,
    idx: &crate::engine::sentence::BoundaryIndex,
    domain: crate::engine::translationese_score::TranslationeseDomain,
    rhythm: bool,
) {
    let text = em.text;

    const SPAN_BREAKERS: &[char] = &['，', '、', '。', '；', '：', ',', ';', ':'];
    let thresholds = domain.thresholds();
    let min_chars = thresholds.zy5_min_chars;

    // Every candidate is defined by a 的, so 1 is a bypass rather than a
    // different threshold, and a zero-length run is a floor no span can fail.
    let gate = if rhythm {
        Zy5Gate {
            min_chars,
            min_de: 1,
            calibrated_min_de: thresholds.zy5_min_de_count,
            min_pause_free_run: RHYTHM_MIN_FRAGMENT_CJK,
        }
    } else {
        Zy5Gate {
            min_chars,
            min_de: thresholds.zy5_min_de_count,
            calibrated_min_de: thresholds.zy5_min_de_count,
            min_pause_free_run: 0,
        }
    };

    for sent in &idx.sentences {
        let s = &text[sent.byte_start..sent.byte_end];
        let mut emit = |start, end| {
            emit_zy5_span_if_qualifies(em, s, sent.byte_start, start..end, gate);
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
pub(super) fn first_predicate_close(span: &str, region_start: usize) -> Option<usize> {
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
pub(super) fn opens_a_predicate(s: &str) -> bool {
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
pub(super) const MARKER_WORDS: &[&str] = &[
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

/// What a span has to clear before ZY5 will report it.
///
/// Three numbers that move together: the rhythm flag relaxes min_de to 1 and
/// buys the gate back with min_pause_free_run, so a caller that set one without
/// the other would get a detector nobody calibrated.
#[derive(Clone, Copy)]
struct Zy5Gate {
    min_chars: usize,
    min_de: usize,
    /// What the domain asks for when the taste flag is absent. A span that
    /// clears min_de but not this one is the flag speaking, and is reported as
    /// advisory so it cannot reach a calibrated score.
    calibrated_min_de: usize,
    /// Zero is a floor no span can fail, which is the flag being off.
    min_pause_free_run: usize,
}

fn emit_zy5_span_if_qualifies(
    em: &mut Emitter<'_>,
    sent_text: &str,
    sent_offset: usize,
    span_bytes: std::ops::Range<usize>,
    gate: Zy5Gate,
) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);
    let Zy5Gate {
        min_chars,
        min_de,
        calibrated_min_de,
        min_pause_free_run,
    } = gate;
    let (span_start, span_end) = (span_bytes.start, span_bytes.end);

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

    // Whether the taste flag is the only reason this span is here. The walk
    // picks the same candidate either way, because a longer candidate can only
    // hold more 的, so a span that clears the calibrated count is the span the
    // default pass would have reported.
    let relaxed_only = de_count < calibrated_min_de;

    // The gate that replaces the relaxed 的 count, so it applies only where
    // that relaxation is what let the span through. Applying it to a span the
    // calibrated gate already passed would let the flag delete a finding the
    // default run makes, which is the score moving in the direction the axis
    // promises never to move it. Applied to the winning candidate rather than
    // inside the walk: it decides only whether to report.
    if relaxed_only && longest_pause_free_run(&span[..candidate_end]) < min_pause_free_run {
        return;
    }
    let abs_start = sent_offset + span_start;
    let abs_end = sent_offset + span_start + candidate_end;
    issues.push(
        Issue::new(
            abs_start,
            candidate_end,
            &text[abs_start..abs_end],
            vec![],
            IssueType::Translationese,
            // An advisory finding carries the advisory severity, or an opt-in
            // taste flag raises the warning count a gate is checked against.
            if relaxed_only {
                Severity::Info
            } else {
                Severity::Warning
            },
        )
        .with_phase_family(
            if relaxed_only {
                PhaseFamily::RhythmLongPremodifier
            } else {
                PhaseFamily::LongPremodifier
            },
            PhasePass::Lexical,
        )
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
pub(crate) fn numbered_list_marker_len(s: &str) -> Option<usize> {
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
