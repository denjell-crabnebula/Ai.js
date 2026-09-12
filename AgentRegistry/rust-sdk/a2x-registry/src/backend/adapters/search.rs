// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Adapter from the `a2x-search` crate to [`SearchEngine`] and
//! [`BuildEngine`] (feature `search`).
//!
//! Mirrors the Python `SearchService` engine cache: one `A2xSearch` per
//! `(dataset, mode)`, one `TraditionalSearch` and one `VectorSearch` per
//! dataset, one embedding model per name, and one shared LLM client that is
//! dropped on provider switch.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::{A2xError, LlmBackend, LlmClient};
use a2x_search::build::LogLevel;
use a2x_search::vector::{
    EmbeddingBackend, EmbeddingModel, VectorSearch, VectorSearchConfig, VectorStore,
    collection_name_for_dataset, resolve_embedding_model,
};
use a2x_search::{
    A2xSearch, A2xSearchConfig, AutoHierarchicalConfig, BuildEvent, BuildSink, ResumeMode, SearchMode,
    StreamMessage, TaxonomyBuilder, TraditionalSearch,
};
use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::backend::engines::{
    A2xMode, BuildContext, BuildEngine, DatasetPaths, EngineError, EngineSearchOutput, NavigationStep,
    SearchEngine, SearchMethod,
};
use crate::backend::state::AppConfig;
use crate::register::BuildRequest;

/// Environment variable selecting the embedding backend
/// (`auto`, `hashing`, `openai`).
pub const ENV_EMBEDDING_BACKEND: &str = "A2X_REGISTRY_EMBEDDING_BACKEND";

fn map_err(e: a2x_search::Error) -> EngineError {
    match e {
        a2x_search::Error::Cancelled => EngineError::Cancelled,
        a2x_search::Error::Invalid(m) => EngineError::Invalid(m),
        a2x_search::Error::Common(A2xError::VectorSearchUnavailable(m)) => EngineError::FeatureNotInstalled {
            feature: "vector".into(),
            extras: "vector".into(),
            detail: m,
        },
        a2x_search::Error::Common(c) => EngineError::from(c),
        other => EngineError::Failed(other.to_string()),
    }
}

fn mode_of(mode: A2xMode) -> SearchMode {
    match mode {
        A2xMode::GetAll => SearchMode::GetAll,
        A2xMode::GetImportant => SearchMode::GetImportant,
        A2xMode::GetOne => SearchMode::GetOne,
    }
}

/// The embedding backend named by `A2X_REGISTRY_EMBEDDING_BACKEND` in `env`,
/// or `Auto` when unset or invalid.
pub fn embedding_backend_from(env: &dyn ap_support::env::EnvSource) -> EmbeddingBackend {
    match env.get(ENV_EMBEDDING_BACKEND) {
        Some(v) if !v.trim().is_empty() => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!("ignoring invalid {} (using auto)", ENV_EMBEDDING_BACKEND);
            EmbeddingBackend::Auto
        }),
        _ => EmbeddingBackend::Auto,
    }
}

/// Engine backed by the `a2x-search` crate.
pub struct A2xEngine {
    llm_apikey_path: PathBuf,
    llm: Mutex<Option<Arc<dyn LlmBackend>>>,
    a2x: Mutex<HashMap<String, Arc<A2xSearch>>>,
    traditional: Mutex<HashMap<String, Arc<TraditionalSearch>>>,
    vector: Mutex<HashMap<String, Arc<VectorSearch>>>,
    embeddings: Mutex<HashMap<String, Arc<dyn EmbeddingModel>>>,
    embedding_backend: EmbeddingBackend,
}

impl A2xEngine {
    /// An engine whose embedding backend comes from `A2X_REGISTRY_EMBEDDING_BACKEND`.
    pub fn new(llm_apikey_path: impl Into<PathBuf>) -> Self {
        Self::with_embedding_backend(
            llm_apikey_path,
            embedding_backend_from(ap_support::env::current()),
        )
    }

