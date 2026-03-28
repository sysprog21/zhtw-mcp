// Integration tests for the CLI `lint` subcommand.
//
// Tests exit codes, output formats, profile selection, content-type handling,
// max-errors gating, max-warnings gating, and multi-file/directory linting.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("zhtw-mcp");
    path
}

fn run_lint_stdin(extra_args: &[&str], input: &str) -> Output {
    let bin = binary_path();
    Command::new(&bin)
        .args(["lint", "--"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .unwrap()
}

#[test]
fn cli_lint_human_format_exit_0_clean() {
    let output = run_lint_stdin(&[], "正確的軟體");
    assert!(output.status.success(), "clean text should exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No issues found"), "should say no issues");
}

#[test]
fn cli_lint_human_format_warnings_exit_0() {
    // Cross-strait terms are Warning severity; default --max-errors 0 only
    // gates on Error-severity issues, so warnings-only text exits 0.
    let output = run_lint_stdin(&[], "這個軟件很好用");
    assert!(output.status.success(), "warnings-only should exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("軟件"), "should mention the issue");
    assert!(stderr.contains("issue(s) found"), "should show count");
}

#[test]
fn cli_lint_json_format() {
    let output = run_lint_stdin(&["--format", "json"], "這個軟件很好用");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");
    assert!(parsed["total"].as_u64().unwrap() > 0);
    assert!(!parsed["issues"].as_array().unwrap().is_empty());
}

#[test]
fn cli_lint_profile_strict_moe() {
    // 裏 is a variant only flagged under strict_moe
    let output = run_lint_stdin(&["--format", "json", "--profile", "strict_moe"], "裏面");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["found"] == "裏"),
        "strict_moe should flag 裏 variant"
    );
}

#[test]
fn cli_lint_max_errors_gate() {
    // With --max-errors 100, even dirty text should exit 0 (below threshold)
    let output = run_lint_stdin(&["--max-errors", "100"], "這個軟件很好用");
    assert!(
        output.status.success(),
        "should exit 0 when errors <= max_errors"
    );
}

#[test]
fn cli_lint_content_type_markdown() {
    // 軟件 in code block should be excluded, 軟件 in prose should be flagged
    let output = run_lint_stdin(
        &["--format", "json", "--content-type", "markdown"],
        "正確文本\n\n```\n軟件 in code\n```\n\n這個軟件有問題",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    let software_issues: Vec<_> = issues.iter().filter(|i| i["found"] == "軟件").collect();
    assert_eq!(
        software_issues.len(),
        1,
        "markdown mode should exclude 軟件 in code block"
    );
}

#[test]
fn cli_lint_content_type_plain_overrides_md_extension() {
    // When --content-type plain is explicit, even .md content is treated as plain text
    let dir = tempfile::tempdir().unwrap();
    let md_file = dir.path().join("test.md");
    std::fs::write(&md_file, "```\n軟件\n```\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args([
            "lint",
            md_file.to_str().unwrap(),
            "--format",
            "json",
            "--content-type",
            "plain",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // In plain mode, backtick exclusion still applies, so 軟件 in triple-backtick
    // block is excluded by the regex-based backtick patterns.
    // The test verifies --content-type plain is accepted and doesn't crash.
    assert!(parsed.get("issues").is_some());
}

// -- max_warnings tests (TODO 14.5) ----------------------------------------

#[test]
fn cli_lint_max_warnings_gate_exit_1_when_exceeded() {
    // Cross-strait terms emit Warning severity. With --max-warnings 0, even one
    // warning should cause exit 1.
    let output = run_lint_stdin(&["--max-warnings", "0"], "這個軟件很好用");
    assert!(
        !output.status.success(),
        "should exit 1 when warnings exceed --max-warnings 0"
    );
}

#[test]
fn cli_lint_max_warnings_gate_exit_0_when_within_limit() {
    // With --max-warnings 100, one warning should exit 0.
    let output = run_lint_stdin(&["--max-warnings", "100"], "這個軟件很好用");
    assert!(
        output.status.success(),
        "should exit 0 when warnings <= --max-warnings 100"
    );
}

#[test]
fn cli_lint_max_warnings_and_max_errors_both_checked() {
    // Both thresholds must pass for exit 0.
    // "軟件" emits 1 warning. With --max-errors 100 --max-warnings 0 → exit 1.
    let output = run_lint_stdin(
        &["--max-errors", "100", "--max-warnings", "0"],
        "這個軟件很好用",
    );
    assert!(
        !output.status.success(),
        "should exit 1 when warnings gate fails even if errors gate passes"
    );
}

#[test]
fn cli_lint_md_file_auto_detects_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let md_file = dir.path().join("test.md");
    std::fs::write(&md_file, "正確\n\n```\n軟件\n```\n\n這個軟件不好").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md_file.to_str().unwrap(), "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    let sw: Vec<_> = issues.iter().filter(|i| i["found"] == "軟件").collect();
    assert_eq!(sw.len(), 1, ".md auto-detection should exclude code block");
}

// -- Multi-file / directory linting tests (19.4) ----------------------------

fn run_lint_args(args: &[&str]) -> Output {
    let bin = binary_path();
    Command::new(&bin)
        .arg("lint")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn cli_lint_directory_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(dir.path().join("a.md"), "這個軟件").unwrap();
    std::fs::write(sub.join("b.txt"), "這個軟件").unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("multi-file JSON is array");
    assert_eq!(arr.len(), 2, "should find 2 files recursively");
}

#[test]
fn cli_lint_directory_skips_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join(".hidden");
    std::fs::create_dir(&hidden).unwrap();
    std::fs::write(hidden.join("file.md"), "這個軟件").unwrap();
    std::fs::write(dir.path().join("visible.md"), "正確的軟體").unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // Only the visible file should be found (single file = object, not array).
    assert!(
        parsed.get("file").is_some(),
        "should find only 1 file (hidden skipped)"
    );
}

