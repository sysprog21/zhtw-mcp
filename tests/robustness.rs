// Adversarial-input robustness for the scanner and the fixer.
//
// The engine is byte-offset architecture: roughly 190 direct text[a..b] slices,
// positions mapped back through NFC normalization and pulldown-cmark event
// ranges. That is the one place in this tree where a bug is a panic rather than
// a wrong answer, and a panic in a linter takes the caller's build down with
// it.
//
// This is a fuzzer in the shape a pure-Rust project can actually run. It is
// deterministic rather than coverage-guided: a seeded xorshift builds inputs
// from the fragments that historically broke offset arithmetic here, and every
// case runs on every "make check" rather than only when someone remembers to
// fuzz.
//
// Not cargo-fuzz, for three reasons in the order they disqualify it. Its
// libfuzzer-sys links LLVM's libFuzzer, which is C++, and this tree states that
// it has no C or C++ dependencies. It needs a nightly toolchain, and the gate
// runs on stable. And a coverage-guided run happens when somebody remembers to
// start one, where these cases run on every gate.
//
// The trade that buys: this explores only what the fragment list can compose,
// so it will not discover an input shape nobody thought of. Adding a fragment
// is the mitigation and is a one-line change.
//
// Failures print the seed and the input, so a red run reproduces from the
// message alone.

use zhtw_mcp::engine::excluded::ByteRange;
use zhtw_mcp::engine::scan::{ContentType, Scanner, ScratchSpace};
use zhtw_mcp::fixer::{apply_fixes, FixMode};
use zhtw_mcp::rules::ruleset::{Issue, Profile, ProfileConfig, Ruleset};

fn load_scanner() -> Scanner {
    let ruleset: Ruleset = serde_json::from_str(include_str!("../assets/ruleset.json")).unwrap();
    Scanner::new(ruleset.spelling_rules, ruleset.case_rules)
}

/// Fragments chosen to stress offset arithmetic rather than to read as prose.
///
/// Each group is something that has broken a byte-offset scanner somewhere:
/// characters wider than the ASCII the arithmetic was first written for,
/// characters that are invisible, characters that normalize to a different
/// length, and markup whose event ranges do not line up with its bytes.
const FRAGMENTS: &[&str] = &[
    // CJK, the common case, plus the terms real rules fire on.
    "軟件",
    "這個問題",
    "予以處理",
    "因為下雨了所以我們待在屋裡",
    "進行討論",
    // 4-byte scalars: any arithmetic assuming 3 bytes per CJK char breaks here.
    "𠀀𠀁𠀂",
    "🈚🈯🉐",
    // Combining marks and NFC: the normalized form is a different length from
    // the source, which is what the offset mapping exists to survive.
    "é",
    "e\u{0301}",
    "が",
    "か\u{3099}",
    // Zero-width and invisible: a detector reports these, so the span it
    // reports has to be a real one.
    "\u{200b}",
    "\u{200d}",
    "\u{feff}",
    "\u{00ad}",
    // Full-width and half-width punctuation, the punctuation pass's subject.
    "，",
    "。",
    "、",
    "：",
    "！？",
    "「引用」",
    "『內層』",
    "（括號）",
    ",",
    ".",
    "!?",
    "\"",
    "'",
    // Markdown whose event ranges do not match a naive byte walk.
    "```rust\n",
    "```\n",
    "`code`",
    "# 標題\n",
    "> 引用\n",
    "- 項目\n",
    "1. 項目\n",
    "**粗體**",
    "[連結](https://example.com)",
    "<!-- zhtw:ignore -->",
    "<!-- zhtw:ignore-block -->",
    "<!-- zhtw:end-ignore -->",
    // YAML, the third content type.
    "key: value\n",
    "---\n",
    // Structure that nests, repeats, or never closes.
    "(",
    ")",
    "「",
    "』",
    "\n\n",
    "\r\n",
    "\t",
    " ",
    "",
    // Latin and digits interleaved with CJK, where char and byte counts part.
    "config.toml",
    "v2",
    "ASCII text",
    "123456",
];

/// xorshift64*, so a failing seed reproduces exactly on any machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn build_input(seed: u64) -> String {
    let mut rng = Rng(seed | 1);
    let pieces = 1 + rng.below(40);
    let mut s = String::new();
    for _ in 0..pieces {
        let f = FRAGMENTS[rng.below(FRAGMENTS.len())];

        // Occasionally repeat a fragment hard: run-length is what pushes a
        // detector's window past the end of its buffer.
        let reps = match rng.below(16) {
            0 => 30,
            1..=3 => 5,
            _ => 1,
        };
        for _ in 0..reps {
            s.push_str(f);
        }
    }
    s
}

fn configs() -> Vec<(&'static str, ProfileConfig)> {
    let mut all = Vec::new();
    for (name, profile) in [("base", Profile::Base), ("strict", Profile::Strict)] {
        all.push((name, profile.config()));

        let mut everything = profile.config();
        everything.ai_filler_detection = true;
        everything.ai_semantic_safety = true;
        everything.ai_density_detection = true;
        everything.ai_structural_patterns = true;
        everything.translationese_detection = true;
        everything.grammar_checks = true;
        everything.rhythm = true;
        all.push((
            if name == "base" {
                "base+all-detectors"
            } else {
                "strict+all-detectors"
            },
            everything,
        ));
    }
    all
}