    /// An engine with an explicit embedding backend.
    pub fn with_embedding_backend(
        llm_apikey_path: impl Into<PathBuf>,
        embedding_backend: EmbeddingBackend,
    ) -> Self {
        Self {
            llm_apikey_path: llm_apikey_path.into(),
            llm: Mutex::new(None),
            a2x: Mutex::new(HashMap::new()),
            traditional: Mutex::new(HashMap::new()),
            vector: Mutex::new(HashMap::new()),
            embeddings: Mutex::new(HashMap::new()),
            embedding_backend,
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

    fn a2x_instance(&self, mode: A2xMode, paths: &DatasetPaths) -> Result<Arc<A2xSearch>, EngineError> {
        let key = format!("{}_{}", paths.dataset, mode.as_str());
        if let Some(s) = self.a2x.lock().get(&key) {
            return Ok(s.clone());
        }
        let llm = self.llm()?;
        let config = A2xSearchConfig {
            taxonomy_path: paths.taxonomy_path.clone(),
            class_path: paths.class_path.clone(),
            service_path: paths.service_path.clone(),
            max_workers: paths.llm_workers.max(1),
            parallel: true,
            mode: mode_of(mode),
        };
        let searcher = Arc::new(A2xSearch::new(config, llm).map_err(map_err)?);
        self.a2x.lock().insert(key, searcher.clone());
        Ok(searcher)
    }

    fn traditional_instance(&self, paths: &DatasetPaths) -> Result<Arc<TraditionalSearch>, EngineError> {
        if let Some(s) = self.traditional.lock().get(&paths.dataset) {
            return Ok(s.clone());
        }
        let llm = self.llm()?;
        let searcher = Arc::new(TraditionalSearch::new(&paths.service_path, llm).map_err(map_err)?);
        self.traditional
            .lock()
            .insert(paths.dataset.clone(), searcher.clone());
        Ok(searcher)
    }

    fn embedding_model(&self, name: &str) -> Result<Arc<dyn EmbeddingModel>, EngineError> {
        if let Some(m) = self.embeddings.lock().get(name) {
            return Ok(m.clone());
        }
        let model = resolve_embedding_model(name, self.embedding_backend).map_err(map_err)?;
        self.embeddings.lock().insert(name.to_string(), model.clone());
        Ok(model)
    }

    async fn vector_instance(&self, paths: &DatasetPaths) -> Result<Arc<VectorSearch>, EngineError> {
        if let Some(s) = self.vector.lock().get(&paths.dataset) {
            return Ok(s.clone());
        }
        let model = self.embedding_model(&paths.embedding_model)?;
        let config = VectorSearchConfig {
            service_path: paths.service_path.clone(),
            collection_name: collection_name_for_dataset(&paths.dataset),
            persist_dir: paths.chroma_dir.clone(),
            model_name: paths.embedding_model.clone(),
            force_rebuild: false,
        };
        let searcher = Arc::new(VectorSearch::new(config, model).await.map_err(map_err)?);
        self.vector.lock().insert(paths.dataset.clone(), searcher.clone());
        Ok(searcher)
    }

    /// Incrementally sync the vector store with `service.json` (port of
    /// `SearchService.sync_vector`).
    pub async fn sync_vector(&self, paths: &DatasetPaths) -> Result<(), EngineError> {
        if !paths.service_path.exists() {
            tracing::warn!("sync_vector: service.json not found for {}", paths.dataset);
            return Ok(());
        }
        let text =
            std::fs::read_to_string(&paths.service_path).map_err(|e| EngineError::Failed(e.to_string()))?;
        let services: Vec<Value> =
            serde_json::from_str(&text).map_err(|e| EngineError::Failed(e.to_string()))?;
        let target: IndexMap<String, String> = services
            .iter()
            .filter_map(|s| {
                s.get("id").and_then(Value::as_str).map(|id| {
                    (
                        id.to_string(),
                        s.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    )
                })
            })
            .collect();
        if target.is_empty() {
            tracing::info!("sync_vector: no services in {}, skipping", paths.dataset);
            return Ok(());
        }
        let model_name = paths.embedding_model.clone();
        let collection = collection_name_for_dataset(&paths.dataset);
        let mut store =
            VectorStore::open(&collection, &paths.chroma_dir, Some(&model_name)).map_err(map_err)?;
        if let Some(stored) = store.stored_embedding_model() {
            if stored != model_name {
                tracing::info!(
                    "sync_vector: model changed {} -> {} for {}, full rebuild",
                    stored,
                    model_name,
                    paths.dataset
                );
                store.clear().map_err(map_err)?;
                store =
                    VectorStore::open(&collection, &paths.chroma_dir, Some(&model_name)).map_err(map_err)?;
            }
        }
        let existing = store.get_all_docs();
        let to_delete: Vec<String> = existing
            .keys()
            .filter(|id| !target.contains_key(*id))
            .cloned()
            .collect();
        let to_upsert: Vec<(String, String)> = target
            .iter()
            .filter(|(id, desc)| existing.get(*id) != Some(desc))
            .map(|(id, desc)| (id.clone(), desc.clone()))
            .collect();
        if to_delete.is_empty() && to_upsert.is_empty() {
            tracing::info!(
                "sync_vector: {} already up-to-date ({} docs)",
                paths.dataset,
                target.len()
            );
            return Ok(());
        }
        if !to_delete.is_empty() {
            store.delete_ids(&to_delete).map_err(map_err)?;
            tracing::info!(
                "sync_vector: deleted {} docs from {}",
                to_delete.len(),
                paths.dataset
            );
        }
        if !to_upsert.is_empty() {
            let model = self.embedding_model(&model_name)?;
            let ids: Vec<String> = to_upsert.iter().map(|(id, _)| id.clone()).collect();
            let texts: Vec<String> = to_upsert.iter().map(|(_, t)| t.clone()).collect();
            let embeddings = model.embed(&texts).await.map_err(map_err)?;
            store.upsert(&ids, &texts, &embeddings).map_err(map_err)?;
            tracing::info!(
                "sync_vector: upserted {} docs in {}",
                to_upsert.len(),
                paths.dataset
            );
        }
        self.vector.lock().remove(&paths.dataset);
        Ok(())
    }
}

fn a2x_stats(llm_calls: u64, total_tokens: u64, visited: usize, pruned: usize) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("llm_calls".into(), json!(llm_calls));
    m.insert("total_tokens".into(), json!(total_tokens));
    m.insert("visited_categories".into(), json!(visited));
    m.insert("pruned_categories".into(), json!(pruned));
    m
}

fn flat_stats(llm_calls: u64, total_tokens: u64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("llm_calls".into(), json!(llm_calls));
    m.insert("total_tokens".into(), json!(total_tokens));
    m
}

#[async_trait]
impl SearchEngine for A2xEngine {
    async fn search(
        &self,
        method: SearchMethod,
        query: &str,
        top_k: usize,
        paths: &DatasetPaths,
    ) -> Result<EngineSearchOutput, EngineError> {
        match method {
            SearchMethod::A2x(mode) => {
                let searcher = self.a2x_instance(mode, paths)?;
                let (results, stats) = searcher.search(query).await;
                Ok(EngineSearchOutput {
                    results,
                    stats: a2x_stats(
                        stats.llm_calls,
                        stats.total_tokens,
                        stats.visited_categories.len(),
                        stats.pruned_categories.len(),
                    ),
                })
            }
            SearchMethod::Vector { .. } => {
                let searcher = self.vector_instance(paths).await?;
                let (results, _stats) = searcher.search(query, top_k).await.map_err(map_err)?;
                Ok(EngineSearchOutput {
                    results,
                    stats: flat_stats(0, 0),
                })
            }
            SearchMethod::Traditional => {
                let searcher = self.traditional_instance(paths)?;
                let (results, stats) = searcher.search(query).await;
                Ok(EngineSearchOutput {
                    results,
                    stats: flat_stats(stats.llm_calls, stats.total_tokens),
                })
            }
        }
    }

