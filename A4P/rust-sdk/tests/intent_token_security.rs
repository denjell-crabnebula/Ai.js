// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_intent_token_security.py`.

pub mod common;

use a4p::JsonDict;
use a4p::canonical::python_float_repr;
use a4p::intent::mandate::{CreateIntentMandate, create_intent_mandate};
use a4p::intent::token::{
    IssueIntentToken, VerifyIntentToken, issue_intent_token, verify_intent_token,
    verify_intent_token_after_signature,
};
use ap_support::testing::{ResultExt, TestResult};
use common::{explicit_server, obj};
use serde_json::{Value, json};

fn valid_token_with_validity(validity_seconds: i64) -> TestResult<JsonDict> {
    let actions = json!([{"name": "delete_note", "params": {"note_id": "*"}}]);
    let agent_key =
        obj(json!({"kty": "OKP", "kid": "agent-key-1", "crv": "Ed25519", "x": "test-public-key"}));
    let mandate = create_intent_mandate(CreateIntentMandate {
        validity_seconds,
        agent_public_key: Some(&agent_key),
        user_signature_method: Some("ed25519"),
        ..CreateIntentMandate::new("local://token-test", "agent-1", Some(&actions))
    })?;
    Ok(issue_intent_token(
        &mandate,
        IssueIntentToken {
            verified_mandate: true,
            ..IssueIntentToken::new("user-1")
        },
    )?)
}

fn valid_token() -> TestResult<JsonDict> {
    valid_token_with_validity(3600)
}

fn verify(token: &JsonDict) -> Result<(), String> {
    let params = obj(json!({"note_id": "note-1"}));
    verify_intent_token(token, VerifyIntentToken::new("delete_note", Some(&params)))
}

#[test]
fn intent_token_rejects_invalid_signature_metadata() -> TestResult {
    let cases = [
        ("type", "a4p/v0/intent-token", "Invalid token type"),
        ("alg", "LegacySymmetric", "Token alg mismatch"),
        ("signature", "", "Token signature missing"),
        ("mandateId", "", "Token mandateId missing"),
        ("keyId", "server#wrong-key", "Token keyId mismatch"),
    ];
    for (field, value, expected_reason) in cases {
        let mut token = valid_token()?;
        token.insert(field.into(), Value::String(value.into()));
        let reason = verify(&token).err_or_fail()?;
        assert!(reason.contains(expected_reason), "{field}: {reason}");
    }
    Ok(())
}

#[test]
fn intent_token_rejects_signed_scope_tampering() -> TestResult {
    let mut token = valid_token()?;
    token["intent"]["actions"][0]["params"]["note_id"] = Value::String("attacker-note".into());
    let params = obj(json!({"note_id": "attacker-note"}));
    let reason =
        verify_intent_token(&token, VerifyIntentToken::new("delete_note", Some(&params))).err_or_fail()?;
    assert_eq!(reason, "Token signature invalid");
    Ok(())
}

#[test]
fn intent_token_rejects_invalid_expiration_after_signature_verification() -> TestResult {
    // The Python test monkeypatches signature verification; here the checks
    // that follow the signature are exercised directly.
    let cases = [
        ("", "Token expireAt missing"),
        ("not-a-timestamp", "Token expireAt format invalid"),
        ("2000-01-01T00:00:00Z", "Token expired"),
    ];
    for (expire_at, expected_reason) in cases {
        let mut token = valid_token()?;
        token.insert("expireAt".into(), Value::String(expire_at.into()));
        let params = obj(json!({"note_id": "note-1"}));
        let reason =
            verify_intent_token_after_signature(&token, VerifyIntentToken::new("delete_note", Some(&params)))
                .err_or_fail()?;
        assert_eq!(reason, expected_reason);
    }
    // A validly signed but expired token is rejected by the full verification too.
    let expired = valid_token_with_validity(-3600)?;
    assert_eq!(verify(&expired).err_or_fail()?, "Token expired");
    Ok(())
}

