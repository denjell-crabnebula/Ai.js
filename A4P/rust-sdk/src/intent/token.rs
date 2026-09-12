// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Intent-token issuance, verification and scope matching.

use serde_json::Value;

use crate::errors::A4PError;
use crate::intent::mandate::{VerifyIntentMandate, normalize_intent_mandate, verify_intent_mandate};
use crate::intent::scope::{normalize_intent_scope, params_match_intent_scope};
use crate::intent::signing::intent_server_signing_key;
use crate::mandate_security::canonical_json;
use crate::security::{ed25519_sign_text, ed25519_verify_text};
use crate::types::{IntentToken, JsonDict};
use crate::user_signature::A4PUserSignatureMethod;
use crate::util::{
    get_object, is_truthy, now_epoch, parse_iso_z, py_str, str_trimmed, token_hex, token_urlsafe,
};

/// Wire type of intent tokens.
pub const INTENT_TOKEN_TYPE: &str = "a4p/v1/intent-token";
const TOKEN_SIGN_ALG: &str = "EdDSA";
const TOKEN_KEY_ID: &str = "server#intent-token-v1";

fn intent_token_key_id(mandate_id: &str) -> String {
    let normalized = mandate_id.trim();
    if normalized.is_empty() {
        return TOKEN_KEY_ID.to_string();
    }
    format!("{TOKEN_KEY_ID}:{normalized}")
}

fn intent_token_core_payload(token: &JsonDict) -> Result<JsonDict, A4PError> {
    let field = |key: &str, default: Value| token.get(key).cloned().unwrap_or(default);
    let mut core = JsonDict::new();
    core.insert("type".into(), field("type", Value::String(String::new())));
    core.insert("tokenId".into(), field("tokenId", Value::String(String::new())));
    core.insert(
        "mandateId".into(),
        field("mandateId", Value::String(String::new())),
    );
    core.insert("subject".into(), field("subject", Value::Object(JsonDict::new())));
    core.insert("user".into(), field("user", Value::Object(JsonDict::new())));
    core.insert(
        "intent".into(),
        Value::Object(normalize_intent_scope(token.get("intent"))?),
    );
    core.insert("issuedAt".into(), field("issuedAt", Value::String(String::new())));
    core.insert("expireAt".into(), field("expireAt", Value::String(String::new())));
    core.insert("nonce".into(), field("nonce", Value::String(String::new())));
    Ok(core)
}

fn intent_token_signing_input(token: &JsonDict) -> Result<String, A4PError> {
    let mut payload = JsonDict::new();
    payload.insert("scope".into(), Value::String("server.intent_token".into()));
    payload.insert("token".into(), Value::Object(intent_token_core_payload(token)?));
    canonical_json(&payload)
}

/// Options of [`issue_intent_token`].
#[derive(Clone, Copy)]
pub struct IssueIntentToken<'a> {
    /// The user the token is bound to.
    pub user_id: &'a str,
    /// Skip mandate verification because the caller already verified it.
    pub verified_mandate: bool,
    /// Whether the mandate must carry a user signature.
    pub require_user_signature: bool,
    /// The method used to verify the user signature.
    pub user_signature_method: Option<&'a dyn A4PUserSignatureMethod>,
}

impl<'a> IssueIntentToken<'a> {
    /// Defaults matching the Python keyword arguments.
    pub fn new(user_id: &'a str) -> Self {
        Self {
            user_id,
            verified_mandate: false,
            require_user_signature: true,
            user_signature_method: None,
        }
    }
}

