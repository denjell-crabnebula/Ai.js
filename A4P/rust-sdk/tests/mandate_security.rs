// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_mandate_security.py`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;

use a4p::credential_store::{InMemoryCredentialStore, UserCredentialRecord};
use a4p::intent::mandate::{CreateIntentMandate, create_intent_mandate};
use a4p::operation::mandate::{CreateOperationMandate, create_operation_mandate, sign_server_mandate};
use a4p::security::{ed25519_public_key_to_base64url, generate_ed25519_private_key};
use a4p::user_signature::A4PUserSignatureMethod;
use a4p::user_signature::webauthn::{WebAuthnSignatureMethod, b64url_decode, b64url_encode};
use a4p::{
    A4PServer, JsonDict, StaticA4PServerTrustStore, UserAuthorizationRequest,
    derive_user_authorization_challenge, user_authorization_challenge_base64url,
    verify_local_user_authorization_request,
};
use common::{explicit_server, obj};
use serde_json::{Value, json};

fn prepared_operation() -> TestResult<(A4PServer, JsonDict)> {
    let server = explicit_server("local://security-test")?;
    let operation = obj(json!({"action": "transfer", "params": {"amount": 10, "currency": "CNY"}}));
    let policy = obj(json!({"userVerification": "required"}));
    let mandate = create_operation_mandate(CreateOperationMandate {
        agent_id: "agent-1",
        validity_seconds: 60,
        user_signature_method: Some("webauthn"),
        user_signature_method_policy: Some(&policy),
        ..CreateOperationMandate::new(&operation, server.server_id())
    })?;
    Ok((server, mandate))
}

fn local_request(mandate: &JsonDict, signing_options: Option<JsonDict>) -> UserAuthorizationRequest {
    UserAuthorizationRequest {
        mandate: mandate.clone(),
        signing_options: signing_options.unwrap_or_default(),
    }
}

#[test]
fn authorization_ids_are_32_byte_random_values() -> TestResult {
    let actions = json!([{"name": "read", "params": {}}]);
    let operation = obj(json!({"action": "write", "params": {}}));
    let mut intent_ids = std::collections::HashSet::new();
    let mut operation_ids = std::collections::HashSet::new();
    for _ in 0..100 {
        let mandate = create_intent_mandate(CreateIntentMandate {
            user_signature_method: Some("ed25519"),
            ..CreateIntentMandate::new("local://security-test", "agent-1", Some(&actions))
        })?;
        intent_ids.insert(mandate["mandateId"].as_str().required()?.to_string());
        let mandate = create_operation_mandate(CreateOperationMandate {
            user_signature_method: Some("ed25519"),
            ..CreateOperationMandate::new(&operation, "local://security-test")
        })?;
        operation_ids.insert(mandate["operationId"].as_str().required()?.to_string());
    }
    assert_eq!(intent_ids.len(), 100);
    assert_eq!(operation_ids.len(), 100);
    for id in &intent_ids {
        assert!(id.starts_with("mdt_"));
        assert_eq!(b64url_decode(id.strip_prefix("mdt_").required()?)?.len(), 32);
    }
    for id in &operation_ids {
        assert!(id.starts_with("op_"));
        assert_eq!(b64url_decode(id.strip_prefix("op_").required()?)?.len(), 32);
    }
    Ok(())
}

fn set_path(mandate: &mut JsonDict, path: &[&str], value: Value) -> TestResult {
    if path.len() == 1 {
        mandate.insert(path[0].into(), value);
        return Ok(());
    }
    let mut target: &mut Value = mandate.get_mut(path[0]).required()?;
    for key in &path[1..path.len() - 1] {
        target = target.get_mut(*key).required()?;
    }
    target[path[path.len() - 1]] = value;
    Ok(())
}

