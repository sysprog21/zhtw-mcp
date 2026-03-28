// Criterion benchmarks for zhtw-mcp scanning pipeline.
//
// Covers the benchmark targets:
//   1. Scanner construction (Aho-Corasick automaton build, clone-free)
//   2. Plain-text scan on 1KB / 10KB / 100KB mixed CJK+ASCII text
//   3. scan_profiled with StrictMoe profile (plain text)
//   4. Fix path: apply-only (50 issues) + end-to-end scan+fix on 10KB
//   5. Context-clue-heavy scan (asserted >= 20% clue-gated issues)
//   6. Markdown exclusion pass (build_markdown_excluded_ranges)
//   7. FMM segmenter on 100-char text
//   8. Post-scan transforms (ignore downgrade + remap)
//   9. Per-stage CPU attribution on 100KB

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

use zhtw_mcp::engine::markdown::build_markdown_excluded_ranges;
use zhtw_mcp::engine::scan::Scanner;
use zhtw_mcp::engine::segment::Segmenter;
use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
use zhtw_mcp::rules::loader::load_embedded_ruleset;
use zhtw_mcp::rules::ruleset::Profile;

// Test data generation

/// Base paragraph (~200 bytes) mixing CJK prose with Mainland terms the
/// scanner will flag, plus some ASCII for realism.  Repeating this block
/// scales linearly while keeping a consistent hit ratio.
const BASE_PARAGRAPH: &str = "\
台灣的軟件工程師使用人工智能技術開發應用程序。\
他們的網絡質量很高，信息安全也很重要。\
The server handles HTTP requests via async runtime.\n\
數據庫中存儲了用戶的個人信息和視頻文件。\
項目採用敏捷開發的方法論，並行運算能力很強。\n";

/// Build mixed CJK+ASCII text of approximately `target_bytes` size.
fn generate_text(target_bytes: usize) -> String {
    let repeats = (target_bytes / BASE_PARAGRAPH.len()).max(1);
    let mut text = BASE_PARAGRAPH.repeat(repeats);
    if text.len() > target_bytes {
        text.truncate(text.floor_char_boundary(target_bytes));
    }
    text
}

/// Generate Markdown text with code blocks, inline code, frontmatter,
/// and prose for the exclusion-pass benchmark.
fn generate_markdown(target_bytes: usize) -> String {
    let block = "\
---\ntitle: 測試文件\ndate: 2024-01-01\n---\n\n\
# 標題：軟件開發指南\n\n\
台灣的軟件工程師使用 `println!` 來調試程序。\n\n\
```rust\nfn main() {\n    let x = 軟件;\n    println!(\"{x}\");\n}\n```\n\n\
數據庫中存儲了用戶的信息。The server runs on port 8080.\n\n\
> 引用文字：這是一段 `inline code` 和一些文字。\n\n\
- 項目一：網絡質量\n  - `async` 子項目\n- 項目二：信息安全\n\n";

    let repeats = (target_bytes / block.len()).max(1);
    let mut text = block.repeat(repeats);
    if text.len() > target_bytes {
        text.truncate(text.floor_char_boundary(target_bytes));
    }
    text
}

/// 100-char CJK string for the FMM segmenter benchmark.
/// Exactly 100 Chinese characters covering a mix of dictionary and
/// non-dictionary terms.
const SEGMENTER_INPUT: &str = "\
台灣的軟體工程師使用人工智慧技術開發應用程式。\
他們的網路品質很高，資訊安全也很重要。\
資料庫中儲存了使用者的個人資訊和影片檔案。\
這個專案採用敏捷開發的方法論，並行運算能力很強大。\
程式語言的選擇也非常關鍵。";

// 1. Scanner construction

fn bench_scanner_construction(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");

    // Use iter_batched so Vec::clone happens in setup, not the timed region.
    c.bench_function("scanner_construction", |b| {
        b.iter_batched(
            || (ruleset.spelling_rules.clone(), ruleset.case_rules.clone()),
            |(spelling, case)| {
                let scanner = Scanner::new(black_box(spelling), black_box(case));
                black_box(&scanner);
            },
            BatchSize::SmallInput,
        );
    });
}

// 1b. Construction breakdown: Segmenter vs Aho-Corasick builds

