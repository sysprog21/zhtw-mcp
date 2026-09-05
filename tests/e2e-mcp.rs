// End-to-end MCP protocol test.
//
// Spawns the zhtw-mcp binary, sends JSON-RPC messages over stdin, and verifies
// the stdout responses match expected structure and content.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Send a JSON-RPC request to the child process and read the response.
fn send_recv(stdin: &mut impl Write, stdout: &mut impl BufRead, request: &Value) -> Value {
    let msg = serde_json::to_string(request).unwrap();
    writeln!(stdin, "{}", msg).unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(line.trim()).expect("response should be valid JSON")
}

fn send_recv_skip_notifications(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: &Value,
) -> (Vec<Value>, Value) {
    let line = serde_json::to_string(request).unwrap();
    writeln!(stdin, "{line}").unwrap();
    stdin.flush().unwrap();
    let id = request.get("id").cloned();
    let mut notifications = Vec::new();
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        if value.get("id") == id.as_ref() {
            return (notifications, value);
        }
        notifications.push(value);
    }
}

/// Send a notification (no response expected).
fn send_notification(stdin: &mut impl Write, request: &Value) {
    let msg = serde_json::to_string(request).unwrap();
    writeln!(stdin, "{}", msg).unwrap();
    stdin.flush().unwrap();
}

/// Path to the binary under test.
///
/// `CARGO_BIN_EXE_<name>` is set by cargo for every integration test and
/// carries the platform's executable suffix.  Deriving it from
/// `current_exe()` instead, which every one of these test files used to do,
/// dropped the `.exe` on Windows and left a path that does not exist.
fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_zhtw-mcp"))
}

/// Spawn the server with a throwaway config and cache, and hold the temp dir
/// alive for the caller's session.
fn spawn_server() -> (
    tempfile::TempDir,
    std::process::Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    spawn_server_with(&[])
}

