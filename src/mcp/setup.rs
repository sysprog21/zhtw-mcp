// Agentic Editor integration: generate host-specific configuration content.
//
// Supports MCP-capable host integrations:
//   - Claude Code: CLAUDE.md section for zh-TW grounding
//   - Codex:       MCP registration command + AGENTS.md guidance
//   - OpenCode:    Skill definition YAML
//   - Copilot:     copilot-instructions.md + VS Code MCP settings

/// Generate a CLAUDE.md section for Claude Code integration.
///
/// Embeds the normalize_tone prompt content and references zhtw-mcp tools
/// so Claude Code automatically enforces zh-TW conventions.
pub fn claude_code_section() -> String {
    r#"## zh-TW Writing Standards (zhtw-mcp)

Use `zhtw` to lint/fix/gate Traditional Chinese (Taiwan) text per MoE standards.
Read `zh-tw://style-guide/moe` resource for full conventions.

### Quick Reference

- Terms: 軟體 (not 軟件), 資訊 (not 信息), 預設 (not 默認)
- Punctuation: full-width ，。：；！？ in CJK prose; 「」 quotes, 『』 nested
- Profiles: `base` (default) | `strict` (char variants). Flags: `relaxed` (UI strings), `detect_ai` (AI writing review)

### Quality Gate

```
zhtw({ "text": "...", "fix_mode": "lexical_safe", "max_errors": 0, "output": "compact" })
```

Re-run until `accepted: true`. Use `output: "compact"` to save context tokens."#
        .to_string()
}

/// Generate an OpenCode skill definition YAML.
pub fn opencode_skill() -> String {
    r#"# OpenCode Skill: zh-TW Text Linting
# Place in .opencode/skills/zhtw-lint.yaml

name: zhtw-lint
description: Lint and fix Traditional Chinese (Taiwan) text using MoE standards
trigger:
  # Activate when working with Chinese text files
  file_patterns:
    - "*.md"
    - "*.txt"
    - "*.rst"
  content_patterns:
    - "[\u4e00-\u9fff]"  # CJK Unified Ideographs

steps:
  - name: check
    tool: zhtw
    arguments:
      text: "{{content}}"
      fix_mode: "lexical_safe"
      max_errors: 0
      content_type: "{{if file_ext == 'md'}}markdown{{else}}plain{{end}}"
      profile: "base"

context:
  resources:
    - zh-tw://style-guide/moe
  prompts:
    - normalize_tone"#
        .to_string()
}

/// Generate GitHub Copilot integration instructions.
///
/// Returns a tuple of (copilot_instructions_md, vscode_settings_json_snippet).
pub fn copilot_config() -> (String, String) {
    let instructions = r#"# GitHub Copilot Instructions for zh-TW

When generating or editing Traditional Chinese (Taiwan) text in this project,
follow Ministry of Education (教育部) standards:

## Vocabulary
Use Taiwan-standard terms, not Mainland China equivalents:
- 軟體 (not 軟件), 硬體 (not 硬件), 網路 (not 網絡)
- 資訊 (not 信息), 預設 (not 默認), 列印 (not 打印)
- 品質 (not 質量 for "quality"), 影片 (not 視頻)
- 螢幕 (not 屏幕), 程式 (not 程序 for "program")
- 滑鼠 (not 鼠標), 介面 (not 接口 for "interface")
- 伺服器 (not 服務器), 記憶體 (not 內存)

## Punctuation
- Use full-width punctuation in CJK prose: ，。：；！？（）
- Use 「」 for primary quotation marks, 『』 for nested quotes
- Use 、(dunhao) for enumerating items, not ，
- Use 《》 for book/publication titles

## Character Forms
- Use MoE standard forms: 裡 (not 裏), 線 (not 綫), 麵 (not 麪), 著 (not 着 as particle)

## MCP Server
The zhtw-mcp server provides automated zh-TW linting and fixing.
Use `zhtw` with `fix_mode: "lexical_safe"` and `max_errors: 0` as a quality gate before committing Chinese text."#
        .to_string();

    let vscode_settings = r#"{
  "github.copilot.chat.codeGeneration.instructions": [
    {
      "file": ".github/copilot-instructions.md"
    }
  ],
  "mcp": {
    "servers": {
      "zhtw-mcp": {
        "command": "zhtw-mcp",
        "args": [],
        "env": {}
      }
    }
  }
}"#
    .to_string();

    (instructions, vscode_settings)
}

