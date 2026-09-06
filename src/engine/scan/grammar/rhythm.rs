//! The rhythm (氣口) advisory axis.
//!
//! A sentence that never pauses, and a run of sentences closing on the same
//! particle. Opt-in and never fixable, because rhythm is taste. ZY5 borrows
//! the fragment test when the flag relaxes its own 的 count.

use super::*;

// Rhythm (氣口) advisory axis, gated by ProfileConfig::rhythm and off by
// default. good-writing-tw measures four things; ZY5 already covers the stacked
// pre-modifier and sentence-length variability is measured elsewhere and
// deliberately unscored, so the two checks here are the ones nothing in the
// tree performs: a sentence that never pauses, and a run of sentences that all
// close on the same particle.
//
// There is no cross-guard against the AI score. The guard earlier drafts asked
// for suppressed the advisory in a paragraph whose sentence-length sigma sat
// near a floor, and ai_score.rs no longer has that floor: variability is
// computed and serialized but contributes nothing, so splitting long sentences
// into uniform short ones cannot raise the AI score for the guard to prevent.
// Reviving variability as a signal needs a coefficient of variation and a
// long-form corpus to calibrate it, neither of which exists. Until then this
// axis ships unguarded, and 均質化本身就是新的 AI tell stays a warning in the
// docs rather than a threshold in the code.

// A sentence past this many CJK characters has not offered the reader a breath.
const RHYTHM_MAX_SENTENCE_CJK: usize = 30;

// A pause-free fragment shorter than this is not a violation however long the
// sentence around it is. good-writing-tw spends more words on the exemptions
// than on the rule, and this is what they amount to: a terminology run, a 頓號
// list, a parenthetical aside and a dash-introduced fragment all already
// contain the pause the rule is asking for.
pub(super) const RHYTHM_MIN_FRAGMENT_CJK: usize = 15;

// Everything that gives the reader somewhere to breathe. Enumeration commas,
// brackets and dashes are here for the reason the exemption list names them:
// the aside they open is itself the pause. Treating them as breakers rather
// than as whole-sentence exemptions keeps a genuinely runaway sentence
// reportable when it happens to contain one bracket.
const RHYTHM_PAUSE_BREAKERS: &[char] = &[
    '，', '、', '；', '：', '。', '！', '？', ',', ';', ':', '.', '!', '?', '（', '）', '(', ')',
    '「', '」', '『', '』', '【', '】', '〔', '〕', '《', '》', '〈', '〉', '—', '－', '-', '…',
    '"', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}',
];

// Particles that end a sentence flatly enough that three in a row read as one
// tune played three times.
const RHYTHM_MONOTONE_ENDINGS: &[char] = &['的', '了', '呢'];

// Consecutive sentences on the same ending particle before it is monotony
// rather than coincidence.
const RHYTHM_MONOTONY_RUN: usize = 3;

/// Count the CJK ideographs in `s`.
///
/// This is the "after stripping Latin, digits and punctuation" measurement the
/// exemption list asks for, done by counting what is left rather than by
/// building a stripped copy: a 40-byte identifier or a version number is not a
/// clause the reader has to hold in their head.
fn rhythm_cjk_len(s: &str) -> usize {
    s.chars().filter(|&ch| is_cjk_ideograph(ch)).count()
}

/// The longest run of CJK characters in `s` uninterrupted by a pause breaker.
///
/// Shared with ZY5's relaxed path, which is why it lives beside the constants
/// rather than inside the long-sentence check: relaxing ZY5's 的 gate hands
/// this the job of deciding whether the span is really one breath.
pub(super) fn longest_pause_free_run(s: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in s.chars() {
        if RHYTHM_PAUSE_BREAKERS.contains(&ch) {
            longest = longest.max(current);
            current = 0;
        } else if is_cjk_ideograph(ch) {
            current += 1;
        }
    }
    longest.max(current)
}

// Rhythm check 1: a single sentence that runs past RHYTHM_MAX_SENTENCE_CJK
// characters without a pause long enough to count as one.
fn scan_rhythm_long_sentence(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);
    for sent in &idx.sentences {
        if is_excluded(sent.byte_start, sent.byte_end, excluded) {
            continue;
        }
        let raw = &text[sent.byte_start..sent.byte_end];
        let total = rhythm_cjk_len(raw);
        if total <= RHYTHM_MAX_SENTENCE_CJK {
            continue;
        }
        if longest_pause_free_run(raw) < RHYTHM_MIN_FRAGMENT_CJK {
            continue;
        }

        // The anchor names the sentence, so it stops where the sentence does. A
        // sentence this long always has eight characters to spare, but the
        // bound means the next reader does not have to work that out.
        let start = sent.byte_start + (raw.len() - raw.trim_start().len());
        let end = char_bounded_end(text, start, 8).min(sent.byte_end);
        if start >= end {
            continue;
        }
        issues.push(
            Issue::new(
                start,
                end - start,
                &text[start..end],
                vec![],
                IssueType::Translationese,
                Severity::Info,
            )
            .with_phase_family(PhaseFamily::RhythmLongSentence, PhasePass::Indexed)
            .with_context(format!(
                "氣口：長句 — 單句 {total} 字未換氣，建議拆句或補標點"
            )),
        );
    }
}

