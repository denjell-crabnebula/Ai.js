// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Wire-level constants shared by the client and the server.
//!
//! This module ports `src/shared/common_types.h` and the header constants of
//! `src/shared/http_common.h`. The JSON-RPC method names are the exact
//! strings sent on the wire.

/// Default transport label for JSON-RPC over HTTP.
pub const JSONRPC_TRANSPORT: &str = "JSONRPC";

/// JSON-RPC protocol version string.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC method for `message/send`.
pub const METHOD_MESSAGE_SEND: &str = "SendMessage";
/// JSON-RPC method for `message/stream`.
pub const METHOD_MESSAGE_STREAM: &str = "SendStreamingMessage";
/// JSON-RPC method for `tasks/get`.
pub const METHOD_TASK_GET: &str = "GetTask";
/// JSON-RPC method for `tasks/cancel`.
pub const METHOD_TASK_CANCEL: &str = "CancelTask";
/// JSON-RPC method for `tasks/resubscribe`.
pub const METHOD_TASK_RESUBSCRIBE: &str = "SubscribeToTask";
/// JSON-RPC method for `tasks/pushNotificationConfig/set`.
pub const METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET: &str = "CreateTaskPushNotificationConfig";
/// JSON-RPC method for `tasks/pushNotificationConfig/get`.
pub const METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET: &str = "GetTaskPushNotificationConfig";
/// JSON-RPC method for `tasks/pushNotificationConfig/list`.
pub const METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST: &str = "ListTaskPushNotificationConfigs";
/// JSON-RPC method for `tasks/pushNotificationConfig/delete`.
pub const METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE: &str = "DeleteTaskPushNotificationConfig";

/// SDK-internal marker for an agent card request.
///
/// It is not a standard A2A method and never appears in a request payload.
pub const METHOD_AGENT_CARD_GET: &str = "GetAgentCard";

/// JSON field name `result`.
pub const JSON_FIELD_RESULT: &str = "result";
/// JSON field name `jsonrpc`.
pub const JSON_FIELD_JSONRPC: &str = "jsonrpc";
/// JSON field name `id`.
pub const JSON_FIELD_ID: &str = "id";
/// JSON field name `method`.
pub const JSON_FIELD_METHOD: &str = "method";
/// JSON field name `params`.
pub const JSON_FIELD_PARAMS: &str = "params";
/// JSON field name `error`.
pub const JSON_FIELD_ERROR: &str = "error";
/// JSON field name `message`.
pub const JSON_FIELD_MESSAGE: &str = "message";
/// JSON field name `metadata`.
pub const JSON_FIELD_METADATA: &str = "metadata";

/// Streaming result key carrying a full task.
pub const STREAM_RESPONSE_TYPE_TASK: &str = "task";
/// Streaming result type for a status update.
pub const STREAM_RESPONSE_TYPE_STATUS_UPDATE: &str = "status-update";
/// Streaming result type for an artifact update.
pub const STREAM_RESPONSE_TYPE_ARTIFACT_UPDATE: &str = "artifact-update";

/// Streaming result key carrying a `TaskStatusUpdateEvent`.
pub const STREAM_RESULT_KEY_STATUS_UPDATE: &str = "statusUpdate";
/// Streaming result key carrying a `TaskArtifactUpdateEvent`.
pub const STREAM_RESULT_KEY_ARTIFACT_UPDATE: &str = "artifactUpdate";

/// Protocol version sent in the `A2A-Version` header.
pub const DEFAULT_PROTOCOL_VERSION: &str = "1.0";

/// Well-known HTTP path of the agent card.
pub const AGENT_CARD_ENDPOINT: &str = "/.well-known/agent-card.json";
/// HTTP path of the authenticated extended agent card.
pub const EXTENDED_AGENT_CARD_ENDPOINT: &str = "/agent/authenticatedExtendedCard";
/// Default JSON-RPC endpoint path.
pub const DEFAULT_JSONRPC_ENDPOINT: &str = "/jsonrpc";

/// HTTP header names and values used on the wire.
pub mod http {
    /// `Content-Type` header name.
    pub const CONTENT_TYPE_HEADER: &str = "Content-Type";
    /// `accept` header name.
    pub const ACCEPT_HEADER: &str = "accept";
    /// `Accept-Encoding` header name.
    pub const ACCEPT_ENCODING_HEADER: &str = "Accept-Encoding";
    /// `Cache-Control` header name.
    pub const CACHE_CONTROL_HEADER: &str = "Cache-Control";
    /// `Connection` header name.
    pub const CONNECTION_HEADER: &str = "Connection";
    /// `X-Accel-Buffering` header name.
    pub const X_ACCEL_BUFFERING_HEADER: &str = "X-Accel-Buffering";
    /// JSON content type.
    pub const CONTENT_TYPE_JSON: &str = "application/json";
    /// Server-sent events content type.
    pub const CONTENT_TYPE_SSE: &str = "text/event-stream";
    /// Plain text content type.
    pub const CONTENT_TYPE_TEXT_PLAIN: &str = "text/plain";
    /// `Cache-Control` value used for streams.
    pub const CACHE_CONTROL_NO_CACHE_NO_TRANSFORM: &str = "no-cache, no-transform";
    /// `Connection: keep-alive`.
    pub const CONNECTION_KEEP_ALIVE: &str = "keep-alive";
    /// `Accept-Encoding` value sent with agent card requests.
    pub const ACCEPT_ENCODING_VALUE: &str = "gzip, deflate";
    /// A2A protocol version header name.
    pub const K_PROTOCOL_VERSION_HEADER: &str = "A2A-Version";

    /// HTTP 200.
    pub const HTTP_STATUS_OK: i64 = 200;
    /// HTTP 404.
    pub const HTTP_STATUS_NOT_FOUND: i64 = 404;
    /// HTTP 500.
    pub const HTTP_STATUS_INTERNAL_SERVER_ERROR: i64 = 500;

    /// Internal parse status used as an error code for malformed responses.
    pub const HTTP_PARSE_ERROR: i64 = -1;

    /// Default connection timeout in seconds.
    pub const DEFAULT_CONNECTION_TIMEOUT_SECS: u64 = 30;
    /// Default request timeout in seconds.
    pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;
}
