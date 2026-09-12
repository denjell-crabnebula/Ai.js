// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/update/`: field level updates and filtered listing.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::StatusCode;
use common::{agent_card, lite_app};
use serde_json::json;

#[tokio::test]
async fn update_status_and_filter() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let sid = app.register(&ds, agent_card("update-1")).await?;
    let r = app
        .put(
            &format!("/api/datasets/{ds}/services/{sid}"),
            json!({"status": "busy"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"service_id": sid, "dataset": ds, "status": "updated", "changed_fields": ["status"], "taxonomy_affected": false})
    );
    let r = app
        .get(&format!("/api/datasets/{ds}/services?status=busy"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json().as_array().required()?.iter().any(|e| e["id"] == sid));
    // The default-online rule: a service without status matches status=online.
    let other = app.register(&ds, agent_card("update-2")).await?;
    let r = app
        .get(&format!("/api/datasets/{ds}/services?status=online"))
        .await?;
    let ids: Vec<String> = r
        .json()
        .as_array()
        .required()?
        .iter()
        .map(|e| -> TestResult<_> { Ok(e["id"].as_str().required()?.into()) })
        .collect::<TestResult<Vec<_>>>()?;
    assert!(ids.contains(&other));
    assert!(!ids.contains(&sid));
    Ok(())
}

#[tokio::test]
async fn update_rules_for_generic_and_unknown() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app
        .post(
            &format!("/api/datasets/{ds}/services/generic"),
            json!({"name": "g", "description": "d"}),
        )
        .await?;
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    let r = app
        .put(
            &format!("/api/datasets/{ds}/services/{sid}"),
            json!({"description": "new", "url": "http://n", "owner_id": "evil"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changed_fields"], json!(["description", "url"]));
    assert_eq!(r.json()["taxonomy_affected"], true);
    let r = app
        .put(&format!("/api/datasets/{ds}/services/{sid}"), json!({"bogus": 1}))
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .put(
            &format!("/api/datasets/{ds}/services/nope"),
            json!({"description": "x"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .put(&format!("/api/datasets/{ds}/services/{sid}"), json!([1, 2]))
        .await?;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}