/// Same server, with extra arguments.
///
/// `--verbose` is the one that matters: at the default filter the server logs
/// nothing above warn, so a test about log delivery has no lines to deliver
/// and passes whatever the code does.
fn spawn_server_with(
    args: &[&str],
) -> (
    tempfile::TempDir,
    std::process::Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    let bin = binary_path();
    let tmp = tempfile::tempdir().expect("create temp dir for the server session");
    let mut child = Command::new(&bin)
        .args(args)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
        .env("XDG_CACHE_HOME", tmp.path().join(".cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zhtw-mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());

    // The caller holds the temp dir: dropping it early would pull the config
    // and cache directories out from under a live server.
    (tmp, child, stdin, stdout)
}

/// Drive a sampling-capable handshake and fire the tier-3 call, as id 2, that
/// is known to ask the client questions.
///
/// Separate from [`handshake`] because that one offers no capabilities, and the
/// server only builds a sampling bridge for a client that declared sampling.
fn start_sampling_call(stdin: &mut impl Write, stdout: &mut impl BufRead) {
    let init = send_recv(
        stdin,
        stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": { "sampling": {} },
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(init["result"].is_object(), "initialize: {init}");
    send_notification(
        stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    let text = "這個項目的質量和性能都很好，軟件的並行處理和內存管理需要優化。".repeat(3);
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "zhtw",
                "arguments": { "text": text, "profile": "strict", "output": "summary" }
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();
}

/// Answer every sampling question until the call with `id` answers back.
///
/// Returns how many questions were answered and the call's own reply, so a
/// caller can assert on either.
fn answer_sampling_until(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    id: i64,
) -> (usize, Value) {
    let mut answered = 0usize;
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("server closed early");
        let msg: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("expected a message, got {line:?}: {e}"));

        if msg["method"] == "sampling/createMessage" {
            answered += 1;
            writeln!(
                stdin,
                "{}",
                json!({
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "result": {
                        "role": "assistant",
                        "model": "test",
                        "content": { "type": "text", "text": "平行" }
                    }
                })
            )
            .unwrap();
            stdin.flush().unwrap();
        } else if msg["id"] == id {
            return (answered, msg);
        }
    }
}

/// Drive the stock handshake and return the `initialize` result.
///
/// Most tests want a session, not a particular handshake; the ones that are
/// about the handshake itself keep their own literal.
fn handshake(stdin: &mut impl Write, stdout: &mut impl BufRead) -> Value {
    let init = send_recv(
        stdin,
        stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(init["result"].is_object(), "initialize failed: {init}");
    send_notification(
        stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    init
}

#[test]
fn e2e_initialize_and_tools_list() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 0. Pre-init: tools/list before initialize should be rejected, and the
    // server must stay up to serve the handshake that follows.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 0,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 0);
    assert!(
        resp["error"].is_object(),
        "tools/list before initialize should return error"
    );
    assert_eq!(resp["error"]["code"], -32002); // SERVER_NOT_INITIALIZED
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not initialized"));

    // 1. Initialize
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    assert!(resp["result"]["capabilities"]["resources"].is_object());
    assert!(resp["result"]["capabilities"]["prompts"].is_object());
    assert_eq!(resp["result"]["serverInfo"]["name"], "zhtw-mcp");

    // 2. Notifications/initialized (no response)
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    // 3. Tools list: 1 tool: zhtw
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 2,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 2);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"zhtw"));

    // Verify tool annotations use the MCP-spec *Hint wire names; any other
    // spelling is silently dropped by spec-compliant clients.
    let zhtw = tools.iter().find(|t| t["name"] == "zhtw").unwrap();
    assert_eq!(zhtw["annotations"]["readOnlyHint"], true);
    assert_eq!(zhtw["annotations"]["idempotentHint"], true);
    assert!(zhtw["annotations"].get("destructiveHint").is_none());
    // Non-spec spellings must not appear on the wire.
    assert!(zhtw["annotations"].get("readOnly").is_none());
    assert!(zhtw["annotations"].get("idempotent").is_none());

    // Verify zhtw schema has expected parameters
    let props = &zhtw["inputSchema"]["properties"];
    assert!(props.get("text").is_some());
    assert!(props.get("fix_mode").is_some());
    assert!(props.get("max_errors").is_some());
    assert!(props.get("ignore_terms").is_some());
    assert!(props.get("profile").is_some());
    assert!(props.get("content_type").is_some());
    assert!(props.get("political_stance").is_some());
    assert!(props.get("include_telemetry").is_some());
    assert!(
        props.get("detect_style").is_some(),
        "detect_style must appear in zhtw schema"
    );

    // 4. zhtw lint-only (fix_mode absent = none): detect 軟件
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 3,
            "params": {
                "name": "zhtw",
                "arguments": { "text": "這個軟件很好用" }
            }
        }),
    );
    assert_eq!(resp["id"], 3);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(output["accepted"], true);
    assert_eq!(output["applied_fixes"], 0);
    assert_eq!(output["gate"]["enabled"], false);
    let issues = output["issues"].as_array().unwrap();
    assert!(!issues.is_empty());
    assert_eq!(issues[0]["found"], "軟件");
    assert!(issues[0]["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "軟體"));
    // text field returns original (no fixes)
    assert_eq!(output["text"], "這個軟件很好用");

    // 5. zhtw gate-pass: clean text + max_errors: 0 + fix_mode: safe
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 4,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟體很好用",
                    "fix_mode": "lexical_safe",
                    "max_errors": 0
                }
            }
        }),
    );
    assert_eq!(resp["id"], 4);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(output["accepted"], true);
    assert_eq!(output["gate"]["enabled"], true);
    assert_eq!(output["gate"]["residual_errors"], 0);

    // 6. zhtw gate-fix: dirty text + fix_mode: safe, verify fixes
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 5,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件用了很多內存",
                    "fix_mode": "lexical_safe"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 5);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(output["accepted"], true);
    let fixed_text = output["text"].as_str().unwrap();
    assert!(fixed_text.contains("軟體"));
    assert!(fixed_text.contains("記憶體"));
    assert!(output["applied_fixes"].as_u64().unwrap() > 0);

    // 7. zhtw with ignore_terms: 軟件 downgraded to info
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 6,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "ignore_terms": ["軟件"]
                }
            }
        }),
    );
    assert_eq!(resp["id"], 6);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let issues = output["issues"].as_array().unwrap();
    assert!(!issues.is_empty());
    let software_issue = issues.iter().find(|i| i["found"] == "軟件").unwrap();
    assert_eq!(software_issue["severity"], "info");
    // Summary should count it as info, not error
    assert_eq!(output["summary"]["info"].as_u64().unwrap(), 1);

    // 8. resources/list
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "resources/list",
            "id": 10,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 10);
    let resources = resp["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0]["uri"], "zh-tw://style-guide/moe");
    assert_eq!(resources[1]["uri"], "zh-tw://dictionary/ambiguous");

    // 9. resources/read: style guide
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "resources/read",
            "id": 11,
            "params": { "uri": "zh-tw://style-guide/moe" }
        }),
    );
    assert_eq!(resp["id"], 11);
    let contents = resp["result"]["contents"].as_array().unwrap();
    assert!(contents[0]["text"]
        .as_str()
        .unwrap()
        .contains("Punctuation"));

    // 10. prompts/list
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "prompts/list",
            "id": 12,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 12);
    let prompts = resp["result"]["prompts"].as_array().unwrap();
    assert!(!prompts.is_empty());
    assert_eq!(prompts[0]["name"], "normalize_tone");

    // 11. prompts/get
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "prompts/get",
            "id": 13,
            "params": { "name": "normalize_tone" }
        }),
    );
    assert_eq!(resp["id"], 13);
    let messages = resp["result"]["messages"].as_array().unwrap();
    assert!(!messages.is_empty());
    assert!(messages[0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("Traditional Chinese"));

    // -- E2E: content_type: "markdown" -- code inside fences excluded --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 20,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好\n\n```\n軟件 is ok in code\n```\n\n另一個軟件",
                    "content_type": "markdown"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 20);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let issues = output["issues"].as_array().unwrap();

    // "軟件" in fenced code block should be excluded; only prose occurrences
    // flagged
    let software_issues: Vec<_> = issues.iter().filter(|i| i["found"] == "軟件").collect();
    assert_eq!(
        software_issues.len(),
        2,
        "code block 軟件 should be excluded"
    );

    // -- E2E: profile: "strict" -- variant rules fire --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 21,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "裏面有線索",
                    "profile": "strict"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 21);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(output["profile"], "strict");
    let issues = output["issues"].as_array().unwrap();
    // strict should catch 裏→裡 variant
    let variant_issue = issues.iter().find(|i| i["found"] == "裏");
    assert!(variant_issue.is_some(), "strict should flag 裏 variant");
    assert!(variant_issue.unwrap()["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "裡"));

    // -- E2E: gate rejection (accepted: false, max_errors exceeded) -- "內地"
    // is political_coloring → Severity::Error, which the gate counts.

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 22,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "回到內地出差",
                    "max_errors": 0
                }
            }
        }),
    );
    assert_eq!(resp["id"], 22);

    // Gate rejection: isError=true on the result, output JSON has
    // accepted=false
    let result = &resp["result"];
    assert_eq!(
        result["isError"], true,
        "gate rejection should set isError=true"
    );
    let output_text = result["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(output_text).unwrap();
    assert_eq!(output["accepted"], false);
    assert_eq!(output["gate"]["enabled"], true);
    assert!(output["gate"]["residual_errors"].as_u64().unwrap() > 0);

    // -- E2E: fix_mode: "lexical_contextual" -- Uses 代碼 (clue-gated: needs
    // 編譯/函式/函數 nearby) + 軟件 (non-clue). lexical_safe would fix 軟件 but
    // skip 代碼; lexical_contextual fixes both.

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 23,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "編譯這個軟件的代碼",
                    "fix_mode": "lexical_contextual"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 23);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let fixed = output["text"].as_str().unwrap();
    assert!(
        fixed.contains("軟體"),
        "lexical_contextual should fix 軟件→軟體"
    );
    assert!(
        fixed.contains("程式碼"),
        "lexical_contextual should fix 代碼→程式碼 (clue-gated, 編譯 present)"
    );
    assert!(output["applied_fixes"].as_u64().unwrap() >= 2);

    // -- E2E: oversized request rejected by MAX_TEXT_BYTES --

    // Exactly 256 KiB should pass (boundary).
    let boundary_text = "a".repeat(256 * 1024);
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 24,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": boundary_text
                }
            }
        }),
    );
    assert_eq!(resp["id"], 24);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !content_text.contains("text too large"),
        "exactly 256 KiB should be accepted"
    );

    // 256 KiB + 1 byte should be rejected with INVALID_PARAMS and structured
    // data.
    let over_text = "a".repeat(256 * 1024 + 1);
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 25,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": over_text
                }
            }
        }),
    );
    assert_eq!(resp["id"], 25);
    let err = resp
        .get("error")
        .expect("expected JSON-RPC error for oversized text");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);
    let data = err.get("data").expect("expected structured data");
    assert_eq!(data["field"], "text", "data.field should be 'text'");

    // -- E2E: invalid arguments (missing text field) --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 26,
            "params": {
                "name": "zhtw",
                "arguments": {}
            }
        }),
    );
    assert_eq!(resp["id"], 26);
    let err = resp
        .get("error")
        .expect("expected JSON-RPC error for missing text");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);
    let data = err.get("data").expect("expected structured data");
    assert_eq!(data["field"], "text", "data.field should be 'text'");

    // -- E2E: invalid content_type rejected with structured data --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 27,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "測試",
                    "content_type": "html"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 27);
    let err = resp
        .get("error")
        .expect("expected JSON-RPC error for invalid content_type");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);
    let data = err.get("data").expect("expected structured data");
    assert_eq!(data["field"], "content_type");
    assert_eq!(data["value"], "html");
    let accepted = data["accepted"]
        .as_array()
        .expect("accepted should be array");
    assert!(
        accepted.iter().any(|v| v == "plain"),
        "accepted should include 'plain'"
    );

    // -- E2E: output: "compact", deduplicated issues, no text/trace fields --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 30,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "視頻和視頻都是視頻",
                    "output": "compact"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 30);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(output["accepted"], true);
    // Compact omits text field when no fixes applied.
    assert!(
        output.get("text").is_none(),
        "compact without fixes should omit text"
    );
    // Compact omits trace field.
    assert!(output.get("trace").is_none(), "compact should omit trace");
    // Issues should be deduplicated: 3x 視頻 collapses to 1 group.
    let issues = output["issues"].as_array().unwrap();
    let video_groups: Vec<_> = issues.iter().filter(|i| i["found"] == "視頻").collect();
    assert_eq!(
        video_groups.len(),
        1,
        "compact should deduplicate identical issues into one group"
    );
    assert_eq!(
        video_groups[0]["count"].as_u64().unwrap(),
        3,
        "deduplicated group should have count=3"
    );
    // Locations array should have 3 entries.
    assert_eq!(
        video_groups[0]["locations"].as_array().unwrap().len(),
        3,
        "locations should list all 3 occurrences"
    );
    // Compact uses shared IssueType::name() for rule_type field.
    assert_eq!(
        video_groups[0]["rule_type"], "cross_strait",
        "rule_type should use snake_case name"
    );

    // -- E2E: output: "compact" with fix_mode, text included when fixes applied
    // --

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 31,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好",
                    "output": "compact",
                    "fix_mode": "lexical_safe"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 31);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert!(output["applied_fixes"].as_u64().unwrap() > 0);
    // Compact includes text when fixes were applied.
    assert!(
        output.get("text").is_some(),
        "compact with fixes should include text"
    );
    assert!(
        output["text"].as_str().unwrap().contains("軟體"),
        "fixed text should contain 軟體"
    );

    // -- E2E: output: "compact" token reduction vs full --

    let test_text = "軟件用了視頻功能，視頻品質好。並行計算很快。";
    let resp_full = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 32,
            "params": {
                "name": "zhtw",
                "arguments": { "text": test_text, "output": "full" }
            }
        }),
    );
    let resp_compact = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 33,
            "params": {
                "name": "zhtw",
                "arguments": { "text": test_text, "output": "compact" }
            }
        }),
    );
    let full_len = resp_full["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .len();
    let compact_len = resp_compact["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .len();
    let reduction = 1.0 - (compact_len as f64 / full_len as f64);
    assert!(
        reduction >= 0.30,
        "MCP compact should achieve ≥30% reduction vs full: full={full_len} compact={compact_len} reduction={reduction:.2}"
    );

    // Close stdin to let the child exit gracefully.
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    // tmp_dir auto-cleaned on drop
}

#[test]
fn e2e_initialize_negotiates_every_supported_version() {
    // 2025-06-18 used to be answered with 2024-11-05 and a stderr warning. It
    // is a version this server serves, so it is answered with itself.
    for version in ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"] {
        let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
        let resp = send_recv(
            &mut stdin,
            &mut stdout,
            &json!({
                "jsonrpc": "2.0",
                "method": "initialize",
                "id": 0,
                "params": {
                    "protocolVersion": version,
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0.1" },
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": version,
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }),
        );
        assert_eq!(resp["result"]["protocolVersion"], version, "{version}");
        assert_eq!(resp["result"]["serverInfo"]["name"], "zhtw-mcp");
        drop(stdin);
        let _ = child.wait();
    }
}

#[test]
fn e2e_initialize_rejects_inline_only_version() {
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 0,
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );

    // Refused, but as a wrong entry point rather than an unsupported version:
    // the alternatives offered must not include the one just refused, or the
    // client is sent back to the method it already failed at.
    assert_eq!(resp["error"]["code"], -32022, "{resp}");
    assert_eq!(
        resp["error"]["data"]["entryPoint"], "server/discover",
        "{resp}"
    );
    let supported = resp["error"]["data"]["supported"]
        .as_array()
        .expect("the error names the versions reachable from here");
    assert!(!supported.contains(&json!("2026-07-28")), "{resp}");
    assert!(supported.contains(&json!("2025-11-25")), "{resp}");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_initialize_rejects_unsupported_version_with_32022() {
    // Since 2026-07-28 an unsupported version is a server error rather than a
    // client judgment call, and the reply says what is on offer.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 0,
            "params": {
                "protocolVersion": "1900-01-01",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );

    assert_eq!(resp["error"]["code"], -32022, "{resp}");
    assert_eq!(resp["error"]["message"], "Unsupported protocol version");
    assert_eq!(resp["error"]["data"]["requested"], "1900-01-01");
    let supported = resp["error"]["data"]["supported"]
        .as_array()
        .expect("the error names the versions on offer");
    assert!(supported.contains(&json!("2024-11-05")), "{resp}");

    // Only the handshake-reachable revisions: 2026-07-28 is served, but not
    // from initialize, so offering it here would be a dead end.
    assert!(!supported.contains(&json!("2026-07-28")), "{resp}");

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_discover_mid_session_keeps_the_client_it_was_told_about() {
    // server/discover may arrive after a handshake that already named the
    // client, and its _meta need not carry client info. Recording that absence
    // as the client's identity loses the name, and with it the compact output
    // an AI-agent client gets: the answer silently grows a text and a trace
    // field it had been told to leave out.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "1" }
            }
        }),
    );
    assert!(init["result"].is_object(), "initialize: {init}");
    send_notification(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    let discover = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
    );
    assert!(discover["result"].is_object(), "discover: {discover}");

    let (_notifications, call) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "zhtw", "arguments": { "text": "這個軟件的質量" } }
        }),
    );
    let body: Value = serde_json::from_str(
        call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected tool output: {call}")),
    )
    .expect("tool output is JSON");

    assert!(
        body.get("text").is_none() && body.get("trace").is_none(),
        "the client was still claude-code, so the answer should stay compact: {body}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_a_first_message_call_is_served_and_knows_its_client() {
    // 2026-07-28 deleted initialize, so a client is free to open a connection
    // and send a call on it with no handshake at all: the declaration rides in
    // every request's _meta. Claude Code does exactly that, running the
    // discovery probe in one process and the calls that follow in another, so
    // this is the shape the server actually meets in production and the one
    // every other test here was missing. Two things have to hold on it. The
    // call has to be answered rather than refused as pre-init, and the client
    // named in _meta has to reach the pipeline: read only at the handshake, the
    // name never arrives on this path and an agent client silently gets the
    // full answer where it had been getting the compact one.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let (_notifications, call) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "claude-code",
                        "version": "1"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    assert!(
        call["result"].is_object(),
        "a self-declaring call needs no handshake: {call}"
    );
    let body: Value = serde_json::from_str(
        call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected tool output: {call}")),
    )
    .expect("tool output is JSON");
    assert!(
        body.get("text").is_none() && body.get("trace").is_none(),
        "`_meta` named claude-code, so the answer should be compact: {body}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_a_first_message_call_from_a_handshake_revision_is_refused() {
    // The exemption is the property of a revision that has no handshake, not of
    // anything that puts a version in _meta. A client naming an older revision
    // there is not a client of that revision, and still owes the initialize its
    // own revision defines.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let call = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-06-18" },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    assert_eq!(
        call["error"]["code"], -32002,
        "a handshake revision does not get to skip its handshake: {call}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_every_termination_path_reports_the_code_it_promises() {
    // The exit contract in one place, because its cells have been wrong
    // separately: a reply queued and then abandoned, a drain that never
    // returned, a code that did not match. shutdown before exit means a clean 0
    // and an answered shutdown; exit alone means 1; end of input means 0. Each
    // holds before the handshake and after it, and those are different code
    // paths: the framing layer answers one, the SDK the other.
    struct Case {
        what: &'static str,
        handshake_first: bool,
        script: String,
        code: i32,
        expects_ack: bool,
    }

    let shutdown = json!({"jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": {}});
    let exit = json!({"jsonrpc": "2.0", "method": "exit"});
    let cases = vec![
        Case {
            what: "pre-handshake: exit alone",
            handshake_first: false,
            script: format!("{exit}\n"),
            code: 1,
            expects_ack: false,
        },
        Case {
            what: "pre-handshake: shutdown then exit, pipelined",
            handshake_first: false,
            script: format!("{shutdown}\n{exit}\n"),
            code: 0,
            expects_ack: true,
        },
        Case {
            what: "post-handshake: exit alone",
            handshake_first: true,
            script: format!("{exit}\n"),
            code: 1,
            expects_ack: false,
        },
        Case {
            what: "post-handshake: shutdown then exit, pipelined",
            handshake_first: true,
            script: format!("{shutdown}\n{exit}\n"),
            code: 0,
            expects_ack: true,
        },
        Case {
            what: "post-handshake: shutdown then end of input",
            handshake_first: true,
            script: format!("{shutdown}\n"),
            code: 0,
            expects_ack: true,
        },
        Case {
            what: "pre-handshake: end of input",
            handshake_first: false,
            script: String::new(),
            code: 0,
            expects_ack: false,
        },
        Case {
            what: "post-handshake: end of input",
            handshake_first: true,
            script: String::new(),
            code: 0,
            expects_ack: false,
        },
    ];

    for case in cases {
        let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
        if case.handshake_first {
            handshake(&mut stdin, &mut stdout);
        }
        write!(stdin, "{}", case.script).unwrap();
        stdin.flush().unwrap();
        drop(stdin);

        let mut acknowledged = false;
        let mut line = String::new();
        while stdout.read_line(&mut line).unwrap_or(0) > 0 {
            if let Ok(msg) = serde_json::from_str::<Value>(line.trim()) {
                if msg["id"] == 99 {
                    // Shape as well as presence: the answer to shutdown is an
                    // empty result, not merely something bearing its id.
                    assert_eq!(msg["result"], json!({}), "{}: shutdown reply", case.what);
                    acknowledged = true;
                }
            }
            line.clear();
        }
        let status = child.wait().unwrap();

        assert_eq!(
            status.code(),
            Some(case.code),
            "{}: wrong exit code",
            case.what
        );
        assert_eq!(
            acknowledged, case.expects_ack,
            "{}: shutdown acknowledgement",
            case.what
        );
    }
}

#[test]
fn e2e_exit_terminates_after_an_answered_call_with_stdin_open() {
    // The exit contract with nothing else left to wake the server.
    //
    // RMCP polls the framing layer inside a select whose other arms carry the
    // outgoing queue and the completion of the task that wrote the last
    // response, so the read future is dropped on either. A termination awaited
    // inside that future is cancelled halfway with the exit line already
    // consumed: nothing terminates, the server goes back to reading stdin, and
    // because this client never closes stdin there is no end of input to fall
    // back on. It parks forever.
    //
    // Answer a call in full and only then send exit, so a send task is on the
    // verge of reporting completion when the exit is gated. A single round
    // lands in that window about one time in twelve, measured against the
    // broken version, so the round is repeated. Twelve rounds caught it in four
    // runs out of six; that is a guard, not a proof, and it is stated here so
    // nobody reads a green run as one. The failure mode is a hang and never a
    // wrong answer, so the repetition cannot make this flaky in the other
    // direction. Two to four seconds on its own depending on the machine, and
    // almost all of that is spawning twelve processes rather than waiting on
    // them: the child is reaped about a millisecond after the exit line, which
    // is what the poll interval below is set from.
    for round in 1..=12 {
        let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
        handshake(&mut stdin, &mut stdout);

        // Rejected on its arguments, which answers without a scan and puts the
        // reply and the exit as close together as this interface allows.
        let answer = send_recv(
            &mut stdin,
            &mut stdout,
            &json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": "這個軟件很好用",
                        "output": "tabular",
                        "include_telemetry": true
                    }
                }
            }),
        );
        assert!(
            answer["error"].is_object(),
            "expected a rejection: {answer}"
        );

        send_notification(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));

        // Held open deliberately, so exit is the only thing that can end this.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            match child.try_wait().expect("poll the server") {
                Some(status) => break status,
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "round {round}: exit was gated and then lost, the server never terminated"
                    );
                }
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        };

        // No shutdown came first, so the contract says 1.
        assert_eq!(status.code(), Some(1), "round {round}: wrong exit code");
    }
}

