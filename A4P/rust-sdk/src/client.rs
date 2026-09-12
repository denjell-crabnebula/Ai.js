// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP A4P client.

use std::time::Duration;

use serde_json::Value;

use crate::errors::A4PError;
use crate::types::{
    IntentAuthorizationResponse, IntoPayload, JsonDict, OperationAuthorizationChallenge,
    OperationAuthorizationResult, TokenVerificationResponse, VerificationResult,
};
use ap_support::env::EnvSource;

use crate::util::{get_object, is_truthy, py_str};

/// Default base URL when `A4P_SERVER_BASE_URL` is unset.
pub const DEFAULT_A4P_BASE_URL: &str = "http://127.0.0.1:8961";
/// Default request timeout in seconds when `A4P_HTTP_TIMEOUT_S` is unset.
pub const DEFAULT_A4P_TIMEOUT_S: f64 = 300.0;

/// Return `A4P_SERVER_BASE_URL` without trailing slashes, or the default.
pub fn default_a4p_base_url() -> String {
    default_a4p_base_url_in(ap_support::env::current())
}

/// [`default_a4p_base_url`] reading from `env`.
pub fn default_a4p_base_url_in(env: &dyn EnvSource) -> String {
    env.get("A4P_SERVER_BASE_URL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_A4P_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Return `A4P_HTTP_TIMEOUT_S` clamped to at least one second, or the default.
pub fn default_a4p_timeout() -> f64 {
    default_a4p_timeout_in(ap_support::env::current())
}

/// [`default_a4p_timeout`] reading from `env`.
pub fn default_a4p_timeout_in(env: &dyn EnvSource) -> f64 {
    let raw = env
        .get("A4P_HTTP_TIMEOUT_S")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "300".to_string());
    match raw.trim().parse::<f64>() {
        Ok(value) if value.is_finite() => value.max(1.0),
        _ => DEFAULT_A4P_TIMEOUT_S,
    }
}

fn verification_result_from_payload(payload: Option<&Value>) -> Option<VerificationResult> {
    let map = payload?.as_object()?;
    Some(VerificationResult {
        valid: map.get("valid").map(is_truthy).unwrap_or(false),
        reason: map.get("reason").filter(|v| !v.is_null()).map(py_str),
        code: map.get("code").filter(|v| !v.is_null()).map(py_str),
        matched_scope: get_object(map, "matchedScope").cloned(),
    })
}

fn optional_string(map: &JsonDict, key: &str) -> Option<String> {
    map.get(key).filter(|value| !value.is_null()).map(py_str)
}

/// Small async HTTP client for A4P server endpoints.
#[derive(Debug, Clone)]
pub struct A4PClient {
    base_url: String,
    timeout: f64,
    http: reqwest::Client,
}

impl Default for A4PClient {
    fn default() -> Self {
        Self::new()
    }
}

impl A4PClient {
    /// Create a client from `A4P_SERVER_BASE_URL` and `A4P_HTTP_TIMEOUT_S`.
    pub fn new() -> Self {
        Self::with_options(None, None)
    }

    /// Create a client from `A4P_SERVER_BASE_URL` and `A4P_HTTP_TIMEOUT_S`
    /// read from `env` instead of the process environment.
    pub fn with_env(env: &dyn EnvSource) -> Self {
        Self::with_options(
            Some(&default_a4p_base_url_in(env)),
            Some(default_a4p_timeout_in(env)),
        )
    }

    /// Create a client with an explicit base URL and timeout in seconds.
    pub fn with_options(base_url: Option<&str>, timeout: Option<f64>) -> Self {
        let base_url = base_url
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_end_matches('/').to_string())
            .unwrap_or_else(default_a4p_base_url);
        let timeout = timeout.unwrap_or_else(default_a4p_timeout);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs_f64(timeout.max(0.001)))
            .build()
            .unwrap_or_default();
        Self {
            base_url,
            timeout,
            http,
        }
    }

    /// Create a client for a base URL with the default timeout.
    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_options(Some(base_url), None)
    }

    /// The base URL without trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The request timeout in seconds.
    pub fn timeout(&self) -> f64 {
        self.timeout
    }

    /// POST `/a4p/v1/intent-authorizations/prepare`.
    pub async fn prepare_intent_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<IntentAuthorizationResponse, A4PError> {
        let payload = self
            .post("/a4p/v1/intent-authorizations/prepare", &request.into_payload())
            .await?;
        Ok(IntentAuthorizationResponse {
            mandate: get_object(&payload, "mandate").cloned(),
            signing_options: get_object(&payload, "signingOptions")
                .cloned()
                .unwrap_or_default(),
            intent_token: None,
            verification_result: verification_result_from_payload(payload.get("verificationResult")),
            approved: payload.get("approved").map(is_truthy).unwrap_or(false),
            reject_reason: optional_string(&payload, "rejectReason"),
        })
    }

    /// POST `/a4p/v1/intent-authorizations/complete`.
    pub async fn complete_intent_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<IntentAuthorizationResponse, A4PError> {
        let payload = self
            .post("/a4p/v1/intent-authorizations/complete", &request.into_payload())
            .await?;
        Ok(IntentAuthorizationResponse {
            mandate: get_object(&payload, "mandate").cloned(),
            signing_options: JsonDict::new(),
            intent_token: get_object(&payload, "intentToken").cloned(),
            verification_result: verification_result_from_payload(payload.get("verificationResult")),
            approved: payload.get("approved").map(is_truthy).unwrap_or(false),
            reject_reason: optional_string(&payload, "rejectReason"),
        })
    }

    /// POST `/a4p/v1/intent-tokens/verify`.
    pub async fn verify_intent_token(
        &self,
        request: impl IntoPayload,
    ) -> Result<TokenVerificationResponse, A4PError> {
        let payload = self
            .post("/a4p/v1/intent-tokens/verify", &request.into_payload())
            .await?;
        Ok(TokenVerificationResponse {
            valid: payload.get("valid").map(is_truthy).unwrap_or(false),
            reason: optional_string(&payload, "reason"),
            code: optional_string(&payload, "code"),
            matched_scope: get_object(&payload, "matchedScope").cloned(),
        })
    }

    /// POST `/a4p/v1/operation-authorizations/prepare`.
    pub async fn prepare_operation_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<OperationAuthorizationChallenge, A4PError> {
        let payload = self
            .post(
                "/a4p/v1/operation-authorizations/prepare",
                &request.into_payload(),
            )
            .await?;
        Ok(OperationAuthorizationChallenge {
            mandate: get_object(&payload, "mandate").cloned(),
            signing_options: get_object(&payload, "signingOptions")
                .cloned()
                .unwrap_or_default(),
            verification_result: verification_result_from_payload(payload.get("verificationResult")),
            reject_reason: optional_string(&payload, "rejectReason"),
        })
    }

    /// POST `/a4p/v1/operation-authorizations/complete`.
    pub async fn complete_operation_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<OperationAuthorizationResult, A4PError> {
        let payload = self
            .post(
                "/a4p/v1/operation-authorizations/complete",
                &request.into_payload(),
            )
            .await?;
        let operation_id = payload
            .get("operationId")
            .filter(|value| is_truthy(value))
            .map(py_str)
            .filter(|value| !value.is_empty());
        Ok(OperationAuthorizationResult {
            operation_id,
            verification_result: verification_result_from_payload(payload.get("verificationResult")),
            approved: payload.get("approved").map(is_truthy).unwrap_or(false),
            reject_reason: optional_string(&payload, "rejectReason"),
        })
    }

    /// POST `/a4p/v1/user-credentials/ed25519/register`.
    pub async fn register_ed25519_credential(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.post("/a4p/v1/user-credentials/ed25519/register", request)
            .await
    }

    /// POST `/a4p/v1/user-credentials/webauthn/register/options`.
    pub async fn webauthn_registration_options(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.post("/a4p/v1/user-credentials/webauthn/register/options", request)
            .await
    }

    /// POST `/a4p/v1/user-credentials/webauthn/register/verify`.
    pub async fn verify_webauthn_registration(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.post("/a4p/v1/user-credentials/webauthn/register/verify", request)
            .await
    }

    /// POST a JSON object and return the JSON object response.
    ///
    /// Non-2xx responses become `A4PError::Runtime("A4P HTTP {status}: {body}")`
    /// and transport failures become `A4PError::Runtime("A4P HTTP request failed: ...")`.
    /// Empty or non-object responses normalize to an empty object.
    pub async fn post(&self, path: &str, payload: &JsonDict) -> Result<JsonDict, A4PError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|error| A4PError::runtime(format!("A4P HTTP request failed: {error}")))?;
        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|error| A4PError::runtime(format!("A4P HTTP request failed: {error}")))?;
        if !status.is_success() {
            let error_payload = match serde_json::from_str::<Value>(&raw) {
                Ok(value) => value,
                Err(_) => {
                    let mut wrapper = JsonDict::new();
                    wrapper.insert("error".into(), Value::String(raw));
                    Value::Object(wrapper)
                }
            };
            return Err(A4PError::runtime(format!(
                "A4P HTTP {}: {}",
                status.as_u16(),
                error_payload
            )));
        }
        if raw.trim().is_empty() {
            return Ok(JsonDict::new());
        }
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Ok(JsonDict::new()),
            Err(error) => Err(A4PError::runtime(format!(
                "A4P HTTP response is not valid JSON: {error}"
            ))),
        }
    }
}
