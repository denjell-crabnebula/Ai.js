// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/integration/test_http.py`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;

use a4p::http_server::{A4PHTTPServer, a4p_http_port_in};
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::ed25519_public_jwk;
use a4p::{A4PClient, JsonDict};
use ap_support::env::MapEnv;
use common::{default_explicit_server, explicit_server, obj, sign_user_mandate};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn raw_http_request(port: u16, request: &[u8]) -> TestResult<Vec<u8>> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(request).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

#[tokio::test]
async fn dispatch_returns_404_for_unknown_path() -> TestResult {
    let server = A4PHTTPServer::new(Arc::new(default_explicit_server()?), Some("127.0.0.1"), Some(0))?;
    let (status, payload) = server.dispatch("/missing", JsonDict::new()).await;
    assert_eq!(status, 404);
    assert_eq!(Value::Object(payload), json!({"error": "not_found"}));
    Ok(())
}

#[test]
fn a4p_http_port_rejects_invalid_value() -> TestResult {
    let port = |value: &str| a4p_http_port_in(&MapEnv::from([("A4P_SERVER_PORT", value)]));
    let error = port("not-a-port").err_or_fail()?;
    assert!(
        error.to_string().contains("A4P_SERVER_PORT must be an integer"),
        "{error}"
    );
    let error = port("70000").err_or_fail()?;
    assert!(error.to_string().contains("between 1 and 65535"), "{error}");
    assert_eq!(port("8000")?, 8000);
    assert_eq!(a4p_http_port_in(&MapEnv::new())?, 8961);
    Ok(())
}

#[tokio::test]
async fn http_prepare_complete_operation_endpoint() -> TestResult {
    let server = A4PHTTPServer::new(
        Arc::new(explicit_server("local://test")?),
        Some("127.0.0.1"),
        Some(0),
    )?;
    let (prepare_status, prepared) = server
        .dispatch(
            "/a4p/v1/operation-authorizations/prepare",
            obj(json!({
                "agentId": "agent-1",
                "userId": "user-1",
                "operation": {"action": "delete_note", "params": {"note_id": "note-1"}},
                "validitySeconds": 60,
            })),
        )
        .await;
    let signed = sign_user_mandate(prepared["mandate"].as_object().required()?)?;
    let (complete_status, completed) = server
        .dispatch(
            "/a4p/v1/operation-authorizations/complete",
            obj(json!({
                "signedMandate": signed,
                "operation": {"action": "delete_note", "params": {"note_id": "note-1"}},
            })),
        )
        .await;
    assert_eq!(prepare_status, 200);
    assert_eq!(complete_status, 200);
    assert_eq!(completed["approved"], true);
    assert_eq!(completed["operationId"], signed["operationId"]);
    assert!(!completed.contains_key("signedMandate"));
    assert!(!completed.contains_key("rejectReason"));
    assert_eq!(completed["verificationResult"], json!({"valid": true}));
    Ok(())
}

#[tokio::test]
async fn http_verify_intent_token_consumes_execution_policy_quota() -> TestResult {
    let server = A4PHTTPServer::new(
        Arc::new(explicit_server("local://test")?),
        Some("127.0.0.1"),
        Some(0),
    )?;
    let (prepare_status, prepared) = server
        .dispatch(
            "/a4p/v1/intent-authorizations/prepare",
            obj(json!({
                "agentId": "agent-1",
                "userId": "user-1",
                "intent": {
                    "actions": [{"name": "delete_note", "params": {"note_id": "*"}}],
                    "executionPolicy": {"maxExecutions": 1},
                },
                "validitySeconds": 60,
            })),
        )
        .await;
    let signed = sign_user_mandate(prepared["mandate"].as_object().required()?)?;
    let (complete_status, completed) = server
        .dispatch(
            "/a4p/v1/intent-authorizations/complete",
            obj(json!({"signedMandate": signed})),
        )
        .await;
    let (first_status, first) = server
        .dispatch(
            "/a4p/v1/intent-tokens/verify",
            obj(json!({
                "token": completed["intentToken"],
                "expected": {"action": "delete_note", "params": {"note_id": "note-1"}},
            })),
        )
        .await;
    let (second_status, second) = server
        .dispatch(
            "/a4p/v1/intent-tokens/verify",
            obj(json!({
                "token": completed["intentToken"],
                "expected": {"action": "delete_note", "params": {"note_id": "note-2"}},
            })),
        )
        .await;
    assert_eq!(prepare_status, 200);
    assert_eq!(complete_status, 200);
    assert_eq!(first_status, 200);
    assert_eq!(second_status, 200);
    assert_eq!(first["valid"], true);
    assert_eq!(first["matchedScope"]["usage"]["executionsUsed"], 1);
    assert_eq!(second["valid"], false);
    assert_eq!(second["code"], "TOKEN_USAGE_EXCEEDED");
    Ok(())
}

