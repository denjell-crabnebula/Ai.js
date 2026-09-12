// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Transport abstractions, port of `src/transport/transport.h` and the HTTP
//! message types of `src/shared/http_common.h`.
//!
//! A transport moves JSON-RPC messages. The session attached to it receives
//! incoming messages through [`TransportCallback`].

pub mod stdio;
pub mod streamable_http_client;
pub mod streamable_http_server;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ap_jsonrpc::Message;
use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::error::McpError;
use crate::protocol::headers;

pub use stdio::{StdioClientTransport, StdioServerTransport};
pub use streamable_http_client::StreamableHttpClientTransport;
pub use streamable_http_server::StreamableHttpServerTransport;

/// Callback interface a session installs on its transport.
#[async_trait]
pub trait TransportCallback: Send + Sync {
    /// Invoked for every JSON-RPC message received.
    async fn on_message_received(&self, message: Message, ctx: RequestContext);

    /// Invoked when the transport is disconnected.
    async fn on_disconnected(&self, _reason: String) {}
}

/// Shared weak handle to a transport callback.
pub type CallbackHandle = Weak<dyn TransportCallback>;

/// Client side transport.
#[async_trait]
pub trait ClientTransport: Send + Sync {
    /// Establish resources required for communication.
    async fn connect(&self) -> Result<(), McpError>;

    /// Terminate the transport and release resources.
    async fn terminate(&self);

    /// Terminate the remote session gracefully (HTTP DELETE). Default: no-op.
    async fn terminate_session(&self, _timeout: Duration) {}

    /// Send a JSON-RPC message.
    async fn send_message(&self, message: Message) -> Result<(), McpError>;

    /// Install the session callback.
    fn set_callback(&self, callback: Option<CallbackHandle>);
}

/// Server side transport.
#[async_trait]
pub trait ServerTransport: Send + Sync {
    /// Start listening.
    async fn listen(&self) -> Result<(), McpError>;

    /// Terminate the transport.
    async fn terminate(&self);

    /// Send a JSON-RPC message to the client described by `ctx`.
    async fn send_message(&self, message: Message, ctx: &RequestContext) -> Result<(), McpError>;

    /// Install the session callback.
    fn set_callback(&self, callback: Option<CallbackHandle>);
}

// ---------------------------------------------------------------------------
// HTTP message types
// ---------------------------------------------------------------------------

/// An HTTP request as seen by the server transport. Header names are lowercase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method, upper case.
    pub method: String,
    /// Request path.
    pub url: String,
    /// Lowercase header names to values.
    pub headers: HashMap<String, String>,
    /// Request body.
    pub body: String,
}

impl HttpRequest {
    /// Build a request.
    pub fn new(method: &str, headers: HashMap<String, String>, body: impl Into<String>) -> Self {
        Self {
            method: method.to_string(),
            url: String::new(),
            headers,
            body: body.into(),
        }
    }

    /// Header value by lowercase name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// A complete HTTP response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// Headers in insertion order.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: String,
}

impl HttpResponse {
    /// Build a response.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// Add a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the body.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// First header value by case insensitive name.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Content type header.
    pub fn content_type(&self) -> Option<&str> {
        self.header_value(headers::CONTENT_TYPE)
    }
}

/// One SSE frame written to a streaming response. `None` ends the stream.
pub type SseFrame = Option<Bytes>;

/// What the server transport answers to an HTTP request.
#[derive(Debug)]
pub enum HttpReply {
    /// A complete response.
    Full(HttpResponse),
    /// A JSON response delivered when the session answers.
    Deferred(oneshot::Receiver<HttpResponse>),
    /// A streaming (SSE) response.
    Stream {
        /// Status code.
        status: u16,
        /// Headers.
        headers: Vec<(String, String)>,
        /// Frames. `None` ends the stream.
        body: mpsc::UnboundedReceiver<SseFrame>,
    },
}

impl HttpReply {
    /// The full response, if this is a [`HttpReply::Full`].
    pub fn as_full(&self) -> Option<&HttpResponse> {
        match self {
            HttpReply::Full(r) => Some(r),
            _ => None,
        }
    }

