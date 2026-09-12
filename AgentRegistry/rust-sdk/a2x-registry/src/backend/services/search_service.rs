// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Unified search facade over the pluggable [`SearchEngine`].
//!
//! Owns the taxonomy availability check, provider switching, the LLM
//! relevance judge (a plain chat completion through `a2x-common`) and the
//! elapsed time accounting of the Python `SearchService`.

use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::{ChatMessage, LlmBackend, LlmClient, SearchResult};
use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::backend::engines::{DatasetPaths, EngineError, NavigationStep, SearchEngine, SearchMethod};
use crate::backend::schemas::{JudgeResult, SearchResponse};
use crate::register::{RegistryService, TaxonomyState};

/// Search facade shared by the routers and the startup warmup.
pub struct SearchService {
    engine: RwLock<Arc<dyn SearchEngine>>,
    registry: Arc<RegistryService>,
    llm_apikey_path: PathBuf,
    llm: Mutex<Option<Arc<dyn LlmBackend>>>,
    current_provider: Mutex<String>,
    llm_workers: usize,
}

impl SearchService {
    pub fn new(
        engine: Arc<dyn SearchEngine>,
        registry: Arc<RegistryService>,
        llm_apikey_path: PathBuf,
        llm_workers: usize,
    ) -> Self {
        Self {
            engine: RwLock::new(engine),
            registry,
            llm_apikey_path,
            llm: Mutex::new(None),
            current_provider: Mutex::new(String::new()),
            llm_workers: llm_workers.max(1),
        }
    }

    /// Replace the engine (used by tests and adapters).
    pub fn set_engine(&self, engine: Arc<dyn SearchEngine>) {
        *self.engine.write() = engine;
    }

    pub fn engine(&self) -> Arc<dyn SearchEngine> {
        self.engine.read().clone()
    }

    /// Inject a chat backend for the judge (tests).
    pub fn set_llm(&self, llm: Option<Arc<dyn LlmBackend>>) {
        *self.llm.lock() = llm;
    }

    // ── provider management ─────────────────────────────────────────────

    pub fn get_current_provider(&self) -> String {
        self.current_provider.lock().clone()
    }

    /// Switch the active provider and reset every LLM backed instance.
    pub fn switch_provider(&self, name: &str) {
        self.reset_a2x();
        *self.current_provider.lock() = name.to_string();
    }

    /// Clear cached A2X and LLM instances.
    pub fn reset_a2x(&self) {
        self.engine().reset();
        *self.llm.lock() = None;
    }

