use std::collections::BTreeSet;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};

// ANSI color helpers for human-format output

/// Whether stderr supports ANSI colors.
fn use_color() -> bool {
    // Respect NO_COLOR env var (https://no-color.org/).
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}

struct Colors {
    red: &'static str,
    yellow: &'static str,
    cyan: &'static str,
    dim: &'static str,
    bold: &'static str,
    reset: &'static str,
}

const COLORS_ON: Colors = Colors {
    red: "\x1b[31m",
    yellow: "\x1b[33m",
    cyan: "\x1b[36m",
    dim: "\x1b[2m",
    bold: "\x1b[1m",
    reset: "\x1b[0m",
};

const COLORS_OFF: Colors = Colors {
    red: "",
    yellow: "",
    cyan: "",
    dim: "",
    bold: "",
    reset: "",
};

fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();

    // Parse CLI args:
    //   zhtw-mcp                                — run MCP server (default paths)
    //   zhtw-mcp --overrides <path>             — custom overrides JSON path
    //   zhtw-mcp --suppressions <path>          — custom suppressions JSON path
    //   zhtw-mcp --pack <name>                  — activate a rule pack (repeatable)
    //   zhtw-mcp lint <file|--> [--format json|compact]  — lint file(s) or stdin
    //                           [--max-errors N]
    //                           [--profile P] [--detect-ai]
    //                           [--content-type plain|markdown|yaml]
    //   zhtw-mcp setup <host>                   — generate agentic editor integration config
    //   zhtw-mcp pack import <file>             — install a pack
    //   zhtw-mcp pack export <name>             — export a pack
    //   zhtw-mcp pack validate <file>           — validate a pack file
    //   zhtw-mcp pack list                      — list available packs
    let mut overrides_path: Option<PathBuf> = None;
    let mut suppressions_path: Option<PathBuf> = None;
    let mut packs_dir: Option<PathBuf> = None;
    let mut active_packs: Vec<String> = Vec::new();
    let mut lint_files: Vec<String> = Vec::new();
    let mut lint_format = LintFormat::Human;
    let mut max_errors: Option<usize> = None;
    let mut max_warnings: Option<usize> = None;
    let mut profile_str: Option<String> = None;
    let mut content_type_str: Option<String> = None;
    let mut exclude_patterns: Vec<String> = Vec::new();
    let mut config_path: Option<PathBuf> = None;
    let mut fix_mode: Option<zhtw_mcp::fixer::FixMode> = None;
    let mut dry_run = false;
    let mut explain = false;
    let mut detect_ai = false;
    let mut ai_threshold_multiplier: f32 = 1.0;
    let mut baseline_path: Option<PathBuf> = None;
    let mut update_baseline = false;
    let mut diff_from: Option<String> = None;
    #[cfg(feature = "translate")]
    let mut verify = false;
    let mut setup_host: Option<String> = None;
    let mut pack_cmd: Option<String> = None;
    let mut pack_arg: Option<String> = None;
    let mut tm_cmd: Option<String> = None;
    let mut tm_arg: Option<String> = None;
    let mut tm_record_found: Option<String> = None;
    let mut tm_record_suggested: Option<String> = None;
    let mut tm_record_chose: Option<String> = None;
    let mut tm_record_context: Option<String> = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--overrides" | "--db" => {
                i += 1;
                overrides_path = Some(PathBuf::from(
                    args.get(i).context("--overrides requires a path")?,
                ));
            }
            "--pack" => {
                i += 1;
                active_packs.push(args.get(i).context("--pack requires a name")?.clone());
            }
            "--packs-dir" => {
                i += 1;
                packs_dir = Some(PathBuf::from(
                    args.get(i).context("--packs-dir requires a path")?,
                ));
            }
            "lint" => {
                i += 1;
                // Collect all non-flag arguments as files.
                while i < args.len() {
                    match args[i].as_str() {
                        "--format" => {
                            i += 1;
                            let fmt = args.get(i).context("--format requires a value")?;
                            lint_format = match fmt.as_str() {
                                "json" => LintFormat::Json,
                                "human" => LintFormat::Human,
                                "sarif" => LintFormat::Sarif,
                                "compact" => LintFormat::Compact,
                                "tabular" => LintFormat::Tabular,
                                _ => anyhow::bail!(
                                    "unknown format: {fmt} (expected 'json', 'human', 'sarif', 'compact', or 'tabular')"
                                ),
                            };
                        }
                        "--max-errors" => {
                            i += 1;
                            let v: usize = args
                                .get(i)
                                .context("--max-errors requires a number")?
                                .parse()
                                .context("--max-errors must be a non-negative integer")?;
                            max_errors = Some(v);
                        }
                        "--max-warnings" => {
                            i += 1;
                            max_warnings = Some(
                                args.get(i)
                                    .context("--max-warnings requires a number")?
                                    .parse()
                                    .context("--max-warnings must be a non-negative integer")?,
                            );
                        }
                        "--profile" => {
                            i += 1;
                            profile_str =
                                Some(args.get(i).context("--profile requires a value")?.clone());
                        }
                        "--content-type" => {
                            i += 1;
                            let ct = args.get(i).context("--content-type requires a value")?;
                            match ct.as_str() {
                                "plain" | "markdown" | "markdown-scan-code" | "yaml" => {
                                    content_type_str = Some(ct.clone())
                                }
                                _ => anyhow::bail!(
                                    "unknown content-type: {ct} (expected 'plain', 'markdown', 'markdown-scan-code', or 'yaml')"
                                ),
                            }
                        }
                        "--exclude" => {
                            i += 1;
                            exclude_patterns
                                .push(args.get(i).context("--exclude requires a pattern")?.clone());
                        }
                        "--fix" | "--fix=lexical_safe" => {
                            fix_mode = Some(zhtw_mcp::fixer::FixMode::LexicalSafe);
                        }
                        "--fix=orthographic" => {
                            fix_mode = Some(zhtw_mcp::fixer::FixMode::Orthographic);
                        }
                        "--fix=lexical_contextual" => {
                            fix_mode = Some(zhtw_mcp::fixer::FixMode::LexicalContextual);
                        }
                        arg if arg.starts_with("--fix=") => {
                            anyhow::bail!(
                                "unknown fix mode: {} (expected 'orthographic', 'lexical_safe', or 'lexical_contextual')",
                                &arg[6..]
                            );
                        }
                        "--dry-run" => {
                            dry_run = true;
                        }
                        "--explain" => {
                            explain = true;
                        }
                        "--baseline" => {
                            i += 1;
                            baseline_path = Some(PathBuf::from(
                                args.get(i).context("--baseline requires a file path")?,
                            ));
                        }
                        "--update-baseline" => {
                            update_baseline = true;
                        }
                        "--diff-from" => {
                            i += 1;
                            diff_from = Some(
                                args.get(i)
                                    .context("--diff-from requires a git ref")?
                                    .clone(),
                            );
                        }
                        "--detect-ai" => {
                            detect_ai = true;
                            // Peek at next arg for optional threshold level.
                            if let Some(next) = args.get(i + 1) {
                                match next.as_str() {
                                    "low" => {
                                        ai_threshold_multiplier = 0.5;
                                        i += 1;
                                    }
                                    "medium" => {
                                        ai_threshold_multiplier = 1.0;
                                        i += 1;
                                    }
                                    "high" => {
                                        ai_threshold_multiplier = 1.5;
                                        i += 1;
                                    }
                                    _ => {} // not a threshold level, leave for next iteration
                                }
                            }
                        }
                        #[cfg(feature = "translate")]
                        "--verify" => {
                            verify = true;
                        }
                        #[cfg(not(feature = "translate"))]
                        "--verify" => {
                            anyhow::bail!("--verify requires the 'translate' feature (rebuild without --no-default-features)");
                        }
                        _ => {
                            lint_files.push(args[i].clone());
                        }
                    }
                    i += 1;
                }
                if lint_files.is_empty() {
                    anyhow::bail!("lint requires at least one file path or '--' for stdin");
                }
            }
            "setup" => {
                i += 1;
                setup_host = Some(args.get(i).context("setup requires a host name")?.clone());
            }
            "convert" => {
                // convert subcommand: SC→TW pipeline (built-in s2t + zhtw-mcp fix).
                // Reads SC text from files or stdin, outputs corrected zh-TW.
                i += 1;
                let mut convert_files: Vec<String> = Vec::new();
                let mut convert_content_type: Option<String> = None;
                while i < args.len() {
                    match args[i].as_str() {
                        "--content-type" => {
                            i += 1;
                            convert_content_type = Some(
                                args.get(i)
                                    .context("--content-type requires a value")?
                                    .clone(),
                            );
                        }
                        "--" => {
                            convert_files.push("--".into());
                        }
                        arg if arg.starts_with('-') => {
                            anyhow::bail!("unknown convert flag: {arg}");
                        }
                        _ => {
                            convert_files.push(args[i].clone());
                        }
                    }
                    i += 1;
                }
                if convert_files.is_empty() {
                    convert_files.push("--".into()); // default: stdin
                }
                return run_convert(
                    &convert_files,
                    convert_content_type.as_deref(),
                    overrides_path.unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path),
                );
            }
            "tm" => {
                i += 1;
                let subcmd = args
                    .get(i)
                    .context("tm requires a subcommand (list|export|import|clear|record)")?
                    .clone();
                match subcmd.as_str() {
                    "export" | "import" => {
                        i += 1;
                        tm_arg = Some(
                            args.get(i)
                                .context(format!("tm {subcmd} requires a file path"))?
                                .clone(),
                        );
                    }
                    "record" => {
                        // Parse --found, --suggested, --chose, --context key-value args.
                        i += 1;
                        while i < args.len() && args[i].starts_with("--") {
                            match args[i].as_str() {
                                "--found" => {
                                    i += 1;
                                    tm_record_found = Some(
                                        args.get(i).context("--found requires a value")?.clone(),
                                    );
                                }
                                "--suggested" => {
                                    i += 1;
                                    tm_record_suggested = Some(
                                        args.get(i)
                                            .context("--suggested requires a value")?
                                            .clone(),
                                    );
                                }
                                "--chose" => {
                                    i += 1;
                                    tm_record_chose = Some(
                                        args.get(i).context("--chose requires a value")?.clone(),
                                    );
                                }
                                "--context" => {
                                    i += 1;
                                    tm_record_context = Some(
                                        args.get(i).context("--context requires a value")?.clone(),
                                    );
                                }
                                other => {
                                    anyhow::bail!("unknown tm record flag: {other}");
                                }
                            }
                            i += 1;
                        }
                        // Back up one so the outer loop's i += 1 doesn't skip.
                        i -= 1;
                    }
                    "list" | "clear" => {}
                    _ => {} // let run_tm_cmd report the error
                }
                tm_cmd = Some(subcmd);
            }
            "pack" => {
                i += 1;
                let subcmd = args
                    .get(i)
                    .context("pack requires a subcommand (import|export|validate|list)")?
                    .clone();
                // Only consume a trailing arg for subcommands that need one.
                match subcmd.as_str() {
                    "import" | "export" | "validate" => {
                        i += 1;
                        pack_arg = Some(
                            args.get(i)
                                .context(format!("pack {subcmd} requires an argument"))?
                                .clone(),
                        );
                    }
                    "list" => {} // no argument
                    _ => {}      // let run_pack_cmd report the error
                }
                pack_cmd = Some(subcmd);
            }
            "--suppressions" => {
                i += 1;
                suppressions_path = Some(PathBuf::from(
                    args.get(i).context("--suppressions requires a path")?,
                ));
            }
            "--config" => {
                i += 1;
                config_path = Some(PathBuf::from(
                    args.get(i).context("--config requires a path")?,
                ));
            }
            _ => {
                anyhow::bail!("unknown argument: {}", args[i]);
            }
        }
        i += 1;
    }

    let packs_dir = packs_dir.unwrap_or_else(zhtw_mcp::rules::store::default_packs_dir);

    // Setup subcommand: generate integration config for a host editor.
    if let Some(ref host_str) = setup_host {
        if host_str == "translation-guide" || host_str == "translation_guide" {
            return run_translation_guide();
        }
        return run_setup(host_str);
    }

    // TM subcommand: manage translation memory.
    // Respect .zhtw-mcp.toml translation_memory override so `tm record`
    // writes to the same file that `lint` reads.
    if let Some(cmd) = tm_cmd {
        let cwd = std::env::current_dir().unwrap_or_default();
        let project_cfg = match &config_path {
            Some(p) => zhtw_mcp::config::ProjectConfig::from_file(p).ok(),
            None => zhtw_mcp::config::ProjectConfig::discover(&cwd),
        };
        let tm_path = project_cfg
            .as_ref()
            .and_then(|c| c.translation_memory.as_ref().map(PathBuf::from))
            .unwrap_or_else(|| zhtw_mcp::rules::store::discover_tm_path(&cwd));
        return run_tm_cmd(
            &cmd,
            tm_arg.as_deref(),
            &tm_path,
            tm_record_found.as_deref(),
            tm_record_suggested.as_deref(),
            tm_record_chose.as_deref(),
            tm_record_context.as_deref(),
        );
    }

    // Pack subcommand: manage rule packs.
    if let Some(cmd) = pack_cmd {
        return run_pack_cmd(&cmd, pack_arg.as_deref(), &packs_dir);
    }

    // Lint subcommand: batch mode supporting multiple files.
    if !lint_files.is_empty() {
        // Load project config: explicit --config > auto-discover from cwd.
        let project_cfg = match &config_path {
            Some(p) => Some(zhtw_mcp::config::ProjectConfig::from_file(p)?),
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                zhtw_mcp::config::ProjectConfig::discover(&cwd)
            }
        };

        // Merge: CLI flags override config, config overrides defaults.
        let cfg_ref = project_cfg.as_ref();
        let eff_overrides = overrides_path
            .or_else(|| cfg_ref.and_then(|c| c.overrides.as_ref().map(PathBuf::from)))
            .unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path);
        let eff_profile = profile_str
            .as_deref()
            .or_else(|| cfg_ref.and_then(|c| c.profile.as_deref()));
        let eff_content_type = content_type_str
            .as_deref()
            .or_else(|| cfg_ref.and_then(|c| c.content_type.as_deref()));
        let eff_max_errors = max_errors
            .or_else(|| cfg_ref.and_then(|c| c.max_errors))
            .unwrap_or(0);
        let eff_max_warnings = max_warnings.or_else(|| cfg_ref.and_then(|c| c.max_warnings));

        // Merge exclude patterns: CLI + config.
        if let Some(cfg_exclude) = cfg_ref.and_then(|c| c.exclude.as_ref()) {
            for pat in cfg_exclude {
                if !exclude_patterns.contains(pat) {
                    exclude_patterns.push(pat.clone());
                }
            }
        }

        // Merge packs: CLI + config.
        if let Some(cfg_packs) = cfg_ref.and_then(|c| c.packs.as_ref()) {
            for p in cfg_packs {
                if !active_packs.contains(p) {
                    active_packs.push(p.clone());
                }
            }
        }

        // Resolve TM path: config override > auto-discover from cwd.
        let eff_tm_path = cfg_ref
            .and_then(|c| c.translation_memory.as_ref().map(PathBuf::from))
            .unwrap_or_else(|| {
                let cwd = std::env::current_dir().unwrap_or_default();
                zhtw_mcp::rules::store::discover_tm_path(&cwd)
            });

        return run_lint_batch(&LintBatchParams {
            file_args: &lint_files,
            format: lint_format,
            max_errors: eff_max_errors,
            max_warnings: eff_max_warnings,
            profile_name: eff_profile,
            content_type_override: eff_content_type,
            overrides_path: &eff_overrides,
            packs_dir: &packs_dir,
            active_packs: &active_packs,
            exclude_patterns: &exclude_patterns,
            fix_mode: fix_mode.unwrap_or(zhtw_mcp::fixer::FixMode::None),
            dry_run,
            explain,
            baseline_path: baseline_path.as_deref(),
            update_baseline,
            diff_from: diff_from.as_deref(),
            #[cfg(feature = "translate")]
            verify,
            detect_ai,
            ai_threshold_multiplier,
            tm_path: Some(eff_tm_path),
        });
    }

    // Reject lint-only flags used without lint subcommand.
    if content_type_str.is_some() {
        anyhow::bail!("--content-type is only valid with the 'lint' subcommand");
    }

    // Server mode: open override store, then run MCP over stdio.
    let overrides_path =
        overrides_path.unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path);

    let suppressions_path =
        suppressions_path.unwrap_or_else(zhtw_mcp::rules::store::default_suppressions_path);
    let store = zhtw_mcp::rules::store::OverrideStore::open(&overrides_path)?;
    let suppression_store = zhtw_mcp::rules::store::SuppressionStore::open(&suppressions_path)?;
    let pack_store = zhtw_mcp::rules::store::PackStore::new(packs_dir);

    // Discover translation memory in project root.
    let tm_store = {
        let cwd = std::env::current_dir().unwrap_or_default();
        let tm_path = zhtw_mcp::rules::store::discover_tm_path(&cwd);
        match zhtw_mcp::rules::store::TranslationMemoryStore::open(&tm_path) {
            Ok(store) => Some(store),
            Err(e) => {
                log::warn!(
                    "failed to open translation memory at {}: {e}",
                    tm_path.display()
                );
                None
            }
        }
    };

    let mut server = zhtw_mcp::mcp::tools::Server::new(
        store,
        suppression_store,
        pack_store,
        active_packs,
        tm_store,
    )?;

    log::info!("zhtw-mcp server starting on stdio");

    #[cfg(feature = "async-transport")]
    {
        log::info!("using async transport (tokio)");
        zhtw_mcp::mcp::transport_async::run_async_stdio(&mut server)?;
    }
    #[cfg(not(feature = "async-transport"))]
    {
        zhtw_mcp::mcp::transport::run_stdio(&mut server)?;
    }

    Ok(())
}

