// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Protocol constants, typed request parameters and message classification.
//!
//! Port of `src/shared/common_type.h`, `src/shared/http_common.h` and the
//! envelope handling in `src/shared/jsonrpc.cpp`. The JSON-RPC envelope
//! types come from `ap_jsonrpc`.

use ap_jsonrpc::{Message, Notification, Request, RequestId, Response, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::JsonRpcErrorCode;
use crate::types::{
    ClientCapabilities, CompleteReference, CompletionArgument, CompletionContext, Implementation, MetaMap,
    ProgressToken, RequestParamsMeta,
};

/// JSON-RPC version string.
pub const JSONRPC_VERSION: &str = "2.0";
/// Latest protocol version supported by the SDK.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
/// Protocol version assumed when a peer sends none.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-03-26";
/// All supported protocol versions.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2025-03-26", LATEST_PROTOCOL_VERSION];
/// Supported versions rendered for error messages.
pub const SUPPORTED_PROTOCOL_VERSIONS_STRING: &str = "2025-03-26, 2025-06-18";
/// Maximum worker or I/O thread count accepted by the server configuration.
pub const MAX_THREAD_NUM: u32 = 64;

/// True when `version` is one of [`SUPPORTED_PROTOCOL_VERSIONS`].
pub fn is_supported_protocol_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// JSON-RPC method names.
pub mod methods {
    pub const INITIALIZE: &str = "initialize";
    pub const PING: &str = "ping";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    pub const PROMPTS_LIST: &str = "prompts/list";
    pub const PROMPTS_GET: &str = "prompts/get";
    pub const RESOURCES_LIST: &str = "resources/list";
    pub const RESOURCES_READ: &str = "resources/read";
    pub const RESOURCES_SUBSCRIBE: &str = "resources/subscribe";
    pub const RESOURCES_UNSUBSCRIBE: &str = "resources/unsubscribe";
    pub const RESOURCES_TEMPLATES_LIST: &str = "resources/templates/list";
    pub const LOGGING_SET_LEVEL: &str = "logging/setLevel";
    pub const COMPLETION_COMPLETE: &str = "completion/complete";
    pub const ROOTS_LIST: &str = "roots/list";
    pub const SAMPLING_CREATE_MESSAGE: &str = "sampling/createMessage";
    pub const ELICITATION_CREATE: &str = "elicitation/create";
    pub const NOTIFICATION_INITIALIZED: &str = "notifications/initialized";
    pub const NOTIFICATION_CANCELLED: &str = "notifications/cancelled";
    pub const NOTIFICATION_PROGRESS: &str = "notifications/progress";
    pub const NOTIFICATION_MESSAGE: &str = "notifications/message";
    pub const NOTIFICATION_RESOURCES_UPDATED: &str = "notifications/resources/updated";
    pub const NOTIFICATION_TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";
    pub const NOTIFICATION_PROMPTS_LIST_CHANGED: &str = "notifications/prompts/list_changed";
    pub const NOTIFICATION_RESOURCES_LIST_CHANGED: &str = "notifications/resources/list_changed";
    pub const NOTIFICATION_ROOTS_LIST_CHANGED: &str = "notifications/roots/list_changed";
}

/// HTTP header names (lowercase) and common values.
pub mod headers {
    pub const MCP_SESSION_ID: &str = "mcp-session-id";
    pub const MCP_PROTOCOL_VERSION: &str = "mcp-protocol-version";
    pub const CONTENT_TYPE: &str = "content-type";
    pub const ACCEPT: &str = "accept";
    pub const CACHE_CONTROL: &str = "cache-control";
    pub const CONNECTION: &str = "connection";
    pub const HOST: &str = "host";
    pub const CONTENT_LENGTH: &str = "content-length";
    pub const TRANSFER_ENCODING: &str = "transfer-encoding";
    pub const X_ACCEL_BUFFERING: &str = "x-accel-buffering";
    pub const AUTHORIZATION: &str = "authorization";
    pub const LAST_EVENT_ID: &str = "last-event-id";
    pub const ALLOW: &str = "allow";

    pub const CONTENT_TYPE_JSON: &str = "application/json";
    pub const CONTENT_TYPE_SSE: &str = "text/event-stream";
    pub const CONTENT_TYPE_TEXT_PLAIN: &str = "text/plain";
    pub const CACHE_CONTROL_NO_CACHE_NO_TRANSFORM: &str = "no-cache, no-transform";
    pub const CONNECTION_KEEP_ALIVE: &str = "keep-alive";
    pub const CONNECTION_CLOSE: &str = "close";
    pub const TRANSFER_ENCODING_CHUNKED: &str = "chunked";
}

/// HTTP status codes used by the transports.
pub mod status {
    pub const OK: u16 = 200;
    pub const ACCEPTED: u16 = 202;
    pub const NO_CONTENT: u16 = 204;
    pub const BAD_REQUEST: u16 = 400;
    pub const UNAUTHORIZED: u16 = 401;
    pub const FORBIDDEN: u16 = 403;
    pub const NOT_FOUND: u16 = 404;
    pub const METHOD_NOT_ALLOWED: u16 = 405;
    pub const NOT_ACCEPTABLE: u16 = 406;
    pub const CONFLICT: u16 = 409;
    pub const UNSUPPORTED_MEDIA_TYPE: u16 = 415;
    pub const INTERNAL_SERVER_ERROR: u16 = 500;
    pub const SERVICE_UNAVAILABLE: u16 = 503;
}

// ---------------------------------------------------------------------------
// Typed parameters
// ---------------------------------------------------------------------------

/// Parameters of `initialize`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequestParams {
    /// Protocol version requested by the client.
    #[serde(default = "default_version")]
    pub protocol_version: String,
    /// Client capabilities.
    #[serde(default)]
    pub capabilities: ClientCapabilities,
    /// Client identity.
    #[serde(default)]
    pub client_info: Implementation,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RequestParamsMeta>,
}

