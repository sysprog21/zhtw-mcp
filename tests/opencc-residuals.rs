// Audit of the residual mainland forms that OpenCC s2twp leaves behind.
//
// tests/fixtures/opencc_residuals.tsv has one row per probe: the source row, a
// sample sentence, the expected finding and a verdict. This test reparses it,
// reruns every sample through the CLI with the style detectors on, and fails
// when the recorded verdict no longer matches, so the fixture cannot rot
// unnoticed.
//
// Verdicts:
//   fired   a finding whose "category:from->to" equals the expected column
//   silent  no finding, and no rule exists for the form
//   gated   no finding, because a rule exists and its context gate or an
//           exception phrase suppresses it in this sample

use std::io::Write;
use std::process::{Command, Stdio};

const FIXTURE: &str = include_str!("fixtures/opencc_residuals.tsv");

/// Compact output lines are "line:col:severity:category:from->to(+n)"; return
/// the "category:from->to" part with the alternatives count dropped.
fn findings(sample: &str) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zhtw-mcp"))
        .args([
            "lint",
            "--detect-ai",
            "--detect-translationese",
            "--rhythm",
            "--format",
            "compact",
            "--",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RUST_LOG")
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(sample.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();

    // Exit 1 is the gate code for findings; anything else is a crash, which
    // would otherwise read as no findings and pass every silent row.
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "lint failed on {sample:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let tail = l.splitn(4, ':').nth(3).unwrap_or(l);
            match tail.rfind('+') {
                Some(i) if tail[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
                    tail[..i].to_string()
                }
                _ => tail.to_string(),
            }
        })
        .collect()
}

#[test]
fn opencc_residuals_match_recorded_verdicts() {
    let mut rows = 0;
    let mut mismatches = Vec::new();
    for (n, line) in FIXTURE.lines().enumerate().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            cols.len(),
            4,
            "line {}: want 4 tab-separated columns",
            n + 1
        );
        let (row, sample, expected, verdict) = (cols[0], cols[1], cols[2], cols[3]);
        let got = findings(sample);
        let ok = match verdict {
            "fired" => got.iter().any(|g| g == expected),
            "silent" | "gated" => expected == "none" && got.is_empty(),
            other => panic!("line {}: unknown verdict {other:?}", n + 1),
        };
        if !ok {
            mismatches.push(format!(
                "line {} [{row}] {sample}: recorded {verdict} {expected}, got {got:?}",
                n + 1
            ));
        }
        rows += 1;
    }
    assert!(rows >= 25, "fixture lost rows: only {rows}");
    assert!(
        mismatches.is_empty(),
        "verdict drift:\n{}",
        mismatches.join("\n")
    );
}
