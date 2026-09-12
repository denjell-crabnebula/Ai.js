// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Registered Ed25519 signature method and User Authorizer signer.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use serde_json::Value;

use crate::credential_store::{A4PCredentialStore, UserCredentialRecord, utc_now_iso};
use crate::errors::{A4PError, A4PProtocolError};
use crate::security::{
    ed25519_public_key_from_base64url, ed25519_public_key_to_base64url, ed25519_sign_text,
    ed25519_verify_text,
};
use crate::types::JsonDict;
use crate::user_signature::contracts::{
    A4PUserSignatureMethod, A4PUserSigner, UserSignature, UserSignatureContext, UserSigningInput,
    canonical_user_authorization_payload,
};
use crate::util::{str_trimmed, token_urlsafe};

/// Identifier of the Ed25519 user-signature method.
pub const ED25519_SIGNATURE_METHOD: &str = "ed25519";

/// Return the OKP JWK of an Ed25519 private key.
pub fn ed25519_public_jwk(private_key: &SigningKey) -> JsonDict {
    let mut jwk = JsonDict::new();
    jwk.insert("kty".into(), Value::String("OKP".into()));
    jwk.insert("crv".into(), Value::String("Ed25519".into()));
    jwk.insert(
        "x".into(),
        Value::String(ed25519_public_key_to_base64url(&private_key.verifying_key())),
    );
    jwk.insert("alg".into(), Value::String("EdDSA".into()));
    jwk
}

fn normalize_public_jwk(value: Option<&Value>) -> Result<JsonDict, A4PError> {
    let Some(Value::Object(map)) = value else {
        return Err(A4PError::value("publicKey must be an OKP JWK object"));
    };
    if map.contains_key("d") {
        return Err(A4PError::value("publicKey must not contain private key material"));
    }
    if map.get("kty").and_then(Value::as_str) != Some("OKP") {
        return Err(A4PError::value("publicKey.kty must be 'OKP'"));
    }
    if map.get("crv").and_then(Value::as_str) != Some("Ed25519") {
        return Err(A4PError::value("publicKey.crv must be 'Ed25519'"));
    }
    if map.contains_key("alg") && map.get("alg").and_then(Value::as_str) != Some("EdDSA") {
        return Err(A4PError::value("publicKey.alg must be 'EdDSA'"));
    }
    let encoded_key = str_trimmed(map, "x");
    ed25519_public_key_from_base64url(&encoded_key)?;
    let mut normalized = JsonDict::new();
    normalized.insert("kty".into(), Value::String("OKP".into()));
    normalized.insert("crv".into(), Value::String("Ed25519".into()));
    normalized.insert("x".into(), Value::String(encoded_key));
    normalized.insert("alg".into(), Value::String("EdDSA".into()));
    Ok(normalized)
}

/// Register and verify user-owned Ed25519 public keys.
pub struct RegisteredEd25519Method {
    credential_store: Arc<dyn A4PCredentialStore>,
}

impl RegisteredEd25519Method {
    /// Create the method on top of a credential store.
    pub fn new(credential_store: Arc<dyn A4PCredentialStore>) -> Self {
        Self { credential_store }
    }

    /// The credential store used by this method.
    pub fn credential_store(&self) -> &Arc<dyn A4PCredentialStore> {
        &self.credential_store
    }

    /// Register `{"userId", "publicKey", "metadata"}` and return `{registered, created, credential}`.
    ///
    /// Registration is idempotent for the same user and key. Reusing a key
    /// across users fails with `CREDENTIAL_KEY_CONFLICT`.
    pub fn register(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        let user_id = str_trimmed(request, "userId");
        if user_id.is_empty() {
            return Err(A4PError::value("userId missing"));
        }
        let public_key = normalize_public_jwk(request.get("publicKey"))?;
        let metadata = match request.get("metadata") {
            None | Some(Value::Null) => JsonDict::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => return Err(A4PError::value("metadata must be an object")),
        };

        let matching = self.credential_store.list_all()?.into_iter().find(|record| {
            record.signature_method == ED25519_SIGNATURE_METHOD && record.public_key == public_key
        });
        if let Some(matching) = matching {
            if matching.user_id != user_id {
                return Err(A4PProtocolError::credential_key_conflict().into());
            }
            return Ok(registration_response(false, &matching));
        }

        let record = UserCredentialRecord {
            user_id,
            credential_id: format!("cred_{}", token_urlsafe(32)),
            signature_method: ED25519_SIGNATURE_METHOD.into(),
            public_key,
            details: JsonDict::new(),
            metadata,
            created_at: utc_now_iso(),
        };
        self.credential_store.save(record.clone())?;
        Ok(registration_response(true, &record))
    }
}

fn registration_response(created: bool, record: &UserCredentialRecord) -> JsonDict {
    let mut response = JsonDict::new();
    response.insert("registered".into(), Value::Bool(true));
    response.insert("created".into(), Value::Bool(created));
    response.insert("credential".into(), Value::Object(record.to_json()));
    response
}