fn default_version() -> String {
    DEFAULT_PROTOCOL_VERSION.to_string()
}

impl Default for InitializeRequestParams {
    fn default() -> Self {
        Self {
            protocol_version: default_version(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation::default(),
            meta: None,
        }
    }
}

/// Parameters of `tools/call`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CallToolParams {
    /// Tool name.
    pub name: String,
    /// Tool arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RequestParamsMeta>,
}

/// Parameters of `prompts/get`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GetPromptParams {
    /// Prompt name.
    pub name: String,
    /// Prompt arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RequestParamsMeta>,
}

/// Parameters carrying a resource URI (`resources/read`, `subscribe`, `unsubscribe`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UriParams {
    /// Resource URI.
    pub uri: String,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RequestParamsMeta>,
}

/// `Mcp::ReadResourceRequestParams`.
pub type ReadResourceParams = UriParams;
/// `Mcp::SubscribeRequestParams`.
pub type SubscribeParams = UriParams;
/// `Mcp::UnsubscribeRequestParams`.
pub type UnsubscribeParams = UriParams;

/// Parameters of paginated list requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaginatedParams {
    /// Opaque cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RequestParamsMeta>,
}

/// Parameters of `logging/setLevel`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SetLoggingLevelParams {
    /// Level name.
    pub level: String,
}

/// Parameters of `elicitation/create`, selected by `mode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ElicitParams {
    /// Form mode.
    #[serde(rename = "form", rename_all = "camelCase")]
    Form {
        /// Message shown to the user.
        message: String,
        /// JSON schema of the requested values.
        requested_schema: MetaMap,
    },
    /// URL mode.
    #[serde(rename = "url", rename_all = "camelCase")]
    Url {
        /// Message shown to the user.
        message: String,
        /// URL to open.
        url: String,
        /// Elicitation id.
        elicitation_id: String,
    },
}

/// Parameters of `completion/complete`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompleteRequestParams {
    /// Reference being completed.
    #[serde(rename = "ref")]
    pub reference: CompleteReference,
    /// Argument being completed.
    pub argument: CompletionArgument,
    /// Additional context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<CompletionContext>,
}

/// Parameters of `notifications/cancelled`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledNotificationParams {
    /// Id of the cancelled request.
    pub request_id: RequestId,
    /// Reason.
    #[serde(default)]
    pub reason: String,
}

/// Parameters of `notifications/progress`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressNotificationParams {
    /// Token identifying the operation.
    pub progress_token: ProgressToken,
    /// Current progress.
    #[serde(default)]
    pub progress: f64,
    /// Total, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    /// Human readable message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Parameters of `notifications/message`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoggingMessageNotificationParams {
    /// Level name.
    #[serde(default)]
    pub level: String,
    /// Logger name.
    #[serde(default)]
    pub logger: String,
    /// Payload.
    #[serde(default)]
    pub data: Value,
}

