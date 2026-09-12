// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! axum router for the cluster endpoints (`/api/cluster/*`).
//!
//! Handlers are thin: they delegate to [`ClusterStore`] and
//! [`MembershipStore`] methods, the same methods the in-process test
//! transport calls. Mount [`router`] when the cluster module is initialised
//! and [`dormant_router`] otherwise, so every `/api/cluster/*` path answers
//! 404 on a standalone registry.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::envelope::{Key, SyncEnvelope};
use crate::membership::{MemberSpec, MembershipStore};
use crate::store::ClusterStore;
use crate::transport::{EvictRequest, JoinRequest, LeaveRequest, OpenRequest, SESSION_HEADER};

/// Message returned with 404 when the cluster module is not initialised.
pub const NOT_INITIALIZED_DETAIL: &str = "Cluster module not initialized on this registry. Run 'a2x-registry cluster init' to enable distributed sync.";

type AppState = Arc<ClusterStore>;

fn detail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"detail": msg.into()}))).into_response()
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn split_csv(raw: Option<&String>) -> Option<Vec<String>> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn require_membership(store: &Arc<ClusterStore>) -> Result<Arc<MembershipStore>, Box<Response>> {
    store.membership().ok_or_else(|| {
        Box::new(detail(
            StatusCode::NOT_FOUND,
            "Membership control plane not available",
        ))
    })
}

// ── request models ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AddPeerRequest {
    address: String,
    #[serde(default)]
    namespaces: Option<Vec<String>>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
struct PullRequest {
    from_node: String,
    #[serde(default)]
    keys: Vec<Key>,
}

#[derive(Deserialize)]
struct UpdatesRequest {
    from_node: String,
    #[serde(default)]
    envelopes: Vec<SyncEnvelope>,
}

#[derive(Deserialize)]
struct KeepaliveRequest {
    from_node: String,
}

#[derive(Deserialize)]
struct SetAddRequest {
    #[serde(default)]
    members: Vec<MemberSpec>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
struct SetRemoveRequest {
    #[serde(default)]
    members: Vec<MemberSpec>,
}

#[derive(Deserialize)]
struct SetPullRequest {
    from_node: String,
    #[serde(default)]
    node_ids: Vec<String>,
}

#[derive(Deserialize)]
struct SetSyncRequest {
    from_node: String,
    #[serde(default)]
    records: Vec<Value>,
}

// ── trigger / session management ─────────────────────────────────────────

async fn add_peer(State(store): State<AppState>, Json(req): Json<AddPeerRequest>) -> Response {
    match store
        .connect_peer(&req.address, req.namespaces, req.token.as_deref())
        .await
    {
        Ok(peer) => Json(json!({"peer": peer.to_summary()})).into_response(),
        Err(err) => detail(StatusCode::BAD_GATEWAY, format!("peer unreachable: {err}")),
    }
}

async fn list_peers(State(store): State<AppState>) -> Response {
    Json(json!({"peers": store.state_summary().peers})).into_response()
}

async fn remove_peer(State(store): State<AppState>, Path(node_id): Path<String>) -> Response {
    let removed = store.disconnect_peer(&node_id);
    Json(json!({"node_id": node_id, "removed": removed})).into_response()
}

// ── peer-facing sync endpoints ───────────────────────────────────────────

async fn open_session(State(store): State<AppState>, Json(req): Json<OpenRequest>) -> Response {
    Json(store.handle_open(req)).into_response()
}

async fn get_merkle(
    State(store): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(from_node) = q.get("from_node") else {
        return detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "from_node query parameter required",
        );
    };
    let ns = split_csv(q.get("namespaces"));
    let token = session_token(&headers);
    Json(store.serve_merkle(from_node, ns.as_deref(), token.as_deref())).into_response()
}

async fn get_digest(
    State(store): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(from_node) = q.get("from_node") else {
        return detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "from_node query parameter required",
        );
    };
    let ns = split_csv(q.get("namespaces"));
    let buckets: Option<Vec<u32>> = match split_csv(q.get("buckets")) {
        None => None,
        Some(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p.parse::<u32>() {
                    Ok(b) => out.push(b),
                    Err(_) => return detail(StatusCode::UNPROCESSABLE_ENTITY, "buckets must be integers"),
                }
            }
            Some(out)
        }
    };
    let token = session_token(&headers);
    Json(store.serve_digest(from_node, ns.as_deref(), token.as_deref(), buckets.as_deref())).into_response()
}

