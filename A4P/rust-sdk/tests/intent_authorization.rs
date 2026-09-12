// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/integration/test_intent_authorization.py`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;

use a4p::errors::IntentTokenUsageStoreError;
use a4p::intent::mandate::{
    CreateIntentMandate, VerifyIntentMandate, create_intent_mandate, sign_server_mandate,
    verify_intent_mandate,
};
use a4p::intent::scope::normalize_execution_policy;
use a4p::intent::token::{IssueIntentToken, VerifyIntentToken, issue_intent_token, verify_intent_token};
use a4p::intent::usage_store::{A4PIntentTokenUsageStore, SQLiteIntentTokenUsageStore};
use a4p::{A4PServer, IntentAuthorizationResponse, JsonDict, approve_user_mandate};
use common::{default_explicit_server, explicit_server, explicit_server_builder, obj, sign_user_mandate};
use serde_json::{Value, json};

async fn prepare_and_complete_intent(
    server: &A4PServer,
    request: JsonDict,
) -> TestResult<IntentAuthorizationResponse> {
    let prepared = server.prepare_intent_authorization(request).await?;
    let signed = sign_user_mandate(prepared.mandate.as_ref().required()?)?;
    Ok(server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?)
}

fn delete_note_request(intent: Value) -> JsonDict {
    obj(json!({
        "agentId": "agent-1",
        "userId": "user-1",
        "intent": intent,
        "validitySeconds": 60,
    }))
}

#[tokio::test]
async fn prepare_complete_intent_preserves_agent_key_binding() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "agentPublicKey": {"kty": "OKP", "kid": "agent-key-1", "crv": "Ed25519", "x": "abc"},
            "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]},
            "validitySeconds": 60,
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["subject"]["agentKey"]["kid"], "agent-key-1");
    assert!(
        mandate["displayText"]
            .as_str()
            .required()?
            .contains("授权 agent:agent-1 在 ")
    );
    assert!(
        mandate["displayText"]
            .as_str()
            .required()?
            .contains("期间调用 delete_note(note_id=任意)")
    );
    let signed = sign_user_mandate(&mandate)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved);
    let token = completed.intent_token.clone().required()?;
    let completed_mandate = completed.mandate.required()?;
    assert_eq!(completed_mandate["signatures"]["server"]["alg"], "EdDSA");
    assert_eq!(completed_mandate["signatures"]["user"]["proof"]["alg"], "EdDSA");
    assert_eq!(token["alg"], "EdDSA");
    assert_eq!(token["subject"]["agentKey"]["kid"], "agent-key-1");
    assert_eq!(
        token["keyId"],
        format!(
            "server#intent-token-v1:{}",
            mandate["mandateId"].as_str().required()?
        )
    );
    assert_eq!(token["tokenId"].as_str().required()?.len(), 32);
    assert_eq!(token["user"], json!({"id": "user-1"}));
    let params = obj(json!({"note_id": "note-1"}));
    let result = verify_intent_token(
        &token,
        VerifyIntentToken {
            expected_agent_key_id: Some("agent-key-1"),
            ..VerifyIntentToken::new("delete_note", Some(&params))
        },
    );
    assert_eq!(result, Ok(()));
    Ok(())
}