/// Issue a Server-signed intent token from a verified intent mandate.
pub fn issue_intent_token(
    mandate: &JsonDict,
    options: IssueIntentToken<'_>,
) -> Result<IntentToken, A4PError> {
    if !options.verified_mandate {
        verify_intent_mandate(
            mandate,
            VerifyIntentMandate {
                expected_server: None,
                expected_user_id: None,
                require_user_signature: options.require_user_signature,
                user_signature_method: options.user_signature_method,
            },
        )
        .map_err(|reason| A4PError::value(format!("invalid intent mandate: {reason}")))?;
    }

    let normalized = normalize_intent_mandate(mandate)?;
    let mandate_id = str_trimmed(&normalized, "mandateId");
    if mandate_id.is_empty() {
        return Err(A4PError::value("mandateId missing"));
    }
    let subject = get_object(&normalized, "subject").cloned().unwrap_or_default();
    let intent = get_object(&normalized, "intent").cloned().unwrap_or_default();
    let valid_time = get_object(&normalized, "validTime").cloned().unwrap_or_default();

    let issued_at = crate::util::format_iso_z(now_epoch());
    let expire_at = str_trimmed(&valid_time, "end");
    if expire_at.is_empty() {
        return Err(A4PError::value("mandate validTime.end missing"));
    }

    let mut token_subject = JsonDict::new();
    let subject_type = str_trimmed(&subject, "type");
    token_subject.insert(
        "type".into(),
        Value::String(if subject_type.is_empty() {
            "agent".into()
        } else {
            subject_type
        }),
    );
    token_subject.insert("id".into(), Value::String(str_trimmed(&subject, "id")));
    if let Some(agent_key) = get_object(&subject, "agentKey") {
        token_subject.insert("agentKey".into(), Value::Object(agent_key.clone()));
    }
    let mut user = JsonDict::new();
    let user_id = options.user_id.trim();
    user.insert(
        "id".into(),
        Value::String(if user_id.is_empty() {
            "user:unknown".into()
        } else {
            user_id.into()
        }),
    );

    let mut token = JsonDict::new();
    token.insert("type".into(), Value::String(INTENT_TOKEN_TYPE.into()));
    token.insert("tokenId".into(), Value::String(token_hex(16)));
    token.insert("mandateId".into(), Value::String(mandate_id.clone()));
    token.insert("subject".into(), Value::Object(token_subject));
    token.insert("user".into(), Value::Object(user));
    token.insert(
        "intent".into(),
        Value::Object(normalize_intent_scope(Some(&Value::Object(intent)))?),
    );
    token.insert("issuedAt".into(), Value::String(issued_at));
    token.insert("expireAt".into(), Value::String(expire_at));
    token.insert("nonce".into(), Value::String(token_urlsafe(16)));
    token.insert("signature".into(), Value::String(String::new()));
    token.insert("alg".into(), Value::String(TOKEN_SIGN_ALG.into()));
    token.insert("keyId".into(), Value::String(intent_token_key_id(&mandate_id)));
    let signature = ed25519_sign_text(
        &intent_token_signing_input(&token)?,
        &intent_server_signing_key()?,
    );
    token.insert("signature".into(), Value::String(signature));
    Ok(token)
}

/// Match `action` and `params` against the token intent scope only.
pub fn params_match_intent_token(
    token: &JsonDict,
    action: &str,
    params: Option<&JsonDict>,
) -> Result<(), String> {
    let intent = get_object(token, "intent").cloned().unwrap_or_default();
    params_match_intent_scope(&intent, action, params)
}

/// Options of [`verify_intent_token`].
#[derive(Clone, Copy, Default)]
pub struct VerifyIntentToken<'a> {
    /// The action about to be executed.
    pub action: &'a str,
    /// The parameters about to be used.
    pub params: Option<&'a JsonDict>,
    /// Expected `subject.id`, when checked.
    pub expected_agent_id: Option<&'a str>,
    /// Expected `user.id`, when checked.
    pub expected_user_id: Option<&'a str>,
    /// Expected `subject.agentKey.kid` (or `keyId`), when checked.
    pub expected_agent_key_id: Option<&'a str>,
}

impl<'a> VerifyIntentToken<'a> {
    /// Verify only action and params.
    pub fn new(action: &'a str, params: Option<&'a JsonDict>) -> Self {
        Self {
            action,
            params,
            ..Default::default()
        }
    }
}

