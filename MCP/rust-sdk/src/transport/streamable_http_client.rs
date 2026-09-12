// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Streamable HTTP client transport, port of
//! `src/client/transport/streamable_http_client_transport.*` and the
//! behaviour of `http_client_service.*` on top of `reqwest`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ap_jsonrpc::sse::SseParser;
use ap_jsonrpc::{Message, RequestId, Response, RpcError};
use async_trait::async_trait;
use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use tokio::task::JoinHandle;

use super::{CallbackHandle, ClientTransport, RequestContext, TransportCallback};
use crate::auth::AuthProvider;
use crate::error::{JsonRpcErrorCode, McpError};
use crate::protocol::{headers, methods, parse_message, serialize_message, status};
use crate::types::TlsConfig;

/// Default HTTP timeout of the transport constructor.
pub const DEFAULT_HTTP_TIMEOUT_MS: u64 = 30_000;
/// Default SSE read timeout of the transport constructor.
pub const DEFAULT_SSE_READ_TIMEOUT_MS: u64 = 60_000;
/// Largest accepted timeout (30 minutes).
pub const MAX_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Client side Streamable HTTP transport.
pub struct StreamableHttpClientTransport {
    url: String,
    request_headers: HashMap<String, String>,
    timeout: Duration,
    sse_read_timeout: Duration,
    tls: TlsConfig,
    auth_provider: Option<Arc<dyn AuthProvider>>,
    client: Mutex<Option<reqwest::Client>>,
    session_id: Mutex<String>,
    protocol_version: Mutex<String>,
    callback: RwLock<Option<CallbackHandle>>,
    get_stream_task: Mutex<Option<JoinHandle<()>>>,
    running: AtomicBool,
    weak_self: Weak<Self>,
}

fn build_client(
    tls: &TlsConfig,
    timeout: Duration,
    resolve: Option<(String, std::net::SocketAddr)>,
) -> Result<reqwest::Client, McpError> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(timeout)
        .no_proxy();
    if !tls.ca_file.is_empty() {
        let pem = std::fs::read(&tls.ca_file)
            .map_err(|e| McpError::Transport(format!("Failed to read TLS CA file {}: {e}", tls.ca_file)))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| McpError::Transport(format!("Invalid TLS CA file {}: {e}", tls.ca_file)))?;
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }
    if !tls.cert_file.is_empty() && !tls.key_file.is_empty() {
        let mut pem = std::fs::read(&tls.cert_file).map_err(|e| {
            McpError::Transport(format!("Failed to read TLS certificate {}: {e}", tls.cert_file))
        })?;
        pem.push(b'\n');
        let key = std::fs::read(&tls.key_file)
            .map_err(|e| McpError::Transport(format!("Failed to read TLS key {}: {e}", tls.key_file)))?;
        pem.extend_from_slice(&key);
        let identity = reqwest::Identity::from_pem(&pem)
            .map_err(|e| McpError::Transport(format!("Invalid TLS client identity: {e}")))?;
        builder = builder.identity(identity);
    }
    if !tls.verify_peer {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some((domain, addr)) = resolve {
        builder = builder.resolve(&domain, addr);
    }
    builder
        .build()
        .map_err(|e| McpError::Transport(format!("HttpClientService creation failed: {e}")))
}