    async fn search_stream(
        &self,
        mode: A2xMode,
        query: &str,
        paths: &DatasetPaths,
        steps: mpsc::Sender<NavigationStep>,
    ) -> Result<EngineSearchOutput, EngineError> {
        let searcher = self.a2x_instance(mode, paths)?;
        let mut rx = searcher.search_streaming(query.to_string());
        while let Some(msg) = rx.recv().await {
            match msg {
                StreamMessage::Step {
                    parent_id,
                    selected,
                    pruned,
                } => {
                    let _ = steps
                        .send(NavigationStep {
                            parent_id,
                            selected,
                            pruned,
                        })
                        .await;
                }
                StreamMessage::Result { results, stats } => {
                    return Ok(EngineSearchOutput {
                        results,
                        stats: a2x_stats(
                            stats.llm_calls,
                            stats.total_tokens,
                            stats.visited_categories,
                            stats.pruned_categories,
                        ),
                    });
                }
            }
        }
        Err(EngineError::Failed(
            "streaming search ended without a result".into(),
        ))
    }

    fn reset(&self) {
        self.a2x.lock().clear();
        *self.llm.lock() = None;
    }

    fn purge_dataset(&self, paths: &DatasetPaths) {
        let collection = collection_name_for_dataset(&paths.dataset);
        match VectorStore::delete_collection(&collection, &paths.chroma_dir) {
            Ok(_) => tracing::info!("Cleared vector collection: {}", collection),
            Err(e) => tracing::warn!("Failed to clear vector store for {}: {}", paths.dataset, e),
        }
        self.vector.lock().remove(&paths.dataset);
        let prefix = format!("{}_", paths.dataset);
        self.a2x.lock().retain(|k, _| !k.starts_with(&prefix));
        self.traditional.lock().remove(&paths.dataset);
    }

