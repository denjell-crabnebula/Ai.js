// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Search and build engine abstractions.
//!
//! The A2X, vector and traditional engines live in the `a2x-search` crate.
//! The backend depends only on these narrow traits so it compiles and runs
//! without that crate; [`UnavailableEngine`] answers every call with the
//! structured 503 `FeatureNotInstalled` body.

use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::SearchResult;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::register::BuildRequest;

/// Failure of an engine call.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    /// The engine or a required subsystem is not available (HTTP 503).
    #[error("{detail}")]
    FeatureNotInstalled {
        feature: String,
        extras: String,
        detail: String,
    },
    /// `llm_apikey.json` is missing or invalid (HTTP 503).
    #[error("{0}")]
    LlmNotConfigured(String),
    /// Bad input such as an unknown method (HTTP 500, like the Python
    /// unhandled `ValueError`).
    #[error("{0}")]
    Invalid(String),
    /// The build was cancelled through its token.
    #[error("build cancelled")]
    Cancelled,
    /// Any other failure; the message becomes the build error status.
    #[error("{0}")]
    Failed(String),
}

impl EngineError {
    /// The 503 body for an engine that is compiled out.
    pub fn unavailable(feature: &str) -> Self {
        let err = a2x_common::A2xError::feature_not_installed(feature, feature);
        EngineError::FeatureNotInstalled {
            feature: feature.to_string(),
            extras: feature.to_string(),
            detail: err.to_string(),
        }
    }
}

impl From<a2x_common::A2xError> for EngineError {
    fn from(e: a2x_common::A2xError) -> Self {
        match &e {
            a2x_common::A2xError::FeatureNotInstalled { feature, extras, .. } => {
                EngineError::FeatureNotInstalled {
                    feature: feature.clone(),
                    extras: extras.clone(),
                    detail: e.to_string(),
                }
            }
            a2x_common::A2xError::LlmNotConfigured(m) => EngineError::LlmNotConfigured(m.clone()),
            other => EngineError::Failed(other.to_string()),
        }
    }
}

/// A2X navigation mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum A2xMode {
    GetAll,
    GetImportant,
    GetOne,
}

impl A2xMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            A2xMode::GetAll => "get_all",
            A2xMode::GetImportant => "get_important",
            A2xMode::GetOne => "get_one",
        }
    }
}

/// Parsed `method` string from a search request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchMethod {
    A2x(A2xMode),
    /// `vector` (top_k from the request) or `vector_N` (top_k = N).
    Vector {
        top_k: Option<usize>,
    },
    Traditional,
}

impl SearchMethod {
    /// Parse `a2x_get_all | a2x_get_important | a2x_get_one | vector |
    /// vector_N | traditional`.
    pub fn parse(method: &str) -> Option<Self> {
        match method {
            "a2x_get_all" => Some(SearchMethod::A2x(A2xMode::GetAll)),
            "a2x_get_important" => Some(SearchMethod::A2x(A2xMode::GetImportant)),
            "a2x_get_one" => Some(SearchMethod::A2x(A2xMode::GetOne)),
            "vector" => Some(SearchMethod::Vector { top_k: None }),
            "traditional" => Some(SearchMethod::Traditional),
            other => other
                .strip_prefix("vector_")
                .and_then(|n| n.parse::<usize>().ok())
                .map(|k| SearchMethod::Vector { top_k: Some(k) }),
        }
    }

    pub fn is_a2x(&self) -> bool {
        matches!(self, SearchMethod::A2x(_))
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, SearchMethod::Vector { .. })
    }
}

/// File locations an engine needs for one dataset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetPaths {
    pub dataset: String,
    pub service_path: PathBuf,
    pub query_path: PathBuf,
    pub taxonomy_path: PathBuf,
    pub class_path: PathBuf,
    /// Shared ChromaDB directory (one per database root).
    pub chroma_dir: PathBuf,
    /// Embedding model name from `vector_config.json`.
    pub embedding_model: String,
    /// LLM fan-out bound (`A2X_REGISTRY_LLM_WORKERS`).
    pub llm_workers: usize,
}

/// Results and statistics of one search call. `stats` is rendered as is
/// (`llm_calls`, `total_tokens`, and for A2X `visited_categories` and
/// `pruned_categories`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineSearchOutput {
    pub results: Vec<SearchResult>,
    pub stats: Map<String, Value>,
}

