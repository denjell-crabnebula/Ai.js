// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Minimal local HTTP server for A4P endpoints, built on axum.
//!
//! All endpoints are JSON `POST`. Non-POST requests get 405, unknown paths
//! 404, invalid JSON or validation errors 400, stable protocol conflicts their
//! own status (409) and unexpected failures 500.

use ap_support::env::EnvSource;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::errors::A4PError;
use crate::server::A4PServer;
use crate::types::{JsonDict, to_payload};

/// Default host when `A4P_SERVER_HOST` is unset.
pub const DEFAULT_A4P_HTTP_HOST: &str = "127.0.0.1";
/// Default port when `A4P_SERVER_PORT` is unset.
pub const DEFAULT_A4P_HTTP_PORT: u16 = 8961;

/// Return `A4P_SERVER_HOST` or `127.0.0.1`.
pub fn a4p_http_host() -> String {
    a4p_http_host_in(ap_support::env::current())
}

/// [`a4p_http_host`] reading from `env`.
pub fn a4p_http_host_in(env: &dyn EnvSource) -> String {
    env.get("A4P_SERVER_HOST")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_A4P_HTTP_HOST.to_string())
        .trim()
        .to_string()
}

/// Return `A4P_SERVER_PORT` validated as 1..=65535, or 8961.
pub fn a4p_http_port() -> Result<u16, A4PError> {
    a4p_http_port_in(ap_support::env::current())
}

/// [`a4p_http_port`] reading from `env`.
pub fn a4p_http_port_in(env: &dyn EnvSource) -> Result<u16, A4PError> {
    let raw = env.get("A4P_SERVER_PORT").unwrap_or_default().trim().to_string();
    if raw.is_empty() {
        return Ok(DEFAULT_A4P_HTTP_PORT);
    }
    let port: i64 = raw
        .parse()
        .map_err(|_| A4PError::value(format!("A4P_SERVER_PORT must be an integer, got '{raw}'")))?;
    if !(1..=65535).contains(&port) {
        return Err(A4PError::value(format!(
            "A4P_SERVER_PORT must be between 1 and 65535, got {port}"
        )));
    }
    Ok(port as u16)
}

struct Running {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// HTTP transport around an [`A4PServer`].
pub struct A4PHTTPServer {
    a4p_server: Arc<A4PServer>,
    host: String,
    port: Mutex<u16>,
    running: Mutex<Option<Running>>,
}

impl A4PHTTPServer {
    /// Create the transport. `host` and `port` default to the environment configuration.
    ///
    /// Port 0 binds an ephemeral port that `port()` reports after `start()`.
    pub fn new(a4p_server: Arc<A4PServer>, host: Option<&str>, port: Option<u16>) -> Result<Self, A4PError> {
        let host = host
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(a4p_http_host);
        let port = match port {
            Some(port) => port,
            None => a4p_http_port()?,
        };
        Ok(Self {
            a4p_server,
            host,
            port: Mutex::new(port),
            running: Mutex::new(None),
        })
    }

    /// The bind host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The configured port, or the bound port after `start()`.
    pub fn port(&self) -> u16 {
        *self.port.lock()
    }

    /// The wrapped A4P server.
    pub fn a4p_server(&self) -> &Arc<A4PServer> {
        &self.a4p_server
    }

    /// Build an axum router serving the eight A4P endpoints.
    pub fn router(a4p_server: Arc<A4PServer>) -> Router {
        Router::new().fallback(handle_request).with_state(a4p_server)
    }