#[test]
fn challenge_is_deterministic_and_binds_server_signed_mandate() -> TestResult {
    let (_server, mandate) = prepared_operation()?;
    let challenge = derive_user_authorization_challenge(&mandate)?;
    assert_eq!(challenge.len(), 32);
    assert_eq!(derive_user_authorization_challenge(&mandate)?, challenge);

    let mutations: Vec<(Vec<&str>, Value)> = vec![
        (vec!["operation", "params", "amount"], json!(11)),
        (vec!["subject", "id"], json!("agent:other")),
        (vec!["validTime", "until"], json!("2099-01-01T00:00:00Z")),
        (vec!["displayText"], json!("Approve something else")),
        (vec!["operationId"], json!("op_changed")),
        (
            vec!["signatures", "server", "signature"],
            json!(b64url_encode(&[b'x'; 64])),
        ),
    ];
    for (path, value) in mutations {
        let mut changed = mandate.clone();
        set_path(&mut changed, &path, value)?;
        assert_ne!(
            derive_user_authorization_challenge(&changed)?,
            challenge,
            "{path:?}"
        );
    }

    let mut with_user_signature = mandate.clone();
    with_user_signature["signatures"]["user"] = json!({
        "signatureMethod": "webauthn",
        "credentialId": "credential-1",
        "proof": {"assertion": {"signature": "first"}},
    });
    assert_eq!(
        derive_user_authorization_challenge(&with_user_signature)?,
        challenge
    );
    with_user_signature["signatures"]["user"]["proof"]["assertion"]["signature"] = json!("second");
    assert_eq!(
        derive_user_authorization_challenge(&with_user_signature)?,
        challenge
    );
    Ok(())
}

#[test]
fn local_authorizer_verifies_server_and_overwrites_untrusted_options() -> TestResult {
    let (server, mandate) = prepared_operation()?;
    let trust_store = StaticA4PServerTrustStore::new(&server.server_trust_config()?)?;
    let hardened = verify_local_user_authorization_request(
        &local_request(
            &mandate,
            Some(obj(json!({
                "signatureMethod": "webauthn",
                "methodOptions": {
                    "challenge": b64url_encode(b"attacker-controlled"),
                    "userVerification": "preferred",
                    "rpId": "localhost",
                },
            }))),
        ),
        &trust_store,
        Some("webauthn"),
    )?;
    assert_eq!(
        hardened["methodOptions"]["challenge"],
        Value::String(user_authorization_challenge_base64url(&mandate)?)
    );
    assert_eq!(hardened["methodOptions"]["userVerification"], "required");
    assert_eq!(hardened["methodOptions"]["rpId"], "localhost");

    let error = verify_local_user_authorization_request(
        &local_request(
            &mandate,
            Some(obj(json!({"signatureMethod": "ed25519", "methodOptions": {}}))),
        ),
        &trust_store,
        None,
    )
    .err_or_fail()?;
    assert_eq!(error.code, "SIGNING_OPTIONS_MISMATCH");
    let error = verify_local_user_authorization_request(
        &local_request(&mandate, Some(obj(json!({"signatureMethod": "webauthn"})))),
        &trust_store,
        None,
    )
    .err_or_fail()?;
    assert_eq!(error.message, "signingOptions.methodOptions missing");
    let error = verify_local_user_authorization_request(
        &local_request(&mandate, None),
        &trust_store,
        Some("ed25519"),
    )
    .err_or_fail()?;
    assert_eq!(error.code, "SIGNING_OPTIONS_MISMATCH");
    assert!(error.message.contains("User signer method mismatch"));
    Ok(())
}

#[test]
fn local_authorizer_rejects_untrusted_key_invalid_signature_and_expiry() -> TestResult {
    let (server, mandate) = prepared_operation()?;
    let trust_store = StaticA4PServerTrustStore::new(&server.server_trust_config()?)?;

    let mut untrusted = mandate.clone();
    untrusted["signatures"]["server"]["keyId"] = json!("server#unknown");
    let error = verify_local_user_authorization_request(&local_request(&untrusted, None), &trust_store, None)
        .err_or_fail()?;
    assert_eq!(error.code, "SERVER_KEY_UNTRUSTED");

    let mut tampered = mandate.clone();
    tampered.insert(
        "displayText".into(),
        json!("Approve an attacker-controlled operation"),
    );
    let error = verify_local_user_authorization_request(&local_request(&tampered, None), &trust_store, None)
        .err_or_fail()?;
    assert_eq!(error.code, "SERVER_SIGNATURE_INVALID");

    let mut expired = mandate.clone();
    expired["validTime"]["until"] = json!("2000-01-01T00:00:00Z");
    let expired = sign_server_mandate(&expired)?;
    let error = verify_local_user_authorization_request(&local_request(&expired, None), &trust_store, None)
        .err_or_fail()?;
    assert_eq!(error.code, "CHALLENGE_BINDING_INVALID");
    assert_eq!(error.message, "Mandate has expired");

    let wrong_public_key = ed25519_public_key_to_base64url(&generate_ed25519_private_key().verifying_key());
    let key_id = mandate["signatures"]["server"]["keyId"].as_str().required()?;
    let server_id = mandate["server"].as_str().required()?;
    let wrong_trust = StaticA4PServerTrustStore::new(&obj(json!({
        server_id: {key_id: {"alg": "EdDSA", "publicKey": wrong_public_key}},
    })))?;
    let error = verify_local_user_authorization_request(&local_request(&mandate, None), &wrong_trust, None)
        .err_or_fail()?;
    assert_eq!(error.code, "SERVER_SIGNATURE_INVALID");

    let mut unsigned = mandate.clone();
    unsigned["signatures"]["server"] = json!({});
    let error = verify_local_user_authorization_request(&local_request(&unsigned, None), &trust_store, None)
        .err_or_fail()?;
    assert_eq!(error.message, "Server signature metadata missing");
    Ok(())
}

