// Integration tests for the CLI lint subcommand.
//
// Tests exit codes, output formats, profile selection, content-type handling,
// max-errors gating, max-warnings gating, and multi-file/directory linting.

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Path to the binary under test.
///
/// `CARGO_BIN_EXE_<name>` is set by cargo for every integration test and
/// carries the platform's executable suffix.  Deriving it from
/// `current_exe()` instead, which every one of these test files used to do,
/// dropped the `.exe` on Windows and left a path that does not exist.
fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_zhtw-mcp"))
}

fn run_lint_stdin(extra_args: &[&str], input: &str) -> Output {
    let bin = binary_path();
    Command::new(&bin)
        .args(["lint", "--"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RUST_LOG")
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
fn cli_lint_json_clean_default_has_empty_stderr() {
    let output = run_lint_stdin(&["--format", "json"], "正確的軟體");
    assert!(output.status.success(), "clean JSON lint should exit 0");
    assert!(
        output.stderr.is_empty(),
        "default tracing should not write stderr on clean JSON runs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_lint_rust_log_debug_emits_scan_trace() {
    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", "--", "--format", "json"])
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all("正確的軟體".as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success(), "debug lint should exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scan") && stderr.contains("elapsed_ms"),
        "debug tracing should include scan timing fields: {stderr}"
    );
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
fn cli_lint_telemetry_summary_on_stderr() {
    let output = run_lint_stdin(&["--telemetry"], "這個軟件很好用");
    assert!(output.status.success(), "warnings-only should still exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[telemetry] files=1 total_issues=1 errors=0 warnings=1"),
        "stderr should include telemetry summary: {stderr}"
    );
}

#[test]
fn cli_lint_profile_strict() {
    // 裏 is a variant only flagged under strict profile
    let output = run_lint_stdin(&["--format", "json", "--profile", "strict"], "裏面");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["found"] == "裏"),
        "strict should flag 裏 variant"
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
    // --content-type plain must beat the .md extension. A 4-space-indented line
    // is an indented code block in Markdown and ordinary prose in plain text,
    // so the same file reports differently under each.
    let dir = tempfile::tempdir().unwrap();
    let md_file = dir.path().join("test.md");
    std::fs::write(&md_file, "    這個軟件很好用\n").unwrap();

    let count = |args: &[&str]| -> u64 {
        let output = run_lint_args(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
        parsed["total"].as_u64().unwrap()
    };

    let path = md_file.to_str().unwrap();
    assert_eq!(
        count(&[path, "--format", "json"]),
        0,
        "as markdown, the indented line is a code block"
    );
    assert_eq!(
        count(&[path, "--format", "json", "--content-type", "plain"]),
        1,
        "--content-type plain must override the .md extension"
    );
}

// -- max_warnings tests ----------------------------------------------------

#[test]
fn cli_lint_unreadable_file_skips_rather_than_aborting() {
    // One file that is not UTF-8 must not discard the findings for its
    // neighbours. Sorted last on purpose: in JSON mode the array is emitted
    // after the loop, so an abort here used to throw away work already done.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "這個軟件很好用\n").unwrap();
    std::fs::write(dir.path().join("b.md"), "這個內存很大\n").unwrap();
    std::fs::write(dir.path().join("zz.md"), [0xff, 0xfe, 0x00, 0x28]).unwrap();

    let output = run_lint_args(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let files = parsed.as_array().expect("multi-file JSON array");
    assert_eq!(files.len(), 2, "both readable files should be reported");
    assert!(
        files.iter().all(|f| f["total"].as_u64().unwrap() > 0),
        "readable files should still carry their issues: {stdout}"
    );

    assert_eq!(
        output.status.code(),
        Some(2),
        "an unprocessable file is an operational failure, not a gate result"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("zz.md"),
        "the skipped file must be named: {stderr}"
    );
}

#[test]
fn cli_lint_exit_codes_separate_gate_from_failure() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.md");
    std::fs::write(&file, "這個軟件很好用\n").unwrap();
    let path = file.to_str().unwrap();

    assert_eq!(
        run_lint_args(&[path]).status.code(),
        Some(0),
        "warnings within budget exit 0"
    );
    assert_eq!(
        run_lint_args(&[path, "--max-warnings", "0"]).status.code(),
        Some(1),
        "a gate violation is exit 1"
    );
    assert_eq!(
        run_lint_args(&["/nonexistent/nope.md"]).status.code(),
        Some(2),
        "a missing file is exit 2"
    );
    assert_eq!(
        run_bin_args(&["--config", "/nonexistent/nope.toml", "lint", path])
            .status
            .code(),
        Some(2),
        "an unreadable config is exit 2"
    );
    assert_eq!(
        run_lint_args(&["--fix=bogus", path]).status.code(),
        Some(2),
        "an invalid flag value is exit 2"
    );
}

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
    // Both thresholds must pass for exit 0. "軟件" emits 1 warning. With
    // --max-errors 100 --max-warnings 0 → exit 1.
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

// -- Multi-file / directory linting tests -----------------------------------

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
fn cli_lint_diff_from_with_no_changes_succeeds() {
    let output = run_lint_args(&["--diff-from", "HEAD"]);
    assert!(
        output.status.success(),
        "an empty diff should be a clean lint batch: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_lint_cache_hit_preserves_tier2_disambiguation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ambiguous.txt");
    std::fs::write(&file, "學習的進程需要耐心和毅力。\n").unwrap();

    // The fast cache path only trusts files at least one second old.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let run = || {
        Command::new(binary_path())
            .args(["lint", file.to_str().unwrap(), "--format", "json"])
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    };
    let first = run();
    let second = run();
    let issue = |output: &Output| {
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|issue| issue["found"] == "進程")
            .cloned()
            .unwrap()
    };

    assert_eq!(
        issue(&second),
        issue(&first),
        "cache hit changed tier-2 output"
    );
}

fn run_bin_args(args: &[&str]) -> Output {
    let bin = binary_path();
    Command::new(&bin)
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
fn cli_cache_clear_rejects_trailing_args() {
    let output = run_bin_args(&["cache", "clear", "unexpected"]);
    assert!(
        !output.status.success(),
        "cache clear with trailing args should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cache clear does not accept additional arguments"),
        "stderr should explain invalid trailing args: {stderr}"
    );
}

#[test]
fn cli_server_explicit_missing_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    let missing = missing.to_str().unwrap();

    let output = run_bin_args(&["--config", missing]);
    assert!(
        !output.status.success(),
        "server should fail on bad --config"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("read config {missing}")),
        "stderr should report config read error: {stderr}"
    );
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
fn cli_lint_config_ignore_terms_downgrades_to_info() {
    // ignore_terms in .zhtw-mcp.toml keeps the term visible but drops it to
    // Info, so it stops failing the warning gate. Same semantics as the MCP
    // tool's ignore_terms argument.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join(".zhtw-mcp.toml");
    let file = dir.path().join("t.md");
    std::fs::write(&file, "這個軟件很好用\n").unwrap();

    let run = |cfg_body: &str| -> serde_json::Value {
        std::fs::write(&cfg, cfg_body).unwrap();
        let output = run_bin_args(&[
            "--config",
            cfg.to_str().unwrap(),
            "lint",
            file.to_str().unwrap(),
            "--format",
            "json",
        ]);
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("valid JSON")
    };

    // Control: the same config without ignore_terms, proving the file is read
    // and that 軟件 is a warning by default.
    let baseline = run("max_warnings = 10\n");
    assert_eq!(baseline["total"].as_u64().unwrap(), 1);
    assert_eq!(baseline["warnings"].as_u64().unwrap(), 1);

    let ignored = run("max_warnings = 10\nignore_terms = [\"軟件\"]\n");
    assert_eq!(
        ignored["total"].as_u64().unwrap(),
        1,
        "ignored terms stay visible"
    );
    assert_eq!(
        ignored["warnings"].as_u64().unwrap(),
        0,
        "ignored term must not count as a warning: {ignored}"
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

/// Install a single-rule pack under "dir" and return the packs directory to
/// pass to "--packs-dir". Tests that need a rule with a specific shape define
/// it here rather than leaning on whichever shipped term currently has that
/// shape: re-classifying a term in assets/ruleset.json is a routine editorial
/// call and must not break unrelated fixer tests.
fn write_pack(dir: &std::path::Path, name: &str, rule: serde_json::Value) -> String {
    let packs = dir.join("packs");
    std::fs::create_dir_all(&packs).unwrap();
    let pack = serde_json::json!({
        "schema_version": 3,
        "metadata": { "name": name },
        "spelling": [rule],
        "case": [],
    });
    std::fs::write(
        packs.join(format!("{name}.json")),
        serde_json::to_string(&pack).unwrap(),
    )
    .unwrap();
    packs.to_str().unwrap().to_string()
}

#[test]
fn cli_lint_lexical_safe_skips_low_editorial_confidence() {
    // 軟件 is a plain cross-strait term the safe tier always rewrites. 測試詞
    // is a synthetic pack rule annotated editorial_confidence low with a single
    // suggestion and no clues, the exact shape the gate targets; no shipped
    // rule currently has that shape. Each tier runs against its own pristine
    // copy so the contextual case proves it handles the original input in one
    // pass rather than inheriting the safe pass's rewrite.
    let dir = tempfile::tempdir().unwrap();
    let packs = write_pack(
        dir.path(),
        "ecgate",
        serde_json::json!({
            "from": "測試詞",
            "to": ["替換詞"],
            "type": "cross_strait",
            "english": "test term",
            "editorial_confidence": "low",
        }),
    );

    let source = "這個軟件需要測試詞";
    let safe_file = dir.path().join("safe.md");
    let contextual_file = dir.path().join("contextual.md");
    std::fs::write(&safe_file, source).unwrap();
    std::fs::write(&contextual_file, source).unwrap();

    let output = run_bin_args(&[
        "--packs-dir",
        &packs,
        "--pack",
        "ecgate",
        "lint",
        safe_file.to_str().unwrap(),
        "--fix=lexical_safe",
        "--format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "lexical_safe fix should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(&safe_file).unwrap();
    assert!(
        content.contains("軟體需要測試詞"),
        "lexical_safe should fix safe terms but leave low-confidence terms: {content}"
    );

    // Counts, not just content: pins that the term was declined by the gate
    // rather than never reported at all.
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("valid JSON");
    assert_eq!(parsed["fixes_applied"], 1, "only 軟件 should be rewritten");
    assert_eq!(parsed["fixes_skipped"], 1, "測試詞 should be skipped");
    assert_eq!(
        parsed["fixes_declined"], 1,
        "and skipped on its merits, not for being out of tier"
    );

    let output = run_bin_args(&[
        "--packs-dir",
        &packs,
        "--pack",
        "ecgate",
        "lint",
        contextual_file.to_str().unwrap(),
        "--fix=lexical_contextual",
    ]);
    assert!(
        output.status.success(),
        "lexical_contextual fix should exit 0"
    );
    let content = std::fs::read_to_string(&contextual_file).unwrap();
    assert!(
        content.contains("軟體需要替換詞"),
        "lexical_contextual should fix low-confidence terms: {content}"
    );
}

#[test]
fn cli_lint_reports_declined_fixes_in_human_output() {
    // A file whose only issue is declined used to print no fix line at all,
    // leaving the run indistinguishable from one without --fix. The pack rule
    // carries two suggestions and no editorial_confidence, so the safe tier
    // declines it at suggestion selection: the count must cover every gate, not
    // just the confidence one.
    let dir = tempfile::tempdir().unwrap();
    let packs = write_pack(
        dir.path(),
        "multisug",
        serde_json::json!({
            "from": "測試詞",
            "to": ["替換甲", "替換乙"],
            "type": "cross_strait",
            "english": "test term",
        }),
    );
    let file = dir.path().join("declined.md");
    std::fs::write(&file, "這樣會測試詞").unwrap();

    let output = run_bin_args(&[
        "--packs-dir",
        &packs,
        "--pack",
        "multisug",
        "lint",
        file.to_str().unwrap(),
        "--fix=lexical_safe",
    ]);
    assert!(output.status.success(), "declined-only fix should exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no fixes applied") && stderr.contains("declined"),
        "human output should report the declined count: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "這樣會測試詞",
        "declined fix must not rewrite the file"
    );

    // A clean fix run must not grow a declined clause.
    let clean = dir.path().join("clean.md");
    std::fs::write(&clean, "這個軟件很好").unwrap();
    let output = run_lint_args(&[clean.to_str().unwrap(), "--fix=lexical_safe"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 fix(es) applied") && !stderr.contains("declined"),
        "no declined clause when nothing was declined: {stderr}"
    );
}

#[test]
fn cli_lint_fix_preserves_markdown_structure() {
    // --fix must not rewrite bytes inside YAML frontmatter delimiters, fenced
    // code, inline code, URLs or HTML tags, matching the MCP fix path.
    //
    // Scan-time exclusion already carries most of that: no issue is emitted in
    // those regions, so they survive even with an empty fixer mask. The line
    // that needs the mask is the fronted-object clause: the grammar match spans
    // the inline code sitting between its two parts, and the whole clause must
    // be left alone rather than half-rewritten.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.md");
    let src = "---\ntitle: \"測試\"\nlang: zh-TW\n---\n\n我們對`x`進行處理，這個軟件很好用。\n\n\
        ```rust\nlet quality = \"這個軟件的內存\";\n```\n\n\
        <span data-note=\"軟件\">HTML</span>\n\nhttps://example.com/軟件/內存.html\n";
    std::fs::write(&file, src).unwrap();

    let output = run_lint_args(&[file.to_str().unwrap(), "--fix=lexical_contextual"]);
    assert!(output.status.success(), "fix should exit 0");
    let out = std::fs::read_to_string(&file).unwrap();

    for preserved in [
        "我們對`x`進行處理，",
        "---\ntitle: \"測試\"\nlang: zh-TW\n---",
        "let quality = \"這個軟件的內存\";",
        "<span data-note=\"軟件\">HTML</span>",
        "https://example.com/軟件/內存.html",
    ] {
        assert!(
            out.contains(preserved),
            "--fix rewrote protected structure {preserved:?}: {out}"
        );
    }

    // Two rules match that clause: the outer one crosses the mask and would
    // relocate the code span, and the inner one does not cross it but would
    // strip 進行 and leave the fronted 對 dangling. Declining the outer span
    // declines both.
    assert!(
        !out.contains("處理`x`"),
        "--fix applied a fix spanning the inline-code mask: {out}"
    );
    assert!(
        out.contains("軟體很好用"),
        "prose should still be fixed: {out}"
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
fn cli_lint_fix_stdin_passes_through_unchanged_text() {
    // With --fix, stdin is a filter, so the document has to reach stdout even
    // when nothing changed. Gating the passthrough on "something was fixed"
    // made "lint -- --fix > out.md" truncate a clean document, on the one input
    // with no copy on disk to recover from.
    let clean = "這是正確的中文。";
    let output = run_lint_stdin(&["--fix"], clean);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        clean,
        "clean input must survive the filter"
    );

    // A document whose only issue is declined is the same case: nothing is
    // written, so nothing must be lost.
    let ambiguous = "這個視頻很好看";
    let output = run_lint_stdin(&["--fix"], ambiguous);
    assert_eq!(String::from_utf8_lossy(&output.stdout), ambiguous);

    // --dry-run still writes nothing: it reports, it does not filter.
    let output = run_lint_stdin(&["--fix", "--dry-run"], clean);
    assert!(
        output.stdout.is_empty(),
        "dry run must not emit the document"
    );

    // Machine formats own stdout, so the document must not be prepended to
    // their report. Both inputs are checked: the clean one covers the
    // passthrough added here, the dirty one the older write it replaced.
    for input in [clean, "這個軟件很好用"] {
        for fmt in ["json", "sarif"] {
            let output = run_lint_stdin(&["--fix", "--format", fmt], input);
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
                panic!("--format {fmt} on {input:?} must parse: {e}: {stdout}")
            });
        }
    }
}

#[test]
fn cli_lint_stdin_emits_s2t_converted_text_without_fix() {
    // S2T conversion rewrites the document whether or not --fix was passed, and
    // the file path writes that rewrite back unconditionally. stdin has no copy
    // on disk, so withholding it there loses the converted text: "lint -- <
    // cn.md > out.md" produced an empty file.
    let simplified = "这个软件很好用。";
    let output = run_lint_stdin(&[], simplified);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("這個") && !stdout.is_empty(),
        "converted document must reach stdout: {stdout:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("S2T only"),
        "status line still belongs on stderr"
    );

    // Traditional input is unchanged, so there is nothing to emit and stdout
    // stays free for a report.
    let output = run_lint_stdin(&[], "這個軟體很好用。");
    assert!(
        output.stdout.is_empty(),
        "no rewrite means no document on stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    // A machine format owns stdout, so the conversion is dropped and said so.
    let output = run_lint_stdin(&["--format", "json"], simplified);
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("--format json must parse: {e}: {stdout}"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not emitted"),
        "the discard must be reported, not silent"
    );

    // --dry-run reports without filtering, exactly as it does for a file.
    let output = run_lint_stdin(&["--dry-run"], simplified);
    assert!(
        output.stdout.is_empty(),
        "dry run must not emit the document"
    );
}

#[test]
fn cli_lint_declined_excludes_out_of_tier_issues() {
    // Every issue here is lexical, so --fix=orthographic leaves them all alone,
    // but it never weighed any of them. Reporting seven declines would read as
    // seven verdicts.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.md");
    let source = "這個軟件的內存和硬盤都不夠。";
    std::fs::write(&file, source).unwrap();

    let output = run_bin_args(&[
        "lint",
        file.to_str().unwrap(),
        "--fix=orthographic",
        "--format",
        "json",
    ]);
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("valid JSON");
    assert!(
        parsed["fixes_skipped"].as_u64().unwrap() >= 3,
        "the lexical issues are still skipped"
    );
    assert!(
        parsed["fixes_declined"].is_null() || parsed["fixes_declined"] == 0,
        "out-of-tier issues are not declines: {}",
        parsed["fixes_declined"]
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), source);

    // The run above asks for JSON, so its stderr never carries the human fix
    // summary and asserting on it would pass no matter what the code did.
    // Exercise the human path on its own.
    let output = run_lint_args(&[file.to_str().unwrap(), "--fix=orthographic"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("declined"),
        "human output must not claim declines either: {stderr}"
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

    // Consumers render informationUri as the tool's home link and validate it
    // as a URI. Two independent properties, because equality alone would be
    // tautological (Cargo defines CARGO_PKG_REPOSITORY as "" when the manifest
    // drops the key, so both sides would go empty together) and a shape check
    // alone would accept any plausible-looking wrong URL.
    let repo = env!("CARGO_PKG_REPOSITORY");
    assert!(
        repo.starts_with("https://"),
        "Cargo.toml must declare a repository URL, got {repo:?}"
    );
    let info_uri = runs[0]["tool"]["driver"]["informationUri"]
        .as_str()
        .expect("informationUri present");
    assert_eq!(info_uri, repo, "SARIF informationUri must track Cargo.toml");
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

// -- Compact format tests --------------------------------------------------

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
    // 5 identical 視頻 issues should deduplicate to one line with ×N. 視頻 is
    // confusable and needs a context clue (e.g. 平台) to fire.
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
    // 視頻 has 3 suggestions: 影片, 影音, 視訊 → compact shows 影片+2 視頻 is
    // confusable and needs a context clue (e.g. 串流) to fire.
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
    // Single-file compact output must include the filename for grep
    // compatibility. Run with current_dir set to the tempdir so strip_prefix
    // relativization is exercised.
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
    // Gate: ≥40% token reduction vs human default. Approximate tokens by
    // character count (reasonable proxy for CJK+ASCII mix).
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
    // Grammar issues run after overlap resolution, so a text with both a
    // spelling issue and a grammar issue should report both. 軟件 triggers a
    // spelling issue; 是不是…嗎 triggers grammar.
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
fn cli_lint_grammar_disabled_with_relaxed() {
    // --relaxed disables grammar_checks. Use input with both a grammar pattern
    // and a spelling issue (軟件) to prove grammar is selectively disabled, not
    // that all issues vanish.
    let input = "你是不是喜歡這個軟件嗎？";
    let output = run_lint_stdin(&["--format", "json", "--relaxed"], input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["rule_type"] != "grammar"),
        "relaxed should still produce non-grammar issues: {stdout}"
    );
    assert!(
        !issues.iter().any(|i| i["rule_type"] == "grammar"),
        "relaxed should not produce grammar issues: {stdout}"
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
    // Without --detect-ai (base profile): no ai_style density issues.
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
        "base profile should not report ai_style density issues: {stdout}"
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

#[test]
fn cli_lint_detect_style_emits_three_axis_scorecard() {
    // --detect-style produces a three-axis scorecard (ai / translationese /
    // regional_density). All three axes are reported side by side and never
    // collapsed into a single number.
    let text = "策略的實施帶來了效率的提升。實際上基本上每個人都同意。\
                這是 20 世紀最重要的發現之一。當我抵達公司的時候，他已經在開會了。";
    let output = run_lint_stdin(&["--detect-style", "--format", "json"], text);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("--detect-style emits valid JSON");

    let scorecard = json
        .get("style_scorecard")
        .expect("style_scorecard present with --detect-style");
    let scores = scorecard
        .get("style_scores")
        .expect("style_scorecard.style_scores present");
    // Three orthogonal axes: at least one of the three carries a score.
    let has_any = ["ai", "translationese", "regional_density"]
        .iter()
        .any(|axis| scores.get(*axis).is_some());
    assert!(has_any, "scorecard must emit at least one axis");

    // Three scores are reported as separate fields: not combined.
    let ai = scores.get("ai");
    let trans = scores.get("translationese");
    let regional_density = scores.get("regional_density");
    assert!(
        ai.is_some() || trans.is_some() || regional_density.is_some(),
        "axes reported individually, never collapsed"
    );
    // No top-level composite "score" / "overall" field.
    assert!(scores.get("score").is_none());
    assert!(scores.get("overall").is_none());

    // top_issues_per_axis present with three keys.
    let top = scorecard
        .get("top_issues_per_axis")
        .expect("top_issues_per_axis present");
    for axis in ["ai", "translationese", "regional_density"] {
        assert!(top.get(axis).is_some(), "top_issues_per_axis.{axis}");
    }
}

#[test]
fn cli_lint_default_format_omits_scorecard() {
    // Without --detect-style the scorecard is omitted entirely.
    let output = run_lint_stdin(&["--format", "json"], "正確的軟體");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("default JSON valid");
    assert!(
        json.get("style_scorecard").is_none(),
        "scorecard absent without --detect-style: {stdout}"
    );
}

#[test]
fn cli_lint_detect_style_preserves_translationese_axis_after_baseline_filtering() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("style.txt");
    let baseline = dir.path().join("baseline.json");
    let text = "這是 20 世紀最重要的發現之一。當我抵達公司的時候，他已經在開會了。".repeat(8);
    std::fs::write(&file, &text).unwrap();

    let update = run_lint_args(&[
        file.to_str().unwrap(),
        "--format",
        "json",
        "--detect-style",
        "--baseline",
        baseline.to_str().unwrap(),
        "--update-baseline",
    ]);
    assert!(update.status.success(), "baseline update should succeed");

    let output = run_lint_args(&[
        file.to_str().unwrap(),
        "--format",
        "json",
        "--detect-style",
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "baseline-filtered lint should succeed"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("baseline-filtered JSON output");
    assert_eq!(
        json["issues"].as_array().map(|issues| issues.len()),
        Some(0),
        "baseline should remove all visible issues: {stdout}"
    );
    assert_eq!(
        json["style_scorecard"]["top_issues_per_axis"]["translationese"]
            .as_array()
            .map(|issues| issues.len()),
        Some(0),
        "translationese top issues should match the filtered output: {stdout}"
    );
    let signature_score = json["translationese_signature"]["score"]
        .as_f64()
        .expect("translationese signature score present");
    let axis_score = json["style_scorecard"]["style_scores"]["translationese"]
        .as_f64()
        .expect("translationese axis present");
    assert!(
        signature_score > 0.0,
        "translationese signature should stay non-zero for this fixture: {stdout}"
    );
    assert_eq!(
        axis_score, signature_score,
        "document-level translationese axis should match the signature even when issues are filtered: {stdout}"
    );
}

#[test]
fn cli_lint_detect_style_preserves_regional_density_on_cache_hits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("style-cache.txt");
    std::fs::write(&file, "這個軟件的服務器內存不夠").unwrap();

    let first = run_lint_args(&[file.to_str().unwrap(), "--format", "json", "--detect-style"]);
    assert!(first.status.success(), "initial lint should succeed");

    let second = run_lint_args(&[file.to_str().unwrap(), "--format", "json", "--detect-style"]);
    assert!(second.status.success(), "cached lint should succeed");

    let first_stdout = String::from_utf8_lossy(&first.stdout);
    let first_json: serde_json::Value =
        serde_json::from_str(&first_stdout).expect("initial JSON output");
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let second_json: serde_json::Value =
        serde_json::from_str(&second_stdout).expect("cached JSON output");

    let first_regional_density = first_json["style_scorecard"]["style_scores"]["regional_density"]
        .as_f64()
        .expect("initial regional_density score present");
    let second_regional_density = second_json["style_scorecard"]["style_scores"]
        ["regional_density"]
        .as_f64()
        .expect("cached regional_density score present");

    assert!(
        first_regional_density > 0.0,
        "fixture should trigger a non-zero regional_density score: {first_stdout}"
    );
    assert_eq!(
        second_regional_density, first_regional_density,
        "cache hits must preserve the same regional_density score: {second_stdout}"
    );
}

#[test]
fn cli_lint_detect_style_requires_json_format() {
    let output = run_lint_stdin(&["--detect-style"], "正確的軟體");
    assert!(!output.status.success(), "human format should be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--detect-style is only supported with --format json"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn cli_lint_detect_style_uses_post_fix_text_length() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("style-fix.txt");
    let filler = "甲".repeat(1200);
    let text = format!("這個軟件很好用{}{}", "...".repeat(120), filler);
    std::fs::write(&file, &text).unwrap();

    let output = run_lint_args(&[
        file.to_str().unwrap(),
        "--format",
        "json",
        "--detect-style",
        "--fix=orthographic",
        "--max-errors",
        "100",
    ]);
    assert!(
        output.status.success(),
        "fix+scorecard run should complete with JSON output"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("post-fix JSON output");
    let final_text = std::fs::read_to_string(&file).unwrap();
    let expected = 1000.0 / (final_text.chars().count() as f64);
    let got = json["style_scorecard"]["style_scores"]["regional_density"]
        .as_f64()
        .expect("regional density present");
    assert!(
        (got - expected).abs() < 1e-6,
        "scorecard must use post-fix text length; expected {expected}, got {got}, stdout={stdout}"
    );
}

#[test]
fn cli_lint_translationese_suggested_rewrite_serialized() {
    let output = run_lint_stdin(
        &["--detect-translationese", "--format", "json"],
        "大家需要互相合作，也要仔細的看文件。",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("translationese JSON output");
    let issues = json["issues"].as_array().expect("issues array");
    let semantic_overlap = issues
        .iter()
        .find(|i| i["found"] == "互相合作")
        .expect("deferred translationese issue present");
    assert_eq!(semantic_overlap["suggested_rewrite"], "合作");
    let particle_mixup = issues
        .iter()
        .find(|i| i["found"] == "仔細的看")
        .expect("direct translationese issue present");
    assert_eq!(particle_mixup["suggested_rewrite"], "仔細地看");
}

#[test]
fn cli_lint_fix_never_deletes_an_attribution_from_native_prose() {
    // The regression this pins: the bare-attribution check once shipped an
    // empty-string suggestion, which is the fixer's delete sentinel, and it ran
    // under "grammar_checks" rather than the AI filter. A plain "lint --fix" on
    // ordinary Taiwanese reporting therefore rewrote
    // "多位專家認為，本次修法將影響地方財政。" into
    // "多位，本次修法將影響地方財政。"
    let native = "研究顯示台灣中小企業的數位轉型速度落後。多位專家認為，本次修法將影響地方財政。";

    // Without the AI filter the check must not run at all.
    let plain = run_lint_stdin(&["--format", "json"], native);
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&plain.stdout)).expect("JSON output");
    assert!(
        !json["issues"]
            .as_array()
            .expect("issues array")
            .iter()
            .any(|issue| issue["rule_type"] == "ai_style"),
        "an AI-only check fired under a plain lint: {json}"
    );

    // With it on, the finding appears but carries no mechanical edit.
    let ai = run_lint_stdin(&["--detect-ai", "--format", "json"], native);
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&ai.stdout)).expect("JSON output");
    let attributions: Vec<_> = json["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|issue| issue["found"] == "研究顯示" || issue["found"] == "專家認為")
        .collect();
    assert_eq!(attributions.len(), 2, "expected both attributions: {json}");
    for issue in attributions {
        let suggestions = issue["suggestions"].as_array().expect("suggestions array");
        assert!(
            suggestions.is_empty(),
            "a bare attribution offered an edit: {issue}"
        );
    }

    // The property the test is named for: run the fixer and require the text
    // back unchanged. Asserting on the suggestion list alone would miss a
    // deletion reintroduced through any other path.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("native.txt");
    std::fs::write(&path, native).expect("write");
    for args in [
        vec!["--fix"],
        vec!["--detect-ai", "--fix"],
        vec!["--detect-ai", "--fix=lexical_contextual"],
    ] {
        std::fs::write(&path, native).expect("rewrite");
        let p = path.to_string_lossy().into_owned();
        let mut argv = args.clone();
        argv.push(p.as_str());
        run_lint_args(&argv);
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, native, "--fix rewrote native prose with {args:?}");
    }
}

#[test]
fn cli_lint_ai_rewrite_hint_is_serialized() {
    let output = run_lint_stdin(
        &["--detect-ai", "--format", "json"],
        "這個函式被廣泛使用，因此改動前要檢查相容性。",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("AI JSON output");
    let issue = json["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .find(|issue| issue["found"] == "被廣泛使用")
        .expect("passive AI-style issue present");
    assert_eq!(issue["suggested_rewrite"], "廣泛使用");
}

#[test]
fn cli_lint_writing_humanizer_watchlist_comprehensive() {
    // 提升 and 增強 are absent on purpose: both are ordinary Taiwanese
    // technical usage and are parked on the watchlist.
    let text = "此外，持久的制度與永恆的價值會增強信任並提升效率。\
        我們要培養能力、促進合作、涵養精神，留下寶貴的經驗。\
        這是一個充滿活力的場域，呈現相互作用與交織的關係。\
        系統追求無縫整合與無縫銜接，卻形成錯綜複雜的織錦與畫卷。\
        這些安排可以賦予力量。\
        在當今數位治理的時代，隨著產業的快速發展，眾所周知，不言而喻，本文將說明背景，本節將補充案例。\
        接下來我們來看，理解了這些資料我們就能明白。願我們持續努力，讓我們都能前進，歷史將會記住，世界和平作出更大貢獻。";
    let output = run_lint_stdin(&["--detect-ai", "--format", "json"], text);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("AI JSON output");
    let found: std::collections::HashSet<_> = json["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|i| i["rule_type"] == "ai_style")
        .filter_map(|i| i["found"].as_str())
        .collect();
    for expected in [
        "此外",
        "持久的",
        "永恆的",
        "培養",
        "促進",
        "涵養",
        "寶貴的",
        "充滿活力",
        "相互作用",
        "交織",
        "無縫",
        "無縫銜接",
        "錯綜複雜",
        "賦予力量",
        "織錦",
        "畫卷",
        "在當今",
        "隨著",
        "眾所周知",
        "不言而喻",
        "本文將",
        "本節將",
        "接下來我們來看",
        "理解了",
        "我們就能明白",
        "願我們",
        "讓我們都能",
        "歷史將會記住",
        "世界和平作出更大貢獻",
    ] {
        assert!(
            found.contains(expected),
            "missing {expected}; found={found:?}; stdout={stdout}"
        );
    }
}

#[test]
fn cli_lint_translationese_technical_gate_low_false_positive_rate() {
    let clean_technical = "本工具使用有限狀態掃描器處理繁體中文文件。\
        規則資料在建置階段編譯，執行時直接載入快取。\
        命令列介面支援 JSON 輸出、Markdown 排除區段、術語表與基準檔。\
        測試資料涵蓋標點、詞彙、文法、表格欄位與快取命中路徑。\
        技術文件可指定 technical profile，讓校準門檻符合規格文件的語氣。";
    let output = run_lint_stdin(
        &[
            "--detect-translationese",
            "--translationese-domain",
            "technical",
            "--format",
            "json",
        ],
        clean_technical,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("technical translationese JSON output");
    let translationese_count = json["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|i| i["rule_type"] == "translationese")
        .count();
    assert!(
        translationese_count <= 1,
        "technical clean-text gate allows <=1 FP per short fixture: {stdout}"
    );
}

#[test]
fn cli_lint_context_suggestions_survive_pack_loading() {
    // The field rides on SpellingRule, so packs get it for free in principle.
    // This pins that the pack deserialization path actually carries it, and
    // that a multi-entry group is reported but never auto-applied.
    let dir = tempfile::tempdir().unwrap();
    let packs = write_pack(
        dir.path(),
        "ctxsug",
        serde_json::json!({
            "from": "測試詞",
            "to": ["預設詞"],
            "type": "cross_strait",
            "english": "test term",
            "context_suggestions": [
                { "clues": ["流程"], "to": ["改善詞", "提升詞"] }
            ],
        }),
    );

    let it_file = dir.path().join("it.md");
    let biz_file = dir.path().join("biz.md");
    std::fs::write(&it_file, "需要測試詞演算法").unwrap();
    std::fs::write(&biz_file, "需要測試詞流程").unwrap();

    let suggestions = |path: &std::path::Path| -> Vec<String> {
        let output = run_bin_args(&[
            "--packs-dir",
            &packs,
            "--pack",
            "ctxsug",
            "lint",
            path.to_str().unwrap(),
            "--format",
            "json",
        ]);
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("valid JSON");
        parsed["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["found"] == "測試詞")
            .map(|i| {
                i["suggestions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect()
            })
            .expect("pack rule should fire")
    };

    assert_eq!(suggestions(&it_file), ["預設詞"], "no clue: rule default");
    assert_eq!(
        suggestions(&biz_file),
        ["改善詞", "提升詞"],
        "clue in window selects the group"
    );

    // A clue inside code is not prose, so it must not select the group. Getting
    // this wrong is worse than a cosmetic wording change: the two-entry group
    // is never auto-applied, so the match silently stops being fixable because
    // of a word in a code sample.
    //
    // The pair below is inline code on the same line, deliberately, because it
    // is the only shape that discriminates. A fenced block has to be preceded
    // by a blank line, and context_byte_window already stops at a paragraph
    // break, so a fenced fixture passes even when the excluded ranges are never
    // threaded into the selection window at all. The control case differs only
    // in the backticks.
    let inline_file = dir.path().join("inline.md");
    std::fs::write(&inline_file, "需要測試詞演算法 `流程` 說明\n").unwrap();
    assert_eq!(
        suggestions(&inline_file),
        ["預設詞"],
        "a clue inside an inline code span must not select the group"
    );

    let bare_file = dir.path().join("bare.md");
    std::fs::write(&bare_file, "需要測試詞演算法 流程 說明\n").unwrap();
    assert_eq!(
        suggestions(&bare_file),
        ["改善詞", "提升詞"],
        "the same clue as prose, same distance, does select the group"
    );

    // The single-suggestion default is auto-fixable; the two-entry group is
    // not.
    let output = run_bin_args(&[
        "--packs-dir",
        &packs,
        "--pack",
        "ctxsug",
        "lint",
        it_file.to_str().unwrap(),
        "--fix=lexical_safe",
    ]);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&it_file).unwrap(),
        "需要預設詞演算法"
    );

    let output = run_bin_args(&[
        "--packs-dir",
        &packs,
        "--pack",
        "ctxsug",
        "lint",
        biz_file.to_str().unwrap(),
        "--fix=lexical_contextual",
    ]);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&biz_file).unwrap(),
        "需要測試詞流程",
        "a multi-entry group must stay a judgment call at every tier"
    );
}

#[test]
fn cli_lint_orthographic_tier_never_picks_among_candidates() {
    // A variant rule is orthographic, so the fixer applies it at every tier. It
    // used to take the first of however many suggestions were offered, which
    // meant a pack could get one of two judgment calls written to the user's
    // file at --fix=orthographic, the most conservative tier there is. Both
    // routes into that arm are covered here: a plain multi-entry to, and a
    // multi-entry context group.
    let dir = tempfile::tempdir().unwrap();
    for (name, rule, text) in [
        (
            "vmulti",
            serde_json::json!({
                "from": "測試詞", "to": ["甲詞", "乙詞"],
                "type": "variant", "english": "test term",
            }),
            "需要測試詞",
        ),
        (
            "vctx",
            serde_json::json!({
                "from": "測試詞", "to": ["預設詞"],
                "type": "variant", "english": "test term",
                "context_suggestions": [{ "clues": ["流程"], "to": ["改善詞", "提升詞"] }],
            }),
            "需要測試詞流程",
        ),
    ] {
        let packs = write_pack(dir.path(), name, rule);
        let file = dir.path().join(format!("{name}.md"));
        std::fs::write(&file, text).unwrap();

        let output = run_bin_args(&[
            "--packs-dir",
            &packs,
            "--pack",
            name,
            "lint",
            file.to_str().unwrap(),
            "--profile",
            "strict",
            "--fix=orthographic",
        ]);
        assert!(output.status.success());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            text,
            "{name}: several candidates must never be resolved by picking one"
        );
    }
}

#[test]
fn cli_lint_editorial_confidence_gate_covers_a_shipped_rule() {
    // Every other test of this gate builds a synthetic pack. Seven shipped
    // rules carry editorial_confidence: low, but six have multi-entry to lists
    // and were already declined at suggestion selection, so 場景 is the only
    // one whose behavior the annotation actually changes. Without this the gate
    // is only ever exercised against rules that do not ship.
    let dir = tempfile::tempdir().unwrap();
    let safe = dir.path().join("safe.md");
    let contextual = dir.path().join("contextual.md");

    // Clue-bearing IT prose, so the rule's own context gate is satisfied and
    // the editorial annotation is the only thing left deciding.
    let source = "這個測試場景需要在系統中重現。";
    std::fs::write(&safe, source).unwrap();
    std::fs::write(&contextual, source).unwrap();

    let output = run_lint_args(&[safe.to_str().unwrap(), "--fix=lexical_safe"]);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&safe).unwrap(),
        source,
        "lexical_safe must decline a low-confidence shipped rule"
    );

    let output = run_lint_args(&[contextual.to_str().unwrap(), "--fix=lexical_contextual"]);
    assert!(output.status.success());
    let fixed = std::fs::read_to_string(&contextual).unwrap();
    assert!(
        fixed.contains("情境"),
        "lexical_contextual must apply it: {fixed}"
    );
}

// The delete sentinel "to": [""] means "remove this span". It is only sound
// when the span is a detachable discourse adjunct. For a predicate, a copula, a
// head noun, or a modifier that leaves a degree adverb stranded, removing the
// span produces ungrammatical zh-TW: 那是很寶貴的經驗 became 那是很經驗. Those
// rules now carry an empty "to", so they report without rewriting.
#[test]
fn cli_fix_never_deletes_a_load_bearing_span() {
    // Each of these must report and survive --fix intact. Asserting only that
    // the text is unchanged would pass just as well if every filler rule
    // stopped firing, so require the finding too: "reported, not rewritten" is
    // the property, and half of it is invisible without this check.
    let reported = [
        "他在急診室待了二十年，那是很寶貴的經驗。",
        "這款鏡頭可以說是這個價位帶最銳利的一支。",
        "道理不言而喻，不必我多說。",
        "無縫整合是它的賣點之一。",
        "作為一個備援方案，它已經夠用了。",
        "這件事眾所周知。",
        "這一點毫無疑問。",
        "這是一個好問題。",
        "他提出非常棒的觀點。",
        "你當然可以這樣做。",
        "這一切都是為了實現這一目標。",
        "讓我來為你介紹。",
        "希望這對你有幫助。",
        "您說得完全正確。",
    ];
    for input in reported {
        let output = run_lint_stdin(&["--fix"], input);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.trim_end(),
            input,
            "--fix rewrote a load-bearing span in {input}"
        );
        let json = run_lint_stdin(&["--format", "json"], input);
        let parsed: serde_json::Value =
            serde_json::from_slice(&json.stdout).expect("lint --format json");
        assert!(
            !parsed["issues"]
                .as_array()
                .expect("issues array")
                .is_empty(),
            "no finding reported for {input}, so the no-rewrite assertion above proves nothing"
        );
    }

    // The other half of the guard. All three carry a rule, but a clue-gated
    // one: 理解了 needs 我們就能明白, 隨著 needs 快速發展, 在當今 needs 時代.
    // These are the literal readings, carrying no clue, that the gate exists to
    // spare. They must stay silent, not merely unrewritten.
    let silent = [
        "我理解了你的意思。",
        "隨著時間過去，傷口慢慢癒合。",
        "在當今的制度下，這條路走不通。",
    ];
    for input in silent {
        let json = run_lint_stdin(&["--format", "json"], input);
        let parsed: serde_json::Value =
            serde_json::from_slice(&json.stdout).expect("lint --format json");
        assert!(
            parsed["issues"]
                .as_array()
                .expect("issues array")
                .is_empty(),
            "clue gate leaked on the literal reading in {input}"
        );
    }
}

// A word ending in 外 abuts the rules 外設/外置/外鍵. Without a segmentation
// entry for the left-hand word the rule fires across the boundary and 額外設定
// becomes 額硬體周邊定.
#[test]
fn cli_fix_does_not_split_a_word_ending_in_wai() {
    let cases = [
        "它不需要額外設定。",
        "公司到海外設廠。",
        "他們在國外設立分公司。",
        "這場意外設計出新的流程。",
        "格外設想周到。",
        // Same class, other opening characters: 標 and 此 and 源.
        "請看達標清單。",
        "投標量很大。",
        "他個性如此外向。",
        "彼此外貌相似。",
        "由此外推可得結論。",
        "除此外還有三個選項。",
        "請參閱來源文件的第三節。",
        "電源文件放在機櫃旁邊。",
    ];
    for input in cases {
        let output = run_lint_stdin(&["--fix"], input);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout.trim_end(), input, "--fix split a word in {input}");
    }
    // The genuine PRC terms still convert.
    for (input, want) in [
        ("購買外設鍵盤。", "購買硬體周邊鍵盤。"),
        ("這是標清畫面。", "這是標準畫質畫面。"),
        ("請參閱源文件。", "請參閱原始檔。"),
    ] {
        let output = run_lint_stdin(&["--fix"], input);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), want);
    }
    // 此外 is reported but not deleted: one connective is ordinary zh-TW.
    let output = run_lint_stdin(&["--fix", "--detect-ai"], "此外，還有三個選項。");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        "此外，還有三個選項。"
    );
}

