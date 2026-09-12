// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Intent mandate and token Server-signing configuration.

use ap_support::env::EnvSource;
use ed25519_dalek::SigningKey;
use serde_json::Value;

use crate::errors::A4PError;
use crate::security::{ed25519_private_key_from_env_in, ed25519_public_key_to_base64url};
use crate::types::JsonDict;

/// Algorithm name of intent Server signatures.
pub const INTENT_SERVER_SIGN_ALGORITHM: &str = "EdDSA";
/// Key id written into intent mandate Server signatures.
pub const INTENT_MANDATE_SERVER_KEY_ID: &str = "server#intent-mandate-k1";
/// Development seed label of the intent Server key.
pub const INTENT_SERVER_PRIVATE_KEY_LABEL: &str = "a4p-intent-server-ed25519-dev-v1";
/// Environment variable holding the intent Server key.
pub const INTENT_SERVER_PRIVATE_KEY_ENV: &str = "INTENT_SERVER_ED25519_PRIVATE_KEY";

/// Load the intent Server signing key from `INTENT_SERVER_ED25519_PRIVATE_KEY`.
pub fn intent_server_signing_key() -> Result<SigningKey, A4PError> {
    intent_server_signing_key_in(ap_support::env::current())
}

/// [`intent_server_signing_key`] reading from `env`.
pub fn intent_server_signing_key_in(env: &dyn EnvSource) -> Result<SigningKey, A4PError> {
    ed25519_private_key_from_env_in(
        env,
        INTENT_SERVER_PRIVATE_KEY_ENV,
        INTENT_SERVER_PRIVATE_KEY_LABEL,
        "intent mandate server Ed25519 signing key",
    )
}

/// Return the public trust entry `{alg, keyId, publicKey}` for the current intent Server key.
pub fn intent_server_trusted_key() -> Result<JsonDict, A4PError> {
    let key = intent_server_signing_key()?;
    let mut entry = JsonDict::new();
    entry.insert("alg".into(), Value::String(INTENT_SERVER_SIGN_ALGORITHM.into()));
    entry.insert("keyId".into(), Value::String(INTENT_MANDATE_SERVER_KEY_ID.into()));
    entry.insert(
        "publicKey".into(),
        Value::String(ed25519_public_key_to_base64url(&key.verifying_key())),
    );
    Ok(entry)
}