#[test]
fn cli_lint_directory_exclude_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let vendor = dir.path().join("vendor");
    std::fs::create_dir(&vendor).unwrap();
    std::fs::write(vendor.join("lib.md"), "這個軟件").unwrap();
    std::fs::write(dir.path().join("main.md"), "這個軟件").unwrap();

    let output = run_lint_args(&[
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "--exclude",
        "vendor/**",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // Only main.md should survive (single file = object).
    assert!(
        parsed.get("file").is_some(),
        "should find only 1 file (vendor excluded)"
    );
}

#[test]
fn cli_lint_directory_deterministic_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("z.md"), "軟件").unwrap();
    std::fs::write(dir.path().join("a.md"), "軟件").unwrap();
    std::fs::write(dir.path().join("m.md"), "軟件").unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("multi-file JSON is array");
    let files: Vec<&str> = arr.iter().filter_map(|v| v["file"].as_str()).collect();
    assert_eq!(files.len(), 3);
    // Files should be sorted lexicographically (canonical paths).
    let mut sorted = files.clone();
    sorted.sort();
    assert_eq!(
        files, sorted,
        "output must be in deterministic sorted order"
    );
}

#[test]
fn cli_lint_directory_aggregate_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "軟件").unwrap();
    std::fs::write(dir.path().join("b.md"), "軟件").unwrap();

    // With --max-warnings 0, any warning fails.
    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--max-warnings", "0"]);
    assert!(
        !output.status.success(),
        "aggregate warnings should cause exit 1"
    );
}

#[test]
fn cli_lint_directory_only_supported_extensions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("code.rs"), "這個軟件").unwrap();
    std::fs::write(dir.path().join("data.json"), "這個軟件").unwrap();
    std::fs::write(dir.path().join("doc.md"), "正確").unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // Only doc.md should be found.
    assert!(
        parsed.get("file").is_some(),
        "should only scan supported extensions"
    );
}

#[test]
fn cli_lint_multiple_file_args() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "軟件").unwrap();
    std::fs::write(dir.path().join("b.md"), "軟件").unwrap();

    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    let output = run_lint_args(&[a.to_str().unwrap(), b.to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("multi-file JSON is array");
    assert_eq!(arr.len(), 2, "two file args should produce two results");
}

#[test]
fn cli_lint_config_file_applied() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.md"), "軟件").unwrap();
    // Config sets max_warnings=0, so even one warning should fail.
    std::fs::write(dir.path().join(".zhtw-mcp.toml"), "max_warnings = 0\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .current_dir(dir.path())
        .args(["lint", "test.md"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "config max_warnings=0 should cause exit 1"
    );
}

#[test]
fn cli_lint_config_cli_overrides_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.md"), "軟件").unwrap();
    // Config sets max_warnings=0, but CLI overrides with max_warnings=100.
    std::fs::write(dir.path().join(".zhtw-mcp.toml"), "max_warnings = 0\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .current_dir(dir.path())
        .args(["lint", "test.md", "--max-warnings", "100"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI --max-warnings should override config"
    );
}

#[test]
fn cli_lint_fix_rewrites_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.md");
    std::fs::write(&file, "這個軟件很好用").unwrap();

    let output = run_lint_args(&[file.to_str().unwrap(), "--fix"]);
    assert!(output.status.success(), "fix should exit 0");
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("軟體"),
        "file should be rewritten with fix: {content}"
    );
    assert!(
        !content.contains("軟件"),
        "original term should be gone: {content}"
    );
}

#[test]
fn cli_lint_fix_dry_run_no_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.md");
    std::fs::write(&file, "這個軟件很好用").unwrap();

    let output = run_lint_args(&[file.to_str().unwrap(), "--fix", "--dry-run"]);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("軟件"),
        "dry run should NOT rewrite: {content}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dry run"), "should mention dry run");
}

