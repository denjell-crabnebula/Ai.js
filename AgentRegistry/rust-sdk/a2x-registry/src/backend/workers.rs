// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Concurrency limits from the environment (see `docs/environment.md`).
//!
//! Invalid values log a warning and fall back to the default, like the
//! Python module level `int(os.environ...)` guards.

use tokio::sync::Semaphore;

pub const DEFAULT_SEARCH_WORKERS: usize = 4;
pub const DEFAULT_DATASET_WORKERS: usize = 2;
pub const DEFAULT_LLM_WORKERS: usize = 20;
pub const DEFAULT_AGENT_CARD_WORKERS: usize = 10;

/// Read a positive integer from `var`, warning and falling back to
/// `default` on invalid input. Empty or unset means the default.
pub fn env_workers(var: &str, default: usize) -> usize {
    env_workers_in(ap_support::env::current(), var, default)
}

/// [`env_workers`] reading from `env`.
pub fn env_workers_in(env: &dyn ap_support::env::EnvSource, var: &str, default: usize) -> usize {
    match env.get(var) {
        Some(v) if !v.trim().is_empty() => match v.trim().parse::<i64>() {
            Ok(n) if n > 0 => n as usize,
            _ => {
                tracing::warn!("ignoring invalid {} (using {})", var, default);
                default
            }
        },
        _ => default,
    }
}

/// Semaphores bounding the request handling pools.
pub struct Workers {
    /// `/api/search` handling (`A2X_REGISTRY_SEARCH_WORKERS`).
    pub search: Semaphore,
    /// `/api/datasets` CRUD and build (`A2X_REGISTRY_DATASET_WORKERS`).
    pub dataset: Semaphore,
    /// LLM fan-out inside A2X navigation (`A2X_REGISTRY_LLM_WORKERS`).
    pub llm: usize,
    /// Agent card fetch fan-out (`A2X_REGISTRY_AGENT_CARD_WORKERS`).
    pub agent_card: usize,
}

impl Workers {
    /// Build from explicit counts (zero is raised to one).
    pub fn new(search: usize, dataset: usize, llm: usize, agent_card: usize) -> Self {
        Self {
            search: Semaphore::new(search.max(1)),
            dataset: Semaphore::new(dataset.max(1)),
            llm: llm.max(1),
            agent_card: agent_card.max(1),
        }
    }

    /// Build from the environment with the documented defaults.
    pub fn from_env() -> Self {
        Self::new(
            env_workers("A2X_REGISTRY_SEARCH_WORKERS", DEFAULT_SEARCH_WORKERS),
            env_workers("A2X_REGISTRY_DATASET_WORKERS", DEFAULT_DATASET_WORKERS),
            env_workers("A2X_REGISTRY_LLM_WORKERS", DEFAULT_LLM_WORKERS),
            env_workers("A2X_REGISTRY_AGENT_CARD_WORKERS", DEFAULT_AGENT_CARD_WORKERS),
        )
    }
}

impl Default for Workers {
    fn default() -> Self {
        Self::new(
            DEFAULT_SEARCH_WORKERS,
            DEFAULT_DATASET_WORKERS,
            DEFAULT_LLM_WORKERS,
            DEFAULT_AGENT_CARD_WORKERS,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn invalid_values_fall_back() -> TestResult {
        use ap_support::env::MapEnv;
        let var = "A2X_REGISTRY_TEST_WORKERS_X";
        assert_eq!(env_workers_in(&MapEnv::from([(var, "abc")]), var, 7), 7);
        assert_eq!(env_workers_in(&MapEnv::from([(var, "3")]), var, 7), 3);
        assert_eq!(env_workers_in(&MapEnv::from([(var, "")]), var, 7), 7);
        assert_eq!(env_workers_in(&MapEnv::new(), var, 7), 7);
        assert_eq!(Workers::new(0, 0, 0, 0).llm, 1);
        Ok(())
    }
}
