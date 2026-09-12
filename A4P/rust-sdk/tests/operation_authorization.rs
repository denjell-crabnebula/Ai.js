// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/integration/test_operation_authorization.py`.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a4p::credential_store::InMemoryCredentialStore;
use a4p::user_signature::webauthn::{WebAuthnSignatureMethod, WebAuthnUserSigner, b64url_decode};
use a4p::{
    A4PServer, JsonDict, approve_user_mandate, derive_user_authorization_challenge,
    sign_user_mandate_with_signer,
};
use common::{
    SoftwareAuthenticator, explicit_server, explicit_server_builder, obj, sign_user_mandate, usage_store,
};
use serde_json::{Value, json};

fn operation() -> JsonDict {
    obj(json!({"action": "delete_note", "params": {"note_id": "note-1"}}))
}

fn request(agent_id: &str, user_id: &str, operation: &JsonDict) -> JsonDict {
    obj(json!({
        "agentId": agent_id,
        "userId": user_id,
        "operation": operation,
        "validitySeconds": 60,
    }))
}

#[tokio::test]
async fn custom_operation_display_text_renderer_is_signed() -> TestResult {
    let renderer: a4p::OperationDisplayTextRenderer = Arc::new(|mandate: JsonDict| {
        format!(
            "授权删除笔记：{}",
            mandate["operation"]["params"]["note_id"].as_str().unwrap_or("")
        )
    });
    let server = explicit_server_builder()
        .server_id("local://test")
        .operation_display_text_renderer(renderer)
        .build()?;
    let operation = operation();
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &operation))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["displayText"], "授权删除笔记：note-1");
    let signed = sign_user_mandate(&mandate)?;
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    assert!(completed.approved);
    assert_eq!(completed.operation_id.as_deref(), signed["operationId"].as_str());
    Ok(())
}

#[tokio::test]
async fn default_operation_display_text_describes_the_call() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &operation()))
        .await?;
    let mandate = prepared.mandate.required()?;
    let text = mandate["displayText"].as_str().required()?;
    assert!(
        text.starts_with("授权执行 delete_note(note_id=\"note-1\")（有效期至 "),
        "{text}"
    );
    assert!(text.ends_with(" 北京时间）"), "{text}");
    assert_eq!(mandate["validTime"]["timezone"], "Asia/Shanghai");
    assert_eq!(
        mandate["subject"],
        json!({"type": "agent", "id": "agent:agent-1"})
    );
    assert_eq!(
        mandate["signatures"]["server"]["keyId"],
        "server#operation-mandate-k1"
    );
    assert_eq!(
        prepared.signing_options["signatureMethod"],
        Value::String("ed25519".into())
    );

    let no_params = server
        .prepare_operation_authorization(request(
            "agent-1",
            "user-1",
            &obj(json!({"action": "list_notes"})),
        ))
        .await?;
    let mandate = no_params.mandate.required()?;
    assert!(
        mandate["displayText"]
            .as_str()
            .required()?
            .starts_with("授权执行 list_notes(无参数)（有效期至 ")
    );
    assert_eq!(
        mandate["operation"],
        json!({"action": "list_notes", "params": {}})
    );
    Ok(())
}

#[tokio::test]
async fn complete_operation_rejects_mandate_from_another_pending_request() -> TestResult {
    let server = explicit_server("local://test")?;
    let operation = operation();
    let first = server
        .prepare_operation_authorization(request("agent-a", "user-a", &operation))
        .await?;
    let second = server
        .prepare_operation_authorization(request("agent-b", "user-b", &operation))
        .await?;
    let first_mandate = first.mandate.required()?;
    let second_mandate = second.mandate.required()?;
    let mut first_signed = sign_user_mandate(&first_mandate)?;
    first_signed.insert("operationId".into(), second_mandate["operationId"].clone());

    let mismatched = server
        .complete_operation_authorization(obj(
            json!({"signedMandate": first_signed, "operation": operation}),
        ))
        .await?;
    let second_signed = sign_user_mandate(&second_mandate)?;
    let completed = server
        .complete_operation_authorization(obj(
            json!({"signedMandate": second_signed, "operation": operation}),
        ))
        .await?;

    assert!(!mismatched.approved);
    assert_eq!(
        mismatched.verification_result.required()?.code.as_deref(),
        Some("MANDATE_PENDING_MISMATCH")
    );
    assert!(completed.approved);
    assert_eq!(
        completed.operation_id.as_deref(),
        second_signed["operationId"].as_str()
    );
    Ok(())
}

#[tokio::test]
async fn prepare_complete_operation_rejects_replay() -> TestResult {
    let server = explicit_server("local://test")?;
    let operation = operation();
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &operation))
        .await?;
    let signed = sign_user_mandate(&prepared.mandate.required()?)?;
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    let replay = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    assert!(completed.approved);
    assert!(!replay.approved);
    assert_eq!(
        replay.verification_result.required()?.code.as_deref(),
        Some("AUTHORIZATION_NOT_PENDING")
    );
    Ok(())
}