// Lint subcommand

#[derive(Clone, Copy)]
enum LintFormat {
    Human,
    Json,
    Sarif,
    Compact,
    Tabular,
}

// Typed output structs for direct serialization (no Value tree allocation).

#[derive(serde::Serialize)]
struct CliFileOutput {
    file: String,
    detected_script: String,
    issues: Vec<zhtw_mcp::rules::ruleset::Issue>,
    total: usize,
    errors: usize,
    warnings: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    tm_suppressed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixes_applied: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixes_skipped: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<zhtw_mcp::engine::ai_score::AiSignatureReport>,
}

#[derive(serde::Serialize)]
struct SarifDocument<'a> {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: [SarifRun<'a>; 1],
}

#[derive(serde::Serialize)]
struct SarifRun<'a> {
    tool: SarifTool,
    results: &'a [SarifResult],
}

#[derive(serde::Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifDriver {
    name: &'static str,
    version: &'static str,
    information_uri: &'static str,
    rules: Vec<SarifRuleDef>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRuleDef {
    id: String,
    short_description: SarifMessage,
}

#[derive(serde::Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: String,
    level: &'static str,
    message: SarifMessage,
    locations: [SarifLocation; 1],
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifLocation {
    physical_location: SarifPhysicalLocation,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifPhysicalLocation {
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifArtifactLocation {
    uri: String,
    uri_base_id: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRegion {
    start_line: usize,
    start_column: usize,
    byte_offset: usize,
    byte_length: usize,
}

struct LintBatchParams<'a> {
    file_args: &'a [String],
    format: LintFormat,
    max_errors: usize,
    max_warnings: Option<usize>,
    profile_name: Option<&'a str>,
    content_type_override: Option<&'a str>,
    overrides_path: &'a Path,
    packs_dir: &'a Path,
    active_packs: &'a [String],
    exclude_patterns: &'a [String],
    fix_mode: zhtw_mcp::fixer::FixMode,
    dry_run: bool,
    explain: bool,
    baseline_path: Option<&'a Path>,
    update_baseline: bool,
    diff_from: Option<&'a str>,
    #[cfg(feature = "translate")]
    verify: bool,
    detect_ai: bool,
    ai_threshold_multiplier: f32,
    tm_path: Option<PathBuf>,
}

fn run_lint_batch(params: &LintBatchParams<'_>) -> Result<()> {
    let c = if use_color() { &COLORS_ON } else { &COLORS_OFF };

    let profile = params
        .profile_name
        .map(zhtw_mcp::rules::ruleset::Profile::from_str_lossy)
        .unwrap_or(zhtw_mcp::rules::ruleset::Profile::Default);

    // Build effective config: profile base + detect_ai override.
    let mut cfg = profile.config();
    if params.detect_ai {
        cfg.ai_filler_detection = true;
        cfg.ai_semantic_safety = true;
        cfg.ai_density_detection = true;
        cfg.ai_structural_patterns = true;
        cfg.ai_threshold_multiplier = params.ai_threshold_multiplier;
    }

    // Build scanner once for all files, merging overrides + active packs.
    let ruleset = zhtw_mcp::rules::loader::load_embedded_ruleset()?;
    let store = zhtw_mcp::rules::store::OverrideStore::open(params.overrides_path)?;
    let pack_store = zhtw_mcp::rules::store::PackStore::new(params.packs_dir.to_path_buf());

    let (spelling_rules, case_rules) = zhtw_mcp::rules::store::build_merged_rules(
        &ruleset.spelling_rules,
        &ruleset.case_rules,
        &store,
        &pack_store,
        params.active_packs,
    );
    let ruleset_hash = zhtw_mcp::rules::loader::compute_ruleset_hash(&spelling_rules, &case_rules);
    let scanner = zhtw_mcp::engine::scan::Scanner::new(spelling_rules, case_rules);
    let s2t = zhtw_mcp::engine::s2t::S2TConverter::new();

    // Open translation memory (if path provided and file exists/creatable).
    let tm_store = params.tm_path.as_ref().and_then(|p| {
        zhtw_mcp::rules::store::TranslationMemoryStore::open(p)
            .map_err(|e| log::warn!("failed to open TM at {}: {e}", p.display()))
            .ok()
    });

    // Scan cache: skip re-scanning unchanged files (lint-only, no fix).
    // Disabled when --verify is active (calibrate_issues needs the full text).
    // Wrapped in Mutex for rayon parallel scanning.
    let mut use_cache = params.fix_mode == zhtw_mcp::fixer::FixMode::None;
    #[cfg(feature = "translate")]
    if params.verify {
        use_cache = false;
    }
    let scan_cache =
        use_cache.then(|| std::sync::Mutex::new(zhtw_mcp::cache::ScanCache::open_default()));

    // --diff-from: resolve changed files via git, use as file args.
    let diff_files: Vec<String>;
    let file_args = if let Some(git_ref) = params.diff_from {
        diff_files = resolve_diff_files(git_ref)?;
        &diff_files
    } else {
        params.file_args
    };

    // Resolve directories into individual files; de-duplicate and sort.
    let resolved = resolve_file_args(file_args, params.exclude_patterns)?;
    let multi = resolved.len() > 1;
    let mut total_errors: usize = 0;
    let mut total_warnings: usize = 0;
    let mut all_file_results: Vec<CliFileOutput> = Vec::new();
    let mut sarif_results: Vec<SarifResult> = Vec::new();
    let mut sarif_rules: std::collections::BTreeMap<String, SarifRuleDef> =
        std::collections::BTreeMap::new();

    // Load baseline if provided.
    let mut baseline = params
        .baseline_path
        .map(zhtw_mcp::baseline::Baseline::load)
        .transpose()?
        .unwrap_or_default();
    let mut baseline_count: usize = 0;
    let mut tabular_header_printed = false;

    /// Maximum file size for CLI lint mode (16 MiB).
    const MAX_CLI_FILE_BYTES: u64 = 16 * 1024 * 1024;

    // Phase 1: Read + S2T + cache check + scan.
    //
    // This closure is shared between sequential and parallel (rayon) paths.
    // It captures only &-refs to immutable state plus the Mutex-wrapped cache,
    // making it Fn + Send + Sync.
    let fix_mode_str = format!("{:?}", params.fix_mode);
    let scan_file = |file_arg: &str| -> Result<(
        String,
        bool,
        zhtw_mcp::engine::scan::ScanOutput,
        zhtw_mcp::engine::scan::ContentType,
    )> {
        let content_type = match params.content_type_override {
            Some("markdown") => zhtw_mcp::engine::scan::ContentType::Markdown,
            Some("markdown-scan-code") => zhtw_mcp::engine::scan::ContentType::MarkdownScanCode,
            Some("yaml") => zhtw_mcp::engine::scan::ContentType::Yaml,
            Some(_) | None => {
                let lower = file_arg.to_ascii_lowercase();
                if lower.ends_with(".md") || lower.ends_with(".markdown") {
                    zhtw_mcp::engine::scan::ContentType::Markdown
                } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
                    zhtw_mcp::engine::scan::ContentType::Yaml
                } else {
                    zhtw_mcp::engine::scan::ContentType::Plain
                }
            }
        };

        let cache_params = zhtw_mcp::cache::ScanParams {
            ruleset_hash: ruleset_hash.clone(),
            profile: profile.name().to_owned(),
            content_type: format!("{content_type:?}"),
            fix_mode: fix_mode_str.clone(),
            detect_ai: params.detect_ai,
            ai_threshold: format!("{:.1}", params.ai_threshold_multiplier),
        };

        // Open file via fd, stat from the fd (TOCTOU-safe).
        // Check cache BEFORE reading — fast path avoids file I/O entirely.
        if file_arg != "--" {
            let file =
                std::fs::File::open(file_arg).with_context(|| format!("open file: {file_arg}"))?;
            let meta = file
                .metadata()
                .with_context(|| format!("stat file: {file_arg}"))?;
            anyhow::ensure!(
                meta.len() <= MAX_CLI_FILE_BYTES,
                "{file_arg}: file too large ({} bytes, limit {MAX_CLI_FILE_BYTES})",
                meta.len()
            );

            // Fast-path: check mtime+size before reading the file.
            let fast_hit = scan_cache.as_ref().and_then(|mtx| {
                let mut c = mtx.lock().ok()?;
                let mtime = zhtw_mcp::cache::mtime_secs(&meta);
                c.check_fast(file_arg, mtime, meta.len(), &cache_params)
                    .into_hit()
            });
            if let Some(hit) = fast_hit {
                if !hit.input_was_sc {
                    // Cache hit for non-SC file — skip file read and scan.
                    return Ok((String::new(), false, hit.output, content_type));
                }
                // SC files need the text for S2T write-back; fall through.
            }

            // Slow path: read file from the same fd.
            let mut text = String::with_capacity(meta.len() as usize);
            std::io::BufReader::new(file)
                .read_to_string(&mut text)
                .with_context(|| format!("read file: {file_arg}"))?;

            let input_was_sc = zhtw_mcp::engine::zhtype::detect_chinese_type(&text)
                == zhtw_mcp::engine::zhtype::ChineseType::Simplified;
            if input_was_sc {
                text = s2t.convert(&text);
            }

            // Slow-path cache: check content hash (mtime missed but content
            // may be unchanged, e.g. after `touch`).
            let content_hit = scan_cache.as_ref().and_then(|mtx| {
                let mut c = mtx.lock().ok()?;
                c.check_content(file_arg, text.as_bytes(), &cache_params)
            });
            let output = match content_hit {
                Some(hit) => hit.output,
                None => {
                    let o = scanner.scan_for_content_type_with_config(&text, content_type, cfg);
                    if let Some(Ok(mut c)) = scan_cache.as_ref().map(|mtx| mtx.lock()) {
                        let mtime = zhtw_mcp::cache::mtime_secs(&meta);
                        c.put(
                            file_arg,
                            text.as_bytes(),
                            mtime,
                            meta.len(),
                            &cache_params,
                            o.clone(),
                            input_was_sc,
                        );
                    }
                    o
                }
            };

            // Drop text eagerly when not needed for fix/write-back/verify
            // to avoid accumulating all files' text in parallel scans.
            let mut need_text = input_was_sc || params.fix_mode != zhtw_mcp::fixer::FixMode::None;
            #[cfg(feature = "translate")]
            if params.verify {
                need_text = true;
            }
            if !need_text {
                text = String::new();
            }

            return Ok((text, input_was_sc, output, content_type));
        }

        // stdin path.
        let mut text = String::new();
        std::io::stdin()
            .take(MAX_CLI_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .context("read stdin")?;
        anyhow::ensure!(
            text.len() as u64 <= MAX_CLI_FILE_BYTES,
            "stdin input exceeds {MAX_CLI_FILE_BYTES} byte limit"
        );

        let input_was_sc = zhtw_mcp::engine::zhtype::detect_chinese_type(&text)
            == zhtw_mcp::engine::zhtype::ChineseType::Simplified;
        if input_was_sc {
            text = s2t.convert(&text);
        }
        let output = scanner.scan_for_content_type_with_config(&text, content_type, cfg);

        Ok((text, input_was_sc, output, content_type))
    };

    // Parallel scan when multiple files and no stdin pipe.
    // Rayon parallelism gives N/cores speedup on multi-file lint.
    let has_stdin = resolved.iter().any(|f| f == "--");
    let scan_results: Vec<
        Result<(
            String,
            bool,
            zhtw_mcp::engine::scan::ScanOutput,
            zhtw_mcp::engine::scan::ContentType,
        )>,
    > = if resolved.len() > 1 && !has_stdin {
        use rayon::prelude::*;
        resolved.par_iter().map(|f| scan_file(f)).collect()
    } else {
        resolved.iter().map(|f| scan_file(f)).collect()
    };

    // Phase 2: Fix + report (always sequential for ordered output).
    for (file_arg, scan_result) in resolved.iter().zip(scan_results) {
        let (text, input_was_sc, output, content_type) = scan_result?;

        let detected_script = if input_was_sc {
            "simplified"
        } else {
            output.detected_script.name()
        };
        let mut ai_signature = output.ai_signature;
        let issues = output.issues;

        let scan = |input: &str| -> zhtw_mcp::engine::scan::ScanOutput {
            scanner.scan_for_content_type_with_config(input, content_type, cfg)
        };

        // Apply fixes if requested. Filter out TM-suppressed issues so the
        // fixer does not auto-correct terms the user deliberately rejected.
        let fix_result = if params.fix_mode != zhtw_mcp::fixer::FixMode::None {
            let fix_issues: Vec<_> = if let Some(ref tm) = tm_store {
                issues
                    .iter()
                    .filter(|i| !tm.should_suppress(&i.found))
                    .cloned()
                    .collect()
            } else {
                issues.clone()
            };
            Some(zhtw_mcp::fixer::apply_fixes_with_context(
                &text,
                &fix_issues,
                params.fix_mode,
                &[],
                Some(scanner.segmenter()),
            ))
        } else {
            None
        };

        // Write fixed text (unless --dry-run).
        // Text is written when either S2T conversion was applied or ruleset fixes were made.
        let fix_applied = fix_result.as_ref().map_or(0, |f| f.applied);
        let has_text_changes = input_was_sc || fix_applied > 0;
        if has_text_changes {
            let output_text = fix_result
                .as_ref()
                .map_or(text.as_str(), |f| f.text.as_str());
            let s2t_label = if input_was_sc && fix_applied == 0 {
                " (S2T only)"
            } else {
                ""
            };
            if params.dry_run {
                eprintln!(
                    "{}{}{}: {} fix(es) would be applied{s2t_label} {}(dry run){}",
                    c.bold, file_arg, c.reset, fix_applied, c.dim, c.reset
                );
            } else if file_arg == "--" {
                // stdin: emit fixed text to stdout.
                print!("{}", output_text);
            } else {
                // Atomic write: tempfile + rename in the same directory.
                let file_path = Path::new(file_arg);
                let parent = file_path.parent().unwrap_or(Path::new("."));
                let mut tmp = tempfile::NamedTempFile::new_in(parent)
                    .with_context(|| format!("create tempfile in {}", parent.display()))?;
                std::io::Write::write_all(&mut tmp, output_text.as_bytes())
                    .with_context(|| format!("write tempfile for {file_arg}"))?;
                tmp.persist(file_path)
                    .with_context(|| format!("rename tempfile to {file_arg}"))?;
                eprintln!(
                    "{}{}{}: {} fix(es) applied{s2t_label}",
                    c.bold, file_arg, c.reset, fix_applied
                );
            }
        }

        // Count remaining issues after fix/S2T (rescan converted text).
        // Single rescan serves both issue reporting and AI signature refresh.
        let report_issues = if has_text_changes && !params.dry_run {
            let rescan_text = fix_result
                .as_ref()
                .map_or(text.as_str(), |f| f.text.as_str());
            let rescan_output = scan(rescan_text);
            // Refresh AI signature from the fixed text (avoids a second scan).
            let ai_active = cfg.ai_filler_detection
                || cfg.ai_semantic_safety
                || cfg.ai_density_detection
                || cfg.ai_structural_patterns;
            if ai_active {
                ai_signature = rescan_output.ai_signature;
            }
            let mut rescan = rescan_output.issues;
            if let Some(ref fix) = fix_result {
                // Suppress convergent-chain noise from the fixer's own replacements.
                zhtw_mcp::fixer::suppress_convergent_issues(&mut rescan, &fix.applied_fixes);
            }
            rescan
        } else {
            issues
        };

        // --verify: calibrate issues via Google Translate.
        #[cfg(feature = "translate")]
        let report_issues = if params.verify {
            let calibrate_text = if has_text_changes && !params.dry_run {
                fix_result
                    .as_ref()
                    .map_or(text.as_str(), |f| f.text.as_str())
            } else {
                &text
            };
            let mut issues_mut = report_issues;
            let result =
                zhtw_mcp::engine::translate::calibrate_issues(calibrate_text, &mut issues_mut);
            eprintln!(
                "{}  verify: {} matched, {} unmatched, {} no_english, api_ok={}{}",
                c.dim, result.matched, result.unmatched, result.no_english, result.api_ok, c.reset,
            );
            issues_mut
        } else {
            report_issues
        };

        // Apply TM suppressions: downgrade rejected terms to Info severity.
        // Only lexical/contextual issue types; orthographic types are immune.
        let mut tm_suppressed: usize = 0;
        let report_issues = if let Some(ref tm) = tm_store {
            let mut issues = report_issues;
            for issue in &mut issues {
                match issue.rule_type {
                    zhtw_mcp::rules::ruleset::IssueType::Punctuation
                    | zhtw_mcp::rules::ruleset::IssueType::Case
                    | zhtw_mcp::rules::ruleset::IssueType::Variant
                    | zhtw_mcp::rules::ruleset::IssueType::Grammar
                    | zhtw_mcp::rules::ruleset::IssueType::AiStyle => continue,
                    _ => {}
                }
                if tm.should_suppress(&issue.found)
                    && issue.severity != zhtw_mcp::rules::ruleset::Severity::Info
                {
                    issue.severity = zhtw_mcp::rules::ruleset::Severity::Info;
                    tm_suppressed += 1;
                }
            }
            issues
        } else {
            report_issues
        };

        // --update-baseline: add all issues to the baseline.
        if params.update_baseline {
            for issue in &report_issues {
                baseline.insert(file_arg, issue);
            }
        }

        // --baseline: filter out baseline issues, count them separately.
        let new_issues: Vec<_> = if params.baseline_path.is_some() && !params.update_baseline {
            report_issues
                .iter()
                .filter(|i| {
                    if baseline.contains(file_arg, i) {
                        baseline_count += 1;
                        false
                    } else {
                        true
                    }
                })
                .cloned()
                .collect()
        } else {
            report_issues.clone()
        };

        let error_count = new_issues
            .iter()
            .filter(|i| i.severity == zhtw_mcp::rules::ruleset::Severity::Error)
            .count();
        let warning_count = new_issues
            .iter()
            .filter(|i| i.severity == zhtw_mcp::rules::ruleset::Severity::Warning)
            .count();
        total_errors += error_count;
        total_warnings += warning_count;

        // Use new_issues for reporting (baseline issues filtered out).
        let report_issues = new_issues;

        match params.format {
            LintFormat::Json => {
                let output = CliFileOutput {
                    file: file_arg.clone(),
                    detected_script: detected_script.to_string(),
                    total: report_issues.len(),
                    issues: report_issues.clone(),
                    errors: error_count,
                    warnings: warning_count,
                    tm_suppressed: if tm_suppressed > 0 {
                        Some(tm_suppressed)
                    } else {
                        None
                    },
                    fixes_applied: fix_result.as_ref().map(|f| f.applied),
                    fixes_skipped: fix_result.as_ref().map(|f| f.skipped),
                    ai_signature: ai_signature.clone(),
                };
                if multi {
                    all_file_results.push(output);
                } else {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
            }
            LintFormat::Human => {
                let prefix = if multi {
                    format!("{}{file_arg}{}:", c.bold, c.reset)
                } else {
                    String::new()
                };
                if report_issues.is_empty() {
                    eprintln!("{prefix}{}No issues found.{}", c.dim, c.reset);
                } else {
                    for issue in &report_issues {
                        let sev_color = match issue.severity {
                            zhtw_mcp::rules::ruleset::Severity::Error => c.red,
                            zhtw_mcp::rules::ruleset::Severity::Warning => c.yellow,
                            zhtw_mcp::rules::ruleset::Severity::Info => c.cyan,
                        };
                        let sev = issue.severity.name();
                        let rule_name = issue.rule_type.name();
                        let suggestions =
                            if issue.suggestions.len() == 1 && issue.suggestions[0].is_empty() {
                                "(delete)".to_string()
                            } else {
                                issue.suggestions.join(", ")
                            };
                        let verify_tag = match issue.anchor_match {
                            Some(true) => " [verified]",
                            Some(false) => " [unverified]",
                            None => "",
                        };
                        eprintln!(
                            "{prefix}{}:{}: {}{}{} {}[{}]{} '{}{}{}' -> {}{}",
                            issue.line,
                            issue.col,
                            sev_color,
                            sev,
                            c.reset,
                            c.dim,
                            rule_name,
                            c.reset,
                            c.bold,
                            issue.found,
                            c.reset,
                            suggestions,
                            verify_tag,
                        );
                        if params.explain {
                            if let Some(ctx) = &issue.context {
                                eprintln!("  {}context:{} {ctx}", c.dim, c.reset);
                            }
                            if let Some(eng) = &issue.english {
                                eprintln!("  {}english:{} {eng}", c.dim, c.reset);
                            }
                        }
                    }
                    eprintln!(
                        "\n{prefix}{}{} issue(s) found.{}",
                        c.bold,
                        report_issues.len(),
                        c.reset
                    );
                }
                // AI signature score (when computed).
                if let Some(ref sig) = ai_signature {
                    let level = if sig.score >= 0.7 {
                        "high"
                    } else if sig.score >= 0.4 {
                        "medium"
                    } else {
                        "low"
                    };
                    eprintln!(
                        "{prefix}{}AI score:{} {:.2} ({level})",
                        c.cyan, c.reset, sig.score
                    );
                    for signal in &sig.top_signals {
                        eprintln!("  {}{signal}{}", c.dim, c.reset);
                    }
                }
            }
            LintFormat::Compact => {
                // Grep-style one-line-per-issue, deduplicated for LLM token efficiency.
                // Format: file:line:col:S:rule:from→to
                // Uses shared Issue::compact_dedup_key() / Severity::letter().
                use std::collections::HashMap;

                // Group by dedup key, preserving first-occurrence order via index.
                type CompactKey<'a> = (&'a str, &'a str, String, &'a str);
                struct CompactGroup {
                    first_loc: (usize, usize),
                    locs: Vec<(usize, usize)>,
                    context: Option<String>,
                    english: Option<String>,
                }
                let mut groups: HashMap<CompactKey<'_>, CompactGroup> = HashMap::new();
                let mut order: Vec<CompactKey<'_>> = Vec::new();
                for issue in &report_issues {
                    let key = issue.compact_dedup_key();
                    let group = groups.entry(key.clone()).or_insert_with(|| {
                        order.push(key);
                        CompactGroup {
                            first_loc: (issue.line, issue.col),
                            locs: Vec::new(),
                            context: issue.context.clone(),
                            english: issue.english.clone(),
                        }
                    });
                    group.locs.push((issue.line, issue.col));
                }

                let file_prefix = if file_arg == "--" {
                    String::new()
                } else {
                    let display_path = std::env::current_dir()
                        .ok()
                        .and_then(|cwd| {
                            Path::new(file_arg)
                                .strip_prefix(&cwd)
                                .ok()
                                .map(|p| p.to_string_lossy().into_owned())
                        })
                        .unwrap_or_else(|| file_arg.clone());
                    format!("{display_path}:")
                };

                // Emit in source order (first occurrence of each group).
                order.sort_by_key(|k| groups[k].first_loc);
                for key in &order {
                    let (found, rt, sug_key, sev) = key;
                    let group = &groups[key];
                    // Render suggestion: first entry + count of alternatives.
                    let parts: Vec<&str> = sug_key.split('|').collect();
                    let display_sug = if parts.len() <= 1 {
                        parts.first().copied().unwrap_or("?").to_string()
                    } else {
                        format!("{}+{}", parts[0], parts.len() - 1)
                    };
                    if group.locs.len() == 1 {
                        print!(
                            "{file_prefix}{}:{}:{sev}:{rt}:{found}\u{2192}{display_sug}",
                            group.locs[0].0, group.locs[0].1
                        );
                    } else {
                        let rest: Vec<String> = group.locs[1..]
                            .iter()
                            .map(|(l, c)| format!("{l}:{c}"))
                            .collect();
                        print!(
                            "{file_prefix}{}:{}:{sev}:{rt}:{found}\u{2192}{display_sug} (\u{00d7}{} also at {})",
                            group.first_loc.0, group.first_loc.1,
                            group.locs.len(),
                            rest.join(",")
                        );
                    }
                    // --explain: append context/english on the same line.
                    // Sanitize newlines to preserve one-line-per-issue format.
                    if params.explain {
                        if let Some(ctx) = &group.context {
                            let sanitized = ctx.replace('\n', " ");
                            print!(" [{sanitized}]");
                        }
                        if let Some(eng) = &group.english {
                            print!(" ({eng})");
                        }
                    }
                    println!();
                }
            }
            LintFormat::Tabular => {
                use std::fmt::Write as FmtWrite;
                use zhtw_mcp::mcp::tools::{
                    compress_locations, escape_tsv_field, group_issues, shorten_severity,
                    shorten_type,
                };

                let groups = group_issues(&report_issues, params.explain);

                let file_prefix = if file_arg == "--" {
                    String::new()
                } else {
                    let display_path = std::env::current_dir()
                        .ok()
                        .and_then(|cwd| {
                            Path::new(file_arg)
                                .strip_prefix(&cwd)
                                .ok()
                                .map(|p| p.to_string_lossy().into_owned())
                        })
                        .unwrap_or_else(|| file_arg.clone());
                    format!("{display_path}:")
                };

                if !report_issues.is_empty() {
                    if !tabular_header_printed {
                        if params.explain {
                            println!("found\tsug\ttype\tsev\tn\tloc\texpl");
                        } else {
                            println!("found\tsug\ttype\tsev\tn\tloc");
                        }
                        tabular_header_printed = true;
                    }
                    for ((found, rt, _, sev), group) in &groups {
                        let sug_str =
                            if group.suggestions.len() == 1 && group.suggestions[0].is_empty() {
                                "(delete)".to_string()
                            } else {
                                group
                                    .suggestions
                                    .iter()
                                    .map(|s| escape_tsv_field(s))
                                    .collect::<Vec<_>>()
                                    .join(",")
                            };
                        // When a file prefix is present, each location
                        // must be individually prefixed so consumers can
                        // parse "file:L:C,file:L:C" tuples correctly.
                        let loc_str = if file_prefix.is_empty() {
                            compress_locations(&group.locs)
                        } else {
                            group
                                .locs
                                .iter()
                                .map(|(l, c)| format!("{file_prefix}{l}:{c}"))
                                .collect::<Vec<_>>()
                                .join(",")
                        };
                        let loc_escaped = escape_tsv_field(&loc_str);
                        let mut line = String::new();
                        let _ = write!(
                            line,
                            "{}\t{sug_str}\t{}\t{}\t{}\t{loc_escaped}",
                            escape_tsv_field(found),
                            shorten_type(rt),
                            shorten_severity(sev),
                            group.count,
                        );
                        if params.explain {
                            if let Some(ref expl) = group.explanation {
                                let _ = write!(line, "\t{}", escape_tsv_field(expl));
                            } else {
                                line.push('\t');
                            }
                        }
                        println!("{line}");
                    }
                }
            }
            LintFormat::Sarif => {
                for issue in &report_issues {
                    let rule_name = issue.rule_type.name();
                    let rule_id = format!("zhtw-mcp/{rule_name}");
                    let level = match issue.severity {
                        zhtw_mcp::rules::ruleset::Severity::Error => "error",
                        zhtw_mcp::rules::ruleset::Severity::Warning => "warning",
                        zhtw_mcp::rules::ruleset::Severity::Info => "note",
                    };

                    sarif_rules
                        .entry(rule_id.clone())
                        .or_insert_with(|| SarifRuleDef {
                            id: rule_id.clone(),
                            short_description: SarifMessage {
                                text: format!("{rule_name} check"),
                            },
                        });

                    let sugg_text =
                        if issue.suggestions.len() == 1 && issue.suggestions[0].is_empty() {
                            "(delete)".to_string()
                        } else {
                            issue.suggestions.join(", ")
                        };

                    sarif_results.push(SarifResult {
                        rule_id,
                        level,
                        message: SarifMessage {
                            text: format!("'{}' -> {sugg_text}", issue.found),
                        },
                        locations: [SarifLocation {
                            physical_location: SarifPhysicalLocation {
                                artifact_location: SarifArtifactLocation {
                                    uri: file_arg.to_string(),
                                    uri_base_id: "%SRCROOT%",
                                },
                                region: SarifRegion {
                                    start_line: issue.line,
                                    start_column: issue.col,
                                    byte_offset: issue.offset,
                                    byte_length: issue.length,
                                },
                            },
                        }],
                    });
                }
            }
        }
    }

    // Multi-file JSON: emit array of per-file results.
    if multi && matches!(params.format, LintFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&all_file_results)?);
    }

    // --update-baseline: save the baseline file.
    if params.update_baseline {
        let bl_path = params
            .baseline_path
            .context("--update-baseline requires --baseline <file>")?;
        baseline.save(bl_path)?;
        eprintln!(
            "{}Baseline updated:{} {} fingerprint(s) in {}",
            c.dim,
            c.reset,
            baseline.len(),
            bl_path.display()
        );
    }

    // Report baseline summary if filtering was active.
    if params.baseline_path.is_some() && !params.update_baseline && baseline_count > 0 {
        eprintln!(
            "{}{baseline_count} baseline issue(s) suppressed.{}",
            c.dim, c.reset
        );
    }

    // SARIF: emit the complete SARIF v2.1.0 document.
    if matches!(params.format, LintFormat::Sarif) {
        let sarif = SarifDocument {
            schema: "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json",
            version: "2.1.0",
            runs: [SarifRun {
                tool: SarifTool {
                    driver: SarifDriver {
                        name: "zhtw-mcp",
                        version: env!("CARGO_PKG_VERSION"),
                        information_uri: "https://github.com/aspect-build/zhtw-mcp",
                        rules: sarif_rules.into_values().collect(),
                    },
                },
                results: &sarif_results,
            }],
        };
        println!("{}", serde_json::to_string_pretty(&sarif)?);
    }

    // Flush scan cache before potential process::exit (which skips Drop).
    if let Some(ref cache_mtx) = scan_cache {
        if let Ok(mut c) = cache_mtx.lock() {
            c.flush();
        }
    }

    // Exit 1 if total error-severity or warning-severity issues exceed thresholds.
    let errors_exceeded = total_errors > params.max_errors;
    let warnings_exceeded = params
        .max_warnings
        .is_some_and(|limit| total_warnings > limit);
    if errors_exceeded || warnings_exceeded {
        process::exit(1);
    }

    Ok(())
}

