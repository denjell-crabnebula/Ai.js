// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_client.py`.
//!
//! The Python tests monkeypatch `urllib`; here a tiny axum server plays the
//! misbehaving A4P endpoint.

pub mod common;

use a4p::client::{A4PClient, default_a4p_base_url_in, default_a4p_timeout_in};
use ap_support::env::MapEnv;
use ap_support::testing::{OptionExt, ResultExt, TestResult};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;
use common::obj;
use serde_json::json;

async fn serve(status: u16, body: &'static str) -> TestResult<(String, tokio::task::JoinHandle<()>)> {
    let router = Router::new().fallback(any(move || async move {
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [("Content-Type", "application/json")],
            body,
        )
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://127.0.0.1:{port}"), task))
}

#[test]
fn client_defaults_are_normalized() -> TestResult {
    let env = MapEnv::from([
        ("A4P_SERVER_BASE_URL", "http://a4p.example/"),
        ("A4P_HTTP_TIMEOUT_S", "0.5"),
    ]);
    assert_eq!(default_a4p_base_url_in(&env), "http://a4p.example");
    assert_eq!(default_a4p_timeout_in(&env), 1.0);
    let client = A4PClient::with_env(&env);
    assert_eq!(client.base_url(), "http://a4p.example");
    assert_eq!(client.timeout(), 1.0);

    let env = env.with("A4P_HTTP_TIMEOUT_S", "invalid");
    assert_eq!(default_a4p_timeout_in(&env), 300.0);

    let env = MapEnv::new();
    assert_eq!(default_a4p_base_url_in(&env), "http://127.0.0.1:8961");
    assert_eq!(default_a4p_timeout_in(&env), 300.0);
    assert_eq!(
        A4PClient::with_base_url("http://x.example///").base_url(),
        "http://x.example"
    );
    Ok(())
}

#[tokio::test]
async fn client_maps_http_errors_with_json_body() -> TestResult {
    let (base_url, task) = serve(422, r#"{"error":"invalid_request"}"#).await?;
    let client = A4PClient::with_options(Some(&base_url), Some(2.0));
    let error = client
        .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
        .await
        .err_or_fail()?;
    let text = error.to_string();
    assert!(text.starts_with("A4P HTTP 422: "), "{text}");
    assert!(text.contains("invalid_request"), "{text}");
    task.abort();
    Ok(())
}

#[tokio::test]
async fn client_maps_http_errors_with_text_body() -> TestResult {
    let (base_url, task) = serve(422, "not-json").await?;
    let client = A4PClient::with_options(Some(&base_url), Some(2.0));
    let error = client
        .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
        .await
        .err_or_fail()?;
    let text = error.to_string();
    assert!(text.starts_with("A4P HTTP 422: "), "{text}");
    assert!(text.contains("not-json"), "{text}");
    task.abort();
    Ok(())
}

#[tokio::test]
async fn client_maps_transport_errors() -> TestResult {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let client = A4PClient::with_options(Some(&format!("http://127.0.0.1:{port}")), Some(2.0));
    let error = client
        .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
        .await
        .err_or_fail()?;
    assert!(
        error.to_string().starts_with("A4P HTTP request failed:"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn client_normalizes_empty_and_non_object_responses() -> TestResult {
    for body in ["", "[]"] {
        let (base_url, task) = serve(200, body).await?;
        let client = A4PClient::with_options(Some(&base_url), Some(2.0));
        let result = client
            .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
            .await?;
        assert!(result.is_empty(), "body {body:?}");
        task.abort();
    }
    Ok(())
}

#[tokio::test]
async fn client_maps_domain_rejections_from_json_bodies() -> TestResult {
    let (base_url, task) = serve(
        200,
        r#"{"approved": false, "rejectReason": "nope", "verificationResult": {"valid": false, "code": "X", "reason": "nope"}}"#,
    )
    .await?;
    let client = A4PClient::with_options(Some(&base_url), Some(2.0));
    let completed = client
        .complete_operation_authorization(obj(json!({"signedMandate": {}, "operation": {}})))
        .await?;
    assert!(!completed.approved);
    assert_eq!(completed.operation_id, None);
    assert_eq!(completed.reject_reason.as_deref(), Some("nope"));
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("X")
    );
    let intent = client.prepare_intent_authorization(obj(json!({}))).await?;
    assert!(intent.mandate.is_none());
    assert!(intent.signing_options.is_empty());
    task.abort();
    Ok(())
}