// Reduplication is productive in Chinese, so a repeated one- or two-character
// unit cannot be deleted without a dictionary: 研究研究 is grammar, not a
// stutter. Those are reported without a suggestion.
#[test]
fn cli_fix_never_collapses_productive_reduplication() {
    let cases = [
        "這件事我們研究研究。",
        "大家一起討論討論吧。",
        "《茜茜公主》是經典。",
        "整整一百年過去了。",
        "錯字連連。",
        "形形色色的人。",
    ];
    for input in cases {
        let output = run_lint_stdin(&["--fix"], input);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            input,
            "--fix collapsed reduplication in {input}"
        );
    }
    // A three-character unit is past the reach of reduplication.
    let output = run_lint_stdin(&["--fix"], "處理器處理器效能高。");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        "處理器效能高。"
    );
}
// Converting one half of a bracket pair is worse than converting neither.
#[test]
fn cli_fix_keeps_bracket_pairs_matched() {
    let output = run_lint_stdin(&["--fix"], "上海電影譯製廠(1957 成立)。");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        "上海電影譯製廠(1957 成立)。"
    );
    let output = run_lint_stdin(&["--fix"], "這是中文(附註)說明。");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        "這是中文（附註）說明。"
    );
}

// A joiner the detector judged stray is reported, not deleted: the neighbour
// test cannot see a word-final Malayalam chillu or a doubled Persian ZWNJ. ZWSP
// has no such reading and is still stripped.
#[test]
fn cli_fix_strips_only_unambiguous_invisible_characters() {
    let input = "\u{0D05}\u{0D35}\u{0D28}\u{0D4D}\u{200D} 走了。\u{200C}\u{200C}在這裡。";
    let output = run_lint_stdin(&["--fix", "--detect-ai"], input);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), input);

    let output = run_lint_stdin(&["--fix", "--detect-ai"], "零寬\u{200B}空格。");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        "零寬空格。"
    );
}

