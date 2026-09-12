// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Public A4P protocol types.
//!
//! The wire format uses camelCase because these objects cross Rust, HTTP and
//! browser boundaries. Mandates and tokens stay plain JSON objects, exactly like
//! the Python SDK, because signatures are computed over their canonical JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object, the runtime shape of every A4P wire object.
pub type JsonDict = Map<String, Value>;

/// An intent mandate: `type`, `mandateId`, `server`, `subject`, `intent`,
/// `validTime`, `userAuthorization`, `displayText` and `signatures`.
pub type IntentMandate = JsonDict;

/// An intent token: `type`, `tokenId`, `mandateId`, `subject`, `user`, `intent`,
/// `issuedAt`, `expireAt`, `nonce`, `signature`, `alg` and `keyId`.
pub type IntentToken = JsonDict;

/// An operation mandate: `type`, `operationId`, `server`, `subject`,
/// `operation`, `validTime`, `userAuthorization`, `displayText` and `signatures`.
pub type OperationMandate = JsonDict;

/// Convert an A4P value to a JSON payload, dropping `null` fields from objects.
///
/// This mirrors the Python `to_payload()`: object entries whose value is `null`
/// are removed recursively, arrays keep their elements.
pub fn to_payload<T: Serialize>(value: &T) -> JsonDict {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => strip_nulls_object(map),
        _ => JsonDict::new(),
    }
}

/// Drop `null` entries from an object recursively.
pub fn strip_nulls_object(map: JsonDict) -> JsonDict {
    map.into_iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key, strip_nulls_value(value)))
        .collect()
}

/// Drop `null` entries from nested objects of any JSON value.
pub fn strip_nulls_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(strip_nulls_object(map)),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls_value).collect()),
        other => other,
    }
}

/// Conversion of request values into the JSON payload consumed by the services.
///
/// The typed request structs and plain JSON objects both implement it, which
/// mirrors the Python services accepting either a dataclass or a dict.
pub trait IntoPayload {
    /// Convert into a JSON object payload.
    fn into_payload(self) -> JsonDict;
}

impl IntoPayload for JsonDict {
    fn into_payload(self) -> JsonDict {
        self
    }
}

impl IntoPayload for &JsonDict {
    fn into_payload(self) -> JsonDict {
        self.clone()
    }
}

impl IntoPayload for Value {
    fn into_payload(self) -> JsonDict {
        match self {
            Value::Object(map) => map,
            _ => JsonDict::new(),
        }
    }
}

impl IntoPayload for &Value {
    fn into_payload(self) -> JsonDict {
        match self {
            Value::Object(map) => map.clone(),
            _ => JsonDict::new(),
        }
    }
}

macro_rules! impl_into_payload {
    ($($name:ty),* $(,)?) => {
        $(
            impl IntoPayload for $name {
                fn into_payload(self) -> JsonDict {
                    to_payload(&self)
                }
            }

            impl IntoPayload for &$name {
                fn into_payload(self) -> JsonDict {
                    to_payload(self)
                }
            }
        )*
    };
}

/// Unified domain verification outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    /// True when verification succeeded.
    pub valid: bool,
    /// Failure reason text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stable failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The matched scope on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_scope: Option<JsonDict>,
}

impl VerificationResult {
    /// A successful result with an optional matched scope.
    pub fn ok(matched_scope: Option<JsonDict>) -> Self {
        Self {
            valid: true,
            reason: None,
            code: None,
            matched_scope,
        }
    }

    /// A failed result with a reason and a stable code.
    pub fn fail(reason: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            valid: false,
            reason: Some(reason.into()),
            code: Some(code.into()),
            matched_scope: None,
        }
    }

    /// A failed result with the generic `AUTHORIZATION_INVALID` code.
    pub fn fail_generic(reason: impl Into<String>) -> Self {
        Self::fail(reason, "AUTHORIZATION_INVALID")
    }
}

/// Authorization material forwarded by the Agent to the local User Authorizer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserAuthorizationRequest {
    /// The Server-signed intent or operation mandate.
    pub mandate: JsonDict,
    /// Signing options generated by the A4P Server.
    #[serde(default)]
    pub signing_options: JsonDict,
}

