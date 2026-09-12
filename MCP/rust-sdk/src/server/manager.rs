// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server manager, port of `src/server/server_manager.*`,
//! `http_server_manager.*` and the HTTP layer of `http_server.*` on top of
//! `axum` and `axum-server`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::any;
use futures_util::StreamExt;
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::session::{IncomingRequestHandler, ServerSession};
use crate::error::McpError;
use crate::protocol::{JSONRPC_VERSION, headers, parse_server_endpoint, status};
use crate::transport::stdio::{BoxedReader, BoxedWriter};
use crate::transport::{
    HttpReply, HttpRequest, HttpResponse, ServerTransport, StdioServerTransport,
    StreamableHttpServerTransport, lowercase_headers,
};
use crate::types::{ServerConfig, StreamableHttpServerConfig, TlsConfig};

struct HttpSessionEntry {
    transport: Arc<StreamableHttpServerTransport>,
    session: Arc<ServerSession>,
}

struct HttpServerHandle {
    handle: axum_server::Handle,
    task: JoinHandle<()>,
}

/// Owns sessions and the listening transport of one server.
pub struct ServerManager {
    config: ServerConfig,
    transport_config: Option<StreamableHttpServerConfig>,
    handler: Arc<dyn IncomingRequestHandler>,
    sessions: Mutex<HashMap<String, HttpSessionEntry>>,
    stdio: Mutex<Option<(Arc<StdioServerTransport>, Arc<ServerSession>)>>,
    stdio_streams: Mutex<Option<(BoxedReader, BoxedWriter)>>,
    http: Mutex<Option<HttpServerHandle>>,
    local_addr: Mutex<Option<SocketAddr>>,
}

