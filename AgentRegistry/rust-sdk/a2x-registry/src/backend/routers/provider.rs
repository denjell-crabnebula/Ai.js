// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! LLM provider management under `/api/providers`: list providers from
//! `llm_apikey.json` and switch the active one by reordering the file.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::RequireAdminOrAnon;
use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;

/// Router mounted at `/api/providers`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_providers))
        .route("/{name}", post(switch_provider))
}

/// Load `llm_apikey.json`, tolerating trailing commas like the Python code.
pub fn load_llm_config(path: &std::path::Path) -> ApiResult<Value> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ApiError::internal(format!("cannot read {}: {e}", path.display())))?;
    let content = content.replace(",\n}", "\n}").replace(",}", "}");
    serde_json::from_str(&content).map_err(|e| ApiError::internal(format!("invalid {}: {e}", path.display())))
}

async fn list_providers(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let config = load_llm_config(&state.llm_apikey_path)?;
    let providers = config
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut current = state.search.get_current_provider();
    if current.is_empty() {
        current = providers
            .first()
            .and_then(|p| p.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
    }
    let listed: Vec<Value> = providers
        .iter()
        .map(|p| json!({"name": p.get("name").cloned().unwrap_or(Value::Null), "model": p.get("model").cloned().unwrap_or(Value::Null)}))
        .collect();
    Ok(Json(json!({ "providers": listed, "current": current })))
}

async fn switch_provider(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
) -> ApiResult<Json<Value>> {
    let mut config = load_llm_config(&state.llm_apikey_path)?;
    let providers = config
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let valid_names: Vec<String> = providers
        .iter()
        .map(|p| p.get("name").and_then(Value::as_str).unwrap_or("").to_string())
        .collect();
    let Some(target_idx) = valid_names.iter().position(|n| *n == name) else {
        return Ok(Json(
            json!({"error": format!("Unknown provider: {name}"), "valid": valid_names}),
        ));
    };
    let mut reordered = vec![providers[target_idx].clone()];
    reordered.extend(
        providers
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != target_idx)
            .map(|(_, p)| p.clone()),
    );
    if let Some(obj) = config.as_object_mut() {
        obj.insert("providers".into(), Value::Array(reordered));
    }
    let text = serde_json::to_string_pretty(&config).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::write(&state.llm_apikey_path, text).map_err(|e| ApiError::internal(e.to_string()))?;
    state.search.switch_provider(&name);
    tracing::info!("Switched LLM provider to: {}", name);
    Ok(Json(json!({"status": "ok", "current": name})))
}
