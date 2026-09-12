// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A4P Server orchestration for one-time operation authorization.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use crate::authorization_common::{error_code, mandate_matches_pending};
use crate::errors::A4PError;
use crate::mandate_security::mandate_identifier;
use crate::operation::mandate::{
    CreateOperationMandate, OperationDisplayTextRenderer, VerifyOperationMandate, create_operation_mandate,
    normalize_operation, normalize_operation_mandate, verify_operation_mandate_for_completion,
};
use crate::types::{
    IntoPayload, JsonDict, OperationAuthorizationChallenge, OperationAuthorizationResult, VerificationResult,
};
use crate::user_signature::A4PUserSignatureMethod;
use crate::util::{as_int, get_object, is_truthy, now_epoch, parse_iso_z, py_str, str_or_empty, str_trimmed};

#[derive(Debug, Clone)]
struct PendingOperation {
    request: JsonDict,
    operation: JsonDict,
    mandate: JsonDict,
    expire_at_epoch: i64,
}

/// Prepare, validate and consume one-time operation mandates.
pub struct OperationAuthorizationService {
    server_id: String,
    display_text_renderer: Option<OperationDisplayTextRenderer>,
    require_user_signature: bool,
    user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
    pending: Mutex<HashMap<String, PendingOperation>>,
}

impl OperationAuthorizationService {
    /// Create the service.
    pub fn new(
        server_id: impl Into<String>,
        display_text_renderer: Option<OperationDisplayTextRenderer>,
        require_user_signature: bool,
        user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            display_text_renderer,
            require_user_signature,
            user_signature_method,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The configured server id.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Number of pending operation authorizations.
    pub fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// True when a pending authorization with this operation id exists.
    pub fn has_pending(&self, operation_id: &str) -> bool {
        self.pending.lock().contains_key(operation_id)
    }

    /// Force a pending entry to expire at `expire_at_epoch`. Returns false when unknown.
    ///
    /// Intended for tests that simulate an expired pending authorization.
    pub fn set_pending_expiry(&self, operation_id: &str, expire_at_epoch: i64) -> bool {
        match self.pending.lock().get_mut(operation_id) {
            Some(pending) => {
                pending.expire_at_epoch = expire_at_epoch;
                true
            }
            None => false,
        }
    }

    /// Prune expired entries, validate the request, create and sign the mandate.
    pub fn prepare(&self, request: impl IntoPayload) -> Result<OperationAuthorizationChallenge, A4PError> {
        let request_payload = request.into_payload();
        self.prune_expired();
        match self.prepare_inner(&request_payload) {
            Ok((operation_id, operation, mandate, signing_options)) => {
                let until = get_object(&mandate, "validTime")
                    .map(|valid_time| str_or_empty(valid_time, "until"))
                    .unwrap_or_default();
                let expire_at_epoch = parse_iso_z(&until).unwrap_or(0);
                self.pending.lock().insert(
                    operation_id,
                    PendingOperation {
                        request: request_payload,
                        operation,
                        mandate: mandate.clone(),
                        expire_at_epoch,
                    },
                );
                Ok(OperationAuthorizationChallenge {
                    mandate: Some(mandate),
                    signing_options,
                    ..Default::default()
                })
            }
            Err(A4PError::Protocol(error)) if error.is_user_credential_not_registered() => {
                Ok(OperationAuthorizationChallenge {
                    reject_reason: Some(error.message.clone()),
                    verification_result: Some(VerificationResult::fail(&error.message, &error.code)),
                    ..Default::default()
                })
            }
            Err(error) if error.is_value_error() => Ok(OperationAuthorizationChallenge {
                reject_reason: Some(error.to_string()),
                verification_result: Some(VerificationResult::fail(error.to_string(), "MANDATE_INVALID")),
                ..Default::default()
            }),
            Err(error) => Err(error),
        }
    }

    fn prepare_inner(
        &self,
        request_payload: &JsonDict,
    ) -> Result<(String, JsonDict, JsonDict, JsonDict), A4PError> {
        let agent_id = str_trimmed(request_payload, "agentId");
        if agent_id.is_empty() {
            return Err(A4PError::value("agentId missing"));
        }
        let user_id = str_trimmed(request_payload, "userId");
        if user_id.is_empty() {
            return Err(A4PError::value("userId missing"));
        }
        let validity_seconds = match request_payload.get("validitySeconds") {
            None | Some(Value::Null) => 300,
            Some(value) => match as_int(value) {
                Some(number) if number > 0 => number,
                _ => return Err(A4PError::value("validitySeconds must be a positive integer")),
            },
        };
        let operation = normalize_operation(request_payload.get("operation"))?;
        let server = match request_payload.get("server").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => self.server_id.clone(),
        };
        let method_name = self
            .user_signature_method
            .as_ref()
            .map(|method| method.signature_method().to_string());
        let method_policy = self
            .user_signature_method
            .as_ref()
            .map(|method| method.method_policy());
        let mandate = create_operation_mandate(CreateOperationMandate {
            operation: &operation,
            server_url: &server,
            agent_id: &agent_id,
            validity_seconds,
            agent_public_key: get_object(request_payload, "agentPublicKey"),
            require_user_signature: self.require_user_signature,
            user_signature_method: method_name.as_deref(),
            user_signature_method_policy: method_policy.as_ref(),
            display_text_renderer: self.display_text_renderer.as_ref(),
        })?;
        let operation_id = mandate_identifier(&mandate)?;
        let signing_options = match &self.user_signature_method {
            Some(method) if self.require_user_signature => method.signing_options(&user_id, &mandate)?,
            _ => JsonDict::new(),
        };
        Ok((operation_id, operation, mandate, signing_options))
    }