#[tokio::test]
async fn operation_no_signature_prepare_and_complete_returns_operation_id() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .require_user_signature(false)
        .build()?;
    let operation = operation();
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &operation))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["userAuthorization"], json!({"required": false}));
    let approved = approve_user_mandate(&mandate);
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": approved, "operation": operation})))
        .await?;
    assert!(completed.approved);
    assert_eq!(
        completed.operation_id.as_deref(),
        approved["operationId"].as_str()
    );
    Ok(())
}

#[tokio::test]
async fn operation_no_signature_approval_rejected_when_signature_required() -> TestResult {
    let server = explicit_server("local://test")?;
    let operation = operation();
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &operation))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["userAuthorization"]["required"], true);
    let approved = approve_user_mandate(&mandate);
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": approved, "operation": operation})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("MANDATE_SIGNATURE_INVALID")
    );
    assert_eq!(completed.reject_reason.as_deref(), Some("User signature missing"));
    Ok(())
}

#[tokio::test]
async fn operation_webauthn_user_signature_path_is_accepted() -> TestResult {
    let store = Arc::new(InMemoryCredentialStore::new());
    let method = Arc::new(WebAuthnSignatureMethod::new(store.clone()));
    let mut authenticator = SoftwareAuthenticator::new_p256("localhost", "http://localhost:8970");
    let server: A4PServer = A4PServer::builder()
        .server_id("local://test")
        .user_signature_method(method.clone())
        .intent_token_usage_store(usage_store())
        .build()?;

    let options =
        server.webauthn_registration_options(&obj(json!({"userId": "user-1", "userName": "Alice"})))?;
    assert_eq!(options["options"]["user"]["name"], "Alice");
    assert_eq!(options["options"]["user"]["displayName"], "Alice");
    let credential = authenticator.register(options["options"]["challenge"].as_str().required()?)?;
    let registered = server.verify_webauthn_registration(&obj(json!({
        "registrationRequestId": options["registrationRequestId"],
        "userId": "user-1",
        "credential": credential,
    })))?;
    assert_eq!(registered["registered"], true);
    assert_eq!(registered["created"], true);
    let credential_id = registered["credential"]["credentialId"]
        .as_str()
        .required()?
        .to_string();
    assert_eq!(credential_id, authenticator.credential_id_b64());

    let expected = operation();
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &expected))
        .await?;
    let mandate = prepared.mandate.required()?;
    let challenge = prepared.signing_options["methodOptions"]["challenge"]
        .as_str()
        .required()?;
    assert_eq!(
        b64url_decode(challenge)?,
        derive_user_authorization_challenge(&mandate)?.to_vec()
    );
    assert_eq!(
        prepared.signing_options["methodOptions"]["userVerification"],
        "required"
    );
    assert_eq!(
        prepared.signing_options["methodOptions"]["allowCredentials"],
        json!([{"id": credential_id, "type": "public-key"}])
    );

    let assertion = authenticator.assert(challenge)?;
    let signed = sign_user_mandate_with_signer(
        &mandate,
        &WebAuthnUserSigner::new(),
        Some(&obj(json!({"assertion": assertion}))),
    )?;
    assert_eq!(signed["signatures"]["user"]["credentialId"], credential_id);
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": expected})))
        .await?;
    assert!(completed.approved, "{:?}", completed.reject_reason);
    assert_eq!(completed.operation_id.as_deref(), signed["operationId"].as_str());

    // The intent flow accepts the same credential.
    let prepared = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]},
            "validitySeconds": 60,
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    let challenge = prepared.signing_options["methodOptions"]["challenge"]
        .as_str()
        .required()?;
    let assertion = authenticator.assert(challenge)?;
    let signed = sign_user_mandate_with_signer(
        &mandate,
        &WebAuthnUserSigner::new(),
        Some(&obj(json!({"assertion": assertion}))),
    )?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved, "{:?}", completed.reject_reason);
    assert!(completed.intent_token.is_some());

    // An assertion for a different mandate (wrong challenge) is rejected.
    let prepared = server
        .prepare_operation_authorization(request("agent-1", "user-1", &expected))
        .await?;
    let mandate = prepared.mandate.required()?;
    let assertion = authenticator.assert(challenge)?;
    let signed = sign_user_mandate_with_signer(
        &mandate,
        &WebAuthnUserSigner::new(),
        Some(&obj(json!({"assertion": assertion}))),
    )?;
    let rejected = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": expected})))
        .await?;
    assert!(!rejected.approved);
    let reason = rejected.reject_reason.unwrap_or_default();
    assert!(
        reason.contains("Client data challenge was not expected challenge"),
        "{reason}"
    );
    assert_eq!(
        rejected.verification_result.required()?.code.as_deref(),
        Some("MANDATE_SIGNATURE_INVALID")
    );
    Ok(())
}
