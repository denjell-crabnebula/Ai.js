// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Small helpers shared across modules.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::error::{Error, Result};

/// Read and parse a JSON file.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| Error::json(path, e))
}

/// Write pretty JSON atomically, creating the parent directory.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
    }
    a2x_common::atomic::atomic_write_json(path, value).map_err(|e| Error::io(path, e))
}

/// Run `jobs` on tokio tasks with at most `workers` running at once.
/// `on_done` is called for each result in completion order, like
/// `concurrent.futures.as_completed`.
pub async fn run_bounded<T, Fut, F>(workers: usize, jobs: Vec<Fut>, mut on_done: F)
where
    T: Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    F: FnMut(T),
{
    let semaphore = Arc::new(Semaphore::new(workers.max(1)));
    let mut set = JoinSet::new();
    for job in jobs {
        let semaphore = semaphore.clone();
        set.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            job.await
        });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(value) => on_done(value),
            Err(e) => tracing::error!("worker task failed: {e}"),
        }
    }
}

/// Truncate `text` to `max_chars` characters and append `...` when cut.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}...")
    } else {
        text.to_string()
    }
}

/// Parse `A2X_REGISTRY_LLM_WORKERS`, falling back to `default` when unset
/// or invalid. Mirrors the backend's `_LLM_WORKERS` resolution.
pub fn llm_workers_from_env(default: usize) -> usize {
    llm_workers_from_env_in(ap_support::env::current(), default)
}

/// [`llm_workers_from_env`] reading from `env`.
pub fn llm_workers_from_env_in(env: &dyn ap_support::env::EnvSource, default: usize) -> usize {
    match env.get("A2X_REGISTRY_LLM_WORKERS") {
        Some(v) if !v.is_empty() => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!("ignoring invalid A2X_REGISTRY_LLM_WORKERS (using {default})");
                default
            }
        },
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[tokio::test]
    async fn bounded_runs_all_jobs() -> TestResult {
        let jobs: Vec<_> = (0..25u32).map(|i| async move { i * 2 }).collect();
        let mut seen = Vec::new();
        run_bounded(4, jobs, |v| seen.push(v)).await;
        seen.sort();
        assert_eq!(seen, (0..25u32).map(|i| i * 2).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn truncation_counts_characters() -> TestResult {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc...");
        assert_eq!(truncate_chars("中文字符串", 2), "中文...");
        Ok(())
    }
}