    /// Compare the three operation copies, verify the mandate and consume the pending entry.
    pub fn complete(&self, request: impl IntoPayload) -> Result<OperationAuthorizationResult, A4PError> {
        self.prune_expired();
        let request_payload = request.into_payload();
        let Some(signed_mandate) = get_object(&request_payload, "signedMandate") else {
            return Ok(rejected("signedMandate missing", "MANDATE_INVALID"));
        };
        let operation_id = match mandate_identifier(signed_mandate) {
            Ok(id) => id,
            Err(error) => return Ok(rejected(error.to_string(), "MANDATE_INVALID")),
        };
        let mut pending_map = self.pending.lock();
        let Some(pending) = pending_map.get(&operation_id).cloned() else {
            return Ok(OperationAuthorizationResult {
                approved: false,
                reject_reason: Some(format!("No pending operation authorization: {operation_id}")),
                verification_result: Some(VerificationResult::fail(
                    "No pending operation authorization",
                    "AUTHORIZATION_NOT_PENDING",
                )),
                ..Default::default()
            });
        };
        let current_operation = match normalize_operation(request_payload.get("operation")) {
            Ok(operation) => operation,
            Err(error) => return Ok(rejected(error.to_string(), "OPERATION_INVALID")),
        };
        if current_operation != pending.operation {
            return Ok(rejected(
                "Current operation does not match pending operation authorization",
                "OPERATION_PENDING_MISMATCH",
            ));
        }
        if !mandate_matches_pending(signed_mandate, &pending.mandate, normalize_operation_mandate) {
            return Ok(rejected(
                "Signed mandate does not match pending operation authorization",
                "MANDATE_PENDING_MISMATCH",
            ));
        }
        let signed_operation = match normalize_operation(signed_mandate.get("operation")) {
            Ok(operation) => operation,
            Err(error) => return Ok(rejected(error.to_string(), "MANDATE_INVALID")),
        };
        if signed_operation != current_operation {
            return Ok(rejected(
                "Signed mandate operation does not match current operation",
                "OPERATION_MANDATE_MISMATCH",
            ));
        }
        let expected_user_id = str_or_empty(&pending.request, "userId");
        let verification = verify_operation_mandate_for_completion(
            signed_mandate,
            VerifyOperationMandate {
                expected: Some(&current_operation),
                expected_user_id: Some(&expected_user_id),
                require_user_signature: self.require_user_signature,
                user_signature_method: self.user_signature_method.as_deref(),
            },
        );
        if let Err(reason) = verification {
            let code = error_code(&reason, "MANDATE");
            return Ok(rejected(reason, code));
        }
        pending_map.remove(&operation_id);
        Ok(OperationAuthorizationResult {
            operation_id: Some(operation_id),
            approved: true,
            verification_result: Some(VerificationResult::ok(None)),
            reject_reason: None,
        })
    }

    fn prune_expired(&self) {
        let now = now_epoch();
        self.pending
            .lock()
            .retain(|_, pending| pending.expire_at_epoch > now);
    }
}

fn rejected(reason: impl Into<String>, code: impl Into<String>) -> OperationAuthorizationResult {
    let reason = reason.into();
    OperationAuthorizationResult {
        approved: false,
        reject_reason: Some(reason.clone()),
        verification_result: Some(VerificationResult::fail(reason, code)),
        ..Default::default()
    }
}