    fn schedule_vector_sync(&self, paths: &DatasetPaths) {
        let paths = paths.clone();
        let engine = self.clone_handle();
        tokio::spawn(async move {
            if let Err(e) = engine.sync_vector(&paths).await {
                tracing::error!("sync_vector failed for {}: {}", paths.dataset, e);
            }
        });
    }

    async fn warmup(&self, datasets: &[DatasetPaths]) {
        for paths in datasets {
            if paths.taxonomy_path.exists() {
                for mode in [A2xMode::GetOne, A2xMode::GetAll, A2xMode::GetImportant] {
                    match self.a2x_instance(mode, paths) {
                        Ok(_) => tracing::info!("  A2X {} ready: {}", mode.as_str(), paths.dataset),
                        Err(e) => {
                            tracing::warn!("  A2X {} failed for {}: {}", mode.as_str(), paths.dataset, e)
                        }
                    }
                }
            }
            if a2x_common::feature_flags::has(a2x_common::feature_flags::Feature::Vector) {
                match self.sync_vector(paths).await {
                    Ok(()) => tracing::info!("  Vector sync done: {}", paths.dataset),
                    Err(e) => tracing::warn!("  Vector sync failed for {}: {}", paths.dataset, e),
                }
            }
        }
    }
}

impl A2xEngine {
    fn clone_handle(&self) -> Arc<A2xEngine> {
        // The engine is always held in an `Arc` by the application state;
        // background sync uses a fresh engine sharing the same on-disk state.
        Arc::new(A2xEngine::new(self.llm_apikey_path.clone()))
    }
}

fn level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Info => 1,
        LogLevel::Warning => 2,
        LogLevel::Error => 3,
    }
}

fn requested_rank(log_level: Option<&str>) -> u8 {
    match log_level.map(|s| s.to_ascii_uppercase()).as_deref() {
        Some("WARNING") | Some("WARN") => 2,
        Some("ERROR") => 3,
        _ => 1,
    }
}

/// Apply the optional `BuildRequest` overrides onto a config.
pub fn apply_build_request(config: &mut AutoHierarchicalConfig, req: &BuildRequest) {
    if let Some(v) = req.generic_ratio {
        config.generic_ratio = v;
    }
    if let Some(v) = req.delete_threshold {
        config.delete_threshold = v.max(0) as usize;
    }
    if let Some(v) = req.max_service_size {
        config.max_service_size = v.max(0) as usize;
    }
    if let Some(v) = req.max_categories_size {
        config.max_categories_size = v.max(0) as usize;
    }
    if let Some(v) = req.max_depth {
        config.max_depth = if v <= 0 { None } else { Some(v as u32) };
    }
    if let Some(v) = req.min_leaf_size {
        config.min_leaf_size = v.max(0) as usize;
    }
    if let Some(v) = req.keyword_batch_size {
        config.keyword_batch_size = v.max(0) as usize;
    }
    if let Some(v) = req.max_keywords_per_service {
        config.max_keywords_per_service = v.max(0) as usize;
    }
    if let Some(v) = req.keyword_threshold {
        config.keyword_threshold = v.max(0) as usize;
    }
    if let Some(v) = req.classification_retries {
        config.classification_retries = v.max(0) as u32;
    }
    if let Some(v) = req.max_refine_iterations {
        config.max_refine_iterations = v.max(0) as usize;
    }
    if let Some(v) = req.temperature_keywords {
        config.temperature_keywords = v as f32;
    }
    if let Some(v) = req.temperature_design {
        config.temperature_design = v as f32;
    }
    if let Some(v) = req.temperature_classify {
        config.temperature_classify = v as f32;
    }
    if let Some(v) = req.max_tokens_design {
        config.max_tokens_design = v.max(0) as u32;
    }
    if let Some(v) = req.max_tokens_design_small {
        config.max_tokens_design_small = v.max(0) as u32;
    }
    if let Some(v) = req.max_tokens_classify {
        config.max_tokens_classify = v.max(0) as u32;
    }
    if let Some(v) = req.max_tokens_validate {
        config.max_tokens_validate = v.max(0) as u32;
    }
    if let Some(v) = req.max_tokens_keywords {
        config.max_tokens_keywords = v.max(0) as u32;
    }
    if let Some(v) = req.enable_cross_domain {
        config.enable_cross_domain = v;
    }
    if let Some(v) = req.workers {
        config.workers = v.max(1) as usize;
    }
}