#[test]
fn e2e_cancelling_a_call_stops_it_waiting_on_sampling() {
    // A cancelled request is not owed an answer and the client that cancelled
    // it will not send one, so the sampling deadline is time spent for nothing,
    // with the server lock held throughout. The discriminator is how soon the
    // next call that needs that lock is served. Waiting the deadline out costs
    // five seconds a question and the budget allows three, measured at just
    // over fifteen seconds; observing the cancellation ends the wait at once
    // and the remaining scan is milliseconds. The threshold sits well below the
    // five-second deadline rather than at it, so a single call that had already
    // burned part of its deadline would still be caught.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": { "sampling": {} },
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(init["result"].is_object(), "initialize: {init}");
    send_notification(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    let text = "這個項目的質量和性能都很好，軟件的並行處理和內存管理需要優化。".repeat(3);
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "zhtw",
                "arguments": { "text": text, "profile": "strict", "output": "summary" }
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    // Wait for the first question, then cancel rather than answering it.
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("server closed early");
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if msg["method"] == "sampling/createMessage" {
            break;
        }
    }
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 2, "reason": "user cancelled" }
        }),
    );

    // The next call needs the same lock the cancelled scan is holding.
    let started = std::time::Instant::now();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "zhtw", "arguments": { "text": "軟件", "output": "summary" } }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("server closed early");
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if msg["id"] == 3 {
            break;
        }
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the cancelled call held the server for {elapsed:?}, so it waited out \
         its sampling deadlines instead of noticing the cancellation"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_framing_replies_survive_concurrent_responses() {
    // The framing layer's own replies are produced inside receive, which RMCP
    // polls in a select! and drops whenever another arm wins. Writing from in
    // there loses the reply and the line that caused it is already consumed, so
    // the client waits on an answer that was never written. It only shows up
    // under concurrent traffic: one bad line on an idle server is always
    // answered.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    handshake(&mut stdin, &mut stdout);

    const N: usize = 50;
    for i in 0..N {
        // Unparsable: answered by the framing layer with -32700.
        writeln!(stdin, "{{not json at all").unwrap();
        // Answered by RMCP, which is what keeps the select! busy.
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": 1000 + i, "method": "tools/list", "params": {}})
        )
        .unwrap();
    }
    stdin.flush().unwrap();

    let mut parse_errors = 0usize;
    let mut results = 0usize;
    while parse_errors + results < N * 2 {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if msg["error"]["code"] == -32700 {
            parse_errors += 1;
        } else if msg.get("result").is_some() {
            results += 1;
        }
    }
    assert_eq!(results, N, "every tools/list should be answered");
    assert_eq!(
        parse_errors, N,
        "every unparsable line should be answered too, not dropped when the \
         transport happens to be writing something else"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_initialize_after_discover_answers_the_version_asked_for() {
    // RMCP patches the negotiated version onto the result only when the session
    // began with the handshake. Opening with server/discover first takes the
    // other path, where the reply used to name the server default rather than
    // the version requested.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let discover = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
    );
    assert!(discover["result"].is_object(), "discover: {discover}");

    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert_eq!(
        init["result"]["protocolVersion"], "2025-06-18",
        "answered a different revision than the one asked for: {init}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_bad_params_on_a_known_method_is_not_method_not_found() {
    // ClientRequest is untagged with CustomRequest last, so a known method
    // whose params are the wrong shape falls through to the custom handler.
    // Answering METHOD_NOT_FOUND there tells a client its tool does not exist.
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    for method in ["tools/call", "resources/read", "prompts/get"] {
        let (_notifications, resp) = send_recv_skip_notifications(
            &mut stdin,
            &mut stdout,
            &json!({"jsonrpc": "2.0", "id": 42, "method": method, "params": {}}),
        );
        assert_eq!(
            resp["error"]["code"], -32602,
            "{method} with bad params should be invalid params: {resp}"
        );
    }

    // A method that genuinely does not exist still says so.
    let (_notifications, resp) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({"jsonrpc": "2.0", "id": 43, "method": "no/such/method", "params": {}}),
    );
    assert_eq!(resp["error"]["code"], -32601, "{resp}");

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_stray_response_before_initialize_does_not_end_the_session() {
    // Nothing can be outstanding before the handshake, so a response-shaped
    // line there matches no request. Forwarding it makes RMCP read the session
    // as a failed handshake and end it, taking the real initialize with it.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 99, "result": {}})
    )
    .unwrap();
    stdin.flush().unwrap();

    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(
        init["result"].is_object(),
        "the handshake must still be answered: {init}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_sampling_reply_reaches_the_server() {
    // Tier 3 asks the client to disambiguate and waits for the answer. The
    // reply is a response-shaped message, which the framing layer must hand to
    // the SDK rather than discard: the SDK owns the ids it has outstanding and
    // is the only thing that can match a reply to its request.
    //
    // The discriminator is time, with a wide margin. A reply that never arrives
    // costs a full DEFAULT_SAMPLING_TIMEOUT (5s) per call and yields no answer,
    // while a reply that lands ends the wait immediately, so the whole exchange
    // is milliseconds of scanning either way. Anything under the timeout means
    // the answer was received.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    start_sampling_call(&mut stdin, &mut stdout);

    let started = std::time::Instant::now();
    let (answered, result) = answer_sampling_until(&mut stdin, &mut stdout, 2);
    let elapsed = started.elapsed();

    assert!(answered > 0, "the server never asked the client to sample");
    assert!(
        result["result"].is_object(),
        "the call should succeed: {result}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "{answered} sampling repl(ies) sent but the call took {elapsed:?}, \
         so the server waited out its timeout instead of reading them"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_a_judgment_survives_a_process_that_is_killed() {
    // The judgment cache used to be written only by the exit flush and by Drop,
    // and a stateless client runs neither: it opens a process per call and ends
    // it with a signal. Every judgment a session paid the client to make was
    // thrown away on the teardown path the clients actually use, so the next
    // call asked the same question again. The write now happens at the end of
    // the call that earned it, which is the last moment the process is
    // guaranteed to still be alive.
    let (tmp, mut child, mut stdin, mut stdout) = spawn_server();
    start_sampling_call(&mut stdin, &mut stdout);
    let (answered, _) = answer_sampling_until(&mut stdin, &mut stdout, 2);
    assert!(answered > 0, "the server never asked the client to sample");

    // No exit, no end of input: the process is taken out from under itself,
    // which is what a signal does to Drop and to the exit flush both.
    child.kill().expect("kill the server");
    let _ = child.wait();

    // spawn_server sets XDG_CONFIG_HOME, and rules::store::config_dir honors it
    // on every platform, so the cache has exactly one place to be.
    let cache = tmp.path().join(".config/zhtw-mcp/judgment_cache.json");
    assert!(
        cache.exists(),
        "the call answered {answered} sampling question(s), so its judgments \
         should already be at {}",
        cache.display()
    );
}

#[test]
fn e2e_modern_logging_opt_in_is_delivered_not_wedged() {
    // Log delivery on this path used to go through the RMCP peer, which on a
    // connection that never saw an initialize accepts the notification and
    // never resolves the future that says it reached the wire. The request
    // parked forever: no reply, no exit, both threads idle. It needs an actual
    // log to deliver, which is why the server runs at info here; at the default
    // filter there is nothing queued and nothing to park on.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server_with(&["--verbose"]);

    // The regression this guards against is a park, not a wrong answer, and a
    // blocking read against a parked server hangs the suite instead of failing
    // it. Killing the child turns that back into end of input, which the read
    // below reports as a failure like any other.
    //
    // Disarmed by dropping the sender once the reply is in. Left to fire
    // unconditionally, the kill outlives child.wait() and lands on whatever
    // process inherited the pid by then, which in this binary is a server
    // another test is still talking to.
    let pid = child.id();
    let (armed, disarmed) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if disarmed.recv_timeout(std::time::Duration::from_secs(30))
            == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
    });

    let (notifications, call) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": { "logging": {} }
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    drop(armed);
    assert!(
        call["result"].is_object(),
        "a self-declaring call that opted into logging must still be answered: {call}"
    );
    assert!(
        notifications
            .iter()
            .any(|n| n["method"] == "notifications/message"),
        "the opt-in rode in `_meta`, so the lines this call logged should have \
         reached the client: {notifications:?}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_discovery_answers_a_revision_it_does_not_serve() {
    // The one question a client on an unknown revision can still ask. Refusing
    // it at the gate answered "server not initialized", which is untrue and
    // leaves the client with nothing to fall back to: the version list is the
    // entire point of the request, and the gate does not have it.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let refused = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2027-05-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
    );
    assert_eq!(
        refused["error"]["code"], -32022,
        "an unknown revision is an unsupported version, not a missing handshake: {refused}"
    );
    let supported = refused["error"]["data"]["supported"]
        .as_array()
        .unwrap_or_else(|| panic!("the refusal must name what is on offer: {refused}"));
    assert!(
        supported.iter().any(|v| v == "2026-07-28"),
        "the client needs the list to fall back onto: {refused}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_a_declared_client_does_not_outlive_its_request() {
    // The identity rides in _meta on this revision, which makes it scoped to
    // the request that carried it, exactly like the logging opt-in beside it.
    // Recorded on the connection instead, one call naming an agent client moved
    // the default output mode for every later call that named nobody, and the
    // answer silently lost the text and trace fields it was supposed to carry.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();

    let declared = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "1" },
                    "io.modelcontextprotocol/clientCapabilities": {}
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    let body: Value = serde_json::from_str(
        declared["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected tool output: {declared}")),
    )
    .expect("tool output is JSON");
    assert!(
        body.get("text").is_none(),
        "the request named claude-code, so its own answer is compact: {body}"
    );

    // The same must hold through server/discover, which is the other request
    // whose _meta can name a client. A probe is not consent for a later request
    // that named nobody to be answered as whoever probed.
    let discover = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "1" },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
    );
    assert!(discover["result"].is_object(), "discover: {discover}");

    let anonymous = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    let body: Value = serde_json::from_str(
        anonymous["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected tool output: {anonymous}")),
    )
    .expect("tool output is JSON");
    assert!(
        body.get("text").is_some(),
        "this request named nobody, so the previous one's identity must not \
         still be choosing its output mode: {body}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_modern_logging_opt_in_does_not_leak() {
    // The logging key in client capabilities is this server's own extension,
    // and it was read only off initialize params. A 2026-07-28 client never
    // sends initialize, so the opt-in had nowhere to arrive and the whole
    // channel was unreachable on the path those clients take. It is read from
    // the same _meta that carries everything else that revision declares.
    //
    // At the default filter this server logs nothing above warn, so neither
    // request produced a line and the second could not have received one
    // whatever the code did. Running at info is what makes a leak observable,
    // which is why the first call now asserts it saw lines at all.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server_with(&["--verbose"]);

    let (notifications, first) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": { "logging": {} }
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    assert!(first["result"].is_object(), "tools/call: {first}");
    assert!(
        notifications
            .iter()
            .any(|n| n["method"] == "notifications/message"),
        "the opt-in has to deliver, or the leak below is unobservable: \
         {notifications:?}"
    );

    // Read strictly: one line, and it has to be the reply. Anything arriving
    // ahead of it is a log line this request never asked for.
    let next = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                },
                "name": "zhtw",
                "arguments": { "text": "這個軟件的質量" }
            }
        }),
    );
    assert!(
        next["result"].is_object(),
        "logging leaked into the next request: {next}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_resource_templates_list_is_empty_not_missing() {
    // resources/templates/list is a standard request under the resources
    // capability this server advertises, and one of the ten 2026-07-28 defines.
    // Having no templates is not the same as not implementing the method, so
    // the answer is an empty list rather than METHOD_NOT_FOUND.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(init.get("result").is_some(), "initialize: {init}");

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "resources/templates/list"}),
    );
    assert_eq!(
        resp["result"]["resourceTemplates"].as_array().map(Vec::len),
        Some(0),
        "{resp}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_closing_stdin_still_answers_a_request_in_flight() {
    // A batch caller writes its requests, closes the write half, and waits for
    // the answers. RMCP ends its service loop as soon as the transport reports
    // end of input, so a handler still running has to be waited for or its
    // response is lost. The text is repetitive on purpose: that shape is slow
    // enough to still be scanning when stdin closes.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();

    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert!(init.get("result").is_some(), "initialize: {init}");

    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "zhtw",
                "arguments": { "text": "這個軟件的性能優化".repeat(2000), "output": "summary" }
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    // The whole point: no more input is coming.
    drop(stdin);

    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .expect("the response outlives end of input");
    let resp: Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("expected a response, got {line:?}: {e}"));
    assert_eq!(resp["id"], 2, "{resp}");
    assert!(resp.get("result").is_some(), "{resp}");

    let _ = child.wait();
}