/// Supported host editors for integration setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    ClaudeCode,
    Codex,
    OpenCode,
    Copilot,
    Cursor,
    Windsurf,
    Cline,
    ContinueDev,
    Generic,
}

impl Host {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "claude_code" | "claude-code" => Some(Self::ClaudeCode),
            "codex" | "codex-cli" => Some(Self::Codex),
            "opencode" | "open-code" => Some(Self::OpenCode),
            "copilot" | "github-copilot" => Some(Self::Copilot),
            "cursor" => Some(Self::Cursor),
            "windsurf" => Some(Self::Windsurf),
            "cline" => Some(Self::Cline),
            "continue" | "continue-dev" | "continue.dev" => Some(Self::ContinueDev),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Cline => "cline",
            Self::ContinueDev => "continue",
            Self::Generic => "generic",
        }
    }
}

/// All supported hosts.
pub const ALL_HOSTS: &[Host] = &[
    Host::ClaudeCode,
    Host::Codex,
    Host::OpenCode,
    Host::Copilot,
    Host::Cursor,
    Host::Windsurf,
    Host::Cline,
    Host::ContinueDev,
    Host::Generic,
];

/// Generate Codex CLI integration instructions.
pub fn codex_instructions() -> String {
    r#"# Codex integration for zhtw-mcp

Register the MCP server under the short name `zhtw` so tool calls appear as
`mcp__zhtw.zhtw`:

```bash
codex mcp add zhtw -- /path/to/zhtw-mcp
```

Replace `/path/to/zhtw-mcp` with the installed binary path, for example
`/Users/you/.local/bin/zhtw-mcp`.

Add this guidance to `AGENTS.md`:

```markdown
When editing Traditional Chinese (Taiwan) text, use the `zhtw` MCP tool to
lint/fix/gate output against Taiwan MoE conventions. Prefer
`fix_mode: "lexical_safe"` for deterministic corrections and use
`content_type: "markdown"` for Markdown files.
```

After installing or rebuilding zhtw-mcp, restart Codex so it launches the new
binary. Run `codex mcp get zhtw` to confirm the configured command."#
        .to_string()
}

/// Generate Cursor rules file content.
pub fn cursor_rules() -> String {
    r#"# Cursor Rules: zh-TW Writing Standards (zhtw-mcp)

## Language Standards
All Chinese text in this project must follow Taiwan Ministry of Education (教育部) standards.
The zhtw-mcp MCP server is available for automated enforcement.

## Tool Usage
Use `zhtw` for linting, fixing, and gating zh-TW text:
- Lint: `zhtw({ "text": "...", "content_type": "markdown" })`
- Fix:  `zhtw({ "text": "...", "fix_mode": "lexical_safe", "max_errors": 0 })`
- Gate: `zhtw({ "text": "...", "max_errors": 0 })` — fails if errors > 0

## Key Conventions
- Taiwan terms: 軟體 (not 軟件), 資訊 (not 信息), 預設 (not 默認), 程式 (not 程序)
- Use full-width punctuation in CJK: ，。：；！？
- Quotes: 「」 primary, 『』 nested
- MoE character forms: 裡 (not 裏), 線 (not 綫), 著 (not 着)

## Profiles
- `base`: Standard vocabulary + punctuation (default)
- `strict`: Full MoE enforcement including character variants

## Capability Flags
- `relaxed`: Relaxed for software UI (disables colon/dunhao/grammar, uses en-dash)
- `detect_ai`: AI writing review — detects filler phrases, semantic safety words, copula/passive overuse"#
        .to_string()
}

