// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP error rendering matching FastAPI's `HTTPException` bodies.
//!
//! Every error becomes `{"detail": ...}` with the mapped status code. The
//! layered contract mirrors the Python `_run` helper: `NotFound` 404,
//! heartbeat errors 400 with a structured detail, `Invalid` 400,
//! `Permission` 403, missing files 404, everything else 500. Feature and
//! LLM configuration failures render as structured 503 bodies.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::store::AuthStoreError;
use crate::heartbeat::HeartbeatError;
use crate::register::RegistryError;

/// An HTTP error response.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    /// Rendered under the `detail` key, or as the whole body for the
    /// structured 503 variants.
    pub body: Value,
    pub www_authenticate: bool,
    raw_body: bool,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            body: Value::String(detail.into()),
            www_authenticate: false,
            raw_body: false,
        }
    }

    pub fn detail(status: StatusCode, detail: Value) -> Self {
        Self {
            status,
            body: detail,
            www_authenticate: false,
            raw_body: false,
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }

    /// 422 for a malformed body or query, like FastAPI validation errors.
    pub fn unprocessable(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, detail)
    }

    /// 401 with a `WWW-Authenticate: Bearer` header.
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: Value::String(detail.into()),
            www_authenticate: true,
            raw_body: false,
        }
    }

    /// 503 `{feature, extras, detail}` for a missing optional feature.
    pub fn feature_not_installed(feature: &str, extras: &str, detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({"feature": feature, "extras": extras, "detail": detail.into()}),
            www_authenticate: false,
            raw_body: true,
        }
    }

    /// 503 `{reason: "llm_not_configured", detail}`.
    pub fn llm_not_configured(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({"reason": "llm_not_configured", "detail": detail.into()}),
            www_authenticate: false,
            raw_body: true,
        }
    }

    /// Body as it will be serialized.
    pub fn json_body(&self) -> Value {
        if self.raw_body {
            self.body.clone()
        } else {
            json!({ "detail": self.body })
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = self.json_body();
        let mut resp = (self.status, axum::Json(body)).into_response();
        if self.www_authenticate {
            resp.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        resp
    }
}

impl From<RegistryError> for ApiError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::NotFound(m) => ApiError::not_found(m),
            RegistryError::Invalid(m) => ApiError::bad_request(m),
            RegistryError::Permission(m) => ApiError::forbidden(m),
            RegistryError::FileNotFound(m) => ApiError::not_found(m),
            RegistryError::Fetch(m) | RegistryError::Io(m) => {
                tracing::error!("Registry error: {}", m);
                ApiError::internal(m)
            }
        }
    }
}

impl From<HeartbeatError> for ApiError {
    fn from(e: HeartbeatError) -> Self {
        ApiError::detail(StatusCode::BAD_REQUEST, Value::Object(e.body()))
    }
}

impl From<AuthStoreError> for ApiError {
    fn from(e: AuthStoreError) -> Self {
        match e {
            AuthStoreError::NotFound(m) => ApiError::not_found(m),
            AuthStoreError::Invalid(m) | AuthStoreError::AlreadyInitialized(m) => ApiError::bad_request(m),
            AuthStoreError::Io(m) => ApiError::internal(m),
        }
    }
}

impl From<a2x_common::A2xError> for ApiError {
    fn from(e: a2x_common::A2xError) -> Self {
        match &e {
            a2x_common::A2xError::FeatureNotInstalled { feature, extras, .. } => {
                ApiError::feature_not_installed(feature, extras, e.to_string())
            }
            a2x_common::A2xError::LlmNotConfigured(m) => ApiError::llm_not_configured(m.clone()),
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<super::engines::EngineError> for ApiError {
    fn from(e: super::engines::EngineError) -> Self {
        use super::engines::EngineError;
        match e {
            EngineError::FeatureNotInstalled {
                feature,
                extras,
                detail,
            } => ApiError::feature_not_installed(&feature, &extras, detail),
            EngineError::LlmNotConfigured(m) => ApiError::llm_not_configured(m),
            EngineError::Invalid(m) => ApiError::internal(m),
            EngineError::Cancelled => ApiError::internal("cancelled"),
            EngineError::Failed(m) => ApiError::internal(m),
        }
    }
}

/// Result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn bodies_follow_fastapi_shape() -> TestResult {
        let e = ApiError::from(RegistryError::NotFound("x".into()));
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        assert_eq!(e.json_body(), json!({"detail": "x"}));
        let hb = ApiError::from(HeartbeatError::not_supported("no"));
        assert_eq!(hb.json_body()["detail"]["code"], "heartbeat_not_supported");
        let f = ApiError::from(a2x_common::A2xError::feature_not_installed("vector", "vector"));
        assert_eq!(f.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(f.json_body()["feature"], "vector");
        let l = ApiError::from(a2x_common::A2xError::LlmNotConfigured(
            "see llm_apikey.json".into(),
        ));
        assert_eq!(l.json_body()["reason"], "llm_not_configured");
        Ok(())
    }
}