#[test]
fn e2e_refused_handshake_does_not_open_the_gate() {
    // The gate opens when a handshake succeeds, not when one is attempted. A
    // version this server does not serve is refused, and the request behind it
    // must not be served as though the session were established.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "1900-01-01",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert_eq!(resp["error"]["code"], -32022, "{resp}");

    // The refused handshake ends the session, and ends it cleanly: the client
    // got a definite protocol answer, which is not a server failure.
    drop(stdin);
    let status = child.wait().expect("server exits");
    assert!(
        status.success(),
        "refusing a version is not a crash: {status}"
    );
}

#[test]
fn e2e_server_discover_before_handshake_lists_versions() {
    // Discovery is pre-handshake by definition: a client asks what the server
    // speaks before committing to a revision.
    let (_tmp, mut child, mut stdin, mut stdout) = spawn_server();
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "server/discover",
            "id": 0,
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
    );

    let versions = resp["result"]["supportedVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("discover lists supported versions: {resp}"));
    assert!(versions.contains(&json!("2026-07-28")), "{resp}");
    assert!(versions.contains(&json!("2024-11-05")), "{resp}");
    assert!(
        resp["result"]["capabilities"]["tools"].is_object(),
        "{resp}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_mcp_logging_capability_receives_message_notifications() {
    let bin = binary_path();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut child = Command::new(&bin)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
        .env("XDG_CACHE_HOME", tmp.path().join(".cache"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "logging": {} },
                "clientInfo": { "name": "test", "version": "1.0" }
            }
        }),
    );
    assert!(
        init.get("result").is_some(),
        "initialize should succeed: {init}"
    );

    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    stdin.flush().unwrap();

    let (notifications, resp) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "zhtw",
                "arguments": { "text": "這個軟件很好用", "output": "summary" }
            }
        }),
    );
    assert!(
        resp.get("result").is_some(),
        "tools/call should succeed: {resp}"
    );
    assert!(
        notifications.iter().any(|n| {
            n["method"] == "notifications/message"
                && n["params"]["logger"] == "zhtw-mcp"
                && n["params"]["level"] == "info"
        }),
        "expected MCP log notification before response, got {notifications:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn e2e_mcp_logging_parse_error_is_not_stale() {
    let bin = binary_path();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut child = Command::new(&bin)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
        .env("XDG_CACHE_HOME", tmp.path().join(".cache"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let init = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "logging": {} },
                "clientInfo": { "name": "test", "version": "1.0" }
            }
        }),
    );
    assert!(
        init.get("result").is_some(),
        "initialize should succeed: {init}"
    );

    writeln!(stdin, "{{not-json").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let notification: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(notification["method"], "notifications/message");
    assert!(
        notification["params"]["data"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("JSON parse"),
        "parse warning should be emitted before parse response: {notification}"
    );

    line.clear();
    stdout.read_line(&mut line).unwrap();
    let parse_resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parse_resp["error"]["code"], -32700, "{parse_resp}");

    let (notifications, resp) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    );
    assert!(
        resp.get("result").is_some(),
        "tools/list should succeed: {resp}"
    );
    assert!(
        notifications.iter().all(|n| {
            !n["params"]["data"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("JSON parse")
        }),
        "parse warning leaked into later request: {notifications:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn e2e_include_telemetry_returns_metrics() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let _ = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "include_telemetry": true
                }
            }
        }),
    );
    assert_eq!(resp["id"], 2);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let telemetry = &output["telemetry"];
    assert!(
        telemetry.is_object(),
        "telemetry should be present when requested"
    );
    assert_eq!(telemetry["raw"]["input_chars"].as_u64(), Some(7));
    assert!(telemetry["raw"]["rule_hits"].as_u64().unwrap() >= 1);
    assert!(telemetry["cache_hit_count"].is_u64());
    assert!(telemetry["cache_miss_count"].is_u64());

    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );
    let _ = child.wait().unwrap();
}

