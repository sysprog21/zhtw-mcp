// Command-line parsing: argv in, a typed Cli out.
//
// Parsing is a pure function over argv (parse_args) and execution is a separate
// dispatch (run in main.rs). They were one 845-line main that mixed the two,
// which meant every flag combination could only be tested by spawning the
// binary. Keep them separate: nothing here may touch the filesystem, the
// environment, or the network, so the flag matrix stays unit-testable.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::cli::help;
use crate::cli::render::LintFormat;

/// Refused the same way on both subcommands that accept `--verify`, so the
/// two cannot drift into telling the user different things.
#[cfg(not(feature = "translate"))]
const VERIFY_NEEDS_TRANSLATE: &str =
    "--verify requires the 'translate' feature; rebuild with --features translate";

/// A fully parsed command line: global flags plus the selected subcommand.
pub(crate) struct Cli {
    pub(crate) overrides_path: Option<PathBuf>,
    pub(crate) suppressions_path: Option<PathBuf>,
    pub(crate) packs_dir: Option<PathBuf>,
    pub(crate) active_packs: Vec<String>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) command: Command,
}

/// The subcommand to run.  Absent subcommand means MCP server over stdio.
pub(crate) enum Command {
    Server,
    Lint(Box<LintArgs>),
    Convert(ConvertArgs),
    Setup(String),
    Pack { cmd: String, arg: Option<String> },
    Tm(TmArgs),
    CacheClear,
    Help(HelpTopic),
}

/// Which help message to print.  A `--help`/`-h` anywhere on the line selects
/// the topic of the first subcommand name anywhere on the line, or the global
/// one if there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelpTopic {
    Global,
    Lint,
    Convert,
    Setup,
    Pack,
    Tm,
    Cache,
}

/// The help message for a topic.
pub(crate) fn help_text(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Global => help::GLOBAL,
        HelpTopic::Lint => help::LINT,
        HelpTopic::Convert => help::CONVERT,
        HelpTopic::Setup => help::SETUP,
        HelpTopic::Pack => help::PACK,
        HelpTopic::Tm => help::TM,
        HelpTopic::Cache => help::CACHE,
    }
}

/// Flags accepted after the `lint` subcommand.  Everything that is not a known
/// flag is a file path, which is why `lint` consumes the rest of the argv.
pub(crate) struct LintArgs {
    pub(crate) files: Vec<String>,
    pub(crate) format: LintFormat,
    pub(crate) max_errors: Option<usize>,
    pub(crate) max_warnings: Option<usize>,
    pub(crate) profile: Option<String>,
    pub(crate) content_type: Option<String>,
    pub(crate) exclude_patterns: Vec<String>,
    pub(crate) fix_mode: Option<zhtw_mcp::fixer::FixMode>,
    pub(crate) dry_run: bool,
    pub(crate) explain: bool,
    pub(crate) relaxed: bool,
    pub(crate) exempt_blockquotes: bool,
    pub(crate) consistency: bool,
    pub(crate) detect_ai: bool,
    pub(crate) detect_translationese: bool,
    /// Advisory rhythm (氣口) axis: over-long sentences, sentence-ending
    /// monotony, and a relaxed 定語堆疊 gate. Composes with any profile.
    pub(crate) rhythm: bool,
    /// Emit the composite three-axis scorecard.  Set only by `--detect-style`,
    /// which also flips detect_ai and detect_translationese.
    pub(crate) detect_style: bool,
    pub(crate) translationese_domain: zhtw_mcp::engine::translationese_score::TranslationeseDomain,
    pub(crate) document_genre: zhtw_mcp::rules::ruleset::DocumentGenre,
    pub(crate) ai_threshold_multiplier: f32,
    pub(crate) baseline_path: Option<PathBuf>,
    pub(crate) update_baseline: bool,
    pub(crate) diff_from: Option<String>,
    #[cfg(feature = "translate")]
    pub(crate) verify: bool,
    pub(crate) telemetry: bool,
}