fn bench_construction_breakdown(c: &mut Criterion) {
    use aho_corasick::{AhoCorasickBuilder, MatchKind};

    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let spelling_rules: Vec<_> = ruleset
        .spelling_rules
        .iter()
        .filter(|r| !r.disabled)
        .cloned()
        .collect();
    let case_rules: Vec<_> = ruleset
        .case_rules
        .iter()
        .filter(|r| !r.disabled)
        .cloned()
        .collect();

    let mut group = c.benchmark_group("construction_breakdown");

    group.bench_function("segmenter_from_rules", |b| {
        b.iter(|| {
            let seg = Segmenter::from_rules(black_box(&spelling_rules));
            black_box(&seg);
        });
    });

    group.bench_function("spelling_aho_corasick", |b| {
        let patterns: Vec<&str> = spelling_rules.iter().map(|r| r.from.as_str()).collect();
        b.iter(|| {
            let ac = AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .build(black_box(&patterns))
                .expect("build spelling AC");
            black_box(&ac);
        });
    });

    group.bench_function("case_aho_corasick", |b| {
        let patterns: Vec<String> = case_rules.iter().map(|r| r.term.to_lowercase()).collect();
        b.iter(|| {
            let ac = AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .ascii_case_insensitive(true)
                .build(black_box(&patterns))
                .expect("build case AC");
            black_box(&ac);
        });
    });

    group.finish();
}

// 2. scan() on 1KB / 10KB / 100KB (plain text, no Markdown exclusion overhead)

fn bench_scan(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

    let sizes: &[(usize, &str)] = &[(1_024, "1KB"), (10_240, "10KB"), (102_400, "100KB")];

    let mut group = c.benchmark_group("scan");
    for &(size, label) in sizes {
        let text = generate_text(size);
        group.bench_with_input(BenchmarkId::from_parameter(label), &text, |b, text| {
            b.iter(|| {
                // scan_profiled_md(..., false) uses ContentType::Plain,
                // avoiding Markdown exclusion pass that scan() implicitly runs.
                let output = scanner.scan_profiled_md(black_box(text), Profile::Default, false);
                black_box(&output);
            });
        });
    }
    group.finish();
}

// 3. scan_profiled() with StrictMoe (plain text path)

fn bench_scan_profiled_strict_moe(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

    let sizes: &[(usize, &str)] = &[(1_024, "1KB"), (10_240, "10KB"), (102_400, "100KB")];

    let mut group = c.benchmark_group("scan_profiled_strict_moe");
    for &(size, label) in sizes {
        let text = generate_text(size);
        group.bench_with_input(BenchmarkId::from_parameter(label), &text, |b, text| {
            b.iter(|| {
                let output = scanner.scan_profiled_md(black_box(text), Profile::StrictMoe, false);
                black_box(&output);
            });
        });
    }
    group.finish();
}

// 4. Fix path benchmarks: apply-only and end-to-end scan+fix

fn bench_apply_fixes(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

    // Generate enough text to produce at least 50 issues.
    // The base paragraph has ~8 flaggable terms, so 10KB should yield plenty.
    let text = generate_text(10_240);
    let mut issues = scanner
        .scan_profiled_md(&text, Profile::Default, false)
        .issues;

    // Cap at exactly 50 issues for a controlled benchmark.
    issues.truncate(50);

    // Ensure we actually have issues to fix (sanity check at setup time).
    assert!(
        !issues.is_empty(),
        "benchmark setup: scanner found no issues in generated text"
    );

    let mut group = c.benchmark_group("fix_path");

    // 4a. Apply-only: measures fixer in isolation with pre-computed issues.
    group.bench_function("apply_fixes_50_issues", |b| {
        b.iter(|| {
            let result = apply_fixes_with_context(
                black_box(&text),
                black_box(&issues),
                FixMode::LexicalSafe,
                &[],
                Some(&segmenter),
            );
            black_box(&result);
        });
    });

    // 4b. End-to-end scan+fix on 10KB (the regression target from TODO 30.1).
    group.bench_function("scan_and_fix_10kb", |b| {
        b.iter(|| {
            let output = scanner.scan_profiled_md(black_box(&text), Profile::Default, false);
            let result = apply_fixes_with_context(
                black_box(&text),
                black_box(&output.issues),
                FixMode::LexicalSafe,
                &[],
                Some(&segmenter),
            );
            black_box(&result);
        });
    });

    group.finish();
}

// 5. Context-clue-heavy scan