/// Stateless token verification: type, alg, keyId, signature, expiry, identity binding and scope.
///
/// This never consumes `maxExecutions`; use `A4PServer::verify_intent_token` for that.
pub fn verify_intent_token(token: &JsonDict, options: VerifyIntentToken<'_>) -> Result<(), String> {
    if token.get("type").and_then(Value::as_str) != Some(INTENT_TOKEN_TYPE) {
        return Err("Invalid token type".into());
    }
    let token_alg = str_trimmed(token, "alg");
    if token_alg != TOKEN_SIGN_ALG {
        return Err(format!(
            "Token alg mismatch: expected '{TOKEN_SIGN_ALG}', got '{token_alg}'"
        ));
    }
    let signature = str_trimmed(token, "signature");
    if signature.is_empty() {
        return Err("Token signature missing".into());
    }
    let mandate_id = str_trimmed(token, "mandateId");
    if mandate_id.is_empty() {
        return Err("Token mandateId missing".into());
    }
    let key_id = str_trimmed(token, "keyId");
    let expected_key_id = intent_token_key_id(&mandate_id);
    if key_id != expected_key_id {
        return Err(format!(
            "Token keyId mismatch: expected '{expected_key_id}', got '{key_id}'"
        ));
    }
    let token_payload = intent_token_signing_input(token).map_err(|error| error.to_string())?;
    let key =
        intent_server_signing_key().map_err(|error| format!("Server signing key unavailable: {error}"))?;
    if !ed25519_verify_text(&token_payload, &signature, &key.verifying_key()) {
        return Err("Token signature invalid".into());
    }
    verify_intent_token_after_signature(token, options)
}

/// The checks that follow signature verification: expiry, identity binding and scope.
///
/// Exposed so tests can exercise the expiry and binding rules without a valid signature.
pub fn verify_intent_token_after_signature(
    token: &JsonDict,
    options: VerifyIntentToken<'_>,
) -> Result<(), String> {
    let expire_at = str_trimmed(token, "expireAt");
    if expire_at.is_empty() {
        return Err("Token expireAt missing".into());
    }
    let Some(expire_ts) = parse_iso_z(&expire_at) else {
        return Err("Token expireAt format invalid".into());
    };
    if now_epoch() > expire_ts {
        return Err("Token expired".into());
    }
    let subject = get_object(token, "subject").cloned().unwrap_or_default();
    let user = get_object(token, "user").cloned().unwrap_or_default();
    if let Some(expected_agent_id) = options.expected_agent_id.filter(|value| !value.is_empty()) {
        let actual_agent_id = str_trimmed(&subject, "id");
        if actual_agent_id != expected_agent_id {
            return Err(format!(
                "Token subject mismatch: expected '{expected_agent_id}', got '{actual_agent_id}'"
            ));
        }
    }
    if let Some(expected_agent_key_id) = options.expected_agent_key_id.filter(|value| !value.is_empty()) {
        let agent_key = get_object(&subject, "agentKey").cloned().unwrap_or_default();
        let actual_key_id = match agent_key.get("kid").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => str_or_empty_key(&agent_key),
        }
        .trim()
        .to_string();
        if actual_key_id != expected_agent_key_id {
            return Err(format!(
                "Token agent key mismatch: expected '{expected_agent_key_id}', got '{actual_key_id}'"
            ));
        }
    }
    if let Some(expected_user_id) = options.expected_user_id.filter(|value| !value.is_empty()) {
        let actual_user_id = str_trimmed(&user, "id");
        if actual_user_id != expected_user_id {
            return Err(format!(
                "Token user mismatch: expected '{expected_user_id}', got '{actual_user_id}'"
            ));
        }
    }
    params_match_intent_token(token, options.action, options.params)
}

fn str_or_empty_key(agent_key: &JsonDict) -> String {
    crate::util::str_or_empty(agent_key, "keyId")
}
