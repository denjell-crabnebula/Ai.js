// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Heartbeat endpoints mounted at `/api/datasets`:
//!
//! - `POST /{ds}/services/{sid}/heartbeat` with body `{status?}` renews
//!   the lease and optionally updates `agent_card.status`.
//! - `DELETE /{ds}/services/{sid}/heartbeat` with body `{permanent?}`
//!   soft revokes (default) or hard deletes through the registry.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::store::HeartbeatStore;
use crate::auth::Authorize;
use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;

/// Detail text of the 404 returned when the heartbeat store is absent.
pub const HEARTBEAT_NOT_INITIALIZED: &str =
    "Heartbeat module not initialized on this registry (no a2x_registry/heartbeat sweeper running).";

/// Router mounted at `/api/datasets`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/{dataset}/services/{service_id}/heartbeat",
        post(send_heartbeat).delete(revoke_heartbeat),
    )
}

#[derive(Debug, Default, Deserialize)]
struct HeartbeatRequest {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RevokeRequest {
    #[serde(default)]
    permanent: bool,
}

fn parse_body<T: Default + for<'de> Deserialize<'de>>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::unprocessable(format!("invalid JSON body: {e}")))
}

fn resolve_store(state: &AppState) -> ApiResult<Arc<HeartbeatStore>> {
    state
        .heartbeat_store()
        .ok_or_else(|| ApiError::not_found(HEARTBEAT_NOT_INITIALIZED))
}

async fn send_heartbeat(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: HeartbeatRequest = parse_body(&body)?;
    let store = resolve_store(&state)?;
    let Some(lease) = store.heartbeat(&dataset, &service_id) else {
        return Err(ApiError::not_found(format!(
            "No heartbeat lease for service '{service_id}' in dataset '{dataset}' - was it registered with lease_ttl?"
        )));
    };
    if let Some(status) = req.status {
        let mut updates = Map::new();
        updates.insert("status".into(), Value::String(status));
        if let Err(e) = state
            .registry
            .update_service(&dataset, &service_id, &updates, ctx.as_ref())
        {
            tracing::warn!(
                "heartbeat: status piggyback failed ({}, {}): {}",
                dataset,
                service_id,
                e
            );
        }
    }
    Ok(Json(json!({
        "service_id": service_id,
        "dataset": dataset,
        "state": lease.state,
        "ttl_seconds": lease.ttl_seconds,
        "expires_at": HeartbeatStore::expires_at_wall(&lease),
    })))
}

async fn revoke_heartbeat(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: RevokeRequest = parse_body(&body)?;
    let store = resolve_store(&state)?;
    if req.permanent {
        store.drop_lease(&dataset, &service_id);
        state
            .registry
            .deregister(&dataset, &service_id, ctx.as_ref())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        return Ok(Json(
            json!({"service_id": service_id, "dataset": dataset, "permanent": true}),
        ));
    }
    if !store.revoke(&dataset, &service_id, false) {
        return Err(ApiError::not_found(format!(
            "No heartbeat lease for service '{service_id}' in dataset '{dataset}'"
        )));
    }
    Ok(Json(
        json!({"service_id": service_id, "dataset": dataset, "permanent": false}),
    ))
}
