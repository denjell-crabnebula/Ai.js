// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Vector index builder: embeds service descriptions into a [`VectorStore`].

use std::path::{Path, PathBuf};

use super::embedding::EmbeddingModel;
use super::store::VectorStore;
use crate::error::Result;
use crate::taxonomy::{ServiceRecord, load_services};

/// Builds and manages the vector index for service retrieval.
#[derive(Clone, Debug)]
pub struct VectorIndexBuilder {
    pub collection_name: String,
    pub persist_dir: PathBuf,
    pub model_name: String,
}

impl Default for VectorIndexBuilder {
    fn default() -> Self {
        Self::new(
            "toolret_new",
            Path::new("database/chroma"),
            super::embedding::DEFAULT_EMBEDDING_MODEL,
        )
    }
}

impl VectorIndexBuilder {
    pub fn new(collection_name: &str, persist_dir: &Path, model_name: &str) -> Self {
        Self {
            collection_name: collection_name.to_string(),
            persist_dir: persist_dir.to_path_buf(),
            model_name: model_name.to_string(),
        }
    }

    /// Build the index from `service.json`. An existing non-empty
    /// collection is reused unless `force_rebuild` is set.
    pub async fn build(
        &self,
        service_path: &Path,
        force_rebuild: bool,
        model: &dyn EmbeddingModel,
    ) -> Result<VectorStore> {
        let services = load_services(service_path)?;
        self.build_from_services(&services, force_rebuild, model).await
    }

    /// Build the index from loaded services.
    pub async fn build_from_services(
        &self,
        services: &[ServiceRecord],
        force_rebuild: bool,
        model: &dyn EmbeddingModel,
    ) -> Result<VectorStore> {
        let mut store = VectorStore::open(&self.collection_name, &self.persist_dir, Some(&self.model_name))?;
        if force_rebuild {
            tracing::info!("Clearing existing index...");
            store.clear()?;
            store.set_embedding_model(Some(&self.model_name))?;
        }
        if store.count() == 0 {
            tracing::info!("Building index for {} services...", services.len());
            let ids: Vec<String> = services.iter().map(|s| s.id.clone()).collect();
            let texts: Vec<String> = services
                .iter()
                .map(|s| s.description_text().to_string())
                .collect();
            tracing::info!("Encoding service descriptions...");
            let embeddings = model.embed(&texts).await?;
            tracing::info!("Adding to vector store...");
            store.upsert(&ids, &texts, &embeddings)?;
            tracing::info!("Index built: {} documents", store.count());
        } else {
            tracing::info!("Using existing index: {} documents", store.count());
        }
        Ok(store)
    }
}
