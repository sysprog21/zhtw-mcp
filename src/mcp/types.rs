// MCP JSON-RPC 2.0 types for stdio transport.
//
// Covers the MCP protocol: initialize (with capability negotiation),
// tools/list, tools/call, resources/list, resources/read, prompts/list,
// prompts/get.

use std::string::FromUtf8Error;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// JSON-RPC base types

/// A JSON-RPC 2.0 request ID (integer, string, or null).
///
/// JSON-RPC 2.0 allows `"id": null` in requests. A request with an explicit
/// null id is NOT a notification (notifications omit the id field entirely).
/// Error responses for requests with null id must include `"id": null`.
#[derive(Debug, Clone)]
pub enum RequestId {
    Int(i64),
    Str(String),
    /// Explicit `"id": null` in the request. Distinct from absent id
    /// (which indicates a notification).
    Null,
}

impl Serialize for RequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RequestId::Int(i) => serializer.serialize_i64(*i),
            RequestId::Str(s) => serializer.serialize_str(s),
            RequestId::Null => serializer.serialize_unit(),
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        match &v {
            Value::Null => Ok(RequestId::Null),
            Value::Number(n) => n
                .as_i64()
                .map(RequestId::Int)
                .ok_or_else(|| serde::de::Error::custom("id number must be an integer")),
            Value::String(s) => Ok(RequestId::Str(s.clone())),
            _ => Err(serde::de::Error::custom(
                "id must be a string, integer, or null",
            )),
        }
    }
}

/// Incoming JSON-RPC request (method call or notification).
///
/// When `id` is `None`, this is a notification (no response expected).
/// When `id` is `Some(RequestId::Null)`, the client sent `"id": null`
/// and still expects a response.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default, deserialize_with = "deserialize_request_id")]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Deserialize the `id` field so that `"id": null` becomes
/// `Some(RequestId::Null)` rather than `None` (which serde's default
/// `Option<T>` handling would produce).  An absent field still yields
/// `None` via `#[serde(default)]`.
fn deserialize_request_id<'de, D>(deserializer: D) -> Result<Option<RequestId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    RequestId::deserialize(deserializer).map(Some)
}

/// Outgoing JSON-RPC response.
///
/// The `id` field is always serialized per JSON-RPC 2.0: `None` produces
/// `"id": null` (required for error responses when the request id is
/// unknown).
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<RequestId>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }

    pub fn error_with_data(id: Option<RequestId>, code: i64, message: String, data: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: Some(data),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// MCP protocol types

/// Initialize request params.
#[derive(Debug, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: ClientCapabilitiesRaw,
    #[serde(rename = "clientInfo", default)]
    pub client_info: Option<ClientInfo>,
}

/// Raw client capabilities from the initialize request.
/// Each field indicates whether the client supports that MCP feature.
#[derive(Debug, Default, Deserialize)]
pub struct ClientCapabilitiesRaw {
    #[serde(default)]
    pub sampling: Option<Value>,
    #[serde(default)]
    pub roots: Option<Value>,
    #[serde(default)]
    pub logging: Option<Value>,
}

/// Parsed client capabilities stored by the server.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientCapabilities {
    /// Client supports sampling/createMessage (server -> client requests).
    pub sampling: bool,
    /// Client supports roots/list.
    pub roots: bool,
    /// Client supports notifications/message.
    pub logging: bool,
}

