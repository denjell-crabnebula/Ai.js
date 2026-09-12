// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/reserve/`: the reservation lease lifecycle.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::StatusCode;
use common::{agent_card, lite_app};
use serde_json::json;

#[tokio::test]
async fn reservation_lifecycle() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let mut sids = Vec::new();
    for i in 0..2 {
        sids.push(app.register(&ds, agent_card(&format!("resv-{i}"))).await?);
    }
    let r = app
        .post(
            &format!("/api/datasets/{ds}/reservations"),
            json!({"filters": {}, "n": 2, "ttl_seconds": 30}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    let holder = body["holder_id"].as_str().required()?.to_string();
    assert!(holder.starts_with("holder_"));
    assert_eq!(body["ttl_seconds"], 30);
    assert!(body["expires_at_unix"].as_f64().required()? > 0.0);
    assert_eq!(body["reservations"].as_array().required()?.len(), 2);
    assert_eq!(body["reservations"][0]["type"], "a2a");

    // Leased entries are hidden from the default listing.
    let r = app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert_eq!(r.json().as_array().required()?.len(), 0);
    let r = app
        .get(&format!("/api/datasets/{ds}/services?include_leased=true"))
        .await?;
    assert_eq!(r.json().as_array().required()?.len(), 2);

    let r = app
        .post(
            &format!("/api/datasets/{ds}/reservations/{holder}/extend"),
            json!({"ttl_seconds": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["expires_at_unix"].as_f64().required()? > 0.0);
    let r = app
        .post(
            &format!("/api/datasets/{ds}/reservations/nobody/extend"),
            json!({"ttl_seconds": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = app
        .delete(&format!("/api/datasets/{ds}/reservations/other/{}", sids[0]))
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .delete(&format!("/api/datasets/{ds}/reservations/{holder}/{}", sids[0]))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"released": [sids[0]]}));
    let r = app
        .delete(&format!("/api/datasets/{ds}/reservations/{holder}/{}", sids[0]))
        .await?;
    assert_eq!(r.json(), json!({"released": []}));
    let r = app
        .delete(&format!("/api/datasets/{ds}/reservations/{holder}"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"released": [sids[1]]}));
    let r = app
        .delete(&format!("/api/datasets/{ds}/reservations/{holder}"))
        .await?;
    assert_eq!(r.json(), json!({"released": []}));
    let r = app
        .post(&format!("/api/datasets/{ds}/reservations"), json!({"n": -1}))
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    Ok(())
}
