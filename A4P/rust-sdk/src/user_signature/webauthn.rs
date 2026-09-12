// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! WebAuthn method and user signer for A4P user authorization.
//!
//! The Server-side verification is implemented natively: attestation objects
//! are parsed with `ciborium`, COSE keys support ES256 (-7), EdDSA (-8) and
//! RS256 (-257), and attestation formats `none` and packed self-attestation.

use std::collections::HashMap;
use std::sync::Arc;

use ciborium::Value as CborValue;
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::credential_store::{A4PCredentialStore, UserCredentialRecord, utc_now_iso};
use crate::errors::{A4PError, A4PProtocolError};
use crate::mandate_security::derive_user_authorization_challenge;
use crate::types::JsonDict;
use crate::user_signature::contracts::{
    A4PUserSignatureMethod, A4PUserSigner, UserSignature, UserSignatureContext, UserSigningInput,
};
pub use crate::util::{b64url_decode, b64url_encode};
use crate::util::{get_object, py_str, random_bytes, str_or_empty, str_trimmed, token_urlsafe};

/// Identifier of the WebAuthn user-signature method.
pub const WEBAUTHN_SIGNATURE_METHOD: &str = "webauthn";

/// COSE algorithm identifier of ES256.
pub const COSE_ALG_ES256: i64 = -7;
/// COSE algorithm identifier of EdDSA.
pub const COSE_ALG_EDDSA: i64 = -8;
/// COSE algorithm identifier of RS256.
pub const COSE_ALG_RS256: i64 = -257;

/// Algorithms advertised in registration options and accepted at registration.
pub const SUPPORTED_COSE_ALGORITHMS: [i64; 3] = [COSE_ALG_ES256, COSE_ALG_EDDSA, COSE_ALG_RS256];

const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// A WebAuthn ceremony verification failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct WebAuthnError(pub String);

fn err(message: impl Into<String>) -> WebAuthnError {
    WebAuthnError(message.into())
}

/// Authenticator data flags byte, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AuthenticatorDataFlags {
    /// User present.
    pub up: bool,
    /// User verified.
    pub uv: bool,
    /// Backup eligible.
    pub be: bool,
    /// Backup state.
    pub bs: bool,
    /// Attested credential data included.
    pub at: bool,
    /// Extension data included.
    pub ed: bool,
}

/// Attested credential data carried by registration authenticator data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedCredentialData {
    /// Authenticator model identifier, 16 bytes.
    pub aaguid: Vec<u8>,
    /// Credential id bytes.
    pub credential_id: Vec<u8>,
    /// CBOR encoded COSE public key.
    pub credential_public_key: Vec<u8>,
}

/// Parsed authenticator data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatorData {
    /// SHA-256 of the RP ID.
    pub rp_id_hash: Vec<u8>,
    /// Decoded flags.
    pub flags: AuthenticatorDataFlags,
    /// Signature counter.
    pub sign_count: u32,
    /// Present when the `at` flag is set.
    pub attested_credential_data: Option<AttestedCredentialData>,
    /// Raw CBOR extensions when the `ed` flag is set.
    pub extensions: Option<Vec<u8>>,
}

struct SliceReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl std::io::Read for SliceReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = &self.data[self.position..];
        let count = remaining.len().min(buf.len());
        buf[..count].copy_from_slice(&remaining[..count]);
        self.position += count;
        Ok(count)
    }
}

/// Decode one CBOR item from the front of `bytes` and return it with its encoded length.
pub fn decode_cbor_prefix(bytes: &[u8]) -> Result<(CborValue, usize), WebAuthnError> {
    let mut reader = SliceReader {
        data: bytes,
        position: 0,
    };
    let value: CborValue =
        ciborium::from_reader(&mut reader).map_err(|error| err(format!("Invalid CBOR: {error}")))?;
    Ok((value, reader.position))
}

/// Parse `authenticatorData` bytes.
pub fn parse_authenticator_data(val: &[u8]) -> Result<AuthenticatorData, WebAuthnError> {
    if val.len() < 37 {
        return Err(err(format!(
            "Authenticator data was {} bytes, expected at least 37 bytes",
            val.len()
        )));
    }
    let rp_id_hash = val[..32].to_vec();
    let flags_byte = val[32];
    let sign_count = u32::from_be_bytes([val[33], val[34], val[35], val[36]]);
    let flags = AuthenticatorDataFlags {
        up: flags_byte & (1 << 0) != 0,
        uv: flags_byte & (1 << 2) != 0,
        be: flags_byte & (1 << 3) != 0,
        bs: flags_byte & (1 << 4) != 0,
        at: flags_byte & (1 << 6) != 0,
        ed: flags_byte & (1 << 7) != 0,
    };
    let mut pointer = 37;
    let mut attested_credential_data = None;
    if flags.at {
        if val.len() < pointer + 18 {
            return Err(err("Attested credential data truncated"));
        }
        let aaguid = val[pointer..pointer + 16].to_vec();
        pointer += 16;
        let credential_id_len = u16::from_be_bytes([val[pointer], val[pointer + 1]]) as usize;
        pointer += 2;
        if val.len() < pointer + credential_id_len {
            return Err(err("Credential id truncated"));
        }
        let credential_id = val[pointer..pointer + credential_id_len].to_vec();
        pointer += credential_id_len;
        let (_, key_len) = decode_cbor_prefix(&val[pointer..])?;
        let credential_public_key = val[pointer..pointer + key_len].to_vec();
        pointer += key_len;
        attested_credential_data = Some(AttestedCredentialData {
            aaguid,
            credential_id,
            credential_public_key,
        });
    }
    let mut extensions = None;
    if flags.ed {
        let (_, ext_len) = decode_cbor_prefix(&val[pointer..])?;
        extensions = Some(val[pointer..pointer + ext_len].to_vec());
        pointer += ext_len;
    }
    if val.len() > pointer {
        return Err(err("Leftover bytes detected while parsing authenticator data"));
    }
    Ok(AuthenticatorData {
        rp_id_hash,
        flags,
        sign_count,
        attested_credential_data,
        extensions,
    })
}