/// Every invariant the rest of the tree is entitled to assume about an issue.
///
/// Checked here rather than trusted, because a span that is off by a byte does
/// not fail until something slices with it, and by then the panic names a
/// different function from the one that computed it.
fn assert_spans_are_sane(text: &str, issues: &[Issue], case: &str) {
    assert_spans_avoid(text, issues, &[], case);
}

/// As above, and additionally that no finding sits wholly inside an excluded
/// range. That is close to the property the register work turns on: an anchor
/// in a code fence or a YAML key must not be read as prose, and a regression
/// there lands a span that is perfectly in bounds, so the checks below would
/// not see it.
///
/// Wholly inside rather than overlapping, because overlapping is not an
/// invariant of this engine: a punctuation finding on a mark that abuts a
/// suppression comment legitimately spans the boundary.
fn assert_spans_avoid(text: &str, issues: &[Issue], excluded: &[ByteRange], case: &str) {
    for issue in issues {
        for span in excluded {
            let (s, e) = (issue.offset, issue.offset + issue.length);
            assert!(
                !(s >= span.start && e <= span.end),
                "{case}: span {s}..{e} sits wholly inside excluded {}..{}",
                span.start,
                span.end
            );
        }
    }
    for issue in issues {
        let start = issue.offset;
        let end = issue.offset + issue.length;
        assert!(
            end <= text.len(),
            "{case}: span {start}..{end} runs past the {} byte input",
            text.len()
        );
        assert!(
            text.is_char_boundary(start),
            "{case}: span start {start} is not a char boundary"
        );
        assert!(
            text.is_char_boundary(end),
            "{case}: span end {end} is not a char boundary"
        );

        // The slice itself: the assertions above should make this infallible,
        // and doing it proves they were the right assertions.
        let _ = &text[start..end];
    }
}

const SEEDS: u64 = 3000;

#[test]
fn scanning_adversarial_input_never_panics_and_never_reports_a_bad_span() {
    let scanner = load_scanner();
    let content_types = [
        ContentType::Plain,
        ContentType::Markdown,
        ContentType::MarkdownScanCode,
        ContentType::Yaml,
    ];

    for seed in 0..SEEDS {
        let text = build_input(seed);
        let content_type = content_types[(seed as usize) % content_types.len()];
        for (name, cfg) in configs() {
            let case =
                format!("seed={seed} cfg={name} content_type={content_type:?} text={text:?}");
            let excluded: Vec<ByteRange> =
                zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, content_type);
            let mut scratch = ScratchSpace::default();
            let out = scanner.scan_with_config_into_content_type(
                &text,
                &excluded,
                cfg,
                content_type,
                &mut scratch,
            );
            assert_spans_avoid(&text, &out.issues, &excluded, &case);
        }
    }
}

#[test]
fn fixing_adversarial_input_never_panics_and_stays_valid_utf8() {
    let scanner = load_scanner();

    for seed in 0..SEEDS {
        let text = build_input(seed);
        let cfg = Profile::Base.config();
        let excluded: Vec<ByteRange> =
            zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, ContentType::Plain);
        let out = scanner.scan_with_config(&text, &excluded, cfg);

        for mode in [
            FixMode::Orthographic,
            FixMode::LexicalSafe,
            FixMode::LexicalContextual,
        ] {
            let case = format!("seed={seed} mode={mode:?} text={text:?}");
            let fixed = apply_fixes(&text, &out.issues, mode, &excluded);

            // Rust guarantees a String is UTF-8; what is worth asserting is
            // that rescanning the output is itself safe, since --fix does
            // exactly that and a fix that produced a broken span would only
            // surface on the second pass.
            let refixed_excluded: Vec<ByteRange> =
                zhtw_mcp::engine::scan::build_exclusions_for_content_type(
                    &fixed.text,
                    ContentType::Plain,
                );
            let second = scanner.scan_with_config(&fixed.text, &refixed_excluded, cfg);
            assert_spans_are_sane(&fixed.text, &second.issues, &case);
        }
    }
}

#[test]
fn a_lone_fragment_is_safe_on_its_own() {
    // The generator composes fragments, so a fragment that only breaks in
    // isolation could hide behind its neighbours. Cheap to rule out.
    let scanner = load_scanner();
    for fragment in FRAGMENTS {
        for (name, cfg) in configs() {
            let case = format!("fragment={fragment:?} cfg={name}");
            let excluded: Vec<ByteRange> =
                zhtw_mcp::engine::scan::build_exclusions_for_content_type(
                    fragment,
                    ContentType::Markdown,
                );
            let out = scanner.scan_with_config(fragment, &excluded, cfg);
            assert_spans_are_sane(fragment, &out.issues, &case);
        }
    }
}
