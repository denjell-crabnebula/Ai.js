// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Behaviour of the agent methods: request shapes, ownership rules, 404
//! cleanup, list flattening and pagination, and the heartbeat wiring.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use a2x_registry_client::{
    A2xRegistryClient, ClientConfig, ClientError, ListOptions, OwnershipFile, RegisterOptions,
    ShutdownOptions,
};
use common::{MockResponse, MockServer, Recorded};
use serde_json::{Map, Value, json};

fn client_for(server: &MockServer) -> TestResult<A2xRegistryClient> {
    Ok(A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Disabled),
    )?)
}

fn card(name: &str) -> TestResult<Map<String, Value>> {
    Ok(
        json!({"name": name, "description": "desc", "endpoint": "http://ep"})
            .as_object()
            .cloned()
            .required()?,
    )
}

/// Responder that mimics the backend routes used by the agent flows.
fn registry_responder(req: &Recorded) -> TestResult<MockResponse> {
    Ok(match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/datasets/ds/services/a2a") => {
            let body = req.body.clone().required()?;
            let sid = body
                .get("service_id")
                .and_then(Value::as_str)
                .unwrap_or("sid_auto")
                .to_string();
            let mut reply = json!({"service_id": sid, "dataset": "ds", "status": "registered"});
            if let Some(ttl) = body.get("lease_ttl") {
                reply["lease_ttl"] = ttl.clone();
                reply["lease_expires_at"] = json!(1_700_000_000.5);
            }
            MockResponse::json(200, reply)
        }
        ("PUT", p) if p.starts_with("/api/datasets/ds/services/") => {
            let sid = p.rsplit('/').next().required()?;
            let fields: Vec<String> = req
                .body
                .as_ref()
                .required()?
                .as_object()
                .required()?
                .keys()
                .cloned()
                .collect();
            MockResponse::json(
                200,
                json!({"service_id": sid, "dataset": "ds", "status": "updated", "changed_fields": fields, "taxonomy_affected": false}),
            )
        }
        ("DELETE", p) if p.ends_with("/heartbeat") => MockResponse::json(200, json!({"revoked": true})),
        ("POST", p) if p.ends_with("/heartbeat") => MockResponse::json(
            200,
            json!({"service_id": "x", "expires_at": 1.0, "state": "healthy"}),
        ),
        ("DELETE", p) if p.ends_with("/lease") => {
            MockResponse::json(200, json!({"released": true, "prev_holder_id": "holder_1"}))
        }
        ("DELETE", p) if p.starts_with("/api/datasets/ds/services/") => {
            let sid = p.rsplit('/').next().required()?;
            MockResponse::json(200, json!({"service_id": sid, "status": "deregistered"}))
        }
        ("GET", p) if p.starts_with("/api/datasets/ds/services/") => {
            let sid = p.rsplit('/').next().required()?;
            MockResponse::json(
                200,
                json!({"id": sid, "type": "a2a", "name": "n", "description": "d.", "metadata": {"name": "n", "description": "d", "endpoint": "http://from-server"}}),
            )
        }
        _ => MockResponse::json(
            404,
            json!({"detail": format!("no route {} {}", req.method, req.path)}),
        ),
    })
}

#[tokio::test]
async fn register_agent_body_and_ownership() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        service_id: Some("sid1".into()),
        ..Default::default()
    };
    let resp = client.register_agent("ds", &card("a")?, &opts).await?;
    assert_eq!(resp.service_id, "sid1");
    assert_eq!(resp.status, "registered");
    assert_eq!(resp.lease_ttl, None);

    let req = server.last()?;
    assert_eq!(req.path, "/api/datasets/ds/services/a2a");
    let body = req.body.required()?;
    assert_eq!(body["agent_card"]["name"], "a");
    assert_eq!(body["persistent"], true);
    assert_eq!(body["service_id"], "sid1");
    assert!(body.get("lease_ttl").is_none());
    assert!(
        !req.headers.contains_key("authorization"),
        "no api key: no Authorization header"
    );
    assert!(client.ownership().contains("ds", "sid1"));
    Ok(())
}

#[tokio::test]
async fn register_non_persistent_does_not_record_ownership() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        persistent: false,
        ..Default::default()
    };
    let resp = client.register_agent("ds", &card("a")?, &opts).await?;
    assert_eq!(server.last()?.body.required()?["persistent"], false);
    assert!(!client.ownership().contains("ds", &resp.service_id));
    let err = client
        .update_agent("ds", &resp.service_id, &Map::new())
        .await
        .err_or_fail()?;
    assert!(err.is_not_owned());
    Ok(())
}