/// Generate Windsurf rules file content.
pub fn windsurf_rules() -> String {
    r#"# Windsurf Rules: zh-TW Writing Standards

All Chinese text must follow Taiwan MoE (教育部) standards.
The zhtw-mcp MCP server provides automated zh-TW linting and fixing.

## MCP Tool: zhtw
- `zhtw({ "text": "...", "fix_mode": "lexical_safe", "max_errors": 0 })`
- Profiles: base, strict. Flags: relaxed (UI), detect_ai (AI writing review)
- Content types: plain, markdown, markdown-scan-code, yaml, json

## Taiwan-Standard Terms
軟體 (not 軟件), 資訊 (not 信息), 預設 (not 默認), 程式 (not 程序),
網路 (not 網絡), 硬體 (not 硬件), 品質 (not 質量), 螢幕 (not 屏幕)

## Punctuation
Full-width in CJK prose: ，。：；！？（）
Quotes: 「」 primary, 『』 nested, 《》 book titles
Ellipsis: …… (two U+2026), Em dash: —— (two U+2014)"#
        .to_string()
}

/// Generate Cline rules file content.
pub fn cline_rules() -> String {
    r#"# Cline Rules: zh-TW Writing Standards

## MCP Server
zhtw-mcp provides `zhtw` for Traditional Chinese (Taiwan) text enforcement.

## Workflow
1. When generating Chinese text, use Taiwan-standard vocabulary
2. Before finalizing, run: `zhtw({ "text": "...", "fix_mode": "lexical_safe", "max_errors": 0 })`
3. If `accepted: false`, fix remaining issues and re-check

## Quick Reference
- Terms: 軟體/資訊/預設/程式/網路/硬體/品質/螢幕 (TW standard)
- Punctuation: ，。：；！？ (full-width in CJK), 「」『』 (quotes)
- Profiles: base | strict. Flags: relaxed (UI), detect_ai (AI writing review)"#
        .to_string()
}

/// Generate Continue.dev MCP configuration.
pub fn continuedev_config() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": [{
            "name": "zhtw-mcp",
            "command": "zhtw-mcp",
            "args": [],
            "env": {}
        }],
        "customInstructions": "When writing Traditional Chinese (Taiwan) text, use Taiwan MoE standards. Use the zhtw MCP tool to lint and fix text. Key terms: 軟體 (not 軟件), 資訊 (not 信息), 預設 (not 默認). Use full-width punctuation in CJK prose."
    }))
    .expect("Value serialization is infallible")
}

