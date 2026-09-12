// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/heartbeat/`: lease config, the four corner register
//! matrix, lifecycle, revoke, sweeper, unhealthy filtering, persistence,
//! inline lease config, auth integration and the README flow.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use axum::http::StatusCode;
use common::{TestApp, agent_card, bearer, lite_app};
use serde_json::{Value, json};

use a2x_common::LeaseState;
use a2x_registry::auth::AuthStore;
use a2x_registry::heartbeat::{HeartbeatSweeper, system_ctx};

async fn enabled_dataset(app: &TestApp) -> TestResult<String> {
    let name = format!("hbon_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    assert_eq!(
        app.post("/api/datasets", json!({"name": name})).await?.status,
        StatusCode::OK
    );
    let r = app
        .post(
            &format!("/api/datasets/{name}/lease-config"),
            json!({"enabled": true, "min_ttl": 5, "max_ttl": 60, "grace_period": 10}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    Ok(name)
}

async fn register_with_lease(app: &TestApp, ds: &str, ttl: i64, name: &str) -> TestResult<String> {
    let r = app
        .post(
            &format!("/api/datasets/{ds}/services/a2a"),
            json!({"agent_card": agent_card(name), "dataset": ds, "lease_ttl": ttl}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    Ok(r.json()["service_id"].as_str().required()?.to_string())
}

async fn list_ids(app: &TestApp, ds: &str, include_unhealthy: bool) -> TestResult<Vec<String>> {
    let path = if include_unhealthy {
        format!("/api/datasets/{ds}/services?include_unhealthy=true")
    } else {
        format!("/api/datasets/{ds}/services")
    };
    app.get(&path)
        .await?
        .json()
        .as_array()
        .required()?
        .iter()
        .map(|s| -> TestResult<_> { Ok(s["id"].as_str().required()?.to_string()) })
        .collect::<TestResult<Vec<_>>>()
}

fn mark_unhealthy(app: &TestApp, ds: &str, sid: &str) -> TestResult {
    let store = app.state.heartbeat_store().required()?;
    let lease = store.get_lease(ds, sid).required()?;
    store.sweep_tick(Some(lease.expires_at + 0.001));
    assert!(store.is_unhealthy(ds, sid));
    Ok(())
}

// ── lease config ─────────────────────────────────────────────────────────

#[tokio::test]
async fn lease_config_endpoints() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app.get(&format!("/api/datasets/{ds}/lease-config")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"dataset": ds, "enabled": false, "min_ttl": 10, "max_ttl": 3600, "grace_period": 300, "schema_version": 1})
    );
    let r = app
        .post(
            &format!("/api/datasets/{ds}/lease-config"),
            json!({"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["max_ttl"], 600);
    assert_eq!(
        app.get(&format!("/api/datasets/{ds}/lease-config")).await?.json()["enabled"],
        true
    );
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join(&ds).join("lease_config.json"),
    )?)?;
    assert_eq!(
        on_disk,
        json!({"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60, "schema_version": 1})
    );
    let r = app
        .post(
            &format!("/api/datasets/{ds}/lease-config"),
            json!({"enabled": false, "min_ttl": 5, "max_ttl": 60, "grace_period": 10}),
        )
        .await?;
    assert_eq!(r.json()["enabled"], false);
    for bad in [
        json!({"enabled": true, "min_ttl": 0, "max_ttl": 60, "grace_period": 10}),
        json!({"enabled": true, "min_ttl": 100, "max_ttl": 10, "grace_period": 10}),
    ] {
        let r = app.post(&format!("/api/datasets/{ds}/lease-config"), bad).await?;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        app.get("/api/datasets/no_such_ds/lease-config").await?.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.post("/api/datasets/no_such_ds/lease-config", json!({"enabled": true}))
            .await?
            .status,
        StatusCode::BAD_REQUEST
    );
    Ok(())
}

// ── register matrix ──────────────────────────────────────────────────────

#[tokio::test]
async fn register_matrix() -> TestResult {
    let app = lite_app().await?;
    let off = app.dataset().await?;
    let on = enabled_dataset(&app).await?;

    let r = app
        .post(
            &format!("/api/datasets/{off}/services/a2a"),
            json!({"agent_card": agent_card("perm1"), "dataset": off}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["lease_ttl"], Value::Null);
    assert_eq!(r.json()["lease_expires_at"], Value::Null);

    let r = app
        .post(
            &format!("/api/datasets/{off}/services/a2a"),
            json!({"agent_card": agent_card("rej1"), "lease_ttl": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["detail"]["code"], "heartbeat_not_supported");

    let r = app
        .post(
            &format!("/api/datasets/{on}/services/a2a"),
            json!({"agent_card": agent_card("rej2")}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let body = r.json();
    assert_eq!(body["detail"]["code"], "ttl_required");
    assert_eq!(body["detail"]["min_ttl"], 5);
    assert_eq!(body["detail"]["max_ttl"], 60);

    for ttl in [1, 9999] {
        let r = app
            .post(
                &format!("/api/datasets/{on}/services/a2a"),
                json!({"agent_card": agent_card("rej3"), "lease_ttl": ttl}),
            )
            .await?;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        assert_eq!(r.json()["detail"]["code"], "ttl_out_of_range");
        assert_eq!(r.json()["detail"]["min_ttl"], 5);
    }

    let r = app
        .post(
            &format!("/api/datasets/{on}/services/a2a"),
            json!({"agent_card": agent_card("ok1"), "lease_ttl": 20}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["lease_ttl"], 20);
    assert!(r.json()["lease_expires_at"].as_f64().required()? > 0.0);
    for ttl in [5, 60] {
        let r = app
            .post(
                &format!("/api/datasets/{on}/services/a2a"),
                json!({"agent_card": agent_card(&format!("b{ttl}")), "lease_ttl": ttl}),
            )
            .await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .post(
            &format!("/api/datasets/{on}/services/generic"),
            json!({"name": "g1", "description": "d", "lease_ttl": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["lease_ttl"], 30);
    Ok(())
}

// ── back compat ──────────────────────────────────────────────────────────

#[tokio::test]
async fn legacy_dataset_unchanged() -> TestResult {
    let app = lite_app().await?;
    let off = app.dataset().await?;
    let r = app
        .post(
            &format!("/api/datasets/{off}/services/a2a"),
            json!({"agent_card": agent_card("noleak")}),
        )
        .await?;
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    let data: Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join(&off).join("api_config.json"),
    )?)?;
    let target = data["services"]
        .as_array()
        .required()?
        .iter()
        .find(|s| s["service_id"] == sid)
        .required()?;
    assert!(target.get("lease_ttl").is_none());
    assert!(list_ids(&app, &off, false).await?.contains(&sid));
    assert_eq!(
        app.delete(&format!("/api/datasets/{off}/services/{sid}"))
            .await?
            .status,
        StatusCode::OK
    );
    Ok(())
}

// ── lifecycle ────────────────────────────────────────────────────────────

#[tokio::test]
async fn heartbeat_lifecycle() -> TestResult {
    let app = lite_app().await?;
    let on = enabled_dataset(&app).await?;
    let off = app.dataset().await?;
    let store = app.state.heartbeat_store().required()?;

    let sid = register_with_lease(&app, &on, 20, "svc").await?;
    let before = store.get_lease(&on, &sid).required()?.expires_at;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let r = app
        .post(&format!("/api/datasets/{on}/services/{sid}/heartbeat"), json!({}))
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(body["service_id"], sid);
    assert_eq!(body["dataset"], on);
    assert_eq!(body["state"], "healthy");
    assert_eq!(body["ttl_seconds"], 20);
    assert!(body["expires_at"].as_f64().required()? > 0.0);
    assert!(store.get_lease(&on, &sid).required()?.expires_at > before);
    // Empty body is accepted too.
    let r = app
        .request(
            axum::http::Method::POST,
            &format!("/api/datasets/{on}/services/{sid}/heartbeat"),
            &[],
            None,
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);

    let r = app
        .post(
            &format!("/api/datasets/{off}/services/a2a"),
            json!({"agent_card": agent_card("p1")}),
        )
        .await?;
    let perm = r.json()["service_id"].as_str().required()?.to_string();
    let r = app
        .post(
            &format!("/api/datasets/{off}/services/{perm}/heartbeat"),
            json!({}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Expire, recover within grace.
    let sid = register_with_lease(&app, &on, 5, "exp").await?;
    let lease = store.get_lease(&on, &sid).required()?;
    let (nu, td) = store.sweep_tick(Some(lease.expires_at + 0.001));
    assert!(nu.contains(&(on.clone(), sid.clone())));
    assert!(td.is_empty());
    assert!(!list_ids(&app, &on, false).await?.contains(&sid));
    assert!(list_ids(&app, &on, true).await?.contains(&sid));
    let r = app
        .post(&format!("/api/datasets/{on}/services/{sid}/heartbeat"), json!({}))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!store.is_unhealthy(&on, &sid));
    assert!(list_ids(&app, &on, false).await?.contains(&sid));

    // Past grace: hard delete through the registry.
    let sid = register_with_lease(&app, &on, 5, "gone").await?;
    let lease = store.get_lease(&on, &sid).required()?;
    let (_, td) = store.sweep_tick(Some(lease.grace_deadline + 0.001));
    assert!(td.contains(&(on.clone(), sid.clone())));
    app.state.registry.deregister(&on, &sid, Some(&system_ctx()))?;
    assert!(!list_ids(&app, &on, true).await?.contains(&sid));

    // Status piggyback.
    let sid = register_with_lease(&app, &on, 30, "piggy").await?;
    let r = app
        .post(
            &format!("/api/datasets/{on}/services/{sid}/heartbeat"),
            json!({"status": "busy"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let detail = app
        .get(&format!("/api/datasets/{on}/services/{sid}"))
        .await?
        .json();
    assert_eq!(detail["metadata"]["status"], "busy");
    Ok(())
}

// ── revoke ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn revoke_soft_and_permanent() -> TestResult {
    let app = lite_app().await?;
    let on = enabled_dataset(&app).await?;
    let store = app.state.heartbeat_store().required()?;
    let sid = register_with_lease(&app, &on, 30, "rev").await?;
    let r = app
        .delete_json(
            &format!("/api/datasets/{on}/services/{sid}/heartbeat"),
            json!({"permanent": false}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"service_id": sid, "dataset": on, "permanent": false})
    );
    assert!(store.is_unhealthy(&on, &sid));
    assert!(!list_ids(&app, &on, false).await?.contains(&sid));
    assert!(list_ids(&app, &on, true).await?.contains(&sid));
    let r = app
        .post(&format!("/api/datasets/{on}/services/{sid}/heartbeat"), json!({}))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(list_ids(&app, &on, false).await?.contains(&sid));

    let r = app
        .delete_json(
            &format!("/api/datasets/{on}/services/{sid}/heartbeat"),
            json!({"permanent": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["permanent"], true);
    assert!(!list_ids(&app, &on, true).await?.contains(&sid));
    assert!(store.get_lease(&on, &sid).is_none());
    let r = app
        .delete_json(
            &format!("/api/datasets/{on}/services/no_such_sid/heartbeat"),
            json!({"permanent": false}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .delete_json(
            &format!("/api/datasets/{on}/services/no_such_sid/heartbeat"),
            json!({"permanent": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    Ok(())
}

// ── sweeper ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn sweeper_state_progression_and_hard_delete() -> TestResult {
    let app = lite_app().await?;
    let on = enabled_dataset(&app).await?;
    let store = app.state.heartbeat_store().required()?;
    let (nu, td) = store.sweep_tick(None);
    assert!(nu.is_empty() && td.is_empty());

    let sid = register_with_lease(&app, &on, 60, "h").await?;
    let lease = store.get_lease(&on, &sid).required()?;
    let (nu, td) = store.sweep_tick(Some(lease.expires_at - 1.0));
    assert!(!nu.contains(&(on.clone(), sid.clone())) && td.is_empty());

    let sid = register_with_lease(&app, &on, 10, "p").await?;
    let key = (on.clone(), sid.clone());
    let lease = store.get_lease(&on, &sid).required()?;
    let (nu, td) = store.sweep_tick(Some(lease.expires_at + 0.01));
    assert!(nu.contains(&key) && !td.contains(&key));
    let (nu, td) = store.sweep_tick(Some(lease.expires_at + 0.02));
    assert!(!nu.contains(&key) && !td.contains(&key));
    let (_, td) = store.sweep_tick(Some(lease.grace_deadline + 0.001));
    assert!(td.contains(&key));
    assert_eq!(
        store.sweep_tick(Some(lease.grace_deadline + 0.002)),
        (vec![], vec![])
    );

    // The production sweeper chain: sweep -> registry.deregister.
    let sweeper = HeartbeatSweeper::new(
        app.state.registry.clone(),
        store.clone(),
        std::time::Duration::from_secs(60),
    );
    let sid = register_with_lease(&app, &on, 10, "dn").await?;
    let lease = store.get_lease(&on, &sid).required()?;
    sweeper.sweep_once_at(Some(lease.grace_deadline + 0.001));
    assert!(!list_ids(&app, &on, true).await?.contains(&sid));

    // A broken deleter must not crash the sweeper.
    struct Broken;
    impl a2x_registry::heartbeat::HardDeleter for Broken {
        fn hard_delete(&self, _: &str, _: &str) -> Result<(), String> {
            Err("oops".into())
        }
    }
    let broken = HeartbeatSweeper::new(
        Arc::new(Broken),
        store.clone(),
        std::time::Duration::from_secs(60),
    );
    let sid = register_with_lease(&app, &on, 10, "broke").await?;
    let lease = store.get_lease(&on, &sid).required()?;
    broken.sweep_once_at(Some(lease.grace_deadline + 0.001));
    assert!(list_ids(&app, &on, true).await?.contains(&sid));
    Ok(())
}

// ── unhealthy filter ─────────────────────────────────────────────────────

#[tokio::test]
async fn unhealthy_filter_and_reserve() -> TestResult {
    let app = lite_app().await?;
    let on = enabled_dataset(&app).await?;
    let a = register_with_lease(&app, &on, 30, "rsva").await?;
    let b = register_with_lease(&app, &on, 30, "rsvb").await?;
    mark_unhealthy(&app, &on, &a)?;
    let listed = list_ids(&app, &on, false)
        .await?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    assert!(!listed.contains(&a) && listed.contains(&b));
    assert!(list_ids(&app, &on, true).await?.contains(&a));
    let r = app
        .post(
            &format!("/api/datasets/{on}/reservations"),
            json!({"filters": {}, "n": 2, "ttl_seconds": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let claimed: Vec<String> = r.json()["reservations"]
        .as_array()
        .required()?
        .iter()
        .map(|s| -> TestResult<_> { Ok(s["id"].as_str().required()?.into()) })
        .collect::<TestResult<Vec<_>>>()?;
    assert!(!claimed.contains(&a) && claimed.contains(&b));
    // Leased and unhealthy filters are independent.
    let r = app
        .get(&format!(
            "/api/datasets/{on}/services?include_unhealthy=true&include_leased=true"
        ))
        .await?;
    assert_eq!(r.json().as_array().required()?.len(), 2);
    let r = app
        .get(&format!("/api/datasets/{on}/services?include_unhealthy=true"))
        .await?;
    assert_eq!(r.json().as_array().required()?.len(), 1);
    Ok(())
}

// ── persistence ──────────────────────────────────────────────────────────

#[tokio::test]
async fn persistence_and_restart_recovery() -> TestResult {
    let app = lite_app().await?;
    let on = enabled_dataset(&app).await?;
    let store = app.state.heartbeat_store().required()?;
    let sid = register_with_lease(&app, &on, 30, "persist").await?;
    let data: Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join(&on).join("api_config.json"),
    )?)?;
    let target = data["services"]
        .as_array()
        .required()?
        .iter()
        .find(|s| s["service_id"] == sid)
        .required()?;
    assert_eq!(target["lease_ttl"], 30);

    store.drop_lease(&on, &sid);
    assert!(store.get_lease(&on, &sid).is_none());
    store.recover_from_persisted(&[(on.clone(), sid.clone(), 30)]);
    let lease = store.get_lease(&on, &sid).required()?;
    assert_eq!(lease.state, LeaseState::Unhealthy);
    assert!(lease.grace_deadline > a2x_common::lease::monotonic_now());
    let r = app
        .post(&format!("/api/datasets/{on}/services/{sid}/heartbeat"), json!({}))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["state"], "healthy");
    assert!(!store.is_unhealthy(&on, &sid));

    // A real restart recovers leases from disk into the grace window.
    let mut config = a2x_registry::AppConfig::with_home(app.tmp.path());
    config.sweeper_period = std::time::Duration::from_secs(3600);
    let state2 = a2x_registry::AppState::new(config);
    a2x_registry::backend::startup::run_warmup(state2.clone()).await;
    let lease = state2
        .heartbeat_store()
        .required()?
        .get_lease(&on, &sid)
        .required()?;
    assert_eq!(lease.state, LeaseState::Unhealthy);
    assert_eq!(lease.ttl_seconds, 30);
    Ok(())
}

// ── inline lease config ──────────────────────────────────────────────────

#[tokio::test]
async fn inline_lease_config_on_create() -> TestResult {
    let app = lite_app().await?;
    let r = app
        .post("/api/datasets", json!({"name": "oneshot", "lease_config": {"enabled": true, "min_ttl": 20, "max_ttl": 1200, "grace_period": 90}}))
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(
        body["lease_config"],
        json!({"enabled": true, "min_ttl": 20, "max_ttl": 1200, "grace_period": 90, "schema_version": 1})
    );
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join("oneshot/lease_config.json"),
    )?)?;
    assert_eq!(on_disk["min_ttl"], 20);
    assert_eq!(
        app.get("/api/datasets/oneshot/lease-config").await?.json()["enabled"],
        true
    );
    let r = app.post("/api/datasets", json!({"name": "legacy"})).await?;
    assert!(r.json().get("lease_config").is_none());
    assert!(!app.database_dir().join("legacy/lease_config.json").exists());

    let r = app
        .post("/api/datasets", json!({"name": "ready", "lease_config": {"enabled": true, "min_ttl": 5, "max_ttl": 120, "grace_period": 30}}))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .post(
            "/api/datasets/ready/services/a2a",
            json!({"agent_card": agent_card("ready1"), "lease_ttl": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["lease_ttl"], 30);

    let r = app
        .post("/api/datasets", json!({"name": "badbounds", "lease_config": {"enabled": true, "min_ttl": 200, "max_ttl": 50, "grace_period": 30}}))
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = app
        .post("/api/datasets", json!({"name": "staged", "lease_config": {"enabled": false, "min_ttl": 5, "max_ttl": 120, "grace_period": 30}}))
        .await?;
    assert_eq!(r.json()["lease_config"]["enabled"], false);
    let r = app
        .post(
            "/api/datasets/staged/services/a2a",
            json!({"agent_card": agent_card("x"), "lease_ttl": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["detail"]["code"], "heartbeat_not_supported");

    // Combined auth and lease config in a single request.
    let (store, admin_token) =
        AuthStore::bootstrap(Some(&app.tmp.path().join("auth_data_oneshot")), None, "root")?;
    app.state.set_auth_store(Some(Arc::new(store)));
    let r = app
        .post_h(
            "/api/datasets",
            &[("authorization", &bearer(&admin_token))],
            json!({"name": "both", "auth_required": true, "lease_config": {"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["auth_required"], true);
    assert_eq!(r.json()["lease_config"]["min_ttl"], 10);
    assert_eq!(
        app.get("/api/datasets/both/auth-config").await?.json()["required"],
        true
    );
    assert_eq!(
        app.get("/api/datasets/both/lease-config").await?.json()["enabled"],
        true
    );
    app.state.set_auth_store(None);
    Ok(())
}

// ── auth integration and README flow ─────────────────────────────────────

#[tokio::test]
async fn readme_three_role_flow() -> TestResult {
    let app = lite_app().await?;
    let (store, admin_token) =
        AuthStore::bootstrap(Some(&app.tmp.path().join("auth_data_rm")), None, "root")?;
    app.state.set_auth_store(Some(Arc::new(store)));
    let admin = bearer(&admin_token);
    let r = app
        .post_h(
            "/api/datasets",
            &[("authorization", &admin)],
            json!({"name": "translators", "auth_required": true, "lease_config": {"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let provision = |handle: &str, role: &str| {
        let admin = admin.clone();
        let app = &app;
        let body = json!({"handle": handle, "role": role, "namespaces": ["translators"]});
        async move {
            let r = app
                .post_h("/api/auth/principals", &[("authorization", &admin)], body)
                .await?;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            Ok::<String, ap_support::testing::TestError>(r.json()["token"].as_str().required()?.to_string())
        }
    };
    let provider_token = provision("alice-provider", "provider").await?;
    let user_token = provision("bob-user", "user").await?;
    let p = bearer(&provider_token);
    let u = bearer(&user_token);

    // Heartbeat without a token on an auth namespace: 401.
    let mut card = agent_card("EN-ZH Translator");
    card["region"] = json!("cn-east-1");
    card["status"] = json!("online");
    let r = app
        .post_h(
            "/api/datasets/translators/services/a2a",
            &[("authorization", &p)],
            json!({"agent_card": card, "lease_ttl": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    assert_eq!(r.json()["lease_ttl"], 60);
    let r = app
        .post(
            &format!("/api/datasets/translators/services/{sid}/heartbeat"),
            json!({}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = app
        .post_h(
            &format!("/api/datasets/translators/services/{sid}/heartbeat"),
            &[("authorization", &p)],
            json!({}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);

    // Provider: update, set busy, restore, and the user only sees online.
    let r = app
        .put_h(
            &format!("/api/datasets/translators/services/{sid}"),
            &[("authorization", &p)],
            json!({"region": "cn-east-2"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .put_h(
            &format!("/api/datasets/translators/services/{sid}"),
            &[("authorization", &p)],
            json!({"status": "busy"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let hits = app
        .get_h(
            "/api/datasets/translators/services?status=online",
            &[("authorization", &u)],
        )
        .await?
        .json();
    assert!(hits.as_array().required()?.iter().all(|s| s["id"] != sid));
    let detail = app
        .get_h(
            &format!("/api/datasets/translators/services/{sid}"),
            &[("authorization", &u)],
        )
        .await?
        .json();
    assert_eq!(detail["metadata"]["status"], "busy");
    assert_eq!(detail["metadata"]["region"], "cn-east-2");
    app.put_h(
        &format!("/api/datasets/translators/services/{sid}"),
        &[("authorization", &p)],
        json!({"status": "online"}),
    )
    .await?;
    let hits = app
        .get_h(
            "/api/datasets/translators/services?status=online&region=cn-east-2",
            &[("authorization", &u)],
        )
        .await?
        .json();
    assert!(hits.as_array().required()?.iter().any(|s| s["id"] == sid));

    // User: reserve with filters, holder coerced to the principal id.
    let r = app
        .post_h(
            "/api/datasets/translators/reservations",
            &[("authorization", &u)],
            json!({"filters": {"region": "cn-east-2", "status": "online"}, "n": 1, "ttl_seconds": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["reservations"][0]["id"], sid);
    let me = app
        .get_h("/api/auth/whoami", &[("authorization", &u)])
        .await?
        .json();
    assert_eq!(r.json()["holder_id"], me["principal_id"]);

    // User cannot register.
    let r = app
        .post_h(
            "/api/datasets/translators/services/a2a",
            &[("authorization", &u)],
            json!({"agent_card": agent_card("nope"), "lease_ttl": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Provider deregisters; confirm gone.
    let r = app
        .delete_h(
            &format!("/api/datasets/translators/services/{sid}"),
            &[("authorization", &p)],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .get_h(
            &format!("/api/datasets/translators/services/{sid}"),
            &[("authorization", &p)],
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.state.set_auth_store(None);
    Ok(())
}