impl Default for LintArgs {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            format: LintFormat::Human,
            max_errors: None,
            max_warnings: None,
            profile: None,
            content_type: None,
            exclude_patterns: Vec::new(),
            fix_mode: None,
            dry_run: false,
            explain: false,
            relaxed: false,
            exempt_blockquotes: false,
            consistency: false,
            detect_ai: false,
            detect_translationese: false,
            rhythm: false,
            detect_style: false,
            translationese_domain:
                zhtw_mcp::engine::translationese_score::TranslationeseDomain::General,
            document_genre: zhtw_mcp::rules::ruleset::DocumentGenre::Casual,
            ai_threshold_multiplier: 1.0,
            baseline_path: None,
            update_baseline: false,
            diff_from: None,
            #[cfg(feature = "translate")]
            verify: false,
            telemetry: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct ConvertArgs {
    pub(crate) files: Vec<String>,
    pub(crate) content_type: Option<String>,
    #[cfg(feature = "translate")]
    pub(crate) verify: bool,
}

#[derive(Default)]
pub(crate) struct TmArgs {
    pub(crate) cmd: String,
    pub(crate) arg: Option<String>,
    pub(crate) found: Option<String>,
    pub(crate) suggested: Option<String>,
    pub(crate) chose: Option<String>,
    pub(crate) context: Option<String>,
}

/// Read a required path value, keeping each flag's own missing-value message.
fn path_value(value: Option<&String>, missing: &'static str) -> Result<PathBuf> {
    Ok(PathBuf::from(value.context(missing)?))
}

/// Validate a `--content-type` value.
///
/// Shared by `lint` and `convert` so a typo is an error in both. `convert` used
/// to accept anything and fall through to auto-detection, which silently gave
/// extension-based behaviour instead of saying the value was wrong.
fn validated_content_type(value: Option<&String>) -> Result<String> {
    let ct = value.context("--content-type requires a value")?;
    match ct.as_str() {
        // convert accepted these two abbreviations before the validator was
        // shared, and run_convert still has arms for them. Normalize instead of
        // rejecting, so sharing the validator does not quietly drop an argument
        // that used to work, and so lint gains them too.
        "md" => Ok("markdown".to_owned()),
        "yml" => Ok("yaml".to_owned()),
        "plain" | "markdown" | "markdown-scan-code" | "yaml" => Ok(ct.clone()),
        _ => anyhow::bail!(
            "unknown content-type: {ct} (expected 'plain', 'markdown', 'markdown-scan-code', or 'yaml')"
        ),
    }
}

/// Read the optional `low|medium|high` level that may follow `--detect-ai` and
/// `--detect-style`.
///
/// Returns `None` when the next argument is not a level, and the caller must
/// then leave the current multiplier alone: `--detect-ai low --detect-style`
/// has to keep the low threshold, not reset it to the default because the
/// second flag carried no level of its own.
fn detect_threshold(next: Option<&String>) -> Option<f32> {
    match next.map(String::as_str) {
        Some("low") => Some(0.5),
        Some("medium") => Some(1.0),
        Some("high") => Some(1.5),
        _ => None,
    }
}

/// Reject a second subcommand rather than letting one win by dispatch order,
/// which is what the flat-variable version did.
fn claim(current: &Command, name: &str) -> Result<()> {
    match current {
        Command::Server => Ok(()),
        _ => anyhow::bail!("only one subcommand is allowed, found a second: {name}"),
    }
}

/// Subcommand name to help topic.  The `--help` check reads this table once at
/// the top of the parse loop; adding a row is the whole of wiring help up for a
/// new subcommand, and the docs cross-check in `build.rs` covers the topic.
const SUBCOMMAND_TOPICS: [(&str, HelpTopic); 6] = [
    ("lint", HelpTopic::Lint),
    ("convert", HelpTopic::Convert),
    ("setup", HelpTopic::Setup),
    ("pack", HelpTopic::Pack),
    ("tm", HelpTopic::Tm),
    ("cache", HelpTopic::Cache),
];

/// Return the help topic from the subcommand name, or `Global` if no subcommand
/// is present.
fn help_topic(args: &[String]) -> Option<HelpTopic> {
    let contain_flag = args.iter().any(|a| a == "--help" || a == "-h");
    if !contain_flag {
        return None;
    }

    let topic = args
        .iter()
        .find_map(|arg| {
            SUBCOMMAND_TOPICS
                .iter()
                .find(|(name, _)| *name == arg)
                .map(|(_, topic)| *topic)
        })
        .unwrap_or(HelpTopic::Global);

    Some(topic)
}

/// Parse argv (including argv[0]) into a `Cli`.
///
/// Pure: no filesystem, environment, or network access.  Defaults that need any
/// of those are resolved in `run`.
pub(crate) fn parse_args(args: &[String]) -> Result<Cli> {
    let mut cli = Cli {
        overrides_path: None,
        suppressions_path: None,
        packs_dir: None,
        active_packs: Vec::new(),
        config_path: None,
        command: Command::Server,
    };
    if let Some(topic) = help_topic(args.get(1..).unwrap_or_default()) {
        cli.command = Command::Help(topic);
        return Ok(cli);
    }
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--overrides" | "--db" => {
                i += 1;
                cli.overrides_path = Some(path_value(args.get(i), "--overrides requires a path")?);
            }
            "--pack" => {
                i += 1;
                cli.active_packs
                    .push(args.get(i).context("--pack requires a name")?.clone());
            }
            "--packs-dir" => {
                i += 1;
                cli.packs_dir = Some(path_value(args.get(i), "--packs-dir requires a path")?);
            }

            "lint" => {
                claim(&cli.command, "lint")?;
                let (lint, used) = parse_lint(&args[i + 1..])?;
                i += used;
                cli.command = Command::Lint(Box::new(lint));
            }
            "setup" => {
                claim(&cli.command, "setup")?;
                i += 1;
                cli.command = Command::Setup(
                    args.get(i)
                        .context("setup requires a host name")?
                        .to_string(),
                );
            }
            "convert" => {
                claim(&cli.command, "convert")?;
                let (convert, used) = parse_convert(&args[i + 1..])?;
                i += used;
                cli.command = Command::Convert(convert);
            }
            "tm" => {
                claim(&cli.command, "tm")?;
                let (tm, used) = parse_tm(&args[i + 1..])?;
                i += used;
                cli.command = Command::Tm(tm);
            }
            "pack" => {
                claim(&cli.command, "pack")?;
                let (cmd, arg, used) = parse_pack(&args[i + 1..])?;
                i += used;
                cli.command = Command::Pack { cmd, arg };
            }
            "cache" => {
                claim(&cli.command, "cache")?;
                i += parse_cache(&args[i + 1..])?;
                cli.command = Command::CacheClear;
            }
            "--suppressions" => {
                i += 1;
                cli.suppressions_path =
                    Some(path_value(args.get(i), "--suppressions requires a path")?);
            }
            "--config" => {
                i += 1;
                cli.config_path = Some(path_value(args.get(i), "--config requires a path")?);
            }
            "--verbose" => {}
            "--debug" => {}
            _ => {
                anyhow::bail!("unknown argument: {}", args[i]);
            }
        }
        i += 1;
    }

    Ok(cli)
}