/// One A2X navigation step streamed over the WebSocket.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NavigationStep {
    pub parent_id: String,
    pub selected: Vec<String>,
    pub pruned: Vec<String>,
}

/// Search engine over A2X, vector and traditional methods.
#[async_trait]
pub trait SearchEngine: Send + Sync {
    /// Run a search and return the results plus stats.
    async fn search(
        &self,
        method: SearchMethod,
        query: &str,
        top_k: usize,
        paths: &DatasetPaths,
    ) -> Result<EngineSearchOutput, EngineError>;

    /// Run an A2X search, sending each navigation step to `steps` before
    /// returning the final output.
    async fn search_stream(
        &self,
        mode: A2xMode,
        query: &str,
        paths: &DatasetPaths,
        steps: mpsc::Sender<NavigationStep>,
    ) -> Result<EngineSearchOutput, EngineError>;

    /// Drop cached LLM backed instances (after a provider switch).
    fn reset(&self) {}

    /// Drop every cached instance and index for a dataset.
    fn purge_dataset(&self, _paths: &DatasetPaths) {}

    /// Synchronize the vector index with `service.json` in the background.
    fn schedule_vector_sync(&self, _paths: &DatasetPaths) {}

    /// Warm caches for the given datasets during startup.
    async fn warmup(&self, _datasets: &[DatasetPaths]) {}
}

/// Receives log lines from a running build.
pub trait BuildLogSink: Send + Sync {
    fn log(&self, line: &str);
}

/// Everything a build engine needs for one job.
pub struct BuildContext {
    pub dataset: String,
    pub paths: DatasetPaths,
    pub request: BuildRequest,
    pub log: Arc<dyn BuildLogSink>,
    pub cancel: CancellationToken,
}

/// Taxonomy build engine.
#[async_trait]
pub trait BuildEngine: Send + Sync {
    /// Run the build to completion. `Err(EngineError::Cancelled)` when the
    /// token fired; any other error becomes the job's error message.
    async fn build(&self, ctx: BuildContext) -> Result<(), EngineError>;
}

/// Engine used when the `a2x-search` crate is not wired in. Every call
/// fails with the structured 503 `FeatureNotInstalled` body.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableEngine;

#[async_trait]
impl SearchEngine for UnavailableEngine {
    async fn search(
        &self,
        method: SearchMethod,
        _query: &str,
        _top_k: usize,
        _paths: &DatasetPaths,
    ) -> Result<EngineSearchOutput, EngineError> {
        Err(EngineError::unavailable(if method.is_vector() {
            "vector"
        } else {
            "search"
        }))
    }

    async fn search_stream(
        &self,
        _mode: A2xMode,
        _query: &str,
        _paths: &DatasetPaths,
        _steps: mpsc::Sender<NavigationStep>,
    ) -> Result<EngineSearchOutput, EngineError> {
        Err(EngineError::unavailable("search"))
    }
}

#[async_trait]
impl BuildEngine for UnavailableEngine {
    async fn build(&self, _ctx: BuildContext) -> Result<(), EngineError> {
        Err(EngineError::unavailable("search"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn parse_methods() -> TestResult {
        assert_eq!(
            SearchMethod::parse("vector_5"),
            Some(SearchMethod::Vector { top_k: Some(5) })
        );
        assert_eq!(
            SearchMethod::parse("vector"),
            Some(SearchMethod::Vector { top_k: None })
        );
        assert_eq!(
            SearchMethod::parse("a2x_get_one"),
            Some(SearchMethod::A2x(A2xMode::GetOne))
        );
        assert_eq!(
            SearchMethod::parse("traditional"),
            Some(SearchMethod::Traditional)
        );
        assert_eq!(SearchMethod::parse("vector_x"), None);
        assert_eq!(SearchMethod::parse("a2x_bogus"), None);
        Ok(())
    }

    #[tokio::test]
    async fn unavailable_engine_returns_503_shape() -> TestResult {
        let paths = DatasetPaths {
            dataset: "d".into(),
            service_path: "s".into(),
            query_path: "q".into(),
            taxonomy_path: "t".into(),
            class_path: "c".into(),
            chroma_dir: "ch".into(),
            embedding_model: "m".into(),
            llm_workers: 1,
        };
        let e = UnavailableEngine
            .search(SearchMethod::Vector { top_k: None }, "q", 5, &paths)
            .await
            .err_or_fail()?;
        assert!(matches!(e, EngineError::FeatureNotInstalled { ref feature, .. } if feature == "vector"));
        Ok(())
    }
}