/// Parsed `clientDataJSON`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedClientData {
    /// `webauthn.create` or `webauthn.get`.
    pub ceremony_type: String,
    /// Decoded challenge bytes.
    pub challenge: Vec<u8>,
    /// Origin string.
    pub origin: String,
}

/// Parse `clientDataJSON` bytes.
pub fn parse_client_data_json(val: &[u8]) -> Result<CollectedClientData, WebAuthnError> {
    let parsed: Value =
        serde_json::from_slice(val).map_err(|_| err("Unable to decode clientDataJSON bytes as JSON"))?;
    let Value::Object(map) = parsed else {
        return Err(err("clientDataJSON was not a dict"));
    };
    for field in ["type", "challenge", "origin"] {
        if !map.contains_key(field) {
            return Err(err(format!(
                "clientDataJSON missing required property \"{field}\""
            )));
        }
    }
    let challenge = b64url_decode(&py_str(&map["challenge"]))
        .map_err(|_| err("clientDataJSON challenge is not base64url"))?;
    Ok(CollectedClientData {
        ceremony_type: py_str(&map["type"]),
        challenge,
        origin: py_str(&map["origin"]),
    })
}

/// A decoded COSE public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CosePublicKey {
    /// EC2 key (`kty` 2).
    Ec2 {
        /// COSE algorithm.
        alg: i64,
        /// COSE curve.
        crv: i64,
        /// X coordinate.
        x: Vec<u8>,
        /// Y coordinate.
        y: Vec<u8>,
    },
    /// OKP key (`kty` 1).
    Okp {
        /// COSE algorithm.
        alg: i64,
        /// COSE curve.
        crv: i64,
        /// Public key bytes.
        x: Vec<u8>,
    },
    /// RSA key (`kty` 3).
    Rsa {
        /// COSE algorithm.
        alg: i64,
        /// Modulus.
        n: Vec<u8>,
        /// Exponent.
        e: Vec<u8>,
    },
}

impl CosePublicKey {
    /// The COSE algorithm identifier.
    pub fn alg(&self) -> i64 {
        match self {
            Self::Ec2 { alg, .. } | Self::Okp { alg, .. } | Self::Rsa { alg, .. } => *alg,
        }
    }
}

fn cbor_int(value: &CborValue) -> Option<i64> {
    match value {
        CborValue::Integer(int) => i64::try_from(*int).ok(),
        _ => None,
    }
}

fn cose_entry(map: &[(CborValue, CborValue)], key: i64) -> Option<&CborValue> {
    map.iter().find(|(k, _)| cbor_int(k) == Some(key)).map(|(_, v)| v)
}

fn cose_bytes(map: &[(CborValue, CborValue)], key: i64, name: &str) -> Result<Vec<u8>, WebAuthnError> {
    match cose_entry(map, key) {
        Some(CborValue::Bytes(bytes)) if !bytes.is_empty() => Ok(bytes.clone()),
        _ => Err(err(format!("credential public key missing {name}"))),
    }
}