impl From<&ClientCapabilitiesRaw> for ClientCapabilities {
    fn from(raw: &ClientCapabilitiesRaw) -> Self {
        Self {
            sampling: raw.sampling.is_some(),
            roots: raw.roots.is_some(),
            logging: raw.logging.is_some(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Initialize response result.
#[derive(Debug, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// Server capabilities declared to the client during initialization.
#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub tools: ToolCapability,
    pub resources: ResourceCapability,
    pub prompts: PromptCapability,
}

#[derive(Debug, Serialize)]
pub struct ToolCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ResourceCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct PromptCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// A tool definition returned by tools/list.
#[derive(Debug, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// MCP tool annotations (hints for clients about tool behavior).
#[derive(Debug, Serialize)]
pub struct ToolAnnotations {
    // MCP wire names are the `*Hint` forms; spec-compliant clients drop any
    // other spelling. Field idents stay short; serde rename carries the wire name.
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(rename = "openWorldHint", skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
}

/// Result of tools/list.
#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDef>,
}

/// Parameters for tools/call.
#[derive(Debug, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// A content block in a tool result.
#[derive(Debug, Serialize)]
pub struct Content {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

/// Result of tools/call.
#[derive(Debug, Serialize)]
pub struct CallToolResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl CallToolResult {
    pub fn text(text: String) -> Self {
        Self {
            content: vec![Content {
                content_type: "text".into(),
                text,
            }],
            is_error: None,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            content: vec![Content {
                content_type: "text".into(),
                text: message,
            }],
            is_error: Some(true),
        }
    }
}

// MCP Resources types

/// A resource definition returned by resources/list.
#[derive(Debug, Serialize)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// Result of resources/list.
#[derive(Debug, Serialize)]
pub struct ResourcesListResult {
    pub resources: Vec<ResourceDef>,
}

/// Parameters for resources/read.
#[derive(Debug, Deserialize)]
pub struct ResourceReadParams {
    pub uri: String,
}

/// A resource content item returned by resources/read.
#[derive(Debug, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub text: String,
}

/// Result of resources/read.
#[derive(Debug, Serialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

// MCP Prompts types

/// A prompt definition returned by prompts/list.
#[derive(Debug, Serialize)]
pub struct PromptDef {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgDef>>,
}

/// An argument definition for a prompt.
#[derive(Debug, Serialize)]
pub struct PromptArgDef {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// Parameters for prompts/get.
#[derive(Debug, Deserialize)]
pub struct PromptGetParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// A message in a prompt result.
#[derive(Debug, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: PromptContent,
}

/// Content of a prompt message.
#[derive(Debug, Serialize)]
pub struct PromptContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

/// Result of prompts/get.
#[derive(Debug, Serialize)]
pub struct PromptGetResult {
    pub description: String,
    pub messages: Vec<PromptMessage>,
}

// Transport error types

/// Structured transport error for distinguishing failure modes in the
/// dispatch loop. Maps to specific JSON-RPC error codes:
///   - Closed → graceful exit (break loop)
///   - Parse → -32700 (PARSE_ERROR)
///   - InvalidUtf8 → -32700 (PARSE_ERROR)
///   - InvalidRequest → -32600 (INVALID_REQUEST)
///   - Io → -32000 (server error) with log
#[derive(Debug)]
pub enum TransportError {
    /// Underlying I/O failure (read/write error).
    Io(std::io::Error),
    /// Transport closed (EOF or channel dropped).
    Closed,
    /// Input is not valid JSON (malformed syntax).
    Parse(serde_json::Error),
    /// Input contains invalid UTF-8 sequences.
    InvalidUtf8(FromUtf8Error),
    /// Valid JSON but not a valid JSON-RPC request (missing method, wrong
    /// version, response-shaped message with id, etc.).  Carries the
    /// extracted request id (if any) for error response correlation.
    InvalidRequest(Option<RequestId>, String),
    /// Response-shaped message (has result/error, no method).
    /// Silently discarded per JSON-RPC 2.0: "The Server MUST NOT reply
    /// to a Response."  Covers both late sampling responses and orphaned
    /// messages, regardless of whether they carry an id.
    StaleResponse,
}

impl TransportError {
    /// Whether this error represents a closed/EOF condition.
    pub fn is_closed(&self) -> bool {
        matches!(self, TransportError::Closed)
    }

    /// JSON-RPC error code for this transport error, if applicable.
    /// Returns None for Closed and StaleResponse (no error response sent).
    pub fn error_code(&self) -> Option<i64> {
        match self {
            TransportError::Io(_) => Some(-32000),
            TransportError::Closed | TransportError::StaleResponse => None,
            TransportError::Parse(_) => Some(PARSE_ERROR),
            TransportError::InvalidUtf8(_) => Some(PARSE_ERROR),
            TransportError::InvalidRequest(..) => Some(INVALID_REQUEST),
        }
    }

    /// Human-readable error message for JSON-RPC error responses.
    pub fn error_message(&self) -> String {
        match self {
            TransportError::Io(e) => format!("transport I/O error: {e}"),
            TransportError::Closed => "transport closed".into(),
            TransportError::StaleResponse => "stale response (discarded)".into(),
            TransportError::Parse(e) => format!("parse error: {e}"),
            TransportError::InvalidUtf8(_) => {
                "request contains malformed UTF-8 character(s)".into()
            }
            TransportError::InvalidRequest(_, msg) => format!("invalid request: {msg}"),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "transport I/O: {e}"),
            TransportError::Closed => write!(f, "transport closed"),
            TransportError::Parse(e) => write!(f, "JSON parse: {e}"),
            TransportError::InvalidUtf8(e) => write!(f, "invalid UTF-8: {e}"),
            TransportError::InvalidRequest(_, msg) => write!(f, "invalid request: {msg}"),
            TransportError::StaleResponse => write!(f, "stale response (discarded)"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(e) => Some(e),
            TransportError::Parse(e) => Some(e),
            TransportError::InvalidUtf8(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<serde_json::Error> for TransportError {
    fn from(e: serde_json::Error) -> Self {
        TransportError::Parse(e)
    }
}

impl From<FromUtf8Error> for TransportError {
    fn from(e: FromUtf8Error) -> Self {
        TransportError::InvalidUtf8(e)
    }
}

impl TransportError {
    /// Build a JSON-RPC error response for this transport error, if one
    /// should be sent.  Returns None for Closed and StaleResponse.
    ///
    /// For InvalidRequest, the carried id (extracted during parsing) is
    /// used so the client can correlate the error with its original request.
    /// The `fallback_id` is used for other error variants (Parse, Io, etc.)
    /// where no id could be extracted.
    pub fn into_response(self, fallback_id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let code = self.error_code()?;
        let id = match &self {
            TransportError::InvalidRequest(carried_id, _) => carried_id.clone(),
            _ => fallback_id,
        };
        let message = self.error_message();
        Some(JsonRpcResponse::error(id, code, message))
    }
}

/// Parse a raw JSON line into a validated JsonRpcRequest.
///
/// Returns TransportError variants that preserve the distinction between
/// malformed JSON (Parse → -32700) and valid-JSON-but-invalid-JSON-RPC
/// (InvalidRequest → -32600).
pub fn parse_jsonrpc_line(line: &str) -> Result<JsonRpcRequest, TransportError> {
    // Step 1: parse as generic JSON.
    let obj: serde_json::Value = serde_json::from_str(line).map_err(TransportError::Parse)?;

    // Step 2: extract id once for error correlation across all branches.
    // id_present is true when the JSON has an "id" key (even if null or
    // an unparseable type like boolean/array).  raw_id is the parsed id
    // when it's a valid string, integer, or null.
    let id_value = obj.get("id");
    let raw_id: Option<RequestId> = id_value.and_then(|v| serde_json::from_value(v.clone()).ok());
    let id_present = id_value.is_some();

    // Step 3: handle messages without a method field.
    if obj.is_object() && obj.get("method").is_none() {
        let is_response = obj.get("result").is_some() || obj.get("error").is_some();
        if is_response && !id_present {
            // Response-shaped (has result/error, no method, no id): silently discard.
            // JSON-RPC 2.0: "The Server MUST NOT reply to a Response."
            // Covers orphaned responses (without id).
            return Err(TransportError::StaleResponse);
        }
        if id_present {
            // Has id, no method, not response-shaped — genuinely invalid.
            return Err(TransportError::InvalidRequest(
                raw_id,
                "message has id but no method".into(),
            ));
        }
        // No id, no method, no result/error — genuinely invalid request
        // (e.g. `{}` or `{"foo":"bar"}`).
        return Err(TransportError::InvalidRequest(
            None,
            "object has no method, result, or error field".into(),
        ));
    }

    // Step 4: convert to typed request.
    let req: JsonRpcRequest = serde_json::from_value(obj)
        .map_err(|e| TransportError::InvalidRequest(raw_id.clone(), e.to_string()))?;

    // Step 5: validate JSON-RPC version.
    if req.jsonrpc != JSONRPC_VERSION {
        return Err(TransportError::InvalidRequest(
            raw_id,
            format!(
                "expected jsonrpc \"{JSONRPC_VERSION}\", got \"{}\"",
                req.jsonrpc
            ),
        ));
    }

    Ok(req)
}

// Protocol constants

pub const JSONRPC_VERSION: &str = "2.0";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// Standard JSON-RPC error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- ToolAnnotations wire format --

    /// Every annotation must serialize under its MCP-spec `*Hint` wire name and
    /// never under a bare spelling. All four fields are set so each rename
    /// attribute is exercised (a skipped `None` field would hide a regression).
    #[test]
    fn tool_annotations_all_fields_use_spec_hint_wire_names() {
        let v = serde_json::to_value(ToolAnnotations {
            destructive: Some(true),
            idempotent: Some(false),
            read_only: Some(true),
            open_world: Some(false),
        })
        .unwrap();
        assert_eq!(v["destructiveHint"], true);
        assert_eq!(v["idempotentHint"], false);
        assert_eq!(v["readOnlyHint"], true);
        assert_eq!(v["openWorldHint"], false);
        for bare in [
            "destructive",
            "idempotent",
            "readOnly",
            "openWorld",
            "open_world",
        ] {
            assert!(v.get(bare).is_none(), "unexpected bare wire name: {bare}");
        }
    }

    // -- RequestId serde round-trips --

    #[test]
    fn request_id_int_roundtrip() {
        let id = RequestId::Int(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RequestId::Int(42)));
    }

    #[test]
    fn request_id_string_roundtrip() {
        let id = RequestId::Str("req-abc".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"req-abc\"");
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        match parsed {
            RequestId::Str(s) => assert_eq!(s, "req-abc"),
            _ => panic!("expected string id"),
        }
    }

    #[test]
    fn request_id_negative_int() {
        let id = RequestId::Int(-1);
        let json = serde_json::to_string(&id).unwrap();
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RequestId::Int(-1)));
    }

    #[test]
    fn request_id_null_roundtrip() {
        let id = RequestId::Null;
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "null");
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RequestId::Null));
    }

