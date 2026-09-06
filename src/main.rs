use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};

mod cli;

use cli::args::{help_text, parse_args, Cli, Command, LintArgs};
use cli::lint::{content_type_for, run_lint_batch, LintBatchParams};

/// Text failed a gate: too many errors or warnings.  The input was linted
/// successfully; the answer is "no".
const EXIT_GATE: i32 = 1;

/// The tool could not do its job: bad arguments, unreadable config, a file it
/// could not process.  Distinct from [EXIT_GATE] so CI can tell "your prose
/// needs work" from "this run is meaningless".
const EXIT_FAILURE: i32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let default_log = if args.iter().any(|a| a == "--debug") {
        "debug"
    } else if args.iter().any(|a| a == "--verbose") {
        "info"
    } else {
        "warn"
    };
    zhtw_mcp::trace::init(default_log);

    // Debug formatting rather than alternate Display: that is what anyhow's own
    // Termination impl used before this function stopped returning Result, so
    // the multi-line "Caused by:" chain users are reading in CI logs stays as
    // it was. The only thing that changed is the exit code.
    let result = parse_args(&args).and_then(run);
    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        process::exit(EXIT_FAILURE);
    }
}

/// Execute a parsed command line.  Everything that reads the environment, the
/// filesystem, or the network lives here rather than in `parse_args`.
fn run(cli: Cli) -> Result<()> {
    let Cli {
        overrides_path,
        suppressions_path,
        packs_dir,
        active_packs,
        config_path,
        command,
    } = cli;
    let packs_dir = packs_dir.unwrap_or_else(zhtw_mcp::rules::store::default_packs_dir);

    match command {
        Command::Help(topic) => match std::io::stdout().write_all(help_text(topic).as_bytes()) {
            Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
            _ => Ok(()),
        },

        // Setup subcommand: generate integration config for a host editor.
        Command::Setup(host) => {
            if host == "translation-guide" || host == "translation_guide" {
                return run_translation_guide();
            }
            run_setup(&host)
        }

        Command::CacheClear => {
            let mut cache = zhtw_mcp::rules::judgment_cache::JudgmentCache::open_default();
            let count = cache.len();
            cache.clear();
            cache.flush();
            eprintln!("judgment cache cleared ({count} entries removed)");
            Ok(())
        }

        Command::Convert(convert) => run_convert(
            &convert.files,
            convert.content_type.as_deref(),
            overrides_path.unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path),
            packs_dir,
            &active_packs,
            #[cfg(feature = "translate")]
            convert.verify,
        ),

        // TM subcommand: manage translation memory. Respect .zhtw-mcp.toml
        // translation_memory override so "tm record" writes to the same file
        // that lint reads.
        Command::Tm(tm) => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let project_cfg = match &config_path {
                Some(p) => Some(zhtw_mcp::config::ProjectConfig::from_file(p)?),
                None => zhtw_mcp::config::ProjectConfig::discover(&cwd),
            };
            let tm_path = project_cfg
                .as_ref()
                .and_then(|c| c.translation_memory.as_ref().map(PathBuf::from))
                .unwrap_or_else(|| zhtw_mcp::rules::store::discover_tm_path(&cwd));
            run_tm_cmd(
                &tm.cmd,
                tm.arg.as_deref(),
                &tm_path,
                tm.found.as_deref(),
                tm.suggested.as_deref(),
                tm.chose.as_deref(),
                tm.context.as_deref(),
            )
        }

        // Pack subcommand: manage rule packs.
        Command::Pack { cmd, arg } => run_pack_cmd(&cmd, arg.as_deref(), &packs_dir),

        // Lint subcommand: batch mode supporting multiple files.
        Command::Lint(lint) => {
            run_lint(*lint, overrides_path, config_path, packs_dir, active_packs)
        }

        Command::Server => {
            // Server mode: open the stores, then run MCP over stdio.
            //
            // All three store paths fall back to .zhtw-mcp.toml, the same
            // discovery the tm subcommand uses, so a project can point the
            // server at its own stores without every MCP client passing flags.
            // Wiring only some of them would be worse than wiring none: the
            // server would answer from the project's overrides while recording
            // decisions into a different translation memory than lint reads.
            let cwd = std::env::current_dir().unwrap_or_default();
            let project_cfg = match &config_path {
                Some(p) => Some(zhtw_mcp::config::ProjectConfig::from_file(p)?),
                None => zhtw_mcp::config::ProjectConfig::discover(&cwd),
            };
            let cfg_ref = project_cfg.as_ref();
            let overrides_path = overrides_path
                .or_else(|| cfg_ref.and_then(|c| c.overrides.as_ref().map(PathBuf::from)))
                .unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path);
            let suppressions_path = suppressions_path
                .or_else(|| cfg_ref.and_then(|c| c.suppressions.as_ref().map(PathBuf::from)))
                .unwrap_or_else(zhtw_mcp::rules::store::default_suppressions_path);
            let tm_path = cfg_ref
                .and_then(|c| c.translation_memory.as_ref().map(PathBuf::from))
                .unwrap_or_else(|| zhtw_mcp::rules::store::discover_tm_path(&cwd));
            run_server(
                &overrides_path,
                &suppressions_path,
                &tm_path,
                packs_dir,
                active_packs,
            )
        }
    }
}