async fn post_pulls(
    State(store): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PullRequest>,
) -> Response {
    let token = session_token(&headers);
    Json(store.serve_pull(&req.from_node, &req.keys, token.as_deref())).into_response()
}

async fn post_updates(
    State(store): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdatesRequest>,
) -> Response {
    let token = session_token(&headers);
    Json(store.serve_updates(&req.from_node, req.envelopes, token.as_deref())).into_response()
}

async fn post_keepalive(
    State(store): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<KeepaliveRequest>,
) -> Response {
    let token = session_token(&headers);
    Json(store.handle_keepalive(&req.from_node, token.as_deref())).into_response()
}

// ── membership control plane (user-facing) ───────────────────────────────

async fn set_add(State(store): State<AppState>, Json(req): Json<SetAddRequest>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.set_add(&req.members, req.token.as_deref()).await).into_response(),
        Err(r) => *r,
    }
}

async fn set_remove(State(store): State<AppState>, Json(req): Json<SetRemoveRequest>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.set_remove(&req.members).await).into_response(),
        Err(r) => *r,
    }
}

async fn get_set(State(store): State<AppState>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.show()).into_response(),
        Err(r) => *r,
    }
}

// ── membership protocol (peer -> us) ─────────────────────────────────────

async fn post_join(State(store): State<AppState>, Json(req): Json<JoinRequest>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.handle_join(req).await).into_response(),
        Err(r) => *r,
    }
}

async fn post_evicted(State(store): State<AppState>, Json(req): Json<EvictRequest>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.handle_evicted(req)).into_response(),
        Err(r) => *r,
    }
}

async fn post_leave(State(store): State<AppState>, Json(req): Json<LeaveRequest>) -> Response {
    match require_membership(&store) {
        Ok(m) => Json(m.handle_evict_self(req).await).into_response(),
        Err(r) => *r,
    }
}

async fn get_set_digest(
    State(store): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(from_node) = q.get("from_node") else {
        return detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "from_node query parameter required",
        );
    };
    let token = session_token(&headers);
    match require_membership(&store) {
        Ok(m) => Json(m.serve_set_digest(from_node, token.as_deref())).into_response(),
        Err(r) => *r,
    }
}

async fn post_set_pull(
    State(store): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetPullRequest>,
) -> Response {
    let token = session_token(&headers);
    match require_membership(&store) {
        Ok(m) => Json(m.serve_set_pull(&req.from_node, &req.node_ids, token.as_deref())).into_response(),
        Err(r) => *r,
    }
}

async fn post_set_sync(
    State(store): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetSyncRequest>,
) -> Response {
    let token = session_token(&headers);
    match require_membership(&store) {
        Ok(m) => Json(m.serve_set_sync(&req.from_node, &req.records, token.as_deref())).into_response(),
        Err(r) => *r,
    }
}

// ── observability ────────────────────────────────────────────────────────

async fn get_state(State(store): State<AppState>) -> Response {
    Json(store.state_summary()).into_response()
}

/// The `/api/cluster/*` router for an initialised node.
pub fn router(store: Arc<ClusterStore>) -> Router {
    let inner = Router::new()
        .route("/peers", post(add_peer).get(list_peers))
        .route("/peers/{node_id}", axum::routing::delete(remove_peer))
        .route("/sessions", post(open_session))
        .route("/merkle", get(get_merkle))
        .route("/digest", get(get_digest))
        .route("/pulls", post(post_pulls))
        .route("/updates", post(post_updates))
        .route("/keepalives", post(post_keepalive))
        .route("/set/add", post(set_add))
        .route("/set/remove", post(set_remove))
        .route("/set", get(get_set))
        .route("/join", post(post_join))
        .route("/evicted", post(post_evicted))
        .route("/leave", post(post_leave))
        .route("/set/digest", get(get_set_digest))
        .route("/set/pull", post(post_set_pull))
        .route("/set/sync", post(post_set_sync))
        .route("/state", get(get_state))
        .with_state(store);
    Router::new().nest("/api/cluster", inner)
}

async fn not_initialized() -> Response {
    detail(StatusCode::NOT_FOUND, NOT_INITIALIZED_DETAIL)
}

/// Router that answers 404 with the Python `detail` message for every
/// `/api/cluster/*` path. Mount it when `cluster_state.json` is absent.
pub fn dormant_router() -> Router {
    Router::new()
        .route("/api/cluster", any(not_initialized))
        .route("/api/cluster/{*rest}", any(not_initialized))
}