#[test]
fn e2e_include_telemetry_summary_output_returns_metrics() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let _ = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "output": "summary",
                    "include_telemetry": true
                }
            }
        }),
    );
    assert_eq!(resp["id"], 2);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    assert!(
        output["issues"].is_null(),
        "summary output should omit issue list"
    );
    assert!(output["telemetry"].is_object());

    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );
    let _ = child.wait().unwrap();
}

#[test]
fn e2e_include_telemetry_rejected_for_tabular_output() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let _ = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "output": "tabular",
                    "include_telemetry": true
                }
            }
        }),
    );
    assert_eq!(resp["id"], 2);
    let content_text = resp["error"]["message"].as_str().unwrap();
    assert!(content_text.contains("include_telemetry"));

    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );
    let _ = child.wait().unwrap();
}

#[test]
fn e2e_detect_style_forces_full_three_axis_scorecard() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();
    let mut text = String::new();
    for i in 0..100 {
        if i % 20 == 0 {
            text.push_str("更重要的是，我們需要重新評估這個方案。");
        } else {
            text.push_str("這是正常的技術內容段落。");
        }
    }
    for _ in 0..8 {
        text.push_str("這是 20 世紀最重要的發現之一。當我抵達公司的時候，他已經在開會了。");
    }

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 50,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": text,
                    "detect_style": true,
                    "detect_ai": false,
                    "detect_translationese": false
                }
            }
        }),
    );
    assert_eq!(resp["id"], 50);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let scores = &output["style_scorecard"]["style_scores"];
    assert!(
        scores.get("ai").is_some(),
        "detect_style should force the AI axis even when detect_ai=false: {content_text}"
    );
    assert!(
        scores.get("translationese").is_some(),
        "detect_style should force the translationese axis even when detect_translationese=false: {content_text}"
    );
    assert!(
        scores.get("regional_density").is_some(),
        "detect_style should always include the regional_density axis: {content_text}"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

/// Spawn an initialized MCP child for malformed protocol tests.
fn spawn_initialized_child() -> (
    impl Write,
    impl BufRead,
    std::process::Child,
    tempfile::TempDir,
) {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let _resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "malformed-test", "version": "0.1" }
            }
        }),
    );
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    (stdin, stdout, child, tmp_dir)
}