    /// Start listening. Calling it twice is a no-op.
    pub async fn start(&self) -> Result<(), A4PError> {
        if self.running.lock().is_some() {
            return Ok(());
        }
        let address = format!("{}:{}", self.host, self.port());
        let listener = tokio::net::TcpListener::bind(&address)
            .await
            .map_err(|error| A4PError::runtime(format!("Cannot bind {address}: {error}")))?;
        let local: SocketAddr = listener
            .local_addr()
            .map_err(|error| A4PError::runtime(format!("Cannot read bound address: {error}")))?;
        *self.port.lock() = local.port();
        let router = Self::router(self.a4p_server.clone());
        let (shutdown, wait) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, router).with_graceful_shutdown(async {
                let _ = wait.await;
            });
            if let Err(error) = server.await {
                tracing::error!("[A4PHTTPServer] serve failed: {error}");
            }
        });
        *self.running.lock() = Some(Running { shutdown, task });
        tracing::info!("[A4PHTTPServer] started: http://{}:{}", self.host, local.port());
        Ok(())
    }

    /// Stop listening. Calling it when stopped is a no-op.
    pub async fn stop(&self) {
        let running = self.running.lock().take();
        if let Some(running) = running {
            let _ = running.shutdown.send(());
            let _ = running.task.await;
            tracing::info!("[A4PHTTPServer] stopped");
        }
    }

    /// Route a JSON payload to the A4P server and return `(status, body)`.
    pub async fn dispatch(&self, path: &str, payload: JsonDict) -> (u16, JsonDict) {
        dispatch(&self.a4p_server, path, payload).await
    }
}

fn error_body(error: &str, message: Option<String>) -> JsonDict {
    let mut body = JsonDict::new();
    body.insert("error".into(), Value::String(error.into()));
    if let Some(message) = message {
        body.insert("message".into(), Value::String(message));
    }
    body
}

fn map_error(error: A4PError) -> (u16, JsonDict) {
    match error {
        A4PError::Protocol(protocol) => (
            protocol.http_status,
            error_body(&protocol.code, Some(protocol.message)),
        ),
        other if other.is_value_error() => (400, error_body("bad_request", Some(other.to_string()))),
        other => {
            tracing::error!("[A4PHTTPServer] request failed: {other}");
            (500, error_body("internal_error", Some(other.to_string())))
        }
    }
}

/// Route a JSON payload to an [`A4PServer`] and return `(status, body)`.
pub async fn dispatch(server: &A4PServer, path: &str, payload: JsonDict) -> (u16, JsonDict) {
    let result: Result<JsonDict, A4PError> = match path {
        "/a4p/v1/user-credentials/ed25519/register" => server.register_ed25519_credential(&payload),
        "/a4p/v1/user-credentials/webauthn/register/options" => {
            server.webauthn_registration_options(&payload)
        }
        "/a4p/v1/user-credentials/webauthn/register/verify" => server.verify_webauthn_registration(&payload),
        "/a4p/v1/intent-authorizations/prepare" => server
            .prepare_intent_authorization(payload)
            .await
            .map(|response| to_payload(&response)),
        "/a4p/v1/intent-authorizations/complete" => server
            .complete_intent_authorization(payload)
            .await
            .map(|response| to_payload(&response)),
        "/a4p/v1/intent-tokens/verify" => server
            .verify_intent_token(payload)
            .await
            .map(|response| to_payload(&response)),
        "/a4p/v1/operation-authorizations/prepare" => server
            .prepare_operation_authorization(payload)
            .await
            .map(|response| to_payload(&response)),
        "/a4p/v1/operation-authorizations/complete" => server
            .complete_operation_authorization(payload)
            .await
            .map(|response| to_payload(&response)),
        _ => return (404, error_body("not_found", None)),
    };
    match result {
        Ok(body) => (200, body),
        Err(error) => map_error(error),
    }
}

fn json_response(status: u16, body: JsonDict) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let text = serde_json::to_string(&Value::Object(body)).unwrap_or_else(|_| "{}".to_string());
    (
        status,
        [("Content-Type", "application/json; charset=utf-8")],
        text,
    )
        .into_response()
}

async fn handle_request(
    State(server): State<Arc<A4PServer>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    if method != Method::POST {
        return json_response(405, error_body("method_not_allowed", None));
    }
    let text = match std::str::from_utf8(&body) {
        Ok(text) => text,
        Err(error) => return json_response(400, error_body("bad_request", Some(error.to_string()))),
    };
    let text = if text.is_empty() { "{}" } else { text };
    let payload = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map,
        Ok(_) => JsonDict::new(),
        Err(error) => return json_response(400, error_body("bad_request", Some(error.to_string()))),
    };
    let (status, response) = dispatch(&server, uri.path(), payload).await;
    json_response(status, response)
}