impl StreamableHttpClientTransport {
    /// Create a transport.
    ///
    /// `timeout` is the connection timeout and `sse_read_timeout` the whole
    /// request timeout for POST requests. Both must be in `1..=MAX_TIMEOUT_MS`.
    pub fn new(
        url: impl Into<String>,
        headers: HashMap<String, String>,
        timeout: Duration,
        sse_read_timeout: Duration,
        tls: TlsConfig,
        auth_provider: Option<Arc<dyn AuthProvider>>,
    ) -> Result<Arc<Self>, McpError> {
        let timeout_ms = timeout.as_millis() as u64;
        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            tracing::error!(
                "Invalid timeout value: {timeout_ms} milliseconds, max is {MAX_TIMEOUT_MS} milliseconds"
            );
            return Err(McpError::argument("Invalid timeout value"));
        }
        let sse_ms = sse_read_timeout.as_millis() as u64;
        if sse_ms == 0 || sse_ms > MAX_TIMEOUT_MS {
            tracing::error!(
                "Invalid SSE read timeout value: {sse_ms} milliseconds, max is {MAX_TIMEOUT_MS} milliseconds"
            );
            return Err(McpError::argument("Invalid SSE read timeout value"));
        }
        let mut request_headers = HashMap::new();
        request_headers.insert(
            headers::ACCEPT.to_string(),
            format!("{}, {}", headers::CONTENT_TYPE_JSON, headers::CONTENT_TYPE_SSE),
        );
        request_headers.insert(
            headers::CONTENT_TYPE.to_string(),
            headers::CONTENT_TYPE_JSON.to_string(),
        );
        for (k, v) in headers {
            request_headers.insert(k, v);
        }
        let client = build_client(&tls, timeout, None)?;
        Ok(Arc::new_cyclic(|weak| Self {
            url: url.into(),
            request_headers,
            timeout,
            sse_read_timeout,
            tls,
            auth_provider,
            client: Mutex::new(Some(client)),
            session_id: Mutex::new(String::new()),
            protocol_version: Mutex::new(String::new()),
            callback: RwLock::new(None),
            get_stream_task: Mutex::new(None),
            running: AtomicBool::new(true),
            weak_self: weak.clone(),
        }))
    }

    /// The session id captured from the `initialize` response.
    pub fn session_id(&self) -> String {
        self.session_id.lock().clone()
    }

    /// The protocol version negotiated in `initialize`.
    pub fn protocol_version(&self) -> String {
        self.protocol_version.lock().clone()
    }

    /// True until [`ClientTransport::terminate`].
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn callback(&self) -> Option<Arc<dyn TransportCallback>> {
        self.callback.read().as_ref().and_then(Weak::upgrade)
    }

    fn client(&self) -> Option<reqwest::Client> {
        self.client.lock().clone()
    }

    fn effective_url(&self) -> String {
        if self.tls.server_name.is_empty() {
            return self.url.clone();
        }
        match url::Url::parse(&self.url) {
            Ok(mut u) if u.scheme() == "https" => {
                if u.set_host(Some(&self.tls.server_name)).is_ok() {
                    return u.to_string();
                }
                self.url.clone()
            }
            _ => self.url.clone(),
        }
    }

    fn prepare_headers(&self) -> HashMap<String, String> {
        let mut h = self.request_headers.clone();
        if let Some(auth) = &self.auth_provider {
            auth.apply(&mut h);
        }
        let session_id = self.session_id();
        if !session_id.is_empty() {
            h.insert(headers::MCP_SESSION_ID.to_string(), session_id);
        }
        let version = self.protocol_version();
        if !version.is_empty() {
            h.insert(headers::MCP_PROTOCOL_VERSION.to_string(), version);
        }
        h
    }

    fn apply_headers(
        mut builder: reqwest::RequestBuilder,
        headers: &HashMap<String, String>,
    ) -> reqwest::RequestBuilder {
        for (k, v) in headers {
            match (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                (Ok(name), Ok(value)) => builder = builder.header(name, value),
                _ => tracing::warn!("Skipping invalid header {k}"),
            }
        }
        builder
    }

    async fn report_error(&self, request_id: &Option<RequestId>, code: JsonRpcErrorCode, message: String) {
        let Some(id) = request_id else {
            return;
        };
        if *id == RequestId::Number(0) {
            return;
        }
        let Some(cb) = self.callback() else {
            return;
        };
        let error = RpcError::new(code.code(), message);
        let ctx = RequestContext::default();
        cb.on_message_received(Message::Response(Response::error(Some(id.clone()), error)), ctx)
            .await;
    }

    async fn deliver(&self, message: Message, is_initialization: bool) {
        if is_initialization {
            self.may_extract_protocol_version(&message);
        }
        if let Some(cb) = self.callback() {
            cb.on_message_received(message, RequestContext::default()).await;
        }
    }

    fn may_extract_protocol_version(&self, message: &Message) {
        if let Message::Response(r) = message {
            if let Some(v) = r
                .result
                .as_ref()
                .and_then(|res| res.get("protocolVersion"))
                .and_then(|v| v.as_str())
            {
                *self.protocol_version.lock() = v.to_string();
                tracing::info!("Extracted protocol version: {v}");
            }
        }
    }

    fn may_extract_session_id(&self, response: &reqwest::Response) {
        if let Some(v) = response
            .headers()
            .get(headers::MCP_SESSION_ID)
            .and_then(|v| v.to_str().ok())
        {
            if !v.is_empty() {
                *self.session_id.lock() = v.to_string();
                tracing::info!("Extracted session ID: {v}");
            }
        }
    }

    async fn do_post(self: Arc<Self>, body: String, request_id: Option<RequestId>, method: Option<String>) {
        let Some(client) = self.client() else {
            self.report_error(
                &request_id,
                JsonRpcErrorCode::InternalError,
                "HTTP request failed: HttpClientService is not running".into(),
            )
            .await;
            return;
        };
        let headers = self.prepare_headers();
        let builder = Self::apply_headers(client.post(self.effective_url()), &headers)
            .timeout(self.sse_read_timeout)
            .body(body);
        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("HTTP request failed: {e}");
                self.report_error(
                    &request_id,
                    JsonRpcErrorCode::InternalError,
                    format!("HTTP request failed: {e}"),
                )
                .await;
                return;
            }
        };
        let code = response.status().as_u16();
        tracing::debug!("Received HTTP status {code}");
        if code == status::ACCEPTED {
            return;
        }
        if code == status::NOT_FOUND {
            tracing::warn!("Session not found or expired (404)");
            self.report_error(
                &request_id,
                JsonRpcErrorCode::InvalidRequest,
                "Session terminated".into(),
            )
            .await;
            return;
        }
        if !(200..300).contains(&code) {
            tracing::error!("HTTP error status {code}");
            self.report_error(
                &request_id,
                JsonRpcErrorCode::InvalidRequest,
                format!("HTTP error: {code}"),
            )
            .await;
            return;
        }
        let is_initialization = method.as_deref() == Some(methods::INITIALIZE);
        if is_initialization {
            self.may_extract_session_id(&response);
        }
        let content_type = response
            .headers()
            .get(headers::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        match content_type.as_deref() {
            Some(ct) if ct.contains(headers::CONTENT_TYPE_JSON) => {
                let text = match response.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        self.report_error(
                            &request_id,
                            JsonRpcErrorCode::InternalError,
                            format!("HTTP request failed: {e}"),
                        )
                        .await;
                        return;
                    }
                };
                match parse_message(&text) {
                    Ok(message) => self.deliver(message, is_initialization).await,
                    Err(e) => {
                        tracing::error!("Error parsing JSON response: {e}");
                        self.report_error(
                            &request_id,
                            JsonRpcErrorCode::InternalError,
                            format!("Failed to parse response: {e}"),
                        )
                        .await;
                    }
                }
            }
            Some(ct) if ct.contains(headers::CONTENT_TYPE_SSE) => {
                self.consume_sse(response, is_initialization, true).await;
            }
            other => {
                let shown = other.unwrap_or("<missing>");
                tracing::error!("Unexpected content type: {shown}");
                self.report_error(
                    &request_id,
                    JsonRpcErrorCode::InternalError,
                    format!("Unexpected content type: {shown}"),
                )
                .await;
            }
        }
    }

    /// Read SSE events until the stream ends. When `stop_after_response` is
    /// set the stream is dropped after the first response or error message.
    async fn consume_sse(
        &self,
        response: reqwest::Response,
        is_initialization: bool,
        stop_after_response: bool,
    ) {
        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("SSE body request failed: {e}");
                    return;
                }
            };
            for event in parser.feed(&chunk) {
                if let Some(done) = self.handle_sse_event(event, is_initialization).await {
                    if done && stop_after_response {
                        return;
                    }
                }
            }
        }
        if let Some(event) = parser.finish() {
            self.handle_sse_event(event, is_initialization).await;
        }
    }

    /// Returns `Some(true)` when the event carried a response or error.
    async fn handle_sse_event(
        &self,
        event: ap_jsonrpc::sse::SseEvent,
        is_initialization: bool,
    ) -> Option<bool> {
        let name = event.event.as_deref().unwrap_or("message");
        if name != "message" {
            tracing::warn!("Unknown SSE event: {name}");
            return None;
        }
        match parse_message(&event.data) {
            Ok(message) => {
                let is_response = message.is_response();
                self.deliver(message, is_initialization).await;
                Some(is_response)
            }
            Err(e) => {
                tracing::error!("Error parsing SSE message: {e}");
                None
            }
        }
    }

    fn start_get_stream(&self) {
        if self.session_id().is_empty() {
            tracing::debug!("Session ID is empty while starting get stream");
            return;
        }
        let Some(this) = self.weak_self.upgrade() else {
            return;
        };
        let task = tokio::spawn(async move {
            let Some(client) = this.client() else {
                return;
            };
            let headers = this.prepare_headers();
            let builder = Self::apply_headers(client.get(this.effective_url()), &headers);
            let response = match builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("SSE header request failed: {e}");
                    return;
                }
            };
            let code = response.status().as_u16();
            if !(200..300).contains(&code) {
                tracing::error!("HTTP error status {code}");
                return;
            }
            tracing::debug!("GET SSE connection established");
            this.consume_sse(response, false, false).await;
            tracing::debug!("GET SSE connection ended");
        });
        let mut slot = self.get_stream_task.lock();
        if let Some(old) = slot.replace(task) {
            old.abort();
        }
    }
}

