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

    // An unrecognized flag value falls back to the name rather than failing,
    // which is what the flag has always done.
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
fn off_is_repeatable_and_rejects_unknown_families() {
    let lint = lint_of(&["lint", "--off", "punctuation", "--off", "variant", "a.md"]);
    assert_eq!(
        lint.off,
        vec![
            zhtw_mcp::rules::ruleset::RuleFamily::Punctuation,
            zhtw_mcp::rules::ruleset::RuleFamily::Variant,
        ]
    );
    assert!(err_of(&["lint", "--off", "unknown", "a.md"]).contains("punctuation"));
}

#[test]
fn spacing_policy_accepts_its_two_values() {
    use zhtw_mcp::rules::ruleset::SpacingPolicy;

    assert_eq!(
        lint_of(&["lint", "--spacing", "strip", "a.md"]).spacing,
        Some(SpacingPolicy::Strip)
    );
    assert!(err_of(&["lint", "--spacing", "off", "a.md"]).contains("unknown --spacing value"));
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
    for ct in ["plain", "markdown", "markdown-scan-code", "yaml", "json"] {
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
        zhtw_mcp::rules::ruleset::AttributionGenre::Financial
    ));
    assert!(err_of(&["lint", "--document-genre", "poetic"]).contains("unknown --document-genre"));
}

#[test]
fn register_defaults_to_auto() {
    let lint = lint_of(&["lint", "a.md"]);
    assert!(matches!(
        lint.register,
        zhtw_mcp::rules::ruleset::RegisterMode::Auto
    ));
}

#[test]
fn register_is_validated() {
    let lint = lint_of(&["lint", "a.md", "--register", "formal"]);
    assert!(matches!(
        lint.register,
        zhtw_mcp::rules::ruleset::RegisterMode::Formal
    ));
    let lint = lint_of(&["lint", "a.md", "--register", "casual"]);
    assert!(matches!(
        lint.register,
        zhtw_mcp::rules::ruleset::RegisterMode::Casual
    ));
    assert!(err_of(&["lint", "--register", "poetic"]).contains("unknown --register"));
    assert!(err_of(&["lint", "--register"]).contains("--register requires a value"));
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

    // The two abbreviations convert accepted before the validator was shared
    // still work, normalized to the long form.
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
    assert!(
        err_of(&["convert", "a.md", "--content-type", "markdwon"]).contains("unknown content-type")
    );
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
fn hook_install_takes_no_extra_arguments() {
    assert!(matches!(
        parse(&["hook", "install"]).unwrap().command,
        Command::HookInstall
    ));
    assert!(err_of(&["hook", "install", "now"]).contains("does not accept additional"));
    assert!(err_of(&["hook", "uninstall"]).contains("unknown hook subcommand"));
    assert!(err_of(&["hook"]).contains("hook requires a subcommand"));
}

#[test]
fn internal_hook_callback_takes_no_extra_arguments() {
    assert!(matches!(
        parse(&["internal", "hook-callback"]).unwrap().command,
        Command::HookCallback
    ));
    assert!(
        err_of(&["internal", "hook-callback", "file.md"]).contains("does not accept additional")
    );
    assert!(err_of(&["internal", "frobnicate"]).contains("unknown internal subcommand"));
    assert!(err_of(&["internal"]).contains("internal requires a subcommand"));
}

#[test]
fn hook_subcommands_participate_in_the_claim() {
    assert!(err_of(&["hook", "install", "cache", "clear"]).contains("does not accept"));
    assert!(err_of(&["setup", "claude", "hook", "install"]).contains("only one subcommand"));
    assert!(
        err_of(&["setup", "claude", "internal", "hook-callback"]).contains("only one subcommand")
    );
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
    // this test compiling until somebody says which help topic it answers to,
    // and a topic needs a help text, which build.rs will not accept without a
    // docs block, which each_subcommand_prints_its_own_ message then runs
    // through the binary. That chain is what makes a missing SUBCOMMAND_TOPICS
    // row a failure rather than silent global help.
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
            Command::Server | Command::Help(_) | Command::HookInstall | Command::HookCallback => {
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

    // A help flag in a value slot is still a request for help: it outranks the
    // rest of the line rather than being recorded as the value.
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
    // subcommand's help. Only a global value slot can do this: it is the only
    // slot ahead of the subcommand, so once the real one appears it is the
    // first match. The cost is the wrong help page on a line that asked for
    // help either way, which is not worth a second table of value-taking flags
    // to keep in sync with the match arms above.
    assert_eq!(help_of(&["--packs-dir", "lint", "--help"]), HelpTopic::Lint);
    assert_eq!(help_of(&["--config", "tm", "--help"]), HelpTopic::Tm);
}
