// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/registered/`: dataset creation and A2A registration.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::StatusCode;
use common::{agent_card, lite_app};
use serde_json::json;

#[tokio::test]
async fn create_dataset() -> TestResult {
    let app = lite_app().await?;
    let name = "ds_create";
    let r = app.post("/api/datasets", json!({"name": name})).await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(body["dataset"], name);
    assert_eq!(body["embedding_model"], "all-MiniLM-L6-v2");
    assert_eq!(body["formats"]["a2a"], "v0.0");
    assert_eq!(body["auth_required"], false);
    assert_eq!(body["status"], "created");
    assert!(body.get("lease_config").is_none());
    let r = app.delete(&format!("/api/datasets/{name}")).await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "deleted");
    Ok(())
}

#[tokio::test]
async fn register_a2a() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app
        .post(
            &format!("/api/datasets/{ds}/services/a2a"),
            json!({"agent_card": agent_card("agent-1"), "persistent": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert!(body["service_id"].as_str().required()?.starts_with("agent_"));
    assert_eq!(body["status"], "registered");
    assert_eq!(body["lease_ttl"], serde_json::Value::Null);
    Ok(())
}

#[tokio::test]
async fn register_then_list_and_get() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let sid = app.register(&ds, agent_card("flow-1")).await?;
    let r = app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert_eq!(r.status, StatusCode::OK);
    let list = r.json();
    assert!(list.as_array().required()?.iter().any(|e| e["id"] == sid));
    let entry = list
        .as_array()
        .required()?
        .iter()
        .find(|e| e["id"] == sid)
        .required()?;
    assert_eq!(entry["source"], "api_config");
    assert_eq!(entry["description"], "tester. Skills: [s] s");
    assert_eq!(entry["metadata"]["description"], "tester");
    let r = app.get(&format!("/api/datasets/{ds}/services/{sid}")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["id"], sid);
    assert!(r.json().get("source").is_none());
    let r = app.get(&format!("/api/datasets/{ds}/services/missing")).await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.json()["detail"].as_str().required()?.contains(&ds));
    Ok(())
}

#[tokio::test]
async fn register_generic_and_auto_init_dataset() -> TestResult {
    let app = lite_app().await?;
    let r = app
        .post(
            "/api/datasets/fresh_ns/services/generic",
            json!({"name": "Calc", "description": "adds", "url": "http://c", "inputSchema": {"type": "object"}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(app.database_dir().join("fresh_ns/vector_config.json").exists());
    assert!(app.database_dir().join("fresh_ns/register_config.json").exists());
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    let r = app.get(&format!("/api/datasets/fresh_ns/services/{sid}")).await?;
    assert_eq!(r.json()["metadata"]["url"], "http://c");
    assert_eq!(r.json()["type"], "generic");
    let r = app
        .post(
            "/api/datasets/fresh_ns/services/generic",
            json!({"name": "", "description": "adds"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["detail"]
            .as_str()
            .required()?
            .contains("name is required")
    );
    Ok(())
}

#[tokio::test]
async fn register_config_gates_types() -> TestResult {
    let app = lite_app().await?;
    let r = app
        .post(
            "/api/datasets",
            json!({"name": "strict", "formats": {"a2a": "v1.0"}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["formats"], json!({"a2a": "v1.0"}));
    let r = app
        .post(
            "/api/datasets/strict/services/generic",
            json!({"name": "n", "description": "d"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .post(
            "/api/datasets/strict/services/a2a",
            json!({"agent_card": {"name": "loose", "description": "d"}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["detail"]
            .as_str()
            .required()?
            .contains("version is required")
    );
    assert!(!app.register("strict", agent_card("strict-ok")).await?.is_empty());
    let r = app.get("/api/datasets/strict/register-config").await?;
    assert_eq!(r.json(), json!({"dataset": "strict", "formats": {"a2a": "v1.0"}}));
    let r = app
        .post(
            "/api/datasets/strict/register-config",
            json!({"formats": {"bogus": "v0.0"}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .post(
            "/api/datasets/strict/register-config",
            json!({"formats": {"generic": {"min_version": "v0.0"}}}),
        )
        .await?;
    assert_eq!(r.json()["formats"], json!({"generic": "v0.0"}));
    let r = app
        .post(
            "/api/datasets",
            json!({"name": "empty", "formats": {"x": "v0.0"}}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app.post("/api/datasets", json!({"name": "strict"})).await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json()["detail"].as_str().required()?.contains("already exists"));
    Ok(())
}

#[tokio::test]
async fn list_datasets_counts_and_pagination_headers() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app.get("/api/datasets").await?;
    assert_eq!(r.json(), json!([]));
    for i in 0..5 {
        app.register(&ds, agent_card(&format!("p-{i}"))).await?;
    }
    let r = app.get("/api/datasets").await?;
    let list = r.json();
    assert_eq!(list[0]["name"], ds);
    assert_eq!(list[0]["service_count"], 5);
    assert_eq!(list[0]["query_count"], 0);

    let r = app
        .get(&format!("/api/datasets/{ds}/services?fields=brief&size=2&page=2"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["X-Total-Count"], "5");
    assert_eq!(r.headers["X-Page"], "2");
    assert_eq!(r.headers["X-Total-Pages"], "3");
    assert_eq!(r.headers["X-Page-Size"], "2");
    let page = r.json();
    assert_eq!(page.as_array().required()?.len(), 2);
    assert_eq!(
        page[0].as_object().required()?.keys().collect::<Vec<_>>(),
        vec!["id", "name", "description"]
    );

    let r = app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert!(r.headers.get("X-Total-Count").is_none());
    assert_eq!(r.json().as_array().required()?.len(), 5);
    let r = app
        .get(&format!("/api/datasets/{ds}/services?fields=weird"))
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app.get(&format!("/api/datasets/{ds}/services?size=abc")).await?;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}

#[tokio::test]
async fn service_json_is_written_and_persisted_entries_reload() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let sid = app.register(&ds, agent_card("persist")).await?;
    let service_json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join(&ds).join("service.json"),
    )?)?;
    assert_eq!(service_json[0]["id"], sid);
    let api: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        app.database_dir().join(&ds).join("api_config.json"),
    )?)?;
    assert_eq!(api["services"][0]["service_id"], sid);
    assert!(api["services"][0].get("owner_id").is_none());
    assert!(api["services"][0].get("lease_ttl").is_none());

    // A second app over the same home reloads the persisted entry.
    let mut config = a2x_registry::AppConfig::with_home(app.tmp.path());
    config.sweeper_period = std::time::Duration::from_secs(3600);
    let state2 = a2x_registry::AppState::new(config);
    a2x_registry::backend::startup::run_warmup(state2.clone()).await;
    assert!(state2.registry.get_entry(&ds, &sid).is_some());
    assert!(state2.warmup.is_ready());
    Ok(())
}
