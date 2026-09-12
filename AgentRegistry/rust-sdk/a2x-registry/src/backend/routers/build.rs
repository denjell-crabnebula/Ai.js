// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Taxonomy build operations: trigger, status, cancel and the SSE log
//! stream under `/api/datasets/{dataset}/build`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::RequireAdminOrAnon;
use crate::backend::build_jobs::BuildJobs;
use crate::backend::engines::{BuildContext, BuildLogSink, EngineError};
use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;
use crate::register::BuildRequest;

pub const MSG_RUNNING: &str = "构建中，请稍候...";
pub const MSG_STARTED: &str = "构建已启动";
pub const MSG_CANCELLED: &str = "构建已取消";
pub const MSG_DONE: &str = "分类树构建完成";

/// Router mounted at `/api/datasets`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/{dataset}/build",
            axum::routing::post(trigger_build).delete(cancel_build),
        )
        .route("/{dataset}/build/status", get(get_build_status))
        .route("/{dataset}/build/stream", get(build_stream))
}

struct JobLogSink {
    jobs: Arc<BuildJobs>,
    dataset: String,
}

impl BuildLogSink for JobLogSink {
    fn log(&self, line: &str) {
        let stamped = format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), line);
        self.jobs.log(&self.dataset, &stamped);
    }
}

async fn trigger_build(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
    body: Option<Json<BuildRequest>>,
) -> ApiResult<Json<Value>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let Some(cancel) = state.build_jobs.start(&dataset, MSG_RUNNING) else {
        return Err(ApiError::conflict(format!(
            "Build already running for '{dataset}'"
        )));
    };
    let jobs = state.build_jobs.clone();
    let engine = state.build_engine.clone();
    let paths = state.search.paths(&dataset);
    let taxonomy = state.taxonomy.clone();
    let ds = dataset.clone();
    tokio::spawn(async move {
        if !paths.service_path.exists() {
            let msg = format!(
                "No services registered for dataset '{ds}' yet (service.json missing). Register at least one service first."
            );
            jobs.finish(&ds, "error", &msg, false);
            return;
        }
        let ctx = BuildContext {
            dataset: ds.clone(),
            paths,
            request: req,
            log: Arc::new(JobLogSink {
                jobs: jobs.clone(),
                dataset: ds.clone(),
            }),
            cancel,
        };
        match engine.build(ctx).await {
            Ok(()) => {
                taxonomy.invalidate(&ds);
                jobs.finish(&ds, "done", MSG_DONE, true);
            }
            Err(EngineError::Cancelled) => {}
            Err(e) => {
                let msg = e.to_string();
                tracing::error!("Taxonomy build error for {}: {}", ds, msg);
                jobs.finish(&ds, "error", &msg, false);
            }
        }
    });
    Ok(Json(
        json!({"dataset": dataset, "status": "started", "message": MSG_STARTED}),
    ))
}

async fn get_build_status(State(state): State<Arc<AppState>>, Path(dataset): Path<String>) -> Json<Value> {
    Json(Value::Object(state.build_jobs.status_json(&dataset)))
}

async fn cancel_build(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
) -> ApiResult<Json<Value>> {
    if !state.build_jobs.cancel(&dataset, MSG_CANCELLED) {
        return Err(ApiError::conflict(format!("No running build for '{dataset}'")));
    }
    Ok(Json(
        json!({"dataset": dataset, "status": "cancelled", "message": MSG_CANCELLED}),
    ))
}

fn sse_data(v: &Value) -> String {
    format!("data: {v}\n\n")
}

/// Build the SSE byte stream for a dataset: replay logs, then live events
/// until a status event, with `: keepalive` comments every second.
pub fn sse_stream(
    jobs: Arc<BuildJobs>,
    dataset: String,
) -> impl futures::Stream<Item = Result<String, std::convert::Infallible>> {
    async_stream::stream! {
        let (job, mut rx, _sub) = jobs.subscribe_with_snapshot(&dataset);
        for msg in job.as_ref().map(|j| j.logs.clone()).unwrap_or_default() {
            yield Ok(sse_data(&json!({"type": "log", "message": msg})));
        }
        let status = job.as_ref().map(|j| j.status.clone()).unwrap_or_else(|| "idle".into());
        if status != "running" {
            let message = job.as_ref().map(|j| j.message.clone()).unwrap_or_default();
            yield Ok(sse_data(&json!({"type": "status", "status": status, "message": message})));
            return;
        }
        loop {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Some(item)) => {
                    let is_status = item.get("type").and_then(Value::as_str) == Some("status");
                    yield Ok(sse_data(&item));
                    if is_status {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let cur = jobs.get(&dataset);
                    let cur_status = cur.as_ref().map(|j| j.status.clone()).unwrap_or_else(|| "idle".into());
                    if cur_status != "running" {
                        let message = cur.as_ref().map(|j| j.message.clone()).unwrap_or_default();
                        yield Ok(sse_data(&json!({"type": "status", "status": cur_status, "message": message})));
                        break;
                    }
                    yield Ok(": keepalive\n\n".to_string());
                }
            }
        }
    }
}

async fn build_stream(State(state): State<Arc<AppState>>, Path(dataset): Path<String>) -> Response {
    let stream = sse_stream(state.build_jobs.clone(), dataset);
    let mut resp = Body::from_stream(stream).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use futures::StreamExt;

    #[tokio::test]
    async fn stream_replays_then_closes_on_status() -> TestResult {
        let jobs = Arc::new(BuildJobs::new());
        jobs.start("ds", MSG_RUNNING).required()?;
        jobs.log("ds", "first");
        let mut s = Box::pin(sse_stream(jobs.clone(), "ds".into()));
        assert_eq!(
            s.next().await.required()??,
            "data: {\"type\":\"log\",\"message\":\"first\"}\n\n"
        );
        jobs.log("ds", "second");
        assert!(s.next().await.required()??.contains("second"));
        jobs.finish("ds", "done", MSG_DONE, true);
        let last = s.next().await.required()??;
        assert!(last.contains("\"type\":\"status\"") && last.contains("\"done\""));
        assert!(s.next().await.is_none());
        let mut idle = Box::pin(sse_stream(Arc::new(BuildJobs::new()), "x".into()));
        assert_eq!(
            idle.next().await.required()??,
            "data: {\"type\":\"status\",\"status\":\"idle\",\"message\":\"\"}\n\n"
        );
        assert!(idle.next().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn stream_sends_keepalive_when_quiet() -> TestResult {
        let jobs = Arc::new(BuildJobs::new());
        jobs.start("ds", MSG_RUNNING).required()?;
        let mut s = Box::pin(sse_stream(jobs.clone(), "ds".into()));
        tokio::time::pause();
        let next = s.next();
        tokio::pin!(next);
        tokio::time::advance(Duration::from_millis(1100)).await;
        assert_eq!(next.await.required()??, ": keepalive\n\n");
        Ok(())
    }
}