    /// Status code of a full or streaming reply.
    pub fn status(&self) -> Option<u16> {
        match self {
            HttpReply::Full(r) => Some(r.status),
            HttpReply::Stream { status, .. } => Some(*status),
            HttpReply::Deferred(_) => None,
        }
    }

    /// Header of a full or streaming reply.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        match self {
            HttpReply::Full(r) => r.header_value(name),
            HttpReply::Stream { headers, .. } => headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str()),
            HttpReply::Deferred(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Response sink and request context
// ---------------------------------------------------------------------------

enum SinkKind {
    Json {
        tx: Mutex<Option<oneshot::Sender<HttpResponse>>>,
        collected: Mutex<Vec<Value>>,
        batch: bool,
        headers: Vec<(String, String)>,
    },
    Sse {
        tx: mpsc::UnboundedSender<SseFrame>,
    },
}

/// Where a server session writes responses for one HTTP request.
///
/// Replaces the `httpSendFunc` callback of the C++ `RequestContext`.
#[derive(Clone)]
pub struct ResponseSink {
    inner: Arc<SinkInner>,
}

struct SinkInner {
    kind: SinkKind,
    expected: AtomicUsize,
}

impl std::fmt::Debug for ResponseSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.inner.kind {
            SinkKind::Json { .. } => "json",
            SinkKind::Sse { .. } => "sse",
        };
        f.debug_struct("ResponseSink")
            .field("kind", &kind)
            .field("expected", &self.inner.expected.load(Ordering::SeqCst))
            .finish()
    }
}

impl ResponseSink {
    /// A sink collecting `expected` JSON responses into one HTTP response.
    pub fn json(
        expected: usize,
        batch: bool,
        headers: Vec<(String, String)>,
    ) -> (Self, oneshot::Receiver<HttpResponse>) {
        let (tx, rx) = oneshot::channel();
        let sink = Self {
            inner: Arc::new(SinkInner {
                kind: SinkKind::Json {
                    tx: Mutex::new(Some(tx)),
                    collected: Mutex::new(Vec::new()),
                    batch,
                    headers,
                },
                expected: AtomicUsize::new(expected),
            }),
        };
        (sink, rx)
    }

    /// A sink streaming SSE frames. The stream ends after `expected` responses.
    pub fn sse(expected: usize) -> (Self, mpsc::UnboundedReceiver<SseFrame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = Self {
            inner: Arc::new(SinkInner {
                kind: SinkKind::Sse { tx },
                expected: AtomicUsize::new(expected),
            }),
        };
        (sink, rx)
    }

    /// A sink wrapping an existing SSE sender (the GET stream).
    pub fn sse_from_sender(tx: mpsc::UnboundedSender<SseFrame>) -> Self {
        Self {
            inner: Arc::new(SinkInner {
                kind: SinkKind::Sse { tx },
                expected: AtomicUsize::new(usize::MAX),
            }),
        }
    }

    /// True for a JSON sink.
    pub fn is_json(&self) -> bool {
        matches!(self.inner.kind, SinkKind::Json { .. })
    }

    /// Write one SSE frame. Fails when the receiver is gone.
    pub fn send_frame(&self, bytes: Bytes) -> Result<(), McpError> {
        match &self.inner.kind {
            SinkKind::Sse { tx } => tx
                .send(Some(bytes))
                .map_err(|_| McpError::Transport("SSE stream is closed".into())),
            SinkKind::Json { .. } => Err(McpError::state("cannot stream to a JSON response sink")),
        }
    }

    /// End an SSE stream.
    pub fn finish(&self) {
        if let SinkKind::Sse { tx } = &self.inner.kind {
            let _ = tx.send(None);
        }
    }

    /// Deliver a JSON-RPC response value. Completes the HTTP response when
    /// every expected response has arrived.
    pub fn deliver_json(&self, value: Value) -> Result<(), McpError> {
        let SinkKind::Json {
            tx,
            collected,
            batch,
            headers,
        } = &self.inner.kind
        else {
            return Err(McpError::state("cannot deliver JSON to an SSE sink"));
        };
        collected.lock().push(value);
        if self.response_delivered() {
            let items = std::mem::take(&mut *collected.lock());
            let body = if *batch {
                Value::Array(items)
            } else {
                items.into_iter().next().unwrap_or(Value::Null)
            };
            let mut response = HttpResponse::new(crate::protocol::status::OK);
            response.headers = headers.clone();
            response.body = body.to_string();
            if let Some(tx) = tx.lock().take() {
                tx.send(response)
                    .map_err(|_| McpError::Transport("HTTP response receiver is gone".into()))?;
            }
        }
        Ok(())
    }