/// Parse the arguments after `lint`.
///
/// `lint` consumes the rest of the command line: anything that is not a known
/// flag is a file path, so a global flag written after `lint` becomes a path
/// rather than an error. Returns the arguments consumed, like every other
/// subcommand parser here, so `parse_args` advances its cursor the same way for
/// all of them.
fn parse_lint(rest: &[String]) -> Result<(LintArgs, usize)> {
    let mut lint = LintArgs::default();
    let mut i = 0;

    while i < rest.len() {
        match rest[i].as_str() {
            "--format" => {
                i += 1;
                let fmt = rest.get(i).context("--format requires a value")?;
                lint.format = match fmt.as_str() {
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
                lint.max_errors = Some(
                    rest.get(i)
                        .context("--max-errors requires a number")?
                        .parse()
                        .context("--max-errors must be a non-negative integer")?,
                );
            }
            "--max-warnings" => {
                i += 1;
                lint.max_warnings = Some(
                    rest.get(i)
                        .context("--max-warnings requires a number")?
                        .parse()
                        .context("--max-warnings must be a non-negative integer")?,
                );
            }
            "--profile" => {
                i += 1;
                lint.profile = Some(rest.get(i).context("--profile requires a value")?.clone());
            }
            "--relaxed" => {
                lint.relaxed = true;
            }
            "--exempt-blockquotes" => {
                lint.exempt_blockquotes = true;
            }
            "--consistency" => {
                lint.consistency = true;
            }
            "--content-type" => {
                i += 1;
                lint.content_type = Some(validated_content_type(rest.get(i))?);
            }
            "--exclude" => {
                i += 1;
                lint.exclude_patterns
                    .push(rest.get(i).context("--exclude requires a pattern")?.clone());
            }
            "--fix" | "--fix=lexical_safe" => {
                lint.fix_mode = Some(zhtw_mcp::fixer::FixMode::LexicalSafe);
            }
            "--fix=orthographic" => {
                lint.fix_mode = Some(zhtw_mcp::fixer::FixMode::Orthographic);
            }
            "--fix=lexical_contextual" => {
                lint.fix_mode = Some(zhtw_mcp::fixer::FixMode::LexicalContextual);
            }
            arg if arg.starts_with("--fix=") => {
                anyhow::bail!(
                    "unknown fix mode: {} (expected 'orthographic', 'lexical_safe', or 'lexical_contextual')",
                    &arg[6..]
                );
            }
            "--dry-run" => {
                lint.dry_run = true;
            }
            "--explain" => {
                lint.explain = true;
            }
            "--baseline" => {
                i += 1;
                lint.baseline_path =
                    Some(path_value(rest.get(i), "--baseline requires a file path")?);
            }
            "--update-baseline" => {
                lint.update_baseline = true;
            }
            "--diff-from" => {
                i += 1;
                lint.diff_from = Some(
                    rest.get(i)
                        .context("--diff-from requires a git ref")?
                        .clone(),
                );
            }
            "--detect-ai" => {
                lint.detect_ai = true;
                if let Some(mult) = detect_threshold(rest.get(i + 1)) {
                    lint.ai_threshold_multiplier = mult;
                    i += 1;
                }
            }
            "--detect-translationese" => {
                lint.detect_translationese = true;
            }
            "--rhythm" => {
                lint.rhythm = true;
            }
            "--translationese-domain" => {
                // Per-domain threshold calibration for the
                // translationese score: general | technical |
                // literary | news.
                let next = rest.get(i + 1).context(
                    "--translationese-domain requires a value (general|technical|literary|news)",
                )?;
                let domain =
                    zhtw_mcp::engine::translationese_score::TranslationeseDomain::from_str_strict(
                        next,
                    );
                lint.translationese_domain = domain.with_context(|| {
                    format!(
                        "unknown --translationese-domain value '{next}' (expected: general|technical|literary|news)"
                    )
                })?;
                i += 1;
            }
            "--document-genre" => {
                let next = rest
                    .get(i + 1)
                    .context("--document-genre requires a value (casual|technical|financial)")?;
                lint.document_genre = zhtw_mcp::rules::ruleset::DocumentGenre::from_str_strict(next)
                    .with_context(|| format!(
                        "unknown --document-genre value '{next}' (expected: casual|technical|financial)"
                    ))?;
                i += 1;
            }
            "--detect-style" => {
                // Combined shorthand: enable both AI filler and translationese
                // detection. Scores remain orthogonal: reported side by side,
                // never merged.
                lint.detect_ai = true;
                lint.detect_translationese = true;
                lint.detect_style = true;

                // Keep the same optional threshold syntax as --detect-ai.
                if let Some(mult) = detect_threshold(rest.get(i + 1)) {
                    lint.ai_threshold_multiplier = mult;
                    i += 1;
                }
            }
            #[cfg(feature = "translate")]
            "--verify" => {
                lint.verify = true;
            }
            #[cfg(not(feature = "translate"))]
            "--verify" => anyhow::bail!("{VERIFY_NEEDS_TRANSLATE}"),
            "--telemetry" => {
                lint.telemetry = true;
            }
            "--verbose" => {}
            "--debug" => {}
            _ => {
                lint.files.push(rest[i].clone());
            }
        }
        i += 1;
    }

    // --diff-from resolves its own list from the git ref, which is the form
    // docs/cli.md documents ("zhtw-mcp lint --diff-from main"). Requiring a
    // path as well rejected it before the ref was ever consulted.
    if lint.files.is_empty() && lint.diff_from.is_none() {
        anyhow::bail!("lint requires at least one file path or '--' for stdin");
    }
    if lint.detect_style && !matches!(lint.format, LintFormat::Json) {
        anyhow::bail!("--detect-style is only supported with --format json");
    }
    Ok((lint, i))
}

/// Parse the arguments after `convert`, which also consumes the rest of the
/// command line.  With no file arguments it reads stdin.
fn parse_convert(rest: &[String]) -> Result<(ConvertArgs, usize)> {
    let mut convert = ConvertArgs::default();
    let mut i = 0;

    while i < rest.len() {
        match rest[i].as_str() {
            "--content-type" => {
                i += 1;
                convert.content_type = Some(validated_content_type(rest.get(i))?);
            }
            #[cfg(feature = "translate")]
            "--verify" => {
                convert.verify = true;
            }
            #[cfg(not(feature = "translate"))]
            "--verify" => anyhow::bail!("{VERIFY_NEEDS_TRANSLATE}"),
            "--" => {
                convert.files.push("--".into());
            }
            arg if arg.starts_with('-') => {
                anyhow::bail!("unknown convert flag: {arg}");
            }
            _ => {
                convert.files.push(rest[i].clone());
            }
        }
        i += 1;
    }
    if convert.files.is_empty() {
        convert.files.push("--".into()); // default: stdin
    }
    Ok((convert, i))
}

/// Parse the arguments after `tm`.  Only `export`, `import`, and `record`
/// consume anything beyond the subcommand name; an unknown subcommand is passed
/// through so `run_tm_cmd` reports it.
fn parse_tm(rest: &[String]) -> Result<(TmArgs, usize)> {
    let mut tm = TmArgs {
        cmd: rest
            .first()
            .context("tm requires a subcommand (list|export|import|clear|record)")?
            .clone(),
        ..TmArgs::default()
    };
    let mut i = 1;

    match tm.cmd.as_str() {
        "export" | "import" => {
            tm.arg = Some(
                rest.get(i)
                    .with_context(|| format!("tm {} requires a file path", tm.cmd))?
                    .clone(),
            );
            i += 1;
        }
        "record" => {
            while i < rest.len() && rest[i].starts_with("--") {
                let flag = rest[i].as_str();
                let slot = match flag {
                    "--found" => &mut tm.found,
                    "--suggested" => &mut tm.suggested,
                    "--chose" => &mut tm.chose,
                    "--context" => &mut tm.context,
                    other => anyhow::bail!("unknown tm record flag: {other}"),
                };
                *slot = Some(
                    rest.get(i + 1)
                        .with_context(|| format!("{flag} requires a value"))?
                        .clone(),
                );
                i += 2;
            }
        }
        _ => {} // list, clear, and anything run_tm_cmd should reject
    }
    Ok((tm, i))
}

/// Parse the arguments after `pack`.  Only `import`, `export`, and `validate`
/// take an argument; `list` does not, and an unknown subcommand is passed
/// through so `run_pack_cmd` reports it.
fn parse_pack(rest: &[String]) -> Result<(String, Option<String>, usize)> {
    let cmd = rest
        .first()
        .context("pack requires a subcommand (import|export|validate|list)")?
        .clone();
    match cmd.as_str() {
        "import" | "export" | "validate" => {
            let arg = rest
                .get(1)
                .with_context(|| format!("pack {cmd} requires an argument"))?
                .clone();
            Ok((cmd, Some(arg), 2))
        }
        _ => Ok((cmd, None, 1)),
    }
}

/// Parse the arguments after `cache`.  `clear` is the only subcommand and it
/// takes nothing, so trailing arguments are a typo worth reporting.
fn parse_cache(rest: &[String]) -> Result<usize> {
    match rest.first().map(String::as_str) {
        Some("clear") => match rest.get(1) {
            Some(extra) => {
                anyhow::bail!("cache clear does not accept additional arguments: {extra}")
            }
            None => Ok(1),
        },
        Some(other) => anyhow::bail!("unknown cache subcommand: {other} (expected 'clear')"),
        None => anyhow::bail!("cache requires a subcommand (clear)"),
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::lint::content_type_for;

    #[test]
    fn content_type_comes_from_the_flag_then_the_file_name() {
        use zhtw_mcp::engine::scan::ContentType;

        // The flag wins, whatever the name says.
        assert_eq!(
            content_type_for(Some("yaml"), "notes.md"),
            ContentType::Yaml
        );
        assert_eq!(
            content_type_for(Some("markdown-scan-code"), "notes.txt"),
            ContentType::MarkdownScanCode
        );

        // Without one, the name decides, case-insensitively.
        assert_eq!(content_type_for(None, "README.MD"), ContentType::Markdown);
        assert_eq!(content_type_for(None, "a.markdown"), ContentType::Markdown);
        assert_eq!(content_type_for(None, "conf.yml"), ContentType::Yaml);
        assert_eq!(content_type_for(None, "conf.YAML"), ContentType::Yaml);
        assert_eq!(content_type_for(None, "plain.txt"), ContentType::Plain);
        assert_eq!(content_type_for(None, "--"), ContentType::Plain);

        // An unrecognized flag value falls back to the name rather than
        // failing, which is what the flag has always done.
        assert_eq!(
            content_type_for(Some("nonsense"), "a.md"),
            ContentType::Markdown
        );
    }
    use super::*;

    /// Parse a command line written without the program name.
    fn parse(argv: &[&str]) -> Result<Cli> {
        let mut args = vec!["zhtw-mcp".to_string()];
        args.extend(argv.iter().map(|s| s.to_string()));
        parse_args(&args)
    }

    fn lint_of(argv: &[&str]) -> LintArgs {
        match parse(argv).expect("parse should succeed").command {
            Command::Lint(l) => *l,
            _ => panic!("expected a lint command from {argv:?}"),
        }
    }

    fn convert_of(argv: &[&str]) -> ConvertArgs {
        match parse(argv).expect("parse should succeed").command {
            Command::Convert(c) => c,
            _ => panic!("expected a convert command from {argv:?}"),
        }
    }

    fn tm_of(argv: &[&str]) -> TmArgs {
        match parse(argv).expect("parse should succeed").command {
            Command::Tm(t) => t,
            _ => panic!("expected a tm command from {argv:?}"),
        }
    }

    fn err_of(argv: &[&str]) -> String {
        parse(argv)
            .err()
            .unwrap_or_else(|| panic!("expected {argv:?} to fail"))
            .to_string()
    }

    #[test]
    fn no_args_runs_the_server() {
        let cli = parse(&[]).unwrap();
        assert!(matches!(cli.command, Command::Server));
        assert!(cli.overrides_path.is_none());
        assert!(cli.active_packs.is_empty());
    }

    #[test]
    fn global_flags_precede_the_subcommand() {
        let cli = parse(&[
            "--pack",
            "medical",
            "--pack",
            "legal",
            "--overrides",
            "/tmp/o.json",
            "--suppressions",
            "/tmp/s.json",
            "--packs-dir",
            "/tmp/packs",
            "--config",
            "/tmp/c.toml",
            "lint",
            "a.md",
        ])
        .unwrap();
        assert_eq!(cli.active_packs, ["medical", "legal"]);
        assert_eq!(cli.overrides_path.unwrap().to_str(), Some("/tmp/o.json"));
        assert_eq!(cli.suppressions_path.unwrap().to_str(), Some("/tmp/s.json"));
        assert_eq!(cli.packs_dir.unwrap().to_str(), Some("/tmp/packs"));
        assert_eq!(cli.config_path.unwrap().to_str(), Some("/tmp/c.toml"));
    }

    #[test]
    fn global_flags_require_their_value() {
        for flag in [
            "--overrides",
            "--pack",
            "--packs-dir",
            "--suppressions",
            "--config",
        ] {
            assert!(parse(&[flag]).is_err(), "{flag} without a value must fail");
        }
    }

    #[test]
    fn lint_collects_files_and_defaults() {
        let lint = lint_of(&["lint", "a.md", "b.md"]);
        assert_eq!(lint.files, ["a.md", "b.md"]);
        assert!(matches!(lint.format, LintFormat::Human));
        assert_eq!(lint.max_errors, None);
        assert!(lint.fix_mode.is_none());
        assert!(!lint.detect_ai);
        assert!(!lint.rhythm);
    }

    #[test]
    fn rhythm_is_a_bare_flag_that_composes() {
        let lint = lint_of(&["lint", "--rhythm", "a.md"]);
        assert!(lint.rhythm);
        assert_eq!(lint.files, ["a.md"], "--rhythm ate the file");

        // It is a capability, not a profile: it must survive beside one.
        let lint = lint_of(&["lint", "--profile", "strict", "--rhythm", "a.md"]);
        assert!(lint.rhythm);
        assert_eq!(lint.profile.as_deref(), Some("strict"));
    }

    #[test]
    fn lint_without_files_fails() {
        assert!(err_of(&["lint"]).contains("at least one file"));
        assert!(err_of(&["lint", "--relaxed"]).contains("at least one file"));
    }

    #[test]
    fn lint_formats() {
        for (name, want) in [
            ("json", LintFormat::Json),
            ("human", LintFormat::Human),
            ("sarif", LintFormat::Sarif),
            ("compact", LintFormat::Compact),
            ("tabular", LintFormat::Tabular),
        ] {
            let lint = lint_of(&["lint", "a.md", "--format", name]);
            assert_eq!(
                std::mem::discriminant(&lint.format),
                std::mem::discriminant(&want),
                "--format {name}"
            );
        }
        assert!(err_of(&["lint", "a.md", "--format", "xml"]).contains("unknown format"));
        assert!(err_of(&["lint", "a.md", "--format"]).contains("requires a value"));
    }

    #[test]
    fn lint_numeric_flags() {
        let lint = lint_of(&["lint", "a.md", "--max-errors", "3", "--max-warnings", "7"]);
        assert_eq!(lint.max_errors, Some(3));
        assert_eq!(lint.max_warnings, Some(7));
        assert!(err_of(&["lint", "a.md", "--max-errors", "x"]).contains("--max-errors"));
        assert!(err_of(&["lint", "a.md", "--max-warnings", "-1"]).contains("--max-warnings"));
    }

    #[test]
    fn lint_fix_modes() {
        use zhtw_mcp::fixer::FixMode;
        for (arg, want) in [
            ("--fix", FixMode::LexicalSafe),
            ("--fix=lexical_safe", FixMode::LexicalSafe),
            ("--fix=orthographic", FixMode::Orthographic),
            ("--fix=lexical_contextual", FixMode::LexicalContextual),
        ] {
            let lint = lint_of(&["lint", "a.md", arg]);
            assert_eq!(lint.fix_mode, Some(want), "{arg}");
        }
        assert!(err_of(&["lint", "a.md", "--fix=wild"]).contains("unknown fix mode"));
    }

    #[test]
    fn lint_content_type_is_validated() {
        for ct in ["plain", "markdown", "markdown-scan-code", "yaml"] {
            let lint = lint_of(&["lint", "a.md", "--content-type", ct]);
            assert_eq!(lint.content_type.as_deref(), Some(ct));
        }
        assert!(err_of(&["lint", "a.md", "--content-type", "rst"]).contains("unknown content-type"));
    }

    #[test]
    fn detect_ai_level_is_optional() {
        // No level: the next token stays a file path.
        let lint = lint_of(&["lint", "--detect-ai", "a.md"]);
        assert!(lint.detect_ai);
        assert_eq!(lint.files, ["a.md"]);
        assert_eq!(lint.ai_threshold_multiplier, 1.0);

        for (level, mult) in [("low", 0.5), ("medium", 1.0), ("high", 1.5)] {
            let lint = lint_of(&["lint", "--detect-ai", level, "a.md"]);
            assert_eq!(lint.ai_threshold_multiplier, mult, "--detect-ai {level}");
            assert_eq!(lint.files, ["a.md"], "--detect-ai {level} ate the file");
        }
    }

    #[test]
    fn a_later_flag_without_a_level_keeps_the_earlier_one() {
        // --detect-style carries no level here, so it must not reset the low
        // threshold --detect-ai already requested. Order must not matter.
        let a = lint_of(&[
            "lint",
            "a.md",
            "--detect-ai",
            "low",
            "--detect-style",
            "--format",
            "json",
        ]);
        let b = lint_of(&[
            "lint",
            "a.md",
            "--detect-style",
            "--detect-ai",
            "low",
            "--format",
            "json",
        ]);
        assert_eq!(a.ai_threshold_multiplier, 0.5);
        assert_eq!(b.ai_threshold_multiplier, 0.5);

        // An explicit level still wins, whichever flag carries it.
        let c = lint_of(&[
            "lint",
            "a.md",
            "--detect-ai",
            "low",
            "--detect-style",
            "high",
            "--format",
            "json",
        ]);
        assert_eq!(c.ai_threshold_multiplier, 1.5);
    }

    #[test]
    fn detect_style_implies_both_axes_and_needs_json() {
        let lint = lint_of(&["lint", "a.md", "--detect-style", "--format", "json"]);
        assert!(lint.detect_style && lint.detect_ai && lint.detect_translationese);
        assert!(err_of(&["lint", "a.md", "--detect-style"]).contains("--format json"));
    }

    #[test]
    fn translationese_domain_is_validated() {
        let lint = lint_of(&["lint", "a.md", "--translationese-domain", "technical"]);
        assert!(matches!(
            lint.translationese_domain,
            zhtw_mcp::engine::translationese_score::TranslationeseDomain::Technical
        ));
        assert!(
            err_of(&["lint", "a.md", "--translationese-domain", "poetic"])
                .contains("unknown --translationese-domain")
        );
        assert!(err_of(&["lint", "--translationese-domain"]).contains("requires a value"));
    }

    #[test]
    fn document_genre_is_validated() {
        let lint = lint_of(&["lint", "a.md", "--document-genre", "financial"]);
        assert!(matches!(
            lint.document_genre,
            zhtw_mcp::rules::ruleset::DocumentGenre::Financial
        ));
        assert!(
            err_of(&["lint", "--document-genre", "poetic"]).contains("unknown --document-genre")
        );
    }

    #[test]
    fn lint_boolean_flags() {
        let lint = lint_of(&[
            "lint",
            "a.md",
            "--relaxed",
            "--exempt-blockquotes",
            "--consistency",
            "--dry-run",
            "--explain",
            "--update-baseline",
            "--telemetry",
            "--detect-translationese",
        ]);
        assert!(lint.relaxed);
        assert!(lint.exempt_blockquotes);
        assert!(lint.consistency);
        assert!(lint.dry_run);
        assert!(lint.explain);
        assert!(lint.update_baseline);
        assert!(lint.telemetry);
        assert!(lint.detect_translationese);
    }

    #[test]
    fn lint_path_flags() {
        let lint = lint_of(&[
            "lint",
            "a.md",
            "--baseline",
            "base.json",
            "--diff-from",
            "origin/main",
            "--exclude",
            "vendor/**",
            "--exclude",
            "*.tmp",
            "--profile",
            "strict",
        ]);
        assert_eq!(lint.baseline_path.unwrap().to_str(), Some("base.json"));
        assert_eq!(lint.diff_from.as_deref(), Some("origin/main"));
        assert_eq!(lint.exclude_patterns, ["vendor/**", "*.tmp"]);
        assert_eq!(lint.profile.as_deref(), Some("strict"));
    }

    #[test]
    fn lint_treats_unknown_flags_as_file_paths() {
        // Documented behavior: global flags belong before the subcommand, so
        // anything unrecognized after lint is a path, not an error.
        let lint = lint_of(&["lint", "--pack", "medical"]);
        assert_eq!(lint.files, ["--pack", "medical"]);
    }

    #[test]
    fn lint_accepts_stdin_and_log_flags() {
        let lint = lint_of(&["lint", "--", "--verbose", "--debug"]);
        assert_eq!(lint.files, ["--"]);
    }

    #[cfg(feature = "translate")]
    #[test]
    fn verify_flag_is_recognized_when_the_feature_is_on() {
        assert!(lint_of(&["lint", "a.md", "--verify"]).verify);
        assert!(convert_of(&["convert", "--verify"]).verify);
    }

    #[cfg(not(feature = "translate"))]
    #[test]
    fn verify_flag_explains_the_missing_feature() {
        assert!(err_of(&["lint", "a.md", "--verify"]).contains("translate"));
        assert!(err_of(&["convert", "--verify"]).contains("translate"));
    }

    #[test]
    fn convert_defaults_to_stdin() {
        assert_eq!(convert_of(&["convert"]).files, ["--"]);
    }

    #[test]
    fn convert_validates_content_type_like_lint() {
        assert_eq!(
            convert_of(&["convert", "a.md", "--content-type", "yaml"])
                .content_type
                .as_deref(),
            Some("yaml")
        );

        // The two abbreviations convert accepted before the validator was
        // shared still work, normalized to the long form.
        for (given, want) in [("md", "markdown"), ("yml", "yaml")] {
            assert_eq!(
                convert_of(&["convert", "a.md", "--content-type", given])
                    .content_type
                    .as_deref(),
                Some(want)
            );
            assert_eq!(
                lint_of(&["lint", "a.md", "--content-type", given])
                    .content_type
                    .as_deref(),
                Some(want)
            );
        }
        // Used to be accepted and silently fall through to auto-detection.
        assert!(err_of(&["convert", "a.md", "--content-type", "markdwon"])
            .contains("unknown content-type"));
        assert!(err_of(&["convert", "a.md", "--content-type"]).contains("requires a value"));
    }

    #[test]
    fn convert_collects_files_and_rejects_unknown_flags() {
        let convert = convert_of(&["convert", "a.md", "--content-type", "markdown"]);
        assert_eq!(convert.files, ["a.md"]);
        assert_eq!(convert.content_type.as_deref(), Some("markdown"));
        assert!(err_of(&["convert", "--nope"]).contains("unknown convert flag"));
    }

    #[test]
    fn tm_record_collects_key_values() {
        let tm = tm_of(&[
            "tm",
            "record",
            "--found",
            "軟件",
            "--suggested",
            "軟體",
            "--chose",
            "軟體",
            "--context",
            "句子",
        ]);
        assert_eq!(tm.cmd, "record");
        assert_eq!(tm.found.as_deref(), Some("軟件"));
        assert_eq!(tm.suggested.as_deref(), Some("軟體"));
        assert_eq!(tm.chose.as_deref(), Some("軟體"));
        assert_eq!(tm.context.as_deref(), Some("句子"));
        assert!(err_of(&["tm", "record", "--bogus", "x"]).contains("unknown tm record flag"));
        assert!(err_of(&["tm", "record", "--found"]).contains("--found requires a value"));
    }

    #[test]
    fn tm_argument_consumption_is_per_subcommand() {
        for sub in ["export", "import"] {
            assert_eq!(tm_of(&["tm", sub, "f.json"]).arg.as_deref(), Some("f.json"));
            assert!(err_of(&["tm", sub]).contains("requires a file path"));
        }
        for sub in ["list", "clear"] {
            assert!(tm_of(&["tm", sub]).arg.is_none());
        }
        assert!(err_of(&["tm"]).contains("tm requires a subcommand"));
    }

    #[test]
    fn pack_argument_consumption_is_per_subcommand() {
        for sub in ["import", "export", "validate"] {
            match parse(&["pack", sub, "x"]).unwrap().command {
                Command::Pack { cmd, arg } => {
                    assert_eq!(cmd, sub);
                    assert_eq!(arg.as_deref(), Some("x"));
                }
                _ => panic!("expected pack"),
            }
            assert!(err_of(&["pack", sub]).contains("requires an argument"));
        }
        match parse(&["pack", "list"]).unwrap().command {
            Command::Pack { cmd, arg } => {
                assert_eq!(cmd, "list");
                assert!(arg.is_none());
            }
            _ => panic!("expected pack"),
        }
        assert!(err_of(&["pack"]).contains("pack requires a subcommand"));
    }

    #[test]
    fn cache_clear_takes_no_extra_arguments() {
        assert!(matches!(
            parse(&["cache", "clear"]).unwrap().command,
            Command::CacheClear
        ));
        assert!(err_of(&["cache", "clear", "all"]).contains("does not accept additional"));
        assert!(err_of(&["cache", "purge"]).contains("unknown cache subcommand"));
        assert!(err_of(&["cache"]).contains("cache requires a subcommand"));
    }

    #[test]
    fn setup_requires_a_host() {
        match parse(&["setup", "claude"]).unwrap().command {
            Command::Setup(h) => assert_eq!(h, "claude"),
            _ => panic!("expected setup"),
        }
        assert!(err_of(&["setup"]).contains("requires a host name"));
    }

    #[test]
    fn a_second_subcommand_is_rejected() {
        // The flat-variable version silently resolved this by dispatch order.
        assert!(err_of(&["setup", "claude", "pack", "list"]).contains("only one subcommand"));
        assert!(err_of(&["pack", "list", "cache", "clear"]).contains("only one subcommand"));
    }

    #[test]
    fn unknown_top_level_argument_is_rejected() {
        assert!(err_of(&["--nope"]).contains("unknown argument"));
        assert!(err_of(&["frobnicate"]).contains("unknown argument"));
        // --content-type is lint-only, so it is unknown at the top level.
        assert!(err_of(&["--content-type", "markdown"]).contains("unknown argument"));
    }

    #[test]
    fn log_level_flags_are_accepted_anywhere() {
        assert!(matches!(
            parse(&["--verbose", "--debug"]).unwrap().command,
            Command::Server
        ));
    }

    /// The topic `argv` asks for, or a panic if it does not ask for help.
    fn help_of(argv: &[&str]) -> HelpTopic {
        match parse(argv).expect("parse should succeed").command {
            Command::Help(topic) => topic,
            _ => panic!("expected a help command from {argv:?}"),
        }
    }

    fn asks_for_help(argv: &[&str]) -> bool {
        matches!(
            parse(argv).expect("parse should succeed").command,
            Command::Help(_)
        )
    }

    #[test]
    fn a_line_without_a_help_flag_never_prints_help() {
        assert!(!asks_for_help(&[]));
        assert!(!asks_for_help(&["lint", "a.md"]));
        assert!(!asks_for_help(&["--pack", "medical", "lint", "a.md"]));
        assert!(!asks_for_help(&["tm", "list"]));
        // "help" is a word, not a flag, and no subcommand is spelled that way.
        assert!(err_of(&["help"]).contains("unknown argument"));
    }

    #[test]
    fn a_help_flag_with_no_subcommand_selects_the_global_topic() {
        assert_eq!(help_of(&["--help"]), HelpTopic::Global);
        assert_eq!(help_of(&["-h"]), HelpTopic::Global);
        assert_eq!(help_of(&["--pack", "medical", "--help"]), HelpTopic::Global);
        // A help flag outranks an argument that would otherwise be rejected.
        assert_eq!(help_of(&["--nope", "--help"]), HelpTopic::Global);
    }

    #[test]
    fn every_subcommand_row_reaches_the_command_it_names() {
        // The match below is exhaustive on purpose. A new Command variant stops
        // this test compiling until somebody says which help topic it answers
        // to, and a topic needs a help text, which build.rs will not accept
        // without a docs block, which each_subcommand_prints_its_own_ message
        // then runs through the binary. That chain is what makes a missing
        // SUBCOMMAND_TOPICS row a failure rather than silent global help.
        for (name, topic) in SUBCOMMAND_TOPICS {
            let argv = match name {
                "lint" => vec![name, "a.md"],
                "convert" => vec![name],
                "setup" => vec![name, "cursor"],
                "pack" => vec![name, "list"],
                "tm" => vec![name, "list"],
                "cache" => vec![name, "clear"],
                _ => panic!("give the new subcommand {name} an argv here"),
            };
            let reached = match parse(&argv).expect("should parse").command {
                Command::Lint(_) => HelpTopic::Lint,
                Command::Convert(_) => HelpTopic::Convert,
                Command::Setup(_) => HelpTopic::Setup,
                Command::Pack { .. } => HelpTopic::Pack,
                Command::Tm(_) => HelpTopic::Tm,
                Command::CacheClear => HelpTopic::Cache,
                Command::Server | Command::Help(_) => {
                    panic!("{name} should parse as a subcommand")
                }
            };
            assert_eq!(reached, topic, "{name}");
        }
    }

    #[test]
    fn an_empty_argv_parses_as_the_default_command() {
        // The parse helper always supplies argv[0], so this one calls through
        // directly. A process exec'd with no argv at all arrives this way.
        let cli = parse_args(&[]).expect("an empty argv should parse");
        assert!(matches!(cli.command, Command::Server));
    }

    #[test]
    fn a_help_flag_after_a_subcommand_selects_that_subcommand() {
        for (name, topic) in SUBCOMMAND_TOPICS {
            assert_eq!(help_of(&[name, "--help"]), topic, "{name}");
            assert_eq!(help_of(&[name, "-h"]), topic, "{name}");
        }
        assert_eq!(help_of(&["lint", "a.md", "--help"]), HelpTopic::Lint);
        assert_eq!(help_of(&["setup", "vscode", "--help"]), HelpTopic::Setup);

        // A help flag in a value slot is still a request for help: it outranks
        // the rest of the line rather than being recorded as the value.
        assert_eq!(help_of(&["lint", "--format", "--help"]), HelpTopic::Lint);
        assert_eq!(
            help_of(&["tm", "record", "--found", "a", "--context", "-h"]),
            HelpTopic::Tm
        );
    }

    #[test]
    fn a_subcommand_topic_outranks_the_global_one() {
        assert_eq!(help_of(&["--help", "lint", "--help"]), HelpTopic::Lint);
        assert_eq!(help_of(&["-h", "convert", "-h"]), HelpTopic::Convert);
        assert_eq!(
            help_of(&["--pack", "medical", "--help", "tm", "--help"]),
            HelpTopic::Tm
        );
        // The subcommand need not carry a help flag of its own.
        assert_eq!(help_of(&["--help", "pack"]), HelpTopic::Pack);
    }

    #[test]
    fn a_subcommand_name_in_a_global_value_slot_still_picks_the_topic() {
        // The scan for a topic does not know which arguments are values, so a
        // directory or pack literally named after a subcommand selects that
        // subcommand's help. Only a global value slot can do this: it is the
        // only slot ahead of the subcommand, so once the real one appears it is
        // the first match. The cost is the wrong help page on a line that asked
        // for help either way, which is not worth a second table of
        // value-taking flags to keep in sync with the match arms above.
        assert_eq!(help_of(&["--packs-dir", "lint", "--help"]), HelpTopic::Lint);
        assert_eq!(help_of(&["--config", "tm", "--help"]), HelpTopic::Tm);
    }
}