#[test]
fn cli_lint_fix_stdin_to_stdout() {
    let output = run_lint_stdin(&["--fix"], "這個軟件很好用");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("軟體"),
        "stdout should contain fixed text: {stdout}"
    );
}

#[test]
fn cli_lint_fix_round_trip() {
    // Fix, then re-lint: should find 0 fixable issues.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.md");
    std::fs::write(&file, "這個軟件的內存很大").unwrap();

    run_lint_args(&[file.to_str().unwrap(), "--fix"]);

    // Re-lint in JSON to check issues.
    let output = run_lint_args(&[file.to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let total = parsed["total"].as_u64().unwrap_or(0);
    assert_eq!(total, 0, "re-lint after fix should find 0 issues");
}

#[test]
fn cli_lint_sarif_output() {
    let output = run_lint_stdin(&["--format", "sarif"], "這個軟件很好用");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid SARIF JSON");
    assert_eq!(parsed["version"], "2.1.0");
    let runs = parsed["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    let results = runs[0]["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should have SARIF results");
    assert!(
        results[0]["ruleId"]
            .as_str()
            .unwrap()
            .starts_with("zhtw-mcp/"),
        "ruleId should be namespaced"
    );
    assert!(
        results[0]["locations"][0]["physicalLocation"]["region"]["startLine"]
            .as_u64()
            .is_some(),
        "should have line number"
    );
}

#[test]
fn cli_lint_explain_shows_context() {
    let output = run_lint_stdin(&["--explain"], "這個軟件很好用");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("english:") || stderr.contains("context:"),
        "explain should show context/english fields"
    );
}

#[test]
fn cli_lint_baseline_update_and_filter() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.md");
    let baseline = dir.path().join("baseline.json");
    std::fs::write(&file, "這個軟件很好用").unwrap();

    // Step 1: Generate baseline.
    let output = run_lint_args(&[
        file.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--update-baseline",
    ]);
    assert!(output.status.success());
    assert!(baseline.exists(), "baseline file should be created");

    // Step 2: Lint with baseline - issues should be suppressed.
    let output = run_lint_args(&[
        file.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--max-warnings",
        "0",
    ]);
    assert!(
        output.status.success(),
        "baselined issues should not count against max-warnings"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("baseline") || stderr.contains("suppressed"),
        "should mention baseline suppression"
    );
}

#[test]
fn cli_lint_human_format_multi_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "軟件").unwrap();
    std::fs::write(dir.path().join("b.md"), "正確").unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Multi-file human format prefixes each line with the filename.
    assert!(
        stderr.contains("a.md:") || stderr.contains("/a.md:"),
        "multi-file human format should include filename prefix"
    );
}

// -- Compact format tests (33.1) -------------------------------------------

#[test]
fn cli_lint_compact_format_single_issue() {
    // Single issue: file:line:col:S:rule:from→to
    let output = run_lint_stdin(&["--format", "compact"], "這個軟件很好用");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should have single-letter severity and arrow.
    assert!(
        stdout.contains(":W:") || stdout.contains(":E:"),
        "compact should use single-letter severity: {stdout}"
    );
    assert!(
        stdout.contains('\u{2192}'),
        "compact should use → arrow: {stdout}"
    );
    // No ANSI escape codes.
    assert!(
        !stdout.contains("\x1b["),
        "compact must not contain ANSI codes: {stdout}"
    );
}

#[test]
fn cli_lint_compact_format_clean_is_empty() {
    let output = run_lint_stdin(&["--format", "compact"], "這是正確的繁體中文。");
    assert!(output.status.success(), "clean compact lint should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "compact on clean text should emit nothing"
    );
}

#[test]
fn cli_lint_compact_format_dedup() {
    // 5 identical 視頻 issues should deduplicate to one line with ×N.
    // 視頻 is confusable and needs a context clue (e.g. 平台) to fire.
    let output = run_lint_stdin(
        &["--format", "compact"],
        "平台上的視頻、視頻、視頻、視頻、視頻",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\u{00d7}") || stdout.contains("×"),
        "repeated issues should deduplicate with × marker: {stdout}"
    );
    // Should be a single line (deduped).
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "5 identical issues should collapse to 1 line"
    );
}

