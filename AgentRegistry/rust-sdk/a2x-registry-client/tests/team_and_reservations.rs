// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Team-agent helpers (`register_blank_agent`, `replace_agent_card`,
//! `restore_to_blank`) and the reservation primitives.

pub mod common;

use a2x_registry_client::{
    A2xRegistryClient, ClientConfig, ClientError, OwnershipFile, RegisterOptions, ReserveOptions,
};
use ap_support::testing::{OptionExt, ResultExt, TestResult};
use common::{MockResponse, MockServer, Recorded};
use serde_json::{Map, Value, json};

fn client_for(server: &MockServer) -> TestResult<A2xRegistryClient> {
    Ok(A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Disabled),
    )?)
}

fn obj(v: Value) -> Map<String, Value> {
    ap_support::json::object(v)
}

fn team_responder(req: &Recorded) -> TestResult<MockResponse> {
    Ok(match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/datasets/ds/services/a2a") => {
            let body = req.body.clone().required()?;
            let sid = body
                .get("service_id")
                .and_then(Value::as_str)
                .unwrap_or("blank_sid")
                .to_string();
            let status = if body.get("service_id").is_some() {
                "updated"
            } else {
                "registered"
            };
            MockResponse::json(200, json!({"service_id": sid, "dataset": "ds", "status": status}))
        }
        ("DELETE", p) if p.ends_with("/lease") => {
            MockResponse::json(200, json!({"released": true, "prev_holder_id": "holder_1"}))
        }
        ("GET", "/api/datasets/ds/services/with_ep") => MockResponse::json(
            200,
            json!({"id": "with_ep", "type": "a2a", "name": "n", "description": "d", "metadata": {"endpoint": "http://persisted"}}),
        ),
        ("GET", "/api/datasets/ds/services/no_ep") => MockResponse::json(
            200,
            json!({"id": "no_ep", "type": "a2a", "name": "n", "description": "d", "metadata": {"name": "n"}}),
        ),
        ("POST", "/api/datasets/ds/reservations") => MockResponse::json(
            200,
            json!({
                "holder_id": req.body.as_ref().and_then(|b| b.get("holder_id")).and_then(Value::as_str).unwrap_or("holder_gen"),
                "ttl_seconds": req.body.as_ref().and_then(|b| b.get("ttl_seconds")).cloned().unwrap_or(json!(30)),
                "expires_at_unix": 1729999999.5,
                "reservations": [
                    {"id": "agent_1", "type": "a2a", "name": "x", "description": "__BLANK__.", "metadata": {"description": "__BLANK__", "endpoint": "http://a1"}}
                ]
            }),
        ),
        ("DELETE", "/api/datasets/ds/reservations/holder_gen") => {
            MockResponse::json(200, json!({"released": ["agent_1", "agent_2"]}))
        }
        ("DELETE", "/api/datasets/ds/reservations/holder_gen/agent_1") => {
            MockResponse::json(200, json!({"released": ["agent_1"]}))
        }
        ("DELETE", "/api/datasets/ds/reservations/holder_gen/agent_9") => {
            MockResponse::json(200, json!({"released": []}))
        }
        ("DELETE", "/api/datasets/ds/reservations/holder_gen/theirs") => {
            MockResponse::json(403, json!({"detail": "held by another holder"}))
        }
        ("POST", "/api/datasets/ds/reservations/holder_gen/extend") => {
            MockResponse::json(200, json!({"expires_at_unix": 1730000060.0}))
        }
        ("POST", "/api/datasets/ds/reservations/expired/extend") => {
            MockResponse::json(404, json!({"detail": "no live leases"}))
        }
        _ => MockResponse::json(
            404,
            json!({"detail": format!("no route {} {}", req.method, req.path)}),
        ),
    })
}