#[test]
fn trust_store_validates_its_configuration() -> TestResult {
    let (server, _mandate) = prepared_operation()?;
    let config = server.server_trust_config()?;
    assert!(StaticA4PServerTrustStore::new(&config).is_ok());
    assert!(StaticA4PServerTrustStore::new(&obj(json!({}))).is_err());
    assert!(StaticA4PServerTrustStore::new(&obj(json!({"local://x": {}}))).is_err());
    assert!(
        StaticA4PServerTrustStore::new(&obj(
            json!({"local://x": {"k": {"alg": "RS256", "publicKey": "AA"}}})
        ))
        .is_err()
    );
    assert!(
        StaticA4PServerTrustStore::new(&obj(
            json!({"local://x": {"k": {"alg": "EdDSA", "publicKey": "AA"}}})
        ))
        .is_err()
    );
    assert!(StaticA4PServerTrustStore::new(&obj(json!({"local://x": {"k": "not-an-object"}}))).is_err());

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("trust.json");
    std::fs::write(&path, serde_json::to_string(&config)?)?;
    let from_file = StaticA4PServerTrustStore::from_json_file(&path)?;
    assert!(
        from_file
            .resolve(server.server_id(), "server#operation-mandate-k1", "EdDSA")
            .is_ok()
    );
    assert_eq!(
        from_file
            .resolve(server.server_id(), "server#operation-mandate-k1", "RS256")
            .err_or_fail()?
            .code,
        "SERVER_KEY_UNTRUSTED"
    );
    std::fs::write(&path, "[]")?;
    assert!(StaticA4PServerTrustStore::from_json_file(&path).is_err());
    Ok(())
}

#[test]
fn mandate_user_authorization_has_no_independent_challenge_fields() -> TestResult {
    let (_server, mandate) = prepared_operation()?;
    let user_authorization = mandate["userAuthorization"].as_object().required()?;
    assert!(!user_authorization.contains_key("challenge"));
    assert!(!user_authorization.contains_key("challengeMethod"));
    assert!(!user_authorization.contains_key("challengeNonce"));
    Ok(())
}

#[test]
fn webauthn_uses_raw_derived_challenge_bytes() -> TestResult {
    let mut record = UserCredentialRecord::new(
        "user-1",
        b64url_encode(b"credential-id"),
        "webauthn",
        obj(json!({"format": "cose", "value": b64url_encode(b"public-key")})),
    );
    record.details = obj(json!({"signCount": 0, "rpId": "localhost", "origin": "http://localhost:8970"}));
    let store = Arc::new(InMemoryCredentialStore::with_records(vec![record]));
    let (_server, mandate) = prepared_operation()?;
    let options = WebAuthnSignatureMethod::new(store).signing_options("user-1", &mandate)?;
    assert_eq!(options["signatureMethod"], "webauthn");
    let challenge = options["methodOptions"]["challenge"].as_str().required()?;
    assert_eq!(
        b64url_decode(challenge)?,
        derive_user_authorization_challenge(&mandate)?.to_vec()
    );
    assert_eq!(options["methodOptions"]["rpId"], "localhost");
    assert_eq!(
        options["methodOptions"]["allowCredentials"],
        json!([{"id": b64url_encode(b"credential-id"), "type": "public-key"}])
    );
    assert_eq!(options["methodOptions"]["userVerification"], "required");
    Ok(())
}