#[test]
fn cli_lint_compact_format_suggestion_plus_n() {
    // 視頻 has 3 suggestions: 影片, 影音, 視訊 → compact shows 影片+2
    // 視頻 is confusable and needs a context clue (e.g. 串流) to fire.
    let output = run_lint_stdin(&["--format", "compact"], "串流視頻");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("+2"),
        "compact should show +N for alternatives: {stdout}"
    );
}

#[test]
fn cli_lint_compact_format_no_file_prefix_stdin() {
    // Stdin should omit file prefix.
    let output = run_lint_stdin(&["--format", "compact"], "這個軟件");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "expected compact output for input containing an issue"
    );
    // Line should start with a digit (line number), not a path.
    for line in stdout.lines() {
        assert!(
            line.starts_with(|c: char| c.is_ascii_digit()),
            "stdin compact should start with line number: {line}"
        );
    }
}

#[test]
fn cli_lint_compact_format_includes_path_single_file() {
    // Single-file compact output must include the filename for grep compatibility.
    // Run with current_dir set to the tempdir so strip_prefix relativization is exercised.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, "這個軟件").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zhtw-mcp"))
        .args(["lint", "test.txt", "--format", "compact"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "expected compact output for input containing an issue"
    );
    for line in stdout.lines() {
        assert!(
            line.starts_with("test.txt:"),
            "single-file compact must start with filename: {line}"
        );
    }
}

#[test]
fn cli_lint_compact_token_reduction_vs_human() {
    // Gate: ≥40% token reduction vs human default.
    // Approximate tokens by character count (reasonable proxy for CJK+ASCII mix).
    let input = "這個軟件使用了串流視頻功能，串流視頻品質不錯。並行計算很快。";
    let human_output = run_lint_stdin(&["--format", "human"], input);
    let compact_output = run_lint_stdin(&["--format", "compact"], input);
    let human_len = String::from_utf8_lossy(&human_output.stderr).len();
    let compact_len = String::from_utf8_lossy(&compact_output.stdout).len();
    assert!(human_len > 0, "human output should be non-empty");
    assert!(compact_len > 0, "compact output should be non-empty");
    let reduction = 1.0 - (compact_len as f64 / human_len as f64);
    assert!(
        reduction >= 0.40,
        "compact should achieve ≥40% reduction vs human: human={human_len} compact={compact_len} reduction={reduction:.2}"
    );
}

// Grammar scanner: plumbing gate tests

// Input that triggers grammar issues (A-not-A + 嗎 clash).
const GRAMMAR_INPUT: &str = "你是不是學生嗎？";

#[test]
fn cli_lint_grammar_json_format() {
    let output = run_lint_stdin(&["--format", "json"], GRAMMAR_INPUT);
    assert!(
        output.status.success(),
        "grammar warnings should not cause non-zero exit"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    let grammar = issues.iter().find(|i| i["rule_type"] == "grammar");
    assert!(
        grammar.is_some(),
        "JSON should contain grammar issue: {stdout}"
    );
    let g = grammar.unwrap();
    assert!(
        g["found"].as_str().unwrap().contains("是不是"),
        "found should contain pattern"
    );
    assert!(g["line"].as_u64().unwrap() > 0, "should have line number");
}

#[test]
fn cli_lint_grammar_sarif_format() {
    let output = run_lint_stdin(&["--format", "sarif"], GRAMMAR_INPUT);
    assert!(
        output.status.success(),
        "grammar warnings should not cause non-zero exit"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid SARIF JSON");
    let results = parsed["runs"][0]["results"].as_array().unwrap();
    let grammar = results
        .iter()
        .find(|r| r["ruleId"].as_str().unwrap() == "zhtw-mcp/grammar");
    assert!(
        grammar.is_some(),
        "SARIF should have zhtw-mcp/grammar ruleId: {stdout}"
    );
    let g = grammar.unwrap();
    assert!(
        g["locations"][0]["physicalLocation"]["region"]["startLine"]
            .as_u64()
            .is_some(),
        "SARIF grammar result should have startLine"
    );
}

#[test]
fn cli_lint_grammar_compact_format() {
    let output = run_lint_stdin(&["--format", "compact"], GRAMMAR_INPUT);
    assert!(
        output.status.success(),
        "grammar warnings should not cause non-zero exit"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "compact output should be non-empty for grammar issue"
    );
    // Compact format: line:col:S:rule:found→suggestion
    assert!(
        stdout.contains(":grammar:"),
        "compact should contain :grammar: rule field: {stdout}"
    );
}

#[test]
fn cli_lint_grammar_human_format() {
    let output = run_lint_stdin(&["--format", "human"], GRAMMAR_INPUT);
    assert!(
        output.status.success(),
        "grammar warnings should not cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[grammar]"),
        "human output should show [grammar] bracketed rule type: {stderr}"
    );
}

#[test]
fn cli_lint_grammar_explain_format() {
    let output = run_lint_stdin(&["--explain"], GRAMMAR_INPUT);
    assert!(
        output.status.success(),
        "grammar warnings should not cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[grammar]"),
        "explain should show [grammar] rule type: {stderr}"
    );
    assert!(
        stderr.contains("A-not-A"),
        "explain should show A-not-A explanation: {stderr}"
    );
}

#[test]
fn cli_lint_grammar_does_not_suppress_spelling() {
    // Grammar issues run after overlap resolution, so a text with both
    // a spelling issue and a grammar issue should report both.
    // 軟件 triggers a spelling issue; 是不是…嗎 triggers grammar.
    let input = "你是不是喜歡這個軟件嗎？";
    let output = run_lint_stdin(&["--format", "json"], input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    let has_grammar = issues.iter().any(|i| i["rule_type"] == "grammar");
    let has_spelling = issues
        .iter()
        .any(|i| i["rule_type"] == "cross_strait" || i["rule_type"] == "confusable");
    assert!(has_grammar, "should have grammar issue: {stdout}");
    assert!(
        has_spelling,
        "grammar should not suppress spelling issues: {stdout}"
    );
}

#[test]
fn cli_lint_grammar_disabled_in_ui_strings_profile() {
    // UiStrings profile has grammar_checks: false.
    // Use input with both a grammar pattern and a spelling issue (軟件)
    // to prove grammar is selectively disabled, not that all issues vanish.
    let input = "你是不是喜歡這個軟件嗎？";
    let output = run_lint_stdin(&["--format", "json", "--profile", "ui_strings"], input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["rule_type"] != "grammar"),
        "ui_strings should still produce non-grammar issues: {stdout}"
    );
    assert!(
        !issues.iter().any(|i| i["rule_type"] == "grammar"),
        "ui_strings profile should not produce grammar issues: {stdout}"
    );
}

#[test]
fn cli_lint_fix_bogus_rejected() {
    let output = run_lint_args(&["--fix=bogus", "dummy.txt"]);
    assert!(
        !output.status.success(),
        "--fix=bogus should fail with non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown fix mode"),
        "should report unknown fix mode, got: {stderr}"
    );
}