/// Merge `lint` flags with `.zhtw-mcp.toml`, then run the batch.  CLI flags win
/// over config values, config values win over defaults.
fn run_lint(
    lint: LintArgs,
    overrides_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
    packs_dir: PathBuf,
    mut active_packs: Vec<String>,
) -> Result<()> {
    // Load project config: explicit --config > auto-discover from cwd.
    let project_cfg = match &config_path {
        Some(p) => Some(zhtw_mcp::config::ProjectConfig::from_file(p)?),
        None => {
            let cwd = std::env::current_dir().unwrap_or_default();
            zhtw_mcp::config::ProjectConfig::discover(&cwd)
        }
    };

    let cfg_ref = project_cfg.as_ref();
    let eff_overrides = overrides_path
        .or_else(|| cfg_ref.and_then(|c| c.overrides.as_ref().map(PathBuf::from)))
        .unwrap_or_else(zhtw_mcp::rules::store::default_overrides_path);
    let eff_profile = lint
        .profile
        .as_deref()
        .or_else(|| cfg_ref.and_then(|c| c.profile.as_deref()));
    // CLI --relaxed flag overrides config file relaxed setting.
    let eff_relaxed = lint.relaxed || cfg_ref.and_then(|c| c.relaxed).unwrap_or(false);
    // CLI --exempt-blockquotes flag OR "[markdown] exempt_blockquotes".
    let eff_exempt_blockquotes = lint.exempt_blockquotes
        || cfg_ref
            .and_then(|c| c.markdown.as_ref())
            .and_then(|m| m.exempt_blockquotes)
            .unwrap_or(false);
    let eff_content_type = lint
        .content_type
        .as_deref()
        .or_else(|| cfg_ref.and_then(|c| c.content_type.as_deref()));
    let eff_max_errors = lint
        .max_errors
        .or_else(|| cfg_ref.and_then(|c| c.max_errors))
        .unwrap_or(0);
    let eff_max_warnings = lint
        .max_warnings
        .or_else(|| cfg_ref.and_then(|c| c.max_warnings));

    // Merge exclude patterns: CLI + config.
    let mut exclude_patterns = lint.exclude_patterns;
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

    // Build project glossary from [glossary] section.
    let eff_glossary = cfg_ref
        .and_then(|c| c.glossary.as_ref())
        .map(|g| zhtw_mcp::rules::glossary::ProjectGlossary {
            banned: g.banned.clone().unwrap_or_default(),
            preferred: g.preferred.clone().unwrap_or_default(),
            proper_nouns: g.proper_nouns.clone().unwrap_or_default(),
        })
        .unwrap_or_default();

    // ignore_terms is config-only: there is no CLI flag for it, matching the
    // documented field list in docs/cli.md.
    let eff_ignore_terms: Vec<String> = cfg_ref
        .and_then(|c| c.ignore_terms.clone())
        .unwrap_or_default();

    run_lint_batch(&LintBatchParams {
        file_args: &lint.files,
        format: lint.format,
        max_errors: eff_max_errors,
        max_warnings: eff_max_warnings,
        profile_name: eff_profile,
        content_type_override: eff_content_type,
        overrides_path: &eff_overrides,
        packs_dir: &packs_dir,
        active_packs: &active_packs,
        exclude_patterns: &exclude_patterns,
        fix_mode: lint.fix_mode.unwrap_or(zhtw_mcp::fixer::FixMode::None),
        dry_run: lint.dry_run,
        explain: lint.explain,
        baseline_path: lint.baseline_path.as_deref(),
        update_baseline: lint.update_baseline,
        diff_from: lint.diff_from.as_deref(),
        #[cfg(feature = "translate")]
        verify: lint.verify,
        relaxed: eff_relaxed,
        exempt_blockquotes: eff_exempt_blockquotes,
        detect_ai: lint.detect_ai,
        rhythm: lint.rhythm,
        detect_translationese: lint.detect_translationese,
        detect_style: lint.detect_style,
        translationese_domain: lint.translationese_domain,
        document_genre: lint.document_genre,
        register: lint.register,
        ai_threshold_multiplier: lint.ai_threshold_multiplier,
        tm_path: Some(eff_tm_path),
        glossary: eff_glossary,
        ignore_terms: &eff_ignore_terms,
        consistency: lint.consistency,
        telemetry: lint.telemetry,
    })
}