/// Text with terms that trigger context_clue rules (函數, 實現, 配置, etc.).
/// This exercises the segmentation cache: many AC matches in close proximity
/// require context-clue resolution on overlapping windows.
const CONTEXT_CLUE_PARAGRAPH: &str = "\
在軟件工程中，函數的實現需要考慮配置管理和代碼質量。\
開發人員使用變量和地址來處理數據結構中的信息。\
刷新頁面後，全局的變量可能被實現為回調函數。\
交互式界面的實現涉及事務處理和場景部署。\
運行時環境中的配置需要注意並行計算的實現。\n";

fn bench_scan_context_clues(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);

    // Validate that the context-clue paragraph actually exercises clue resolution.
    // At least 20% of issues on 10KB should have context_clues (the TODO target is ~30%).
    {
        let check_text =
            CONTEXT_CLUE_PARAGRAPH.repeat((10_240 / CONTEXT_CLUE_PARAGRAPH.len()).max(1));
        let output = scanner.scan_profiled_md(&check_text, Profile::Default, false);
        let total = output.issues.len();
        let with_clues = output
            .issues
            .iter()
            .filter(|i| i.context_clues.is_some())
            .count();
        assert!(
            total > 0 && with_clues * 100 / total >= 20,
            "benchmark setup: context-clue paragraph must produce >= 20% clue-gated issues, \
             got {with_clues}/{total}"
        );
    }

    let sizes: &[(usize, &str)] = &[(1_024, "1KB"), (10_240, "10KB"), (102_400, "100KB")];

    let mut group = c.benchmark_group("scan_context_clues");
    for &(size, label) in sizes {
        let repeats = (size / CONTEXT_CLUE_PARAGRAPH.len()).max(1);
        let mut text = CONTEXT_CLUE_PARAGRAPH.repeat(repeats);
        if text.len() > size {
            text.truncate(text.floor_char_boundary(size));
        }
        group.bench_with_input(BenchmarkId::from_parameter(label), &text, |b, text| {
            b.iter(|| {
                let output = scanner.scan_profiled_md(black_box(text), Profile::Default, false);
                black_box(&output);
            });
        });
    }
    group.finish();
}

// 5. Markdown exclusion pass

fn bench_markdown_exclusion(c: &mut Criterion) {
    let sizes: &[(usize, &str)] = &[(1_024, "1KB"), (10_240, "10KB"), (102_400, "100KB")];

    let mut group = c.benchmark_group("markdown_exclusion");
    for &(size, label) in sizes {
        let md = generate_markdown(size);
        group.bench_with_input(BenchmarkId::from_parameter(label), &md, |b, md| {
            b.iter(|| {
                let ranges = build_markdown_excluded_ranges(black_box(md));
                black_box(&ranges);
            });
        });
    }
    group.finish();
}

// 6. FMM segmenter on 100-char text

fn bench_segmenter(c: &mut Criterion) {
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);

    // Verify the input is roughly 100 chars.
    let char_count = SEGMENTER_INPUT.chars().count();
    assert!(
        (90..=110).contains(&char_count),
        "segmenter input should be ~100 chars, got {char_count}"
    );

    c.bench_function("segmenter_100_chars", |b| {
        b.iter(|| {
            let tokens = segmenter.segment(black_box(SEGMENTER_INPUT));
            black_box(&tokens);
        });
    });
}

// 7. Post-scan transforms: ignore_terms downgrade + preserved-state remap

