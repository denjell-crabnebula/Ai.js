// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Operation mandate Server-signing configuration.

use ap_support::env::EnvSource;
use ed25519_dalek::SigningKey;
use serde_json::Value;

use crate::errors::A4PError;
use crate::security::{ed25519_private_key_from_env_in, ed25519_public_key_to_base64url};
use crate::types::JsonDict;

/// Algorithm name of operation Server signatures.
pub const OPERATION_SERVER_SIGN_ALGORITHM: &str = "EdDSA";
/// Key id written into operation mandate Server signatures.
pub const OPERATION_MANDATE_SERVER_KEY_ID: &str = "server#operation-mandate-k1";
/// Development seed label of the operation Server key.
pub const OPERATION_SERVER_PRIVATE_KEY_LABEL: &str = "a4p-operation-server-ed25519-dev-v1";
/// Environment variable holding the operation Server key.
pub const OPERATION_SERVER_PRIVATE_KEY_ENV: &str = "OPERATION_SERVER_ED25519_PRIVATE_KEY";

/// Load the operation Server signing key from `OPERATION_SERVER_ED25519_PRIVATE_KEY`.
pub fn operation_server_signing_key() -> Result<SigningKey, A4PError> {
    operation_server_signing_key_in(ap_support::env::current())
}

/// [`operation_server_signing_key`] reading from `env`.
pub fn operation_server_signing_key_in(env: &dyn EnvSource) -> Result<SigningKey, A4PError> {
    ed25519_private_key_from_env_in(
        env,
        OPERATION_SERVER_PRIVATE_KEY_ENV,
        OPERATION_SERVER_PRIVATE_KEY_LABEL,
        "operation mandate server Ed25519 signing key",
    )
}

/// Return the public trust entry `{alg, keyId, publicKey}` for the current operation Server key.
pub fn operation_server_trusted_key() -> Result<JsonDict, A4PError> {
    let key = operation_server_signing_key()?;
    let mut entry = JsonDict::new();
    entry.insert(
        "alg".into(),
        Value::String(OPERATION_SERVER_SIGN_ALGORITHM.into()),
    );
    entry.insert(
        "keyId".into(),
        Value::String(OPERATION_MANDATE_SERVER_KEY_ID.into()),
    );
    entry.insert(
        "publicKey".into(),
        Value::String(ed25519_public_key_to_base64url(&key.verifying_key())),
    );
    Ok(entry)
}