/// Decode a CBOR encoded COSE public key (EC2, OKP or RSA).
pub fn decode_credential_public_key(key: &[u8]) -> Result<CosePublicKey, WebAuthnError> {
    if key.first() == Some(&0x04) && key.len() == 65 {
        return Ok(CosePublicKey::Ec2 {
            alg: COSE_ALG_ES256,
            crv: 1,
            x: key[1..33].to_vec(),
            y: key[33..65].to_vec(),
        });
    }
    let (value, _) = decode_cbor_prefix(key)?;
    let CborValue::Map(map) = value else {
        return Err(err("Credential public key is not a CBOR map"));
    };
    let kty = cose_entry(&map, 1).and_then(cbor_int).filter(|v| *v != 0);
    let alg = cose_entry(&map, 3).and_then(cbor_int).filter(|v| *v != 0);
    let Some(kty) = kty else {
        return Err(err("Credential public key missing kty"));
    };
    let Some(alg) = alg else {
        return Err(err("Credential public key missing alg"));
    };
    match kty {
        1 => {
            let crv = cose_entry(&map, -1)
                .and_then(cbor_int)
                .filter(|v| *v != 0)
                .ok_or_else(|| err("OKP credential public key missing crv"))?;
            let x = cose_bytes(&map, -2, "x").map_err(|_| err("OKP credential public key missing x"))?;
            Ok(CosePublicKey::Okp { alg, crv, x })
        }
        2 => {
            let crv = cose_entry(&map, -1)
                .and_then(cbor_int)
                .filter(|v| *v != 0)
                .ok_or_else(|| err("EC2 credential public key missing crv"))?;
            let x = cose_bytes(&map, -2, "x").map_err(|_| err("EC2 credential public key missing x"))?;
            let y = cose_bytes(&map, -3, "y").map_err(|_| err("EC2 credential public key missing y"))?;
            Ok(CosePublicKey::Ec2 { alg, crv, x, y })
        }
        3 => {
            let n = cose_bytes(&map, -1, "n").map_err(|_| err("RSA credential public key missing n"))?;
            let e = cose_bytes(&map, -2, "e").map_err(|_| err("RSA credential public key missing e"))?;
            Ok(CosePublicKey::Rsa { alg, n, e })
        }
        other => Err(err(format!("Unsupported credential public key type \"{other}\""))),
    }
}

/// Verify `signature` over `data` with a COSE public key and its algorithm.
pub fn verify_signature(
    public_key: &CosePublicKey,
    signature: &[u8],
    data: &[u8],
) -> Result<(), WebAuthnError> {
    let invalid = || err("Could not verify signature");
    match public_key {
        CosePublicKey::Ec2 { alg, crv, x, y } => {
            if *alg != COSE_ALG_ES256 || *crv != 1 {
                return Err(err(format!("Unsupported EC2 signature alg {alg} on curve {crv}")));
            }
            use p256::ecdsa::signature::Verifier;
            if x.len() != 32 || y.len() != 32 {
                return Err(err("EC2 coordinates must be 32 bytes"));
            }
            let point =
                p256::EncodedPoint::from_affine_coordinates(x.as_slice().into(), y.as_slice().into(), false);
            let verifying_key = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| err("EC2 public key is not on the P-256 curve"))?;
            let signature = p256::ecdsa::Signature::from_der(signature).map_err(|_| invalid())?;
            verifying_key.verify(data, &signature).map_err(|_| invalid())
        }
        CosePublicKey::Okp { alg, crv, x } => {
            if *alg != COSE_ALG_EDDSA || *crv != 6 {
                return Err(err(format!("Unsupported OKP signature alg {alg} on curve {crv}")));
            }
            use ed25519_dalek::Verifier;
            let bytes: [u8; 32] = x
                .as_slice()
                .try_into()
                .map_err(|_| err("Ed25519 public key must be 32 bytes"))?;
            let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
                .map_err(|_| err("Ed25519 public key invalid"))?;
            let signature = ed25519_dalek::Signature::from_slice(signature).map_err(|_| invalid())?;
            verifying_key.verify(data, &signature).map_err(|_| invalid())
        }
        CosePublicKey::Rsa { alg, n, e } => {
            if *alg != COSE_ALG_RS256 {
                return Err(err(format!("Unsupported RSA signature alg {alg}")));
            }
            use rsa::signature::Verifier;
            let public_key =
                rsa::RsaPublicKey::new(rsa::BigUint::from_bytes_be(n), rsa::BigUint::from_bytes_be(e))
                    .map_err(|_| err("RSA public key invalid"))?;
            let verifying_key = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(public_key);
            let signature = rsa::pkcs1v15::Signature::try_from(signature).map_err(|_| invalid())?;
            verifying_key.verify(data, &signature).map_err(|_| invalid())
        }
    }
}

fn aaguid_to_string(val: &[u8]) -> Result<String, WebAuthnError> {
    if val.len() != 16 {
        return Err(err(format!("AAGUID was {} bytes, expected 16 bytes", val.len())));
    }
    let hex = hex::encode(val);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

fn parse_backup_flags(flags: AuthenticatorDataFlags) -> Result<(String, bool), WebAuthnError> {
    let device_type = if flags.be { "multi_device" } else { "single_device" };
    if device_type == "single_device" && flags.bs {
        return Err(err(
            "Single-device credential indicated that it was backed up, which should be impossible.",
        ));
    }
    Ok((device_type.to_string(), flags.bs))
}

fn response_bytes(response: &JsonDict, field: &str, label: &str) -> Result<Vec<u8>, WebAuthnError> {
    let Some(Value::String(text)) = response.get(field) else {
        return Err(err(format!("Credential response missing required {label}")));
    };
    b64url_decode(text).map_err(|_| err(format!("Credential response {label} is not base64url")))
}

fn check_credential_shape(credential: &JsonDict) -> Result<JsonDict, WebAuthnError> {
    let Some(Value::String(id)) = credential.get("id") else {
        return Err(err("Credential missing required id"));
    };
    let Some(Value::String(raw_id)) = credential.get("rawId") else {
        return Err(err("Credential missing required rawId"));
    };
    let Some(Value::Object(response)) = credential.get("response") else {
        return Err(err("Credential missing required response"));
    };
    if credential.get("type").and_then(Value::as_str) != Some("public-key") {
        return Err(err("Credential had unexpected type"));
    }
    let raw = b64url_decode(raw_id).map_err(|_| err("Credential rawId is not base64url"))?;
    if &b64url_encode(&raw) != id {
        return Err(err("id and raw_id were not equivalent"));
    }
    Ok(response.clone())
}

/// Information about a verified registration ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRegistration {
    /// Credential id bytes.
    pub credential_id: Vec<u8>,
    /// CBOR COSE public key bytes.
    pub credential_public_key: Vec<u8>,
    /// Initial sign count.
    pub sign_count: u32,
    /// AAGUID as a GUID string.
    pub aaguid: String,
    /// Attestation format.
    pub fmt: String,
    /// Whether the user was verified.
    pub user_verified: bool,
    /// `single_device` or `multi_device`.
    pub credential_device_type: String,
    /// Whether the credential is backed up.
    pub credential_backed_up: bool,
}

