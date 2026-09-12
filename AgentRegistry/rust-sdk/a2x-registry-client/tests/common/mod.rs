// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! In-process axum mock server shared by the integration tests.
//!
//! Every request is recorded (method, path, query, headers, JSON body) and
//! answered by a responder closure, mirroring the `httpx.MockTransport`
//! handlers used by the Python test suite. The server runs on its own thread
//! with its own runtime so both async and blocking clients can use it.

use ap_support::testing::{OptionExt, TestResult};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::any;
use serde_json::Value;

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: HashMap<String, String>,
    pub body: Option<Value>,
}

impl Recorded {
    pub fn query_get(&self, key: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Response the responder wants sent back.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl MockResponse {
    pub fn json(status: u16, value: Value) -> Self {
        MockResponse {
            status,
            content_type: "application/json".into(),
            body: value.to_string().into_bytes(),
        }
    }

    pub fn raw(status: u16, content_type: &str, body: &[u8]) -> Self {
        MockResponse {
            status,
            content_type: content_type.into(),
            body: body.to_vec(),
        }
    }
}

pub type Responder = Arc<dyn Fn(&Recorded) -> TestResult<MockResponse> + Send + Sync>;

#[derive(Clone)]
struct Shared {
    responder: Responder,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

pub struct MockServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    /// Start a server whose every request is answered by `responder`.
    pub fn start<F>(responder: F) -> TestResult<MockServer>
    where
        F: Fn(&Recorded) -> TestResult<MockResponse> + Send + Sync + 'static,
    {
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            responder: Arc::new(responder),
            requests: Arc::clone(&requests),
        };
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            // A failure here leaves `rx.recv()` below without a sender, which
            // surfaces as a test error.
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
                    return;
                };
                let Ok(addr) = listener.local_addr() else {
                    return;
                };
                if tx.send(format!("http://{addr}")).is_err() {
                    return;
                }
                let app = Router::new().fallback(any(handle)).with_state(shared);
                let _ = axum::serve(listener, app).await;
            });
        });
        let base_url = rx.recv()?;
        Ok(MockServer { base_url, requests })
    }

    /// Start a server that always answers with the same JSON.
    pub fn json(status: u16, value: Value) -> TestResult<MockServer> {
        MockServer::start(move |_| Ok(MockResponse::json(status, value.clone())))
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().clone()
    }

    pub fn last(&self) -> TestResult<Recorded> {
        Ok(self.requests().last().cloned().required()?)
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

async fn handle(State(shared): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap_or_default();
    let query = parts
        .uri
        .query()
        .map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let headers = parts
        .headers
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let recorded = Recorded {
        method: parts.method.as_str().to_string(),
        path: parts.uri.path().to_string(),
        query,
        headers,
        body: serde_json::from_slice(&bytes).ok(),
    };
    let resp = match (shared.responder)(&recorded) {
        Ok(resp) => resp,
        Err(e) => MockResponse::json(500, serde_json::json!({"error": e.to_string()})),
    };
    shared.requests.lock().push(recorded);
    Response::builder()
        .status(resp.status)
        .header("content-type", resp.content_type)
        .body(Body::from(resp.body))
        .unwrap_or_else(|e| {
            axum::response::IntoResponse::into_response((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
            ))
        })
}