#[tokio::test]
async fn prepare_intent_rejects_missing_identity_and_invalid_validity() -> TestResult {
    let actions = json!([{"name": "delete_note", "params": {}}]);
    let cases = [
        (
            json!({"userId": "user-1", "intent": {"actions": actions}}),
            "agentId missing",
        ),
        (
            json!({"agentId": "agent-1", "intent": {"actions": actions}}),
            "userId missing",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": actions}, "validitySeconds": 0}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": actions}, "validitySeconds": true}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": actions}, "validitySeconds": "60"}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": actions}, "validitySeconds": 1.5}),
            "validitySeconds must be a positive integer",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {}}),
            "Intent actions must be a list",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": []}}),
            "Intent actions must not be empty",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": [{"params": {}}]}}),
            "Intent action name missing",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": [{"name": "x", "params": 1}]}}),
            "Action params must be an object or '*'",
        ),
        (
            json!({"agentId": "agent-1", "userId": "user-1", "intent": {"actions": [{"name": "x", "allowExtraParams": 1}]}}),
            "Intent action allowExtraParams must be a boolean",
        ),
    ];
    for (request, reason) in cases {
        let prepared = default_explicit_server()?
            .prepare_intent_authorization(obj(request.clone()))
            .await?;
        assert!(prepared.mandate.is_none(), "{request}");
        assert_eq!(prepared.reject_reason.as_deref(), Some(reason), "{request}");
        assert_eq!(
            prepared.verification_result.required()?.code.as_deref(),
            Some("MANDATE_INVALID")
        );
    }
    Ok(())
}

#[test]
fn missing_user_signature_method_is_rejected() -> TestResult {
    let actions = json!([{"name": "delete_note", "params": {}}]);
    let mut mandate = create_intent_mandate(CreateIntentMandate {
        user_signature_method: Some("ed25519"),
        ..CreateIntentMandate::new("local://test", "agent-1", Some(&actions))
    })?;
    mandate["userAuthorization"]
        .as_object_mut()
        .required()?
        .remove("signatureMethod");
    let mandate = sign_server_mandate(&mandate)?;
    let result = verify_intent_mandate(
        &mandate,
        VerifyIntentMandate {
            expected_server: Some("local://test"),
            ..VerifyIntentMandate::new()
        },
    );
    assert_eq!(result, Err("userAuthorization.signatureMethod missing".into()));

    let error = create_intent_mandate(CreateIntentMandate::new(
        "local://test",
        "agent-1",
        Some(&actions),
    ))
    .err_or_fail()?;
    assert_eq!(error.to_string(), "userSignatureMethod missing");
    Ok(())
}

#[tokio::test]
async fn complete_intent_rejects_mandate_from_another_pending_request() -> TestResult {
    let server = explicit_server("local://test")?;
    let broad = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-a",
            "userId": "user-a",
            "intent": {"actions": [{"name": "delete_all", "params": "*"}]},
            "validitySeconds": 60,
        })))
        .await?;
    let benign = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-b",
            "userId": "user-b",
            "intent": {"actions": [{"name": "read_note", "params": {"note_id": "note-1"}}]},
            "validitySeconds": 60,
        })))
        .await?;
    let broad_mandate = broad.mandate.required()?;
    let benign_mandate = benign.mandate.required()?;
    let mut broad_signed = sign_user_mandate(&broad_mandate)?;
    broad_signed.insert("mandateId".into(), benign_mandate["mandateId"].clone());

    let mismatched = server
        .complete_intent_authorization(obj(json!({"signedMandate": broad_signed})))
        .await?;
    let benign_signed = sign_user_mandate(&benign_mandate)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": benign_signed})))
        .await?;

    assert!(!mismatched.approved);
    assert_eq!(
        mismatched.verification_result.required()?.code.as_deref(),
        Some("MANDATE_PENDING_MISMATCH")
    );
    assert_eq!(mismatched.mandate.required()?, benign_mandate);
    assert!(completed.approved);
    let token = completed.intent_token.required()?;
    assert_eq!(token["user"]["id"], "user-b");
    assert_eq!(token["subject"]["id"], "agent:agent-b");
    Ok(())
}

