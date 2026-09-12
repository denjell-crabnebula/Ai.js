// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/deregistered/`: service removal and lease self release.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::StatusCode;
use common::{agent_card, lite_app};
use serde_json::json;

#[tokio::test]
async fn deregister_service() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let sid = app.register(&ds, agent_card("dereg-1")).await?;
    let r = app.delete(&format!("/api/datasets/{ds}/services/{sid}")).await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json(), json!({"service_id": sid, "status": "deregistered"}));
    let r = app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert!(!r.json().as_array().required()?.iter().any(|e| e["id"] == sid));
    let r = app.delete(&format!("/api/datasets/{ds}/services/{sid}")).await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn release_lease_self() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let sid = app.register(&ds, agent_card("self-lease")).await?;
    let r = app
        .post(
            &format!("/api/datasets/{ds}/reservations"),
            json!({"filters": {}, "n": 1, "ttl_seconds": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let holder = r.json()["holder_id"].as_str().required()?.to_string();
    let r = app
        .delete(&format!("/api/datasets/{ds}/services/{sid}/lease"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"released": true, "prev_holder_id": holder}));
    let r = app
        .delete(&format!("/api/datasets/{ds}/services/{sid}/lease"))
        .await?;
    assert_eq!(r.json(), json!({"released": false, "prev_holder_id": null}));
    Ok(())
}

#[tokio::test]
async fn user_config_entries_cannot_be_deregistered() -> TestResult {
    let app = lite_app().await?;
    let ds_dir = app.database_dir().join("uc");
    std::fs::create_dir_all(&ds_dir)?;
    std::fs::write(
        ds_dir.join("user_config.json"),
        json!({"services": [{"type": "generic", "service_id": "locked", "name": "n", "description": "d"}]})
            .to_string(),
    )?;
    let mut config = a2x_registry::AppConfig::with_home(app.tmp.path());
    config.sweeper_period = std::time::Duration::from_secs(3600);
    let state = a2x_registry::AppState::new(config);
    a2x_registry::backend::startup::run_warmup(state.clone()).await;
    let app2 = common::TestApp {
        tmp: app.tmp,
        state: state.clone(),
        router: a2x_registry::build_router(state),
    };
    let r = app2.delete("/api/datasets/uc/services/locked").await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json()["detail"].as_str().required()?.contains("user_config"));
    let r = app2
        .put("/api/datasets/uc/services/locked", json!({"description": "x"}))
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    Ok(())
}
