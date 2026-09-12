// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `test_error_mapping.py` and `test_error_subclasses.py`: the
//! transport maps status codes and heartbeat error codes to typed variants.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::time::Duration;

use a2x_registry_client::{ClientError, HttpMethod, Transport};
use common::{MockResponse, MockServer};
use serde_json::json;

fn transport(server: &MockServer) -> TestResult<Transport> {
    Ok(Transport::new(&server.base_url, Duration::from_secs(5), None)?)
}

async fn get(
    server: &MockServer,
    method: HttpMethod,
    path: &str,
) -> TestResult<Result<a2x_registry_client::Response, ClientError>> {
    Ok(transport(server)?.request(method, path, None, None).await)
}

#[tokio::test]
async fn status_401_maps_to_authentication_error() -> TestResult {
    let server = MockServer::json(401, json!({"detail": "Missing token"}))?;
    let err = get(&server, HttpMethod::Get, "/anything").await?.err_or_fail()?;
    assert!(
        matches!(err, ClientError::Authentication { status: 401, .. }),
        "{err:?}"
    );
    assert_eq!(err.status_code(), Some(401));
    assert!(err.is_http());
    assert_eq!(err.to_string(), "HTTP 401: Missing token");
    Ok(())
}

#[tokio::test]
async fn status_403_maps_to_authorization_error() -> TestResult {
    let server = MockServer::json(403, json!({"detail": "Forbidden"}))?;
    let err = get(&server, HttpMethod::Get, "/anything").await?.err_or_fail()?;
    assert!(
        matches!(err, ClientError::Authorization { status: 403, .. }),
        "{err:?}"
    );
    assert!(err.is_http());
    Ok(())
}

#[tokio::test]
async fn status_404_unchanged() -> TestResult {
    let server = MockServer::json(404, json!({"detail": "Missing"}))?;
    let err = get(&server, HttpMethod::Get, "/x").await?.err_or_fail()?;
    assert!(err.is_not_found());
    Ok(())
}

#[tokio::test]
async fn status_400_unchanged() -> TestResult {
    let server = MockServer::json(400, json!({"detail": "Bad shape"}))?;
    let err = get(&server, HttpMethod::Post, "/x").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::Validation { .. }));
    Ok(())
}

#[tokio::test]
async fn status_5xx_maps_to_server_error_and_other_4xx_to_http() -> TestResult {
    let server = MockServer::json(503, json!({"detail": "down"}))?;
    let err = get(&server, HttpMethod::Get, "/x").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::Server { status: 503, .. }));
    let server = MockServer::json(409, json!({"detail": "busy"}))?;
    let err = get(&server, HttpMethod::Get, "/x").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::Http { status: 409, .. }));
    Ok(())
}

#[tokio::test]
async fn status_2xx_returns_response() -> TestResult {
    let server = MockServer::json(200, json!({"ok": true}))?;
    let resp = get(&server, HttpMethod::Get, "/x").await??;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()?, json!({"ok": true}));
    assert_eq!(server.last()?.path, "/x");
    Ok(())
}

#[tokio::test]
async fn non_json_error_body_uses_reason_phrase() -> TestResult {
    let server = MockServer::start(|_| Ok(MockResponse::raw(404, "text/plain", b"nope")))?;
    let err = get(&server, HttpMethod::Get, "/x").await?.err_or_fail()?;
    assert_eq!(err.to_string(), "HTTP 404: Not Found");
    assert!(err.payload().is_none());
    Ok(())
}

#[test]
fn auth_error_classes_distinct() -> TestResult {
    let a = ClientError::Authentication {
        status: 401,
        message: String::new(),
        payload: None,
    };
    let b = ClientError::Authorization {
        status: 403,
        message: String::new(),
        payload: None,
    };
    assert!(!matches!(a, ClientError::Authorization { .. }));
    assert!(!matches!(b, ClientError::Authentication { .. }));
    Ok(())
}

// ── 400 dispatch by heartbeat code (test_error_subclasses.py) ─────────────

#[tokio::test]
async fn status_400_ttl_required_dispatches_to_subclass() -> TestResult {
    let body = json!({"detail": {
        "code": "ttl_required", "detail": "Namespace 'x' requires lease_ttl", "min_ttl": 10, "max_ttl": 60,
    }});
    let server = MockServer::json(400, body)?;
    let err = get(&server, HttpMethod::Post, "/whatever").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::TtlRequired { .. }), "{err:?}");
    assert_eq!(err.min_ttl(), Some(10));
    assert_eq!(err.max_ttl(), Some(60));
    assert!(err.is_validation());
    assert_eq!(err.to_string(), "HTTP 400: Namespace 'x' requires lease_ttl");
    // payload is the inner detail object
    assert_eq!(err.payload().required()?["code"], "ttl_required");
    Ok(())
}

#[tokio::test]
async fn status_400_ttl_out_of_range_dispatches_to_subclass() -> TestResult {
    let body = json!({"detail": {"code": "ttl_out_of_range", "detail": "...", "min_ttl": 5, "max_ttl": 60}});
    let server = MockServer::json(400, body)?;
    let err = get(&server, HttpMethod::Post, "/whatever").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::TtlOutOfRange { .. }), "{err:?}");
    assert_eq!(err.min_ttl(), Some(5));
    assert_eq!(err.max_ttl(), Some(60));
    Ok(())
}

#[tokio::test]
async fn status_400_not_supported_dispatches_to_subclass() -> TestResult {
    let body = json!({"detail": {"code": "heartbeat_not_supported", "detail": "Namespace doesn't enable heartbeat"}});
    let server = MockServer::json(400, body)?;
    let err = get(&server, HttpMethod::Post, "/whatever").await?.err_or_fail()?;
    assert!(
        matches!(err, ClientError::HeartbeatNotSupported { .. }),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn status_400_unknown_code_falls_through_to_validation_error() -> TestResult {
    let body = json!({"detail": {"code": "made_up_code", "detail": "..."}});
    let server = MockServer::json(400, body)?;
    let err = get(&server, HttpMethod::Post, "/whatever").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::Validation { .. }), "{err:?}");
    Ok(())
}

#[tokio::test]
async fn status_400_legacy_string_detail_still_works() -> TestResult {
    let server = MockServer::json(400, json!({"detail": "boring 400 message"}))?;
    let err = get(&server, HttpMethod::Post, "/whatever").await?.err_or_fail()?;
    assert!(matches!(err, ClientError::Validation { .. }), "{err:?}");
    Ok(())
}

#[tokio::test]
async fn status_422_user_config_maps_to_immutable() -> TestResult {
    let server = MockServer::json(
        422,
        json!({"detail": "service is from user_config; edit the file"}),
    )?;
    let err = get(&server, HttpMethod::Put, "/whatever").await?.err_or_fail()?;
    assert!(
        matches!(err, ClientError::UserConfigServiceImmutable { status: 422, .. }),
        "{err:?}"
    );
    assert!(err.is_validation());
    Ok(())
}

#[tokio::test]
async fn timeout_maps_to_timeout_error() -> TestResult {
    let server = MockServer::start(|_| {
        std::thread::sleep(Duration::from_millis(300));
        Ok(MockResponse::json(200, json!({})))
    })?;
    let t = Transport::new(&server.base_url, Duration::from_millis(50), None)?;
    let err = t
        .request(HttpMethod::Get, "/slow", None, None)
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::Timeout { .. }), "{err:?}");
    assert!(err.is_connection());
    Ok(())
}