#[tokio::test]
async fn complete_intent_rejects_unknown_mandate_id() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "read_note", "params": {"note_id": "note-1"}}]
        })))
        .await?;
    let mut signed = sign_user_mandate(&prepared.mandate.required()?)?;
    signed.insert("mandateId".into(), json!("mdt_unknown"));
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("AUTHORIZATION_NOT_PENDING")
    );
    assert_eq!(
        completed.reject_reason.as_deref(),
        Some("No pending intent authorization: mdt_unknown")
    );

    let missing = server.complete_intent_authorization(obj(json!({}))).await?;
    assert_eq!(missing.reject_reason.as_deref(), Some("signedMandate missing"));
    assert_eq!(
        missing.verification_result.required()?.code.as_deref(),
        Some("MANDATE_INVALID")
    );
    let no_id = server
        .complete_intent_authorization(obj(json!({"signedMandate": {"type": "a4p/v1/intent-mandate"}})))
        .await?;
    assert_eq!(no_id.reject_reason.as_deref(), Some("mandateId missing"));
    let bad_type = server
        .complete_intent_authorization(obj(json!({"signedMandate": {"type": "x"}})))
        .await?;
    assert_eq!(
        bad_type.reject_reason.as_deref(),
        Some("Unsupported A4P mandate type: 'x'")
    );
    Ok(())
}

#[tokio::test]
async fn prepare_generates_independent_pending_mandate_ids() -> TestResult {
    let server = explicit_server("local://test")?;
    let first = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "read_note", "params": {"note_id": "note-1"}}]
        })))
        .await?;
    let duplicate = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-2",
            "userId": "user-2",
            "intent": {"actions": [{"name": "delete_all", "params": "*"}]},
            "validitySeconds": 60,
        })))
        .await?;
    let first_mandate = first.mandate.required()?;
    let duplicate_mandate = duplicate.mandate.required()?;
    assert_ne!(duplicate_mandate["mandateId"], first_mandate["mandateId"]);
    assert_eq!(server.intent_service().pending_len(), 2);
    let signed = sign_user_mandate(&first_mandate)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved);
    assert_eq!(server.intent_service().pending_len(), 1);
    Ok(())
}

#[tokio::test]
async fn intent_execution_policy_is_signed_and_copied_to_token() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
            "executionPolicy": {"maxExecutions": 2},
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    let policy = mandate["intent"]["executionPolicy"].clone();
    assert_eq!(policy, json!({"maxExecutions": 2}));
    assert!(mandate["displayText"].as_str().required()?.contains("最多 2 次"));

    let signed = sign_user_mandate(&mandate)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved);
    let token = completed.intent_token.required()?;
    assert_eq!(token["intent"]["executionPolicy"], policy);

    let mut tampered = token.clone();
    tampered["intent"]["executionPolicy"]["maxExecutions"] = json!(3);
    let params = obj(json!({"note_id": "note-1"}));
    let reason =
        verify_intent_token(&tampered, VerifyIntentToken::new("delete_note", Some(&params))).err_or_fail()?;
    assert!(reason.contains("signature invalid"), "{reason}");
    Ok(())
}

#[test]
fn intent_execution_policy_ignores_unknown_fields() -> TestResult {
    let expected = obj(json!({"maxExecutions": 2}));
    assert_eq!(
        normalize_execution_policy(Some(&json!({
            "maxExecutions": 2,
            "period": {"periodSeconds": 86400, "maxExecutions": 1},
        })))?,
        Some(expected.clone())
    );
    assert_eq!(
        normalize_execution_policy(Some(&json!({"maxExecutions": 2, "unknownPolicyOption": true})))?,
        Some(expected)
    );
    assert_eq!(normalize_execution_policy(None)?, None);
    Ok(())
}

