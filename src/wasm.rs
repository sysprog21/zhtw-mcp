use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::engine::scan::{ContentType, Scanner};
use crate::rules::loader::load_embedded_ruleset;
use crate::rules::ruleset::{Issue, Profile, Severity};

static SCANNER: OnceLock<Scanner> = OnceLock::new();

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScanOptions {
    profile: Option<String>,
    relaxed: bool,
}

#[derive(Debug, Serialize)]
struct BrowserScanResult {
    issues: Vec<BrowserIssue>,
    issue_count: usize,
    badge_count: usize,
    severity_counts: SeverityCounts,
    detected_script: String,
}

#[derive(Debug, Default, Serialize)]
struct SeverityCounts {
    info: usize,
    warning: usize,
    error: usize,
}

#[derive(Debug, Serialize)]
struct BrowserIssue {
    offset: usize,
    length: usize,
    line: usize,
    col: usize,
    found: String,
    suggestions: Vec<String>,
    rule_type: String,
    severity: String,
    context: Option<String>,
    english: Option<String>,
}

#[wasm_bindgen]
pub fn scan_text(text: &str, options_json: Option<String>) -> Result<String, JsValue> {
    let options = parse_options(options_json)?;
    let profile = match options.profile.as_deref().unwrap_or("base") {
        "base" => Profile::Base,
        "strict" => Profile::Strict,
        other => {
            return Err(JsValue::from_str(&format!(
                "unsupported profile '{other}', expected 'base' or 'strict'"
            )));
        }
    };

    let mut config = profile.config();
    if options.relaxed {
        config = config.with_relaxed();
    }

    let scanner = scanner()?;
    let output = scanner.scan_for_content_type_with_config(text, ContentType::Plain, config);
    let mut severity_counts = SeverityCounts::default();
    let issues: Vec<BrowserIssue> = output
        .issues
        .iter()
        .map(|issue| {
            match issue.severity {
                Severity::Info => severity_counts.info += 1,
                Severity::Warning => severity_counts.warning += 1,
                Severity::Error => severity_counts.error += 1,
            }
            BrowserIssue::from(issue)
        })
        .collect();

    let result = BrowserScanResult {
        issue_count: issues.len(),
        badge_count: severity_counts.warning + severity_counts.error,
        issues,
        severity_counts,
        detected_script: output.detected_script.name().to_owned(),
    };

    serde_json::to_string(&result)
        .map_err(|err| JsValue::from_str(&format!("serialize scan result: {err}")))
}

fn parse_options(options_json: Option<String>) -> Result<ScanOptions, JsValue> {
    match options_json {
        Some(json) if !json.trim().is_empty() => serde_json::from_str(&json)
            .map_err(|err| JsValue::from_str(&format!("parse scan options: {err}"))),
        _ => Ok(ScanOptions::default()),
    }
}

fn scanner() -> Result<&'static Scanner, JsValue> {
    if let Some(scanner) = SCANNER.get() {
        return Ok(scanner);
    }

    let ruleset = load_embedded_ruleset()
        .map_err(|err| JsValue::from_str(&format!("load embedded ruleset: {err}")))?;
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    let _ = SCANNER.set(scanner);
    SCANNER
        .get()
        .ok_or_else(|| JsValue::from_str("initialize scanner"))
}

impl From<&Issue> for BrowserIssue {
    fn from(issue: &Issue) -> Self {
        Self {
            offset: issue.offset,
            length: issue.length,
            line: issue.line,
            col: issue.col,
            found: issue.found.clone(),
            suggestions: issue.suggestions.iter().cloned().collect(),
            rule_type: issue.rule_type.name().to_owned(),
            severity: issue.severity.name().to_owned(),
            context: issue.context.as_ref().map(ToString::to_string),
            english: issue.english.as_ref().map(ToString::to_string),
        }
    }
}