// Convert subcommand: SC → TW pipeline

/// Built-in SC→TC conversion (character/phrase level via embedded OpenCC
/// dictionaries) then zhtw-mcp aggressive fix for context-aware zh-TW
/// phrase correction. No external OpenCC dependency required.
fn run_convert(
    file_args: &[String],
    content_type_str: Option<&str>,
    overrides_path: PathBuf,
) -> Result<()> {
    use zhtw_mcp::engine::scan::{ContentType, Scanner};
    use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
    use zhtw_mcp::rules::loader::load_embedded_ruleset;
    use zhtw_mcp::rules::store::OverrideStore;

    // Read input (files or stdin).
    let mut raw_input = String::new();
    for arg in file_args {
        if arg == "--" {
            std::io::stdin()
                .read_to_string(&mut raw_input)
                .context("failed to read stdin")?;
        } else {
            let content =
                std::fs::read_to_string(arg).with_context(|| format!("failed to read {arg}"))?;
            raw_input.push_str(&content);
        }
    }

    // Step 1: SC→TC character/phrase conversion (built-in, no OpenCC dependency).
    let s2t = zhtw_mcp::engine::s2t::S2TConverter::new();
    let s2t_output = s2t.convert(&raw_input);

    // Step 2: Build scanner with overrides.
    let store = OverrideStore::open(&overrides_path)?;
    let ruleset = load_embedded_ruleset()?;
    let (spelling_rules, case_rules) = zhtw_mcp::rules::store::build_merged_rules(
        &ruleset.spelling_rules,
        &ruleset.case_rules,
        &store,
        &zhtw_mcp::rules::store::PackStore::new(zhtw_mcp::rules::store::default_packs_dir()),
        &[],
    );
    let scanner = Scanner::new(spelling_rules, case_rules);

    // Determine content type.
    let content_type = match content_type_str {
        Some("markdown" | "md") => ContentType::Markdown,
        Some("markdown-scan-code") => ContentType::MarkdownScanCode,
        Some("yaml" | "yml") => ContentType::Yaml,
        Some("plain") => ContentType::Plain,
        _ => {
            // Auto-detect from first file extension.
            let first_file = file_args.iter().find(|a| *a != "--");
            match first_file
                .and_then(|f| Path::new(f).extension())
                .and_then(|e| e.to_str())
            {
                Some("md") => ContentType::Markdown,
                Some("yml" | "yaml") => ContentType::Yaml,
                _ => ContentType::Plain,
            }
        }
    };

    // Step 3: Iterative fix loop — scan + fix until convergence or max rounds.
    let mut text = s2t_output;
    let max_rounds = 3;
    for round in 0..max_rounds {
        let excluded =
            zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, content_type);
        let scan_out = scanner.scan_with_prebuilt_excluded(
            &text,
            &excluded,
            zhtw_mcp::rules::ruleset::Profile::Default,
            content_type,
        );
        let issues = scan_out.issues;

        if issues.is_empty() {
            break;
        }

        let excluded_pairs: Vec<(usize, usize)> =
            excluded.iter().map(|r| (r.start, r.end)).collect();
        let fix_result = apply_fixes_with_context(
            &text,
            &issues,
            FixMode::LexicalContextual,
            &excluded_pairs,
            Some(scanner.segmenter()),
        );

        if fix_result.applied == 0 {
            break;
        }

        eprintln!(
            "convert: round {} — {} issues, {} fixes applied",
            round + 1,
            issues.len(),
            fix_result.applied,
        );
        text = fix_result.text;
    }

    // Step 4: Final verification via Google Translate (if feature enabled).
    #[cfg(feature = "translate")]
    {
        let excluded =
            zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, content_type);
        let scan_out = scanner.scan_with_prebuilt_excluded(
            &text,
            &excluded,
            zhtw_mcp::rules::ruleset::Profile::Default,
            content_type,
        );
        let mut remaining = scan_out.issues;
        if !remaining.is_empty() {
            let cr = zhtw_mcp::engine::translate::calibrate_issues(&text, &mut remaining);
            eprintln!(
                "convert: verify — {} matched, {} unmatched, {} no_english, api_ok={}",
                cr.matched, cr.unmatched, cr.no_english, cr.api_ok,
            );
            let rejected_count = remaining
                .iter()
                .filter(|i| i.anchor_match == Some(false))
                .count();
            let no_signal_count = remaining
                .iter()
                .filter(|i| i.anchor_match.is_none() && i.english.is_some())
                .count();
            if rejected_count + no_signal_count > 0 {
                eprintln!(
                    "convert: {} residual issues ({} unconfirmed, {} no signal)",
                    rejected_count + no_signal_count,
                    rejected_count,
                    no_signal_count,
                );
            }
        }
    }

    // Output the corrected text.
    print!("{text}");

    Ok(())
}