    #[test]
    fn request_id_boolean_rejected() {
        let result = serde_json::from_str::<RequestId>("true");
        assert!(result.is_err());
    }

    #[test]
    fn request_id_array_rejected() {
        let result = serde_json::from_str::<RequestId>("[1,2]");
        assert!(result.is_err());
    }

    #[test]
    fn request_null_id_is_not_notification() {
        // "id": null is a request, not a notification.
        let line = r#"{"jsonrpc":"2.0","method":"ping","id":null}"#;
        let req = parse_jsonrpc_line(line).unwrap();
        assert!(req.id.is_some(), "null id must be Some(RequestId::Null)");
        assert!(matches!(req.id, Some(RequestId::Null)));
    }

    #[test]
    fn request_absent_id_is_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req = parse_jsonrpc_line(line).unwrap();
        assert!(req.id.is_none(), "absent id must be None (notification)");
    }

    // -- JsonRpcError serde --

    #[test]
    fn jsonrpc_error_with_data_roundtrip() {
        let err = JsonRpcError {
            code: INVALID_REQUEST,
            message: "bad request".into(),
            data: Some(json!({"field": "profile", "accepted": ["base", "strict"]})),
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["code"], INVALID_REQUEST);
        assert_eq!(parsed["message"], "bad request");
        assert_eq!(parsed["data"]["field"], "profile");
    }

    #[test]
    fn jsonrpc_error_without_data_omits_field() {
        let err = JsonRpcError {
            code: PARSE_ERROR,
            message: "parse error".into(),
            data: None,
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["code"], PARSE_ERROR);
        assert!(parsed.get("data").is_none());
    }

    // -- ClientCapabilities serde --

    #[test]
    fn client_capabilities_empty_object() {
        let raw: ClientCapabilitiesRaw = serde_json::from_str("{}").unwrap();
        let caps = ClientCapabilities::from(&raw);
        assert!(!caps.sampling);
        assert!(!caps.roots);
    }

    #[test]
    fn client_capabilities_with_sampling_only() {
        let raw: ClientCapabilitiesRaw = serde_json::from_str(r#"{"sampling": {}}"#).unwrap();
        let caps = ClientCapabilities::from(&raw);
        assert!(caps.sampling);
        assert!(!caps.roots);
    }

    #[test]
    fn client_capabilities_with_extra_fields_ignored() {
        let raw: ClientCapabilitiesRaw = serde_json::from_str(
            r#"{"sampling": {}, "roots": {"listChanged": true}, "experimental": 42}"#,
        )
        .unwrap();
        let caps = ClientCapabilities::from(&raw);
        assert!(caps.sampling);
        assert!(caps.roots);
    }

    #[test]
    fn client_capabilities_null_sampling_is_none() {
        // JSON null for sampling should be treated as absent
        let raw: ClientCapabilitiesRaw = serde_json::from_str(r#"{"sampling": null}"#).unwrap();
        let caps = ClientCapabilities::from(&raw);
        assert!(!caps.sampling);
    }

    // -- JsonRpcResponse serde --

    #[test]
    fn response_success_omits_error() {
        let resp = JsonRpcResponse::success(Some(RequestId::Int(1)), json!("ok"));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["result"], "ok");
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn response_error_omits_result() {
        let resp = JsonRpcResponse::error(Some(RequestId::Int(1)), -32600, "bad".into());
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("result").is_none());
        assert_eq!(parsed["error"]["code"], -32600);
    }

    #[test]
    fn response_unknown_id_serializes_as_null() {
        // JSON-RPC 2.0: error responses with unknown id must include "id": null
        let resp = JsonRpcResponse::error(None, PARSE_ERROR, "err".into());
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("id").is_some(), "id field must be present");
        assert!(parsed["id"].is_null(), "unknown id must serialize as null");
    }

    // -- parse_jsonrpc_line --

    #[test]
    fn parse_valid_request() {
        let line = r#"{"jsonrpc":"2.0","method":"tools/list","id":1,"params":{}}"#;
        let req = parse_jsonrpc_line(line).unwrap();
        assert_eq!(req.method, "tools/list");
        assert!(matches!(req.id, Some(RequestId::Int(1))));
    }

    #[test]
    fn parse_malformed_json_returns_parse_error() {
        let line = "not json at all";
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::Parse(_)));
        assert_eq!(err.error_code(), Some(PARSE_ERROR));
    }

    #[test]
    fn parse_response_shaped_with_id_without_method_returns_invalid_request() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::InvalidRequest(..)));
        assert_eq!(err.error_code(), Some(INVALID_REQUEST));
        let resp = err.into_response(None).expect("should produce response");
        match &resp.id {
            Some(RequestId::Int(1)) => {}
            other => panic!("expected id=1, got {other:?}"),
        }
    }

    #[test]
    fn parse_response_shaped_without_id_returns_stale() {
        let line = r#"{"jsonrpc":"2.0","result":"stale"}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::StaleResponse));
        assert_eq!(err.error_code(), None);
        assert!(err.into_response(None).is_none());
    }

    #[test]
    fn parse_wrong_jsonrpc_version() {
        let line = r#"{"jsonrpc":"1.0","method":"test","id":1}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::InvalidRequest(..)));
    }

    #[test]
    fn parse_notification_no_id() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req = parse_jsonrpc_line(line).unwrap();
        assert!(req.id.is_none());
        assert_eq!(req.method, "notifications/initialized");
    }

    #[test]
    fn parse_empty_object_returns_invalid_request() {
        let line = r#"{}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::InvalidRequest(..)));
        assert_eq!(err.error_code(), Some(INVALID_REQUEST));
    }

    #[test]
    fn parse_arbitrary_object_without_method_returns_invalid_request() {
        let line = r#"{"foo":"bar","baz":42}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::InvalidRequest(..)));
    }

    #[test]
    fn parse_invalid_request_carries_id() {
        // A message with id but no method and not response-shaped should
        // produce an error response that echoes the id back to the client.
        let line = r#"{"id":99,"jsonrpc":"2.0"}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        assert!(matches!(err, TransportError::InvalidRequest(..)));
        let resp = err.into_response(None).expect("should produce response");
        match &resp.id {
            Some(RequestId::Int(99)) => {}
            other => panic!("expected id=99, got {other:?}"),
        }
    }

    #[test]
    fn parse_wrong_version_carries_id() {
        let line = r#"{"jsonrpc":"1.0","method":"test","id":7}"#;
        let err = parse_jsonrpc_line(line).unwrap_err();
        let resp = err.into_response(None).expect("should produce response");
        match &resp.id {
            Some(RequestId::Int(7)) => {}
            other => panic!("expected id=7, got {other:?}"),
        }
    }

    // -- TransportError --

    #[test]
    fn transport_error_closed_has_no_code() {
        let err = TransportError::Closed;
        assert!(err.is_closed());
        assert_eq!(err.error_code(), None);
        assert!(err.into_response(None).is_none());
    }

    #[test]
    fn transport_error_io_has_server_code() {
        let err = TransportError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe"));
        assert_eq!(err.error_code(), Some(-32000));
    }
}
