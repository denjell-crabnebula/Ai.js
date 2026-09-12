// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/query/`: build trigger and status, embedding models,
//! search and judge 503 bodies, and the WebSocket error hint.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::StatusCode;
use common::lite_app;
use futures::{SinkExt, StreamExt};
use serde_json::json;

#[tokio::test]
async fn build_trigger_and_status() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app.get(&format!("/api/datasets/{ds}/build/status")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"dataset": ds, "status": "idle"}));

    let r = app
        .post(&format!("/api/datasets/{ds}/build"), json!({"resume": "no"}))
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"dataset": ds, "status": "started", "message": "构建已启动"})
    );
    // The background task ends in error: no service.json yet.
    for _ in 0..50 {
        if app
            .state
            .build_jobs
            .get(&ds)
            .map(|j| j.status != "running")
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let r = app.get(&format!("/api/datasets/{ds}/build/status")).await?;
    let body = r.json();
    assert_eq!(body["status"], "error");
    assert!(
        body["message"]
            .as_str()
            .required()?
            .contains("service.json missing")
    );
    assert!(body["started_at"].is_number());
    assert!(body["finished_at"].is_number());
    assert_eq!(body["logs"], json!([]));
    let r = app.delete(&format!("/api/datasets/{ds}/build")).await?;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // With a service.json the unavailable engine reports the 503 detail.
    app.register(&ds, common::agent_card("b")).await?;
    let r = app.post(&format!("/api/datasets/{ds}/build"), json!({})).await?;
    assert_eq!(r.status, StatusCode::OK);
    for _ in 0..50 {
        if app
            .state
            .build_jobs
            .get(&ds)
            .map(|j| j.status != "running")
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(app.state.build_jobs.get(&ds).required()?.status, "error");

    // SSE replay for a finished job: status event, then the stream ends.
    let r = app.get(&format!("/api/datasets/{ds}/build/stream")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "text/event-stream");
    assert_eq!(r.headers["cache-control"], "no-cache");
    let text = r.text();
    assert!(
        text.starts_with("data: {\"type\":\"status\",\"status\":\"error\""),
        "{text}"
    );
    assert!(text.ends_with("\n\n"));
    Ok(())
}

#[tokio::test]
async fn build_cancel_flow() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    // Start a job by hand so it stays running.
    app.state.build_jobs.start(&ds, "构建中，请稍候...").required()?;
    app.state.build_jobs.log(&ds, "10:00:00  line");
    let r = app.post(&format!("/api/datasets/{ds}/build"), json!({})).await?;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = app.delete(&format!("/api/datasets/{ds}/build")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"dataset": ds, "status": "cancelled", "message": "构建已取消"})
    );
    let r = app.get(&format!("/api/datasets/{ds}/build/stream")).await?;
    let text = r.text();
    assert_eq!(
        text,
        "data: {\"type\":\"log\",\"message\":\"10:00:00  line\"}\n\ndata: {\"type\":\"status\",\"status\":\"cancelled\",\"message\":\"构建已取消\"}\n\n"
    );
    Ok(())
}

#[tokio::test]
async fn embedding_models_route() -> TestResult {
    let app = lite_app().await?;
    let r = app.get("/api/datasets/embedding-models").await?;
    assert_eq!(r.status, StatusCode::OK);
    let models = &r.json()["models"];
    assert_eq!(models["all-MiniLM-L6-v2"]["dim"], 384);
    assert_eq!(models["shibing624/text2vec-base-chinese"]["language"], "zh");
    assert_eq!(
        a2x_registry::register::embedding::DEFAULT_EMBEDDING_MODEL,
        "all-MiniLM-L6-v2"
    );
    Ok(())
}

#[tokio::test]
async fn search_returns_503_without_vector_feature() -> TestResult {
    a2x_common::feature_flags::set_available(a2x_common::feature_flags::Feature::Vector, false);
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app
        .post(
            "/api/search",
            json!({"query": "x", "method": "vector", "dataset": ds, "top_k": 3}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = r.json();
    assert_eq!(body["feature"], "vector");
    assert_eq!(body["extras"], "vector");
    assert!(body["detail"].as_str().required()?.contains("vector"));

    // Non vector methods reach the (unavailable) engine: structured 503 too.
    let r = app
        .post(
            "/api/search",
            json!({"query": "x", "method": "traditional", "dataset": ds}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json()["feature"], "search");
    // Unknown method is an unhandled ValueError in the original: 500.
    let r = app
        .post(
            "/api/search",
            json!({"query": "x", "method": "bogus", "dataset": ds}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    // A dataset created at runtime has no taxonomy state (only startup
    // computes one), so A2X reaches the engine like in the original.
    let r = app
        .post(
            "/api/search",
            json!({"query": "x", "method": "a2x_get_all", "dataset": ds}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json()["feature"], "search");
    Ok(())
}

#[tokio::test]
async fn a2x_search_blocked_by_taxonomy_state() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    app.register(&ds, common::agent_card("t")).await?;
    // Restart so the dataset gets a taxonomy state computed at startup.
    let mut config = a2x_registry::AppConfig::with_home(app.tmp.path());
    config.sweeper_period = std::time::Duration::from_secs(3600);
    let state = a2x_registry::AppState::new(config);
    a2x_registry::backend::startup::run_warmup(state.clone()).await;
    let app2 = common::TestApp {
        tmp: app.tmp,
        state: state.clone(),
        router: a2x_registry::build_router(state.clone()),
    };
    assert_eq!(
        state.registry.get_taxonomy_state(&ds),
        Some(a2x_registry::register::TaxonomyState::Nonexistent)
    );
    let r = app2
        .post(
            "/api/search",
            json!({"query": "x", "method": "a2x_get_all", "dataset": ds}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(r.json()["detail"].as_str().required()?.contains("build"));
    Ok(())
}

#[tokio::test]
async fn search_judge_returns_503_when_llm_unconfigured() -> TestResult {
    let app = lite_app().await?;
    let r = app
        .post("/api/search/judge", json!({"query": "x", "services": []}))
        .await?;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = r.json();
    assert_eq!(body["reason"], "llm_not_configured");
    assert!(body["detail"].as_str().required()?.contains("llm_apikey.json"));
    Ok(())
}

#[tokio::test]
async fn search_ws_returns_install_hint() -> TestResult {
    a2x_common::feature_flags::set_available(a2x_common::feature_flags::Feature::Vector, false);
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = app.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/search/ws")).await?;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"query": "x", "method": "vector", "dataset": ds, "top_k": 3})
            .to_string()
            .into(),
    ))
    .await?;
    let msg = ws.next().await.required()??;
    let text = msg.into_text()?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(v["type"], "error");
    assert!(v["message"].as_str().required()?.contains("vector"));
    // Server closes after the error.
    let next = ws.next().await;
    assert!(matches!(
        next,
        None | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | Some(Err(_))
    ));

    // A2X method on a dataset without taxonomy also yields an error message.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/search/ws")).await?;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"query": "x", "method": "a2x_get_all", "dataset": ds})
            .to_string()
            .into(),
    ))
    .await?;
    let v: serde_json::Value = serde_json::from_str(&ws.next().await.required()??.into_text()?)?;
    assert_eq!(v["type"], "error");
    Ok(())
}
