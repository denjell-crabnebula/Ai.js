// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A4P Server orchestration for intent authorization.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use crate::authorization_common::{error_code, mandate_matches_pending};
use crate::errors::{A4PError, IntentTokenUsageStoreError};
use crate::intent::mandate::{
    CreateIntentMandate, IntentDisplayTextRenderer, VerifyIntentMandate, create_intent_mandate,
    normalize_intent_mandate, verify_intent_mandate,
};
use crate::intent::scope::normalize_intent_scope;
use crate::intent::token::{IssueIntentToken, VerifyIntentToken, issue_intent_token, verify_intent_token};
use crate::intent::usage_store::A4PIntentTokenUsageStore;
use crate::mandate_security::mandate_identifier;
use crate::types::{
    IntentAuthorizationResponse, IntoPayload, JsonDict, TokenVerificationResponse, VerificationResult,
};
use crate::user_signature::A4PUserSignatureMethod;
use crate::util::{as_int, get_object, is_truthy, parse_iso_z, py_str, str_or_empty, str_trimmed};

#[derive(Debug, Clone)]
struct PendingIntent {
    request: JsonDict,
    mandate: JsonDict,
}

/// Prepare and complete intent mandates, then verify issued tokens.
pub struct IntentAuthorizationService {
    server_id: String,
    display_text_renderer: Option<IntentDisplayTextRenderer>,
    require_user_signature: bool,
    user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
    token_usage_store: Arc<dyn A4PIntentTokenUsageStore>,
    pending: Mutex<HashMap<String, PendingIntent>>,
}

