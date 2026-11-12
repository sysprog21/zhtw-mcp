/// The text of a prompt's single message.
fn message_text(result: &GetPromptResult) -> &str {
    result.messages[0]
        .content
        .as_text()
        .expect("prompts return text content")
        .text
        .as_str()
}

use super::*;

fn empty_args() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

#[test]
fn list_returns_three_prompts() {
    let prompts = list_prompts();
    assert_eq!(prompts.len(), 3);
    assert_eq!(prompts[0].name, NORMALIZE_TONE);
    assert_eq!(prompts[1].name, LINT_NATURAL);
    assert_eq!(prompts[2].name, EDITORIAL_REVIEW);
}

#[test]
fn get_normalize_tone_returns_content() {
    let result = get_prompt(NORMALIZE_TONE, &empty_args()).unwrap();
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role, Role::User);
    assert!(message_text(&result).contains("Traditional Chinese"));
    assert!(message_text(&result).contains("軟體"));
}

#[test]
fn get_lint_natural_includes_instruction_and_text() {
    let mut args = std::collections::HashMap::new();
    args.insert("instruction".into(), "check for mainland terms".into());
    args.insert("text".into(), "這個軟件很好用".into());
    let result = get_prompt(LINT_NATURAL, &args).unwrap();
    assert_eq!(result.messages.len(), 1);
    assert!(message_text(&result).contains("check for mainland terms"));
    assert!(message_text(&result).contains("這個軟件很好用"));
    assert!(message_text(&result).contains("zhtw"));
}

#[test]
fn get_editorial_review_includes_text_and_max_iterations() {
    let mut args = std::collections::HashMap::new();
    args.insert("text".into(), "使用默認設置".into());
    args.insert("max_iterations".into(), "5".into());
    let result = get_prompt(EDITORIAL_REVIEW, &args).unwrap();
    assert_eq!(result.messages.len(), 1);
    assert!(message_text(&result).contains("使用默認設置"));
    assert!(message_text(&result).contains("up to 5 total"));
}

#[test]
fn get_editorial_review_default_iterations() {
    let mut args = std::collections::HashMap::new();
    args.insert("text".into(), "測試文字".into());
    let result = get_prompt(EDITORIAL_REVIEW, &args).unwrap();
    assert!(message_text(&result).contains("up to 3 total"));
}

#[test]
fn get_unknown_returns_none() {
    assert!(get_prompt("unknown", &empty_args()).is_none());
}

#[test]
fn prompts_state_no_rewrite_goal() {
    let mut args = empty_args();
    args.insert("instruction".into(), "fix it".into());
    args.insert("text".into(), "文字".into());
    for prompt in list_prompts() {
        let result = get_prompt(&prompt.name, &args).unwrap();
        let hits = crate::mcp::rewrite_goal::rewrite_goal_hits(message_text(&result));
        assert!(
            hits.is_empty(),
            "{} states a rewrite goal: {hits:?}",
            prompt.name
        );
    }
}