// Setup subcommand

fn run_setup(host_str: &str) -> Result<()> {
    use zhtw_mcp::mcp::setup::{self, Host};

    let host = match Host::from_name(host_str) {
        Some(h) => h,
        None => {
            let hosts: Vec<&str> = setup::ALL_HOSTS.iter().map(|h| h.name()).collect();
            anyhow::bail!(
                "unknown host: '{host_str}'. Available: {}",
                hosts.join(", ")
            );
        }
    };

    let output = setup::generate_for_host(host);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn run_translation_guide() -> Result<()> {
    let output = zhtw_mcp::mcp::setup::generate_translation_guide();
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// Pack subcommand

fn run_tm_cmd(
    cmd: &str,
    arg: Option<&str>,
    tm_path: &std::path::Path,
    record_found: Option<&str>,
    record_suggested: Option<&str>,
    record_chose: Option<&str>,
    record_context: Option<&str>,
) -> Result<()> {
    use zhtw_mcp::rules::store::{iso_date_today, TmEntry, TranslationMemoryStore};

    match cmd {
        "list" => {
            let store = TranslationMemoryStore::open(tm_path)?;
            let entries = store.list();
            if entries.is_empty() {
                eprintln!("Translation memory is empty.");
            } else {
                let json = serde_json::to_string_pretty(entries)?;
                println!("{json}");
            }
            Ok(())
        }
        "export" => {
            let dest = arg.context("tm export requires a file path")?;
            let store = TranslationMemoryStore::open(tm_path)?;
            store.export(Path::new(dest))?;
            eprintln!("Exported TM ({} entries) to {dest}", store.list().len());
            Ok(())
        }
        "import" => {
            let src = arg.context("tm import requires a file path")?;
            let mut store = TranslationMemoryStore::open(tm_path)?;
            let (added, updated) = store.import(Path::new(src))?;
            eprintln!(
                "Imported {added} new, {updated} updated ({} total)",
                store.list().len()
            );
            Ok(())
        }
        "clear" => {
            let mut store = TranslationMemoryStore::open(tm_path)?;
            store.clear()?;
            eprintln!("Translation memory cleared.");
            Ok(())
        }
        "record" => {
            let found = record_found.context("tm record requires --found")?;
            let suggested = record_suggested.context("tm record requires --suggested")?;
            let chose = record_chose.context("tm record requires --chose")?;

            let mut store = TranslationMemoryStore::open(tm_path)?;
            store.record(TmEntry {
                found: found.to_string(),
                scanner_suggested: suggested.to_string(),
                user_chose: chose.to_string(),
                context: record_context.map(String::from),
                timestamp: iso_date_today(),
            })?;
            eprintln!("Recorded: '{found}' -> chose '{chose}'");
            Ok(())
        }
        _ => {
            anyhow::bail!(
                "unknown tm subcommand: '{cmd}' (expected list|export|import|clear|record)"
            );
        }
    }
}

fn run_pack_cmd(cmd: &str, arg: Option<&str>, packs_dir: &std::path::Path) -> Result<()> {
    use zhtw_mcp::rules::store::PackStore;

    let pack_store = PackStore::new(packs_dir.to_path_buf());

    match cmd {
        "list" => {
            let packs = pack_store.list();
            if packs.is_empty() {
                eprintln!("No packs installed in {}", packs_dir.display());
            } else {
                for pack in &packs {
                    let desc = pack
                        .metadata
                        .as_ref()
                        .and_then(|m| m.description.as_deref())
                        .unwrap_or("");
                    eprintln!(
                        "  {} ({} spelling, {} case){}",
                        pack.name,
                        pack.spelling_count,
                        pack.case_count,
                        if desc.is_empty() {
                            String::new()
                        } else {
                            format!(" — {desc}")
                        },
                    );
                }
            }
            Ok(())
        }
        "import" => {
            let source = arg.context("pack import requires a file path")?;
            let source_path = std::path::Path::new(source);
            let name = source_path
                .file_stem()
                .context("cannot determine pack name from file path")?
                .to_string_lossy();
            pack_store.install(&name, source_path)?;
            eprintln!("Installed pack '{name}' to {}", packs_dir.display());
            Ok(())
        }
        "export" => {
            let name = arg.context("pack export requires a pack name")?;
            let dest = format!("{name}.json");
            pack_store.export(name, std::path::Path::new(&dest))?;
            eprintln!("Exported pack '{name}' to {dest}");
            Ok(())
        }
        "validate" => {
            let file = arg.context("pack validate requires a file path")?;
            let warnings = PackStore::validate(std::path::Path::new(file))?;
            if warnings.is_empty() {
                eprintln!("Pack is valid.");
            } else {
                for w in &warnings {
                    eprintln!("  warning: {w}");
                }
                eprintln!("{} warning(s).", warnings.len());
            }
            Ok(())
        }
        _ => {
            anyhow::bail!(
                "unknown pack subcommand: '{cmd}' (expected import|export|validate|list)"
            );
        }
    }
}

// Helpers

// Diff-from: resolve changed files via git

/// Resolve files changed since a given git ref.
fn resolve_diff_files(git_ref: &str) -> Result<Vec<String>> {
    // Reject refs starting with - to prevent git flag injection.
    // Command::new does not invoke a shell, but a ref like --output=x
    // would still be interpreted as a git flag by the subprocess.
    anyhow::ensure!(
        !git_ref.starts_with('-'),
        "--diff-from ref must not start with '-'"
    );
    anyhow::ensure!(
        git_ref
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-~^@{}".contains(c)),
        "--diff-from ref contains invalid characters"
    );

    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", &format!("{git_ref}...HEAD")])
        .output()
        .context("run git diff --name-only")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| {
            // Only include supported extensions.
            let lower = l.to_ascii_lowercase();
            lower
                .rsplit_once('.')
                .is_some_and(|(_, ext)| SUPPORTED_EXTENSIONS.contains(&ext))
        })
        .map(String::from)
        .collect();

    Ok(files)
}

// Directory walking for multi-file linting

/// Supported file extensions for recursive directory discovery.
const SUPPORTED_EXTENSIONS: &[&str] = &["md", "markdown", "yml", "yaml", "txt"];

/// Resolve a list of file/directory arguments into a deduplicated, sorted list
/// of file paths.  Directories are expanded recursively; hidden entries and
/// symlinks are skipped; --exclude patterns are applied.
fn resolve_file_args(args: &[String], exclude: &[String]) -> Result<Vec<String>> {
    let mut files = BTreeSet::new();

    for arg in args {
        if arg == "--" {
            // stdin sentinel — pass through as-is.
            files.insert("--".to_string());
            continue;
        }

        let path = Path::new(arg);
        if !path.exists() {
            anyhow::bail!("path does not exist: {arg}");
        }

        if path.is_dir() {
            walk_directory(path, &mut files, exclude)?;
        } else if path.is_file() {
            let canonical = normalize_path(path);
            if !is_excluded(&canonical, exclude) {
                files.insert(canonical);
            }
        }
        // Skip symlinks and other non-file/non-dir entries.
    }

    if files.is_empty() {
        anyhow::bail!("no supported files found in the given paths");
    }

    Ok(files.into_iter().collect())
}

/// Recursively walk a directory, collecting supported files.
fn walk_directory(dir: &Path, files: &mut BTreeSet<String>, exclude: &[String]) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read directory: {}", dir.display()))?
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                eprintln!("warning: {}: {err}", dir.display());
                None
            }
        })
        .collect();

    // Deterministic: sort entries lexicographically by file name.
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let ft = entry
            .file_type()
            .with_context(|| format!("file type: {}", entry.path().display()))?;

        // Skip symlinks.
        if ft.is_symlink() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files/directories.
        if name_str.starts_with('.') {
            continue;
        }

        let path = entry.path();

        if ft.is_dir() {
            walk_directory(&path, files, exclude)?;
        } else if ft.is_file() {
            // Check extension.
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_ascii_lowercase();
                if SUPPORTED_EXTENSIONS.contains(&ext_lower.as_str()) {
                    let canonical = normalize_path(&path);
                    if !is_excluded(&canonical, exclude) {
                        files.insert(canonical);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Normalize a path to a string for consistent deduplication.
fn normalize_path(path: &Path) -> String {
    match path.canonicalize() {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// Check if a file path matches any --exclude pattern.
///
/// Supported patterns:
/// - *.ext — match files with the given extension
/// - dir/** — match anything under the given directory component
/// - Literal path-component match as a fallback
fn is_excluded(path: &str, patterns: &[String]) -> bool {
    for pat in patterns {
        if pat.starts_with("*.") {
            // Extension match: *.tmp, *.bak
            let ext = &pat[1..]; // ".tmp"
            if path.ends_with(ext) {
                return true;
            }
        } else if pat.ends_with("/**") {
            // Directory component match: vendor/** matches /path/to/vendor/file.md
            // but not /path/to/some_vendor/file.md.
            let prefix = &pat[..pat.len() - 3];
            let sep_prefix = format!("/{prefix}/");
            if path.contains(&sep_prefix) || path.ends_with(&format!("/{prefix}")) {
                return true;
            }
        } else {
            // Path-component match: check if any path component equals the pattern.
            let sep_pat = format!("/{pat}/");
            if path.contains(&sep_pat)
                || path.ends_with(&format!("/{pat}"))
                || path.starts_with(&format!("{pat}/"))
            {
                return true;
            }
        }
    }
    false
}