#[test]
fn cli_lint_detect_ai_enables_density_detection() {
    // Build text with high density of a tracked phrase.
    let filler = "這是正常的技術內容段落。";
    let mut text = String::new();
    for i in 0..100 {
        if i % 20 == 0 {
            text.push_str("更重要的是，我們需要重新評估這個方案。");
        } else {
            text.push_str(filler);
        }
    }
    // Without --detect-ai (default profile): no ai_style density issues.
    let output_default = run_lint_stdin(&["--format", "json"], &text);
    let stdout = String::from_utf8_lossy(&output_default.stdout);
    let json_default: serde_json::Value =
        serde_json::from_str(&stdout).expect("default output should be valid JSON");
    let has_ai_density_default = json_default["issues"].as_array().is_some_and(|arr| {
        arr.iter().any(|i| {
            i["rule_type"] == "ai_style"
                && i["context"].as_str().is_some_and(|c| c.contains("次/千字"))
        })
    });
    assert!(
        !has_ai_density_default,
        "default profile should not report ai_style density issues: {stdout}"
    );

    // With --detect-ai: ai_style density issues should appear.
    let output_ai = run_lint_stdin(&["--detect-ai", "--format", "json"], &text);
    let stdout_ai = String::from_utf8_lossy(&output_ai.stdout);
    let json_ai: serde_json::Value =
        serde_json::from_str(&stdout_ai).expect("--detect-ai output should be valid JSON");
    let has_ai_density = json_ai["issues"].as_array().is_some_and(|arr| {
        arr.iter().any(|i| {
            i["rule_type"] == "ai_style"
                && i["context"].as_str().is_some_and(|c| c.contains("次/千字"))
        })
    });
    assert!(
        has_ai_density,
        "--detect-ai should report ai_style density issues: {stdout_ai}"
    );
}
