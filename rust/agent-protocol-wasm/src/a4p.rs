// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A4P User Authorizer bindings.
//!
//! The User Authorizer is the component on the user's device that receives a
//! forwarded `{mandate, signingOptions}` request from the Agent, verifies the
//! A4P Server signature against a local trust store, checks validity, derives
//! the challenge, shows the verified content to the user and finally produces
//! `signatures.user`. Everything here is transport free: the Agent or the page
//! does the HTTP calls, the wasm module does the cryptography.

use a4p::canonical::canonical_json_value;
use a4p::errors::{A4PError, MandateSecurityError};
use a4p::intent::params_match_intent_scope;
use a4p::mandate_security::{
    StaticA4PServerTrustStore, mandate_identifier, user_authorization_challenge_base64url,
    verify_trusted_server_mandate,
};
use a4p::types::{JsonDict, UserAuthorizationRequest, UserAuthorizationResponse};
use a4p::user_authorizer::{
    approve_user_mandate, sign_user_mandate_with_signer, verify_local_user_authorization_request,
};
use a4p::user_signature::A4PUserSigner;
use a4p::user_signature::ed25519::{ED25519_SIGNATURE_METHOD, Ed25519UserSigner, ed25519_public_jwk};
use a4p::util::{b64url_decode, b64url_encode};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::convert::{from_js, object_from_js, optional_object_from_js, to_js};

fn security_error(e: MandateSecurityError) -> JsError {
    JsError::new(&format!("{}: {}", e.code, e.message))
}

fn a4p_error(e: A4PError) -> JsError {
    match e {
        A4PError::MandateSecurity(inner) => security_error(inner),
        A4PError::Protocol(inner) => JsError::new(&format!("{}: {}", inner.code, inner.message)),
        other => JsError::new(&other.to_string()),
    }
}

fn parse_request(request: JsValue) -> Result<UserAuthorizationRequest, JsError> {
    let obj = object_from_js(request, "authorization request")?;
    let mandate = match obj.get("mandate") {
        Some(Value::Object(m)) => m.clone(),
        _ => return Err(JsError::new("authorization request needs a `mandate` object")),
    };
    let signing_options = match obj.get("signingOptions") {
        Some(Value::Object(m)) => m.clone(),
        None | Some(Value::Null) => JsonDict::new(),
        _ => return Err(JsError::new("`signingOptions` must be an object when present")),
    };
    Ok(UserAuthorizationRequest {
        mandate,
        signing_options,
    })
}

/// Static trust anchors for A4P Servers: `{serverId: {keyId: {alg, publicKey}}}`.
///
/// This is the JSON the Rust and Python servers print as their trust
/// configuration. It must reach the device over a protected channel; never
/// accept trust keys forwarded by the Agent.
#[wasm_bindgen]
pub struct A4pTrustStore {
    inner: StaticA4PServerTrustStore,
}

#[wasm_bindgen]
impl A4pTrustStore {
    /// Build a trust store from a configuration object.
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<A4pTrustStore, JsError> {
        let config = object_from_js(config, "trust configuration")?;
        let inner = StaticA4PServerTrustStore::new(&config).map_err(a4p_error)?;
        Ok(A4pTrustStore { inner })
    }

    /// Build a trust store from JSON text.
    #[wasm_bindgen(js_name = fromJson)]
    pub fn from_json(text: &str) -> Result<A4pTrustStore, JsError> {
        let config: JsonDict = serde_json::from_str(text).map_err(|e| JsError::new(&e.to_string()))?;
        let inner = StaticA4PServerTrustStore::new(&config).map_err(a4p_error)?;
        Ok(A4pTrustStore { inner })
    }
}

/// An Ed25519 key pair owned by the user. The seed never leaves the module
/// unless the caller exports it explicitly with `seedBase64Url`.
#[wasm_bindgen]
pub struct Ed25519KeyPair {
    key: SigningKey,
}

#[wasm_bindgen]
impl Ed25519KeyPair {
    /// Generate a fresh random key pair.
    pub fn generate() -> Ed25519KeyPair {
        Ed25519KeyPair {
            key: a4p::security::generate_ed25519_private_key(),
        }
    }

