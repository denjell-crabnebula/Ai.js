// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for HTTP-level tests: spawn an axum server on
//! `127.0.0.1:0` and return its base URL.

use ap_support::testing::TestResult;
use std::sync::Arc;

use a2x_cluster::testing::{FakeAuth, FakeRegistry};
use a2x_cluster::{ClusterConfig, ClusterState, ClusterStore, HttpTransport, router};

/// Serve `app` on an ephemeral port; returns `http://127.0.0.1:<port>`.
pub async fn spawn(app: axum::Router) -> TestResult<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

/// One HTTP-served node backed by a fake registry and the real transport.
pub struct HttpNode {
    pub store: Arc<ClusterStore>,
    pub registry: Arc<FakeRegistry>,
    pub base: String,
}

#[derive(Default)]
pub struct HttpNodeOptions {
    pub config: ClusterConfig,
    pub auth: Option<FakeAuth>,
    pub clock: Option<a2x_cluster::store::Clock>,
}

/// Bind first so the advertise URL is known before the store is built.
pub async fn http_node(dir: &std::path::Path, name: &str, opts: HttpNodeOptions) -> TestResult<HttpNode> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let registry = Arc::new(FakeRegistry::new());
    let mut b = ClusterStore::builder()
        .config(opts.config.clone())
        .registry(registry.clone())
        .transport(Arc::new(HttpTransport::new(opts.config.http_timeout)))
        .advertise(base.clone());
    if let Some(a) = opts.auth {
        b = b.auth(Arc::new(a));
    }
    if let Some(c) = opts.clock {
        b = b.clock(c);
    }
    let state = ClusterState::init_at(Some(name), &dir.join(format!("{name}.json")))?;
    let store = b.build(state);
    let app = router(store.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(HttpNode {
        store,
        registry,
        base,
    })
}

pub fn client() -> TestResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()?)
}

/// Poll `f` until it returns true or the timeout elapses.
pub async fn eventually<F, Fut>(mut f: F, timeout: std::time::Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if f().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    f().await
}