/// Parameters of `notifications/resources/updated`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceUpdatedNotificationParams {
    /// Updated resource URI.
    #[serde(default)]
    pub uri: String,
}

/// Read `params._meta.progressToken` from raw params.
pub fn progress_meta(params: &Option<Value>) -> Option<RequestParamsMeta> {
    let meta = params.as_ref()?.get("_meta")?;
    if !meta.is_object() {
        return None;
    }
    serde_json::from_value(meta.clone()).ok()
}

/// Inject `params._meta.progressToken`. Params that are not an object are
/// replaced by an object, so any request can carry progress.
pub fn inject_progress_token(params: &mut Option<Value>, token: &ProgressToken) {
    let token_value = serde_json::to_value(token).unwrap_or(Value::Null);
    let mut obj = match params.take() {
        Some(Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    let meta = obj
        .entry("_meta")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !meta.is_object() {
        *meta = Value::Object(serde_json::Map::new());
    }
    meta["progressToken"] = token_value;
    *params = Some(Value::Object(obj));
}

// ---------------------------------------------------------------------------
// Message parsing
// ---------------------------------------------------------------------------

/// Why an incoming payload was rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageParseError {
    /// The text is not valid JSON.
    InvalidJson(String),
    /// The JSON is not a valid JSON-RPC message. `id` is taken from the payload
    /// when possible, otherwise it is `0`, matching the C++ SDK.
    InvalidMessage {
        /// Id extracted from the payload.
        id: RequestId,
        /// The `Deserialization Failed` message.
        message: String,
        /// True when the payload carried a `method` field.
        has_method: bool,
    },
}

impl MessageParseError {
    /// The JSON-RPC error a peer should receive.
    pub fn to_rpc_error(&self) -> RpcError {
        match self {
            MessageParseError::InvalidJson(m) => {
                RpcError::new(JsonRpcErrorCode::ParseError.code(), m.clone())
            }
            MessageParseError::InvalidMessage { message, .. } => {
                RpcError::new(JsonRpcErrorCode::InvalidRequest.code(), message.clone())
            }
        }
    }

    /// The error response (`id` is `0` for unparsable JSON).
    pub fn to_error_response(&self) -> Response {
        let id = match self {
            MessageParseError::InvalidJson(_) => RequestId::Number(0),
            MessageParseError::InvalidMessage { id, .. } => id.clone(),
        };
        Response::error(Some(id), self.to_rpc_error())
    }
}

impl std::fmt::Display for MessageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageParseError::InvalidJson(m) => write!(f, "invalid JSON: {m}"),
            MessageParseError::InvalidMessage { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for MessageParseError {}

fn id_from_value(v: Option<&Value>) -> RequestId {
    match v {
        Some(Value::String(s)) => RequestId::String(s.clone()),
        Some(Value::Number(n)) => n.as_i64().map(RequestId::Number).unwrap_or(RequestId::Number(0)),
        _ => RequestId::Number(0),
    }
}

fn invalid(obj: &serde_json::Map<String, Value>, detail: &str) -> MessageParseError {
    MessageParseError::InvalidMessage {
        id: id_from_value(obj.get("id")),
        message: format!("Deserialization Failed: {detail}"),
        has_method: obj.contains_key("method"),
    }
}

/// Classify one JSON value as a request, notification, response or error
/// response, following the C++ `DeserializeJSONRPCMessage` rules.
pub fn classify_message(value: Value) -> Result<Message, MessageParseError> {
    let Value::Object(obj) = value else {
        return Err(MessageParseError::InvalidMessage {
            id: RequestId::Number(0),
            message: "Deserialization Failed: No matching message type".into(),
            has_method: false,
        });
    };
    let has_id = obj.contains_key("id");
    let has_method = obj.contains_key("method");
    let has_code = obj
        .get("error")
        .and_then(Value::as_object)
        .map(|e| e.contains_key("code"))
        .unwrap_or(false);

    let valid_id = matches!(obj.get("id"), Some(Value::String(_)))
        || matches!(obj.get("id"), Some(Value::Number(n)) if n.is_i64() || n.is_u64());

    if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(invalid(&obj, "jsonrpc must be \"2.0\""));
    }

    if has_id && has_method {
        if !valid_id {
            return Err(invalid(&obj, "id must be a string or an integer"));
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return Err(invalid(&obj, "method must be a string"));
        };
        return Ok(Message::Request(Request::new(
            id_from_value(obj.get("id")),
            method,
            obj.get("params").cloned(),
        )));
    }

    if has_id && !has_method && !has_code {
        if !valid_id {
            return Err(invalid(&obj, "id must be a string or an integer"));
        }
        return Ok(Message::Response(Response {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id_from_value(obj.get("id"))),
            result: obj.get("result").cloned(),
            error: None,
        }));
    }

    if !has_id && has_method {
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return Err(invalid(&obj, "method must be a string"));
        };
        return Ok(Message::Notification(Notification::new(
            method,
            obj.get("params").cloned(),
        )));
    }

    if has_id && has_code {
        if !valid_id {
            return Err(invalid(&obj, "id must be a string or an integer"));
        }
        let error = obj
            .get("error")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let Some(code) = error.get("code").and_then(Value::as_i64) else {
            return Err(invalid(&obj, "error.code must be an integer"));
        };
        let Some(message) = error.get("message").and_then(Value::as_str) else {
            return Err(invalid(&obj, "error.message must be a string"));
        };
        let mut rpc = RpcError::new(code, message);
        rpc.data = error.get("data").cloned();
        return Ok(Message::Response(Response::error(
            Some(id_from_value(obj.get("id"))),
            rpc,
        )));
    }

    Err(invalid(&obj, "No matching message type"))
}