    /// Restore a key pair from a base64url (unpadded) 32-byte seed.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed_base64url: &str) -> Result<Ed25519KeyPair, JsError> {
        let bytes =
            b64url_decode(seed_base64url.trim()).map_err(|e| JsError::new(&format!("invalid seed: {e}")))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| JsError::new("Ed25519 seed must be exactly 32 bytes"))?;
        Ok(Ed25519KeyPair {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// Export the private seed as base64url (unpadded). Handle with care.
    #[wasm_bindgen(js_name = seedBase64Url)]
    pub fn seed_base64url(&self) -> String {
        b64url_encode(&self.key.to_bytes())
    }

    /// The public key as an OKP JWK, the shape the registration endpoint expects.
    #[wasm_bindgen(js_name = publicKeyJwk)]
    pub fn public_key_jwk(&self) -> Result<JsValue, JsError> {
        to_js(&ed25519_public_jwk(&self.key))
    }

    /// The raw public key as base64url (unpadded).
    #[wasm_bindgen(js_name = publicKeyBase64Url)]
    pub fn public_key_base64url(&self) -> String {
        a4p::security::ed25519_public_key_to_base64url(&self.key.verifying_key())
    }
}

/// A user signer bound to a registered credential id.
#[wasm_bindgen]
pub struct Ed25519Signer {
    inner: Ed25519UserSigner,
}

#[wasm_bindgen]
impl Ed25519Signer {
    /// Bind a key pair to the credential id returned by the registration endpoint.
    #[wasm_bindgen(constructor)]
    pub fn new(key_pair: &Ed25519KeyPair, credential_id: &str) -> Result<Ed25519Signer, JsError> {
        let inner = Ed25519UserSigner::new(credential_id, key_pair.key.clone()).map_err(a4p_error)?;
        Ok(Ed25519Signer { inner })
    }

    /// The credential id this signer signs for.
    #[wasm_bindgen(js_name = credentialId)]
    pub fn credential_id(&self) -> String {
        self.inner.credential_id().to_string()
    }

    /// The signature method identifier (`ed25519`).
    #[wasm_bindgen(js_name = signatureMethod)]
    pub fn signature_method(&self) -> String {
        self.inner.signature_method().to_string()
    }
}

/// Verify a forwarded `{mandate, signingOptions}` request before showing it.
///
/// Runs the local processing order: trust lookup, Server signature, validity,
/// signature method, challenge re-derivation and `userVerification=required`.
/// Returns the hardened signing options to hand to the authenticator. Throws
/// `CODE: message` on any failure.
#[wasm_bindgen(js_name = verifyUserAuthorizationRequest)]
pub fn verify_user_authorization_request(
    request: JsValue,
    trust: &A4pTrustStore,
    expected_signature_method: Option<String>,
) -> Result<JsValue, JsError> {
    let request = parse_request(request)?;
    let options =
        verify_local_user_authorization_request(&request, &trust.inner, expected_signature_method.as_deref())
            .map_err(security_error)?;
    to_js(&options)
}

/// Verify only the Server signature and return the mandate core.
#[wasm_bindgen(js_name = verifyServerMandate)]
pub fn verify_server_mandate(mandate: JsValue, trust: &A4pTrustStore) -> Result<JsValue, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    let core = verify_trusted_server_mandate(&mandate, &trust.inner).map_err(security_error)?;
    to_js(&core)
}

/// Derive the WebAuthn challenge for a Server-signed mandate as base64url.
#[wasm_bindgen(js_name = deriveUserAuthorizationChallenge)]
pub fn derive_user_authorization_challenge(mandate: JsValue) -> Result<String, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    user_authorization_challenge_base64url(&mandate).map_err(a4p_error)
}

/// Sign a verified mandate with an Ed25519 signer, returning the signed mandate.
#[wasm_bindgen(js_name = signUserMandate)]
pub fn sign_user_mandate(mandate: JsValue, signer: &Ed25519Signer) -> Result<JsValue, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    let signed = sign_user_mandate_with_signer(&mandate, &signer.inner, None).map_err(a4p_error)?;
    to_js(&signed)
}

/// Attach a WebAuthn assertion produced by `navigator.credentials.get` as the
/// user signature. `assertion` is the JSON form of the `PublicKeyCredential`
/// (`id`, `rawId`, `type`, `response.{clientDataJSON, authenticatorData, signature, userHandle}`).
#[wasm_bindgen(js_name = attachWebAuthnAssertion)]
pub fn attach_webauthn_assertion(mandate: JsValue, assertion: JsValue) -> Result<JsValue, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    let assertion = object_from_js(assertion, "assertion")?;
    let signer = a4p::user_signature::webauthn::WebAuthnUserSigner::new();
    let mut input = JsonDict::new();
    input.insert("assertion".into(), Value::Object(assertion));
    let signed = sign_user_mandate_with_signer(&mandate, &signer, Some(&input)).map_err(a4p_error)?;
    to_js(&signed)
}

/// Return the mandate with an empty user signature (explicit no-signature mode).
#[wasm_bindgen(js_name = approveUserMandateUnsigned)]
pub fn approve_user_mandate_unsigned(mandate: JsValue) -> Result<JsValue, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    to_js(&approve_user_mandate(&mandate))
}