// -- 25.8 Malformed protocol E2E tests --

#[test]
fn e2e_missing_jsonrpc_field() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "method": "tools/list",
            "id": 100
        }),
    );
    assert!(
        resp["error"].is_object(),
        "missing jsonrpc should return error: {resp}"
    );
    let code = resp["error"]["code"].as_i64().unwrap();
    assert!(
        code == -32600,
        "expected INVALID_REQUEST (-32600), got {code}"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_wrong_jsonrpc_version() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "1.0",
            "method": "tools/list",
            "id": 101
        }),
    );
    assert!(resp["error"].is_object());
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32600,
        "wrong version should be INVALID_REQUEST"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_id_as_array_rejected() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let raw = r#"{"jsonrpc":"2.0","method":"tools/list","id":[1,2],"params":{}}"#;
    writeln!(stdin, "{raw}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert!(
        resp["error"].is_object(),
        "array id should be rejected: {resp}"
    );
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32600);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_id_as_object_rejected() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let raw = r#"{"jsonrpc":"2.0","method":"tools/list","id":{"a":1},"params":{}}"#;
    writeln!(stdin, "{raw}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert!(resp["error"].is_object(), "object id should be rejected");
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32600);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_params_as_array_handled() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let raw = r#"{"jsonrpc":"2.0","method":"tools/list","id":102,"params":[1,2,3]}"#;
    writeln!(stdin, "{raw}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["id"], 102);
    assert!(
        resp.get("result").is_some() || resp.get("error").is_some(),
        "server should handle array params without crashing"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_empty_method_returns_not_found() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "",
            "id": 103
        }),
    );
    assert!(resp["error"].is_object(), "empty method should error");
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32601,
        "empty method should be METHOD_NOT_FOUND"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_method_trailing_whitespace() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/list ",
            "id": 104,
            "params": {}
        }),
    );
    assert!(
        resp["error"].is_object(),
        "trailing whitespace method should error"
    );
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32601,
        "mangled method should be METHOD_NOT_FOUND"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_not_json_returns_parse_error() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    writeln!(stdin, "this is not json at all").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32700,
        "non-JSON should return PARSE_ERROR"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_response_shaped_with_id_discarded() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Response-shaped message WITH id: silently discarded per JSON-RPC 2.0
    // ("The Server MUST NOT reply to a Response").
    let response_msg = r#"{"jsonrpc":"2.0","id":999,"result":"stale"}"#;
    writeln!(stdin, "{response_msg}").unwrap();
    stdin.flush().unwrap();

    // No error response expected: verify the server is still alive by sending a
    // real request and getting a valid response.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 105,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 105);
    assert!(resp["result"].is_object(), "server should still be alive");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_response_shaped_without_id_discarded() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Response-shaped message WITHOUT id: silently discarded (stale sampling).
    let response_msg = r#"{"jsonrpc":"2.0","result":"stale"}"#;
    writeln!(stdin, "{response_msg}").unwrap();
    stdin.flush().unwrap();

    // No response for the stale message. Send a real request to verify alive.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 106,
            "params": {}
        }),
    );
    assert_eq!(resp["id"], 106);
    assert!(resp["result"].is_object(), "server should still be alive");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_request_with_id_without_method_rejected() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Request-shaped message WITH id but WITHOUT method: should be rejected
    // with INVALID_REQUEST.
    let request_msg = r#"{"jsonrpc":"2.0","id":999}"#;
    writeln!(stdin, "{request_msg}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["id"], 999);
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32600);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn e2e_notification_with_id_no_response() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Step 1: initialize
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert_eq!(resp["id"], 1);
    assert!(
        resp.get("error").is_none(),
        "initialize should succeed: {resp}"
    );
    assert!(resp["result"].is_object());

    // Step 2: notifications/initialized with an id field (invalid).
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "id": 200
        }),
    );
    assert_eq!(resp["id"], 200);
    let err = resp.get("error").expect("expected error response");
    assert_eq!(err["code"].as_i64().unwrap(), -32600); // INVALID_REQUEST

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

