// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/integration/test_operation_authorization_flow.py`.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a4p::http_server::A4PHTTPServer;
use a4p::{A4PServer, JsonDict};
use common::{default_explicit_server, explicit_server, obj, sign_user_mandate};
use serde_json::json;

fn note_operation(note_id: &str) -> JsonDict {
    obj(json!({"action": "delete_note", "params": {"note_id": note_id}}))
}

async fn prepare_signed(server: &A4PServer) -> TestResult<(JsonDict, JsonDict)> {
    let operation = note_operation("note-1");
    let challenge = server
        .prepare_operation_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "operation": operation,
            "validitySeconds": 60,
        })))
        .await?;
    let signed = sign_user_mandate(challenge.mandate.as_ref().required()?)?;
    Ok((operation, signed))
}

#[tokio::test]
async fn complete_requires_current_operation_and_matches_all_three_copies() -> TestResult {
    let server = explicit_server("local://test")?;
    let (operation, signed) = prepare_signed(&server).await?;

    let missing = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    let changed_current = server
        .complete_operation_authorization(obj(
            json!({"signedMandate": signed, "operation": note_operation("note-2")}),
        ))
        .await?;
    let mut changed_mandate = signed.clone();
    changed_mandate.insert(
        "operation".into(),
        serde_json::Value::Object(note_operation("note-2")),
    );
    let changed_signed = server
        .complete_operation_authorization(obj(
            json!({"signedMandate": changed_mandate, "operation": operation}),
        ))
        .await?;
    let invalid_current = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": {"params": {}}})))
        .await?;
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;

    assert_eq!(
        missing.verification_result.required()?.code.as_deref(),
        Some("OPERATION_INVALID")
    );
    assert_eq!(
        missing.reject_reason.as_deref(),
        Some("Operation must be an object")
    );
    assert_eq!(
        changed_current.verification_result.required()?.code.as_deref(),
        Some("OPERATION_PENDING_MISMATCH")
    );
    assert_eq!(
        changed_signed.verification_result.required()?.code.as_deref(),
        Some("MANDATE_PENDING_MISMATCH")
    );
    assert_eq!(
        invalid_current.verification_result.required()?.code.as_deref(),
        Some("OPERATION_INVALID")
    );
    assert_eq!(
        invalid_current.reject_reason.as_deref(),
        Some("Operation action missing")
    );
    assert!(completed.approved);
    assert_eq!(completed.operation_id.as_deref(), signed["operationId"].as_str());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_complete_approves_and_executes_only_once() -> TestResult {
    let server = Arc::new(explicit_server("local://test")?);
    let (operation, signed) = prepare_signed(&server).await?;
    let mut handles = Vec::new();
    for _ in 0..5 {
        let server = server.clone();
        let payload = obj(json!({"signedMandate": signed, "operation": operation}));
        handles.push(tokio::spawn(async move {
            let result = server.complete_operation_authorization(payload).await?;
            Ok::<Option<String>, ap_support::testing::TestError>(if result.approved {
                result.operation_id
            } else {
                None
            })
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await??);
    }
    let approved = results.iter().filter(|r| r.is_some()).count();
    assert_eq!(approved, 1);
    assert_eq!(results.iter().filter(|r| r.is_none()).count(), 4);
    assert_eq!(
        results.iter().flatten().next().map(String::as_str),
        signed["operationId"].as_str()
    );
    Ok(())
}

#[tokio::test]
async fn business_failure_does_not_restore_consumed_authorization() -> TestResult {
    let server = explicit_server("local://test")?;
    let (operation, signed) = prepare_signed(&server).await?;
    let completed = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    assert!(completed.approved);

    let business: Result<(), &str> = Err("business failed");
    assert_eq!(business, Err("business failed"));

    let replay = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    assert_eq!(
        replay.verification_result.required()?.code.as_deref(),
        Some("AUTHORIZATION_NOT_PENDING")
    );
    Ok(())
}

#[tokio::test]
async fn prepare_rejects_missing_identity_and_invalid_validity() -> TestResult {
    let cases = [
        (
            json!({"userId": "user-1", "operation": note_operation("note-1")}),
            "agentId missing",
        ),
        (
            json!({"agentId": "agent-1", "operation": note_operation("note-1")}),
            "userId missing",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "operation": note_operation("note-1"), "validitySeconds": 0}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "operation": note_operation("note-1"), "validitySeconds": true}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "operation": note_operation("note-1"), "validitySeconds": "60"}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1"}),
            "Operation must be an object",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "operation": {"action": "x", "params": []}}),
            "Operation params must be an object",
        ),
    ];
    for (payload, reason) in cases {
        let challenge = default_explicit_server()?
            .prepare_operation_authorization(obj(payload.clone()))
            .await?;
        assert!(challenge.mandate.is_none(), "{payload}");
        assert_eq!(challenge.reject_reason.as_deref(), Some(reason), "{payload}");
        assert_eq!(
            challenge.verification_result.required()?.code.as_deref(),
            Some("MANDATE_INVALID")
        );
    }
    Ok(())
}

#[tokio::test]
async fn expired_and_lost_pending_authorizations_fail_closed() -> TestResult {
    let server = explicit_server("local://test")?;
    let (operation, signed) = prepare_signed(&server).await?;
    let operation_id = signed["operationId"].as_str().required()?;
    assert!(server.operation_service().has_pending(operation_id));
    assert!(server.operation_service().set_pending_expiry(operation_id, 1));
    assert!(!server.operation_service().set_pending_expiry("op_unknown", 1));

    let expired = server
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;
    let restarted = explicit_server("local://test")?
        .complete_operation_authorization(obj(json!({"signedMandate": signed, "operation": operation})))
        .await?;

    assert_eq!(
        expired.verification_result.required()?.code.as_deref(),
        Some("AUTHORIZATION_NOT_PENDING")
    );
    assert!(!server.operation_service().has_pending(operation_id));
    assert_eq!(
        restarted.verification_result.required()?.code.as_deref(),
        Some("AUTHORIZATION_NOT_PENDING")
    );
    Ok(())
}

#[tokio::test]
async fn prepare_prunes_expired_pending_entries() -> TestResult {
    let server = explicit_server("local://test")?;
    let (_operation, signed) = prepare_signed(&server).await?;
    let operation_id = signed["operationId"].as_str().required()?;
    server.operation_service().set_pending_expiry(operation_id, 1);
    let (_operation, _signed_again) = prepare_signed(&server).await?;
    assert_eq!(server.operation_service().pending_len(), 1);
    assert!(!server.operation_service().has_pending(operation_id));
    Ok(())
}

#[tokio::test]
async fn legacy_direct_authorization_apis_are_removed() -> TestResult {
    let server = A4PHTTPServer::new(Arc::new(default_explicit_server()?), Some("127.0.0.1"), Some(0))?;
    for path in [
        "/a4p/v1/intent-authorizations",
        "/a4p/v1/operation-authorizations",
        "/a4p/v1/user-credentials/webauthn/authorization/options",
    ] {
        let (status, payload) = server.dispatch(path, JsonDict::new()).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(serde_json::Value::Object(payload), json!({"error": "not_found"}));
    }
    Ok(())
}