#[tokio::test]
async fn register_blank_agent_builds_template_and_seeds_cache() -> TestResult {
    let server = MockServer::start(team_responder)?;
    let client = client_for(&server)?;
    let resp = client
        .register_blank_agent("ds", "http://teammate_1:8080", None, true)
        .await?;
    assert_eq!(resp.service_id, "blank_sid");
    let body = server.last()?.body.required()?;
    assert_eq!(
        body["agent_card"],
        json!({"name": "_BlankAgent_http://teammate_1:8080", "description": "__BLANK__", "endpoint": "http://teammate_1:8080", "status": "online"})
    );
    assert_eq!(body["persistent"], true);
    assert!(client.ownership().contains("ds", "blank_sid"));

    // restore_to_blank hits the L1 cache: no GET, one POST plus the lease hook.
    let before = server.request_count();
    let resp = client.restore_to_blank("ds", "blank_sid").await?;
    assert_eq!(resp.status, "updated");
    let new: Vec<Recorded> = server.requests().into_iter().skip(before).collect();
    assert_eq!(new.len(), 2);
    assert_eq!(new[0].method, "POST");
    assert_eq!(new[0].path, "/api/datasets/ds/services/a2a");
    assert_eq!(new[0].body.clone().required()?["service_id"], "blank_sid");
    assert_eq!(
        new[0].body.clone().required()?["agent_card"]["description"],
        "__BLANK__"
    );
    assert_eq!(new[1].method, "DELETE");
    assert_eq!(new[1].path, "/api/datasets/ds/services/blank_sid/lease");

    let err = client
        .register_blank_agent("ds", "   ", None, true)
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    Ok(())
}

#[tokio::test]
async fn replace_agent_card_ownership_autofill_and_lease_hook() -> TestResult {
    let server = MockServer::start(team_responder)?;
    let client = client_for(&server)?;
    let team_card = obj(json!({"name": "Task Planner", "description": "plans", "status": "busy"}));

    // 1. ownership first, no HTTP
    let err = client
        .replace_agent_card("ds", "with_ep", &team_card, true)
        .await
        .err_or_fail()?;
    assert!(err.is_not_owned());
    assert_eq!(server.request_count(), 0);

    // 2. owned, endpoint missing, L1 empty: L2 GET fills it in
    let opts = RegisterOptions {
        service_id: Some("with_ep".into()),
        ..Default::default()
    };
    client
        .register_agent("ds", &obj(json!({"name": "n", "description": "d"})), &opts)
        .await?;
    let before = server.request_count();
    let resp = client
        .replace_agent_card("ds", "with_ep", &team_card, true)
        .await?;
    assert_eq!(resp.status, "updated");
    let new: Vec<Recorded> = server.requests().into_iter().skip(before).collect();
    assert_eq!(
        new.iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect::<Vec<_>>(),
        vec![
            "GET /api/datasets/ds/services/with_ep",
            "POST /api/datasets/ds/services/a2a",
            "DELETE /api/datasets/ds/services/with_ep/lease",
        ]
    );
    let posted = new[1].body.clone().required()?;
    assert_eq!(posted["service_id"], "with_ep");
    assert_eq!(posted["persistent"], true);
    assert_eq!(
        posted["agent_card"]["endpoint"], "http://persisted",
        "auto-filled from get_agent"
    );
    assert_eq!(posted["agent_card"]["status"], "busy");

    // 3. second replace uses the refreshed L1 cache: no GET
    let before = server.request_count();
    client
        .replace_agent_card("ds", "with_ep", &team_card, false)
        .await?;
    let new: Vec<Recorded> = server.requests().into_iter().skip(before).collect();
    assert_eq!(
        new.len(),
        1,
        "release_lease=false skips the hook; L1 cache skips the GET"
    );
    assert_eq!(
        new[0].body.clone().required()?["agent_card"]["endpoint"],
        "http://persisted"
    );

    // 4. explicit endpoint wins and no auto-fill happens
    let explicit = obj(json!({"name": "n", "description": "d", "endpoint": "http://explicit"}));
    client
        .replace_agent_card("ds", "with_ep", &explicit, false)
        .await?;
    assert_eq!(
        server.last()?.body.required()?["agent_card"]["endpoint"],
        "http://explicit"
    );
    Ok(())
}