/// One-call User Authorizer for Ed25519: verify, then sign.
///
/// Never throws for protocol outcomes. Returns
/// `{approved: true, signedMandate}` or
/// `{approved: false, rejectReason, errorCode}` like `A4PUserAuthorizer`.
#[wasm_bindgen(js_name = authorizeWithEd25519)]
pub fn authorize_with_ed25519(
    request: JsValue,
    trust: &A4pTrustStore,
    signer: &Ed25519Signer,
) -> Result<JsValue, JsError> {
    let request = parse_request(request)?;
    let response =
        match verify_local_user_authorization_request(&request, &trust.inner, Some(ED25519_SIGNATURE_METHOD))
        {
            Ok(_) => match sign_user_mandate_with_signer(&request.mandate, &signer.inner, None) {
                Ok(signed) => UserAuthorizationResponse {
                    approved: true,
                    signed_mandate: Some(signed),
                    reject_reason: None,
                    error_code: None,
                },
                Err(e) => rejection(e),
            },
            Err(e) => UserAuthorizationResponse {
                approved: false,
                signed_mandate: None,
                reject_reason: Some(e.message),
                error_code: Some(e.code),
            },
        };
    to_js(&a4p::types::to_payload(&response))
}

fn rejection(e: A4PError) -> UserAuthorizationResponse {
    let (code, message) = match e {
        A4PError::MandateSecurity(inner) => (Some(inner.code), inner.message),
        A4PError::Protocol(inner) => (Some(inner.code), inner.message),
        other => (None, other.to_string()),
    };
    UserAuthorizationResponse {
        approved: false,
        signed_mandate: None,
        reject_reason: Some(message),
        error_code: code,
    }
}

/// Python-compatible canonical JSON (sorted keys, no whitespace, UTF-8).
#[wasm_bindgen(js_name = canonicalJson)]
pub fn canonical_json(value: JsValue) -> Result<String, JsError> {
    let value = from_js(value)?;
    canonical_json_value(&value).map_err(a4p_error)
}

/// The `mandateId` or `operationId` of a mandate.
#[wasm_bindgen(js_name = mandateIdentifier)]
pub fn mandate_identifier_js(mandate: JsValue) -> Result<String, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    mandate_identifier(&mandate).map_err(a4p_error)
}

/// Check whether `action` with `params` falls inside an intent scope.
///
/// Returns `{matches: true}` or `{matches: false, reason}`. Useful for a Tool
/// Server or a page that wants to pre-check a token before calling verify.
#[wasm_bindgen(js_name = paramsMatchIntentScope)]
pub fn params_match_intent_scope_js(
    intent: JsValue,
    action: &str,
    params: JsValue,
) -> Result<JsValue, JsError> {
    let intent = object_from_js(intent, "intent")?;
    let params = optional_object_from_js(params, "params")?;
    let result = match params_match_intent_scope(&intent, action, params.as_ref()) {
        Ok(()) => json!({"matches": true}),
        Err(reason) => json!({"matches": false, "reason": reason}),
    };
    to_js(&result)
}

/// Human readable summary of a mandate for display: type, id, subject,
/// display text and validity. Only call this after verification.
#[wasm_bindgen(js_name = describeMandate)]
pub fn describe_mandate(mandate: JsValue) -> Result<JsValue, JsError> {
    let mandate = object_from_js(mandate, "mandate")?;
    let id = mandate_identifier(&mandate).map_err(a4p_error)?;
    let summary = json!({
        "type": mandate.get("type"),
        "id": id,
        "server": mandate.get("server"),
        "subject": mandate.get("subject"),
        "displayText": mandate.get("displayText"),
        "validTime": mandate.get("validTime"),
        "signatureMethod": mandate.get("userAuthorization").and_then(|u| u.get("signatureMethod")),
    });
    to_js(&summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn keypair_roundtrip_and_jwk_shape() -> TestResult {
        let kp = Ed25519KeyPair::generate();
        let seed = kp.seed_base64url();
        let back = Ed25519KeyPair::from_seed(&seed)
            .map_err(|_| ap_support::testing::TestFailure::new("from_seed failed"))?;
        assert_eq!(kp.public_key_base64url(), back.public_key_base64url());
        let jwk = ed25519_public_jwk(&kp.key);
        assert_eq!(jwk["kty"], "OKP");
        assert_eq!(jwk["crv"], "Ed25519");
        Ok(())
    }

    // Error paths construct JavaScript `Error` values and can only run inside a
    // wasm host; see tests/wasm.rs for those.
    #[test]
    fn signer_binds_credential_id() -> TestResult {
        let kp = Ed25519KeyPair::generate();
        let signer = Ed25519Signer::new(&kp, "cred_1")
            .map_err(|_| ap_support::testing::TestFailure::new("signer construction failed"))?;
        assert_eq!(signer.credential_id(), "cred_1");
        assert_eq!(signer.signature_method(), "ed25519");
        Ok(())
    }
}