#[tokio::test]
async fn intent_execution_policy_total_cap_is_consumed_by_server_verification() -> TestResult {
    let server = explicit_server("local://test")?;
    let auth = prepare_and_complete_intent(
        &server,
        delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
            "executionPolicy": {"maxExecutions": 2},
        })),
    )
    .await?;
    let token = auth.intent_token.required()?;
    let verify = |note_id: &str| {
        server.verify_intent_token(obj(json!({
            "token": token,
            "expected": {"action": "delete_note", "params": {"note_id": note_id}},
        })))
    };
    let first = verify("note-1").await?;
    let second = verify("note-2").await?;
    let third = verify("note-3").await?;

    assert!(first.valid);
    let scope = first.matched_scope.required()?;
    assert_eq!(scope["usage"], json!({"executionsUsed": 1, "executionsLimit": 2}));
    assert_eq!(scope["action"], "delete_note");
    assert_eq!(scope["params"], json!({"note_id": "note-1"}));
    assert!(second.valid);
    assert_eq!(second.matched_scope.required()?["usage"]["executionsUsed"], 2);
    assert!(!third.valid);
    assert_eq!(third.code.as_deref(), Some("TOKEN_USAGE_EXCEEDED"));
    assert_eq!(third.reason.as_deref(), Some("Token execution usage exceeded"));
    Ok(())
}

#[tokio::test]
async fn intent_execution_policy_usage_persists_across_server_instances() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("usage.sqlite3");
    let first_server = explicit_server_builder()
        .server_id("local://test")
        .intent_token_usage_store(Arc::new(SQLiteIntentTokenUsageStore::new(Some(&path))?))
        .build()?;
    let auth = prepare_and_complete_intent(
        &first_server,
        delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
            "executionPolicy": {"maxExecutions": 1},
        })),
    )
    .await?;
    let request = obj(json!({
        "token": auth.intent_token.required()?,
        "expected": {"action": "delete_note", "params": {"note_id": "note-1"}},
    }));
    let first = first_server.verify_intent_token(&request).await?;
    let restarted_server = explicit_server_builder()
        .server_id("local://test")
        .intent_token_usage_store(Arc::new(SQLiteIntentTokenUsageStore::new(Some(&path))?))
        .build()?;
    let after_restart = restarted_server.verify_intent_token(&request).await?;
    assert!(first.valid);
    assert!(!after_restart.valid);
    assert_eq!(after_restart.code.as_deref(), Some("TOKEN_USAGE_EXCEEDED"));
    Ok(())
}

struct FailingUsageStore;

impl A4PIntentTokenUsageStore for FailingUsageStore {
    fn consume(&self, _: &str, _: i64, _: i64) -> Result<(bool, i64), IntentTokenUsageStoreError> {
        Err(IntentTokenUsageStoreError("database unavailable".into()))
    }
}

#[tokio::test]
async fn intent_usage_store_failure_is_fail_closed() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .intent_token_usage_store(Arc::new(FailingUsageStore))
        .build()?;
    let auth = prepare_and_complete_intent(
        &server,
        delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
            "executionPolicy": {"maxExecutions": 1},
        })),
    )
    .await?;
    let verified = server
        .verify_intent_token(obj(json!({
            "token": auth.intent_token.required()?,
            "expected": {"action": "delete_note", "params": {"note_id": "note-1"}},
        })))
        .await?;
    assert!(!verified.valid);
    assert_eq!(verified.code.as_deref(), Some("TOKEN_USAGE_STORE_ERROR"));
    assert_eq!(
        verified.reason.as_deref(),
        Some("Intent token usage store unavailable")
    );
    Ok(())
}

struct UnexpectedUsageStore;

impl A4PIntentTokenUsageStore for UnexpectedUsageStore {
    fn consume(&self, _: &str, _: i64, _: i64) -> Result<(bool, i64), IntentTokenUsageStoreError> {
        Err(IntentTokenUsageStoreError(
            "usage store must not be called".into(),
        ))
    }
}