/// Generate a version 4 UUID session id.
pub fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl ServerManager {
    /// Create a manager. `transport_config` is `None` for stdio.
    pub fn new(
        config: ServerConfig,
        transport_config: Option<StreamableHttpServerConfig>,
        handler: Arc<dyn IncomingRequestHandler>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            transport_config,
            handler,
            sessions: Mutex::new(HashMap::new()),
            stdio: Mutex::new(None),
            stdio_streams: Mutex::new(None),
            http: Mutex::new(None),
            local_addr: Mutex::new(None),
        })
    }

    /// True for a stdio server.
    pub fn is_stdio(&self) -> bool {
        self.transport_config.is_none()
    }

    /// Use explicit streams instead of the process stdio (tests).
    pub fn set_stdio_streams(&self, reader: BoxedReader, writer: BoxedWriter) {
        *self.stdio_streams.lock() = Some((reader, writer));
    }

    /// Address the HTTP server listens on, once started.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        *self.local_addr.lock()
    }

    /// Number of live HTTP sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.lock().len()
    }

    /// The session with the given id.
    pub fn get_session(&self, session_id: &str) -> Option<Arc<ServerSession>> {
        self.sessions.lock().get(session_id).map(|e| e.session.clone())
    }

    /// The stdio session, once started.
    pub fn stdio_session(&self) -> Option<Arc<ServerSession>> {
        self.stdio.lock().as_ref().map(|(_, s)| s.clone())
    }

    /// Start listening.
    pub async fn start(self: &Arc<Self>) -> Result<(), McpError> {
        match &self.transport_config {
            None => self.start_stdio().await,
            Some(cfg) => self.start_http(cfg.clone()).await,
        }
    }

    async fn start_stdio(&self) -> Result<(), McpError> {
        let streams = self.stdio_streams.lock().take();
        let transport = match streams {
            Some((r, w)) => StdioServerTransport::with_streams(r, w),
            None => StdioServerTransport::new(),
        };
        let dyn_transport: Arc<dyn ServerTransport> = transport.clone();
        let session = ServerSession::new(
            Some(dyn_transport),
            self.config.clone(),
            generate_session_id(),
            false,
        );
        session.set_server_capabilities(self.config.capabilities.clone());
        session.set_incoming_request_handler(self.handler.clone());
        transport.listen().await?;
        *self.stdio.lock() = Some((transport, session));
        Ok(())
    }

    async fn start_http(self: &Arc<Self>, cfg: StreamableHttpServerConfig) -> Result<(), McpError> {
        let endpoint = parse_server_endpoint(&cfg.endpoint).map_err(McpError::InvalidState)?;
        tracing::info!(
            "Parsed endpoint - Host: {}, Port: {}, URI: {}",
            endpoint.host,
            endpoint.port,
            endpoint.path
        );
        let bind = format!("{}:{}", endpoint.host, endpoint.port);
        let mut addrs = tokio::net::lookup_host(bind.as_str())
            .await
            .map_err(|e| McpError::Transport(format!("Failed to resolve {bind}: {e}")))?;
        let addr = addrs
            .next()
            .ok_or_else(|| McpError::Transport(format!("Failed to resolve {bind}")))?;
        let listener = std::net::TcpListener::bind(addr)
            .map_err(|e| McpError::Transport(format!("Failed to bind {addr}: {e}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| McpError::Transport(format!("Failed to configure listener: {e}")))?;
        let local = listener
            .local_addr()
            .map_err(|e| McpError::Transport(format!("Failed to read listener address: {e}")))?;

        let path = if endpoint.path.is_empty() {
            "/".to_string()
        } else {
            endpoint.path.clone()
        };
        let app = Router::new()
            .route(&path, any(handle_http))
            .fallback(|| async { (StatusCode::NOT_FOUND, "Endpoint not found") })
            .layer(DefaultBodyLimit::disable())
            .with_state(self.clone());

        let handle = axum_server::Handle::new();
        let task = if cfg.tls_config.enabled {
            let rustls_config = build_tls_config(&cfg.tls_config)?;
            let server = axum_server::from_tcp_rustls(listener, rustls_config).handle(handle.clone());
            tokio::spawn(async move {
                if let Err(e) = server.serve(app.into_make_service()).await {
                    tracing::error!("HTTPS server stopped: {e}");
                }
            })
        } else {
            let server = axum_server::from_tcp(listener).handle(handle.clone());
            tokio::spawn(async move {
                if let Err(e) = server.serve(app.into_make_service()).await {
                    tracing::error!("HTTP server stopped: {e}");
                }
            })
        };
        *self.local_addr.lock() = Some(local);
        *self.http.lock() = Some(HttpServerHandle { handle, task });
        tracing::info!("HTTP server manager started successfully on {local}");
        Ok(())
    }

    /// Stop listening and drop every session.
    pub async fn stop(&self) {
        let stdio = self.stdio.lock().take();
        if let Some((transport, _session)) = stdio {
            transport.terminate().await;
        }
        let http = self.http.lock().take();
        if let Some(HttpServerHandle { handle, task }) = http {
            handle.graceful_shutdown(Some(Duration::from_millis(200)));
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        let entries: Vec<HttpSessionEntry> = self.sessions.lock().drain().map(|(_, e)| e).collect();
        for entry in entries {
            entry.transport.terminate().await;
        }
        *self.local_addr.lock() = None;
    }

    fn new_http_session(&self, session_id: &str, stateless: bool) -> Result<HttpSessionEntry, McpError> {
        let cfg = self
            .transport_config
            .as_ref()
            .ok_or_else(|| McpError::state("HTTP transport is not configured"))?;
        let transport_id = if stateless { "" } else { session_id };
        let transport =
            StreamableHttpServerTransport::new(transport_id, cfg.is_json_response_enabled, stateless)?;
        let dyn_transport: Arc<dyn ServerTransport> = transport.clone();
        let session = ServerSession::new(Some(dyn_transport), self.config.clone(), session_id, stateless);
        session.set_server_capabilities(self.config.capabilities.clone());
        session.set_incoming_request_handler(self.handler.clone());
        Ok(HttpSessionEntry { transport, session })
    }

    fn auth_error(status_code: u16, text: &str, message: &str) -> HttpReply {
        let body = serde_json::json!({"error": message}).to_string();
        HttpReply::Full(
            HttpResponse::new(status_code)
                .header(headers::CONTENT_TYPE, headers::CONTENT_TYPE_JSON)
                .header("x-status-text", text)
                .body(body),
        )
    }

    /// Handle one HTTP request: authentication, session lookup and transport dispatch.
    pub async fn handle_request(&self, request: HttpRequest) -> HttpReply {
        let Some(cfg) = &self.transport_config else {
            return HttpReply::Full(HttpResponse::new(status::NOT_FOUND).body("Endpoint not found"));
        };
        if let Some(authenticator) = &cfg.authenticator {
            let result = authenticator.authenticate(&request.headers);
            if !result.authenticated {
                let description = result
                    .error_description
                    .clone()
                    .unwrap_or_else(|| "Unknown error".into());
                tracing::warn!("Authentication failed: {description}");
                let message = result
                    .error_description
                    .unwrap_or_else(|| "Authentication failed".into());
                return Self::auth_error(status::UNAUTHORIZED, "Unauthorized", &message);
            }
            if let Some(authorizer) = &cfg.authorizer {
                if !authorizer.authorize(&result) {
                    tracing::warn!("Authorization failed: insufficient scopes");
                    return Self::auth_error(status::FORBIDDEN, "Forbidden", "Insufficient permissions");
                }
            }
            tracing::debug!("Authentication and authorization successful");
        }

        let request_session_id = request.header(headers::MCP_SESSION_ID).unwrap_or("").to_string();

        if cfg.stateless {
            let entry = match self.new_http_session(&request_session_id, true) {
                Ok(e) => e,
                Err(e) => return internal_error(&e),
            };
            let reply = entry.transport.handle_request(request).await;
            return keep_alive(entry, reply);
        }

        let (entry, created) = if request_session_id.is_empty() {
            let id = generate_session_id();
            tracing::info!("Creating new session {id}");
            match self.new_http_session(&id, false) {
                Ok(e) => (e, true),
                Err(e) => return internal_error(&e),
            }
        } else {
            let existing = self
                .sessions
                .lock()
                .get(&request_session_id)
                .map(|e| HttpSessionEntry {
                    transport: e.transport.clone(),
                    session: e.session.clone(),
                });
            match existing {
                Some(e) => (e, false),
                None => {
                    let body = serde_json::json!({
                        "jsonrpc": JSONRPC_VERSION,
                        "id": "server-error",
                        "error": {"code": -32600, "message": "Not Found: Invalid or expired session ID"},
                    });
                    return HttpReply::Full(
                        HttpResponse::new(status::NOT_FOUND)
                            .header(headers::CONTENT_TYPE, headers::CONTENT_TYPE_JSON)
                            .body(body.to_string()),
                    );
                }
            }
        };
        if created {
            self.sessions.lock().insert(
                entry.session.session_id().to_string(),
                HttpSessionEntry {
                    transport: entry.transport.clone(),
                    session: entry.session.clone(),
                },
            );
        }
        let reply = entry.transport.handle_request(request).await;
        let rejected = matches!(&reply, HttpReply::Full(r) if r.status >= 400);
        if entry.transport.is_terminated() || (created && rejected) {
            // Drop terminated sessions and sessions whose very first request was rejected.
            self.sessions.lock().remove(entry.session.session_id());
        }
        reply
    }
}

/// Keep a stateless session alive until its reply has been produced.
fn keep_alive(entry: HttpSessionEntry, reply: HttpReply) -> HttpReply {
    match reply {
        HttpReply::Deferred(rx) => {
            let (tx, new_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _keep = entry;
                if let Ok(response) = rx.await {
                    let _ = tx.send(response);
                }
            });
            HttpReply::Deferred(new_rx)
        }
        HttpReply::Stream {
            status,
            headers,
            body,
        } => {
            let (tx, new_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _keep = entry;
                let mut body = body;
                while let Some(frame) = body.recv().await {
                    let done = frame.is_none();
                    if tx.send(frame).is_err() || done {
                        break;
                    }
                }
            });
            HttpReply::Stream {
                status,
                headers,
                body: new_rx,
            }
        }
        full => full,
    }
}

fn internal_error(e: &McpError) -> HttpReply {
    HttpReply::Full(
        HttpResponse::new(status::INTERNAL_SERVER_ERROR)
            .header(headers::CONTENT_TYPE, headers::CONTENT_TYPE_TEXT_PLAIN)
            .body(format!("Error: {e}")),
    )
}

async fn handle_http(State(manager): State<Arc<ServerManager>>, request: axum::extract::Request) -> Response {
    let method = request.method().as_str().to_uppercase();
    let url = request.uri().path().to_string();
    let header_map = lowercase_headers(request.headers());
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            return simple_response(status::BAD_REQUEST, &format!("Failed to read request body: {e}"));
        }
    };
    let body = String::from_utf8_lossy(&body_bytes).to_string();
    let http_request = HttpRequest {
        method,
        url,
        headers: header_map,
        body,
    };
    let reply = manager.handle_request(http_request).await;
    reply_to_response(reply).await
}

