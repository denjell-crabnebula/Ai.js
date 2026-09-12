// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A4P user authorization boundary.

use async_trait::async_trait;
use serde_json::Value;

use crate::errors::{A4PError, MandateSecurityError};
use crate::intent::mandate::{intent_user_signature_context, normalize_intent_mandate};
use crate::mandate_security::{
    INTENT_MANDATE_TYPE, OPERATION_MANDATE_TYPE, StaticA4PServerTrustStore,
    user_authorization_challenge_base64url, verify_mandate_valid_time, verify_trusted_server_mandate,
};
use crate::operation::mandate::{normalize_operation_mandate, operation_user_signature_context};
use crate::types::{JsonDict, UserAuthorizationRequest, UserAuthorizationResponse};
use crate::user_signature::{A4PUserSigner, UserSigningInput, attach_user_signature, sign_user_signature};
use crate::util::{get_object, str_trimmed};

/// Verify a forwarded request and return locally hardened signing options.
///
/// Processing order: trust lookup, Server signature, validity, signature
/// method, challenge re-derivation and `userVerification=required`.
pub fn verify_local_user_authorization_request(
    request: &UserAuthorizationRequest,
    trust_store: &StaticA4PServerTrustStore,
    expected_signature_method: Option<&str>,
) -> Result<JsonDict, MandateSecurityError> {
    let core = verify_trusted_server_mandate(&request.mandate, trust_store)?;
    verify_mandate_valid_time(&core)?;
    let Some(user_authorization) = get_object(&core, "userAuthorization") else {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "userAuthorization missing",
        ));
    };
    let Some(Value::Bool(required)) = user_authorization.get("required") else {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "userAuthorization.required missing",
        ));
    };
    if !required {
        return Ok(JsonDict::new());
    }
    let signature_method = str_trimmed(user_authorization, "signatureMethod");
    if signature_method.is_empty() {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "userAuthorization.signatureMethod missing",
        ));
    }
    if let Some(expected) = expected_signature_method {
        if signature_method != expected {
            return Err(MandateSecurityError::new(
                "SIGNING_OPTIONS_MISMATCH",
                format!(
                    "User signer method mismatch: mandate requires '{signature_method}', local signer is '{expected}'"
                ),
            ));
        }
    }
    let mut signing_options = request.signing_options.clone();
    if signing_options.get("signatureMethod").and_then(Value::as_str) != Some(signature_method.as_str()) {
        return Err(MandateSecurityError::new(
            "SIGNING_OPTIONS_MISMATCH",
            "signingOptions.signatureMethod does not match the Server-signed mandate",
        ));
    }
    let Some(Value::Object(method_options)) = signing_options.get_mut("methodOptions") else {
        return Err(MandateSecurityError::new(
            "SIGNING_OPTIONS_MISMATCH",
            "signingOptions.methodOptions missing",
        ));
    };
    if signature_method == "webauthn" {
        let challenge = user_authorization_challenge_base64url(&request.mandate)
            .map_err(|error| MandateSecurityError::new("CHALLENGE_BINDING_INVALID", error.to_string()))?;
        method_options.insert("challenge".into(), Value::String(challenge));
        method_options.insert("userVerification".into(), Value::String("required".into()));
    }
    Ok(signing_options)
}

/// Sign either mandate type with a user signer and attach `signatures.user`.
pub fn sign_user_mandate_with_signer(
    mandate: &JsonDict,
    user_signer: &dyn A4PUserSigner,
    signing_input: Option<&UserSigningInput>,
) -> Result<JsonDict, A4PError> {
    let (normalized, context) = match mandate.get("type").and_then(Value::as_str) {
        Some(INTENT_MANDATE_TYPE) => {
            let normalized = normalize_intent_mandate(mandate)?;
            let context = intent_user_signature_context(&normalized, None)?;
            (normalized, context)
        }
        Some(OPERATION_MANDATE_TYPE) => {
            let normalized = normalize_operation_mandate(mandate)?;
            let context = operation_user_signature_context(&normalized, None)?;
            (normalized, context)
        }
        _ => {
            return Err(A4PError::value(format!(
                "Unsupported A4P mandate type: {}",
                crate::mandate_security::python_repr(mandate.get("type"))
            )));
        }
    };
    let signature = sign_user_signature(&context, user_signer, signing_input)?;
    Ok(attach_user_signature(&normalized, &signature))
}

/// Return an unsigned mandate for explicit no-signature test mode.
pub fn approve_user_mandate(mandate: &JsonDict) -> JsonDict {
    let mut approved = mandate.clone();
    let signatures = approved
        .entry("signatures")
        .or_insert_with(|| Value::Object(JsonDict::new()));
    if let Value::Object(map) = signatures {
        map.insert("user".into(), Value::Object(JsonDict::new()));
    }
    approved
}

/// The device or UI side authorization protocol.
#[async_trait]
pub trait A4PUserAuthorizer: Send + Sync {
    /// Ask the user to approve the mandate and return the result.
    async fn authorize(&self, request: UserAuthorizationRequest) -> UserAuthorizationResponse;
}

/// Authorizer that rejects every request.
#[derive(Debug, Clone, Default)]
pub struct RejectingA4PUserAuthorizer;

#[async_trait]
impl A4PUserAuthorizer for RejectingA4PUserAuthorizer {
    async fn authorize(&self, _request: UserAuthorizationRequest) -> UserAuthorizationResponse {
        UserAuthorizationResponse {
            approved: false,
            reject_reason: Some("A4P user authorizer is not configured".into()),
            ..Default::default()
        }
    }
}

/// Test authorizer that approves with an explicitly configured signer.
pub struct ApprovingA4PUserAuthorizer {
    user_signer: Box<dyn A4PUserSigner>,
}

impl ApprovingA4PUserAuthorizer {
    /// Create the authorizer around a signer.
    pub fn new(user_signer: Box<dyn A4PUserSigner>) -> Self {
        Self { user_signer }
    }
}

#[async_trait]
impl A4PUserAuthorizer for ApprovingA4PUserAuthorizer {
    async fn authorize(&self, request: UserAuthorizationRequest) -> UserAuthorizationResponse {
        match sign_user_mandate_with_signer(&request.mandate, self.user_signer.as_ref(), None) {
            Ok(signed) => UserAuthorizationResponse {
                approved: true,
                signed_mandate: Some(signed),
                ..Default::default()
            },
            Err(error) => UserAuthorizationResponse {
                approved: false,
                reject_reason: Some(error.to_string()),
                error_code: error.code().map(str::to_string),
                ..Default::default()
            },
        }
    }
}