/// Verify a `navigator.credentials.create()` response.
pub fn verify_registration_response(
    credential: &JsonDict,
    expected_challenge: &[u8],
    expected_rp_id: &str,
    expected_origin: &str,
    require_user_presence: bool,
    require_user_verification: bool,
) -> Result<VerifiedRegistration, WebAuthnError> {
    let response = check_credential_shape(credential)?;
    let client_data_bytes = response_bytes(&response, "clientDataJSON", "clientDataJSON")?;
    let attestation_object_bytes = response_bytes(&response, "attestationObject", "attestationObject")?;
    let client_data = parse_client_data_json(&client_data_bytes)?;
    if client_data.ceremony_type != "webauthn.create" {
        return Err(err(format!(
            "Unexpected client data type \"{}\", expected \"webauthn.create\"",
            client_data.ceremony_type
        )));
    }
    if expected_challenge != client_data.challenge.as_slice() {
        return Err(err("Client data challenge was not expected challenge"));
    }
    if expected_origin != client_data.origin {
        return Err(err(format!(
            "Unexpected client data origin \"{}\", expected \"{expected_origin}\"",
            client_data.origin
        )));
    }

    let (attestation, _) =
        decode_cbor_prefix(&attestation_object_bytes).map_err(|_| err("attestationObject was malformed"))?;
    let CborValue::Map(attestation_map) = attestation else {
        return Err(err("attestationObject was malformed"));
    };
    let text_entry = |name: &str| {
        attestation_map
            .iter()
            .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
            .map(|(_, value)| value)
    };
    let fmt = match text_entry("fmt") {
        Some(CborValue::Text(fmt)) => fmt.clone(),
        _ => return Err(err("attestationObject was malformed")),
    };
    let auth_data_bytes = match text_entry("authData") {
        Some(CborValue::Bytes(bytes)) => bytes.clone(),
        _ => return Err(err("attestationObject was malformed")),
    };
    let att_stmt = match text_entry("attStmt") {
        Some(CborValue::Map(map)) => map.clone(),
        None => Vec::new(),
        _ => return Err(err("attestationObject was malformed")),
    };
    let auth_data = parse_authenticator_data(&auth_data_bytes)?;

    let expected_rp_id_hash = Sha256::digest(expected_rp_id.as_bytes());
    if auth_data.rp_id_hash != expected_rp_id_hash.to_vec() {
        return Err(err("Unexpected RP ID hash"));
    }
    if require_user_presence && !auth_data.flags.up {
        return Err(err(
            "User presence was required, but was not present during attestation",
        ));
    }
    if require_user_verification && !auth_data.flags.uv {
        return Err(err(
            "User verification is required but user was not verified during attestation",
        ));
    }
    let Some(attested) = auth_data.attested_credential_data.clone() else {
        return Err(err("Authenticator did not provide attested credential data"));
    };
    if attested.credential_id.is_empty() {
        return Err(err("Authenticator did not provide a credential ID"));
    }
    if attested.credential_public_key.is_empty() {
        return Err(err("Authenticator did not provide a credential public key"));
    }
    if attested.aaguid.is_empty() {
        return Err(err("Authenticator did not provide an AAGUID"));
    }
    let decoded_public_key = decode_credential_public_key(&attested.credential_public_key)?;
    if !SUPPORTED_COSE_ALGORITHMS.contains(&decoded_public_key.alg()) {
        return Err(err(format!(
            "Unsupported credential public key alg \"{}\", expected one of: {:?}",
            decoded_public_key.alg(),
            SUPPORTED_COSE_ALGORITHMS
        )));
    }

    let att_entry = |name: &str| {
        att_stmt
            .iter()
            .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
            .map(|(_, value)| value)
    };
    match fmt.as_str() {
        "none" => {
            let known = ["sig", "x5c", "response", "alg", "ver", "certInfo", "pubArea"];
            if known.iter().any(|name| att_entry(name).is_some()) {
                return Err(err("None attestation had unexpected attestation statement"));
            }
        }
        "packed" => {
            let sig = match att_entry("sig") {
                Some(CborValue::Bytes(bytes)) if !bytes.is_empty() => bytes.clone(),
                _ => return Err(err("Attestation statement was missing signature (Packed)")),
            };
            let alg = match att_entry("alg").and_then(cbor_int) {
                Some(alg) if alg != 0 => alg,
                _ => return Err(err("Attestation statement was missing algorithm (Packed)")),
            };
            if att_entry("x5c").is_some() {
                return Err(err(
                    "Packed attestation with a certificate chain is not supported; use attestation 'none'",
                ));
            }
            if decoded_public_key.alg() != alg {
                return Err(err(format!(
                    "Credential public key alg {} did not equal attestation statement alg {alg}",
                    decoded_public_key.alg()
                )));
            }
            let mut verification_data = auth_data_bytes.clone();
            verification_data.extend_from_slice(&Sha256::digest(&client_data_bytes));
            verify_signature(&decoded_public_key, &sig, &verification_data)
                .map_err(|_| err("Could not verify attestation statement signature (Packed|Self)"))?;
        }
        other => return Err(err(format!("Unsupported attestation type \"{other}\""))),
    }

    let (credential_device_type, credential_backed_up) = parse_backup_flags(auth_data.flags)?;
    Ok(VerifiedRegistration {
        credential_id: attested.credential_id,
        credential_public_key: attested.credential_public_key,
        sign_count: auth_data.sign_count,
        aaguid: aaguid_to_string(&attested.aaguid)?,
        fmt,
        user_verified: auth_data.flags.uv,
        credential_device_type,
        credential_backed_up,
    })
}