/// Generate a generic platform-agnostic instruction file.
pub fn generic_instructions() -> String {
    r#"# zhtw-mcp: zh-TW Text Quality Enforcement

## What It Does
zhtw-mcp is an MCP server that enforces Traditional Chinese (Taiwan) writing standards
per the Ministry of Education (教育部) guidelines. It detects mainland Chinese vocabulary,
incorrect punctuation, and non-standard character variants in your text.

## MCP Tool: zhtw
The single unified tool for linting, fixing, and gating zh-TW text.

### Parameters
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| text | string | (required) | Text to check |
| fix_mode | string | "none" | "none", "orthographic", "lexical_safe", or "lexical_contextual" |
| max_errors | integer | (none) | Gate: reject if errors exceed this |
| profile | string | "base" | "base" or "strict" (full MoE with character variants) |
| relaxed | boolean | false | UI strings mode: disables colon/dunhao/grammar, en-dash ranges |
| content_type | string | "plain" | "plain", "markdown", "markdown-scan-code", "yaml" or "json" |
| political_stance | string | "roc_centric" | "roc_centric", "international", "neutral" |
| ignore_terms | array | [] | Terms to downgrade to Info severity |
| explain | boolean | false | Attach cultural explanations to issues |

### Workflow
1. Lint: `zhtw({ "text": "...", "content_type": "markdown" })`
2. Fix:  `zhtw({ "text": "...", "fix_mode": "lexical_safe" })`
3. Gate: `zhtw({ "text": "...", "max_errors": 0 })` — accepted=false if errors>0

### MCP Resources
- `zh-tw://style-guide/moe` — Full MoE style guide (punctuation, variants, vocabulary)
- `zh-tw://dictionary/ambiguous` — Terms needing LLM disambiguation

### MCP Prompts
- `normalize_tone` — Editorial persona for naturalizing zh-TW text

## Taiwan-Standard Vocabulary (Common Substitutions)
| Mainland (CN) | Taiwan (TW) | English |
|---------------|-------------|---------|
| 軟件 | 軟體 | Software |
| 信息 | 資訊 | Information |
| 默認 | 預設 | Default |
| 程序 | 程式 | Program |
| 網絡 | 網路 | Network |
| 質量 | 品質 | Quality |

## Punctuation Rules
- Use full-width punctuation in CJK prose: ，。：；！？（）
- Quotes: 「primary」, 『nested』, 《book title》
- Ellipsis: …… (two U+2026), Em dash: —— (two U+2014)
- Enum comma: 、(dunhao) for list items"#
        .to_string()
}

/// Generate integration content for a specific host.
///
/// Returns a JSON-serializable structure with the configuration content.
pub fn generate_for_host(host: Host) -> serde_json::Value {
    match host {
        Host::ClaudeCode => {
            serde_json::json!({
                "host": "claude_code",
                "file": ".claude/CLAUDE.md",
                "instruction": "Append the following section to your project's CLAUDE.md file:",
                "content": claude_code_section(),
            })
        }
        Host::Codex => {
            serde_json::json!({
                "host": "codex",
                "file": "AGENTS.md",
                "instruction": "Register the MCP server with Codex CLI and add the following guidance to AGENTS.md:",
                "content": codex_instructions(),
            })
        }
        Host::OpenCode => {
            serde_json::json!({
                "host": "opencode",
                "file": ".opencode/skills/zhtw-lint.yaml",
                "instruction": "Save the following as a skill definition file:",
                "content": opencode_skill(),
            })
        }
        Host::Copilot => {
            let (instructions, vscode_settings) = copilot_config();
            serde_json::json!({
                "host": "copilot",
                "files": [
                    {
                        "path": ".github/copilot-instructions.md",
                        "content": instructions,
                    },
                    {
                        "path": ".vscode/settings.json (merge into existing)",
                        "content": vscode_settings,
                    }
                ],
                "instruction": "Create the copilot-instructions.md file and merge the MCP server settings into your VS Code settings.json:",
            })
        }
        Host::Cursor => {
            serde_json::json!({
                "host": "cursor",
                "file": ".cursor/rules",
                "instruction": "Save the following as your Cursor rules file:",
                "content": cursor_rules(),
            })
        }
        Host::Windsurf => {
            serde_json::json!({
                "host": "windsurf",
                "file": ".windsurfrules",
                "instruction": "Save the following as your Windsurf rules file:",
                "content": windsurf_rules(),
            })
        }
        Host::Cline => {
            serde_json::json!({
                "host": "cline",
                "file": ".clinerules",
                "instruction": "Save the following as your Cline rules file:",
                "content": cline_rules(),
            })
        }
        Host::ContinueDev => {
            serde_json::json!({
                "host": "continue",
                "file": ".continue/config.json (merge into existing)",
                "instruction": "Merge the following MCP server configuration into your Continue.dev config:",
                "content": continuedev_config(),
            })
        }
        Host::Generic => {
            serde_json::json!({
                "host": "generic",
                "file": ".zhtw-mcp.md",
                "instruction": "Save the following as a platform-agnostic instruction file that any MCP-aware agent can read:",
                "content": generic_instructions(),
            })
        }
    }
}

