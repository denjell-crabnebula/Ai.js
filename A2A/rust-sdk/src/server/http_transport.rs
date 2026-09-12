// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP server transport over axum, the port of `src/transport/http_server_transport.*`
//! and the raw HTTP server of `src/server/http_server.*`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use futures::StreamExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::a2a_log;
use crate::error::A2aServerError;
use crate::log::A2aLogLevel;
use crate::protocol::http::{
    CACHE_CONTROL_NO_CACHE_NO_TRANSFORM, CONNECTION_KEEP_ALIVE, CONTENT_TYPE_JSON, CONTENT_TYPE_SSE,
};
use crate::protocol::{
    AGENT_CARD_ENDPOINT, DEFAULT_JSONRPC_ENDPOINT, EXTENDED_AGENT_CARD_ENDPOINT, METHOD_MESSAGE_STREAM,
    METHOD_TASK_RESUBSCRIBE,
};
use crate::types::A2AError;

use super::builder::HttpConfig;
use super::emitter::{StreamServerEmitter, TransportEmitter};

/// Handler invoked for every JSON-RPC request body.
///
/// It returns the response body for non-streaming requests that did not
/// answer through the emitter, or `None` when the emitter was used.
pub type ServerTransportRpcHandler =
    Arc<dyn Fn(String, Arc<dyn TransportEmitter>) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// Handler that renders an agent card as JSON text.
pub type ServerTransportCardHandler = Arc<dyn Fn() -> String + Send + Sync>;

/// Server side transport abstraction.
#[async_trait]
pub trait ServerTransport: Send + Sync {
    /// Start serving.
    async fn start(&self) -> Result<(), A2aServerError>;
    /// Stop serving.
    async fn stop(&self);
    /// Install the JSON-RPC handler.
    fn set_rpc_handler(&self, handler: ServerTransportRpcHandler);
    /// Install the agent card handler.
    fn set_card_handler(&self, handler: ServerTransportCardHandler);
    /// Install the extended agent card handler.
    fn set_extended_card_handler(&self, handler: ServerTransportCardHandler);
    /// Address the transport listens on once started.
    fn local_addr(&self) -> Option<SocketAddr>;
}

struct RunningServer {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

struct AppState {
    rpc_handler: Option<ServerTransportRpcHandler>,
    card_handler: Option<ServerTransportCardHandler>,
    extended_card_handler: Option<ServerTransportCardHandler>,
    headers: BTreeMap<String, String>,
}

/// HTTP transport serving the JSON-RPC endpoint and the agent card endpoints.
pub struct HttpServerTransport {
    config: HttpConfig,
    jsonrpc_endpoint: String,
    headers: Mutex<BTreeMap<String, String>>,
    bearer_token: Mutex<Option<String>>,
    connect_timeout_ms: Mutex<u64>,
    read_timeout_ms: Mutex<u64>,
    rpc_handler: Mutex<Option<ServerTransportRpcHandler>>,
    card_handler: Mutex<Option<ServerTransportCardHandler>>,
    extended_card_handler: Mutex<Option<ServerTransportCardHandler>>,
    running: tokio::sync::Mutex<Option<RunningServer>>,
}

impl HttpServerTransport {
    /// Create a transport for a configuration.
    pub fn new(config: HttpConfig) -> Self {
        let mut endpoint = config.endpoint.clone();
        if endpoint.is_empty() {
            endpoint = DEFAULT_JSONRPC_ENDPOINT.to_string();
        }
        if !endpoint.starts_with('/') {
            endpoint.insert(0, '/');
        }
        HttpServerTransport {
            config,
            jsonrpc_endpoint: endpoint,
            headers: Mutex::new(BTreeMap::new()),
            bearer_token: Mutex::new(None),
            connect_timeout_ms: Mutex::new(10_000),
            read_timeout_ms: Mutex::new(60_000),
            rpc_handler: Mutex::new(None),
            card_handler: Mutex::new(None),
            extended_card_handler: Mutex::new(None),
            running: tokio::sync::Mutex::new(None),
        }
    }

    /// Add a static response header.
    pub fn set_header(&self, key: &str, value: &str) {
        self.headers.lock().insert(key.to_string(), value.to_string());
    }

