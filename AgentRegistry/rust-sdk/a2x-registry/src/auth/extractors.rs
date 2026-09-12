// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! axum extractors reproducing the FastAPI auth dependencies.
//!
//! - [`Authorize`]: anonymous fast path when auth is not initialized or the
//!   `{dataset}` in the path is an anonymous namespace; otherwise a valid
//!   bearer token with namespace access is required (401 / 403).
//! - [`RequirePrincipal`]: like `Authorize` but rejects anonymous callers.
//! - [`RequireAdmin`]: `RequirePrincipal` plus the admin role.
//! - [`RequireAdminStrict`]: admin token regardless of namespace state;
//!   404 before bootstrap.
//! - [`RequireAdminOrAnon`]: anonymous or admin; provider and user get 403.

use std::collections::HashMap;
use std::sync::Arc;

use a2x_common::AuthContext;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use serde_json::Value;

use super::store::AuthStore;
use crate::backend::errors::ApiError;
use crate::backend::state::AppState;

/// Detail text of the 404 returned before `auth init`.
pub const AUTH_NOT_INITIALIZED: &str = "Auth is not initialized on this registry. Run 'a2x-registry auth init' to enable the authentication module.";

/// Token from a `Bearer <token>` header (case insensitive scheme).
pub fn parse_bearer(authorization: Option<&str>) -> Option<String> {
    let value = authorization?.trim();
    let mut parts = value.splitn(2, char::is_whitespace);
    let scheme = parts.next()?;
    let token = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

fn authorization_header(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn dataset_param(parts: &mut Parts, state: &Arc<AppState>) -> Option<String> {
    Path::<HashMap<String, String>>::from_request_parts(parts, state)
        .await
        .ok()
        .and_then(|Path(map)| map.get("dataset").cloned())
}

/// Core of the `authorize` dependency.
pub async fn authorize(parts: &mut Parts, state: &Arc<AppState>) -> Result<Option<AuthContext>, ApiError> {
    let Some(store) = state.auth_store() else {
        return Ok(None);
    };
    let dataset = dataset_param(parts, state).await;
    if let Some(ds) = &dataset {
        if !state.registry.is_auth_required(ds) {
            return Ok(None);
        }
    }
    let ds_value = dataset.clone().map(Value::String).unwrap_or(Value::Null);
    let Some(token) = parse_bearer(authorization_header(parts).as_deref()) else {
        store.audit(
            "auth.failed",
            &[("reason", "no_token".into()), ("dataset", ds_value)],
        );
        return Err(ApiError::unauthorized(
            "Authentication required for this namespace",
        ));
    };
    let ctx = store
        .authenticate(&token)
        .map_err(|e| ApiError::unauthorized(e.0))?;
    if let Some(ds) = &dataset {
        if !ctx.is_admin() && !ctx.namespaces.as_ref().map(|n| n.contains(ds)).unwrap_or(false) {
            store.audit(
                "permission.denied",
                &[
                    ("reason", "namespace_out_of_scope".into()),
                    ("principal_id", ctx.principal_id.clone().into()),
                    ("dataset", ds_value),
                ],
            );
            return Err(ApiError::forbidden(format!(
                "Principal lacks access to namespace '{ds}'"
            )));
        }
    }
    Ok(Some(ctx))
}

fn audit_admin_required(store: &AuthStore, ctx: &AuthContext, reason: &str) {
    store.audit(
        "permission.denied",
        &[
            ("reason", reason.into()),
            ("principal_id", ctx.principal_id.clone().into()),
            ("role", ctx.role.as_str().into()),
        ],
    );
}

/// `Option<AuthContext>` per the namespace gating rules.
pub struct Authorize(pub Option<AuthContext>);

impl FromRequestParts<Arc<AppState>> for Authorize {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        authorize(parts, state).await.map(Authorize)
    }
}

/// Always identifies the caller; anonymous callers get 401.
pub struct RequirePrincipal(pub AuthContext);

impl FromRequestParts<Arc<AppState>> for RequirePrincipal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        match authorize(parts, state).await? {
            Some(ctx) => Ok(RequirePrincipal(ctx)),
            None => Err(ApiError::unauthorized("Authentication required")),
        }
    }
}

/// `RequirePrincipal` plus the admin role (403 otherwise).
pub struct RequireAdmin(pub AuthContext);

impl FromRequestParts<Arc<AppState>> for RequireAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let RequirePrincipal(ctx) = RequirePrincipal::from_request_parts(parts, state).await?;
        if !ctx.is_admin() {
            if let Some(store) = state.auth_store() {
                audit_admin_required(&store, &ctx, "admin_required");
            }
            return Err(ApiError::forbidden("Admin role required"));
        }
        Ok(RequireAdmin(ctx))
    }
}

/// Admin authentication that bypasses per namespace gating. 404 before
/// bootstrap, 401 without a valid token, 403 for non admins.
pub struct RequireAdminStrict(pub AuthContext);

impl RequireAdminStrict {
    /// Shared implementation, also used by `POST /api/datasets`.
    pub fn check(
        state: &AppState,
        authorization: Option<&str>,
        missing_token_detail: &str,
        not_admin_detail: &str,
    ) -> Result<AuthContext, ApiError> {
        let Some(store) = state.auth_store() else {
            return Err(ApiError::not_found(AUTH_NOT_INITIALIZED));
        };
        let Some(token) = parse_bearer(authorization) else {
            return Err(ApiError::unauthorized(missing_token_detail));
        };
        let ctx = store
            .authenticate(&token)
            .map_err(|e| ApiError::unauthorized(e.0))?;
        if !ctx.is_admin() {
            audit_admin_required(&store, &ctx, "admin_required");
            return Err(ApiError::forbidden(not_admin_detail));
        }
        Ok(ctx)
    }
}

impl FromRequestParts<Arc<AppState>> for RequireAdminStrict {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let auth = authorization_header(parts);
        Self::check(
            state,
            auth.as_deref(),
            "Admin token required",
            "Admin role required",
        )
        .map(RequireAdminStrict)
    }
}

/// Anonymous (no store or anonymous namespace) or admin; others get 403.
pub struct RequireAdminOrAnon(pub Option<AuthContext>);

impl FromRequestParts<Arc<AppState>> for RequireAdminOrAnon {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let ctx = authorize(parts, state).await?;
        match ctx {
            None => Ok(RequireAdminOrAnon(None)),
            Some(c) if c.is_admin() => Ok(RequireAdminOrAnon(Some(c))),
            Some(c) => {
                if let Some(store) = state.auth_store() {
                    audit_admin_required(&store, &c, "admin_required_for_dataset_ops");
                }
                Err(ApiError::forbidden(
                    "Admin role required for dataset-level operation",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn bearer_parsing() -> TestResult {
        assert_eq!(parse_bearer(Some("Bearer abc")).as_deref(), Some("abc"));
        assert_eq!(parse_bearer(Some("bearer   abc ")).as_deref(), Some("abc"));
        assert_eq!(parse_bearer(Some("Basic abc")), None);
        assert_eq!(parse_bearer(Some("Bearer")), None);
        assert_eq!(parse_bearer(Some("Bearer ")), None);
        assert_eq!(parse_bearer(None), None);
        Ok(())
    }
}