#[async_trait]
impl ClientTransport for StreamableHttpClientTransport {
    async fn connect(&self) -> Result<(), McpError> {
        if !self.tls.server_name.is_empty() {
            // SNI override: connect to the configured host but present server_name.
            if let Ok(u) = url::Url::parse(&self.url) {
                if u.scheme() == "https" {
                    if let Some(host) = u.host_str() {
                        let port = u.port_or_known_default().unwrap_or(443);
                        let mut addrs = tokio::net::lookup_host((host, port))
                            .await
                            .map_err(|e| McpError::Transport(format!("Failed to resolve {host}: {e}")))?;
                        if let Some(addr) = addrs.next() {
                            let client = build_client(
                                &self.tls,
                                self.timeout,
                                Some((self.tls.server_name.clone(), addr)),
                            )?;
                            *self.client.lock() = Some(client);
                        }
                    }
                }
            }
        }
        tracing::info!("HTTP client transport initialized");
        Ok(())
    }

    async fn terminate(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.get_stream_task.lock().take() {
            task.abort();
        }
        self.session_id.lock().clear();
        self.protocol_version.lock().clear();
        *self.callback.write() = None;
    }

    async fn terminate_session(&self, timeout: Duration) {
        if self.session_id().is_empty() {
            tracing::debug!("No session ID set, skipping session termination");
            return;
        }
        let Some(client) = self.client() else {
            return;
        };
        tracing::info!("Terminating session");
        let headers = self.prepare_headers();
        let builder = Self::apply_headers(client.delete(self.effective_url()), &headers).timeout(timeout);
        match builder.send().await {
            Ok(r) => {
                let code = r.status().as_u16();
                if (status::OK..=status::NO_CONTENT).contains(&code) {
                    tracing::info!("Session terminated successfully");
                } else if code == status::METHOD_NOT_ALLOWED {
                    tracing::warn!("Server does not allow session termination");
                } else {
                    tracing::warn!("Session termination returned status: {code}");
                }
            }
            Err(e) => tracing::error!("Session termination failed: {e}"),
        }
    }