fn bench_post_scan_transforms(c: &mut Criterion) {
    use rustc_hash::FxHashMap;
    use std::collections::HashSet;
    use zhtw_mcp::fixer::remap_to_post_fix;
    use zhtw_mcp::fixer::AppliedFix;
    use zhtw_mcp::rules::ruleset::{Issue, IssueType, Severity};

    // Simulate post-scan transform workload at three issue counts.
    let issue_counts: &[(usize, &str)] = &[(100, "100"), (500, "500"), (1000, "1000")];
    let ignore_terms: HashSet<&str> = ["軟件", "信息", "數據"].iter().copied().collect();

    let mut group = c.benchmark_group("post_scan_transforms");
    for &(count, label) in issue_counts {
        // Build synthetic issues at evenly spaced offsets.
        let terms = ["軟件", "信息", "數據", "應用程序", "網絡"];
        let issues: Vec<Issue> = (0..count)
            .map(|i| {
                let term = terms[i % terms.len()];
                {
                    let mut issue = Issue::new(
                        i * 100,
                        term.len(),
                        term,
                        vec!["替代".to_string()],
                        IssueType::CrossStrait,
                        Severity::Warning,
                    );
                    issue.line = i + 1;
                    issue.col = 1;
                    issue.context = Some("test context".to_string());
                    issue.english = Some("term".to_string());
                    issue
                }
            })
            .collect();

        // Simulate applied fixes (every 5th issue gets a fix with +3 byte delta).
        let applied_fixes: Vec<AppliedFix> = (0..count)
            .step_by(5)
            .map(|i| AppliedFix {
                offset: i * 100,
                old_len: terms[i % terms.len()].len(),
                replacement: "替代用語".to_string(),
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("ignore_downgrade", label),
            &issues,
            |b, issues| {
                b.iter(|| {
                    let mut cloned = issues.clone();
                    let set = black_box(&ignore_terms);
                    for issue in &mut cloned {
                        if set.contains(issue.found.as_str()) {
                            issue.severity = Severity::Info;
                        }
                    }
                    black_box(&cloned);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("remap_hashmap_lookup", label),
            &issues,
            |b, issues| {
                b.iter(|| {
                    // Precompute remapped offsets and build index.
                    let mut state_by_offset: FxHashMap<usize, Vec<usize>> =
                        FxHashMap::with_capacity_and_hasher(issues.len(), Default::default());
                    for (idx, issue) in issues.iter().enumerate() {
                        let remapped = remap_to_post_fix(issue.offset, black_box(&applied_fixes));
                        state_by_offset.entry(remapped).or_default().push(idx);
                    }
                    // Simulate lookup for each remaining issue using post-fix offsets.
                    let mut match_count = 0usize;
                    for issue in issues {
                        let remapped = remap_to_post_fix(issue.offset, black_box(&applied_fixes));
                        if let Some(candidates) = state_by_offset.get(&remapped) {
                            if candidates.iter().any(|&idx| {
                                let s = &issues[idx];
                                s.found == issue.found && s.length == issue.length
                            }) {
                                match_count += 1;
                            }
                        }
                    }
                    black_box(match_count);
                });
            },
        );
    }
    group.finish();
}

// 8. Per-stage CPU attribution on 100KB input
//
// Measures each scan stage in isolation to determine where time is spent.
// Uses ProfileConfig flags to enable/disable individual stages.
// Note: baseline_no_checks (all-off) early-returns on zero issues, so it
// excludes LineIndex construction.  LineIndex cost is benchmarked separately
// in lineindex_100kb and is hidden inside stages that produce issues
// (e.g. spelling_only).

fn bench_cpu_attribution_100kb(c: &mut Criterion) {
    use zhtw_mcp::engine::scan::ContentType;
    use zhtw_mcp::engine::zhtype::detect_chinese_type;
    use zhtw_mcp::rules::ruleset::{PoliticalStance, ProfileConfig};

    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    let text = generate_text(102_400);

    // Pre-build excluded ranges once (shared across stage benchmarks).
    let excluded =
        zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, ContentType::Plain);

    // All-off config: measures baseline overhead (detect_chinese_type +
    // vec alloc + sort).  LineIndex is skipped (early-return on 0 issues).
    let cfg_none = ProfileConfig {
        spelling: false,
        casing: false,
        basic_punctuation: false,
        colon_enforcement: false,
        dunhao_detection: false,
        range_normalization: false,
        variant_normalization: false,
        ellipsis_normalization: false,
        range_en_dash: false,
        grammar_checks: false,
        ai_filler_detection: false,
        ai_semantic_safety: false,
        ai_density_detection: false,
        ai_structural_patterns: false,
        ai_threshold_multiplier: 1.0,
        political_stance: PoliticalStance::RocCentric,
        offset_only: false,
    };

    // Spelling-only config.
    let cfg_spelling = ProfileConfig {
        spelling: true,
        ..cfg_none
    };

    // Punctuation + spacing only (basic_punctuation gates both scan_punctuation
    // and scan_spacing + scan_cn_curly_quotes).
    let cfg_punct = ProfileConfig {
        basic_punctuation: true,
        colon_enforcement: true,
        dunhao_detection: true,
        range_normalization: true,
        ellipsis_normalization: true,
        ..cfg_none
    };

    // Grammar-only config.
    let cfg_grammar = ProfileConfig {
        grammar_checks: true,
        ..cfg_none
    };

    // Case-only config.
    let cfg_case = ProfileConfig {
        casing: true,
        ..cfg_none
    };

    // Full default config.
    let cfg_full = Profile::Default.config();

    let mut group = c.benchmark_group("cpu_attribution_100kb");

    // Stage 0: detect_chinese_type alone.
    group.bench_function("detect_chinese_type", |b| {
        b.iter(|| {
            let t = detect_chinese_type(black_box(&text));
            black_box(t);
        });
    });

    // Stage 0b: build exclusion ranges.
    group.bench_function("build_exclusions_plain", |b| {
        b.iter(|| {
            let e = zhtw_mcp::engine::scan::build_exclusions_for_content_type(
                black_box(&text),
                ContentType::Plain,
            );
            black_box(&e);
        });
    });

    // Stage 0c: baseline (all checks off).
    group.bench_function("baseline_no_checks", |b| {
        b.iter(|| {
            let out = scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_none);
            black_box(&out);
        });
    });

    // Stage 1: spelling AC only.
    group.bench_function("spelling_only", |b| {
        b.iter(|| {
            let out =
                scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_spelling);
            black_box(&out);
        });
    });

    // Stage 2: case AC only.
    group.bench_function("case_only", |b| {
        b.iter(|| {
            let out = scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_case);
            black_box(&out);
        });
    });

    // Stage 3: punctuation + spacing + ellipsis.
    group.bench_function("punctuation_spacing", |b| {
        b.iter(|| {
            let out = scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_punct);
            black_box(&out);
        });
    });

    // Stage 4: grammar only.
    group.bench_function("grammar_only", |b| {
        b.iter(|| {
            let out = scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_grammar);
            black_box(&out);
        });
    });

    // Stage 5: full default profile (reference).
    group.bench_function("full_default", |b| {
        b.iter(|| {
            let out = scanner.scan_with_config(black_box(&text), black_box(&excluded), cfg_full);
            black_box(&out);
        });
    });

    // Post-scan overhead: LineIndex construction + line/col lookups.
    // This cost is hidden inside spelling_only (which produces issues)
    // but absent from baseline_no_checks (which early-returns on 0 issues).
    group.bench_function("lineindex_100kb", |b| {
        use zhtw_mcp::engine::lineindex::{ColumnEncoding, LineIndex};
        // Pre-collect valid char-boundary offsets for lookup simulation.
        let offsets: Vec<usize> = text
            .char_indices()
            .step_by(text.chars().count() / 200)
            .map(|(i, _)| i)
            .collect();
        b.iter(|| {
            let idx = LineIndex::new(black_box(&text));
            let mut sum = 0usize;
            for &off in &offsets {
                let (line, col) = idx.line_col(off, ColumnEncoding::Utf16);
                sum += line + col;
            }
            black_box(sum);
        });
    });

    // Stage 6: pure AC traversal (no eval, no sort, no overlap, no line/col).
    // Measures the floor cost of the DAAC iteration itself.
    group.bench_function("ac_traversal_only", |b| {
        let ac = scanner
            .ac_charwise()
            .expect("charwise AC must be available for traversal benchmark");
        b.iter(|| {
            let mut count = 0usize;
            for mat in ac.leftmost_find_iter(black_box(&text)) {
                black_box(mat.value());
                count += 1;
            }
            black_box(count);
        });
    });

    // Markdown exclusion (real-world content type, vs plain above).
    group.bench_function("build_exclusions_markdown", |b| {
        let md = generate_markdown(102_400);
        b.iter(|| {
            let e = zhtw_mcp::engine::scan::build_exclusions_for_content_type(
                black_box(&md),
                ContentType::Markdown,
            );
            black_box(&e);
        });
    });

    group.finish();
}

// Criterion harness

criterion_group!(
    benches,
    bench_scanner_construction,
    bench_construction_breakdown,
    bench_scan,
    bench_scan_profiled_strict_moe,
    bench_apply_fixes,
    bench_scan_context_clues,
    bench_markdown_exclusion,
    bench_segmenter,
    bench_post_scan_transforms,
    bench_cpu_attribution_100kb,
);
criterion_main!(benches);
