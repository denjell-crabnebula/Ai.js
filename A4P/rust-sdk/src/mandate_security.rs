// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared mandate challenge binding and local A4P Server trust verification.

use std::collections::HashMap;
use std::path::Path;

use ed25519_dalek::VerifyingKey;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use crate::canonical::{canonical_json, canonical_json_value};
use crate::errors::{A4PError, MandateSecurityError};
use crate::security::{ed25519_public_key_from_base64url, ed25519_verify_text};
use crate::types::JsonDict;
use crate::util::{b64url_encode, get_object, is_truthy, now_epoch, parse_iso_z, py_str, str_trimmed};

/// Scope string of the common user-signature payload.
pub const USER_AUTHORIZATION_SCOPE: &str = "a4p/v1/user-authorization";
/// Algorithm name used by every A4P Server signature.
pub const SERVER_SIGNATURE_ALGORITHM: &str = "EdDSA";
/// Wire type of intent mandates.
pub const INTENT_MANDATE_TYPE: &str = "a4p/v1/intent-mandate";
/// Wire type of operation mandates.
pub const OPERATION_MANDATE_TYPE: &str = "a4p/v1/operation-mandate";

/// Static local trust anchors indexed by `serverId` and server signature `keyId`.
#[derive(Debug, Clone)]
pub struct StaticA4PServerTrustStore {
    trusted: HashMap<(String, String), (String, VerifyingKey)>,
}

impl StaticA4PServerTrustStore {
    /// Build a trust store from `{serverId: {keyId: {alg, publicKey}}}`.
    pub fn new(config: &JsonDict) -> Result<Self, A4PError> {
        let mut trusted = HashMap::new();
        for (server_id, keys_value) in config {
            if server_id.trim().is_empty() {
                return Err(A4PError::value("Trusted A4P serverId must be a non-empty string"));
            }
            let keys = match keys_value.as_object() {
                Some(keys) if !keys.is_empty() => keys,
                _ => {
                    return Err(A4PError::value(format!(
                        "Trusted A4P server '{server_id}' must contain keys"
                    )));
                }
            };
            for (key_id, key_value) in keys {
                if key_id.trim().is_empty() {
                    return Err(A4PError::value("Trusted A4P keyId must be a non-empty string"));
                }
                let key = key_value.as_object().ok_or_else(|| {
                    A4PError::value(format!("Trusted A4P key '{key_id}' must be an object"))
                })?;
                let alg = str_trimmed(key, "alg");
                if alg != SERVER_SIGNATURE_ALGORITHM {
                    return Err(A4PError::value(format!(
                        "Trusted A4P key '{key_id}' must use '{SERVER_SIGNATURE_ALGORITHM}'"
                    )));
                }
                let public_key = ed25519_public_key_from_base64url(&str_trimmed(key, "publicKey"))?;
                trusted.insert(
                    (server_id.trim().to_string(), key_id.trim().to_string()),
                    (alg, public_key),
                );
            }
        }
        if trusted.is_empty() {
            return Err(A4PError::value(
                "Trusted A4P server key configuration must not be empty",
            ));
        }
        Ok(Self { trusted })
    }

    /// Load the trust configuration from a JSON file.
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self, A4PError> {
        let text = std::fs::read_to_string(path.as_ref()).map_err(|error| {
            A4PError::runtime(format!("Cannot read trusted A4P server key file: {error}"))
        })?;
        let payload: Value = serde_json::from_str(&text).map_err(|error| {
            A4PError::value(format!("Trusted A4P server key file is not valid JSON: {error}"))
        })?;
        match payload.as_object() {
            Some(map) => Self::new(map),
            None => Err(A4PError::value(
                "Trusted A4P server key file must contain a JSON object",
            )),
        }
    }

