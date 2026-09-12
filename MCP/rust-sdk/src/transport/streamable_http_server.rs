// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Streamable HTTP server transport, port of
//! `src/server/transport/streamable_http_server_transport.*`.
//!
//! One transport instance serves one MCP session. It validates HTTP
//! requests (headers, session id, protocol version), converts bodies into
//! JSON-RPC messages for the session and writes session output either as
//! JSON responses or as SSE events.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Weak};

use ap_jsonrpc::Message;
use ap_jsonrpc::sse::SseEvent;
use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};

use super::{
    CallbackHandle, HttpReply, HttpRequest, HttpResponse, RequestContext, ResponseSink, ServerTransport,
    SseFrame, TransportCallback,
};
use crate::error::{JsonRpcErrorCode, McpError};
use crate::protocol::{
    self, DEFAULT_PROTOCOL_VERSION, IncomingPayload, JSONRPC_VERSION, SUPPORTED_PROTOCOL_VERSIONS_STRING,
    headers, is_supported_protocol_version, methods, parse_payload, serialize_message, status,
};

/// How long a server initiated send waits for the client's GET stream to attach.
pub const GET_STREAM_WAIT_MS: u64 = 2000;

/// True when every byte is visible ASCII (0x21 to 0x7E).
fn is_visible_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

/// Server side Streamable HTTP transport for one session.
pub struct StreamableHttpServerTransport {
    session_id: Mutex<String>,
    is_json_response_enabled: bool,
    stateless: bool,
    get_stream: Mutex<Option<mpsc::UnboundedSender<SseFrame>>>,
    get_stream_ready: Notify,
    is_terminated: AtomicBool,
    callback: RwLock<Option<CallbackHandle>>,
    connection_counter: AtomicI64,
}

impl StreamableHttpServerTransport {
    /// Create a transport for `session_id`. An empty id means stateless or
    /// "no session id header". The id must be visible ASCII (`0x21..=0x7E`).
    pub fn new(
        session_id: &str,
        is_json_response_enabled: bool,
        stateless: bool,
    ) -> Result<Arc<Self>, McpError> {
        if !session_id.is_empty() && !is_visible_ascii(session_id) {
            return Err(McpError::argument(
                "Session ID must only contain visible ASCII characters (0x21-0x7E)",
            ));
        }
        Ok(Arc::new(Self {
            session_id: Mutex::new(session_id.to_string()),
            is_json_response_enabled,
            stateless,
            get_stream: Mutex::new(None),
            get_stream_ready: Notify::new(),
            is_terminated: AtomicBool::new(false),
            callback: RwLock::new(None),
            connection_counter: AtomicI64::new(1),
        }))
    }

    /// The session id (empty after termination).
    pub fn session_id(&self) -> String {
        self.session_id.lock().clone()
    }

    /// True after [`ServerTransport::terminate`].
    pub fn is_terminated(&self) -> bool {
        self.is_terminated.load(Ordering::SeqCst)
    }

    /// True while a GET stream is attached.
    pub fn has_get_stream(&self) -> bool {
        self.get_stream.lock().is_some()
    }

    fn callback(&self) -> Option<Arc<dyn TransportCallback>> {
        self.callback.read().as_ref().and_then(Weak::upgrade)
    }

    fn session_header(&self) -> Vec<(String, String)> {
        let id = self.session_id();
        if id.is_empty() {
            Vec::new()
        } else {
            vec![(headers::MCP_SESSION_ID.to_string(), id)]
        }
    }

