// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `/api/auth/*` endpoints. The whole group returns 404 until the auth
//! store is initialized, so an unbootstrapped registry behaves as if the
//! feature does not exist.
//!
//! Plaintext tokens leave this module only in the bodies of
//! `POST /api/auth/principals` and `POST /api/auth/keys`.

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::extractors::{AUTH_NOT_INITIALIZED, RequireAdmin, RequirePrincipal};
use super::store::{AuthStore, NamespacesUpdate};
use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;

/// Router mounted at `/api/auth`.
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/principals", get(list_principals).post(create_principal))
        .route(
            "/principals/{principal_id}",
            get(get_principal).patch(update_principal),
        )
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{key_id}", axum::routing::delete(revoke_key))
        .layer(middleware::from_fn_with_state(state, require_initialized_auth))
}

async fn require_initialized_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if state.auth_store().is_none() {
        return ApiError::not_found(AUTH_NOT_INITIALIZED).into_response();
    }
    next.run(request).await
}

fn store(state: &AppState) -> ApiResult<Arc<AuthStore>> {
    state
        .auth_store()
        .ok_or_else(|| ApiError::not_found(AUTH_NOT_INITIALIZED))
}

#[derive(Debug, Deserialize)]
struct CreatePrincipalRequest {
    handle: String,
    role: String,
    #[serde(default)]
    namespaces: Option<Vec<String>>,
    #[serde(default)]
    note: String,
}

/// Distinguishes an absent `namespaces` key from an explicit `null`.
fn double_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Vec<String>>>, D::Error> {
    Option::<Vec<String>>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
struct UpdatePrincipalRequest {
    #[serde(default, deserialize_with = "double_option")]
    namespaces: Option<Option<Vec<String>>>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateKeyRequest {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ListKeysQuery {
    #[serde(default)]
    principal_id: Option<String>,
}

async fn whoami(
    State(state): State<Arc<AppState>>,
    RequirePrincipal(ctx): RequirePrincipal,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    let Some(p) = store.get_principal(&ctx.principal_id) else {
        return Err(ApiError::unauthorized("Principal not found"));
    };
    Ok(Json(json!({
        "principal_id": p.id,
        "handle": p.handle,
        "role": p.role,
        "namespaces": p.namespaces,
        "disabled": p.is_disabled(),
    })))
}

async fn create_principal(
    State(state): State<Arc<AppState>>,
    RequireAdmin(ctx): RequireAdmin,
    Json(req): Json<CreatePrincipalRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let store = store(&state)?;
    if req.role != "admin" {
        if let Some(namespaces) = &req.namespaces {
            let unknown: Vec<&String> = namespaces
                .iter()
                .filter(|n| !state.registry.dataset_exists(n))
                .collect();
            if !unknown.is_empty() {
                let listed: Vec<String> = unknown.iter().map(|n| format!("'{n}'")).collect();
                return Err(ApiError::bad_request(format!(
                    "Unknown namespace(s): [{}]. Create the dataset first.",
                    listed.join(", ")
                )));
            }
        }
    }
    let (principal, token) = store
        .create_principal(
            &req.handle,
            &req.role,
            req.namespaces.clone(),
            &req.note,
            Some(&ctx.principal_id),
        )
        .map_err(|e| match e {
            super::store::AuthStoreError::NotFound(m) => ApiError::bad_request(m),
            other => ApiError::from(other),
        })?;
    let key = store
        .list_keys(Some(&principal.id))
        .into_iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "principal_id": principal.id,
            "handle": principal.handle,
            "role": principal.role,
            "namespaces": principal.namespaces,
            "key_id": key.as_ref().map(|k| k.key_id.clone()),
            "key_prefix": key.as_ref().map(|k| k.key_prefix.clone()),
            "token": token,
        })),
    ))
}

async fn list_principals(
    State(state): State<Arc<AppState>>,
    RequireAdmin(_ctx): RequireAdmin,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    Ok(Json(json!(store.list_principals())))
}

async fn get_principal(
    State(state): State<Arc<AppState>>,
    Path(principal_id): Path<String>,
    RequireAdmin(_ctx): RequireAdmin,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    match store.get_principal(&principal_id) {
        Some(p) => Ok(Json(json!(p))),
        None => Err(ApiError::not_found(format!(
            "Principal '{principal_id}' not found"
        ))),
    }
}

async fn update_principal(
    State(state): State<Arc<AppState>>,
    Path(principal_id): Path<String>,
    RequireAdmin(ctx): RequireAdmin,
    Json(req): Json<UpdatePrincipalRequest>,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    let namespaces = match req.namespaces {
        None => NamespacesUpdate::Unset,
        Some(v) => NamespacesUpdate::Set(v),
    };
    let updated = store.update_principal(
        &principal_id,
        namespaces,
        req.role.as_deref(),
        req.disabled,
        req.note.as_deref(),
        Some(&ctx.principal_id),
    )?;
    Ok(Json(json!(updated)))
}

async fn list_keys(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListKeysQuery>,
    RequirePrincipal(ctx): RequirePrincipal,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    let principal_id = if ctx.is_admin() {
        q.principal_id
    } else {
        Some(ctx.principal_id.clone())
    };
    Ok(Json(json!(store.list_keys(principal_id.as_deref()))))
}

async fn create_key(
    State(state): State<Arc<AppState>>,
    RequirePrincipal(ctx): RequirePrincipal,
    Json(req): Json<CreateKeyRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let store = store(&state)?;
    let (key, token) = store.create_key(&ctx.principal_id, &req.name, Some(&ctx.principal_id))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "key_id": key.key_id,
            "principal_id": key.principal_id,
            "key_prefix": key.key_prefix,
            "name": key.name,
            "created_at": key.created_at,
            "token": token,
        })),
    ))
}

async fn revoke_key(
    State(state): State<Arc<AppState>>,
    Path(key_id): Path<String>,
    RequirePrincipal(ctx): RequirePrincipal,
) -> ApiResult<Json<Value>> {
    let store = store(&state)?;
    let Some(target) = store.list_keys(None).into_iter().find(|k| k.key_id == key_id) else {
        return Err(ApiError::not_found(format!("Key '{key_id}' not found")));
    };
    if !ctx.is_admin() && target.principal_id != ctx.principal_id {
        store.audit(
            "permission.denied",
            &[
                ("reason", "not_key_owner".into()),
                ("principal_id", ctx.principal_id.clone().into()),
                ("key_id", key_id.clone().into()),
            ],
        );
        return Err(ApiError::forbidden("You can only revoke your own keys"));
    }
    let revoked = store.revoke_key(&key_id, Some(&ctx.principal_id))?;
    Ok(Json(
        json!({"key_id": revoked.key_id, "revoked_at": revoked.revoked_at}),
    ))
}