    /// Resolve the trusted public key for a server and key id.
    pub fn resolve(
        &self,
        server_id: &str,
        key_id: &str,
        alg: &str,
    ) -> Result<VerifyingKey, MandateSecurityError> {
        let Some((trusted_alg, public_key)) = self.trusted.get(&(server_id.to_string(), key_id.to_string()))
        else {
            return Err(MandateSecurityError::new(
                "SERVER_KEY_UNTRUSTED",
                format!("A4P Server key is not trusted: server='{server_id}', keyId='{key_id}'"),
            ));
        };
        if alg != trusted_alg {
            return Err(MandateSecurityError::new(
                "SERVER_KEY_UNTRUSTED",
                format!("A4P Server signature algorithm is not trusted: '{alg}'"),
            ));
        }
        Ok(*public_key)
    }
}

/// Normalize either A4P mandate type and return its complete signed core.
pub fn mandate_core_payload(mandate: &JsonDict) -> Result<JsonDict, MandateSecurityError> {
    match mandate.get("type").and_then(Value::as_str) {
        Some(INTENT_MANDATE_TYPE) => crate::intent::mandate::intent_mandate_core_payload(mandate)
            .map_err(|error| MandateSecurityError::new("SERVER_SIGNATURE_INVALID", error.to_string())),
        Some(OPERATION_MANDATE_TYPE) => crate::operation::mandate::operation_mandate_core_payload(mandate)
            .map_err(|error| MandateSecurityError::new("SERVER_SIGNATURE_INVALID", error.to_string())),
        _ => Err(MandateSecurityError::new(
            "SERVER_SIGNATURE_INVALID",
            format!(
                "Unsupported A4P mandate type: {}",
                python_repr(mandate.get("type"))
            ),
        )),
    }
}

/// Return the authorization identifier carried by either mandate type.
pub fn mandate_identifier(mandate: &JsonDict) -> Result<String, A4PError> {
    let mandate_type = mandate.get("type").and_then(Value::as_str);
    let field_name = match mandate_type {
        Some(INTENT_MANDATE_TYPE) => "mandateId",
        Some(OPERATION_MANDATE_TYPE) => "operationId",
        _ => {
            return Err(A4PError::value(format!(
                "Unsupported A4P mandate type: {}",
                python_repr(mandate.get("type"))
            )));
        }
    };
    let identifier = str_trimmed(mandate, field_name);
    if identifier.is_empty() {
        return Err(A4PError::value(format!("{field_name} missing")));
    }
    Ok(identifier)
}

/// Return the canonical WebAuthn input: the signed mandate with no user signature.
pub fn server_signed_mandate_without_user_signature(mandate: &JsonDict) -> Result<JsonDict, A4PError> {
    let mut core = mandate_core_payload(mandate)?;
    let server_signature = get_object(mandate, "signatures")
        .and_then(|signatures| get_object(signatures, "server"))
        .ok_or_else(|| A4PError::value("Server signature missing"))?;
    let mut signatures = JsonDict::new();
    signatures.insert("server".into(), Value::Object(server_signature.clone()));
    core.insert("signatures".into(), Value::Object(signatures));
    Ok(core)
}

/// Return the common proof input for all user-signature methods.
pub fn canonical_user_authorization_payload(mandate: &JsonDict) -> Result<String, A4PError> {
    let mut payload = JsonDict::new();
    payload.insert("scope".into(), Value::String(USER_AUTHORIZATION_SCOPE.into()));
    payload.insert(
        "mandate".into(),
        Value::Object(server_signed_mandate_without_user_signature(mandate)?),
    );
    canonical_json(&payload)
}

/// Return the SHA-256 WebAuthn challenge for the common proof input.
pub fn derive_user_authorization_challenge(mandate: &JsonDict) -> Result<[u8; 32], A4PError> {
    let payload = canonical_user_authorization_payload(mandate)?;
    Ok(Sha256::digest(payload.as_bytes()).into())
}

