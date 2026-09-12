// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared fixtures for the integration suites, mirroring
//! `tests/conftest.py`: an in-process app rooted at a fresh temp home, an
//! agent card factory and request helpers.

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use a2x_registry::backend::startup::run_warmup;
use a2x_registry::{AppConfig, AppState, build_router};

pub struct TestApp {
    pub tmp: tempfile::TempDir,
    pub state: Arc<AppState>,
    pub router: Router,
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).to_string()
    }
}

/// Boot the app the way the `lite_app` fixture did: fresh home, warmup done.
pub async fn lite_app() -> TestResult<TestApp> {
    let tmp = tempfile::tempdir()?;
    let mut config = AppConfig::with_home(tmp.path());
    config.sweeper_period = std::time::Duration::from_secs(3600);
    let state = AppState::new(config);
    run_warmup(state.clone()).await;
    let router = build_router(state.clone());
    Ok(TestApp { tmp, state, router })
}

impl TestApp {
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> TestResult<Resp> {
        let mut builder = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let req = match body {
            Some(v) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))?,
            None => builder.body(Body::empty())?,
        };
        self.send(req).await
    }

    pub async fn send(&self, req: Request<Body>) -> TestResult<Resp> {
        let resp = self.router.clone().oneshot(req).await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
            .await?
            .to_vec();
        Ok(Resp {
            status,
            headers,
            bytes,
        })
    }

    pub async fn get(&self, path: &str) -> TestResult<Resp> {
        self.request(Method::GET, path, &[], None).await
    }

    pub async fn get_h(&self, path: &str, headers: &[(&str, &str)]) -> TestResult<Resp> {
        self.request(Method::GET, path, headers, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> TestResult<Resp> {
        self.request(Method::POST, path, &[], Some(body)).await
    }

    pub async fn post_h(&self, path: &str, headers: &[(&str, &str)], body: Value) -> TestResult<Resp> {
        self.request(Method::POST, path, headers, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> TestResult<Resp> {
        self.request(Method::PUT, path, &[], Some(body)).await
    }

    pub async fn put_h(&self, path: &str, headers: &[(&str, &str)], body: Value) -> TestResult<Resp> {
        self.request(Method::PUT, path, headers, Some(body)).await
    }

    pub async fn patch_h(&self, path: &str, headers: &[(&str, &str)], body: Value) -> TestResult<Resp> {
        self.request(Method::PATCH, path, headers, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> TestResult<Resp> {
        self.request(Method::DELETE, path, &[], None).await
    }

    pub async fn delete_h(&self, path: &str, headers: &[(&str, &str)]) -> TestResult<Resp> {
        self.request(Method::DELETE, path, headers, None).await
    }

    pub async fn delete_json(&self, path: &str, body: Value) -> TestResult<Resp> {
        self.request(Method::DELETE, path, &[], Some(body)).await
    }

    /// Create a fresh dataset and return its name.
    pub async fn dataset(&self) -> TestResult<String> {
        let name = format!("ds_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let r = self.post("/api/datasets", json!({"name": name})).await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        Ok(name)
    }

    /// Register an A2A card and return the service id.
    pub async fn register(&self, dataset: &str, card: Value) -> TestResult<String> {
        let r = self
            .post(
                &format!("/api/datasets/{dataset}/services/a2a"),
                json!({"agent_card": card, "persistent": true}),
            )
            .await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        Ok(r.json()["service_id"].as_str().required()?.to_string())
    }

    pub fn database_dir(&self) -> std::path::PathBuf {
        self.tmp.path().join("database")
    }
}

/// Minimal but valid A2A agent card.
pub fn agent_card(name: &str) -> Value {
    json!({
        "name": name,
        "description": "tester",
        "url": "http://example.invalid",
        "version": "1.0",
        "protocolVersion": "0.0.1",
        "capabilities": {},
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{"id": "s", "name": "s", "description": "s", "tags": ["t"]}]
    })
}

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}
