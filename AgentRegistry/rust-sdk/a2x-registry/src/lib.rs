// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2X agent service registry.
//!
//! Rust port of `AgentRegistry/a2x_registry/{register,auth,heartbeat,backend}`
//! plus the `a2x-registry` and `a2x-register` command line tools.
//!
//! - [`register`]: the registry core (`RegistryService`, `RegistryStore`,
//!   models, format validation, agent card fetching).
//! - [`auth`]: static API key authentication with three roles and per
//!   namespace scoping.
//! - [`heartbeat`]: TTL leases with a two stage sweeper.
//! - [`backend`]: the axum HTTP application, routers, engines and startup.
//!
//! Wire formats (HTTP paths, JSON bodies, headers, status codes) and the
//! files persisted under `database/` match the original Python package.

pub mod auth;
pub mod backend;
pub mod heartbeat;
pub mod register;
pub mod util;

pub use backend::app::build_router;
pub use backend::state::{AppConfig, AppState};
pub use register::service::RegistryService;

/// The environment variables the registry and the crates it embeds read, with
/// the values a safe deployment accepts. Binaries pass it to
/// [`ap_support::env::EnvArgs::apply_or_exit`].
pub fn env_policy() -> ap_support::env::EnvPolicy {
    use ap_support::env::{EnvPolicy, Kind, Rule, VarSpec};
    let workers = |name: &'static str, description: &'static str, default: &'static str| {
        VarSpec::new(name, Kind::Integer, description)
            .default(default)
            .rule(Rule::int_range(1, 1024))
    };
    let policy = EnvPolicy::base()
        .merge(a2x_common::env_policy())
        .prefix("A2X_")
        .var(
            VarSpec::new(
                "A2X_REGISTRY_AUTH_DATA",
                Kind::Path,
                "Authentication data directory",
            )
            .rule(Rule::safe_path()),
        )
        .var(
            VarSpec::new(
                "A2X_FRONTEND_DIST_DIR",
                Kind::Path,
                "Built React UI to serve at /",
            )
            .rule(Rule::safe_path()),
        )
        .var(workers(
            "A2X_REGISTRY_SEARCH_WORKERS",
            "Concurrent /api/search requests",
            "4",
        ))
        .var(workers(
            "A2X_REGISTRY_DATASET_WORKERS",
            "Concurrent dataset operations and builds",
            "2",
        ))
        .var(workers(
            "A2X_REGISTRY_AGENT_CARD_WORKERS",
            "Concurrent agent-card fetches",
            "10",
        ));
    #[cfg(feature = "cluster")]
    let policy = policy.merge(a2x_cluster::env_policy());
    #[cfg(feature = "search")]
    let policy = policy.merge(a2x_search::env_policy());
    policy
}