/// AI agent clients (e.g. claude-code) should auto-default to compact output
/// without explicitly passing `"output": "compact"`.
#[test]
fn e2e_auto_compact_for_ai_clients() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize with clientInfo.name = "claude-code" (AI agent).
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "1.0" }
            }
        }),
    );
    assert_eq!(resp["id"], 1);

    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    // Call zhtw WITHOUT explicit "output" field: should auto-compact.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "視頻和視頻都是視頻"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 2);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();

    // Compact signature: no "text" field (no fixes applied), no "trace" field.
    assert!(
        output.get("text").is_none(),
        "auto-compact for AI client should omit text: {output}"
    );
    assert!(
        output.get("trace").is_none(),
        "auto-compact for AI client should omit trace: {output}"
    );
    // Issues should be deduplicated (compact grouping).
    let issues = output["issues"].as_array().unwrap();
    let video_groups: Vec<_> = issues.iter().filter(|i| i["found"] == "視頻").collect();
    assert_eq!(
        video_groups.len(),
        1,
        "auto-compact should deduplicate issues"
    );
    assert_eq!(
        video_groups[0]["count"].as_u64().unwrap(),
        3,
        "should show count=3 for 3 occurrences"
    );

    // Explicit "output": "full" should override auto-compact.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 3,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件",
                    "output": "full"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 3);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    // Full mode: text and trace fields present.
    assert!(
        output.get("text").is_some(),
        "explicit full should include text"
    );
    assert!(
        output.get("trace").is_some(),
        "explicit full should include trace"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}

/// Verify explain mode includes explanation annotations and deterministic
/// results.
#[test]
fn e2e_explain_mode_and_determinism() {
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let overrides_path = tmp_dir.path().join("overrides.json");
    let suppressions_path = tmp_dir.path().join("suppressions.json");

    let mut child = Command::new(&bin)
        .args([
            "--overrides",
            overrides_path.to_str().unwrap(),
            "--suppressions",
            suppressions_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-explain", "version": "0.1" }
            }
        }),
    );
    assert_eq!(resp["id"], 1);

    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    // Lint with explain mode.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "explain": true,
                    "output": "full"
                }
            }
        }),
    );
    assert_eq!(resp["id"], 2);
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();

    // Issues should be present with explain-specific annotations.
    let issues = output["issues"].as_array().unwrap();
    assert!(!issues.is_empty());

    // Verify explain mode actually produces the explanation annotation
    // (distinct from the context field which exists regardless of explain
    // mode).
    let has_explanation = issues.iter().any(|i| i.get("explanation").is_some());
    assert!(
        has_explanation,
        "explain mode should produce 'explanation' field on at least one issue"
    );

    // Lint same text twice: results should be identical (deterministic).
    let resp2 = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 3,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "這個軟件很好用",
                    "explain": true,
                    "output": "full"
                }
            }
        }),
    );
    assert_eq!(resp2["id"], 3);
    let content_text2 = resp2["result"]["content"][0]["text"].as_str().unwrap();
    let output2: Value = serde_json::from_str(content_text2).unwrap();

    let issues2 = output2["issues"].as_array().unwrap();
    assert_eq!(issues, issues2, "same text should produce identical issues");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}