// What may sit between the last spoken character and the end of a sentence:
// whitespace, the terminal mark itself, and any bracket or quote closing around
// it. Anything else is content, and a sentence ending in content is not ending
// on a particle however recently one appeared.
const RHYTHM_SENTENCE_CLOSERS: &[char] = &[
    '。', '！', '？', '；', '，', '、', '…', '.', '!', '?', ';', ',', '」', '』', '）', ')', '】',
    '〕', '》', '〉', '"', '\u{201D}', '\u{2019}',
];

/// The sentence-final particle of `s`, or `None` when the sentence does not
/// end on one.
///
/// The scan stops at the first character that is not a closer rather than
/// hunting backwards for a CJK ideograph: 他來了 v2 and 他來了（v2）end on a
/// version number, and reading them as 了-endings would build a monotony run
/// out of sentences that do not rhyme.
fn rhythm_ending_particle(s: &str) -> Option<char> {
    s.trim_end_matches(|ch: char| ch.is_whitespace() || RHYTHM_SENTENCE_CLOSERS.contains(&ch))
        .chars()
        .next_back()
        .filter(|ch| RHYTHM_MONOTONE_ENDINGS.contains(ch))
}

// Rhythm check 2: RHYTHM_MONOTONY_RUN or more consecutive sentences closing on
// the same particle. Runs are maximal, so a paragraph of five 了-endings is one
// finding rather than three overlapping ones.
fn scan_rhythm_ending_monotony(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    let (text, excluded, issues) = (em.text, em.excluded, &mut *em.issues);

    // Paragraph by paragraph: a paragraph break is a change of subject, and the
    // tune restarting there is not monotony.
    for para in &idx.paragraphs {
        let mut run_particle: Option<char> = None;
        let mut run_start = 0usize;
        let mut run_limit = 0usize;
        let mut run_len = 0usize;

        // The anchor names the first sentence of the run, so it stops where
        // that sentence does rather than running into the next one.
        let flush = |particle: Option<char>,
                     start: usize,
                     limit: usize,
                     len: usize,
                     issues: &mut Vec<Issue>| {
            let (Some(particle), true) = (particle, len >= RHYTHM_MONOTONY_RUN) else {
                return;
            };
            let end = char_bounded_end(text, start, 8).min(limit);
            if start >= end {
                return;
            }
            issues.push(
                Issue::new(
                    start,
                    end - start,
                    &text[start..end],
                    vec![],
                    IssueType::Translationese,
                    Severity::Info,
                )
                .with_phase_family(PhaseFamily::RhythmMonotony, PhasePass::Indexed)
                .with_context(format!(
                    "氣口：句尾單調 — 連續 {len} 句以「{particle}」結尾，建議換句式"
                )),
            );
        };

        for sent in idx.sentence_slice(para) {
            if is_excluded(sent.byte_start, sent.byte_end, excluded) {
                flush(run_particle, run_start, run_limit, run_len, issues);
                run_particle = None;
                run_len = 0;
                continue;
            }
            let raw = &text[sent.byte_start..sent.byte_end];
            let particle = rhythm_ending_particle(raw);
            if particle.is_some() && particle == run_particle {
                run_len += 1;
                continue;
            }
            flush(run_particle, run_start, run_limit, run_len, issues);
            run_particle = particle;
            run_len = usize::from(particle.is_some());
            run_start = sent.byte_start + (raw.len() - raw.trim_start().len());
            run_limit = sent.byte_end;
        }
        flush(run_particle, run_start, run_limit, run_len, issues);
    }
}

// Entry point for the rhythm axis. Gated by ProfileConfig::rhythm, which is off
// in every profile, so this runs only when the user asks for it.
pub(crate) fn scan_rhythm(em: &mut Emitter<'_>, idx: &crate::engine::sentence::BoundaryIndex) {
    scan_rhythm_long_sentence(em, idx);
    scan_rhythm_ending_monotony(em, idx);
}

// Entry point for AI writing detection grammar checks. Gated by
// ProfileConfig::ai_semantic_safety, NOT called from scan_grammar.
