// End-to-end MCP protocol test.
//
// Spawns the zhtw-mcp binary, sends JSON-RPC messages over stdin, and
// verifies the stdout responses match expected structure and content.

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

/// Build the binary path. In cargo test, the binary is in target/debug/.
fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    // test binary is in target/debug/deps/e2e_mcp-<hash>
    // the main binary is in target/debug/zhtw-mcp
    path.pop(); // remove test binary name
    if path.ends_with("deps") {
        path.pop(); // remove deps/
    }
    path.push("zhtw-mcp");
    path
}

#[test]
fn e2e_initialize_and_tools_list() {
    let bin = binary_path();
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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

    // 0. Pre-init: tools/list before initialize should be rejected
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

    // 3. Tools list — 1 tool: zhtw
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

    // Verify tool annotations use the MCP-spec `*Hint` wire names; any other
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

    // 4. zhtw lint-only (fix_mode absent = none) — detect 軟件
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

    // 5. zhtw gate-pass — clean text + max_errors: 0 + fix_mode: safe
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

    // 6. zhtw gate-fix — dirty text + fix_mode: safe, verify fixes
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

    // 7. zhtw with ignore_terms — 軟件 downgraded to info
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

    // 9. resources/read — style guide
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
    // "軟件" in fenced code block should be excluded; only prose occurrences flagged
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

    // -- E2E: gate rejection (accepted: false, max_errors exceeded) --
    // "內地" is political_coloring → Severity::Error, which the gate counts.

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
    // Gate rejection: isError=true on the result, output JSON has accepted=false
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

    // -- E2E: fix_mode: "lexical_contextual" --
    // Uses 代碼 (clue-gated: needs 編譯/函式/函數 nearby) + 軟件 (non-clue).
    // lexical_safe would fix 軟件 but skip 代碼; lexical_contextual fixes both.

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

    // 256 KiB + 1 byte should be rejected with INVALID_PARAMS and structured data.
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

    // -- E2E: output: "compact" — deduplicated issues, no text/trace fields --

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

    // -- E2E: output: "compact" with fix_mode — text included when fixes applied --

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
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
        scores.get("consistency").is_some(),
        "detect_style should always include the consistency axis: {content_text}"
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
fn e2e_response_shaped_with_id_rejected() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Reject response-shaped messages with an ID but no method with INVALID_REQUEST (-32600)
    let response_msg = r#"{"jsonrpc":"2.0","id":999,"result":"stale"}"#;
    writeln!(stdin, "{response_msg}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32600);

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

    // Call zhtw WITHOUT explicit "output" field — should auto-compact.
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

/// Verify explain mode includes explanation annotations and deterministic results.
#[test]
fn e2e_explain_mode_and_determinism() {
    let bin = binary_path();
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
    // (distinct from the `context` field which exists regardless of explain mode).
    let has_explanation = issues.iter().any(|i| i.get("explanation").is_some());
    assert!(
        has_explanation,
        "explain mode should produce 'explanation' field on at least one issue"
    );

    // Lint same text twice — results should be identical (deterministic).
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

#[test]
fn e2e_shutdown_returns_empty_and_clean_exit() {
    let bin = binary_path();
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    );
    assert_eq!(resp["id"], 1);
    assert!(resp["result"].is_object());

    // Shutdown: must respond with empty result {}
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "shutdown",
            "id": 2
        }),
    );
    assert_eq!(resp["id"], 2);
    assert_eq!(resp["result"], json!({}));
    assert!(resp.get("error").is_none());

    // After shutdown, the server exits the serve loop cleanly.
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn e2e_exit_terminates_process() {
    let bin = binary_path();
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
    send_recv(
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

    // Shutdown first (proper lifecycle)
    let resp = send_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "method": "shutdown",
            "id": 2
        }),
    );
    assert_eq!(resp["result"], json!({}));

    // Exit notification — process should terminate with code 0
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );

    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "exit after shutdown should exit with code 0"
    );
}

#[test]
fn e2e_exit_without_shutdown_exits_nonzero() {
    let bin = binary_path();
    if !bin.exists() {
        panic!("binary not found at {:?}; run `cargo build` first", bin);
    }

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
    send_recv(
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

    // Exit without shutdown — process should terminate with code 1
    send_notification(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );

    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "exit without prior shutdown should exit with non-zero code"
    );
}

// -- Reject unknown parameters in tools/call --

#[test]
fn e2e_reject_unknown_params() {
    let (mut stdin, mut stdout, mut child, _tmp) = spawn_initialized_child();

    // Send tools/call with a known typo (max_error instead of max_errors)
    // and an entirely unknown field.
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

    // Send tools/call with only known parameters — should succeed (no error).
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

// -- Structured error data for invalid parameter values (25.4) --

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

    // Must be INVALID_PARAMS (-32602) at JSON-RPC level, not a tool-level error.
    let err = resp.get("error").expect("expected JSON-RPC error");
    assert_eq!(err["code"].as_i64().unwrap(), -32602);

    // Structured data must identify the field, rejected value, and accepted values.
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