// -- Reject unknown parameters in tools/call --

#[test]
fn e2e_reject_unknown_params() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Send tools/call with a known typo (max_error instead of max_errors) and
    // an entirely unknown field.
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 900,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "hi",
                    "unknownField": 1,
                    "max_error": 5
                }
            }
        }),
    );

    // Must be INVALID_PARAMS (-32602).
    let err = resp.get("error").expect("expected error response");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);

    // data.unexpected must list both unknown keys.
    let data = err.get("data").expect("expected structured data field");
    let unexpected = data["unexpected"]
        .as_array()
        .expect("unexpected should be an array");
    let keys: Vec<&str> = unexpected.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        keys.contains(&"unknownField"),
        "missing unknownField in {keys:?}"
    );
    assert!(keys.contains(&"max_error"), "missing max_error in {keys:?}");

    // Clean up.
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_all_known_params_accepted() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Send tools/call with only known parameters: should succeed (no error).
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 901,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "測試文字",
                    "max_errors": 0
                }
            }
        }),
    );

    // Should be a successful result, not an error.
    assert!(
        resp.get("error").is_none(),
        "expected success but got error: {resp}"
    );
    assert!(resp.get("result").is_some(), "expected result field");

    drop(stdin);
    let _ = child.wait();
}

// -- Structured error data for invalid parameter values --

#[test]
fn e2e_invalid_profile_structured_error_data() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 910,
            "params": {
                "name": "zhtw",
                "arguments": {
                    "text": "測試",
                    "profile": "nonexistent"
                }
            }
        }),
    );

    // Must be INVALID_PARAMS (-32602) at JSON-RPC level, not a tool-level
    // error.
    let err = resp.get("error").expect("expected JSON-RPC error");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);

    // Structured data must identify the field, rejected value, and accepted
    // values.
    let data = err.get("data").expect("expected structured data field");
    assert_eq!(data["field"], "profile");
    assert_eq!(data["value"], "nonexistent");
    let accepted = data["accepted"]
        .as_array()
        .expect("accepted should be an array");
    let accepted_strs: Vec<&str> = accepted.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        accepted_strs.contains(&"base"),
        "accepted should include 'base': {accepted_strs:?}"
    );
    assert!(
        accepted_strs.contains(&"strict"),
        "accepted should include 'strict': {accepted_strs:?}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn e2e_server_reads_store_paths_from_config() {
    // The server takes all three store paths from .zhtw-mcp.toml when the flags
    // are absent, so a project can point it at its own stores without every MCP
    // client passing flags. Wiring only some of them would be worse than none:
    // the server would answer from the project's overrides while recording into
    // a different translation memory than lint reads.
    //
    // Two observable proofs, one per store: a suppressed term and a TM-rejected
    // term both come back as Info instead of Warning.
    let bin = binary_path();
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let suppressions_path = tmp_dir.path().join("suppressions.json");
    std::fs::write(
        &suppressions_path,
        format!(
            r#"{{"schema_version":{},"terms":["軟件"]}}"#,
            zhtw_mcp::rules::store::SCHEMA_VERSION
        ),
    )
    .unwrap();

    // A TM entry whose user_chose equals found is a rejection: the user kept
    // the flagged term, so it must stop being a warning.
    let tm_path = tmp_dir.path().join("project-tm.json");
    std::fs::write(
        &tm_path,
        format!(
            r#"{{"schema_version":{},"entries":[{{"found":"內存","scanner_suggested":"記憶體","user_chose":"內存","timestamp":"2026-01-01T00:00:00Z"}}]}}"#,
            zhtw_mcp::rules::store::TM_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let cfg_path = tmp_dir.path().join(".zhtw-mcp.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "overrides = {:?}\nsuppressions = {:?}\ntranslation_memory = {:?}\n",
            tmp_dir.path().join("overrides.json").to_str().unwrap(),
            suppressions_path.to_str().unwrap(),
            tm_path.to_str().unwrap()
        ),
    )
    .unwrap();

    let mut child = Command::new(&bin)
        .args(["--config", cfg_path.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn zhtw-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let _ = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    send_notification(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {
                "name": "zhtw",
                "arguments": { "text": "這個軟件很好用，這個內存很大" }
            }
        }),
    );
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(content_text).unwrap();
    let severity_of = |term: &str| -> String {
        output["issues"]
            .as_array()
            .and_then(|a| a.iter().find(|i| i["found"] == term))
            .unwrap_or_else(|| panic!("{term} should still be reported: {output}"))["severity"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        severity_of("軟件"),
        "info",
        "suppressions path from config should downgrade the term: {output}"
    );
    assert_eq!(
        severity_of("內存"),
        "info",
        "translation_memory path from config should downgrade the term: {output}"
    );

    send_notification(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "exit" }));
    let _ = child.wait().unwrap();
}

#[test]
fn e2e_notifications_cancelled_with_id_rejected() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "id": 300
        }),
    );
    assert_eq!(resp["id"], 300);
    let err = resp
        .get("error")
        .expect("expected error for notifications/cancelled with id");
    assert_eq!(err["code"].as_i64().unwrap(), -32600);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "child exited with {status}");
}

// "verify" is the one tool argument that causes egress: it ships sentence-sized
// excerpts of the caller's text to Google Translate. Under MCP the decision to
// pass it belongs to a model, not to the person whose document it is, so the
// operator's switch has to outrank the argument and say so in the error. Gated
// like convert_accepts_verify_flag in tests/cli-convert.rs: without the feature
// the tool has no "verify" argument to refuse, so the assertion below would
// fail a legitimate build rather than catch a bug.
#[cfg(feature = "translate")]
#[test]
fn e2e_no_network_env_refuses_the_verify_argument() {
    let bin = binary_path();
    let tmp = tempfile::tempdir().expect("create temp dir for the server session");
    let mut child = Command::new(&bin)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
        .env("XDG_CACHE_HOME", tmp.path().join(".cache"))
        .env("ZHTW_NO_NETWORK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zhtw-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    handshake(&mut stdin, &mut stdout);

    let text = "這個軟件需要優化。";
    let (_, refused) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "zhtw", "arguments": { "text": text, "verify": true } }
        }),
    );
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ZHTW_NO_NETWORK"),
        "refusal must name the variable so an operator can find it: {refused}"
    );
    assert_eq!(
        refused["error"]["data"]["reason"], "network_disabled",
        "refusal needs a machine-readable reason, not only prose: {refused}"
    );

    // The switch governs egress, not linting. Refusing the whole tool would
    // make the server useless offline, which is where it is most wanted.
    let (_, ordinary) = send_recv_skip_notifications(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "zhtw", "arguments": { "text": text } }
        }),
    );
    assert!(
        ordinary["result"].is_object(),
        "a call without verify must still succeed: {ordinary}"
    );
    assert!(
        ordinary["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("軟體"),
        "and must still report findings: {ordinary}"
    );

    // Close stdin so the server exits on its own, then reap it: a killed but
    // unwaited child stays a zombie for the rest of the run.
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    drop(tmp);
}