#[tokio::test]
async fn register_with_lease_ttl_sends_it_and_parses_lease_fields() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        lease_ttl: Some(60),
        ..Default::default()
    };
    let resp = client.register_agent("ds", &card("a")?, &opts).await?;
    assert_eq!(server.last()?.body.required()?["lease_ttl"], 60);
    assert_eq!(resp.lease_ttl, Some(60));
    assert_eq!(resp.lease_expires_at, Some(1_700_000_000.5));
    assert!(
        client.heartbeat_renewers().is_empty(),
        "auto_renew=false spawns nothing"
    );
    Ok(())
}

#[tokio::test]
async fn register_with_auto_renew_spawns_renewer_that_posts_heartbeats() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Disabled)
            .heartbeat_period(Duration::from_millis(20)),
    )?;
    let opts = RegisterOptions {
        lease_ttl: Some(60),
        auto_renew: true,
        ..Default::default()
    };
    let resp = client.register_agent("ds", &card("a")?, &opts).await?;
    assert!(client.heartbeat_renewers().contains("ds", &resp.service_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let heartbeat_path = format!("/api/datasets/ds/services/{}/heartbeat", resp.service_id);
    loop {
        let beats = server
            .requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path == heartbeat_path)
            .count();
        if beats >= 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "renewer never heartbeated");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let beat = server
        .requests()
        .into_iter()
        .find(|r| r.path == heartbeat_path)
        .required()?;
    assert_eq!(beat.body.required()?, json!({}));

    client.close().await;
    assert!(client.heartbeat_renewers().is_empty());
    let after_close = server.request_count();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        server.request_count(),
        after_close,
        "renewer kept running after close"
    );
    Ok(())
}

#[tokio::test]
async fn register_with_auto_renew_but_no_lease_spawns_nothing() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        auto_renew: true,
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    assert!(client.heartbeat_renewers().is_empty());
    Ok(())
}

#[tokio::test]
async fn heartbeat_requires_ownership_and_sends_status() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let err = client.heartbeat("ds", "ghost", None).await.err_or_fail()?;
    assert!(err.is_not_owned());
    assert_eq!(server.request_count(), 0, "ownership failure must not send HTTP");

    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    let info = client.heartbeat("ds", "s", Some("busy")).await?;
    assert_eq!(info["state"], "healthy");
    let req = server.last()?;
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/datasets/ds/services/s/heartbeat");
    assert_eq!(req.body.required()?, json!({"status": "busy"}));
    Ok(())
}

#[tokio::test]
async fn update_and_set_status_enforce_ownership_and_clear_on_404() -> TestResult {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    let server = MockServer::start(move |req| {
        if req.method == "PUT" {
            h.fetch_add(1, Ordering::SeqCst);
            if h.load(Ordering::SeqCst) >= 3 {
                return Ok(MockResponse::json(404, json!({"detail": "gone"})));
            }
        }
        registry_responder(req)
    })?;
    let client = client_for(&server)?;
    assert!(
        client
            .update_agent("ds", "s", &Map::new())
            .await
            .err_or_fail()?
            .is_not_owned()
    );

    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    let fields = json!({"region": "cn-east-2"}).as_object().cloned().required()?;
    let patch = client.update_agent("ds", "s", &fields).await?;
    assert_eq!(patch.changed_fields, vec!["region".to_string()]);
    let req = server.last()?;
    assert_eq!(req.method, "PUT");
    assert_eq!(req.path, "/api/datasets/ds/services/s");
    assert_eq!(req.body.required()?, json!({"region": "cn-east-2"}));

    // set_status validates the enum before the ownership check and before HTTP
    let before = server.request_count();
    let err = client.set_status("ds", "nobody", "weird").await.err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }), "{err:?}");
    assert_eq!(server.request_count(), before);
    let patch = client.set_status("ds", "s", "busy").await?;
    assert_eq!(patch.changed_fields, vec!["status".to_string()]);
    assert_eq!(server.last()?.body.required()?, json!({"status": "busy"}));

    // third PUT returns 404: ownership is cleared, error surfaces
    let err = client.set_status("ds", "s", "online").await.err_or_fail()?;
    assert!(err.is_not_found());
    assert!(!client.ownership().contains("ds", "s"));
    assert!(
        client
            .update_agent("ds", "s", &Map::new())
            .await
            .err_or_fail()?
            .is_not_owned()
    );
    Ok(())
}

#[tokio::test]
async fn drain_sets_offline() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    client.drain("ds", "s").await?;
    assert_eq!(server.last()?.body.required()?, json!({"status": "offline"}));
    Ok(())
}