#[tokio::test]
async fn intent_without_execution_policy_does_not_access_usage_store() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .intent_token_usage_store(Arc::new(UnexpectedUsageStore))
        .build()?;
    let auth = prepare_and_complete_intent(
        &server,
        delete_note_request(json!({"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]})),
    )
    .await?;
    let verified = server
        .verify_intent_token(obj(json!({
            "token": auth.intent_token.required()?,
            "expected": {"action": "delete_note", "params": {"note_id": "note-1"}},
        })))
        .await?;
    assert!(verified.valid);
    assert!(!verified.matched_scope.required()?.contains_key("usage"));
    Ok(())
}

#[tokio::test]
async fn invalid_intent_execution_policies_reject_mandate_creation() -> TestResult {
    let cases = [
        json!({"maxExecutions": 0}),
        json!({"maxExecutions": -1}),
        json!({"maxExecutions": 1.5}),
        json!({"maxExecutions": "2"}),
        json!(null),
        json!({}),
        json!({"period": {"periodSeconds": 60, "maxExecutions": 1}}),
        json!({"unknownPolicyOption": true}),
        json!([]),
    ];
    for policy in cases {
        let server = explicit_server("local://test")?;
        let prepared = server
            .prepare_intent_authorization(delete_note_request(json!({
                "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
                "executionPolicy": policy,
            })))
            .await?;
        assert!(!prepared.approved, "{policy}");
        assert!(prepared.mandate.is_none(), "{policy}");
        assert_eq!(
            prepared.verification_result.required()?.code.as_deref(),
            Some("MANDATE_INVALID"),
            "{policy}"
        );
        assert_eq!(server.intent_service().pending_len(), 0);
    }
    Ok(())
}

#[tokio::test]
async fn intent_no_signature_prepare_and_complete_issues_token() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .require_user_signature(false)
        .build()?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}]
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["userAuthorization"], json!({"required": false}));
    let approved = approve_user_mandate(&mandate);
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": approved})))
        .await?;
    assert!(completed.approved);
    assert!(completed.intent_token.is_some());
    assert_eq!(completed.mandate.required()?["signatures"]["user"], json!({}));
    Ok(())
}

#[tokio::test]
async fn intent_no_signature_approval_rejected_when_signature_required() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}]
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["userAuthorization"]["required"], true);
    let approved = approve_user_mandate(&mandate);
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": approved})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("MANDATE_SIGNATURE_INVALID")
    );
    assert_eq!(completed.reject_reason.as_deref(), Some("User signature missing"));
    Ok(())
}

#[test]
fn intent_no_signature_primitive_token_issue_requires_explicit_opt_out() -> TestResult {
    let actions = json!([{"name": "delete_note", "params": {"note_id": "*"}}]);
    let mandate = create_intent_mandate(CreateIntentMandate {
        validity_seconds: 60,
        require_user_signature: false,
        ..CreateIntentMandate::new("local://test", "agent-1", Some(&actions))
    })?;
    let approved = approve_user_mandate(&mandate);
    let error = issue_intent_token(&approved, IssueIntentToken::new("user-1")).err_or_fail()?;
    assert!(
        error.to_string().contains("User signature method missing"),
        "{error}"
    );
    let token = issue_intent_token(
        &approved,
        IssueIntentToken {
            require_user_signature: false,
            ..IssueIntentToken::new("user-1")
        },
    )?;
    assert_eq!(token["type"], "a4p/v1/intent-token");
    Ok(())
}

#[tokio::test]
async fn intent_no_signature_mode_rejects_nonempty_user_signature() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .require_user_signature(false)
        .build()?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}]
        })))
        .await?;
    let mut approved = approve_user_mandate(&prepared.mandate.required()?);
    approved["signatures"]["user"] = json!({"signatureMethod": "ed25519"});
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": approved})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.reject_reason.as_deref(),
        Some("User signature must be empty")
    );
    Ok(())
}

#[tokio::test]
async fn intent_no_signature_still_rejects_tampered_mandate_core() -> TestResult {
    let server = explicit_server_builder()
        .server_id("local://test")
        .require_user_signature(false)
        .build()?;
    let prepared = server
        .prepare_intent_authorization(delete_note_request(json!({
            "actions": [{"name": "delete_note", "params": {"note_id": "*"}}]
        })))
        .await?;
    let mut approved = approve_user_mandate(&prepared.mandate.required()?);
    approved.insert("displayText".into(), json!("tampered"));
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": approved})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("MANDATE_PENDING_MISMATCH")
    );
    assert_eq!(
        completed.reject_reason.as_deref(),
        Some("Signed mandate does not match pending intent authorization")
    );
    Ok(())
}

