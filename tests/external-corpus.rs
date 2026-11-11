// Score "convert" against Microsoft's published VS Code zh-Hant strings, with
// OpenCC s2twp as the baseline.
//
// The Simplified bundle is the input and the Traditional bundle for the same
// key is the reference. Both are fetched at a pinned upstream commit by
// scripts/fetch-external-corpus.py, which verifies per-file sha256 pins in
// tests/fixtures/external-corpus-pins.json. A published translation is
// evidence, not ground truth, so a disagreement is output, never a failure.
//
// Needs network and the Python package "opencc", so it is #[ignore] and not
// part of make check. Run:
//   cargo test --test external-corpus -- --ignored --nocapture
//
// Metrics, per system:
//   exact      the converted line equals the reference line
//   spans      the set of edits a system made to the source, scored against
//              the set the reference made. An edit is a source character that
//              was dropped, or a character inserted at a gap in the source, so
//              a replacement is one drop plus one insertion and the inserted
//              character must match. Precision is the share of a system's
//              edits the reference also made, recall the share of the
//              reference's edits the system made, summed over all pairs.
//
// The disagreement list goes to target/external-corpus/disagreements.tsv.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

mod common;
use common::{changed_positions, Edit};

use serde::Deserialize;

#[derive(Deserialize)]
struct Pair {
    id: String,
    src: String,
    #[serde(rename = "ref")]
    reference: String,
    opencc: String,
}

fn out_dir() -> PathBuf {
    std::env::var_os("EXTERNAL_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/external-corpus"))
}

fn load_pairs() -> Vec<Pair> {
    let path = out_dir().join("pairs.json");
    if !path.exists() {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/fetch-external-corpus.py");
        let status = Command::new("python3")
            .arg(script)
            .arg("--out")
            .arg(out_dir())
            .status()
            .expect("python3 must be available");
        assert!(status.success(), "fetch-external-corpus.py failed");
    }
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

/// Run "convert" over every line in one process.
fn convert_all(lines: &[&str]) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zhtw-mcp"))
        .args(["convert", "--"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("RUST_LOG")
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = lines.join("\n") + "\n";
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).unwrap());
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    text.lines().map(str::to_string).collect()
}

#[derive(Default)]
struct Score {
    exact: usize,
    tp: usize,
    predicted: usize,
    gold: usize,
}

impl Score {
    fn add(&mut self, src: &[char], output: &str, gold: &HashSet<Edit>, reference: &str) {
        if output == reference {
            self.exact += 1;
        }
        let out: Vec<char> = output.chars().collect();
        let predicted = changed_positions(src, &out);
        self.tp += predicted.intersection(gold).count();
        self.predicted += predicted.len();
        self.gold += gold.len();
    }

    fn report(&self, name: &str, pairs: usize) {
        let pct = |a: usize, b: usize| {
            if b == 0 {
                0.0
            } else {
                100.0 * a as f64 / b as f64
            }
        };
        let p = pct(self.tp, self.predicted);
        let r = pct(self.tp, self.gold);
        let f1 = if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        };
        println!(
            "{name:<10} exact={:>5.1}% ({}/{pairs})  span_precision={p:>5.1}%  span_recall={r:>5.1}%  span_f1={f1:>5.1}%",
            pct(self.exact, pairs),
            self.exact,
        );
    }
}

#[test]
#[ignore = "needs network and the opencc Python package"]
fn external_corpus_vscode_zh_hant() {
    let pairs = load_pairs();
    assert!(!pairs.is_empty(), "no pairs fetched");

    let srcs: Vec<&str> = pairs.iter().map(|p| p.src.as_str()).collect();
    let converted = convert_all(&srcs);
    assert_eq!(
        converted.len(),
        pairs.len(),
        "convert changed the line count"
    );

    let (mut ours, mut base) = (Score::default(), Score::default());
    let mut rows = Vec::new();
    for (pair, ours_out) in pairs.iter().zip(&converted) {
        let src: Vec<char> = pair.src.chars().collect();
        let reference: Vec<char> = pair.reference.chars().collect();
        let gold = changed_positions(&src, &reference);
        ours.add(&src, ours_out, &gold, &pair.reference);
        base.add(&src, &pair.opencc, &gold, &pair.reference);
        if *ours_out != pair.opencc || *ours_out != pair.reference {
            rows.push(format!(
                "{}\t{}\t{}\t{}\t{}",
                pair.id, pair.src, pair.reference, ours_out, pair.opencc
            ));
        }
    }

    println!();
    println!(
        "=== External corpus: VS Code zh-Hans to zh-Hant ({} pairs) ===",
        pairs.len()
    );
    ours.report("zhtw-mcp", pairs.len());
    base.report("opencc", pairs.len());
    let path = out_dir().join("disagreements.tsv");
    std::fs::write(
        &path,
        "id\tsource\treference\tzhtw_mcp\topencc\n".to_string() + &rows.join("\n") + "\n",
    )
    .unwrap();
    println!("disagreements: {} rows in {}", rows.len(), path.display());
}
