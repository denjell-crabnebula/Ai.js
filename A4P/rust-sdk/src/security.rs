// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Ed25519 key loading, signing and verification helpers.

use ap_support::env::EnvSource;
use std::collections::HashSet;

use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::errors::A4PError;
use crate::util::{b64url_decode, b64url_encode, token_urlsafe};

const PRODUCTION_ENV_VALUES: [&str; 2] = ["prod", "production"];
const ENVIRONMENT_VARIABLES: [&str; 4] = ["A4P_ENV", "APP_ENV", "ENV", "PYTHON_ENV"];

static WARNED_DEFAULT_KEYS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Return a cryptographically secure, URL-safe authorization challenge.
pub fn random_challenge() -> String {
    token_urlsafe(32)
}

/// True when `A4P_ENV`, `APP_ENV`, `ENV` or `PYTHON_ENV` names a production environment.
pub fn is_production_environment() -> bool {
    is_production_environment_in(ap_support::env::current())
}

/// [`is_production_environment`] reading from `env`.
pub fn is_production_environment_in(env: &dyn EnvSource) -> bool {
    ENVIRONMENT_VARIABLES.iter().any(|name| {
        let value = env.get(name).unwrap_or_default().trim().to_lowercase();
        PRODUCTION_ENV_VALUES.contains(&value.as_str())
    })
}

/// Load an Ed25519 signing key from an environment variable.
///
/// The value may be a PKCS8 PEM key or a 32 byte seed encoded as base64url,
/// `base64url:`, `base64:` or `hex:`. When the variable is unset, development
/// mode derives a deterministic key from `default_seed_label` and logs a
/// CRITICAL warning; production mode refuses.
pub fn ed25519_private_key_from_env(
    env_name: &str,
    default_seed_label: &str,
    purpose: &str,
) -> Result<SigningKey, A4PError> {
    ed25519_private_key_from_env_in(ap_support::env::current(), env_name, default_seed_label, purpose)
}

/// [`ed25519_private_key_from_env`] reading from `env`.
pub fn ed25519_private_key_from_env_in(
    env: &dyn EnvSource,
    env_name: &str,
    default_seed_label: &str,
    purpose: &str,
) -> Result<SigningKey, A4PError> {
    if let Some(raw) = env.get(env_name) {
        let value = raw.trim();
        if !value.is_empty() {
            if value == default_seed_label {
                reject_or_warn_default_key(env, purpose, env_name)?;
            }
            return load_ed25519_private_key(value, env_name);
        }
    }
    if is_production_environment_in(env) {
        return Err(A4PError::runtime(format!(
            "{purpose} requires {env_name} in production mode; \
             refusing to use the built-in development Ed25519 signing key."
        )));
    }
    warn_default_key(purpose, env_name);
    let seed: [u8; 32] = Sha256::digest(default_seed_label.as_bytes()).into();
    Ok(SigningKey::from_bytes(&seed))
}

/// Sign UTF-8 text and return the unpadded base64url signature.
pub fn ed25519_sign_text(text: &str, private_key: &SigningKey) -> String {
    b64url_encode(&private_key.sign(text.as_bytes()).to_bytes())
}

/// Verify a base64url signature over UTF-8 text.
pub fn ed25519_verify_text(text: &str, signature: &str, public_key: &VerifyingKey) -> bool {
    let Ok(raw) = b64url_decode(signature) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&raw) else {
        return false;
    };
    public_key.verify(text.as_bytes(), &signature).is_ok()
}