#[tokio::test]
async fn replace_agent_card_l3_fails_locally_when_no_endpoint_anywhere() -> TestResult {
    let server = MockServer::start(team_responder)?;
    let client = client_for(&server)?;
    let opts = RegisterOptions {
        service_id: Some("no_ep".into()),
        ..Default::default()
    };
    client
        .register_agent("ds", &obj(json!({"name": "n", "description": "d"})), &opts)
        .await?;
    let err = client
        .replace_agent_card("ds", "no_ep", &obj(json!({"name": "n"})), true)
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }), "{err:?}");
    assert!(err.to_string().contains("No 'endpoint' available"));
    assert_eq!(server.last()?.method, "GET", "only the L2 lookup was sent");
    let err = client.restore_to_blank("ds", "no_ep").await.err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    Ok(())
}

#[tokio::test]
async fn replace_agent_card_lease_hook_failures_are_warnings_only() -> TestResult {
    for status in [404u16, 500, 503] {
        let server = MockServer::start(move |req| {
            if req.path.ends_with("/lease") {
                Ok(MockResponse::json(status, json!({"detail": "hook failed"})))
            } else {
                team_responder(req)
            }
        })?;
        let client = client_for(&server)?;
        client
            .register_blank_agent("ds", "http://ep", Some("sid"), true)
            .await?;
        let resp = client
            .replace_agent_card("ds", "sid", &obj(json!({"name": "n"})), true)
            .await?;
        assert_eq!(
            resp.status, "updated",
            "hook status {status} must not fail the replace"
        );
    }
    // A 403 from the hook is not in the tolerated set and propagates.
    let server = MockServer::start(|req| {
        if req.path.ends_with("/lease") {
            Ok(MockResponse::json(403, json!({"detail": "nope"})))
        } else {
            team_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    client
        .register_blank_agent("ds", "http://ep", Some("sid"), true)
        .await?;
    let err = client
        .replace_agent_card("ds", "sid", &obj(json!({"name": "n"})), true)
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::Authorization { .. }));
    Ok(())
}

#[tokio::test]
async fn replace_agent_card_404_clears_ownership_and_cache() -> TestResult {
    let server = MockServer::start(|req| {
        if req.method == "POST" && req.body.as_ref().is_some_and(|b| b.get("service_id").is_some()) {
            Ok(MockResponse::json(404, json!({"detail": "gone"})))
        } else {
            team_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    client.register_blank_agent("ds", "http://ep", None, true).await?;
    let err = client
        .replace_agent_card("ds", "blank_sid", &obj(json!({"name": "n"})), true)
        .await
        .err_or_fail()?;
    assert!(err.is_not_found());
    assert!(!client.ownership().contains("ds", "blank_sid"));
    assert!(
        client
            .restore_to_blank("ds", "blank_sid")
            .await
            .err_or_fail()?
            .is_not_owned()
    );
    Ok(())
}

#[tokio::test]
async fn release_my_lease_requires_ownership_and_returns_bool() -> TestResult {
    let server = MockServer::start(|req| {
        if req.path.ends_with("/none/lease") {
            Ok(MockResponse::json(
                200,
                json!({"released": false, "prev_holder_id": null}),
            ))
        } else {
            team_responder(req)
        }
    })?;
    let client = client_for(&server)?;
    assert!(
        client
            .release_my_lease("ds", "sid")
            .await
            .err_or_fail()?
            .is_not_owned()
    );
    client
        .register_blank_agent("ds", "http://ep", Some("sid"), true)
        .await?;
    assert!(client.release_my_lease("ds", "sid").await?);
    let req = server.last()?;
    assert_eq!(req.method, "DELETE");
    assert_eq!(req.path, "/api/datasets/ds/services/sid/lease");
    client
        .register_blank_agent("ds", "http://ep2", Some("none"), true)
        .await?;
    assert!(!client.release_my_lease("ds", "none").await?);
    Ok(())
}

#[tokio::test]
async fn reserve_release_extend_flow() -> TestResult {
    let server = MockServer::start(team_responder)?;
    let client = client_for(&server)?;

    let opts = ReserveOptions {
        extra_filters: obj(json!({"region": "cn-east-1", "status": "online"})),
        ..Default::default()
    };
    let mut r = client.reserve_blank_agents("ds", &opts).await?;
    assert_eq!(r.holder_id, "holder_gen");
    assert_eq!(r.dataset, "ds");
    assert_eq!(r.ttl_seconds, 30);
    assert_eq!(r.expires_at_unix, 1729999999.5);
    assert_eq!(r.agents.len(), 1);
    assert_eq!(r.agents[0]["id"], "agent_1");
    assert!(!r.is_released());
    let body = server.last()?.body.required()?;
    assert_eq!(
        body["filters"],
        json!({"description": "__BLANK__", "status": "online", "region": "cn-east-1"})
    );
    assert_eq!(body["n"], 1);
    assert_eq!(body["ttl_seconds"], 30);
    assert!(body.get("holder_id").is_none());

    let new_exp = client.extend_reservation(&mut r, 60).await?;
    assert_eq!(new_exp, 1730000060.0);
    assert_eq!(r.expires_at_unix, new_exp);
    assert_eq!(r.ttl_seconds, 60);
    let req = server.last()?;
    assert_eq!(req.path, "/api/datasets/ds/reservations/holder_gen/extend");
    assert_eq!(req.body.required()?, json!({"ttl_seconds": 60}));

    let released = client
        .release_reservation(&mut r, Some(&["agent_1".to_string(), "agent_9".to_string()]))
        .await?;
    assert_eq!(released, vec!["agent_1".to_string()]);
    assert!(r.is_released());
    let paths: Vec<String> = server
        .requests()
        .iter()
        .rev()
        .take(2)
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/api/datasets/ds/reservations/holder_gen/agent_9".to_string(),
            "/api/datasets/ds/reservations/holder_gen/agent_1".to_string(),
        ]
    );

    let released = client.release_reservation(&mut r, None).await?;
    assert_eq!(released, vec!["agent_1".to_string(), "agent_2".to_string()]);
    assert_eq!(server.last()?.path, "/api/datasets/ds/reservations/holder_gen");

    let err = client
        .release_reservation(&mut r, Some(&["theirs".to_string()]))
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::Authorization { .. }));
    Ok(())
}

#[tokio::test]
async fn reserve_validates_locally_and_sends_holder_id() -> TestResult {
    let server = MockServer::start(team_responder)?;
    let client = client_for(&server)?;
    let err = client
        .reserve_blank_agents(
            "ds",
            &ReserveOptions {
                ttl_seconds: 0,
                ..Default::default()
            },
        )
        .await
        .err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    assert_eq!(server.request_count(), 0);

    let opts = ReserveOptions {
        n: 3,
        ttl_seconds: 45,
        holder_id: Some("leader-7".into()),
        ..Default::default()
    };
    let r = client.reserve_blank_agents("ds", &opts).await?;
    assert_eq!(r.holder_id, "leader-7");
    let body = server.last()?.body.required()?;
    assert_eq!(body["holder_id"], "leader-7");
    assert_eq!(body["n"], 3);
    assert_eq!(body["ttl_seconds"], 45);

    let mut expired = r.clone();
    expired.holder_id = "expired".into();
    let err = client.extend_reservation(&mut expired, 30).await.err_or_fail()?;
    assert!(err.is_not_found());
    let err = client.extend_reservation(&mut expired, 0).await.err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    Ok(())
}
