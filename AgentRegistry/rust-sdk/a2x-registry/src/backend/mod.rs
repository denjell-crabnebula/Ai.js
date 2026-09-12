// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! The axum backend: application assembly, shared state, startup warmup,
//! routers, engines and services.

pub mod adapters;
pub mod app;
pub mod build_jobs;
pub mod cluster_hooks;
pub mod default_queries;
pub mod engines;
pub mod errors;
pub mod routers;
pub mod schemas;
pub mod services;
pub mod startup;
pub mod state;
pub mod workers;

pub use app::build_router;
pub use errors::ApiError;
pub use state::{AppConfig, AppState};