    /// Remember a bearer token (kept for API parity; not enforced).
    pub fn set_bearer_token(&self, token: &str) {
        *self.bearer_token.lock() = Some(token.to_string());
    }

    /// Remember timeouts (kept for API parity).
    pub fn set_timeout_ms(&self, connect_ms: u64, read_ms: u64) {
        *self.connect_timeout_ms.lock() = connect_ms;
        *self.read_timeout_ms.lock() = read_ms;
    }

    /// The JSON-RPC endpoint path.
    pub fn jsonrpc_endpoint(&self) -> &str {
        &self.jsonrpc_endpoint
    }

    /// The configuration.
    pub fn config(&self) -> &HttpConfig {
        &self.config
    }

    /// Whether the server is running.
    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }

    /// Whether a request body carries a streaming method.
    pub fn is_streaming_method(req_body: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(req_body) else {
            return false;
        };
        match value.get("method").and_then(|m| m.as_str()) {
            Some(m) => m == METHOD_MESSAGE_STREAM || m == METHOD_TASK_RESUBSCRIBE,
            None => false,
        }
    }

    /// Server does not send data to external URLs.
    pub fn send_data(&self, _url: &str, _data: &str) -> i32 {
        -1
    }

    fn router(&self) -> Router {
        let state = Arc::new(AppState {
            rpc_handler: self.rpc_handler.lock().clone(),
            card_handler: self.card_handler.lock().clone(),
            extended_card_handler: self.extended_card_handler.lock().clone(),
            headers: self.headers.lock().clone(),
        });
        Router::new()
            .route(&self.jsonrpc_endpoint, any(handle_jsonrpc))
            .route(AGENT_CARD_ENDPOINT, get(handle_card))
            .route(EXTENDED_AGENT_CARD_ENDPOINT, get(handle_extended_card))
            .fallback(handle_not_found)
            .with_state(state)
    }
}

fn apply_headers(response: &mut Response, headers: &BTreeMap<String, String>) {
    for (k, v) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(k.as_str()),
            HeaderValue::try_from(v.as_str()),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
}

async fn handle_not_found() -> Response {
    (StatusCode::NOT_FOUND, "Endpoint not found").into_response()
}

async fn handle_card(State(state): State<Arc<AppState>>) -> Response {
    card_response(&state, state.card_handler.as_ref())
}

async fn handle_extended_card(State(state): State<Arc<AppState>>) -> Response {
    card_response(&state, state.extended_card_handler.as_ref())
}

fn card_response(state: &AppState, handler: Option<&ServerTransportCardHandler>) -> Response {
    let Some(handler) = handler else {
        return (StatusCode::NOT_FOUND, "Endpoint not found").into_response();
    };
    let body = handler();
    let mut response = (StatusCode::OK, body).into_response();
    apply_headers(&mut response, &state.headers);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_JSON));
    response
}

async fn handle_jsonrpc(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    body: String,
) -> Response {
    if method == axum::http::Method::GET {
        // Serve the agent card so that `Client::get_card` works against this server.
        return card_response(&state, state.card_handler.as_ref());
    }
    let Some(handler) = state.rpc_handler.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server not started").into_response();
    };
    if HttpServerTransport::is_streaming_method(&body) {
        handle_streaming_request(state, handler, body)
    } else {
        handle_non_streaming_request(state, handler, body).await
    }
}

fn handle_streaming_request(
    state: Arc<AppState>,
    handler: ServerTransportRpcHandler,
    body: String,
) -> Response {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let emitter: Arc<dyn TransportEmitter> = Arc::new(StreamServerEmitter::streaming(tx));
    tokio::spawn(async move {
        let _ = handler(body, emitter).await;
    });
    let stream = UnboundedReceiverStream::new(rx).map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk)));
    let mut response = Response::new(Body::from_stream(stream));
    apply_headers(&mut response, &state.headers);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_SSE));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_NO_CACHE_NO_TRANSFORM),
    );
    headers.insert(
        header::CONNECTION,
        HeaderValue::from_static(CONNECTION_KEEP_ALIVE),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

async fn handle_non_streaming_request(
    state: Arc<AppState>,
    handler: ServerTransportRpcHandler,
    body: String,
) -> Response {
    let (tx, rx) = oneshot::channel::<String>();
    let emitter = Arc::new(StreamServerEmitter::non_streaming(tx));
    let emitter_dyn: Arc<dyn TransportEmitter> = emitter.clone();
    tokio::spawn(async move {
        if let Some(resp) = handler(body, emitter_dyn).await {
            emitter.write_non_streaming_data(&resp);
        }
    });
    let (status, body) = match rx.await {
        Ok(body) => (StatusCode::OK, body),
        Err(_) => {
            let err = A2AError::internal_error();
            let resp = ap_jsonrpc::Response::error(None, err.into());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                resp.to_json().unwrap_or_default(),
            )
        }
    };
    let mut response = (status, body).into_response();
    apply_headers(&mut response, &state.headers);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_JSON));
    response
}