// The invisible-character layer covers more than the six zero-width points it
// started with: soft hyphen, CGJ, word joiner, invisible math operators, bidi
// overrides, loose tag characters and noncharacters are all copy-paste or
// watermark residue in zh-TW prose.
#[test]
fn cli_detects_the_wider_invisible_character_set() {
    let text = "字\u{00AD}形\u{034F}測\u{2060}試\u{2062}與\u{202E}方向\u{FDD0}結束。";
    let output = run_lint_stdin(&["--detect-ai", "--format", "json"], text);
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("AI JSON output");
    let hits = json["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|i| {
            i["context"]
                .as_str()
                .is_some_and(|c| c.contains("隱形字元"))
        })
        .count();
    assert_eq!(
        hits,
        6,
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );

    // A flag tag sequence, an emoji ZWJ sequence and an ideographic variation
    // selector are orthography, not residue.
    let clean = "旗幟 \u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F} \
                 與 \u{1F469}\u{1F3FB}\u{200D}\u{1F680} 和 葛\u{E0100} 都合法。";
    let output = run_lint_stdin(&["--detect-ai"], clean);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("隱形字元"),
        "flagged valid sequences: {stdout}"
    );
}

// The MoE standard-form variants documented in docs/rules.md. 綫 was listed
// there but never shipped, so the doc and the ruleset had drifted; these are
// strict-profile rules, which is why a base-profile corpus cannot cover them.
#[test]
fn cli_lint_moe_variant_forms_fire_under_strict() {
    let text = "這條綫路的裏面有一碗麪。";

    // Reading from stdin keeps stdout for the document, so findings are on
    // stderr.
    let output = run_lint_stdin(&["--profile", "strict"], text);
    let report = String::from_utf8_lossy(&output.stderr);
    for want in ["綫", "裏", "麪"] {
        assert!(report.contains(want), "missing {want} in {report}");
    }
    // Off by default, so ordinary linting does not rewrite an author's glyphs.
    let output = run_lint_stdin(&[], text);
    assert!(String::from_utf8_lossy(&output.stderr).contains("No issues"));
}

