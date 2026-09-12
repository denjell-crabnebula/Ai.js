// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2X taxonomy build and search, with vector and traditional baselines.
//!
//! Port of `AgentRegistry/a2x_registry/{a2x,vector,traditional}`:
//!
//! - [`search`]: two-phase LLM navigated search ([`A2xSearch`]) and the
//!   relevance judge ([`judge_relevance`])
//! - [`build`]: fully automatic taxonomy construction ([`TaxonomyBuilder`])
//! - [`incremental`]: add or remove services in a built taxonomy
//! - [`traditional`]: full-context MCP style baseline
//! - [`vector`]: embedding based baseline with an in-memory cosine store
//! - [`evaluation`]: evaluators, metrics and error analysis
//! - [`taxonomy`]: the on-disk file formats shared by everything above
//! - [`testing`]: a scripted [`FakeLlm`] for offline tests
//!
//! All LLM access goes through `Arc<dyn a2x_common::LlmBackend>`.

pub mod build;
pub mod error;
pub mod evaluation;
pub mod incremental;
pub mod search;
pub mod taxonomy;
pub mod testing;
pub mod traditional;
pub mod util;
pub mod vector;

pub use a2x_common::{LlmBackend, LlmClient, SearchResult};
pub use build::{
    AutoHierarchicalConfig, BuildEvent, BuildOutcome, BuildPhase, BuildSink, ResumeMode, TaxonomyBuilder,
};
pub use error::{Error, Result};
pub use evaluation::{A2xEvaluator, TraditionalEvaluator, VectorEvaluator};
pub use incremental::IncrementalBuilder;
pub use search::{
    A2xSearch, A2xSearchConfig, NavigationStep, SearchMode, SearchStats, StreamMessage, judge_relevance,
};
pub use taxonomy::{
    CategoryInfo, CategoryNode, ClassFile, QueryObject, ServiceRecord, ServicesIndex, TaxonomyFile, TreeIndex,
};
pub use testing::FakeLlm;
pub use traditional::{TraditionalSearch, TraditionalStats};
pub use util::llm_workers_from_env;
pub use vector::{
    DEFAULT_EMBEDDING_MODEL, EmbeddingModel, HashingEmbedding, OpenAiCompatibleEmbedding, VectorIndexBuilder,
    VectorSearch, VectorSearchConfig, VectorStore, embedding_models,
};

/// The environment variables this crate reads, with the values a safe
/// deployment accepts. Merge it into a binary's [`ap_support::env::EnvPolicy`].
pub fn env_policy() -> ap_support::env::EnvPolicy {
    a2x_common::env_policy()
}