impl IntentAuthorizationService {
    /// Create the service.
    pub fn new(
        server_id: impl Into<String>,
        display_text_renderer: Option<IntentDisplayTextRenderer>,
        require_user_signature: bool,
        user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
        token_usage_store: Arc<dyn A4PIntentTokenUsageStore>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            display_text_renderer,
            require_user_signature,
            user_signature_method,
            token_usage_store,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The configured server id.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Number of pending intent authorizations.
    pub fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// True when a pending authorization with this mandate id exists.
    pub fn has_pending(&self, mandate_id: &str) -> bool {
        self.pending.lock().contains_key(mandate_id)
    }

    /// Validate the request, create and sign the mandate, then store the pending entry.
    pub fn prepare(&self, request: impl IntoPayload) -> Result<IntentAuthorizationResponse, A4PError> {
        let request_payload = request.into_payload();
        let intent = get_object(&request_payload, "intent")
            .cloned()
            .unwrap_or_default();
        let outcome = self.prepare_inner(&request_payload, &intent);
        match outcome {
            Ok((mandate_id, mandate, signing_options)) => {
                self.pending.lock().insert(
                    mandate_id,
                    PendingIntent {
                        request: request_payload,
                        mandate: mandate.clone(),
                    },
                );
                Ok(IntentAuthorizationResponse {
                    mandate: Some(mandate),
                    signing_options,
                    approved: false,
                    ..Default::default()
                })
            }
            Err(A4PError::Protocol(error)) if error.is_user_credential_not_registered() => {
                Ok(IntentAuthorizationResponse {
                    approved: false,
                    reject_reason: Some(error.message.clone()),
                    verification_result: Some(VerificationResult::fail(&error.message, &error.code)),
                    ..Default::default()
                })
            }
            Err(error) if error.is_value_error() => Ok(IntentAuthorizationResponse {
                approved: false,
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
        intent: &JsonDict,
    ) -> Result<(String, JsonDict, JsonDict), A4PError> {
        let agent_id = str_trimmed(request_payload, "agentId");
        if agent_id.is_empty() {
            return Err(A4PError::value("agentId missing"));
        }
        let user_id = str_trimmed(request_payload, "userId");
        if user_id.is_empty() {
            return Err(A4PError::value("userId missing"));
        }
        let validity_seconds = match request_payload.get("validitySeconds") {
            None | Some(Value::Null) => 3600,
            Some(value) => match as_int(value) {
                Some(number) if number > 0 => number,
                _ => return Err(A4PError::value("validitySeconds must be a positive integer")),
            },
        };
        if intent.contains_key("executionPolicy") && intent.get("executionPolicy") == Some(&Value::Null) {
            return Err(A4PError::value("executionPolicy must include maxExecutions"));
        }
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
        let mandate = create_intent_mandate(CreateIntentMandate {
            server: &server,
            agent_id: &agent_id,
            actions: intent.get("actions"),
            execution_policy: intent.get("executionPolicy"),
            validity_seconds,
            subject_type: "agent",
            agent_public_key: get_object(request_payload, "agentPublicKey"),
            require_user_signature: self.require_user_signature,
            user_signature_method: method_name.as_deref(),
            user_signature_method_policy: method_policy.as_ref(),
            display_text_renderer: self.display_text_renderer.as_ref(),
        })?;
        let mandate_id = mandate_identifier(&mandate)?;
        let signing_options = match &self.user_signature_method {
            Some(method) if self.require_user_signature => method.signing_options(&user_id, &mandate)?,
            _ => JsonDict::new(),
        };
        Ok((mandate_id, mandate, signing_options))
    }

    /// Verify the submitted mandate against the pending entry and issue a token.
    pub fn complete(&self, request: impl IntoPayload) -> Result<IntentAuthorizationResponse, A4PError> {
        let request = request.into_payload();
        let Some(signed_mandate) = get_object(&request, "signedMandate") else {
            return Ok(rejected(None, "signedMandate missing", "MANDATE_INVALID"));
        };
        let mandate_id = match mandate_identifier(signed_mandate) {
            Ok(id) => id,
            Err(error) => return Ok(rejected(None, error.to_string(), "MANDATE_INVALID")),
        };
        let mut pending_map = self.pending.lock();
        let Some(pending) = pending_map.get(&mandate_id).cloned() else {
            return Ok(IntentAuthorizationResponse {
                approved: false,
                reject_reason: Some(format!("No pending intent authorization: {mandate_id}")),
                verification_result: Some(VerificationResult::fail(
                    "No pending intent authorization",
                    "AUTHORIZATION_NOT_PENDING",
                )),
                ..Default::default()
            });
        };
        if !mandate_matches_pending(signed_mandate, &pending.mandate, normalize_intent_mandate) {
            return Ok(rejected(
                Some(pending.mandate.clone()),
                "Signed mandate does not match pending intent authorization",
                "MANDATE_PENDING_MISMATCH",
            ));
        }
        let expected_server = match pending.request.get("server").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => self.server_id.clone(),
        };
        let expected_user_id = str_or_empty(&pending.request, "userId");
        let verification = verify_intent_mandate(
            signed_mandate,
            VerifyIntentMandate {
                expected_server: Some(&expected_server),
                expected_user_id: Some(&expected_user_id),
                require_user_signature: self.require_user_signature,
                user_signature_method: self.user_signature_method.as_deref(),
            },
        );
        if let Err(reason) = verification {
            let code = error_code(&reason, "MANDATE");
            return Ok(rejected(Some(pending.mandate.clone()), reason, code));
        }
        pending_map.remove(&mandate_id);
        drop(pending_map);
        let user_id = match pending.request.get("userId").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => "user:unknown".to_string(),
        };
        let token = issue_intent_token(
            signed_mandate,
            IssueIntentToken {
                user_id: &user_id,
                verified_mandate: true,
                require_user_signature: self.require_user_signature,
                user_signature_method: None,
            },
        )?;
        Ok(IntentAuthorizationResponse {
            mandate: Some(signed_mandate.clone()),
            intent_token: Some(token),
            approved: true,
            verification_result: Some(VerificationResult::ok(None)),
            ..Default::default()
        })
    }

    /// Stateless token verification followed by atomic usage consumption.
    pub fn verify_token(&self, request: impl IntoPayload) -> Result<TokenVerificationResponse, A4PError> {
        let request_payload = request.into_payload();
        let token = get_object(&request_payload, "token").cloned().unwrap_or_default();
        let expected = get_object(&request_payload, "expected")
            .cloned()
            .unwrap_or_default();
        let action = str_or_empty(&expected, "action");
        let params = get_object(&expected, "params").cloned().unwrap_or_default();
        let expected_agent_id = expected.get("agentId").filter(|v| !v.is_null()).map(py_str);
        let expected_user_id = expected.get("userId").filter(|v| !v.is_null()).map(py_str);
        let expected_agent_key_id = expected.get("agentKeyId").filter(|v| !v.is_null()).map(py_str);
        let verification = verify_intent_token(
            &token,
            VerifyIntentToken {
                action: &action,
                params: Some(&params),
                expected_agent_id: expected_agent_id.as_deref(),
                expected_user_id: expected_user_id.as_deref(),
                expected_agent_key_id: expected_agent_key_id.as_deref(),
            },
        );
        if let Err(reason) = verification {
            let code = error_code(&reason, "TOKEN");
            return Ok(TokenVerificationResponse {
                valid: false,
                reason: Some(reason),
                code: Some(code),
                matched_scope: None,
            });
        }
        let usage = match self.consume_token_usage(&token) {
            Ok(Ok(usage)) => usage,
            Ok(Err(reason)) => {
                return Ok(TokenVerificationResponse {
                    valid: false,
                    reason: Some(reason),
                    code: Some("TOKEN_USAGE_EXCEEDED".into()),
                    matched_scope: None,
                });
            }
            Err(_) => {
                return Ok(TokenVerificationResponse {
                    valid: false,
                    reason: Some("Intent token usage store unavailable".into()),
                    code: Some("TOKEN_USAGE_STORE_ERROR".into()),
                    matched_scope: None,
                });
            }
        };
        let mut matched_scope = JsonDict::new();
        matched_scope.insert(
            "action".into(),
            expected.get("action").cloned().unwrap_or(Value::Null),
        );
        matched_scope.insert("params".into(), Value::Object(params));
        for (key, value) in usage {
            matched_scope.insert(key, value);
        }
        Ok(TokenVerificationResponse {
            valid: true,
            reason: None,
            code: None,
            matched_scope: Some(matched_scope),
        })
    }

    /// Returns `Ok(Ok(usage))` when consumed, `Ok(Err(reason))` when exhausted.
    fn consume_token_usage(
        &self,
        token: &JsonDict,
    ) -> Result<Result<JsonDict, String>, IntentTokenUsageStoreError> {
        let token_id = str_trimmed(token, "tokenId");
        if token_id.is_empty() {
            return Ok(Err("Token tokenId missing".into()));
        }
        let intent = match normalize_intent_scope(token.get("intent")) {
            Ok(intent) => intent,
            Err(error) => return Ok(Err(error.to_string())),
        };
        let Some(policy) = get_object(&intent, "executionPolicy") else {
            return Ok(Ok(JsonDict::new()));
        };
        let max_executions = policy.get("maxExecutions").and_then(as_int).unwrap_or(0);
        let expire_at = str_trimmed(token, "expireAt");
        let Some(expire_at_epoch) = parse_iso_z(&expire_at) else {
            return Err(IntentTokenUsageStoreError(
                "Intent token expiration is invalid".into(),
            ));
        };
        let (consumed, executions_used) =
            self.token_usage_store
                .consume(&token_id, max_executions, expire_at_epoch)?;
        if !consumed {
            return Ok(Err("Token execution usage exceeded".into()));
        }
        let mut usage = JsonDict::new();
        usage.insert("executionsUsed".into(), Value::from(executions_used));
        usage.insert("executionsLimit".into(), Value::from(max_executions));
        let mut wrapper = JsonDict::new();
        wrapper.insert("usage".into(), Value::Object(usage));
        Ok(Ok(wrapper))
    }
}

fn rejected(
    mandate: Option<JsonDict>,
    reason: impl Into<String>,
    code: impl Into<String>,
) -> IntentAuthorizationResponse {
    let reason = reason.into();
    IntentAuthorizationResponse {
        mandate,
        approved: false,
        reject_reason: Some(reason.clone()),
        verification_result: Some(VerificationResult::fail(reason, code)),
        ..Default::default()
    }
}