// The calibration layer is the only code in the crate that opens a socket, and
// it sends sentence-sized excerpts of the linted document to Google. Under MCP
// the decision to pass "verify" belongs to a model rather than to the person
// whose text it is, so an operator needs a switch the caller cannot argue past.
#[test]
fn cli_no_network_env_refuses_verify() {
    let bin = binary_path();
    let run = |value: Option<&str>, args: &[&str]| -> Output {
        let mut cmd = Command::new(&bin);
        cmd.args(["lint", "--"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("RUST_LOG")
            .env_remove("ZHTW_NO_NETWORK");
        if let Some(v) = value {
            cmd.env("ZHTW_NO_NETWORK", v);
        }
        let mut child = cmd.spawn().expect("spawn zhtw-mcp");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all("這個軟件需要優化。".as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("collect output")
    };

    // Set: --verify is refused, by name, and the run fails rather than quietly
    // linting without the verification that was asked for.
    let blocked = run(Some("1"), &["--verify"]);
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        stderr.contains("ZHTW_NO_NETWORK"),
        "refusal must name the variable, got: {stderr}"
    );
    assert!(
        stderr.contains("--verify"),
        "refusal must name the flag, got: {stderr}"
    );
    assert!(!blocked.status.success(), "refusing must not exit zero");

    // Set: ordinary linting is untouched. The switch governs egress, not the
    // scanner, and a linter that stopped working offline would be useless.
    let ordinary = run(Some("1"), &[]);
    assert!(ordinary.status.success(), "plain lint must still run");
    let reported = String::from_utf8_lossy(&ordinary.stderr);
    assert!(
        reported.contains("軟體"),
        "plain lint must still report findings, got: {reported}"
    );

    // The off-values ("0", empty, unset) are checked in
    // engine::translate::no_network_off_values instead. Exercising them here
    // would mean running --verify without a refusal, and that posts the fixture
    // to Google from the test suite for the feature whose whole point is not
    // doing that.
}

// Refusing has to happen before anything is written. The check used to sit in
// the per-file path, after the fixer had already rewritten the file back to
// disk, so "--fix --verify" under the switch destroyed the input and then
// refused, once per file, with no report for any of them.
#[cfg(feature = "translate")]
#[test]
fn cli_no_network_refuses_before_fix_writes_anything() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("a.md");
    let second = dir.path().join("b.md");
    let original = "這個軟件需要優化。\n";
    std::fs::write(&first, original).expect("write a.md");
    std::fs::write(&second, original).expect("write b.md");

    let out = Command::new(binary_path())
        .args([
            "lint",
            "--fix",
            "--verify",
            first.to_str().expect("utf-8 path"),
            second.to_str().expect("utf-8 path"),
        ])
        .env_remove("RUST_LOG")
        .env("ZHTW_NO_NETWORK", "1")
        .output()
        .expect("run zhtw-mcp");

    assert!(!out.status.success(), "refusing must not exit zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ZHTW_NO_NETWORK"),
        "refusal must name the variable: {stderr}"
    );
    // Once for the invocation, not once per file.
    assert_eq!(
        stderr.matches("ZHTW_NO_NETWORK").count(),
        1,
        "the refusal is about the run, not each file: {stderr}"
    );
    for path in [&first, &second] {
        assert_eq!(
            std::fs::read_to_string(path).expect("read back"),
            original,
            "{path:?} was rewritten before the refusal"
        );
    }
}

