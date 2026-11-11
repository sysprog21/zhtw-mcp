use super::*;

#[test]
fn claude_code_section_contains_tools() {
    let section = claude_code_section();
    assert!(section.contains("zhtw"));
    assert!(!section.contains("zh_lint"));
    assert!(!section.contains("zh_finalize"));
    assert!(!section.contains("zh_apply_fixes"));
}

#[test]
fn claude_code_section_contains_conventions() {
    let section = claude_code_section();
    assert!(section.contains("軟體"));
    assert!(section.contains("資訊"));
    assert!(section.contains("full-width"));
}

#[test]
fn codex_instructions_use_short_server_name() {
    let instructions = codex_instructions();
    assert!(instructions.contains("codex mcp add zhtw"));
    assert!(instructions.contains("mcp__zhtw.zhtw"));
    assert!(instructions.contains("AGENTS.md"));
}

#[test]
fn opencode_skill_contains_expected_fields() {
    let skill = opencode_skill();
    assert!(skill.contains("name: zhtw-lint"));
    assert!(skill.contains("zhtw"));
    assert!(!skill.contains("zh_lint"));
    assert!(!skill.contains("zh_finalize"));
    assert!(skill.contains("normalize_tone"));
}

#[test]
fn copilot_config_has_instructions_and_settings() {
    let (instructions, settings) = copilot_config();
    assert!(instructions.contains("軟體"));
    assert!(instructions.contains("full-width"));
    assert!(settings.contains("zhtw-mcp"));
    assert!(settings.contains("mcp"));
}

#[test]
fn host_from_str_parses_all_variants() {
    assert_eq!(Host::from_name("claude_code"), Some(Host::ClaudeCode));
    assert_eq!(Host::from_name("claude-code"), Some(Host::ClaudeCode));
    assert_eq!(Host::from_name("codex"), Some(Host::Codex));
    assert_eq!(Host::from_name("codex-cli"), Some(Host::Codex));
    assert_eq!(Host::from_name("opencode"), Some(Host::OpenCode));
    assert_eq!(Host::from_name("copilot"), Some(Host::Copilot));
    assert_eq!(Host::from_name("github-copilot"), Some(Host::Copilot));
    assert_eq!(Host::from_name("cursor"), Some(Host::Cursor));
    assert_eq!(Host::from_name("windsurf"), Some(Host::Windsurf));
    assert_eq!(Host::from_name("cline"), Some(Host::Cline));
    assert_eq!(Host::from_name("continue"), Some(Host::ContinueDev));
    assert_eq!(Host::from_name("continue-dev"), Some(Host::ContinueDev));
    assert_eq!(Host::from_name("continue.dev"), Some(Host::ContinueDev));
    assert_eq!(Host::from_name("generic"), Some(Host::Generic));
    assert!(Host::from_name("unknown").is_none());
}

#[test]
fn cursor_rules_contains_tool_and_conventions() {
    let rules = cursor_rules();
    assert!(rules.contains("zhtw"));
    assert!(rules.contains("軟體"));
    assert!(rules.contains("full-width"));
}

#[test]
fn windsurf_rules_contains_tool_and_terms() {
    let rules = windsurf_rules();
    assert!(rules.contains("zhtw"));
    assert!(rules.contains("軟體"));
}

#[test]
fn cline_rules_contains_tool() {
    let rules = cline_rules();
    assert!(rules.contains("zhtw"));
}

#[test]
fn continuedev_config_has_mcp_server() {
    let config = continuedev_config();
    assert!(config.contains("zhtw-mcp"));
    assert!(config.contains("mcpServers"));
}

#[test]
fn generic_instructions_comprehensive() {
    let instructions = generic_instructions();
    assert!(instructions.contains("zhtw"));
    assert!(instructions.contains("fix_mode"));
    assert!(instructions.contains("max_errors"));
    assert!(instructions.contains("軟體"));
    assert!(instructions.contains("full-width"));
}

#[test]
fn translation_guide_json_contract() {
    let guide = generate_translation_guide();
    assert!(guide.is_object());
    assert_eq!(guide["host"], "translation-guide");
    assert_eq!(guide["file"], "(system prompt injection)");
    let content = guide["content"].as_str().unwrap();
    assert!(content.contains("繁體中文"));
    assert!(content.contains("值得注意的是"));
    assert!(content.len() > 500);
}

#[test]
fn generate_for_all_hosts_succeeds() {
    for host in ALL_HOSTS {
        let output = generate_for_host(*host);
        assert!(output.is_object());
    }
}

/// Every text the setup module hands an agent must be free of rewrite goals.
#[test]
fn setup_texts_state_no_rewrite_goal() {
    let mut texts = vec![
        ("translation_guide", translation_guide_text()),
        ("claude_code_section", claude_code_section()),
        ("opencode_skill", opencode_skill()),
        ("codex_instructions", codex_instructions()),
        ("cursor_rules", cursor_rules()),
        ("windsurf_rules", windsurf_rules()),
        ("cline_rules", cline_rules()),
        ("continuedev_config", continuedev_config()),
        ("generic_instructions", generic_instructions()),
        ("copilot_instructions", copilot_config().0),
        ("copilot_settings", copilot_config().1),
    ];
    for host in ALL_HOSTS {
        let v = generate_for_host(*host);
        // Copilot returns a files array; every other host one content string.
        let contents: Vec<&str> = match v["files"].as_array() {
            Some(files) => files.iter().filter_map(|f| f["content"].as_str()).collect(),
            None => vec![v["content"].as_str().expect("host content is a string")],
        };
        for content in contents {
            texts.push(("host content", content.to_string()));
        }
    }
    for (name, text) in texts {
        let hits = crate::mcp::rewrite_goal::rewrite_goal_hits(&text);
        assert!(hits.is_empty(), "{name} states a rewrite goal: {hits:?}");
    }
}