    async fn send_message(&self, message: Message) -> Result<(), McpError> {
        if !self.is_running() {
            return Err(McpError::Transport("HttpClientService is not running".into()));
        }
        if let Message::Notification(n) = &message {
            if n.method == methods::NOTIFICATION_INITIALIZED {
                self.start_get_stream();
            }
        }
        let (request_id, method) = match &message {
            Message::Request(r) => (Some(r.id.clone()), Some(r.method.clone())),
            _ => (None, None),
        };
        let body = serialize_message(&message);
        let Some(this) = self.weak_self.upgrade() else {
            return Err(McpError::Transport("transport is gone".into()));
        };
        tokio::spawn(this.do_post(body, request_id, method));
        Ok(())
    }

    fn set_callback(&self, callback: Option<CallbackHandle>) {
        *self.callback.write() = callback;
    }
}

impl std::fmt::Debug for StreamableHttpClientTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamableHttpClientTransport")
            .field("url", &self.url)
            .field("timeout", &self.timeout)
            .field("sse_read_timeout", &self.sse_read_timeout)
            .field("session_id", &self.session_id())
            .field("running", &self.is_running())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};

    fn make(
        url: &str,
        timeout: Duration,
        sse: Duration,
    ) -> Result<Arc<StreamableHttpClientTransport>, McpError> {
        StreamableHttpClientTransport::new(url, HashMap::new(), timeout, sse, TlsConfig::default(), None)
    }

    #[test]
    fn constructor_validates_timeouts() -> TestResult {
        assert!(
            make(
                "http://localhost:99999",
                Duration::from_millis(1),
                Duration::from_millis(2)
            )
            .is_ok()
        );
        assert!(make("", Duration::from_millis(1), Duration::from_millis(2)).is_ok());
        assert!(
            make(
                "https://example.com",
                Duration::from_millis(150),
                Duration::from_millis(150)
            )
            .is_ok()
        );
        let err = make("http://x", Duration::from_millis(0), Duration::from_millis(2)).err_or_fail()?;
        assert_eq!(err.to_string(), "Invalid timeout value");
        let err = make("http://x", Duration::from_millis(1), Duration::from_millis(0)).err_or_fail()?;
        assert_eq!(err.to_string(), "Invalid SSE read timeout value");
        let too_long = Duration::from_millis(MAX_TIMEOUT_MS + 1);
        assert!(make("http://x", too_long, Duration::from_millis(1)).is_err());
        assert!(make("http://x", Duration::from_millis(1), too_long).is_err());
        Ok(())
    }

    #[test]
    fn headers_include_defaults_user_headers_and_session() -> TestResult {
        let mut user = HashMap::new();
        user.insert("Test-Header".to_string(), "Test-Value".to_string());
        let auth: Arc<dyn AuthProvider> = Arc::new(crate::auth::BearerTokenProvider::new("tok"));
        let t = StreamableHttpClientTransport::new(
            "http://localhost:1",
            user,
            Duration::from_millis(10),
            Duration::from_millis(10),
            TlsConfig::default(),
            Some(auth),
        )?;
        *t.session_id.lock() = "sid".into();
        *t.protocol_version.lock() = "2025-06-18".into();
        let h = t.prepare_headers();
        assert_eq!(h.get("accept").required()?, "application/json, text/event-stream");
        assert_eq!(h.get("content-type").required()?, "application/json");
        assert_eq!(h.get("Test-Header").required()?, "Test-Value");
        assert_eq!(h.get("authorization").required()?, "Bearer tok");
        assert_eq!(h.get("mcp-session-id").required()?, "sid");
        assert_eq!(h.get("mcp-protocol-version").required()?, "2025-06-18");
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_connect_terminate() -> TestResult {
        let t = make(
            "http://localhost:99999",
            Duration::from_millis(1),
            Duration::from_millis(2),
        )?;
        t.connect().await?;
        t.connect().await?;
        assert!(t.is_running());
        t.terminate().await;
        t.terminate().await;
        assert!(!t.is_running());
        let err = t
            .send_message(Message::Request(ap_jsonrpc::Request::new(1, "ping", None)))
            .await
            .err_or_fail()?;
        assert_eq!(
            err.to_string(),
            "transport error: HttpClientService is not running"
        );
        t.terminate_session(Duration::from_millis(10)).await;
        Ok(())
    }

    #[test]
    fn effective_url_uses_server_name() -> TestResult {
        let tls = TlsConfig {
            server_name: "sni.example".into(),
            ..Default::default()
        };
        let t = StreamableHttpClientTransport::new(
            "https://127.0.0.1:8443/mcp",
            HashMap::new(),
            Duration::from_millis(10),
            Duration::from_millis(10),
            tls,
            None,
        )?;
        assert_eq!(t.effective_url(), "https://sni.example:8443/mcp");
        let plain = make(
            "http://127.0.0.1:8000/mcp",
            Duration::from_millis(10),
            Duration::from_millis(10),
        )?;
        assert_eq!(plain.effective_url(), "http://127.0.0.1:8000/mcp");
        Ok(())
    }
}