// The five-then-ten attribution phrases used to live in a const inside the
// scanner, so they had no override, no disabled flag and no provenance gate
// while every other rule had all three. They are ruleset rules now, carrying a
// structural_guard that keeps the citation check the schema cannot express.
// What that buys is exactly this: retiring one phrase without touching the
// rest.
#[test]
fn cli_a_guarded_attribution_rule_can_be_retired_by_override() {
    let dir = tempfile::tempdir().expect("temp dir");
    let overrides = dir.path().join("overrides.json");
    std::fs::write(
        &overrides,
        r#"{"schema_version":3,"spelling":[{"from":"觀察者指出","to":[],"type":"ai_filler",
            "disabled":true,"context":"retired","structural_guard":"uncited_attribution"}],
            "case":[]}"#,
    )
    .expect("write overrides");

    let run = |args: &[&str], input: &str| -> String {
        let mut child = Command::new(binary_path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("RUST_LOG")
            .spawn()
            .expect("spawn zhtw-mcp");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("collect output");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let ov = overrides.to_str().expect("utf-8 path");

    // Baseline: it fires, so the override below is proving something.
    assert!(
        run(
            &["lint", "--detect-ai", "--"],
            "觀察者指出這個現象值得注意。"
        )
        .contains("觀察者指出"),
        "the phrase must fire without an override, or this test proves nothing"
    );
    assert!(
        !run(
            &["--overrides", ov, "lint", "--detect-ai", "--"],
            "觀察者指出這個現象值得注意。"
        )
        .contains("觀察者指出"),
        "a disabled override must retire the phrase"
    );
    // Retiring one must not empty the detector.
    assert!(
        run(
            &["--overrides", ov, "lint", "--detect-ai", "--"],
            "研究顯示成果很好。"
        )
        .contains("研究顯示"),
        "sibling phrases must survive retiring one"
    );

    // And the guard still runs for the survivors: a cited claim is ordinary
    // zh-TW, which is the check that kept these out of the ruleset before.
    assert!(
        !run(
            &["--overrides", ov, "lint", "--detect-ai", "--"],
            "專家認為這項安排可行[1]。"
        )
        .contains("專家認為"),
        "the citation guard must still suppress a cited attribution"
    );
}

/// The rhythm (氣口) axis is opt-in, advisory, and outside every fix tier.
///
/// Three properties in one test because they are one contract: without the
/// flag the output is byte-identical to what it was before the axis existed,
/// with the flag the two new checks report, and no tier of `--fix` acts on
/// what they report.
#[test]
fn cli_lint_rhythm_is_opt_in_advisory_and_never_fixed() {
    let bad = include_str!("fixtures/translationese/rhythm_bad.txt");
    let good = include_str!("fixtures/translationese/rhythm_good.txt");

    let issues = |args: &[&str], text: &str| -> Vec<String> {
        let out = run_lint_stdin(args, text);
        let json: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("JSON output");
        json["issues"]
            .as_array()
            .expect("issues array")
            .iter()
            .map(|i| i.to_string())
            .collect()
    };

    // Off by default: the flag is the only thing that can add a finding. Byte
    // identity against the pre-change binary was measured out of band over 214
    // runs (every corpus and fixture file crossed with the profiles, renderers,
    // fix tiers and convert) and recorded in DONE.md; what a test can hold is
    // that nothing rhythm-shaped reaches the default path.
    let plain = issues(&["--format", "json"], bad);
    let rhythm = issues(&["--rhythm", "--format", "json"], bad);
    for issue in &plain {
        assert!(
            !issue.contains("氣口"),
            "a rhythm finding reached the default path: {issue}"
        );
    }
    assert!(
        rhythm.len() > plain.len(),
        "--rhythm reported nothing new on the fixture: {rhythm:?}"
    );
    for issue in &plain {
        assert!(
            rhythm.contains(issue),
            "--rhythm dropped a finding the default run made: {issue}"
        );
    }
    let added: Vec<&String> = rhythm.iter().filter(|i| !plain.contains(i)).collect();
    assert_eq!(
        added.len(),
        2,
        "expected the long-sentence and the monotony finding: {added:?}"
    );
    for issue in &added {
        assert!(
            issue.contains("氣口"),
            "--rhythm added a finding that is not a rhythm finding: {issue}"
        );
    }

    // Prose that breathes gets nothing, flag or no flag: a 頓號 list, an
    // identifier run and three different sentence endings are all exempt.
    assert_eq!(
        issues(&["--rhythm", "--format", "json"], good),
        issues(&["--format", "json"], good),
        "--rhythm fired on prose that already breathes"
    );

    // Rhythm is taste, and the fixer is not: every tier must leave it alone.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rhythm.txt");
    for args in [
        vec!["--rhythm", "--fix=orthographic"],
        vec!["--rhythm", "--fix"],
        vec!["--rhythm", "--fix=lexical_contextual"],
    ] {
        std::fs::write(&path, bad).expect("write");
        let p = path.to_string_lossy().into_owned();
        let mut argv = args.clone();
        argv.push(p.as_str());
        let output = run_lint_args(&argv);
        assert!(
            output.status.success(),
            "--fix failed with {args:?}, so leaving the file alone proves nothing"
        );
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, bad, "--fix rewrote a rhythm finding with {args:?}");
    }
}