    fn error_response(
        &self,
        message: &str,
        status_code: u16,
        code: i64,
        extra: Vec<(String, String)>,
    ) -> HttpResponse {
        let mut response = HttpResponse::new(status_code);
        response.headers = extra;
        response.headers.push((
            headers::CONTENT_TYPE.to_string(),
            headers::CONTENT_TYPE_JSON.to_string(),
        ));
        response.headers.extend(self.session_header());
        let body = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": "server-error",
            "error": {"code": code, "message": message},
        });
        response.body = body.to_string();
        response
    }

    fn invalid_request(&self, message: &str, status_code: u16) -> HttpReply {
        HttpReply::Full(self.error_response(
            message,
            status_code,
            JsonRpcErrorCode::InvalidRequest.code(),
            vec![],
        ))
    }

    fn json_response(&self, body: Option<String>, status_code: u16) -> HttpResponse {
        let mut response = HttpResponse::new(status_code);
        response.headers.push((
            headers::CONTENT_TYPE.to_string(),
            headers::CONTENT_TYPE_JSON.to_string(),
        ));
        response.headers.extend(self.session_header());
        response.body = body.unwrap_or_default();
        response
    }

    fn sse_headers(&self, with_accel: bool) -> Vec<(String, String)> {
        let mut h = vec![
            (
                headers::CONTENT_TYPE.to_string(),
                headers::CONTENT_TYPE_SSE.to_string(),
            ),
            (
                headers::CACHE_CONTROL.to_string(),
                headers::CACHE_CONTROL_NO_CACHE_NO_TRANSFORM.to_string(),
            ),
            (
                headers::CONNECTION.to_string(),
                headers::CONNECTION_KEEP_ALIVE.to_string(),
            ),
        ];
        if with_accel {
            h.push((headers::X_ACCEL_BUFFERING.to_string(), "no".to_string()));
        }
        h.extend(self.session_header());
        h
    }

    fn validate_protocol_version(&self, request: &HttpRequest) -> Option<HttpReply> {
        let version = request
            .header(headers::MCP_PROTOCOL_VERSION)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION);
        if is_supported_protocol_version(version) {
            return None;
        }
        let message = format!(
            "Bad Request: Unsupported protocol version: {version}. Supported versions: {SUPPORTED_PROTOCOL_VERSIONS_STRING}"
        );
        Some(self.invalid_request(&message, status::BAD_REQUEST))
    }

    fn validate_post_headers(&self, request: &HttpRequest) -> Option<HttpReply> {
        let accept = request.header(headers::ACCEPT).unwrap_or("");
        let has_json = accept.contains(headers::CONTENT_TYPE_JSON);
        let has_sse = accept.contains(headers::CONTENT_TYPE_SSE);
        if self.is_json_response_enabled {
            if !has_json {
                return Some(self.invalid_request(
                    "Not Acceptable: Client must accept application/json",
                    status::NOT_ACCEPTABLE,
                ));
            }
        } else if !has_json || !has_sse {
            return Some(self.invalid_request(
                "Not Acceptable: Client must accept both application/json and text/event-stream",
                status::NOT_ACCEPTABLE,
            ));
        }
        let content_type = request.header(headers::CONTENT_TYPE).unwrap_or("");
        if !content_type.contains(headers::CONTENT_TYPE_JSON) {
            return Some(self.invalid_request(
                "Unsupported Media Type: Content-Type must be application/json",
                status::UNSUPPORTED_MEDIA_TYPE,
            ));
        }
        None
    }

    fn validate_session_id(&self, request: &HttpRequest, is_initialization: bool) -> Option<HttpReply> {
        let expected = self.session_id();
        if is_initialization || expected.is_empty() {
            return None;
        }
        match request.header(headers::MCP_SESSION_ID) {
            None | Some("") => {
                Some(self.invalid_request("Bad Request: Missing session ID", status::BAD_REQUEST))
            }
            Some(id) if id != expected => {
                tracing::debug!("Invalid session ID: {id}, expected: {expected}");
                Some(self.invalid_request("Not Found: Invalid or expired session ID", status::NOT_FOUND))
            }
            _ => None,
        }
    }

    fn deliver(&self, messages: Vec<(Message, RequestContext)>) {
        let Some(callback) = self.callback() else {
            return;
        };
        tokio::spawn(async move {
            for (message, ctx) in messages {
                callback.on_message_received(message, ctx).await;
            }
        });
    }

    /// Handle one HTTP request and produce the reply.
    pub async fn handle_request(&self, request: HttpRequest) -> HttpReply {
        let request_session_id = request.header(headers::MCP_SESSION_ID).unwrap_or("");
        tracing::debug!(
            "Handle request for session {}, request.sessionid is {}",
            self.session_id(),
            request_session_id
        );

        if self.stateless && request.method != "POST" {
            return HttpReply::Full(self.error_response(
                "Method Not Allowed",
                status::METHOD_NOT_ALLOWED,
                JsonRpcErrorCode::InvalidRequest.code(),
                vec![(headers::ALLOW.to_string(), "POST".to_string())],
            ));
        }

        if self.is_terminated() {
            return self.invalid_request("Not Found: Session has been terminated", status::NOT_FOUND);
        }

        match request.method.as_str() {
            "POST" => self.handle_post(request),
            "GET" => self.handle_get(&request),
            "DELETE" => self.handle_delete(&request).await,
            _ => HttpReply::Full(self.error_response(
                "Method Not Allowed",
                status::METHOD_NOT_ALLOWED,
                JsonRpcErrorCode::InvalidRequest.code(),
                vec![(headers::ALLOW.to_string(), "GET, POST, DELETE".to_string())],
            )),
        }
    }

    fn handle_post(&self, request: HttpRequest) -> HttpReply {
        if self.callback().is_none() {
            return HttpReply::Full(
                HttpResponse::new(status::INTERNAL_SERVER_ERROR)
                    .header(headers::CONTENT_TYPE, headers::CONTENT_TYPE_TEXT_PLAIN)
                    .body("Callbacks not set"),
            );
        }
        if let Some(reply) = self.validate_post_headers(&request) {
            return reply;
        }

        let raw: Value = match serde_json::from_str(&request.body) {
            Ok(v) => v,
            Err(e) => {
                return HttpReply::Full(self.error_response(
                    &format!("Parse error: {e}"),
                    status::BAD_REQUEST,
                    JsonRpcErrorCode::ParseError.code(),
                    vec![],
                ));
            }
        };

        let is_initialization = match &raw {
            Value::Object(o) => o.get("method").and_then(Value::as_str) == Some(methods::INITIALIZE),
            Value::Array(items) => items
                .iter()
                .any(|m| m.get("method").and_then(Value::as_str) == Some(methods::INITIALIZE)),
            _ => false,
        };
        tracing::debug!("isInitializationRequest {is_initialization}");

        if let Some(reply) = self.validate_session_id(&request, is_initialization) {
            return reply;
        }
        if let Some(reply) = self.validate_protocol_version(&request) {
            return reply;
        }

        let (messages, is_batch) = match parse_payload(&request.body) {
            Ok(IncomingPayload::Single(m)) => (vec![m], false),
            Ok(IncomingPayload::Batch(list)) => (list, true),
            Err(err) => {
                let has_method = matches!(
                    &err,
                    protocol::MessageParseError::InvalidMessage { has_method: true, .. }
                );
                let response = err.to_error_response();
                if has_method {
                    // A malformed request is answered with the error body directly.
                    let code = if self.is_json_response_enabled {
                        status::OK
                    } else {
                        status::BAD_REQUEST
                    };
                    return HttpReply::Full(
                        self.json_response(Some(serialize_message(&Message::Response(response))), code),
                    );
                }
                // Malformed responses and notifications are accepted and delivered to the session,
                // which ignores them. Mirrors the C++ behaviour.
                let ctx = self.new_context(None);
                self.deliver(vec![(Message::Response(response), ctx)]);
                return HttpReply::Full(self.json_response(None, status::ACCEPTED));
            }
        };

        let request_count = messages.iter().filter(|m| m.is_request()).count();
        if request_count == 0 {
            tracing::debug!("Handle non request message for session {}", self.session_id());
            let ctx = self.new_context(None);
            let batch: Vec<(Message, RequestContext)> =
                messages.into_iter().map(|m| (m, ctx.clone())).collect();
            self.deliver(batch);
            return HttpReply::Full(self.json_response(None, status::ACCEPTED));
        }

        if self.is_json_response_enabled {
            let mut response_headers = vec![(
                headers::CONTENT_TYPE.to_string(),
                headers::CONTENT_TYPE_JSON.to_string(),
            )];
            response_headers.extend(self.session_header());
            let (sink, rx) = ResponseSink::json(request_count, is_batch, response_headers);
            let ctx = self.new_context(Some(sink));
            let batch: Vec<(Message, RequestContext)> = messages
                .into_iter()
                .map(|m| {
                    let mut c = ctx.clone();
                    c.method = m.method().unwrap_or("").to_string();
                    (m, c)
                })
                .collect();
            self.deliver(batch);
            return HttpReply::Deferred(rx);
        }

        let (sink, body) = ResponseSink::sse(request_count);
        let ctx = self.new_context(Some(sink));
        let batch: Vec<(Message, RequestContext)> = messages
            .into_iter()
            .map(|m| {
                let mut c = ctx.clone();
                c.method = m.method().unwrap_or("").to_string();
                (m, c)
            })
            .collect();
        self.deliver(batch);
        HttpReply::Stream {
            status: status::OK,
            headers: self.sse_headers(true),
            body,
        }
    }

    fn new_context(&self, sink: Option<ResponseSink>) -> RequestContext {
        RequestContext {
            connection_id: self.connection_counter.fetch_add(1, Ordering::SeqCst),
            session_id: self.session_id(),
            method: String::new(),
            is_get_stream: false,
            is_cancelled: false,
            sink,
        }
    }

    fn handle_get(&self, request: &HttpRequest) -> HttpReply {
        let accept = request.header(headers::ACCEPT).unwrap_or("");
        if !accept.contains(headers::CONTENT_TYPE_SSE) {
            return self.invalid_request(
                "Not Acceptable: Client must accept text/event-stream",
                status::NOT_ACCEPTABLE,
            );
        }
        let expected = self.session_id();
        if !expected.is_empty() {
            let got = request.header(headers::MCP_SESSION_ID).unwrap_or("");
            if got.is_empty() || got != expected {
                tracing::debug!("Invalid session ID: {got}, expected: {expected}");
                return self.invalid_request("Bad Request: Invalid session ID", status::BAD_REQUEST);
            }
        }
        if let Some(reply) = self.validate_protocol_version(request) {
            return reply;
        }
        let mut slot = self.get_stream.lock();
        if slot.as_ref().map(|tx| !tx.is_closed()).unwrap_or(false) {
            return self.invalid_request(
                "Conflict: Only one SSE stream is allowed per session",
                status::CONFLICT,
            );
        }
        let (tx, rx) = mpsc::unbounded_channel();
        *slot = Some(tx);
        drop(slot);
        self.get_stream_ready.notify_one();
        HttpReply::Stream {
            status: status::OK,
            headers: self.sse_headers(false),
            body: rx,
        }
    }

    async fn handle_delete(&self, request: &HttpRequest) -> HttpReply {
        let expected = self.session_id();
        if expected.is_empty() {
            return HttpReply::Full(self.error_response(
                "Method Not Allowed: Session termination not supported",
                status::METHOD_NOT_ALLOWED,
                JsonRpcErrorCode::InvalidRequest.code(),
                vec![],
            ));
        }
        let got = request.header(headers::MCP_SESSION_ID).unwrap_or("");
        if got.is_empty() || got != expected {
            return self.invalid_request("Bad Request: Invalid session ID", status::BAD_REQUEST);
        }
        // Build the response before termination so it still carries the session id.
        let response = self.json_response(None, status::OK);
        self.terminate().await;
        HttpReply::Full(response)
    }

    fn event_data(message: &Message) -> Bytes {
        let event = SseEvent::typed("message", serialize_message(message));
        Bytes::from(event.to_wire())
    }
}