    /// Record that one response was delivered. Returns true when the sink is complete.
    pub fn response_delivered(&self) -> bool {
        let remaining = self.inner.expected.fetch_sub(1, Ordering::SeqCst);
        remaining <= 1
    }

    /// Responses still expected.
    pub fn remaining(&self) -> usize {
        self.inner.expected.load(Ordering::SeqCst)
    }
}

/// Per request context passed from the transport to the session and back.
#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    /// Connection identifier. `0` means "the server initiated this send".
    pub connection_id: i64,
    /// Session id.
    pub session_id: String,
    /// Method of the request being handled.
    pub method: String,
    /// True when the message must go to the standalone GET stream.
    pub is_get_stream: bool,
    /// True when the request was cancelled.
    pub is_cancelled: bool,
    /// Where responses for this request are written (HTTP only).
    pub sink: Option<ResponseSink>,
}

impl RequestContext {
    /// A context for a server initiated message.
    pub fn server_initiated(session_id: &str, method: &str) -> Self {
        Self {
            connection_id: 0,
            session_id: session_id.to_string(),
            method: method.to_string(),
            is_get_stream: false,
            is_cancelled: false,
            sink: None,
        }
    }
}

/// Lowercase a header map, keeping the first value of each name.
pub fn lowercase_headers(headers: &http::HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_ascii_lowercase();
        if let Ok(v) = value.to_str() {
            out.entry(key).or_insert_with(|| v.trim().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use serde_json::json;

    #[tokio::test]
    async fn json_sink_collects_single_and_batch() -> TestResult {
        let (sink, rx) = ResponseSink::json(1, false, vec![("x".into(), "y".into())]);
        assert!(sink.is_json());
        assert_eq!(sink.remaining(), 1);
        sink.deliver_json(json!({"a": 1}))?;
        let response = rx.await?;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, r#"{"a":1}"#);
        assert_eq!(response.header_value("X"), Some("y"));

        let (sink, rx) = ResponseSink::json(2, true, vec![]);
        sink.deliver_json(json!(1))?;
        sink.deliver_json(json!(2))?;
        assert_eq!(rx.await?.body, "[1,2]");
        assert!(sink.send_frame(Bytes::from_static(b"x")).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sse_sink_streams_and_finishes() -> TestResult {
        let (sink, mut rx) = ResponseSink::sse(1);
        sink.send_frame(Bytes::from_static(b"data: 1\n\n"))?;
        assert!(sink.response_delivered());
        sink.finish();
        assert_eq!(
            rx.recv().await.required()?.required()?,
            Bytes::from_static(b"data: 1\n\n")
        );
        assert!(rx.recv().await.required()?.is_none());
        assert!(sink.deliver_json(json!(1)).is_err());
        drop(rx);
        assert!(sink.send_frame(Bytes::from_static(b"x")).is_err());
        Ok(())
    }

    #[test]
    fn http_helpers() -> TestResult {
        let mut headers = http::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse()?);
        headers.append("X-Multi", "a".parse()?);
        headers.append("X-Multi", "b".parse()?);
        let lowered = lowercase_headers(&headers);
        assert_eq!(lowered.get("content-type").required()?, "application/json");
        assert_eq!(lowered.get("x-multi").required()?, "a");
        let resp = HttpResponse::new(200)
            .header("Content-Type", "text/plain")
            .body("b");
        assert_eq!(resp.content_type(), Some("text/plain"));
        let req = HttpRequest::new("GET", lowered, "");
        assert_eq!(req.header("content-type"), Some("application/json"));
        let ctx = RequestContext::server_initiated("s", "m");
        assert_eq!(ctx.connection_id, 0);
        Ok(())
    }
}
