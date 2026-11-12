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

    // hook install: register the Claude Code PostToolUse hook in the user's
    // settings.json.
    HookInstall,

    // internal hook-callback: the machine-invoked entry point that hook
    // registration points at. Hidden: absent from the help text because nothing
    // under the internal namespace is for humans to type.
    HookCallback,
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
    pub(crate) spacing: Option<zhtw_mcp::rules::ruleset::SpacingPolicy>,
    pub(crate) off: Vec<zhtw_mcp::rules::ruleset::RuleFamily>,
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
    pub(crate) document_genre: zhtw_mcp::rules::ruleset::AttributionGenre,
    /// Register policy. `Auto` reads it off the text; the explicit values are
    /// the recourse when a 公文 opens on prose and the heuristic misses it.
    pub(crate) register: zhtw_mcp::rules::ruleset::RegisterMode,
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
            spacing: None,
            off: Vec::new(),
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
            document_genre: zhtw_mcp::rules::ruleset::AttributionGenre::Casual,
            register: zhtw_mcp::rules::ruleset::RegisterMode::Auto,
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
        "plain" | "markdown" | "markdown-scan-code" | "yaml" | "json" => Ok(ct.clone()),
        _ => anyhow::bail!(
            "unknown content-type: {ct} (expected 'plain', 'markdown', 'markdown-scan-code', 'yaml', or 'json')"
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
            "hook" => {
                claim(&cli.command, "hook")?;
                i += parse_hook(&args[i + 1..])?;
                cli.command = Command::HookInstall;
            }
            "internal" => {
                claim(&cli.command, "internal")?;
                i += parse_internal(&args[i + 1..])?;
                cli.command = Command::HookCallback;
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

            // Read from the environment by the tracing setup, not here;
            // accepted anywhere on the line so they can precede or follow a
            // subcommand.
            "--verbose" | "--debug" => {}
            _ => {
                anyhow::bail!("unknown argument: {}", args[i]);
            }
        }
        i += 1;
    }

    Ok(cli)
}

/// Flags that take no value. Returns true when `arg` was one of them.
///
/// Split out of `parse_lint` because a bare switch is a one-line fact that does
/// not need a match arm with a body. The parser was growing by three lines
/// every time the CLI grew by one switch, which is why it was the longest
/// function in the tree.
///
/// `--verify` is not here: it is cfg-gated and has to fail loudly rather than
/// silently when the `translate` feature is off.
fn set_bare_flag(arg: &str, lint: &mut LintArgs) -> bool {
    let field = match arg {
        "--relaxed" => &mut lint.relaxed,
        "--exempt-blockquotes" => &mut lint.exempt_blockquotes,
        "--consistency" => &mut lint.consistency,
        "--dry-run" => &mut lint.dry_run,
        "--explain" => &mut lint.explain,
        "--update-baseline" => &mut lint.update_baseline,
        "--detect-translationese" => &mut lint.detect_translationese,
        "--rhythm" => &mut lint.rhythm,
        "--telemetry" => &mut lint.telemetry,
        // Read by the driver before parse_lint runs; accepted and ignored here.
        "--verbose" | "--debug" => return true,
        _ => return false,
    };
    *field = true;
    true
}

/// Parse a `--flag value` whose value is one of a fixed set.
///
/// The enum-valued flags had the same eight lines each, differing only in the
/// type and in the list repeated twice in the messages. Both messages are
/// generated from one `expected` string so they cannot drift apart.
fn enum_value<T>(
    rest: &[String],
    i: usize,
    flag: &str,
    expected: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    let next = rest
        .get(i + 1)
        .with_context(|| format!("{flag} requires a value ({expected})"))?;
    parse(next).with_context(|| format!("unknown {flag} value '{next}' (expected: {expected})"))
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
        if set_bare_flag(&rest[i], &mut lint) {
            i += 1;
            continue;
        }
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
            "--spacing" => {
                lint.spacing = Some(enum_value(
                    rest,
                    i,
                    "--spacing",
                    "require|strip",
                    zhtw_mcp::rules::ruleset::SpacingPolicy::from_str_strict,
                )?);
                i += 1;
            }
            "--off" => {
                // Repeats are harmless: with_disabled only clears flags.
                lint.off.push(enum_value(
                    rest,
                    i,
                    "--off",
                    &zhtw_mcp::rules::ruleset::RuleFamily::names(),
                    zhtw_mcp::rules::ruleset::RuleFamily::from_str_strict,
                )?);
                i += 1;
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
            "--baseline" => {
                i += 1;
                lint.baseline_path =
                    Some(path_value(rest.get(i), "--baseline requires a file path")?);
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
            // Per-domain threshold calibration for the translationese score.
            "--translationese-domain" => {
                lint.translationese_domain = enum_value(
                    rest,
                    i,
                    "--translationese-domain",
                    "general|technical|literary|news",
                    zhtw_mcp::engine::translationese_score::TranslationeseDomain::from_str_strict,
                )?;
                i += 1;
            }
            "--document-genre" => {
                lint.document_genre = enum_value(
                    rest,
                    i,
                    "--document-genre",
                    "casual|technical|financial",
                    zhtw_mcp::rules::ruleset::AttributionGenre::from_str_strict,
                )?;
                i += 1;
            }
            "--register" => {
                lint.register = enum_value(
                    rest,
                    i,
                    "--register",
                    "auto|formal|casual",
                    zhtw_mcp::rules::ruleset::RegisterMode::from_str_strict,
                )?;
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

/// Parse the arguments after `hook`.  `install` is the only subcommand and it
/// takes nothing, so trailing arguments are a typo worth reporting.
fn parse_hook(rest: &[String]) -> Result<usize> {
    match rest.first().map(String::as_str) {
        Some("install") => match rest.get(1) {
            Some(extra) => {
                anyhow::bail!("hook install does not accept additional arguments: {extra}")
            }
            None => Ok(1),
        },
        Some(other) => anyhow::bail!("unknown hook subcommand: {other} (expected 'install')"),
        None => anyhow::bail!("hook requires a subcommand (install)"),
    }
}

/// Parse the arguments after `internal`, the namespace for machine-invoked
/// entry points.  `hook-callback` is the only one; it reads its input from
/// stdin, so trailing arguments are a mis-wired hook registration.
fn parse_internal(rest: &[String]) -> Result<usize> {
    match rest.first().map(String::as_str) {
        Some("hook-callback") => match rest.get(1) {
            Some(extra) => {
                anyhow::bail!("hook-callback does not accept additional arguments: {extra}")
            }
            None => Ok(1),
        },
        Some(other) => anyhow::bail!("unknown internal subcommand: {other}"),
        None => anyhow::bail!("internal requires a subcommand"),
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
#[path = "../../tests/unit/cli/args/tests.rs"]
mod tests;