#[async_trait]
impl ServerTransport for StreamableHttpServerTransport {
    async fn listen(&self) -> Result<(), McpError> {
        tracing::info!("Transport connected");
        Ok(())
    }

    async fn terminate(&self) {
        tracing::info!("Terminating session: {}", self.session_id());
        self.is_terminated.store(true, Ordering::SeqCst);
        if let Some(tx) = self.get_stream.lock().take() {
            let _ = tx.send(None);
        }
        self.session_id.lock().clear();
    }

    async fn send_message(&self, message: Message, ctx: &RequestContext) -> Result<(), McpError> {
        let is_get_stream = ctx.is_get_stream || ctx.connection_id == 0;
        let is_response = message.is_response();

        if is_get_stream {
            let mut tx = self.get_stream.lock().clone();
            if tx.is_none() && !self.is_terminated() {
                // The client attaches its GET stream right after `initialize`; give it a moment.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(GET_STREAM_WAIT_MS),
                    self.get_stream_ready.notified(),
                )
                .await;
                tx = self.get_stream.lock().clone();
            }
            let Some(tx) = tx else {
                return Err(McpError::state("SSE stream request context not set"));
            };
            if tx.send(Some(Self::event_data(&message))).is_err() {
                // The client went away; forget the stream so a new GET can attach.
                let mut slot = self.get_stream.lock();
                if slot.as_ref().map(|s| s.same_channel(&tx)).unwrap_or(false) {
                    *slot = None;
                }
                return Err(McpError::Transport("SSE stream is closed".into()));
            }
            return Ok(());
        }