#[tokio::test]
async fn custom_intent_display_text_renderer_is_signed() -> TestResult {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<JsonDict>::new()));
    let seen_for_renderer = seen.clone();
    let renderer: a4p::IntentDisplayTextRenderer = Arc::new(move |mut mandate: JsonDict| {
        seen_for_renderer.lock().push(mandate.clone());
        let Some(params) = mandate["intent"]["actions"][0]["params"].as_object_mut() else {
            return String::new();
        };
        let text = format!(
            "授权 {} 支付，最高金额 {:.2} {}",
            params["merchant"].as_str().unwrap_or(""),
            params["max_amount_cents"].as_i64().unwrap_or(0) as f64 / 100.0,
            params["currency"].as_str().unwrap_or("")
        );
        params.insert("max_amount_cents".into(), json!(1));
        text
    });
    let server = explicit_server_builder()
        .server_id("local://test")
        .intent_display_text_renderer(renderer)
        .build()?;
    let prepared = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "payment-agent",
            "userId": "user-1",
            "intent": {
                "actions": [{
                    "name": "pay_order",
                    "params": {"merchant": "luckin", "currency": "CNY", "max_amount_cents": 2000},
                }]
            },
            "validitySeconds": 60,
        })))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert!(!seen.lock().is_empty());
    assert_eq!(mandate["displayText"], "授权 luckin 支付，最高金额 20.00 CNY");
    assert_eq!(
        mandate["intent"]["actions"][0]["params"]["max_amount_cents"],
        2000
    );

    let signed = sign_user_mandate(&mandate)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved);
    assert!(completed.intent_token.is_some());
    Ok(())
}

#[tokio::test]
async fn intent_token_verification_binds_identity_through_the_server() -> TestResult {
    let server = explicit_server("local://test")?;
    let auth = prepare_and_complete_intent(
        &server,
        delete_note_request(json!({"actions": [{"name": "delete_note", "params": {"note_id": "note-*"}}]})),
    )
    .await?;
    let token = auth.intent_token.required()?;
    let verify =
        |expected: Value| server.verify_intent_token(obj(json!({"token": token, "expected": expected})));
    let ok = verify(json!({
        "action": "delete_note",
        "params": {"note_id": "note-1"},
        "agentId": "agent:agent-1",
        "userId": "user-1",
    }))
    .await?;
    assert!(ok.valid, "{:?}", ok.reason);
    let wrong_agent = verify(json!({
        "action": "delete_note",
        "params": {"note_id": "note-1"},
        "agentId": "agent:other",
    }))
    .await?;
    assert_eq!(wrong_agent.code.as_deref(), Some("TOKEN_SCOPE_MISMATCH"));
    let wrong_params = verify(json!({"action": "delete_note", "params": {"note_id": "other"}})).await?;
    assert_eq!(wrong_params.code.as_deref(), Some("TOKEN_SCOPE_MISMATCH"));
    assert_eq!(
        wrong_params.reason.as_deref(),
        Some("Param 'note_id' mismatch for action 'delete_note'")
    );
    let no_params = verify(json!({"action": "delete_note"})).await?;
    assert_eq!(
        no_params.reason.as_deref(),
        Some("Required param 'note_id' missing for action 'delete_note'")
    );
    let missing_token = server
        .verify_intent_token(obj(json!({"expected": {"action": "delete_note"}})))
        .await?;
    assert_eq!(missing_token.code.as_deref(), Some("TOKEN_INVALID"));
    assert_eq!(missing_token.reason.as_deref(), Some("Invalid token type"));
    Ok(())
}
