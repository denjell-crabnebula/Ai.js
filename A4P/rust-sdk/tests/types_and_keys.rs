// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_types_and_keys.py`.
//!
//! These tests mutate process environment variables, so they live in their own
//! test binary and serialize through a mutex.

pub mod common;

use ap_support::testing::{ResultExt, TestResult};
use parking_lot::Mutex as StdMutex;
use std::io::Write;
use std::sync::Arc;

use a4p::intent::signing::intent_server_signing_key_in;
use a4p::operation::signing::{
    OPERATION_SERVER_PRIVATE_KEY_LABEL, operation_server_signing_key, operation_server_signing_key_in,
};
use a4p::security::reset_default_key_warnings;
use a4p::to_payload;
use ap_support::env::MapEnv;
use common::obj;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::json;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Serialise the tests that observe the default-key warning state.
fn clean_env() -> parking_lot::MutexGuard<'static, ()> {
    let guard = ENV_LOCK.lock();
    reset_default_key_warnings();
    guard
}

#[derive(Clone, Default)]
struct SharedBuffer(Arc<StdMutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn wire_types_are_plain_json_serializable_dicts() -> TestResult {
    let intent_mandate = obj(json!({
        "type": "a4p/v1/intent-mandate",
        "mandateId": "mdt-1",
        "server": "local://test",
        "subject": {},
        "intent": {},
        "validTime": {},
        "userAuthorization": {},
        "displayText": "test intent",
        "signatures": {},
    }));
    let intent_token = obj(json!({
        "type": "a4p/v1/intent-token",
        "tokenId": "token-1",
        "mandateId": "mdt-1",
        "subject": {},
        "user": {},
        "intent": {},
        "issuedAt": "2026-07-21T00:00:00Z",
        "expireAt": "2026-07-21T01:00:00Z",
        "nonce": "nonce-1",
        "signature": "signature-1",
        "alg": "EdDSA",
        "keyId": "key-1",
    }));
    let operation_mandate = obj(json!({
        "type": "a4p/v1/operation-mandate",
        "operationId": "op-1",
        "server": "local://test",
        "subject": {},
        "operation": {},
        "validTime": {},
        "userAuthorization": {},
        "displayText": "test operation",
        "signatures": {},
    }));
    for wire_object in [intent_mandate, intent_token, operation_mandate] {
        assert_eq!(to_payload(&wire_object), wire_object);
        let text = serde_json::to_string(&wire_object)?;
        let round_trip: a4p::JsonDict = serde_json::from_str(&text)?;
        assert_eq!(round_trip, wire_object);
    }
    Ok(())
}

#[test]
fn default_server_signing_key_logs_high_risk_warning() -> TestResult {
    let _guard = clean_env();
    let buffer = SharedBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        assert!(operation_server_signing_key().is_ok());
    });
    let logged = String::from_utf8(buffer.0.lock().clone())?;
    assert!(logged.contains("HIGH RISK"), "{logged}");
    assert!(logged.contains("operation mandate server Ed25519 signing key"));
    assert!(a4p::security::default_key_warning_logged(
        "operation mandate server Ed25519 signing key"
    ));
    Ok(())
}

#[test]
fn production_requires_explicit_server_signing_key() -> TestResult {
    let _guard = clean_env();
    let env = MapEnv::from([("A4P_ENV", "production")]);
    let error = intent_server_signing_key_in(&env).err_or_fail()?;
    assert!(error.to_string().contains("production mode"), "{error}");
    Ok(())
}

#[test]
fn production_rejects_configured_default_server_signing_key() -> TestResult {
    let _guard = clean_env();
    let env = MapEnv::from([
        ("A4P_ENV", "prod"),
        (
            "OPERATION_SERVER_ED25519_PRIVATE_KEY",
            OPERATION_SERVER_PRIVATE_KEY_LABEL,
        ),
    ]);
    let error = operation_server_signing_key_in(&env).err_or_fail()?;
    assert!(
        error
            .to_string()
            .contains("built-in development Ed25519 signing key"),
        "{error}"
    );
    Ok(())
}

#[test]
fn explicit_keys_are_loaded_from_the_environment() -> TestResult {
    let _guard = clean_env();
    let key = a4p::security::generate_ed25519_private_key();
    let env = MapEnv::new().with(
        "INTENT_SERVER_ED25519_PRIVATE_KEY",
        format!("hex:{}", hex::encode(key.to_bytes())),
    );
    let loaded = intent_server_signing_key_in(&env)?;
    assert_eq!(loaded.to_bytes(), key.to_bytes());
    let env = env.with("INTENT_SERVER_ED25519_PRIVATE_KEY", "hex:00");
    assert!(intent_server_signing_key_in(&env).is_err());
    Ok(())
}