    /// Embedding model configured for a dataset.
    pub fn read_vector_config(&self, dataset: &str) -> String {
        self.registry
            .get_vector_config(dataset)
            .ok()
            .and_then(|c| {
                c.get("embedding_model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| crate::register::embedding::DEFAULT_EMBEDDING_MODEL.to_string())
    }

    /// Engine paths for a dataset.
    pub fn paths(&self, dataset: &str) -> DatasetPaths {
        DatasetPaths {
            dataset: dataset.to_string(),
            service_path: self.registry.service_json_path(dataset),
            query_path: self.registry.query_path(dataset),
            taxonomy_path: self.registry.taxonomy_path(dataset),
            class_path: self.registry.class_path(dataset),
            chroma_dir: self.registry.chroma_dir(),
            embedding_model: self.read_vector_config(dataset),
            llm_workers: self.llm_workers,
        }
    }

    /// Datasets that have both `service.json` and `query/query.json`.
    pub fn discover_datasets(&self) -> Vec<String> {
        self.registry
            .list_datasets_with_counts()
            .into_iter()
            .filter_map(|info| info.get("name").and_then(Value::as_str).map(str::to_string))
            .filter(|name| self.registry.query_path(name).exists())
            .collect()
    }

    /// Drop all search side state for a dataset.
    pub fn purge_dataset(&self, dataset: &str) {
        if !a2x_common::feature_flags::has(a2x_common::feature_flags::Feature::Vector) {
            return;
        }
        self.engine().purge_dataset(&self.paths(dataset));
    }

    /// Ask the engine to synchronize the vector index in the background.
    pub fn schedule_vector_sync(&self, dataset: &str) {
        if !a2x_common::feature_flags::has(a2x_common::feature_flags::Feature::Vector) {
            return;
        }
        self.engine().schedule_vector_sync(&self.paths(dataset));
    }

    fn check_taxonomy(&self, dataset: &str) -> Result<(), EngineError> {
        match self.registry.check_taxonomy_state(dataset) {
            None | Some(TaxonomyState::Available) | Some(TaxonomyState::Stale) => Ok(()),
            Some(TaxonomyState::Unavailable) => Err(EngineError::Invalid(format!(
                "Dataset '{dataset}' 的分类树已过时（services 已变更），请先重新 build 再搜索"
            ))),
            Some(TaxonomyState::Nonexistent) => Err(EngineError::Invalid(format!(
                "Dataset '{dataset}' 尚未构建分类树，请先运行 build"
            ))),
        }
    }

    fn llm(&self) -> Result<Arc<dyn LlmBackend>, EngineError> {
        let mut guard = self.llm.lock();
        if let Some(l) = guard.as_ref() {
            return Ok(l.clone());
        }
        let client = LlmClient::new(Some(&self.llm_apikey_path), Default::default())?;
        let arc: Arc<dyn LlmBackend> = Arc::new(client);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    // ── search ──────────────────────────────────────────────────────────

    /// Judge which services are relevant to `query` with the shared LLM.
    pub async fn judge_services(
        &self,
        query: &str,
        services: &[SearchResult],
    ) -> Result<Vec<JudgeResult>, EngineError> {
        let llm = self.llm()?;
        let svc_list: Vec<String> = services
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let desc: String = s.description.chars().take(200).collect();
                format!("{}. {}: {}", i + 1, s.name, desc)
            })
            .collect();
        let prompt = format!(
            "Given the user query, judge which services are relevant (can help fulfill any part of the query) and which are irrelevant.\n\n\
Query: {query}\n\nServices:\n{}\n\n\
Return ONLY the numbers of RELEVANT services, separated by commas (e.g. \"1,3,5\"). Return \"NONE\" if no service is relevant.",
            svc_list.join("\n")
        );
        let resp = llm.call(&[ChatMessage::user(prompt)], 0.0, Some(200)).await;
        let mut relevant: std::collections::HashSet<String> = std::collections::HashSet::new();
        if resp.content.trim().to_uppercase() != "NONE" {
            for part in resp.content.replace(',', " ").split_whitespace() {
                if let Ok(num) = part.trim().parse::<usize>() {
                    if num >= 1 && num <= services.len() {
                        relevant.insert(services[num - 1].id.clone());
                    }
                }
            }
        }
        Ok(services
            .iter()
            .map(|s| JudgeResult {
                service_id: s.id.clone(),
                relevant: relevant.contains(&s.id),
            })
            .collect())
    }

    /// Execute a search and return the unified response.
    pub async fn search(
        &self,
        query: &str,
        method: &str,
        dataset: &str,
        top_k: usize,
    ) -> Result<SearchResponse, EngineError> {
        let start = std::time::Instant::now();
        let parsed = SearchMethod::parse(method)
            .ok_or_else(|| EngineError::Invalid(format!("Unknown method: {method}")))?;
        if parsed.is_a2x() {
            self.check_taxonomy(dataset)?;
        }
        let top_k = match parsed {
            SearchMethod::Vector { top_k: Some(k) } => k,
            _ => top_k,
        };
        let paths = self.paths(dataset);
        let out = self.engine().search(parsed, query, top_k, &paths).await?;
        Ok(SearchResponse {
            results: out.results,
            stats: out.stats,
            elapsed_time: round2(start.elapsed().as_secs_f64()),
        })
    }

    /// Streaming A2X search: forwards navigation steps to `steps` and
    /// returns the final response with `elapsed_time`.
    pub async fn search_stream(
        &self,
        query: &str,
        method: &str,
        dataset: &str,
        steps: mpsc::Sender<NavigationStep>,
    ) -> Result<SearchResponse, EngineError> {
        self.check_taxonomy(dataset)?;
        let start = std::time::Instant::now();
        let mode = match SearchMethod::parse(method) {
            Some(SearchMethod::A2x(mode)) => mode,
            _ => return Err(EngineError::Invalid(format!("Unknown method: {method}"))),
        };
        let paths = self.paths(dataset);
        let out = self.engine().search_stream(mode, query, &paths, steps).await?;
        Ok(SearchResponse {
            results: out.results,
            stats: out.stats,
            elapsed_time: round2(start.elapsed().as_secs_f64()),
        })
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The `{"type":"result", ...}` message of the WebSocket stream.
pub fn result_message(resp: &SearchResponse) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("result".into()));
    m.insert("results".into(), json!(resp.results));
    m.insert("stats".into(), Value::Object(resp.stats.clone()));
    m.insert("elapsed_time".into(), json!(resp.elapsed_time));
    Value::Object(m)
}

/// The `{"type":"step", ...}` message of the WebSocket stream.
pub fn step_message(step: &NavigationStep) -> Value {
    json!({
        "type": "step",
        "parent_id": step.parent_id,
        "selected": step.selected,
        "pruned": step.pruned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2x_common::{LlmResponse, LlmStats};
    use ap_support::testing::{ResultExt, TestResult};
    use async_trait::async_trait;

    struct FakeLlm(String);

    #[async_trait]
    impl LlmBackend for FakeLlm {
        async fn call(&self, _m: &[ChatMessage], _t: f32, _mt: Option<u32>) -> LlmResponse {
            LlmResponse {
                content: self.0.clone(),
                success: true,
                ..Default::default()
            }
        }
        fn model(&self) -> String {
            "fake".into()
        }
        fn stats(&self) -> LlmStats {
            LlmStats::default()
        }
        fn reset_stats(&self) {}
    }

    fn service(tmp: &tempfile::TempDir) -> SearchService {
        let registry = Arc::new(RegistryService::new(tmp.path().join("database"), None));
        SearchService::new(
            Arc::new(crate::backend::engines::UnavailableEngine),
            registry,
            tmp.path().join("llm_apikey.json"),
            4,
        )
    }

    #[tokio::test]
    async fn judge_parses_numbers() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let s = service(&tmp);
        let services = vec![
            SearchResult::new("a", "A", "x"),
            SearchResult::new("b", "B", "y"),
            SearchResult::new("c", "C", "z"),
        ];
        s.set_llm(Some(Arc::new(FakeLlm("1, 3, 9".into()))));
        let r = s.judge_services("q", &services).await?;
        assert_eq!(
            r.iter().map(|j| j.relevant).collect::<Vec<_>>(),
            vec![true, false, true]
        );
        s.set_llm(Some(Arc::new(FakeLlm("NONE".into()))));
        assert!(
            s.judge_services("q", &services)
                .await?
                .iter()
                .all(|j| !j.relevant)
        );
        Ok(())
    }

    #[tokio::test]
    async fn judge_without_config_is_llm_not_configured() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let s = service(&tmp);
        let e = s.judge_services("q", &[]).await.err_or_fail()?;
        assert!(matches!(e, EngineError::LlmNotConfigured(ref m) if m.contains("llm_apikey.json")));
        Ok(())
    }

    #[tokio::test]
    async fn search_dispatch_and_errors() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let s = service(&tmp);
        assert!(matches!(
            s.search("q", "bogus", "ds", 5).await,
            Err(EngineError::Invalid(_))
        ));
        assert!(matches!(
            s.search("q", "vector", "ds", 5).await,
            Err(EngineError::FeatureNotInstalled { .. })
        ));
        s.switch_provider("p");
        assert_eq!(s.get_current_provider(), "p");
        Ok(())
    }
}