/// Open the stores and serve MCP over stdio.
fn run_server(
    overrides_path: &Path,
    suppressions_path: &Path,
    tm_path: &Path,
    packs_dir: PathBuf,
    active_packs: Vec<String>,
) -> Result<()> {
    let store = zhtw_mcp::rules::store::OverrideStore::open(overrides_path)?;
    let suppression_store = zhtw_mcp::rules::store::SuppressionStore::open(suppressions_path)?;
    let pack_store = zhtw_mcp::rules::store::PackStore::new(packs_dir);

    // Translation memory: the caller resolved the path, from translation_memory
    // in the project config or by walking up from cwd. A missing or unreadable
    // TM degrades to none with a warning, the same as on the lint path, because
    // it is an optional store rather than a precondition.
    let tm_store = match zhtw_mcp::rules::store::TranslationMemoryStore::open(tm_path) {
        Ok(store) => Some(store),
        Err(e) => {
            tracing::warn!(
                "failed to open translation memory at {}: {e}",
                tm_path.display()
            );
            None
        }
    };

    let server = zhtw_mcp::mcp::tools::Server::new(
        store,
        suppression_store,
        pack_store,
        active_packs,
        tm_store,
    )?;

    tracing::info!("zhtw-mcp server starting on stdio");

    // One stdio connection, one server behind one lock: a worker pool per core
    // would be idle threads. The lint pipeline runs on the blocking pool so it
    // never stalls the protocol loop.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        use rmcp::ServiceExt;
        let service = zhtw_mcp::mcp::sdk::SdkServer::new(server);
        let transport = zhtw_mcp::mcp::transport::stdio(service.lifecycle());
        let running = match service.serve(transport).await {
            Ok(running) => running,

            // A client that closed the pipe before the handshake is a client
            // that went away, not a failure to report: the hand-rolled
            // transport returned cleanly on EOF and supervisors still probe
            // this binary by spawning it and closing stdin.
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(reason)) => {
                tracing::info!("client disconnected before initialize: {reason}");
                return Ok(());
            }

            // A pre-handshake request the SDK answered and then declined to
            // continue from, which in practice is a server/discover missing the
            // per-request metadata its revision requires. The client has its
            // error; ending quietly is the whole of the outcome.
            Err(rmcp::service::ServerInitializeError::ExpectedInitializeRequest(message)) => {
                tracing::warn!("client ended the session before initialize: {message:?}");
                return Ok(());
            }

            // The client asked for a protocol revision this server does not
            // serve and was told so, with the list it can choose from. The
            // handshake failing ends the session, but a negotiation that
            // reached a definite answer is not a crash to report as one.
            Err(rmcp::service::ServerInitializeError::InitializeFailed(error)) => {
                tracing::warn!("handshake refused: {error:?}");
                return Ok(());
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        };
        running.waiting().await?;
        Ok::<(), anyhow::Error>(())
    });

    // End of input is bounded on purpose: the transport waits a fixed while for
    // responses still owed and then reports EOF regardless. Dropping the
    // runtime here would undo that, because a runtime waits for its blocking
    // tasks with no deadline and the lint runs on that pool. A scan wedged past
    // the drain would hold the process open indefinitely, having already been
    // given up on. Shutting down with a deadline keeps the exit bounded;
    // whatever the scan still held is lost either way, and the judgment cache
    // is flushed on the exit path rather than here.
    runtime.shutdown_timeout(zhtw_mcp::mcp::transport::BLOCKING_SHUTDOWN_GRACE);
    outcome
}