#[async_trait]
impl ServerTransport for HttpServerTransport {
    async fn start(&self) -> Result<(), A2aServerError> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let ip = if self.config.ip.is_empty() {
            "0.0.0.0"
        } else {
            self.config.ip.as_str()
        };
        let bind = format!("{}:{}", ip, self.config.port);
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(|e| A2aServerError::new(format!("Failed to bind {bind}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| A2aServerError::new(format!("Failed to read local address: {e}")))?;
        let app = self.router();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(e) = serve.await {
                a2a_log!(A2aLogLevel::Error, "HTTP server error: {e}");
            }
        });
        a2a_log!(
            A2aLogLevel::Info,
            "JSON-RPC endpoint setup at: {}",
            self.jsonrpc_endpoint
        );
        *running = Some(RunningServer {
            addr,
            shutdown: Some(shutdown_tx),
            join,
        });
        Ok(())
    }

    async fn stop(&self) {
        let server = self.running.lock().await.take();
        let Some(mut server) = server else {
            return;
        };
        if let Some(tx) = server.shutdown.take() {
            let _ = tx.send(());
        }
        if tokio::time::timeout(Duration::from_millis(500), &mut server.join)
            .await
            .is_err()
        {
            server.join.abort();
        }
    }

    fn set_rpc_handler(&self, handler: ServerTransportRpcHandler) {
        *self.rpc_handler.lock() = Some(handler);
    }

    fn set_card_handler(&self, handler: ServerTransportCardHandler) {
        *self.card_handler.lock() = Some(handler);
    }

    fn set_extended_card_handler(&self, handler: ServerTransportCardHandler) {
        *self.extended_card_handler.lock() = Some(handler);
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        self.running
            .try_lock()
            .ok()
            .and_then(|r| r.as_ref().map(|s| s.addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn streaming_method_detection() -> TestResult {
        assert!(HttpServerTransport::is_streaming_method(
            "{\"method\":\"SendStreamingMessage\"}"
        ));
        assert!(HttpServerTransport::is_streaming_method(
            "{\"method\":\"SubscribeToTask\"}"
        ));
        assert!(!HttpServerTransport::is_streaming_method(
            "{\"method\":\"SendMessage\"}"
        ));
        assert!(!HttpServerTransport::is_streaming_method("not json"));
        assert!(!HttpServerTransport::is_streaming_method("{}"));
        Ok(())
    }

    #[test]
    fn endpoint_normalisation_and_setters() -> TestResult {
        let t = HttpServerTransport::new(HttpConfig {
            endpoint: "rpc".into(),
            ..Default::default()
        });
        assert_eq!(t.jsonrpc_endpoint(), "/rpc");
        let t = HttpServerTransport::new(HttpConfig {
            endpoint: String::new(),
            ..Default::default()
        });
        assert_eq!(t.jsonrpc_endpoint(), "/jsonrpc");
        t.set_header("X-Test", "1");
        t.set_bearer_token("tok");
        t.set_timeout_ms(1, 2);
        assert_eq!(t.send_data("http://x", "d"), -1);
        assert!(t.local_addr().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn start_stop_cycle() -> TestResult {
        let t = HttpServerTransport::new(HttpConfig {
            ip: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        });
        t.stop().await;
        t.start().await?;
        assert!(t.is_running().await);
        assert!(t.local_addr().is_some());
        t.start().await?;
        t.stop().await;
        assert!(!t.is_running().await);
        t.stop().await;
        t.start().await?;
        t.stop().await;
        Ok(())
    }
}