/// Parse one JSON-RPC message from text.
pub fn parse_message(text: &str) -> Result<Message, MessageParseError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| MessageParseError::InvalidJson(e.to_string()))?;
    classify_message(value)
}

/// A parsed POST body: one message or a batch.
#[derive(Debug, Clone, PartialEq)]
pub enum IncomingPayload {
    /// A single message.
    Single(Message),
    /// A batch of messages.
    Batch(Vec<Message>),
}

/// Parse a single message or a batch array.
pub fn parse_payload(text: &str) -> Result<IncomingPayload, MessageParseError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| MessageParseError::InvalidJson(e.to_string()))?;
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                return Err(MessageParseError::InvalidMessage {
                    id: RequestId::Number(0),
                    message: "Deserialization Failed: empty batch".into(),
                    has_method: false,
                });
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(classify_message(item)?);
            }
            Ok(IncomingPayload::Batch(out))
        }
        other => classify_message(other).map(IncomingPayload::Single),
    }
}

/// Serialize a message to compact JSON.
pub fn serialize_message(message: &Message) -> String {
    serde_json::to_string(&message.to_value()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Endpoint validation
// ---------------------------------------------------------------------------

const MAX_STREAMABLE_HTTP_URL_LENGTH: usize = 2048;

/// Split `http(s)://host[:port][/path]` into host and port text. Returns
/// `None` when the text does not have that shape.
fn split_http_endpoint(endpoint: &str) -> Option<(&str, Option<&str>)> {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if authority
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '?' | '#'))
    {
        return None;
    }
    if path.chars().any(|c| c.is_whitespace() || c == '#') {
        return None;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    Some((host, port))
}

/// Validate a Streamable HTTP endpoint URL (`IsValidStreamableHttpEndpoint`).
///
/// Requires an `http` or `https` scheme and a host. The port is optional
/// and must be in `1..=65535`. Fragments, spaces and oversized URLs are rejected.
pub fn is_valid_streamable_http_endpoint(endpoint: &str) -> Result<(), String> {
    if endpoint.is_empty() {
        return Err("endpoint is empty".into());
    }
    if endpoint.contains(' ') {
        return Err("endpoint contains spaces".into());
    }
    if endpoint.len() > MAX_STREAMABLE_HTTP_URL_LENGTH {
        return Err("url length more than max url length".into());
    }
    let Some((_host, port)) = split_http_endpoint(endpoint) else {
        return Err("url is not valid".into());
    };
    if let Some(port) = port {
        match port.parse::<i64>() {
            Ok(p) if (1..=65535).contains(&p) => {}
            Ok(_) => return Err("port out of range".into()),
            Err(_) => return Err("invalid port".into()),
        }
    }
    Ok(())
}

/// Parsed listen endpoint of the HTTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEndpoint {
    /// `http` or `https`.
    pub scheme: String,
    /// Host or IP literal (without brackets).
    pub host: String,
    /// Port. Defaults to 80 or 443 when absent.
    pub port: u16,
    /// Path, `/` when absent.
    pub path: String,
}

