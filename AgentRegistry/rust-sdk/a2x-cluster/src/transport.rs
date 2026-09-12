// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Peer transport: the RPC boundary between registry instances.
//!
//! [`Transport`] has one method per cluster RPC. [`HttpTransport`] is the
//! production implementation over the peer's `/api/cluster/*` endpoints;
//! tests use [`crate::testing::InProcessTransport`], which routes calls
//! straight to the target store's handler methods.
//!
//! Every method takes the peer's base `address` (for example
//! `http://host:port`) and returns parsed JSON. Failures surface as
//! [`TransportError`] so callers can treat an unreachable peer as best-effort.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{Key, SyncEnvelope, Version};
use crate::membership::MembershipRecord;

/// Header carrying the per-session secret on every authenticated RPC.
pub const SESSION_HEADER: &str = "X-Cluster-Session";

/// Keep-alive socket pool ceiling per host. A full mesh holds about N-1
/// connections per node; reusing them is essential at scale.
const MAX_IDLE_PER_HOST: usize = 512;

/// A peer call failed (unreachable, non-2xx or undecodable body).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct TransportError(pub String);

impl TransportError {
    pub fn new(msg: impl Into<String>) -> Self {
        TransportError(msg.into())
    }
}

/// Body of `POST /api/cluster/sessions`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub node_id: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// Response of `POST /api/cluster/sessions`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenResponse {
    pub node_id: String,
    #[serde(default)]
    pub accepted: Vec<String>,
    #[serde(default)]
    pub ephemeral: Vec<String>,
    #[serde(default)]
    pub session_token: Option<String>,
}

/// One digest row `[dataset, origin_id, service_id, [ms, node_id]]`.
pub type DigestRow = (String, String, String, Version);

/// Response of `POST /api/cluster/updates`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatesResponse {
    #[serde(default)]
    pub accepted: usize,
    #[serde(default)]
    pub received: usize,
    #[serde(default)]
    pub rejected: usize,
}

/// `{"ok": bool}` responses (keepalive, evicted, leave).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkResponse {
    #[serde(default)]
    pub ok: bool,
}

/// Body of `POST /api/cluster/join`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JoinRequest {
    #[serde(default)]
    pub cluster_id: Option<String>,
    #[serde(default)]
    pub roster: Vec<Value>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub from_node: String,
    #[serde(default)]
    pub from_address: String,
}

/// Response of `POST /api/cluster/join`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JoinResponse {
    #[serde(default)]
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Body of `POST /api/cluster/evicted`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvictRequest {
    #[serde(default)]
    pub from_node: String,
    #[serde(default)]
    pub cluster_id: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// Body of `POST /api/cluster/leave`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LeaveRequest {
    #[serde(default)]
    pub from_node: String,
    #[serde(default)]
    pub token: Option<String>,
}

/// Response of `POST /api/cluster/set/sync`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSyncResponse {
    #[serde(default)]
    pub accepted: bool,
}