fn simple_response(status_code: u16, body: &str) -> Response {
    Response::builder()
        .status(status_code)
        .header(headers::CONTENT_TYPE, headers::CONTENT_TYPE_TEXT_PLAIN)
        .body(Body::from(body.to_string()))
        .unwrap_or_default()
}

fn build_response(status_code: u16, header_list: &[(String, String)], body: Body) -> Response {
    let mut builder = Response::builder().status(status_code);
    for (name, value) in header_list {
        if name == "x-status-text" {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            builder = builder.header(n, v);
        }
    }
    builder.body(body).unwrap_or_default()
}

/// Convert a transport reply into an axum response.
pub async fn reply_to_response(reply: HttpReply) -> Response {
    match reply {
        HttpReply::Full(r) => build_response(r.status, &r.headers, Body::from(r.body)),
        HttpReply::Deferred(rx) => match rx.await {
            Ok(r) => build_response(r.status, &r.headers, Body::from(r.body)),
            Err(_) => simple_response(status::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        },
        HttpReply::Stream {
            status,
            headers,
            body,
        } => {
            let stream = UnboundedReceiverStream::new(body)
                .take_while(|frame| futures_util::future::ready(frame.is_some()))
                .map(|frame| Ok::<Bytes, Infallible>(frame.unwrap_or_default()));
            build_response(status, &headers, Body::from_stream(stream))
        }
    }
}

/// Build the rustls server configuration from a [`TlsConfig`].
pub fn build_tls_config(tls: &TlsConfig) -> Result<axum_server::tls_rustls::RustlsConfig, McpError> {
    use std::fs::File;
    use std::io::BufReader;

    let cert_file = File::open(&tls.cert_file)
        .map_err(|e| McpError::Transport(format!("Failed to open TLS certificate {}: {e}", tls.cert_file)))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| McpError::Transport(format!("Invalid TLS certificate {}: {e}", tls.cert_file)))?;
    let key_file = File::open(&tls.key_file)
        .map_err(|e| McpError::Transport(format!("Failed to open TLS key {}: {e}", tls.key_file)))?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| McpError::Transport(format!("Invalid TLS key {}: {e}", tls.key_file)))?
        .ok_or_else(|| McpError::Transport(format!("No private key found in {}", tls.key_file)))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| McpError::Transport(format!("TLS configuration failed: {e}")))?;
    let builder = if tls.ca_file.is_empty() {
        builder.with_no_client_auth()
    } else {
        let ca_file = File::open(&tls.ca_file)
            .map_err(|e| McpError::Transport(format!("Failed to open TLS CA file {}: {e}", tls.ca_file)))?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut BufReader::new(ca_file)) {
            let cert =
                cert.map_err(|e| McpError::Transport(format!("Invalid TLS CA file {}: {e}", tls.ca_file)))?;
            roots
                .add(cert)
                .map_err(|e| McpError::Transport(format!("Invalid TLS CA certificate: {e}")))?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|e| McpError::Transport(format!("TLS client verifier failed: {e}")))?;
        builder.with_client_cert_verifier(verifier)
    };
    let mut config = builder
        .with_single_cert(certs, key)
        .map_err(|e| McpError::Transport(format!("TLS certificate rejected: {e}")))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(
        config,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn session_ids_are_uuids() -> TestResult {
        let id = generate_session_id();
        assert_eq!(id.len(), 36);
        assert_ne!(id, generate_session_id());
        Ok(())
    }

    #[test]
    fn tls_config_requires_files() -> TestResult {
        let tls = TlsConfig {
            enabled: true,
            cert_file: "/nonexistent/cert.pem".into(),
            key_file: "/nonexistent/key.pem".into(),
            ..Default::default()
        };
        assert!(build_tls_config(&tls).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn reply_conversion() -> TestResult {
        let full = reply_to_response(HttpReply::Full(
            HttpResponse::new(202).header("x-a", "b").body("ok"),
        ))
        .await;
        assert_eq!(full.status(), 202);
        assert_eq!(full.headers().get("x-a").required()?, "b");
        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(tx);
        let failed = reply_to_response(HttpReply::Deferred(rx)).await;
        assert_eq!(failed.status(), 500);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Some(Bytes::from_static(b"data: x\n\n")))?;
        tx.send(None)?;
        let streamed = reply_to_response(HttpReply::Stream {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: rx,
        })
        .await;
        assert_eq!(streamed.status(), 200);
        let bytes = axum::body::to_bytes(streamed.into_body(), usize::MAX).await?;
        assert_eq!(&bytes[..], b"data: x\n\n");
        Ok(())
    }
}