/// Built-in SC→TC conversion (character/phrase level via embedded OpenCC
/// dictionaries) then zhtw-mcp aggressive fix for context-aware zh-TW
/// phrase correction. No external OpenCC dependency required.
/// `verify` opts into the Google Translate anchor check, which sends the
/// sentences around each remaining issue off the machine.  Off by default:
/// conversion is otherwise entirely local, and a converter that phones home
/// unless told not to is the wrong default for anyone holding an unpublished
/// document.
fn run_convert(
    file_args: &[String],
    content_type_str: Option<&str>,
    overrides_path: PathBuf,
    packs_dir: PathBuf,
    active_packs: &[String],
    #[cfg(feature = "translate")] verify: bool,
) -> Result<()> {
    use zhtw_mcp::engine::scan::Scanner;
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

    // Step 1: SC→TC character/phrase conversion (built-in, no OpenCC
    // dependency).
    let s2t = zhtw_mcp::engine::s2t::S2TConverter::new();
    let s2t_output = s2t.convert(&raw_input);

    // Step 2: Build scanner with overrides.
    let store = OverrideStore::open(&overrides_path)?;
    let ruleset = load_embedded_ruleset()?;

    // The active packs, not an empty selection. Passing "&[]" here meant every
    // convert ran against the unpacked ruleset: --pack parsed, the command
    // succeeded, and the answer was computed from rules the caller had asked to
    // add. That reached the fix loop below as well as --verify, so the rewrite
    // itself was wrong, not just the verification of it.
    let (spelling_rules, case_rules) = zhtw_mcp::rules::store::build_merged_rules(
        &ruleset.spelling_rules,
        &ruleset.case_rules,
        &store,
        &zhtw_mcp::rules::store::PackStore::new(packs_dir),
        active_packs,
    );
    let scanner = Scanner::new(spelling_rules, case_rules);

    // Determine content type. Same rule as the lint path. Keeping a second copy
    // here had convert reading .markdown and README.MD as plain text, so it
    // rewrote what a code fence was there to protect.
    let content_type = content_type_for(
        content_type_str,
        file_args
            .iter()
            .find(|a| *a != "--")
            .map_or("", |f| f.as_str()),
    );

    // Step 3: Iterative fix loop, scan + fix until convergence or max rounds.
    let mut text = s2t_output;
    let max_rounds = 3;
    for round in 0..max_rounds {
        let excluded =
            zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, content_type);
        let scan_out = scanner.scan_with_prebuilt_excluded(
            &text,
            &excluded,
            zhtw_mcp::rules::ruleset::Profile::Base,
            content_type,
        );
        let issues = scan_out.issues;

        if issues.is_empty() {
            break;
        }

        let fix_result = apply_fixes_with_context(
            &text,
            &issues,
            FixMode::LexicalContextual,
            &excluded,
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

    // Step 4: Optional verification via Google Translate. Requires --verify;
    // see the note on this function.
    #[cfg(feature = "translate")]
    if verify {
        zhtw_mcp::engine::translate::refuse_if_network_disabled("--verify")
            .map_err(|e| anyhow::anyhow!(e))?;
    }
    #[cfg(feature = "translate")]
    if verify {
        let excluded =
            zhtw_mcp::engine::scan::build_exclusions_for_content_type(&text, content_type);
        let scan_out = scanner.scan_with_prebuilt_excluded(
            &text,
            &excluded,
            zhtw_mcp::rules::ruleset::Profile::Base,
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