/// One method per cluster RPC. `token` is the per-session secret (`None`
/// on open clusters).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Release pooled connections on shutdown. Default: no-op.
    fn close(&self) {}

    async fn open(&self, address: &str, body: &OpenRequest) -> Result<OpenResponse, TransportError>;

    async fn merkle(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
    ) -> Result<BTreeMap<String, String>, TransportError>;

    async fn digest(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
        buckets: Option<&[u32]>,
    ) -> Result<Vec<DigestRow>, TransportError>;

    async fn pull(
        &self,
        address: &str,
        from_node: &str,
        keys: &[Key],
        token: Option<&str>,
    ) -> Result<Vec<SyncEnvelope>, TransportError>;

    async fn updates(
        &self,
        address: &str,
        from_node: &str,
        envelopes: &[SyncEnvelope],
        token: Option<&str>,
    ) -> Result<UpdatesResponse, TransportError>;

    async fn keepalive(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<OkResponse, TransportError>;

    // Membership control plane.

    /// Pull a node into a cluster. Carries `cluster_id` plus the roster.
    async fn join(&self, address: &str, body: &JoinRequest) -> Result<JoinResponse, TransportError>;

    /// Notify a node it has been removed from the cluster.
    async fn evict(&self, address: &str, body: &EvictRequest) -> Result<OkResponse, TransportError>;

    /// Notify a peer that the sender is gracefully leaving.
    async fn evict_self(&self, address: &str, body: &LeaveRequest) -> Result<OkResponse, TransportError>;

    /// Fetch a peer's roster version map (anti-entropy).
    async fn set_digest(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<BTreeMap<String, Version>, TransportError>;

    /// Fetch full membership records by node id (anti-entropy).
    async fn set_pull(
        &self,
        address: &str,
        from_node: &str,
        node_ids: &[String],
        token: Option<&str>,
    ) -> Result<Vec<Value>, TransportError>;

    /// Push membership records to a peer for LWW merge (immediate push).
    async fn set_sync(
        &self,
        address: &str,
        from_node: &str,
        records: &[MembershipRecord],
        token: Option<&str>,
    ) -> Result<SetSyncResponse, TransportError>;
}

/// Production transport over the peer's REST endpoints.
///
/// Holds one long-lived `reqwest::Client` with a keep-alive pool reused
/// across calls, and ignores system proxies so localhost cannot be
/// intercepted.
#[derive(Debug)]
pub struct HttpTransport {
    client: reqwest::Client,
    closed: AtomicBool,
}

impl HttpTransport {
    /// Build a pooled client with the given per-request timeout (seconds).
    pub fn new(timeout: f64) -> Self {
        let timeout = if timeout.is_finite() && timeout > 0.0 {
            Duration::from_secs_f64(timeout)
        } else {
            Duration::from_secs(5)
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(timeout)
            .pool_max_idle_per_host(MAX_IDLE_PER_HOST)
            .build()
            .unwrap_or_default();
        HttpTransport {
            client,
            closed: AtomicBool::new(false),
        }
    }

    /// Mark the transport closed. Idempotent; later calls fail with
    /// `transport closed`.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        address: &str,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        query: Option<&[(&str, String)]>,
        json: Option<&Value>,
    ) -> Result<T, TransportError> {
        if self.is_closed() {
            return Err(TransportError::new("transport closed"));
        }
        let url = format!("{}{}", address.trim_end_matches('/'), path);
        let mut req = self.client.request(method.clone(), &url);
        if let Some(t) = token {
            req = req.header(SESSION_HEADER, t);
        }
        if let Some(q) = query {
            req = req.query(q);
        }
        if let Some(body) = json {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::new(format!("{method} {url} failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(TransportError::new(format!(
                "{method} {url} -> {}: {text}",
                status.as_u16()
            )));
        }
        resp.json::<T>()
            .await
            .map_err(|e| TransportError::new(format!("{method} {url} bad response: {e}")))
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, TransportError> {
    serde_json::to_value(v).map_err(|e| TransportError::new(format!("encode failed: {e}")))
}

#[async_trait]
impl Transport for HttpTransport {
    fn close(&self) {
        HttpTransport::close(self);
    }

    async fn open(&self, address: &str, body: &OpenRequest) -> Result<OpenResponse, TransportError> {
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/sessions",
            None,
            None,
            Some(&to_value(body)?),
        )
        .await
    }

    async fn merkle(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
    ) -> Result<BTreeMap<String, String>, TransportError> {
        let q = [
            ("from_node", from_node.to_string()),
            ("namespaces", namespaces.join(",")),
        ];
        self.call(
            address,
            reqwest::Method::GET,
            "/api/cluster/merkle",
            token,
            Some(&q),
            None,
        )
        .await
    }

    async fn digest(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
        buckets: Option<&[u32]>,
    ) -> Result<Vec<DigestRow>, TransportError> {
        let mut q = vec![
            ("from_node", from_node.to_string()),
            ("namespaces", namespaces.join(",")),
        ];
        if let Some(b) = buckets {
            let joined = b.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            q.push(("buckets", joined));
        }
        self.call(
            address,
            reqwest::Method::GET,
            "/api/cluster/digest",
            token,
            Some(&q),
            None,
        )
        .await
    }

    async fn pull(
        &self,
        address: &str,
        from_node: &str,
        keys: &[Key],
        token: Option<&str>,
    ) -> Result<Vec<SyncEnvelope>, TransportError> {
        let body = serde_json::json!({"from_node": from_node, "keys": keys});
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/pulls",
            token,
            None,
            Some(&body),
        )
        .await
    }

    async fn updates(
        &self,
        address: &str,
        from_node: &str,
        envelopes: &[SyncEnvelope],
        token: Option<&str>,
    ) -> Result<UpdatesResponse, TransportError> {
        let body = serde_json::json!({"from_node": from_node, "envelopes": envelopes});
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/updates",
            token,
            None,
            Some(&body),
        )
        .await
    }

    async fn keepalive(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<OkResponse, TransportError> {
        let body = serde_json::json!({"from_node": from_node});
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/keepalives",
            token,
            None,
            Some(&body),
        )
        .await
    }

    async fn join(&self, address: &str, body: &JoinRequest) -> Result<JoinResponse, TransportError> {
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/join",
            None,
            None,
            Some(&to_value(body)?),
        )
        .await
    }

    async fn evict(&self, address: &str, body: &EvictRequest) -> Result<OkResponse, TransportError> {
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/evicted",
            None,
            None,
            Some(&to_value(body)?),
        )
        .await
    }

    async fn evict_self(&self, address: &str, body: &LeaveRequest) -> Result<OkResponse, TransportError> {
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/leave",
            None,
            None,
            Some(&to_value(body)?),
        )
        .await
    }

    async fn set_digest(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<BTreeMap<String, Version>, TransportError> {
        let q = [("from_node", from_node.to_string())];
        self.call(
            address,
            reqwest::Method::GET,
            "/api/cluster/set/digest",
            token,
            Some(&q),
            None,
        )
        .await
    }

    async fn set_pull(
        &self,
        address: &str,
        from_node: &str,
        node_ids: &[String],
        token: Option<&str>,
    ) -> Result<Vec<Value>, TransportError> {
        let body = serde_json::json!({"from_node": from_node, "node_ids": node_ids});
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/set/pull",
            token,
            None,
            Some(&body),
        )
        .await
    }

    async fn set_sync(
        &self,
        address: &str,
        from_node: &str,
        records: &[MembershipRecord],
        token: Option<&str>,
    ) -> Result<SetSyncResponse, TransportError> {
        let body = serde_json::json!({"from_node": from_node, "records": records});
        self.call(
            address,
            reqwest::Method::POST,
            "/api/cluster/set/sync",
            token,
            None,
            Some(&body),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[tokio::test]
    async fn close_guard_blocks_calls() -> TestResult {
        let tr = HttpTransport::new(1.0);
        tr.close();
        tr.close();
        let err = tr
            .keepalive("http://127.0.0.1:9", "A", None)
            .await
            .err_or_fail()?;
        assert_eq!(err.0, "transport closed");
        Ok(())
    }

    #[tokio::test]
    async fn unreachable_peer_is_transport_error() -> TestResult {
        let tr = HttpTransport::new(1.0);
        let err = tr
            .keepalive("http://127.0.0.1:9", "A", None)
            .await
            .err_or_fail()?;
        assert!(err.0.contains("/api/cluster/keepalives"));
        Ok(())
    }
}