#[tokio::test]
async fn a4p_client_uses_real_http_server_for_public_authorization_apis() -> TestResult {
    let http_server = A4PHTTPServer::new(
        Arc::new(explicit_server("local://http-client-test")?),
        Some("127.0.0.1"),
        Some(0),
    )?;
    assert_eq!(http_server.port(), 0);
    http_server.start().await?;
    assert_ne!(http_server.port(), 0);
    let client = A4PClient::with_options(
        Some(&format!("http://127.0.0.1:{}/", http_server.port())),
        Some(2.0),
    );

    let operation_prepared = client
        .prepare_operation_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "operation": {"action": "delete_note", "params": {"note_id": "note-1"}},
            "validitySeconds": 60,
        })))
        .await?;
    let operation_signed = sign_user_mandate(operation_prepared.mandate.as_ref().required()?)?;
    let operation_completed = client
        .complete_operation_authorization(obj(json!({
            "signedMandate": operation_signed,
            "operation": {"action": "delete_note", "params": {"note_id": "note-1"}},
        })))
        .await?;

    let intent_prepared = client
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]},
            "validitySeconds": 60,
        })))
        .await?;
    let intent_signed = sign_user_mandate(intent_prepared.mandate.as_ref().required()?)?;
    let intent_completed = client
        .complete_intent_authorization(obj(json!({"signedMandate": intent_signed})))
        .await?;
    let token_verified = client
        .verify_intent_token(obj(json!({
            "token": intent_completed.intent_token,
            "expected": {"action": "delete_note", "params": {"note_id": "note-2"}},
        })))
        .await?;
    let registered_public_key = ed25519_public_jwk(&generate_ed25519_private_key());
    let registration = client
        .register_ed25519_credential(&obj(
            json!({"userId": "user-2", "publicKey": registered_public_key}),
        ))
        .await?;

    assert!(operation_completed.approved);
    assert_eq!(
        operation_completed.operation_id.as_deref(),
        operation_signed["operationId"].as_str()
    );
    assert!(intent_completed.approved);
    assert!(token_verified.valid);
    assert_eq!(registration["created"], true);
    assert!(
        registration["credential"]["credentialId"]
            .as_str()
            .required()?
            .starts_with("cred_")
    );

    let error = client
        .register_ed25519_credential(&obj(json!({
            "userId": "user-3",
            "publicKey": {"kty": "OKP", "crv": "Ed25519", "x": "invalid"},
        })))
        .await
        .err_or_fail()?;
    assert!(error.to_string().starts_with("A4P HTTP 400"), "{error}");
    assert!(error.to_string().contains("bad_request"), "{error}");
    let error = client
        .register_ed25519_credential(&obj(
            json!({"userId": "user-3", "publicKey": registered_public_key}),
        ))
        .await
        .err_or_fail()?;
    assert!(error.to_string().starts_with("A4P HTTP 409"), "{error}");
    assert!(error.to_string().contains("CREDENTIAL_KEY_CONFLICT"), "{error}");
    let error = client
        .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
        .await
        .err_or_fail()?;
    assert!(error.to_string().starts_with("A4P HTTP 409"), "{error}");
    assert!(
        error.to_string().contains("SIGNATURE_METHOD_NOT_ENABLED"),
        "{error}"
    );

    http_server.start().await?;
    http_server.stop().await;
    http_server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_http_server_returns_protocol_error_statuses() -> TestResult {
    let http_server = A4PHTTPServer::new(
        Arc::new(explicit_server("local://http-status-test")?),
        Some("127.0.0.1"),
        Some(0),
    )?;
    http_server.start().await?;
    let port = http_server.port();

    let get_response = raw_http_request(
        port,
        b"GET /missing HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    let invalid_json_response = raw_http_request(
        port,
        b"POST /a4p/v1/intent-authorizations/prepare HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 1\r\n\r\n{",
    )
    .await?;
    let missing_response = raw_http_request(
        port,
        b"POST /missing HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}",
    )
    .await?;
    let empty_body_response = raw_http_request(
        port,
        b"POST /a4p/v1/intent-tokens/verify HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
    )
    .await?;

    assert!(get_response.starts_with(b"HTTP/1.1 405 Method Not Allowed"));
    assert!(String::from_utf8_lossy(&get_response).contains("method_not_allowed"));
    assert!(invalid_json_response.starts_with(b"HTTP/1.1 400 Bad Request"));
    assert!(missing_response.starts_with(b"HTTP/1.1 404 Not Found"));
    assert!(empty_body_response.starts_with(b"HTTP/1.1 200 OK"));
    let body = String::from_utf8_lossy(&empty_body_response);
    assert!(body.contains("\"valid\":false"), "{body}");
    assert!(body.contains("application/json; charset=utf-8"), "{body}");
    http_server.stop().await;
    Ok(())
}