/// Parse the server endpoint (`http://{host}:{port}/{path}`).
pub fn parse_server_endpoint(endpoint: &str) -> Result<ServerEndpoint, String> {
    if endpoint.is_empty() {
        return Err("No endpoint specified for transport".into());
    }
    let with_scheme = if endpoint.contains("://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    };
    let url =
        url::Url::parse(&with_scheme).map_err(|e| format!("Failed to parse endpoint '{endpoint}': {e}"))?;
    let scheme = url.scheme().to_string();
    if scheme != "http" && scheme != "https" {
        return Err(format!("Unsupported scheme in endpoint: {endpoint}"));
    }
    let host = url
        .host_str()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("Host is required in endpoint: {endpoint}"))?;
    let port = match url.port() {
        Some(p) => p,
        None => {
            if scheme == "https" {
                443
            } else {
                80
            }
        }
    };
    let mut path = url.path().to_string();
    if path.is_empty() {
        path = "/".into();
    }
    Ok(ServerEndpoint {
        scheme,
        host,
        port,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use serde_json::json;

    #[test]
    fn classify_request_response_notification_error() -> TestResult {
        let req = parse_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)?;
        assert!(matches!(req, Message::Request(ref r) if r.method == "initialize"));
        let req = parse_message(r#"{"jsonrpc":"2.0","id":"request-123","method":"tools/list"}"#)?;
        assert_eq!(req.id(), Some(&RequestId::String("request-123".into())));
        let resp = parse_message(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#)?;
        assert!(matches!(resp, Message::Response(ref r) if r.result.is_some()));
        let notif = parse_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
        assert!(notif.is_notification());
        let err = parse_message(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request","data":{"x":1}}}"#,
        )?;
        match err {
            Message::Response(r) => {
                let e = r.error.required()?;
                assert_eq!(e.code, -32600);
                assert_eq!(e.message, "Invalid Request");
                assert_eq!(e.data, Some(json!({"x": 1})));
            }
            _ => {
                return Err(
                    ap_support::testing::TestFailure::new("expected error response".to_string()).into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn classify_rejects_invalid_messages() -> TestResult {
        assert!(matches!(
            parse_message("not json"),
            Err(MessageParseError::InvalidJson(_))
        ));
        let missing_jsonrpc = parse_message(r#"{"id":1,"method":"x"}"#).err_or_fail()?;
        match missing_jsonrpc {
            MessageParseError::InvalidMessage {
                id,
                message,
                has_method,
            } => {
                assert_eq!(id, RequestId::Number(1));
                assert!(message.contains("Deserialization Failed"));
                assert!(has_method);
            }
            _ => return Err(ap_support::testing::TestFailure::new("unexpected value").into()),
        }
        let wrong_version =
            parse_message(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#).err_or_fail()?;
        assert_eq!(wrong_version.to_rpc_error().code, -32600);
        assert_eq!(wrong_version.to_error_response().id, Some(RequestId::Number(1)));
        let null_id = parse_message(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).err_or_fail()?;
        assert_eq!(null_id.to_rpc_error().code, -32600);
        let nothing = parse_message(r#"{"jsonrpc":"2.0"}"#).err_or_fail()?;
        assert!(nothing.to_string().contains("No matching message type"));
        let bad_code =
            parse_message(r#"{"jsonrpc":"2.0","id":1,"error":{"code":"x","message":"m"}}"#).err_or_fail()?;
        assert!(bad_code.to_string().contains("error.code"));
        let missing_message =
            parse_message(r#"{"jsonrpc":"2.0","id":1,"error":{"code":1}}"#).err_or_fail()?;
        assert!(missing_message.to_string().contains("error.message"));
        assert!(parse_message("[]").is_err());
        assert_eq!(
            parse_message("5").err_or_fail()?.to_error_response().id,
            Some(RequestId::Number(0))
        );
        Ok(())
    }

    #[test]
    fn response_with_error_missing_code_is_a_response() -> TestResult {
        let msg = parse_message(r#"{"jsonrpc":"2.0","id":1,"error":{"message":"m"}}"#)?;
        assert!(matches!(msg, Message::Response(ref r) if r.error.is_none()));
        let both = parse_message(r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"m"}}"#)?;
        assert!(matches!(both, Message::Response(ref r) if r.error.is_some()));
        Ok(())
    }

    #[test]
    fn payload_batches() -> TestResult {
        let batch =
            parse_payload(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"n"}]"#)?;
        match batch {
            IncomingPayload::Batch(items) => assert_eq!(items.len(), 2),
            _ => return Err(ap_support::testing::TestFailure::new("unexpected value").into()),
        }
        assert!(parse_payload("[]").is_err());
        assert!(matches!(
            parse_payload(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)?,
            IncomingPayload::Single(_)
        ));
        let bad = parse_payload(
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"1.0","id":2,"method":"x"}]"#,
        );
        assert!(bad.is_err());
        Ok(())
    }

    #[test]
    fn progress_token_injection() -> TestResult {
        let mut params = Some(json!({"name": "t"}));
        inject_progress_token(&mut params, &ProgressToken::Number(7));
        assert_eq!(params.as_ref().required()?["_meta"]["progressToken"], json!(7));
        assert_eq!(
            progress_meta(&params).required()?.progress_token,
            Some(ProgressToken::Number(7))
        );
        let mut none = None;
        inject_progress_token(&mut none, &ProgressToken::from("x"));
        assert_eq!(none.required()?["_meta"]["progressToken"], json!("x"));
        assert!(progress_meta(&Some(json!({"_meta": 5}))).is_none());
        Ok(())
    }

    #[test]
    fn endpoint_validation() -> TestResult {
        for ok in [
            "http://localhost:8080",
            "https://127.0.0.1:8001/mcp",
            "http://example.com:443/path?x=1",
            "http://localhost",
            "https://example.com/mcp",
        ] {
            assert!(is_valid_streamable_http_endpoint(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            is_valid_streamable_http_endpoint("").err_or_fail()?,
            "endpoint is empty"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://localhost:8080 ").err_or_fail()?,
            "endpoint contains spaces"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("not-a-url").err_or_fail()?,
            "url is not valid"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://local#host:8080").err_or_fail()?,
            "url is not valid"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://localhost:8080/path#frag").err_or_fail()?,
            "url is not valid"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://localhost:8080#frag").err_or_fail()?,
            "url is not valid"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("ftp://localhost:8080").err_or_fail()?,
            "url is not valid"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://localhost:99999").err_or_fail()?,
            "port out of range"
        );
        assert_eq!(
            is_valid_streamable_http_endpoint("http://localhost:0").err_or_fail()?,
            "port out of range"
        );
        let long = format!("http://{}.com", "a".repeat(3000));
        assert_eq!(
            is_valid_streamable_http_endpoint(&long).err_or_fail()?,
            "url length more than max url length"
        );
        Ok(())
    }

    #[test]
    fn server_endpoint_parsing() -> TestResult {
        let e = parse_server_endpoint("http://127.0.0.1:8000/mcp")?;
        assert_eq!(e.host, "127.0.0.1");
        assert_eq!(e.port, 8000);
        assert_eq!(e.path, "/mcp");
        let e = parse_server_endpoint("https://example.com")?;
        assert_eq!(e.port, 443);
        assert_eq!(e.path, "/");
        let e = parse_server_endpoint("http://[::1]:8080")?;
        assert_eq!(e.host, "::1");
        assert!(parse_server_endpoint("").is_err());
        assert!(parse_server_endpoint("ftp://x:1").is_err());
        Ok(())
    }

    #[test]
    fn version_helpers() -> TestResult {
        assert!(is_supported_protocol_version(DEFAULT_PROTOCOL_VERSION));
        assert!(is_supported_protocol_version(LATEST_PROTOCOL_VERSION));
        assert!(!is_supported_protocol_version("2024-11-05"));
        assert_eq!(JsonRpcErrorCode::InvalidRequest.code(), -32600);
        Ok(())
    }

    #[test]
    fn elicit_params_tagging() -> TestResult {
        let form: ElicitParams = serde_json::from_value(json!({"mode": "form", "message": "m",
            "requestedSchema": {"type": "object"}}))?;
        assert!(matches!(form, ElicitParams::Form { .. }));
        let url = ElicitParams::Url {
            message: "m".into(),
            url: "https://x".into(),
            elicitation_id: "e1".into(),
        };
        let v = serde_json::to_value(&url)?;
        assert_eq!(
            v,
            json!({"mode": "url", "message": "m", "url": "https://x", "elicitationId": "e1"})
        );
        Ok(())
    }
}