#[tokio::test]
async fn deregister_enforces_ownership_and_clears_local_state() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    assert!(
        client
            .deregister_agent("ds", "s")
            .await
            .err_or_fail()?
            .is_not_owned()
    );
    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    let resp = client.deregister_agent("ds", "s").await?;
    assert_eq!(resp.status, "deregistered");
    let req = server.last()?;
    assert_eq!(req.method, "DELETE");
    assert_eq!(req.path, "/api/datasets/ds/services/s");
    assert!(!client.ownership().contains("ds", "s"));
    Ok(())
}

#[tokio::test]
async fn deregister_404_clears_ownership_then_fails() -> TestResult {
    let server = MockServer::start(|req| {
        if req.method == "DELETE" {
            Ok(MockResponse::json(404, json!({"detail": "missing"})))
        } else {
            registry_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    let err = client.deregister_agent("ds", "s").await.err_or_fail()?;
    assert!(err.is_not_found());
    assert!(!client.ownership().contains("ds", "s"));
    Ok(())
}

#[tokio::test]
async fn list_agents_filters_pagination_and_flattening() -> TestResult {
    let server = MockServer::start(|req| {
        assert_eq!(req.path, "/api/datasets/ds/services");
        Ok(MockResponse::json(
            200,
            json!([
                {"id": "a", "type": "a2a", "name": "n", "description": "raw.", "metadata": {"description": "raw", "endpoint": "http://a", "status": "online"}},
                {"id": "g", "type": "generic", "name": "gn", "description": "gd", "metadata": {"url": "http://g"}}
            ]),
        ))
    })?;
    let client = client_for(&server)?;

    let all = client.list_agents("ds", &ListOptions::new()).await?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["description"], "raw", "metadata wins over wrapper");
    assert_eq!(all[0]["endpoint"], "http://a");
    assert!(all[0].get("metadata").is_none());
    assert_eq!(all[1]["name"], "gn");
    assert!(server.last()?.query.is_empty());

    let opts = ListOptions::new()
        .filter("status", "online")
        .filter("region", "cn")
        .page(3)
        .size(50);
    client.list_agents("ds", &opts).await?;
    let req = server.last()?;
    assert_eq!(req.query_get("status"), Some("online"));
    assert_eq!(req.query_get("region"), Some("cn"));
    assert_eq!(req.query_get("page"), Some("3"));
    assert_eq!(req.query_get("size"), Some("50"));

    let before = server.request_count();
    let err = client
        .list_agents("ds", &ListOptions::new().filter("page", "1"))
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    let err = client
        .list_agents("ds", &ListOptions::new().page(0))
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    let err = client
        .list_agents("ds", &ListOptions::new().size(-2))
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    assert_eq!(
        server.request_count(),
        before,
        "local validation must not send HTTP"
    );
    Ok(())
}

#[tokio::test]
async fn list_agents_non_list_body_yields_empty() -> TestResult {
    let server = MockServer::json(200, json!({"unexpected": true}))?;
    let client = client_for(&server)?;
    assert!(client.list_agents("ds", &ListOptions::new()).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn list_idle_blank_agents_filters_and_caps() -> TestResult {
    let server = MockServer::json(
        200,
        json!([
            {"id": "a", "type": "a2a", "name": "x", "description": "__BLANK__.", "metadata": {"description": "__BLANK__", "endpoint": "http://a"}},
            {"id": "b", "type": "a2a", "name": "y", "description": "__BLANK__.", "metadata": {"description": "__BLANK__", "endpoint": "http://b"}}
        ]),
    )?;
    let client = client_for(&server)?;
    let one = client.list_idle_blank_agents("ds", 1).await?;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0]["id"], "a");
    let req = server.last()?;
    assert_eq!(req.query_get("description"), Some("__BLANK__"));
    assert_eq!(req.query_get("status"), Some("online"));
    let both = client.list_idle_blank_agents("ds", 5).await?;
    assert_eq!(both.len(), 2);
    let before = server.request_count();
    assert!(client.list_idle_blank_agents("ds", 0).await?.is_empty());
    assert_eq!(server.request_count(), before, "n=0 sends nothing");
    Ok(())
}

#[tokio::test]
async fn get_agent_parses_detail_and_rejects_zip() -> TestResult {
    let server = MockServer::start(|req| {
        if req.path.ends_with("/skill1") {
            Ok(MockResponse::raw(200, "application/zip", b"PK\x03\x04"))
        } else if req.path.ends_with("/list") {
            Ok(MockResponse::json(200, json!([1, 2])))
        } else {
            registry_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    let detail = client.get_agent("ds", "abc").await?;
    assert_eq!(detail.id, "abc");
    assert_eq!(detail.r#type, "a2a");
    assert_eq!(detail.description, "d.");
    assert_eq!(detail.metadata["endpoint"], "http://from-server");
    assert_eq!(detail.raw["id"], "abc");
    assert_eq!(server.last()?.path, "/api/datasets/ds/services/abc");

    let err = client.get_agent("ds", "skill1").await.err_or_fail()?;
    assert!(
        matches!(err, ClientError::UnexpectedServiceType { status: 200, .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("application/zip"));
    let err = client.get_agent("ds", "list").await.err_or_fail()?;
    assert!(
        matches!(err, ClientError::UnexpectedServiceType { .. }),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn path_segments_are_percent_encoded() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    let err = client.get_agent("my ds", "id/with slash").await.err_or_fail()?;
    assert!(err.is_not_found());
    // axum keeps the raw request path, so the encoding is visible: one segment per id.
    let req = server.last()?;
    assert_eq!(req.path, "/api/datasets/my%20ds/services/id%2Fwith%20slash");
    Ok(())
}

#[tokio::test]
async fn delete_dataset_clears_ownership_on_success_and_on_400() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let server = MockServer::start(move |req| {
        if req.method == "DELETE" && req.path == "/api/datasets/ds" {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(MockResponse::json(
                    200,
                    json!({"dataset": "ds", "status": "deleted"}),
                ))
            } else {
                Ok(MockResponse::json(
                    400,
                    json!({"detail": "Dataset 'ds' not found"}),
                ))
            }
        } else {
            registry_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    let resp = client.delete_dataset("ds").await?;
    assert_eq!(resp.status, "deleted");
    assert!(!client.ownership().contains("ds", "s"));

    client.register_agent("ds", &card("a")?, &opts).await?;
    let err = client.delete_dataset("ds").await.err_or_fail()?;
    assert!(matches!(err, ClientError::Validation { .. }));
    assert!(
        !client.ownership().contains("ds", "s"),
        "400 also clears local ownership"
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_revokes_leases_or_deregisters() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = client_for(&server)?;
    for sid in ["s1", "s2"] {
        let opts = RegisterOptions {
            service_id: Some(sid.into()),
            ..Default::default()
        };
        client.register_agent("ds", &card(sid)?, &opts).await?;
    }
    let report = client.shutdown(&ShutdownOptions::default()).await?;
    assert_eq!(report.revoked.len(), 2);
    assert!(report.errors.is_empty());
    let revokes: Vec<Recorded> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/heartbeat"))
        .collect();
    assert_eq!(revokes.len(), 2);
    assert_eq!(revokes[0].method, "DELETE");
    assert_eq!(revokes[0].body.clone().required()?, json!({"permanent": false}));
    assert!(
        client.ownership().contains("ds", "s1"),
        "lease revoke keeps ownership"
    );

    let opts = ShutdownOptions {
        permanent: true,
        dataset: Some("ds".into()),
        ..Default::default()
    };
    let report = client.shutdown(&opts).await?;
    assert_eq!(report.revoked.len(), 2);
    assert!(!client.ownership().contains("ds", "s1"));
    assert!(!client.ownership().contains("ds", "s2"));
    Ok(())
}

#[tokio::test]
async fn shutdown_collects_errors_unless_raise_on_error() -> TestResult {
    let server = MockServer::start(|req| {
        if req.path.ends_with("/heartbeat") {
            Ok(MockResponse::json(500, json!({"detail": "boom"})))
        } else {
            registry_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    let opts = ShutdownOptions {
        sids: Some(vec![("ds".into(), "x".into())]),
        ..Default::default()
    };
    let report = client.shutdown(&opts).await?;
    assert!(report.revoked.is_empty());
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].1, "x");
    assert!(report.errors[0].2.contains("HTTP 500"));

    let opts = ShutdownOptions {
        raise_on_error: true,
        ..opts
    };
    let err = client.shutdown(&opts).await.err_or_fail()?;
    assert!(matches!(err, ClientError::Server { .. }));
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_renewers_first() -> TestResult {
    let server = MockServer::start(registry_responder)?;
    let client = A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Disabled)
            .heartbeat_period(Duration::from_millis(10)),
    )?;
    let opts = RegisterOptions {
        lease_ttl: Some(30),
        auto_renew: true,
        service_id: Some("s".into()),
        ..Default::default()
    };
    client.register_agent("ds", &card("a")?, &opts).await?;
    assert!(client.heartbeat_renewers().contains("ds", "s"));
    client.shutdown(&ShutdownOptions::default()).await?;
    assert!(client.heartbeat_renewers().is_empty());
    Ok(())
}
