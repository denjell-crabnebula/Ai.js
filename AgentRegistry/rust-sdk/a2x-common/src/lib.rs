// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared building blocks for the A2X registry crates.
//!
//! Mirrors the `a2x_registry.common` Python package:
//!
//! - [`models`]: `SearchResult`, the record every search method returns
//! - [`errors`]: user-facing error type with actionable messages
//! - [`paths`]: runtime path resolution (`A2X_REGISTRY_HOME`, database dir)
//! - [`auth_context`]: neutral caller identity passed into the registry
//! - [`feature_flags`]: runtime availability of optional subsystems
//! - [`lease`]: generic TTL lease state machine (heartbeat and cluster)
//! - [`atomic`]: atomic JSON file writes
//! - [`llm_client`]: multi-provider OpenAI-compatible chat client
//! - [`evaluation`]: set metrics and result directory naming

pub mod atomic;
pub mod auth_context;
pub mod errors;
pub mod evaluation;
pub mod feature_flags;
pub mod lease;
pub mod llm_client;
pub mod models;
pub mod paths;

pub use auth_context::{AuthContext, Role};
pub use errors::A2xError;
pub use evaluation::{SetMetrics, compute_set_metrics, generate_output_dir};
pub use lease::{Lease, LeaseState, LeaseTable};
pub use llm_client::{
    ChatMessage, LlmBackend, LlmClient, LlmResponse, LlmStats, ProviderConfig, parse_json_response,
};
pub use models::SearchResult;

/// The environment variables this crate reads, with the values a safe
/// deployment accepts. Merge it into a binary's [`ap_support::env::EnvPolicy`].
pub fn env_policy() -> ap_support::env::EnvPolicy {
    use ap_support::env::{EnvPolicy, Kind, Rule, VarSpec};
    EnvPolicy::new()
        .prefix("A2X_")
        .var(
            VarSpec::new(
                paths::ENV_VAR,
                Kind::Path,
                "Registry home holding database/ and auth_data/",
            )
            .default("~/.a2x_registry")
            .rule(Rule::safe_path()),
        )
        .var(
            VarSpec::new(
                "A2X_REGISTRY_DISABLED_FEATURES",
                Kind::Text,
                "Features to disable, comma-separated",
            )
            .rule(Rule::list_of(&["vector", "evaluation"])),
        )
        .var(
            VarSpec::new("A2X_REGISTRY_LLM_WORKERS", Kind::Integer, "Concurrent LLM calls")
                .default("20")
                .rule(Rule::int_range(1, 1024)),
        )
        .var(
            VarSpec::new("A2X_REGISTRY_EMBEDDING_BACKEND", Kind::Text, "Embedding backend")
                .default("auto")
                .rule(Rule::one_of(&["auto", "hashing", "openai"])),
        )
}
