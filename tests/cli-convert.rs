// Integration tests for the CLI convert subcommand, focused on the --verify
// gate.
//
// Conversion is otherwise entirely local. --verify is what sends the sentences
// around any residual issue to Google Translate for anchor matching, so the
// default has to stay off and the flag has to be the only way in.

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

fn run_convert(extra_args: &[&str], input: &str) -> Output {
    let bin = binary_path();
    Command::new(&bin)
        .arg("convert")
        .args(extra_args)
        .arg("--")
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

/// The default path converts and says nothing about verification, because
/// it never reaches the network.
#[test]
fn convert_default_does_not_verify() {
    let out = run_convert(&[], "用户使用软件\n");
    assert!(out.status.success(), "convert should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("使用者") && stdout.contains("軟體"),
        "conversion must still happen; got {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("convert: verify"),
        "default convert must not run anchor verification; got {stderr:?}"
    );
}

/// --verify is accepted.  The input converts cleanly, so no issue survives
/// to be calibrated and this test stays offline; it pins the flag wiring,
/// not the network call.
#[cfg(feature = "translate")]
#[test]
fn convert_accepts_verify_flag() {
    let out = run_convert(&["--verify"], "用户使用软件\n");
    assert!(
        out.status.success(),
        "convert --verify should be accepted; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Built without the feature, the flag has to fail with the explanation
/// rather than be silently ignored.  Asserting this instead of skipping
/// keeps both build configurations covered.
#[cfg(not(feature = "translate"))]
#[test]
fn convert_verify_flag_explains_missing_feature() {
    let out = run_convert(&["--verify"], "用户使用软件\n");
    assert!(
        !out.status.success(),
        "--verify must fail without the feature"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires the 'translate' feature"),
        "expected the feature explanation; got {stderr:?}"
    );
}

/// Unknown flags still fail loudly rather than being read as filenames.
#[test]
fn convert_rejects_unknown_flag() {
    let out = run_convert(&["--verifyy"], "用户\n");
    assert!(!out.status.success(), "unknown flag must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown convert flag"),
        "expected a flag error; got {stderr:?}"
    );
}

/// A file name that means Markdown means it everywhere.
///
/// `convert` used to decide this itself, case-sensitively and on the `md`
/// extension alone, so `.markdown` and `README.MD` were read as plain text and
/// the fixer rewrote terminology inside code fences that Markdown protects.
#[test]
fn convert_reads_markdown_file_names_the_way_lint_does() {
    let dir = tempfile::tempdir().expect("temp dir");
    let body = "正文软件\n\n```\n代码软件\n```\n";

    let mut outputs = Vec::new();
    for name in ["t.md", "t.markdown", "T.MD"] {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let out = Command::new(binary_path())
            .arg("convert")
            .arg(&path)
            .output()
            .expect("run convert");
        assert!(out.status.success(), "convert {name} failed");
        outputs.push((name, String::from_utf8_lossy(&out.stdout).into_owned()));
    }

    // The fence content is what distinguishes the two readings: Markdown leaves
    // the term inside it alone, plain text rewrites it.
    for (name, text) in &outputs {
        assert!(
            text.contains("代碼軟件"),
            "{name}: the fenced term should be left alone, got: {text:?}"
        );
        assert!(
            !text.contains("程式碼軟體"),
            "{name}: the fixer reached inside a code fence, got: {text:?}"
        );
    }
    assert_eq!(
        outputs[0].1, outputs[1].1,
        ".md and .markdown must convert alike"
    );
    assert_eq!(
        outputs[0].1, outputs[2].1,
        "the extension is not case-sensitive"
    );
}

/// Install a single-rule pack and return its directory, for `--packs-dir`.
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

/// A pack rule has to reach the conversion.
///
/// `run_convert` merged with an empty pack selection, so `--pack` parsed, the
/// command succeeded, and the rules the caller asked for were absent from the
/// scanner that drives the fix loop. Nothing errored; the output was just
/// computed against the wrong ruleset. The term here is synthetic so that
/// re-classifying a shipped term cannot make this pass for the wrong reason.
#[test]
fn convert_pack_rules_reach_the_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let packs = write_pack(
        dir.path(),
        "convpack",
        serde_json::json!({
            "from": "測試詞",
            "to": ["替換詞"],
            "type": "cross_strait",
            "english": "test term",
        }),
    );
    let file = dir.path().join("input.txt");
    std::fs::write(&file, "這個測試詞").unwrap();

    let without = Command::new(binary_path())
        .arg("convert")
        .arg(&file)
        .output()
        .expect("run convert");
    let without = String::from_utf8_lossy(&without.stdout).into_owned();
    assert!(
        without.contains("測試詞"),
        "no pack selected, so the term stays; got {without:?}"
    );

    let with = Command::new(binary_path())
        .args(["--packs-dir", &packs, "--pack", "convpack", "convert"])
        .arg(&file)
        .output()
        .expect("run convert");
    assert!(
        with.status.success(),
        "convert with a pack should exit 0; stderr={:?}",
        String::from_utf8_lossy(&with.stderr)
    );
    let with = String::from_utf8_lossy(&with.stdout).into_owned();
    assert!(
        with.contains("替換詞"),
        "the pack rule should have been applied; got {with:?}"
    );
}

const JSON_SC: &str =
    "{\"设置\": \"软件设置\", \"list\": [\"用户使用软件\"], \"n\": \"{name}软件\\n\"}\n";

/// Keys, placeholders and escapes survive, values convert, and the output
/// still parses.
#[test]
fn cli_convert_json_converts_values_and_keeps_keys() {
    let out = run_convert(&["--content-type", "json"], JSON_SC);
    assert!(out.status.success(), "convert should exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("output must be valid JSON");
    let obj = v.as_object().unwrap();
    let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["list", "n", "设置"], "keys untouched: {stdout}");
    let value = obj["设置"].as_str().unwrap();
    assert!(value.contains('設'), "value should be Traditional: {value}");
    assert!(
        !value.contains('软'),
        "value should be Traditional: {value}"
    );
    assert!(stdout.contains("{name}"), "placeholder kept: {stdout}");
    assert!(stdout.contains("\\n"), "escape kept: {stdout}");
}

/// The type also comes from a .json file name.
#[test]
fn cli_convert_json_by_extension() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("zh.json");
    std::fs::write(&path, JSON_SC).unwrap();
    let out = Command::new(binary_path())
        .arg("convert")
        .arg(&path)
        .output()
        .expect("run convert");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"设置\""), "key rewritten: {stdout}");
    assert!(stdout.contains('設'), "value not converted: {stdout}");
    serde_json::from_str::<serde_json::Value>(&stdout).expect("valid JSON");
}

/// Plain is the opt-out and converts the whole text, keys included.
#[test]
fn cli_convert_json_plain_opt_out_converts_keys() {
    let out = run_convert(&["--content-type", "plain"], JSON_SC);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("「設置」"), "{stdout}");
}