/// Information about a verified authentication ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAuthentication {
    /// Credential id bytes.
    pub credential_id: Vec<u8>,
    /// The new sign count to persist.
    pub new_sign_count: u32,
    /// `single_device` or `multi_device`.
    pub credential_device_type: String,
    /// Whether the credential is backed up.
    pub credential_backed_up: bool,
    /// Whether the user was verified.
    pub user_verified: bool,
}

/// Verify a `navigator.credentials.get()` response.
pub fn verify_authentication_response(
    credential: &JsonDict,
    expected_challenge: &[u8],
    expected_rp_id: &str,
    expected_origin: &str,
    credential_public_key: &[u8],
    credential_current_sign_count: u32,
    require_user_verification: bool,
) -> Result<VerifiedAuthentication, WebAuthnError> {
    let response = check_credential_shape(credential)?;
    let raw_id = b64url_decode(
        credential
            .get("rawId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .map_err(|_| err("Credential rawId is not base64url"))?;
    let client_data_bytes = response_bytes(&response, "clientDataJSON", "clientDataJSON")?;
    let authenticator_data_bytes = response_bytes(&response, "authenticatorData", "authenticatorData")?;
    let signature_bytes = response_bytes(&response, "signature", "signature")?;
    let client_data = parse_client_data_json(&client_data_bytes)?;
    if client_data.ceremony_type != "webauthn.get" {
        return Err(err(format!(
            "Unexpected client data type \"{}\", expected \"webauthn.get\"",
            client_data.ceremony_type
        )));
    }
    if expected_challenge != client_data.challenge.as_slice() {
        return Err(err("Client data challenge was not expected challenge"));
    }
    if expected_origin != client_data.origin {
        return Err(err(format!(
            "Unexpected client data origin \"{}\", expected \"{expected_origin}\"",
            client_data.origin
        )));
    }
    let auth_data = parse_authenticator_data(&authenticator_data_bytes)
        .map_err(|error| err(format!("authenticatorData was malformed: {error}")))?;
    let expected_rp_id_hash = Sha256::digest(expected_rp_id.as_bytes());
    if auth_data.rp_id_hash != expected_rp_id_hash.to_vec() {
        return Err(err("Unexpected RP ID hash"));
    }
    if !auth_data.flags.up {
        return Err(err("User was not present during authentication"));
    }
    if require_user_verification && !auth_data.flags.uv {
        return Err(err(
            "User verification is required but user was not verified during authentication",
        ));
    }
    if (auth_data.sign_count > 0 || credential_current_sign_count > 0)
        && auth_data.sign_count <= credential_current_sign_count
    {
        return Err(err(format!(
            "Response sign count of {} was not greater than current count of {credential_current_sign_count}",
            auth_data.sign_count
        )));
    }
    let mut signature_base = authenticator_data_bytes.clone();
    signature_base.extend_from_slice(&Sha256::digest(&client_data_bytes));
    let decoded_public_key = decode_credential_public_key(credential_public_key)?;
    verify_signature(&decoded_public_key, &signature_bytes, &signature_base)
        .map_err(|_| err("Could not verify authentication signature"))?;
    let (credential_device_type, credential_backed_up) = parse_backup_flags(auth_data.flags)?;
    Ok(VerifiedAuthentication {
        credential_id: raw_id,
        new_sign_count: auth_data.sign_count,
        credential_device_type,
        credential_backed_up,
        user_verified: auth_data.flags.uv,
    })
}

/// A4P Server-side WebAuthn enrollment and signature method.
pub struct WebAuthnSignatureMethod {
    credential_store: Arc<dyn A4PCredentialStore>,
    rp_id: String,
    rp_name: String,
    expected_origin: String,
    registration_challenges: Mutex<HashMap<String, (String, Vec<u8>)>>,
}

impl WebAuthnSignatureMethod {
    /// Create the method with `rp_id="localhost"`, `rp_name="A4P"` and
    /// `expected_origin="http://localhost:8970"`.
    pub fn new(credential_store: Arc<dyn A4PCredentialStore>) -> Self {
        Self {
            credential_store,
            rp_id: "localhost".into(),
            rp_name: "A4P".into(),
            expected_origin: "http://localhost:8970".into(),
            registration_challenges: Mutex::new(HashMap::new()),
        }
    }

    /// Set the relying party id.
    pub fn with_rp_id(mut self, rp_id: impl Into<String>) -> Self {
        self.rp_id = rp_id.into();
        self
    }

    /// Set the relying party display name.
    pub fn with_rp_name(mut self, rp_name: impl Into<String>) -> Self {
        self.rp_name = rp_name.into();
        self
    }

    /// Set the expected browser origin.
    pub fn with_expected_origin(mut self, expected_origin: impl Into<String>) -> Self {
        self.expected_origin = expected_origin.into();
        self
    }

    /// The relying party id.
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    /// The relying party name.
    pub fn rp_name(&self) -> &str {
        &self.rp_name
    }

    /// The expected origin.
    pub fn expected_origin(&self) -> &str {
        &self.expected_origin
    }

    /// The credential store used by this method.
    pub fn credential_store(&self) -> &Arc<dyn A4PCredentialStore> {
        &self.credential_store
    }

    /// Number of one-shot registration challenges waiting for `verify_registration`.
    pub fn pending_registration_count(&self) -> usize {
        self.registration_challenges.lock().len()
    }

    /// Generate creation options and remember a one-shot challenge keyed by `registrationRequestId`.
    pub fn registration_options(
        &self,
        user_id: &str,
        user_name: Option<&str>,
        user_display_name: Option<&str>,
    ) -> Result<JsonDict, A4PError> {
        let challenge = random_bytes(32);
        let registration_request_id = format!("webauthn_registration_{}", token_urlsafe(18));
        self.registration_challenges.lock().insert(
            registration_request_id.clone(),
            (user_id.to_string(), challenge.clone()),
        );
        let existing: Vec<Value> = self
            .credential_store
            .list_for_user(user_id)?
            .into_iter()
            .filter(|record| record.signature_method == WEBAUTHN_SIGNATURE_METHOD)
            .map(|record| credential_descriptor(&record.credential_id))
            .collect();
        let user_name = user_name.filter(|v| !v.is_empty()).unwrap_or(user_id);
        let user_display_name = user_display_name.filter(|v| !v.is_empty()).unwrap_or(user_name);

        let mut rp = JsonDict::new();
        rp.insert("name".into(), Value::String(self.rp_name.clone()));
        rp.insert("id".into(), Value::String(self.rp_id.clone()));
        let mut user = JsonDict::new();
        user.insert("id".into(), Value::String(b64url_encode(user_id.as_bytes())));
        user.insert("name".into(), Value::String(user_name.into()));
        user.insert("displayName".into(), Value::String(user_display_name.into()));
        let pub_key_cred_params: Vec<Value> = SUPPORTED_COSE_ALGORITHMS
            .iter()
            .map(|alg| {
                let mut param = JsonDict::new();
                param.insert("type".into(), Value::String("public-key".into()));
                param.insert("alg".into(), Value::from(*alg));
                Value::Object(param)
            })
            .collect();
        let mut authenticator_selection = JsonDict::new();
        authenticator_selection.insert("residentKey".into(), Value::String("required".into()));
        authenticator_selection.insert("requireResidentKey".into(), Value::Bool(true));
        authenticator_selection.insert("userVerification".into(), Value::String("required".into()));

        let mut options = JsonDict::new();
        options.insert("rp".into(), Value::Object(rp));
        options.insert("user".into(), Value::Object(user));
        options.insert("challenge".into(), Value::String(b64url_encode(&challenge)));
        options.insert("pubKeyCredParams".into(), Value::Array(pub_key_cred_params));
        options.insert("timeout".into(), Value::from(DEFAULT_TIMEOUT_MS));
        options.insert("excludeCredentials".into(), Value::Array(existing));
        options.insert(
            "authenticatorSelection".into(),
            Value::Object(authenticator_selection),
        );
        options.insert("attestation".into(), Value::String("none".into()));

        let mut response = JsonDict::new();
        response.insert(
            "registrationRequestId".into(),
            Value::String(registration_request_id),
        );
        response.insert("userId".into(), Value::String(user_id.into()));
        response.insert("options".into(), Value::Object(options));
        Ok(response)
    }

    /// Verify a registration credential and store it.
    ///
    /// The request carries `userId`, `registrationRequestId` and `credential`.
    pub fn verify_registration(&self, payload: &JsonDict) -> Result<UserCredentialRecord, A4PError> {
        let user_id = str_trimmed(payload, "userId");
        if user_id.is_empty() {
            return Err(A4PError::value("userId missing"));
        }
        let registration_request_id = str_trimmed(payload, "registrationRequestId");
        if registration_request_id.is_empty() {
            return Err(A4PError::value("registrationRequestId missing"));
        }
        let pending = self
            .registration_challenges
            .lock()
            .remove(&registration_request_id);
        let Some((pending_user_id, challenge)) = pending else {
            return Err(A4PError::value(format!(
                "No WebAuthn registration challenge: {registration_request_id}"
            )));
        };
        if pending_user_id != user_id {
            return Err(A4PError::value(format!(
                "WebAuthn registration user mismatch: expected '{pending_user_id}', got '{user_id}'"
            )));
        }
        let Some(credential) = get_object(payload, "credential") else {
            return Err(A4PError::value("credential missing"));
        };
        let verified = verify_registration_response(
            credential,
            &challenge,
            &self.rp_id,
            &self.expected_origin,
            true,
            true,
        )
        .map_err(|error| A4PError::value(format!("WebAuthn registration invalid: {error}")))?;
        let credential_id = b64url_encode(&verified.credential_id);
        let transports: Vec<Value> = get_object(credential, "response")
            .and_then(|response| response.get("transports"))
            .and_then(Value::as_array)
            .map(|items| items.iter().map(|item| Value::String(py_str(item))).collect())
            .unwrap_or_default();

        let mut public_key = JsonDict::new();
        public_key.insert("format".into(), Value::String("cose".into()));
        public_key.insert(
            "value".into(),
            Value::String(b64url_encode(&verified.credential_public_key)),
        );
        let mut details = JsonDict::new();
        details.insert("signCount".into(), Value::from(verified.sign_count));
        details.insert("rpId".into(), Value::String(self.rp_id.clone()));
        details.insert("origin".into(), Value::String(self.expected_origin.clone()));
        details.insert("transports".into(), Value::Array(transports));
        details.insert("aaguid".into(), Value::String(verified.aaguid));
        details.insert("fmt".into(), Value::String(verified.fmt));
        details.insert(
            "credentialDeviceType".into(),
            Value::String(verified.credential_device_type),
        );
        details.insert(
            "credentialBackedUp".into(),
            Value::Bool(verified.credential_backed_up),
        );
        let record = UserCredentialRecord {
            user_id,
            credential_id,
            signature_method: WEBAUTHN_SIGNATURE_METHOD.into(),
            public_key,
            details,
            metadata: JsonDict::new(),
            created_at: utc_now_iso(),
        };
        self.credential_store.save(record.clone())?;
        Ok(record)
    }
}

fn credential_descriptor(credential_id: &str) -> Value {
    let mut descriptor = JsonDict::new();
    descriptor.insert("id".into(), Value::String(credential_id.into()));
    descriptor.insert("type".into(), Value::String("public-key".into()));
    Value::Object(descriptor)
}

impl A4PUserSignatureMethod for WebAuthnSignatureMethod {
    fn signature_method(&self) -> &str {
        WEBAUTHN_SIGNATURE_METHOD
    }

    fn method_policy(&self) -> JsonDict {
        let mut policy = JsonDict::new();
        policy.insert("userVerification".into(), Value::String("required".into()));
        policy
    }

    fn signing_options(&self, user_id: &str, mandate: &JsonDict) -> Result<JsonDict, A4PError> {
        let records: Vec<UserCredentialRecord> = self
            .credential_store
            .list_for_user(user_id)?
            .into_iter()
            .filter(|record| record.signature_method == WEBAUTHN_SIGNATURE_METHOD)
            .collect();
        if records.is_empty() {
            return Err(
                A4PProtocolError::user_credential_not_registered(user_id, WEBAUTHN_SIGNATURE_METHOD).into(),
            );
        }
        let challenge = derive_user_authorization_challenge(mandate)?;
        let mut method_options = JsonDict::new();
        method_options.insert("challenge".into(), Value::String(b64url_encode(&challenge)));
        method_options.insert("timeout".into(), Value::from(DEFAULT_TIMEOUT_MS));
        method_options.insert("rpId".into(), Value::String(self.rp_id.clone()));
        method_options.insert(
            "allowCredentials".into(),
            Value::Array(
                records
                    .iter()
                    .map(|record| credential_descriptor(&record.credential_id))
                    .collect(),
            ),
        );
        method_options.insert("userVerification".into(), Value::String("required".into()));
        let mut options = JsonDict::new();
        options.insert(
            "signatureMethod".into(),
            Value::String(WEBAUTHN_SIGNATURE_METHOD.into()),
        );
        options.insert("methodOptions".into(), Value::Object(method_options));
        Ok(options)
    }

    fn verify(&self, context: &UserSignatureContext, signature: &JsonDict) -> Result<(), String> {
        let credential_id = str_trimmed(signature, "credentialId");
        if credential_id.is_empty() {
            return Err("WebAuthn credentialId missing".into());
        }
        let record = self
            .credential_store
            .get(&credential_id)
            .map_err(|error| format!("WebAuthn credential store error: {error}"))?;
        let Some(record) = record else {
            return Err(format!("WebAuthn credential not registered: {credential_id}"));
        };
        if record.signature_method != WEBAUTHN_SIGNATURE_METHOD {
            return Err("WebAuthn credential signature method mismatch".into());
        }
        if let Some(expected_user_id) = &context.expected_user_id {
            if &record.user_id != expected_user_id {
                return Err(format!(
                    "WebAuthn credential user mismatch: expected '{expected_user_id}', got '{}'",
                    record.user_id
                ));
            }
        }
        let record_rp_id = str_or_empty(&record.details, "rpId");
        let record_origin = str_or_empty(&record.details, "origin");
        if !record_rp_id.is_empty() && record_rp_id != self.rp_id {
            return Err(format!(
                "WebAuthn credential RP ID mismatch: expected '{}', got '{record_rp_id}'",
                self.rp_id
            ));
        }
        if !record_origin.is_empty() && record_origin != self.expected_origin {
            return Err(format!(
                "WebAuthn credential origin mismatch: expected '{}', got '{record_origin}'",
                self.expected_origin
            ));
        }
        let assertion = get_object(signature, "proof").and_then(|proof| get_object(proof, "assertion"));
        let Some(assertion) = assertion else {
            return Err("WebAuthn assertion missing".into());
        };
        let expected_challenge = derive_user_authorization_challenge(&context.server_signed_mandate)
            .map_err(|error| format!("WebAuthn signature invalid: {error}"))?;
        let public_key = b64url_decode(&str_or_empty(&record.public_key, "value"))
            .map_err(|_| "WebAuthn signature invalid: registered public key is not base64url".to_string())?;
        let current_sign_count = record
            .details
            .get("signCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32;
        let verified = verify_authentication_response(
            assertion,
            &expected_challenge,
            &self.rp_id,
            &self.expected_origin,
            &public_key,
            current_sign_count,
            true,
        )
        .map_err(|error| format!("WebAuthn signature invalid: {error}"))?;
        if !verified.user_verified {
            return Err("WebAuthn user verification missing".into());
        }
        let mut updated = record.clone();
        updated
            .details
            .insert("signCount".into(), Value::from(verified.new_sign_count));
        self.credential_store
            .save(updated)
            .map_err(|error| format!("WebAuthn credential store error: {error}"))?;
        Ok(())
    }

    fn webauthn_registrar(&self) -> Option<&WebAuthnSignatureMethod> {
        Some(self)
    }
}

/// User Authorizer-side signer that wraps a browser WebAuthn assertion.
#[derive(Debug, Clone, Default)]
pub struct WebAuthnUserSigner;

impl WebAuthnUserSigner {
    /// Create the signer.
    pub fn new() -> Self {
        Self
    }
}

impl A4PUserSigner for WebAuthnUserSigner {
    fn signature_method(&self) -> &str {
        WEBAUTHN_SIGNATURE_METHOD
    }

    fn sign(
        &self,
        _context: &UserSignatureContext,
        signing_input: Option<&UserSigningInput>,
    ) -> Result<UserSignature, A4PError> {
        let assertion = signing_input.and_then(|input| get_object(input, "assertion"));
        let Some(assertion) = assertion else {
            return Err(A4PError::value("WebAuthn assertion missing"));
        };
        let credential_id = str_trimmed(assertion, "id");
        if credential_id.is_empty() {
            return Err(A4PError::value("WebAuthn credentialId missing"));
        }
        let mut proof = JsonDict::new();
        proof.insert("assertion".into(), Value::Object(assertion.clone()));
        let mut signature = JsonDict::new();
        signature.insert(
            "signatureMethod".into(),
            Value::String(WEBAUTHN_SIGNATURE_METHOD.into()),
        );
        signature.insert("credentialId".into(), Value::String(credential_id));
        signature.insert("proof".into(), Value::Object(proof));
        Ok(signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn cbor_prefix_length_is_exact() -> TestResult {
        let mut encoded = Vec::new();
        let value = CborValue::Map(vec![(
            CborValue::Integer(1.into()),
            CborValue::Bytes(vec![1, 2, 3]),
        )]);
        ciborium::into_writer(&value, &mut encoded)?;
        let length = encoded.len();
        encoded.extend_from_slice(&[0xAA, 0xBB]);
        let (decoded, consumed) = decode_cbor_prefix(&encoded)?;
        assert_eq!(consumed, length);
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn authenticator_data_requires_37_bytes() -> TestResult {
        let error = parse_authenticator_data(&[0; 10]).err_or_fail()?;
        assert!(error.0.contains("expected at least 37 bytes"));
        let mut data = vec![0u8; 37];
        data[32] = 0x05;
        let parsed = parse_authenticator_data(&data)?;
        assert!(parsed.flags.up && parsed.flags.uv && !parsed.flags.at);
        data.push(0);
        assert!(parse_authenticator_data(&data).is_err());
        Ok(())
    }

    #[test]
    fn aaguid_formats_as_guid() -> TestResult {
        assert_eq!(
            aaguid_to_string(&[0; 16])?,
            "00000000-0000-0000-0000-000000000000"
        );
        assert!(aaguid_to_string(&[0; 3]).is_err());
        Ok(())
    }
}