#[test]
fn intent_token_rejects_identity_binding_mismatches() -> TestResult {
    let params = obj(json!({"note_id": "note-1"}));
    let cases: [(VerifyIntentToken<'_>, &str); 3] = [
        (
            VerifyIntentToken {
                expected_agent_id: Some("agent-2"),
                ..VerifyIntentToken::new("delete_note", Some(&params))
            },
            "Token subject mismatch: expected 'agent-2', got 'agent:agent-1'",
        ),
        (
            VerifyIntentToken {
                expected_agent_key_id: Some("agent-key-2"),
                ..VerifyIntentToken::new("delete_note", Some(&params))
            },
            "Token agent key mismatch: expected 'agent-key-2', got 'agent-key-1'",
        ),
        (
            VerifyIntentToken {
                expected_user_id: Some("user-2"),
                ..VerifyIntentToken::new("delete_note", Some(&params))
            },
            "Token user mismatch: expected 'user-2', got 'user-1'",
        ),
    ];
    for (options, expected_reason) in cases {
        let reason = verify_intent_token(&valid_token()?, options).err_or_fail()?;
        assert_eq!(reason, expected_reason);
    }
    let ok = verify_intent_token(
        &valid_token()?,
        VerifyIntentToken {
            expected_agent_id: Some("agent:agent-1"),
            expected_agent_key_id: Some("agent-key-1"),
            expected_user_id: Some("user-1"),
            ..VerifyIntentToken::new("delete_note", Some(&params))
        },
    );
    assert_eq!(ok, Ok(()));
    Ok(())
}

#[test]
fn intent_token_rejects_non_canonical_json_values() -> TestResult {
    // `serde_json::Value` cannot hold NaN, so the canonical writer's float
    // formatter is checked directly for the Python rejection message.
    let error = python_float_repr(f64::NAN).err_or_fail()?;
    assert!(error.to_string().contains("Out of range float values"), "{error}");
    let error = python_float_repr(f64::NEG_INFINITY).err_or_fail()?;
    assert!(error.to_string().contains("Out of range float values"), "{error}");
    // Finite floats inside a signed scope still verify.
    let actions = json!([{"name": "pay", "params": {"amount": 1.5, "rate": 1e-7}}]);
    let mandate = create_intent_mandate(CreateIntentMandate {
        user_signature_method: Some("ed25519"),
        ..CreateIntentMandate::new("local://token-test", "agent-1", Some(&actions))
    })?;
    let token = issue_intent_token(
        &mandate,
        IssueIntentToken {
            verified_mandate: true,
            ..IssueIntentToken::new("user-1")
        },
    )?;
    let params = obj(json!({"amount": 1.5, "rate": 1e-7}));
    assert_eq!(
        verify_intent_token(&token, VerifyIntentToken::new("pay", Some(&params))),
        Ok(())
    );
    Ok(())
}

#[tokio::test]
async fn intent_token_service_returns_stable_fail_closed_codes() -> TestResult {
    let cases: [(JsonDict, JsonDict, &str); 4] = [
        (
            obj(json!({"type": "a4p/v0/intent-token"})),
            obj(json!({})),
            "TOKEN_INVALID",
        ),
        (
            obj(json!({"signature": ""})),
            obj(json!({})),
            "TOKEN_SIGNATURE_INVALID",
        ),
        (obj(json!({"expired": true})), obj(json!({})), "TOKEN_EXPIRED"),
        (
            obj(json!({})),
            obj(json!({"userId": "user-2"})),
            "TOKEN_SCOPE_MISMATCH",
        ),
    ];
    for (mutation, expected, expected_code) in cases {
        let mut token = if mutation.contains_key("expired") {
            valid_token_with_validity(-3600)
        } else {
            valid_token()
        }?;
        for (key, value) in mutation {
            if key != "expired" {
                token.insert(key, value);
            }
        }
        let mut expected_scope = obj(json!({"action": "delete_note", "params": {"note_id": "note-1"}}));
        for (key, value) in expected {
            expected_scope.insert(key, value);
        }
        let server = explicit_server("local://token-test")?;
        let response = server
            .verify_intent_token(obj(json!({"token": token, "expected": expected_scope})))
            .await?;
        assert!(!response.valid);
        assert_eq!(response.code.as_deref(), Some(expected_code));
    }
    Ok(())
}
