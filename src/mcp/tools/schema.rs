//! The tool, resource and prompt listings, and the JSON schema that
//! describes the `zhtw` tool's arguments to a client.
//!
//! The schema is the client-facing half of the parsing in
//! [`super::params`]: a field named here has to be accepted there, and a unit
//! test walks both to keep the pair honest.

use serde_json::json;

use rmcp::model::{
    GetPromptResult, JsonObject, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, Tool, ToolAnnotations,
};

use crate::mcp::prompts;
use crate::mcp::resources;

use super::*;

/// The properties of the `zhtw` tool's input schema.
///
/// Built once and shared, so what the tool advertises and what it accepts are
/// the same list rather than two lists that have to be kept in step. They were
/// two, and the accepted one was spelled out twice more, once per `translate`
/// build, so adding a parameter meant editing three places and being told it
/// was unknown if you missed one.
fn input_schema() -> &'static std::sync::Arc<JsonObject> {
    static SCHEMA: std::sync::OnceLock<std::sync::Arc<JsonObject>> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = JsonObject::new();
        schema.insert("type".into(), json!("object"));
        schema.insert(
            "properties".into(),
            Value::Object(input_schema_properties().clone()),
        );
        schema.insert("required".into(), json!(["text"]));
        std::sync::Arc::new(schema)
    })
}

/// The parameter names the schema declares, which is what the tool accepts.
pub(super) fn input_schema_properties() -> &'static JsonObject {
    static PROPS: std::sync::OnceLock<JsonObject> = std::sync::OnceLock::new();
    PROPS.get_or_init(|| {
        let mut props = serde_json::Map::new();
        props.insert("text".into(), json!({ "type": "string" }));
        props.insert(
            "fix_mode".into(),
            json!({
                "type": "string",
                "enum": ["none", "orthographic", "lexical_safe", "lexical_contextual"]
            }),
        );
        props.insert("max_errors".into(), json!({ "type": "integer" }));
        props.insert("max_warnings".into(), json!({ "type": "integer" }));
        props.insert("profile".into(), json!({
                "type": "string",
                "enum": ["base", "strict"],
                "description": "Norm strictness: 'base' (default) or 'strict' (full MoE with character variants)"
            }));
        props.insert("spacing".into(), json!({
                "type": "string",
                "enum": ["require", "strip"],
                "description": "CJK/Latin and CJK/digit boundary policy: require spaces (default), or strip stored spaces for renderer autospacing"
            }));
        props.insert("relaxed".into(), json!({
                "type": "boolean",
                "description": "Capability flag for software UI strings: disables colon enforcement, dunhao detection, grammar checks; uses en-dash for ranges"
            }));
        props.insert("off".into(), json!({
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": crate::rules::ruleset::RuleFamily::ALL
                        .iter()
                        .map(|family| family.name())
                        .collect::<Vec<_>>(),
                },
                "description": "Disable named rule families after the profile and capability flags resolve"
            }));
        props.insert("exempt_blockquotes".into(), json!({
                "type": "boolean",
                "description": "Markdown only: exclude pulldown-cmark `Tag::BlockQuote` ranges from scanning.  Useful when a document quotes mainland-Chinese sources for illustrative purposes.  Off by default."
            }));
        props.insert(
            "content_type".into(),
            json!({
                "type": "string",
                "default": "plain",
                "enum": ["plain", "markdown", "markdown-scan-code", "yaml", "json"]
            }),
        );
        props.insert(
            "political_stance".into(),
            json!({
                "type": "string",
                "enum": ["roc_centric", "international", "neutral"]
            }),
        );
        props.insert(
            "ignore_terms".into(),
            json!({
                "type": "array",
                "items": { "type": "string" }
            }),
        );
        props.insert("glossary".into(), json!({
                "type": "object",
                "description": "Project-level glossary.  `banned` terms always fire (project-wide truth, banned > TM); `proper_nouns` suppress matching issues; `preferred` chooses canonical TW form for the consistency report.",
                "properties": {
                    "banned": { "type": "array", "items": { "type": "string" } },
                    "preferred": { "type": "array", "items": { "type": "string" } },
                    "proper_nouns": { "type": "array", "items": { "type": "string" } },
                }
            }));
        props.insert("consistency".into(), json!({
                "type": "boolean",
                "description": "Emit a `consistency` block when both regional variants of one concept appear in the document (e.g. both 線程 and 執行緒).  Off by default."
            }));
        props.insert("explain".into(), json!({ "type": "boolean" }));
        props.insert("fix_output".into(), json!({
                "type": "string",
                "enum": ["full", "search_replace", "patch"],
                "description": "Fix output format: full text (default), search/replace blocks, or patch array with byte offsets"
            }));
        #[cfg(feature = "translate")]
        props.insert(
            "verify".into(),
            json!({
                "type": "boolean",
                "description": "Anchor-verify issues via Google Translate. \
Sends the sentences around each issue to Google. Off unless asked, and refused \
when the server has ZHTW_NO_NETWORK set."
            }),
        );
        props.insert("output".into(), json!({
                "type": "string",
                "enum": ["full", "compact", "tabular", "summary"],
                "description": "Output mode. 'summary' returns only issue counts + AI signature (no individual issues)"
            }));
        props.insert("detect_ai".into(), json!({
                "type": "boolean",
                "description": "Enable AI writing artifact detection (density + grammar patterns). Default: on. Set false to suppress AI filler findings."
            }));
        props.insert("detect_translationese".into(), json!({
                "type": "boolean",
                "description": "Enable translationese (翻譯腔 / 歐化) detection — Europeanized syntax and calques from the dewesternise checklist. Default: on. Orthogonal to detect_ai; reported separately."
            }));
        props.insert("detect_style".into(), json!({
                "type": "boolean",
                "description": "Composite style scorecard: emit `style_scorecard` with three orthogonal axes (ai, translationese, regional_density) plus top contributing issues. Default: false. Three scores never collapsed into a single number."
            }));
        props.insert("translationese_domain".into(), json!({
                "type": "string",
                "enum": ["general", "technical", "literary", "news"],
                "description": "Per-domain calibration for translationese scoring thresholds. 'technical' tolerates more passive voice and weak-verb nominalization; 'literary' is the strictest; 'news' favors active voice. Default: 'general'."
            }));
        props.insert("document_genre".into(), json!({
                "type": "string",
                "enum": ["casual", "technical", "financial"],
                "description": "How strictly the document is held to sourcing, for unsupported authority attributions. Requires detect_ai. Distinct from the register parameter, which is a property of the prose and suppresses findings; this one only selects advice and never suppresses. Never suggests an edit: casual prose is advised to name the source or drop the appeal, technical and financial prose that the claim needs a citation. Default: casual."
            }));
        props.insert("register".into(), json!({
                "type": "string",
                "enum": ["auto", "formal", "casual"],
                "description": "Register the document is written in. 'auto' (default) reads it off the text: a 公文 opens 敬啟者 and signs off 謹啟. 'formal' licenses the forms that register mandates, so 予以核准 and 因為…所以 stop being reported. Suppression only; never changes what is suggested for anything it does report."
            }));
        props.insert("rhythm".into(), json!({
                "type": "boolean",
                "description": "Advisory rhythm (氣口) checks: over-long sentences, consecutive sentences closing on the same particle, and a relaxed 定語堆疊 gate. Default: false. Advisory only, never applied by any fix tier."
            }));
        props.insert("ai_threshold".into(), json!({
                "type": "string",
                "enum": ["low", "medium", "high"],
                "description": "AI detection sensitivity: 'low' (sensitive, catches more), 'medium' (balanced), 'high' (conservative). Only effective with detect_ai=true"
            }));
        props.insert("include_telemetry".into(), json!({
                "type": "boolean",
                "description": "Include per-request token telemetry metrics in the response (LLM cost accounting)"
            }));
        props.insert("include_stats".into(), json!({
                "type": "boolean",
                "description": "Include per-issue resolution tier and session-level summary_metrics (deterministic/heuristic/llm_judged/unresolved counts, confidence distribution)"
            }));
        props
    })
}