        let Some(sink) = &ctx.sink else {
            return Err(McpError::state("HTTP callback not set"));
        };

        if self.is_json_response_enabled && sink.is_json() {
            if is_response {
                return sink.deliver_json(message.to_value());
            }
            tracing::debug!("not getstream and not response or error, not send message");
            return Ok(());
        }

        sink.send_frame(Self::event_data(&message))?;
        if is_response && sink.response_delivered() {
            sink.finish();
        }
        Ok(())
    }

    fn set_callback(&self, callback: Option<CallbackHandle>) {
        *self.callback.write() = callback;
    }
}

impl std::fmt::Debug for StreamableHttpServerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamableHttpServerTransport")
            .field("session_id", &self.session_id())
            .field("is_json_response_enabled", &self.is_json_response_enabled)
            .field("stateless", &self.stateless)
            .field("terminated", &self.is_terminated())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_jsonrpc::{RequestId, Response};
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    struct CountingCallback {
        count: AtomicUsize,
        last: Mutex<Option<(Message, RequestContext)>>,
    }

    impl CountingCallback {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                count: AtomicUsize::new(0),
                last: Mutex::new(None),
            })
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TransportCallback for CountingCallback {
        async fn on_message_received(&self, message: Message, ctx: RequestContext) {
            self.count.fetch_add(1, Ordering::SeqCst);
            *self.last.lock() = Some((message, ctx));
        }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    const VALID: &str = "test-session-12345-ABCDE";

    fn transport(id: &str, json: bool, stateless: bool) -> TestResult<Arc<StreamableHttpServerTransport>> {
        Ok(StreamableHttpServerTransport::new(id, json, stateless)?)
    }

    fn attach(t: &Arc<StreamableHttpServerTransport>) -> Arc<CountingCallback> {
        let cb = CountingCallback::new();
        let weak: Weak<dyn TransportCallback> = Arc::downgrade(&cb) as Weak<dyn TransportCallback>;
        t.set_callback(Some(weak));
        cb
    }

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn init_json() -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{DEFAULT_PROTOCOL_VERSION}","capabilities":{{}},"clientInfo":{{"name":"TestClient","version":"1.0.0"}}}}}}"#
        )
    }

    fn tools_list_json() -> String {
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#.to_string()
    }

    fn post_headers() -> HashMap<String, String> {
        headers(&[
            ("accept", "application/json, text/event-stream"),
            ("content-type", "application/json"),
        ])
    }

    fn expect_error_body(reply: &HttpReply, status_code: u16) -> TestResult<Value> {
        let full = reply.as_full().required()?;
        assert_eq!(full.status, status_code);
        assert_eq!(full.content_type(), Some("application/json"));
        Ok(serde_json::from_str(&full.body)?)
    }

    #[test]
    fn constructor_validates_session_id() -> TestResult {
        assert!(StreamableHttpServerTransport::new(VALID, false, false).is_ok());
        assert!(StreamableHttpServerTransport::new("", false, false).is_ok());
        assert!(StreamableHttpServerTransport::new("test-session-\u{a9}", false, false).is_err());
        assert!(StreamableHttpServerTransport::new("with space", true, false).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn listen_and_terminate() -> TestResult {
        let t = transport(VALID, false, false)?;
        t.listen().await?;
        assert!(!t.is_terminated());
        t.terminate().await;
        assert!(t.is_terminated());
        assert_eq!(t.session_id(), "");
        transport("", false, false)?.terminate().await;
        Ok(())
    }

    #[tokio::test]
    async fn terminated_session_returns_not_found() -> TestResult {
        let t = transport(VALID, false, false)?;
        t.terminate().await;
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), init_json()))
            .await;
        let body = expect_error_body(&reply, status::NOT_FOUND)?;
        assert_eq!(body["error"]["code"], -32600);
        assert_eq!(body["id"], "server-error");
        Ok(())
    }

    #[tokio::test]
    async fn post_without_callback_is_internal_error() -> TestResult {
        let t = transport(VALID, false, false)?;
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), init_json()))
            .await;
        assert_eq!(reply.status(), Some(status::INTERNAL_SERVER_ERROR));
        Ok(())
    }

    #[tokio::test]
    async fn post_accept_and_content_type_validation() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "application/xml"),
                    ("content-type", "application/json"),
                ]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));

        // SSE mode rejects a JSON-only accept header.
        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "application/json"),
                    ("content-type", "application/json"),
                ]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));

        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[("accept", "application/json, text/event-stream")]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::UNSUPPORTED_MEDIA_TYPE));

        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "application/json, text/event-stream"),
                    ("content-type", "application/xml"),
                ]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::UNSUPPORTED_MEDIA_TYPE));
        settle().await;
        assert_eq!(cb.count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn json_mode_accept_rules() -> TestResult {
        let t = transport(VALID, true, false)?;
        let cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "application/json"),
                    ("content-type", "application/json"),
                ]),
                init_json(),
            ))
            .await;
        assert!(matches!(reply, HttpReply::Deferred(_)));
        settle().await;
        assert_eq!(cb.count(), 1);

        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("content-type", "application/json"),
                ]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));
        settle().await;
        assert_eq!(cb.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn stateless_rules() -> TestResult {
        let t = transport("", false, true)?;
        let cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), tools_list_json()))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        assert_eq!(reply.header_value("content-type"), Some("text/event-stream"));
        settle().await;
        assert_eq!(cb.count(), 1);

        let get = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[("accept", "text/event-stream")]),
                "",
            ))
            .await;
        assert_eq!(get.status(), Some(status::METHOD_NOT_ALLOWED));
        assert_eq!(get.header_value("allow"), Some("POST"));
        let del = t
            .handle_request(HttpRequest::new("DELETE", HashMap::new(), ""))
            .await;
        assert_eq!(del.status(), Some(status::METHOD_NOT_ALLOWED));

        // JSON stateless accepts a JSON-only accept header and skips session validation.
        let t = transport("", true, true)?;
        let cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "application/json"),
                    ("content-type", "application/json"),
                ]),
                tools_list_json(),
            ))
            .await;
        assert!(matches!(reply, HttpReply::Deferred(_)));
        settle().await;
        assert_eq!(cb.count(), 1);
        let reply = t
            .handle_request(HttpRequest::new(
                "POST",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("content-type", "application/json"),
                ]),
                init_json(),
            ))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_json_and_wrong_version() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        for body in ["invalid json", "", "   "] {
            let mut h = post_headers();
            h.insert("mcp-session-id".into(), VALID.into());
            let reply = t.handle_request(HttpRequest::new("POST", h, body)).await;
            let json = expect_error_body(&reply, status::BAD_REQUEST)?;
            assert_eq!(json["error"]["code"], -32700);
        }

        let wrong = r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#;
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), wrong))
            .await;
        let json = expect_error_body(&reply, status::BAD_REQUEST)?;
        assert_eq!(json["id"], 1);
        assert_eq!(json["error"]["code"], -32600);
        assert!(
            json["error"]["message"]
                .as_str()
                .required()?
                .contains("Deserialization Failed")
        );

        let t_json = transport("", true, false)?;
        let _cb2 = attach(&t_json);
        let reply = t_json
            .handle_request(HttpRequest::new("POST", post_headers(), wrong))
            .await;
        let json = expect_error_body(&reply, status::OK)?;
        assert_eq!(json["error"]["code"], -32600);
        settle().await;
        assert_eq!(cb.count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn client_error_response_is_accepted_and_delivered() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        let mut h = post_headers();
        h.insert("mcp-session-id".into(), VALID.into());
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"User rejected"}}"#;
        let reply = t.handle_request(HttpRequest::new("POST", h, body)).await;
        assert_eq!(reply.status(), Some(status::ACCEPTED));
        assert_eq!(reply.header_value("mcp-session-id"), Some(VALID));
        settle().await;
        assert_eq!(cb.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn session_id_validation_for_post() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        // Initialize passes without a session id and with one.
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), init_json()))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        let mut h = post_headers();
        h.insert("mcp-session-id".into(), VALID.into());
        let reply = t
            .handle_request(HttpRequest::new("POST", h.clone(), init_json()))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        settle().await;
        assert_eq!(cb.count(), 2);

        // Non-initialize requests need the right id.
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), tools_list_json()))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));
        let mut wrong = post_headers();
        wrong.insert("mcp-session-id".into(), "wrong-session-id".into());
        let reply = t
            .handle_request(HttpRequest::new("POST", wrong, tools_list_json()))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_FOUND));
        let reply = t
            .handle_request(HttpRequest::new("POST", h.clone(), tools_list_json()))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        settle().await;
        assert_eq!(cb.count(), 3);

        // Protocol version.
        h.insert("mcp-protocol-version".into(), "unsupported-version".into());
        let reply = t
            .handle_request(HttpRequest::new("POST", h.clone(), tools_list_json()))
            .await;
        let json = expect_error_body(&reply, status::BAD_REQUEST)?;
        assert!(
            json["error"]["message"]
                .as_str()
                .required()?
                .contains("Unsupported protocol version")
        );
        h.insert("mcp-protocol-version".into(), DEFAULT_PROTOCOL_VERSION.into());
        let reply = t
            .handle_request(HttpRequest::new("POST", h, tools_list_json()))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        settle().await;
        assert_eq!(cb.count(), 4);

        // Upper case header names are not recognised, like the C++ lowercase lookup.
        let upper = headers(&[
            ("Accept", "application/json, text/event-stream"),
            ("Content-Type", "application/json"),
            ("MCP-SESSION-ID", VALID),
        ]);
        let reply = t
            .handle_request(HttpRequest::new("POST", upper, tools_list_json()))
            .await;
        assert_ne!(reply.status(), Some(status::OK));
        Ok(())
    }

    #[tokio::test]
    async fn get_stream_rules() -> TestResult {
        let t = transport(VALID, false, false)?;
        let _cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new("GET", HashMap::new(), ""))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));
        let reply = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[("accept", "application/json")]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::NOT_ACCEPTABLE));
        let reply = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[("accept", "text/event-stream")]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));
        let reply = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("mcp-session-id", "wrong-session-id"),
                ]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));
        let reply = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("mcp-session-id", VALID),
                    ("mcp-protocol-version", "unsupported-version"),
                ]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));

        let good = headers(&[
            ("accept", "text/event-stream"),
            ("mcp-session-id", VALID),
            ("mcp-protocol-version", DEFAULT_PROTOCOL_VERSION),
        ]);
        let reply = t.handle_request(HttpRequest::new("GET", good.clone(), "")).await;
        assert_eq!(reply.status(), Some(status::OK));
        assert_eq!(reply.header_value("content-type"), Some("text/event-stream"));
        assert!(t.has_get_stream());
        let dup = t.handle_request(HttpRequest::new("GET", good, "")).await;
        assert_eq!(dup.status(), Some(status::CONFLICT));

        // Server initiated messages go to the GET stream.
        let HttpReply::Stream { mut body, .. } = reply else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        let ctx = RequestContext::server_initiated(VALID, "notifications/tools/list_changed");
        t.send_message(
            Message::Notification(ap_jsonrpc::Notification::new(
                "notifications/tools/list_changed",
                None,
            )),
            &ctx,
        )
        .await?;
        let frame = body.recv().await.required()?.required()?;
        let text = String::from_utf8(frame.to_vec())?;
        assert!(text.starts_with("event: message\ndata: "));
        assert!(text.contains("notifications/tools/list_changed"));

        // Dropping the client stream makes the next send fail and frees the slot.
        drop(body);
        assert!(
            t.send_message(
                Message::Notification(ap_jsonrpc::Notification::new("x", None)),
                &ctx
            )
            .await
            .is_err()
        );
        assert!(!t.has_get_stream());
        let empty = transport("", false, false)?;
        let reply = empty
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("mcp-protocol-version", DEFAULT_PROTOCOL_VERSION),
                ]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        Ok(())
    }

    #[tokio::test]
    async fn get_stream_missing_is_an_error() -> TestResult {
        // A terminated transport does not wait for a stream to attach.
        let t = transport(VALID, false, false)?;
        t.terminate().await;
        let ctx = RequestContext::server_initiated(VALID, "notifications/progress");
        let err = t
            .send_message(
                Message::Notification(ap_jsonrpc::Notification::new("notifications/progress", None)),
                &ctx,
            )
            .await
            .err_or_fail()?;
        assert_eq!(err.to_string(), "SSE stream request context not set");

        // A stream attached while waiting is used.
        let t2 = transport(VALID, false, false)?;
        let t3 = t2.clone();
        let sender = tokio::spawn(async move {
            t3.send_message(
                Message::Notification(ap_jsonrpc::Notification::new("n", None)),
                &ctx,
            )
            .await
        });
        tokio::task::yield_now().await;
        let reply = t2
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[("accept", "text/event-stream"), ("mcp-session-id", VALID)]),
                "",
            ))
            .await;
        let HttpReply::Stream { mut body, .. } = reply else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        sender.await??;
        assert!(body.recv().await.required()?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn delete_rules() -> TestResult {
        let t = transport("", false, false)?;
        let reply = t
            .handle_request(HttpRequest::new("DELETE", HashMap::new(), ""))
            .await;
        assert_eq!(reply.status(), Some(status::METHOD_NOT_ALLOWED));

        let t = transport(VALID, false, false)?;
        let reply = t
            .handle_request(HttpRequest::new("DELETE", HashMap::new(), ""))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));
        let reply = t
            .handle_request(HttpRequest::new(
                "DELETE",
                headers(&[("mcp-session-id", "wrong-session-id")]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::BAD_REQUEST));
        let reply = t
            .handle_request(HttpRequest::new(
                "DELETE",
                headers(&[("mcp-session-id", VALID)]),
                "",
            ))
            .await;
        assert_eq!(reply.status(), Some(status::OK));
        assert_eq!(reply.header_value("mcp-session-id"), Some(VALID));
        assert!(t.is_terminated());
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_method() -> TestResult {
        let t = transport(VALID, false, false)?;
        let reply = t
            .handle_request(HttpRequest::new("PUT", HashMap::new(), ""))
            .await;
        assert_eq!(reply.status(), Some(status::METHOD_NOT_ALLOWED));
        assert_eq!(reply.header_value("Allow"), Some("GET, POST, DELETE"));
        Ok(())
    }

    #[tokio::test]
    async fn complete_workflow_and_response_streaming() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        let reply = t
            .handle_request(HttpRequest::new("POST", post_headers(), init_json()))
            .await;
        let HttpReply::Stream {
            mut body, headers: h, ..
        } = reply
        else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert!(h.iter().any(|(k, v)| k == "mcp-session-id" && v == VALID));
        settle().await;
        assert_eq!(cb.count(), 1);
        let (msg, ctx) = cb.last.lock().take().required()?;
        assert_eq!(msg.method(), Some("initialize"));
        assert_eq!(ctx.method, "initialize");
        assert!(ctx.connection_id > 0);

        // Notification on the request context is streamed, then the response ends the stream.
        t.send_message(
            Message::Notification(ap_jsonrpc::Notification::new("notifications/progress", None)),
            &ctx,
        )
        .await?;
        t.send_message(
            Message::Response(Response::success(RequestId::Number(1), serde_json::json!({}))),
            &ctx,
        )
        .await?;
        let first = body.recv().await.required()?.required()?;
        assert!(String::from_utf8_lossy(&first).contains("notifications/progress"));
        let second = body.recv().await.required()?.required()?;
        assert!(String::from_utf8_lossy(&second).contains(r#""result":{}"#));
        assert!(body.recv().await.required()?.is_none());

        let get = t
            .handle_request(HttpRequest::new(
                "GET",
                headers(&[
                    ("accept", "text/event-stream"),
                    ("mcp-session-id", VALID),
                    ("mcp-protocol-version", DEFAULT_PROTOCOL_VERSION),
                ]),
                "",
            ))
            .await;
        assert_eq!(get.status(), Some(status::OK));
        let del = t
            .handle_request(HttpRequest::new(
                "DELETE",
                headers(&[("mcp-session-id", VALID)]),
                "",
            ))
            .await;
        assert_eq!(del.status(), Some(status::OK));
        let after = t
            .handle_request(HttpRequest::new("POST", post_headers(), tools_list_json()))
            .await;
        assert_eq!(after.status(), Some(status::NOT_FOUND));
        Ok(())
    }

    #[tokio::test]
    async fn json_mode_response_and_batch() -> TestResult {
        let t = transport(VALID, true, false)?;
        let cb = attach(&t);
        let mut h = headers(&[
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ]);
        h.insert("mcp-session-id".into(), VALID.into());
        let batch = r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":2,"method":"ping"}]"#;
        let reply = t.handle_request(HttpRequest::new("POST", h, batch)).await;
        let HttpReply::Deferred(rx) = reply else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        settle().await;
        assert_eq!(cb.count(), 3);
        let (_, ctx) = cb.last.lock().take().required()?;
        // Notifications are dropped in JSON mode; responses complete the HTTP response.
        t.send_message(
            Message::Notification(ap_jsonrpc::Notification::new("n", None)),
            &ctx,
        )
        .await?;
        t.send_message(
            Message::Response(Response::success(RequestId::Number(1), serde_json::json!({}))),
            &ctx,
        )
        .await?;
        t.send_message(
            Message::Response(Response::success(RequestId::Number(2), serde_json::json!({}))),
            &ctx,
        )
        .await?;
        let response = rx.await?;
        assert_eq!(response.status, status::OK);
        assert_eq!(response.header_value("mcp-session-id"), Some(VALID));
        let json: Value = serde_json::from_str(&response.body)?;
        assert_eq!(json.as_array().required()?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn batch_of_notifications_is_accepted() -> TestResult {
        let t = transport(VALID, false, false)?;
        let cb = attach(&t);
        let mut h = post_headers();
        h.insert("mcp-session-id".into(), VALID.into());
        let batch = r#"[{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":9,"result":{}}]"#;
        let reply = t.handle_request(HttpRequest::new("POST", h, batch)).await;
        assert_eq!(reply.status(), Some(status::ACCEPTED));
        settle().await;
        assert_eq!(cb.count(), 2);
        Ok(())
    }
}