/// Generate a zh-TW translation style guide as a JSON setup object.
///
/// Returns a `serde_json::Value` following the same JSON contract as
/// `generate_for_host()`: {host, file, instruction, content}.
/// Designed to be injected into LLM system prompts to prevent common
/// AI writing artifacts at generation time. Covers: cross-strait
/// terminology, semantic safety alternatives, nominalization avoidance,
/// filler prohibition, and verb-driven syntax.
pub fn generate_translation_guide() -> serde_json::Value {
    let guide = translation_guide_text();
    serde_json::json!({
        "host": "translation-guide",
        "file": "(system prompt injection)",
        "instruction": "Inject the following into your LLM system prompt:",
        "content": guide,
    })
}

/// The raw translation guide text content.
fn translation_guide_text() -> String {
    r#"# 繁體中文（台灣）翻譯風格指南

## 目的
本指南用於 LLM 系統提示，確保產出的繁體中文文本符合台灣教育部標準，
避免常見的 AI 翻譯偽跡（translation artifact）。

## 詞彙規範

### 跨海峽術語
使用台灣慣用譯名，避免中國大陸用語：
- 軟體（非「軟件」）、硬體（非「硬件」）
- 程式（非「程序」，指 program）、程式碼（非「代碼」）
- 記憶體（非「內存」）、網路（非「網絡」）
- 資料庫（非「數據庫」）、資料（非「數據」，指 data）
- 伺服器（非「服務器」）、瀏覽器（非簡體「浏览器」）
- 滑鼠（非「鼠標」）、列印（非「打印」）
- 預設（非「默認」）、支援（非「支持」，指 support）

### 語意安全詞
避免直接翻譯「means」為「意味著」。根據語境選擇：
- 定義語境 →「表示」（X 表示 Y）
- 因果語境 →「代表」（這代表我們需要……）
- 解釋語境 →「也就是說」

### 避免繁複動詞
- 避免「作為」「標誌著」「充當」等書面語堆疊，改以簡潔句式表達
- 避免「擁有」「設有」等冗餘動詞（技術文件語境），直接敘述
- 用主動語態取代「被廣泛使用」→「廣泛使用」

## AI 寫作偽跡禁忌

### 禁用填充詞
以下片語在 AI 生成文本中出現頻率異常高，應避免或替換：
- ❌ 值得注意的是 → ✅ 直接陳述事實
- ❌ 需要注意的是 → ✅ 直接陳述事實
- ❌ 更重要的是 → ✅ 精簡為直述句
- ❌ 在某種程度上 → ✅ 刪除或改為具體程度
- ❌ 不容忽視 → ✅ 用具體影響取代
- ❌ 深刻影響 → ✅ 說明具體影響

### 禁用說教句式
- ❌ 讓我們……（祈使句開頭）
- ❌ 我們需要理解……（居高臨下）
- ❌ ……是非常重要的（空泛強調）

### 結構限制
- 列表只用於本來就是並列的項目；敘述性內容寫成段落，不要為了版面把句子拆成條列
- 避免公式化段落結尾：「這……證明了」「正是這……讓」
- 避免二元對比堆疊：同一段落不要連續使用「雖然……但」「不僅……更」
- 破折號（——）本身不是問題，單獨一個不必改；只有同一段反覆堆疊時才需要檢視
- 避免公式化標題：「挑戰與未來展望」「結論與展望」「核心優勢」

## 標點符號
- 使用全形標點：，。！？；：（）「」『』
- 引號層級：外層「」，內層『』
- 刪節號：使用「……」（兩個 U+2026），非「...」
- 數字範圍：使用「～」或「–」，非「-」

## 語法
- CJK 與半形字母/數字間加一個半形空格
- 避免 的/地/得 誤用
- 句子以動詞驅動，減少名詞化（nominalization）
"#
    .to_string()
}

#[cfg(test)]
#[path = "../../tests/unit/mcp/setup/tests.rs"]
mod tests;