/// Load a raw 32 byte Ed25519 public key from unpadded canonical base64url.
pub fn ed25519_public_key_from_base64url(value: &str) -> Result<VerifyingKey, A4PError> {
    let is_base64url = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !is_base64url {
        return Err(A4PError::value("Ed25519 public key must be valid base64url"));
    }
    let raw =
        b64url_decode(value).map_err(|_| A4PError::value("Ed25519 public key must be valid base64url"))?;
    if raw.len() != 32 {
        return Err(A4PError::value("Ed25519 public key must decode to 32 bytes"));
    }
    if b64url_encode(&raw) != value {
        return Err(A4PError::value(
            "Ed25519 public key must use unpadded canonical base64url",
        ));
    }
    let bytes: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| A4PError::value("Ed25519 public key must decode to 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| A4PError::value("Ed25519 public key must decode to 32 bytes"))
}

/// Serialize an Ed25519 public key as unpadded base64url.
pub fn ed25519_public_key_to_base64url(public_key: &VerifyingKey) -> String {
    b64url_encode(public_key.as_bytes())
}

/// Generate a fresh Ed25519 signing key.
pub fn generate_ed25519_private_key() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

/// Forget which purposes already logged the default key warning.
///
/// Test helper that mirrors clearing the Python `_WARNED_DEFAULT_KEYS` set.
pub fn reset_default_key_warnings() {
    WARNED_DEFAULT_KEYS.lock().clear();
}

/// True when the default key warning was already logged for `purpose`.
pub fn default_key_warning_logged(purpose: &str) -> bool {
    WARNED_DEFAULT_KEYS.lock().contains(purpose)
}

fn load_ed25519_private_key(value: &str, env_name: &str) -> Result<SigningKey, A4PError> {
    if value.starts_with("-----BEGIN") {
        return SigningKey::from_pkcs8_pem(value)
            .map_err(|_| A4PError::runtime(format!("{env_name} must contain an Ed25519 private key")));
    }
    let seed = decode_seed(value, env_name)?;
    if seed.len() != 32 {
        return Err(A4PError::runtime(format!(
            "{env_name} must decode to a 32-byte Ed25519 private key seed"
        )));
    }
    let bytes: [u8; 32] = seed.as_slice().try_into().map_err(|_| {
        A4PError::runtime(format!(
            "{env_name} must decode to a 32-byte Ed25519 private key seed"
        ))
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn decode_seed(value: &str, env_name: &str) -> Result<Vec<u8>, A4PError> {
    let format_error = || {
        A4PError::runtime(format!(
            "{env_name} must be an Ed25519 PEM key or a 32-byte seed encoded as base64url, base64:, or hex:"
        ))
    };
    if let Some(hex_value) = value.strip_prefix("hex:") {
        return hex::decode(hex_value).map_err(|_| format_error());
    }
    if let Some(b64_value) = value.strip_prefix("base64:") {
        use base64::Engine;
        return base64::engine::general_purpose::STANDARD
            .decode(b64_value)
            .map_err(|_| format_error());
    }
    if let Some(b64url_value) = value.strip_prefix("base64url:") {
        return b64url_decode(b64url_value).map_err(|_| format_error());
    }
    b64url_decode(value).map_err(|_| format_error())
}

fn reject_or_warn_default_key(env: &dyn EnvSource, purpose: &str, env_name: &str) -> Result<(), A4PError> {
    if is_production_environment_in(env) {
        return Err(A4PError::runtime(format!(
            "{purpose} is configured with the built-in development Ed25519 signing key via {env_name}; \
             set {env_name} to a non-default secret."
        )));
    }
    warn_default_key(purpose, env_name);
    Ok(())
}

fn warn_default_key(purpose: &str, env_name: &str) {
    let mut warned = WARNED_DEFAULT_KEYS.lock();
    if !warned.insert(purpose.to_string()) {
        return;
    }
    tracing::error!(
        "CRITICAL HIGH RISK: {purpose} is using the built-in development signing key. \
         Set {env_name} before using A4P outside local development."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn sign_and_verify_round_trip() -> TestResult {
        let key = generate_ed25519_private_key();
        let signature = ed25519_sign_text("hello", &key);
        assert!(ed25519_verify_text("hello", &signature, &key.verifying_key()));
        assert!(!ed25519_verify_text("hello!", &signature, &key.verifying_key()));
        assert!(!ed25519_verify_text("hello", "AAAA", &key.verifying_key()));
        assert!(!ed25519_verify_text(
            "hello",
            "not base64!!",
            &key.verifying_key()
        ));
        Ok(())
    }

    #[test]
    fn public_key_encoding_is_canonical() -> TestResult {
        let key = generate_ed25519_private_key();
        let encoded = ed25519_public_key_to_base64url(&key.verifying_key());
        assert_eq!(encoded.len(), 43);
        let decoded = ed25519_public_key_from_base64url(&encoded)?;
        assert_eq!(decoded, key.verifying_key());
        assert!(
            ed25519_public_key_from_base64url("AA")
                .err_or_fail()?
                .to_string()
                .contains("32 bytes")
        );
        assert!(
            ed25519_public_key_from_base64url(&"!".repeat(43))
                .err_or_fail()?
                .to_string()
                .contains("base64url")
        );
        assert!(
            ed25519_public_key_from_base64url(&format!("{encoded}="))
                .err_or_fail()?
                .to_string()
                .contains("base64url")
        );
        Ok(())
    }

    #[test]
    fn seed_decoding_supports_all_prefixes() -> TestResult {
        let key = generate_ed25519_private_key();
        let seed = key.to_bytes();
        use base64::Engine;
        let variants = [
            format!("hex:{}", hex::encode(seed)),
            format!(
                "base64:{}",
                base64::engine::general_purpose::STANDARD.encode(seed)
            ),
            format!("base64url:{}", b64url_encode(&seed)),
            b64url_encode(&seed),
        ];
        for variant in variants {
            let loaded = load_ed25519_private_key(&variant, "TEST_KEY")?;
            assert_eq!(loaded.to_bytes(), seed, "{variant}");
        }
        assert!(load_ed25519_private_key("hex:zz", "TEST_KEY").is_err());
        assert!(
            load_ed25519_private_key("AAAA", "TEST_KEY")
                .err_or_fail()?
                .to_string()
                .contains("32-byte")
        );
        Ok(())
    }

    #[test]
    fn pem_keys_are_accepted() -> TestResult {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        let key = generate_ed25519_private_key();
        let pem = key.to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)?;
        let loaded = load_ed25519_private_key(pem.as_str(), "TEST_KEY")?;
        assert_eq!(loaded.to_bytes(), key.to_bytes());
        assert!(load_ed25519_private_key("-----BEGIN NOTHING-----", "TEST_KEY").is_err());
        Ok(())
    }
}
