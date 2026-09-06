// --consistency flag emits consistency report when both regional variants of
// the same concept appear in one document.

use std::process::{Command, Stdio};

/// Path to the binary under test.
///
/// `CARGO_BIN_EXE_<name>` is set by cargo for every integration test and
/// carries the platform's executable suffix.  Deriving it from
/// `current_exe()` instead, which every one of these test files used to do,
/// dropped the `.exe` on Windows and left a path that does not exist.
fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_zhtw-mcp"))
}

#[test]
fn consistency_block_appears_when_both_forms_present() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("test.md");
    // Mixed usage: 線程 (mainland) + 執行緒 (TW) both present.
    std::fs::write(&md, "我們的線程實作太慢，需要重新設計執行緒。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args([
            "lint",
            md.to_str().unwrap(),
            "--format",
            "json",
            "--consistency",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    let consistency = parsed["consistency"]
        .as_object()
        .expect("consistency block present");
    let groups = consistency["groups"].as_array().expect("groups array");
    assert!(
        groups.iter().any(|g| g["term_group"] == "thread"),
        "expected a 'thread' consistency group; got {groups:?}"
    );
}

#[test]
fn consistency_block_absent_when_only_one_form_present() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("test.md");
    // Only mainland form, no TW counterpart.
    std::fs::write(&md, "我們的線程實作太慢。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args([
            "lint",
            md.to_str().unwrap(),
            "--format",
            "json",
            "--consistency",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        parsed.get("consistency").is_none() || parsed["consistency"].is_null(),
        "no mixed usage → no consistency block; got: {}",
        parsed
    );
}

#[test]
fn consistency_ignores_canonical_substring_of_flagged_form() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "我們前往厄瓜多爾旅遊。\n").unwrap();

    let output = Command::new(binary_path())
        .args([
            "lint",
            md.to_str().unwrap(),
            "--format",
            "json",
            "--consistency",
        ])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(
        parsed["issues"]
            .as_array()
            .expect("issues array")
            .iter()
            .any(|issue| issue["found"] == "厄瓜多爾"),
        "the embedded country-name rule must flag the source form: {parsed}"
    );
    assert!(
        parsed.get("consistency").is_none() || parsed["consistency"].is_null(),
        "厄瓜多 appears only inside the flagged 厄瓜多爾: {parsed}"
    );
}

#[test]
fn consistency_block_omitted_without_flag() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "線程 ... 執行緒\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        parsed.get("consistency").is_none(),
        "consistency must be omitted without --consistency flag"
    );
}

/// `--consistency` must still report mixed usage when the
/// orthographic fixer actually rewrites the document.  Half-width
/// `,` and `.` adjacent to CJK are punctuation issues that
/// orthographic mode rewrites; the 線程/執行緒 lexical pair is left
/// untouched (orthographic skips CrossStrait per src/fixer.rs:197),
/// so the consistency block must still surface the `thread` group on
/// the post-fix issue list.
#[test]
fn consistency_block_present_during_fix_runs() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("test.md");

    // Half-width "," and "." next to CJK trigger Punctuation issues
    // (FixMode::Orthographic eligible). 線程/執行緒 stay as residual
    // CrossStrait issues for the consistency report to grab.
    std::fs::write(&md, "我們的線程太慢, 需要重構執行緒.\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args([
            "lint",
            md.to_str().unwrap(),
            "--format",
            "json",
            "--consistency",
            "--fix=orthographic",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Confirm the orthographic fixer actually rewrote the document: otherwise
    // the test would only exercise the pre-fix path.
    assert!(
        parsed["fixes_applied"].as_u64().unwrap_or(0) > 0,
        "orthographic fix must apply at least one rewrite; got fixes_applied={}",
        parsed["fixes_applied"],
    );
    assert_ne!(
        parsed["text"].as_str(),
        Some("我們的線程太慢, 需要重構執行緒.\n"),
        "post-fix `text` field must reflect the rewritten document",
    );

    let groups = parsed["consistency"]["groups"]
        .as_array()
        .expect("consistency groups");
    assert!(
        groups.iter().any(|g| g["term_group"] == "thread"),
        "expected a 'thread' consistency group during fix run; got {groups:?}"
    );
}