pub(super) fn tool_definitions() -> Vec<Tool> {
    // Cloning the Arc, not the schema: the value is identical on every listing,
    // and deep-copying a dozen nested property objects to produce it again is
    // work with no result.
    let input_schema = input_schema().clone();

    vec![Tool::new(
        "zhtw",
        "Lint/fix/gate zh-TW text. Auto-converts Simplified Chinese to Traditional before applying rules, except for JSON input. Use verify=true to calibrate issues via Google Translate anchor matching.",
        input_schema,
    )
    .with_annotations(ToolAnnotations::new().read_only(true).idempotent(true))]
}

/// The tools this server exposes.
///
/// The lists below all say the same thing about caching, and say it because
/// they have to: `ttlMs` and `cacheScope` are required of a cacheable result
/// from 2026-07-28 on and the SDK leaves both unset. Zero and private is the
/// honest answer here, since the ruleset is fixed for the process but a
/// restart with different overrides or packs changes these lists and nothing
/// would tell the client.
pub(crate) fn list_tools() -> ListToolsResult {
    ListToolsResult::with_all_items(tool_definitions())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn list_resources() -> ListResourcesResult {
    resources::list_resources()
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

/// No resource templates: this server exposes two fixed URIs and no patterns.
pub(crate) fn list_resource_templates() -> ListResourceTemplatesResult {
    ListResourceTemplatesResult::with_all_items(Vec::new())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn list_prompts() -> ListPromptsResult {
    ListPromptsResult::with_all_items(prompts::list_prompts())
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
}

pub(crate) fn get_prompt(
    name: &str,
    arguments: &std::collections::HashMap<String, String>,
) -> ParamResult<GetPromptResult> {
    prompts::get_prompt(name, arguments)
        .ok_or_else(|| ErrorData::invalid_params(format!("unknown prompt: {name}"), None))
}
