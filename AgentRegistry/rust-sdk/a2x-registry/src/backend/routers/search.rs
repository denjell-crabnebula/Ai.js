// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Search API: `POST /api/search`, `POST /api/search/judge` and the
//! WebSocket `/api/search/ws` streaming A2X navigation steps.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::schemas::{JudgeRequest, JudgeResponse, SearchRequest, SearchResponse};
use crate::backend::services::search_service::{result_message, step_message};
use crate::backend::state::AppState;

/// Router mounted at `/api/search`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(search))
        .route("/judge", post(judge_relevance))
        .route("/ws", get(search_ws))
}

async fn search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> ApiResult<Json<SearchResponse>> {
    if req.method == "vector" {
        a2x_common::feature_flags::require(a2x_common::feature_flags::Feature::Vector)?;
    }
    let _permit = state
        .workers
        .search
        .acquire()
        .await
        .map_err(|_| ApiError::internal("worker pool closed"))?;
    let top_k = req.top_k.filter(|k| *k > 0).unwrap_or(10) as usize;
    let resp = state
        .search
        .search(&req.query, &req.method, &req.dataset, top_k)
        .await?;
    Ok(Json(resp))
}

async fn judge_relevance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JudgeRequest>,
) -> ApiResult<Json<JudgeResponse>> {
    let _permit = state
        .workers
        .search
        .acquire()
        .await
        .map_err(|_| ApiError::internal("worker pool closed"))?;
    let results = state.search.judge_services(&req.query, &req.services).await?;
    Ok(Json(JudgeResponse { results }))
}

async fn search_ws(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(state, socket))
}

async fn send_json(socket: &mut WebSocket, v: Value) -> bool {
    socket.send(Message::Text(v.to_string().into())).await.is_ok()
}

async fn handle_socket(state: Arc<AppState>, mut socket: WebSocket) {
    let raw = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(t))) => break t.to_string(),
            Some(Ok(Message::Binary(b))) => break String::from_utf8_lossy(&b).to_string(),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            _ => return,
        }
    };
    if let Err(msg) = run_ws_search(&state, &mut socket, &raw).await {
        tracing::error!("WebSocket error: {}", msg);
        let _ = send_json(&mut socket, json!({"type": "error", "message": msg})).await;
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn run_ws_search(state: &Arc<AppState>, socket: &mut WebSocket, raw: &str) -> Result<(), String> {
    let req: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let query = req.get("query").and_then(Value::as_str).unwrap_or("").to_string();
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("a2x_get_all")
        .to_string();
    let dataset = req
        .get("dataset")
        .and_then(Value::as_str)
        .unwrap_or("ToolRet_clean")
        .to_string();
    let top_k = req
        .get("top_k")
        .and_then(crate::util::py_int)
        .filter(|k| *k > 0)
        .unwrap_or(10) as usize;
    if method == "vector" {
        a2x_common::feature_flags::require(a2x_common::feature_flags::Feature::Vector)
            .map_err(|e| e.to_string())?;
    }
    let _permit = state
        .workers
        .search
        .acquire()
        .await
        .map_err(|_| "worker pool closed".to_string())?;
    if method.starts_with("a2x") {
        let (tx, mut rx) = mpsc::channel(64);
        let search = state.search.clone();
        let (q, m, d) = (query.clone(), method.clone(), dataset.clone());
        let mut task = tokio::spawn(async move { search.search_stream(&q, &m, &d, tx).await });
        loop {
            tokio::select! {
                step = rx.recv() => match step {
                    Some(step) => {
                        let msg = step_message(&step);
                        if !send_json(socket, json!({"type": "step", "data": msg})).await {
                            task.abort();
                            return Ok(());
                        }
                    }
                    None => break,
                },
                res = &mut task => {
                    while let Ok(step) = rx.try_recv() {
                        let msg = step_message(&step);
                        send_json(socket, json!({"type": "step", "data": msg})).await;
                    }
                    let resp = res.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
                    send_json(socket, json!({"type": "result", "data": result_message(&resp)})).await;
                    return Ok(());
                }
            }
        }
        let resp = task
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        send_json(socket, json!({"type": "result", "data": result_message(&resp)})).await;
    } else {
        let resp = state
            .search
            .search(&query, &method, &dataset, top_k)
            .await
            .map_err(|e| e.to_string())?;
        send_json(socket, json!({"type": "result", "data": resp})).await;
    }
    Ok(())
}