#[async_trait]
impl BuildEngine for A2xEngine {
    async fn build(&self, ctx: BuildContext) -> Result<(), EngineError> {
        let llm = self.llm()?;
        let resume: ResumeMode = ctx.request.resume.parse().map_err(map_err)?;
        let mut config = AutoHierarchicalConfig::new(ctx.paths.service_path.clone());
        if let Some(dir) = ctx.paths.taxonomy_path.parent() {
            config.output_dir = dir.to_path_buf();
        }
        apply_build_request(&mut config, &ctx.request);
        let min_rank = requested_rank(ctx.request.log_level.as_deref());
        let log = ctx.log.clone();
        let sink = BuildSink::new(move |event: BuildEvent| {
            let rank = match &event {
                BuildEvent::Log { level, .. } => level_rank(*level),
                _ => 1,
            };
            if rank >= min_rank {
                log.log(event.message());
            }
        });
        let mut builder = TaxonomyBuilder::new(config, llm);
        builder
            .build(resume, sink, ctx.cancel)
            .await
            .map(|_| ())
            .map_err(map_err)?;
        // Drop cached A2X instances so the next search reloads the tree.
        let prefix = format!("{}_", ctx.dataset);
        self.a2x.lock().retain(|k, _| !k.starts_with(&prefix));
        Ok(())
    }
}

/// Install the real engines on an application config.
pub fn configure(mut cfg: AppConfig) -> AppConfig {
    let engine = Arc::new(A2xEngine::new(cfg.llm_apikey_path.clone()));
    cfg.search_engine = engine.clone();
    cfg.build_engine = engine;
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn build_request_overrides() -> TestResult {
        let mut config = AutoHierarchicalConfig::new("database/ds/service.json");
        let req: BuildRequest = serde_json::from_value(
            json!({"workers": 3, "max_depth": 0, "generic_ratio": 0.5, "enable_cross_domain": false}),
        )?;
        apply_build_request(&mut config, &req);
        assert_eq!(config.workers, 3);
        assert_eq!(config.max_depth, None);
        assert_eq!(config.generic_ratio, 0.5);
        assert!(!config.enable_cross_domain);
        assert_eq!(requested_rank(Some("warning")), 2);
        assert_eq!(requested_rank(None), 1);
        Ok(())
    }

    #[tokio::test]
    async fn unconfigured_llm_is_reported() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let engine = A2xEngine::new(tmp.path().join("llm_apikey.json"));
        let paths = DatasetPaths {
            dataset: "ds".into(),
            service_path: tmp.path().join("service.json"),
            query_path: tmp.path().join("q.json"),
            taxonomy_path: tmp.path().join("taxonomy/taxonomy.json"),
            class_path: tmp.path().join("taxonomy/class.json"),
            chroma_dir: tmp.path().join("chroma"),
            embedding_model: "all-MiniLM-L6-v2".into(),
            llm_workers: 2,
        };
        let e = engine
            .search(SearchMethod::Traditional, "q", 5, &paths)
            .await
            .err_or_fail()?;
        assert!(matches!(e, EngineError::LlmNotConfigured(_)));
        Ok(())
    }

    #[tokio::test]
    async fn vector_sync_with_hashing_backend() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::write(
            tmp.path().join("service.json"),
            json!([{"id": "a", "name": "A", "description": "book flights"}, {"id": "b", "name": "B", "description": "cook food"}]).to_string(),
        )
        ?;
        let engine =
            A2xEngine::with_embedding_backend(tmp.path().join("llm_apikey.json"), EmbeddingBackend::Hashing);
        let paths = DatasetPaths {
            dataset: "My-DS".into(),
            service_path: tmp.path().join("service.json"),
            query_path: tmp.path().join("q.json"),
            taxonomy_path: tmp.path().join("taxonomy/taxonomy.json"),
            class_path: tmp.path().join("taxonomy/class.json"),
            chroma_dir: tmp.path().join("chroma"),
            embedding_model: "all-MiniLM-L6-v2".into(),
            llm_workers: 2,
        };
        engine.sync_vector(&paths).await?;
        let store = VectorStore::open("my_ds", &paths.chroma_dir, None)?;
        assert_eq!(store.count(), 2);
        let out = engine
            .search(SearchMethod::Vector { top_k: Some(1) }, "flights", 1, &paths)
            .await?;
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].id, "a");
        assert_eq!(out.stats["llm_calls"], 0);
        engine.purge_dataset(&paths);
        assert!(!paths.chroma_dir.join("my_ds.json").exists());
        Ok(())
    }
}
