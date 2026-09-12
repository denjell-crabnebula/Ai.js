// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP routers: datasets, build, search and providers.

pub mod build;
pub mod dataset;
pub mod provider;
pub mod search;

use std::sync::Arc;

use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;
use crate::register::RegistryError;

/// Run a blocking registry call on the dataset worker pool and map its
/// error to HTTP (the Python `_run` helper).
pub async fn run<T, F>(state: &Arc<AppState>, f: F) -> ApiResult<T>
where
    F: FnOnce() -> Result<T, RegistryError> + Send + 'static,
    T: Send + 'static,
{
    let _permit = state
        .workers
        .dataset
        .acquire()
        .await
        .map_err(|_| ApiError::internal("worker pool closed"))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::from)
}

/// Parse a boolean query value the way FastAPI does.
pub fn parse_bool(name: &str, value: &str) -> ApiResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "y" | "t" => Ok(true),
        "false" | "0" | "no" | "off" | "n" | "f" => Ok(false),
        _ => Err(ApiError::unprocessable(format!(
            "query parameter '{name}' must be a boolean, got {value:?}"
        ))),
    }
}