impl A4PUserSignatureMethod for RegisteredEd25519Method {
    fn signature_method(&self) -> &str {
        ED25519_SIGNATURE_METHOD
    }

    fn method_policy(&self) -> JsonDict {
        JsonDict::new()
    }

    fn signing_options(&self, user_id: &str, _mandate: &JsonDict) -> Result<JsonDict, A4PError> {
        let credential_ids: Vec<Value> = self
            .credential_store
            .list_for_user(user_id)?
            .into_iter()
            .filter(|record| record.signature_method == ED25519_SIGNATURE_METHOD)
            .map(|record| Value::String(record.credential_id))
            .collect();
        if credential_ids.is_empty() {
            return Err(
                A4PProtocolError::user_credential_not_registered(user_id, ED25519_SIGNATURE_METHOD).into(),
            );
        }
        let mut method_options = JsonDict::new();
        method_options.insert("allowedCredentialIds".into(), Value::Array(credential_ids));
        let mut options = JsonDict::new();
        options.insert(
            "signatureMethod".into(),
            Value::String(ED25519_SIGNATURE_METHOD.into()),
        );
        options.insert("methodOptions".into(), Value::Object(method_options));
        Ok(options)
    }

    fn verify(&self, context: &UserSignatureContext, signature: &JsonDict) -> Result<(), String> {
        let credential_id = str_trimmed(signature, "credentialId");
        if credential_id.is_empty() {
            return Err("User credentialId missing".into());
        }
        let record = self
            .credential_store
            .get(&credential_id)
            .map_err(|error| format!("User credential store error: {error}"))?;
        let Some(record) = record else {
            return Err(format!("User credential not registered: {credential_id}"));
        };
        if record.signature_method != ED25519_SIGNATURE_METHOD {
            return Err("User credential signature method mismatch".into());
        }
        if let Some(expected_user_id) = &context.expected_user_id {
            if &record.user_id != expected_user_id {
                return Err(format!(
                    "User credential user mismatch: expected '{expected_user_id}', got '{}'",
                    record.user_id
                ));
            }
        }
        let Some(Value::Object(proof)) = signature.get("proof") else {
            return Err("User signature proof missing".into());
        };
        if proof.get("alg").and_then(Value::as_str) != Some("EdDSA") {
            return Err("User signature proof alg must be 'EdDSA'".into());
        }
        let encoded_signature = str_trimmed(proof, "signature");
        if encoded_signature.is_empty() {
            return Err("User signature missing".into());
        }
        let public_key = ed25519_public_key_from_base64url(&str_trimmed(&record.public_key, "x"))
            .map_err(|error| format!("Registered Ed25519 public key invalid: {error}"))?;
        let payload = canonical_user_authorization_payload(context).map_err(|error| error.to_string())?;
        if ed25519_verify_text(&payload, &encoded_signature, &public_key) {
            Ok(())
        } else {
            Err("User signature invalid".into())
        }
    }

    fn ed25519_registrar(&self) -> Option<&RegisteredEd25519Method> {
        Some(self)
    }
}

/// Sign with a caller-managed registered Ed25519 private key.
#[derive(Clone)]
pub struct Ed25519UserSigner {
    credential_id: String,
    private_key: SigningKey,
}

impl std::fmt::Debug for Ed25519UserSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519UserSigner")
            .field("credential_id", &self.credential_id)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl Ed25519UserSigner {
    /// Create a signer for a registered credential id.
    pub fn new(credential_id: &str, private_key: SigningKey) -> Result<Self, A4PError> {
        let normalized_id = credential_id.trim();
        if normalized_id.is_empty() {
            return Err(A4PError::value("credential_id missing"));
        }
        Ok(Self {
            credential_id: normalized_id.to_string(),
            private_key,
        })
    }

    /// The registered credential id.
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    /// The caller-managed private key.
    pub fn private_key(&self) -> &SigningKey {
        &self.private_key
    }
}

impl A4PUserSigner for Ed25519UserSigner {
    fn signature_method(&self) -> &str {
        ED25519_SIGNATURE_METHOD
    }

    fn sign(
        &self,
        context: &UserSignatureContext,
        _signing_input: Option<&UserSigningInput>,
    ) -> Result<UserSignature, A4PError> {
        let payload = canonical_user_authorization_payload(context)?;
        let mut proof = JsonDict::new();
        proof.insert("alg".into(), Value::String("EdDSA".into()));
        proof.insert(
            "signature".into(),
            Value::String(ed25519_sign_text(&payload, &self.private_key)),
        );
        let mut signature = JsonDict::new();
        signature.insert(
            "signatureMethod".into(),
            Value::String(ED25519_SIGNATURE_METHOD.into()),
        );
        signature.insert("credentialId".into(), Value::String(self.credential_id.clone()));
        signature.insert("proof".into(), Value::Object(proof));
        Ok(signature)
    }
}
