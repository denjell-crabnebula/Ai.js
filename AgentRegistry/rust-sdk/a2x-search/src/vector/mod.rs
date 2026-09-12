// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Vector search baseline.
//!
//! The Python package used sentence-transformers and ChromaDB. Neither is
//! available in Rust, so this module substitutes:
//!
//! - [`EmbeddingModel`]: a trait with [`OpenAiCompatibleEmbedding`] (HTTP
//!   embeddings endpoint) and the deterministic offline [`HashingEmbedding`]
//! - [`VectorStore`]: an in-memory cosine store persisted as one JSON file
//!   per collection, with Chroma's collection naming and incremental
//!   add / upsert / delete semantics
//!
//! The [`embedding_models`] table and [`DEFAULT_EMBEDDING_MODEL`] are kept
//! so `GET /api/datasets/embedding-models` and vector-config validation
//! behave like the original.

pub mod embedding;
pub mod index_builder;
pub mod metrics;
pub mod search;
pub mod store;

pub use embedding::{
    DEFAULT_EMBEDDING_MODEL, EmbeddingBackend, EmbeddingModel, EmbeddingModelInfo, EmbeddingProviderConfig,
    HashingEmbedding, OpenAiCompatibleEmbedding, default_embedding_model, embedding_dim, embedding_models,
    normalize, resolve_embedding_model,
};
pub use index_builder::VectorIndexBuilder;
pub use metrics::{hit_at_k, mrr, ndcg_at_k, precision_at_k, recall_at_k};
pub use search::{VectorSearch, VectorSearchConfig, VectorStats};
pub use store::{QueryHit, StoredDoc, VectorStore, collection_name_for_dataset};
