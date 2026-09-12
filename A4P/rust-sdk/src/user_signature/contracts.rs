// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Carrier-neutral user-signature contracts and shared helpers.

use serde_json::Value;

use crate::errors::A4PError;
use crate::mandate_security;
use crate::types::JsonDict;
use crate::util::str_trimmed;

/// A user signature envelope: `signatureMethod`, `credentialId` and `proof`.
pub type UserSignature = JsonDict;
/// Method specific signing input, for example a WebAuthn assertion.
pub type UserSigningInput = JsonDict;

/// The complete Server-signed mandate and expected user binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSignatureContext {
    /// `a4p/v1/intent-mandate` or `a4p/v1/operation-mandate`.
    pub mandate_type: String,
    /// Mandate core plus `signatures.server`, without `signatures.user`.
    pub server_signed_mandate: JsonDict,
    /// The signature method the mandate requires.
    pub signature_method: String,
    /// The user the mandate was prepared for, when known.
    pub expected_user_id: Option<String>,
}

/// Return the common proof input used by every signature method.
pub fn canonical_user_authorization_payload(context: &UserSignatureContext) -> Result<String, A4PError> {
    mandate_security::canonical_user_authorization_payload(&context.server_signed_mandate)
}

/// Return the WebAuthn challenge for the common proof input.
pub fn user_authorization_challenge(context: &UserSignatureContext) -> Result<[u8; 32], A4PError> {
    mandate_security::derive_user_authorization_challenge(&context.server_signed_mandate)
}

/// User Authorizer-side signer that creates one proof envelope.
pub trait A4PUserSigner: Send + Sync {
    /// The signature method identifier, for example `ed25519`.
    fn signature_method(&self) -> &str;

    /// Create a user signature envelope for the approved mandate.
    fn sign(
        &self,
        context: &UserSignatureContext,
        signing_input: Option<&UserSigningInput>,
    ) -> Result<UserSignature, A4PError>;
}

/// One Server-side user-signature method selected for an `A4PServer`.
pub trait A4PUserSignatureMethod: Send + Sync {
    /// The signature method identifier, for example `webauthn`.
    fn signature_method(&self) -> &str;

    /// Return the Server-signed policy for this method.
    fn method_policy(&self) -> JsonDict;

    /// Return method-specific options for the User Authorizer.
    ///
    /// Returns `A4PProtocolError` with code `USER_CREDENTIAL_NOT_REGISTERED`
    /// when the user has no credential for this method.
    fn signing_options(&self, user_id: &str, mandate: &JsonDict) -> Result<JsonDict, A4PError>;

    /// Verify one user proof and its credential binding.
    fn verify(&self, context: &UserSignatureContext, signature: &JsonDict) -> Result<(), String>;

    /// The Ed25519 registrar behind this method, when it supports registration.
    fn ed25519_registrar(&self) -> Option<&crate::user_signature::ed25519::RegisteredEd25519Method> {
        None
    }

    /// The WebAuthn registrar behind this method, when it supports registration.
    fn webauthn_registrar(&self) -> Option<&crate::user_signature::webauthn::WebAuthnSignatureMethod> {
        None
    }
}

/// Return a copy of the mandate with `signatures.user` replaced.
pub fn attach_user_signature(mandate: &JsonDict, signature: &JsonDict) -> JsonDict {
    let mut signed = mandate.clone();
    let mut signatures = match signed.get("signatures") {
        Some(Value::Object(map)) => map.clone(),
        _ => JsonDict::new(),
    };
    signatures.insert("user".into(), Value::Object(signature.clone()));
    signed.insert("signatures".into(), Value::Object(signatures));
    signed
}

/// Ask a user signer for a proof and check the method identifiers agree.
pub fn sign_user_signature(
    context: &UserSignatureContext,
    user_signer: &dyn A4PUserSigner,
    signing_input: Option<&UserSigningInput>,
) -> Result<UserSignature, A4PError> {
    if user_signer.signature_method() != context.signature_method {
        return Err(A4PError::value(format!(
            "User signer method mismatch: mandate requires '{}', got '{}'",
            context.signature_method,
            user_signer.signature_method()
        )));
    }
    let signature = user_signer.sign(context, signing_input)?;
    if signature.get("signatureMethod").and_then(Value::as_str) != Some(context.signature_method.as_str()) {
        return Err(A4PError::value(
            "User signer returned a mismatched signatureMethod",
        ));
    }
    Ok(signature)
}

/// Verify `signatures.user` against the configured method and policy.
pub fn verify_user_signature(
    context: &UserSignatureContext,
    signature: &JsonDict,
    method: Option<&dyn A4PUserSignatureMethod>,
    require_user_signature: bool,
) -> Result<(), String> {
    if !require_user_signature {
        return if signature.is_empty() {
            Ok(())
        } else {
            Err("User signature must be empty".into())
        };
    }
    let Some(method) = method else {
        return Err("User signature method missing".into());
    };
    let signature_method = str_trimmed(signature, "signatureMethod");
    if signature_method.is_empty() {
        return Err("User signature missing".into());
    }
    if signature_method != context.signature_method {
        return Err(format!(
            "User signature method mismatch: mandate requires '{}', got '{}'",
            context.signature_method, signature_method
        ));
    }
    if method.signature_method() != context.signature_method {
        return Err(format!(
            "Configured user signature method mismatch: mandate requires '{}', configured method is '{}'",
            context.signature_method,
            method.signature_method()
        ));
    }
    method.verify(context, signature)
}