/// Return the derived challenge as unpadded base64url.
pub fn user_authorization_challenge_base64url(mandate: &JsonDict) -> Result<String, A4PError> {
    Ok(b64url_encode(&derive_user_authorization_challenge(mandate)?))
}

/// Verify a mandate using only locally configured A4P Server trust anchors.
///
/// Returns the mandate core on success.
pub fn verify_trusted_server_mandate(
    mandate: &JsonDict,
    trust_store: &StaticA4PServerTrustStore,
) -> Result<JsonDict, MandateSecurityError> {
    let core = mandate_core_payload(mandate)?;
    let server_id = str_trimmed(&core, "server");
    let server_signature = get_object(mandate, "signatures")
        .and_then(|signatures| get_object(signatures, "server"))
        .ok_or_else(|| MandateSecurityError::new("SERVER_SIGNATURE_INVALID", "Server signature missing"))?;
    let alg = str_trimmed(server_signature, "alg");
    let key_id = str_trimmed(server_signature, "keyId");
    let signature = str_trimmed(server_signature, "signature");
    if server_id.is_empty() || alg.is_empty() || key_id.is_empty() || signature.is_empty() {
        return Err(MandateSecurityError::new(
            "SERVER_SIGNATURE_INVALID",
            "Server signature metadata missing",
        ));
    }
    let public_key = trust_store.resolve(&server_id, &key_id, &alg)?;
    let payload = server_signature_payload(&core)
        .map_err(|error| MandateSecurityError::new("SERVER_SIGNATURE_INVALID", error.to_string()))?;
    if !ed25519_verify_text(&payload, &signature, &public_key) {
        return Err(MandateSecurityError::new(
            "SERVER_SIGNATURE_INVALID",
            "Server signature invalid",
        ));
    }
    Ok(core)
}

/// Return the canonical `{"scope": "server", "mandate": core}` signing input.
pub fn server_signature_payload(core: &JsonDict) -> Result<String, A4PError> {
    let mut payload = JsonDict::new();
    payload.insert("scope".into(), Value::String("server".into()));
    payload.insert("mandate".into(), Value::Object(core.clone()));
    canonical_json(&payload)
}

/// Perform the local fail-closed validity check for either mandate type.
pub fn verify_mandate_valid_time(mandate_core: &JsonDict) -> Result<(), MandateSecurityError> {
    verify_mandate_valid_time_at(mandate_core, now_epoch())
}

/// Validity check against an explicit UNIX time.
pub fn verify_mandate_valid_time_at(mandate_core: &JsonDict, now: i64) -> Result<(), MandateSecurityError> {
    let valid_time = get_object(mandate_core, "validTime")
        .ok_or_else(|| MandateSecurityError::new("CHALLENGE_BINDING_INVALID", "Mandate validTime missing"))?;
    let is_intent = mandate_core.get("type").and_then(Value::as_str) == Some(INTENT_MANDATE_TYPE);
    let fields: &[&str] = if is_intent { &["start", "end"] } else { &["until"] };
    let mut timestamps = Vec::new();
    for field in fields {
        let Some(value) = valid_time.get(*field).filter(|value| is_truthy(value)) else {
            continue;
        };
        let parsed = parse_iso_z(&py_str(value)).ok_or_else(|| {
            MandateSecurityError::new("CHALLENGE_BINDING_INVALID", "Mandate validTime format invalid")
        })?;
        timestamps.push(parsed);
    }
    if timestamps.len() != fields.len() {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "Mandate validTime missing",
        ));
    }
    if is_intent && now < timestamps[0] {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "Mandate not yet valid",
        ));
    }
    if now > timestamps[timestamps.len() - 1] {
        return Err(MandateSecurityError::new(
            "CHALLENGE_BINDING_INVALID",
            "Mandate has expired",
        ));
    }
    Ok(())
}

/// Python `repr()` of an optional JSON value, used in error messages.
pub(crate) fn python_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(text)) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Some(other) => py_str(other),
    }
}
