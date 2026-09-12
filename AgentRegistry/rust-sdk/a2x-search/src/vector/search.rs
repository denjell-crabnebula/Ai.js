// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Vector search with an interface consistent with [`crate::A2xSearch`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a2x_common::SearchResult;
use serde::{Deserialize, Serialize};

use super::embedding::{DEFAULT_EMBEDDING_MODEL, EmbeddingModel};
use super::index_builder::VectorIndexBuilder;
use super::store::VectorStore;
use crate::error::Result;
use crate::taxonomy::{ServiceRecord, ServicesIndex, index_services, load_services};

/// Search statistics (always zero: vector search makes no LLM calls).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorStats {
    pub llm_calls: u64,
    pub total_tokens: u64,
}

/// Configuration for [`VectorSearch`].
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchConfig {
    pub service_path: PathBuf,
    pub collection_name: String,
    pub persist_dir: PathBuf,
    pub model_name: String,
    pub force_rebuild: bool,
}

impl Default for VectorSearchConfig {
    fn default() -> Self {
        Self {
            service_path: PathBuf::from("database/ToolRet_clean/service.json"),
            collection_name: "toolret_new".into(),
            persist_dir: PathBuf::from("database/chroma"),
            model_name: DEFAULT_EMBEDDING_MODEL.into(),
            force_rebuild: false,
        }
    }
}

impl VectorSearchConfig {
    pub fn new(service_path: &Path, collection_name: &str, persist_dir: &Path, model_name: &str) -> Self {
        Self {
            service_path: service_path.to_path_buf(),
            collection_name: collection_name.to_string(),
            persist_dir: persist_dir.to_path_buf(),
            model_name: model_name.to_string(),
            force_rebuild: false,
        }
    }

    pub fn with_force_rebuild(mut self, force: bool) -> Self {
        self.force_rebuild = force;
        self
    }
}

/// Vector based service retrieval.
pub struct VectorSearch {
    config: VectorSearchConfig,
    model: Arc<dyn EmbeddingModel>,
    store: VectorStore,
    service_map: ServicesIndex,
}

impl VectorSearch {
    /// Load services from `config.service_path` and build or reuse the index.
    pub async fn new(config: VectorSearchConfig, model: Arc<dyn EmbeddingModel>) -> Result<Self> {
        let services = load_services(&config.service_path)?;
        Self::from_services(config, services, model).await
    }

    /// Build from loaded services.
    pub async fn from_services(
        config: VectorSearchConfig,
        services: Vec<ServiceRecord>,
        model: Arc<dyn EmbeddingModel>,
    ) -> Result<Self> {
        let builder =
            VectorIndexBuilder::new(&config.collection_name, &config.persist_dir, &config.model_name);
        let store = builder
            .build_from_services(&services, config.force_rebuild, model.as_ref())
            .await?;
        Ok(Self {
            config,
            model,
            store,
            service_map: index_services(services),
        })
    }

    pub fn config(&self) -> &VectorSearchConfig {
        &self.config
    }

    pub fn store(&self) -> &VectorStore {
        &self.store
    }

    pub fn model(&self) -> &Arc<dyn EmbeddingModel> {
        &self.model
    }

    pub fn model_name(&self) -> String {
        self.config.model_name.clone()
    }

    /// Search for the `top_k` nearest services.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<(Vec<SearchResult>, VectorStats)> {
        let query_emb = self.model.embed_one(query).await?;
        let hits = self.store.query(&query_emb, top_k);
        let results = hits
            .into_iter()
            .map(|hit| match self.service_map.get(&hit.id) {
                Some(svc) => SearchResult::new(hit.id, svc.name.clone(), svc.description_text()),
                None => SearchResult::new(hit.id, "Unknown", ""),
            })
            .collect();
        Ok((results, VectorStats::default()))
    }
}
