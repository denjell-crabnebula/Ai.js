// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Application assembly: router mounting, CORS, JSON fallbacks and the
//! optional static front end.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::cors::{Any, CorsLayer};

use super::routers::{build, dataset, provider, search};
use super::state::AppState;

/// Build the full application router.
pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .nest("/api/search", search::router())
        .nest("/api/datasets", dataset::router())
        .nest("/api/datasets", build::router())
        .nest("/api/providers", provider::router())
        .nest("/api/auth", crate::auth::router::router(state.clone()))
        .nest("/api/datasets", crate::heartbeat::router::router())
        .route("/api/warmup-status", get(warmup_status))
        .route("/api/cluster/{*rest}", any(cluster_dispatch));
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any);
    let router = api.method_not_allowed_fallback(method_not_allowed);
    let router = match &state.frontend_dist {
        Some(dir) if dir.is_dir() => router.fallback_service(
            tower_http::services::ServeDir::new(dir)
                .append_index_html_on_directories(true)
                .not_found_service(axum::routing::any(not_found)),
        ),
        _ => router.fallback(not_found),
    };
    router.layer(cors).with_state(state)
}

/// Detail text answered by `/api/cluster/*` while the module is dormant.
pub const CLUSTER_NOT_INITIALIZED: &str = "Cluster module not initialized on this registry. Run 'a2x-registry cluster init' to enable distributed sync.";

/// Forward `/api/cluster/*` to the cluster router when initialized.
async fn cluster_dispatch(State(state): State<Arc<AppState>>, req: Request) -> Response {
    match state.cluster_router() {
        Some(router) => match router.oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        },
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": CLUSTER_NOT_INITIALIZED})),
        )
            .into_response(),
    }
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"})))
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"detail": "Method Not Allowed"})),
    )
}

/// `GET /api/warmup-status`: public warmup fields only.
async fn warmup_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(Value::Object(state.warmup.public()))
}