/// The local User Authorizer answer returned to the Agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserAuthorizationResponse {
    /// True when the user approved the mandate.
    pub approved: bool,
    /// The mandate with the user signature attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_mandate: Option<JsonDict>,
    /// Reason text when rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
    /// Stable error code when rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Intent prepare request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentAuthorizationRequest {
    /// Agent identifier.
    pub agent_id: String,
    /// User identifier.
    pub user_id: String,
    /// Intent scope with `actions` and optional `executionPolicy`.
    pub intent: JsonDict,
    /// Optional validity in seconds, default 3600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validity_seconds: Option<i64>,
    /// Optional agent public key copied into `subject.agentKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_public_key: Option<JsonDict>,
    /// Free form metadata, not signed.
    #[serde(default)]
    pub metadata: JsonDict,
}

/// Intent prepare and complete response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentAuthorizationResponse {
    /// The Server-signed mandate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate: Option<IntentMandate>,
    /// Signing options for the User Authorizer.
    #[serde(default)]
    pub signing_options: JsonDict,
    /// The issued intent token after a successful complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_token: Option<IntentToken>,
    /// Verification details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_result: Option<VerificationResult>,
    /// True after a successful complete.
    #[serde(default)]
    pub approved: bool,
    /// Rejection reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
}

/// Intent token verification request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenVerificationRequest {
    /// The intent token.
    pub token: IntentToken,
    /// Expected `action`, `params`, `agentId`, `userId` and `agentKeyId`.
    pub expected: JsonDict,
}

/// Intent token verification response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenVerificationResponse {
    /// True when the token authorizes the expected call.
    pub valid: bool,
    /// Failure reason text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stable failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The matched scope including optional usage counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_scope: Option<JsonDict>,
}

/// Operation prepare request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperationAuthorizationRequest {
    /// Agent identifier.
    pub agent_id: String,
    /// User identifier.
    pub user_id: String,
    /// The exact operation with `action` and `params`.
    pub operation: JsonDict,
    /// Optional validity in seconds, default 300.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validity_seconds: Option<i64>,
    /// Optional agent public key copied into `subject.agentKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_public_key: Option<JsonDict>,
    /// Free form metadata, not signed.
    #[serde(default)]
    pub metadata: JsonDict,
}

/// Operation complete request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperationAuthorizationCompletionRequest {
    /// The user-signed operation mandate.
    pub signed_mandate: OperationMandate,
    /// The operation rebuilt by the Tool Server from the current request.
    pub operation: JsonDict,
}

/// Operation prepare response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperationAuthorizationChallenge {
    /// The Server-signed mandate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate: Option<OperationMandate>,
    /// Signing options for the User Authorizer.
    #[serde(default)]
    pub signing_options: JsonDict,
    /// Verification details when rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_result: Option<VerificationResult>,
    /// Rejection reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
}

/// Operation complete response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperationAuthorizationResult {
    /// The consumed operation id on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// True on success.
    #[serde(default)]
    pub approved: bool,
    /// Verification details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_result: Option<VerificationResult>,
    /// Rejection reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
}

impl_into_payload!(
    IntentAuthorizationRequest,
    TokenVerificationRequest,
    OperationAuthorizationRequest,
    OperationAuthorizationCompletionRequest,
    UserAuthorizationRequest,
);

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use serde_json::json;

    #[test]
    fn to_payload_strips_none_fields_recursively() -> TestResult {
        let response = IntentAuthorizationResponse::default();
        let payload = to_payload(&response);
        assert_eq!(
            payload,
            json!({"signingOptions": {}, "approved": false})
                .as_object()
                .required()?
                .clone()
        );

        let value = json!({"a": null, "b": {"c": null, "d": 1}, "e": [null, {"f": null}]});
        let stripped = strip_nulls_value(value);
        assert_eq!(stripped, json!({"b": {"d": 1}, "e": [null, {}]}));
        Ok(())
    }

    #[test]
    fn typed_requests_convert_to_camel_case_payloads() -> TestResult {
        let request = OperationAuthorizationRequest {
            agent_id: "agent-1".into(),
            user_id: "user-1".into(),
            operation: json!({"action": "x", "params": {}})
                .as_object()
                .required()?
                .clone(),
            validity_seconds: Some(60),
            agent_public_key: None,
            metadata: JsonDict::new(),
        };
        let payload = request.into_payload();
        assert_eq!(payload["agentId"], "agent-1");
        assert_eq!(payload["validitySeconds"], 60);
        assert!(!payload.contains_key("agentPublicKey"));
        assert_eq!(payload["metadata"], json!({}));
        Ok(())
    }
}
