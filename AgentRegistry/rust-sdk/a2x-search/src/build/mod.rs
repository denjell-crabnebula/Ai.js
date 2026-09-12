// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Fully automatic hierarchical taxonomy construction.
//!
//! Pipeline (BFS over oversized nodes):
//!
//! 1. Category design ([`category_designer::CategoryDesigner`]): keyword
//!    based for large nodes, description based for small nodes. Root
//!    categories are additionally validated and repaired by the LLM.
//! 2. Classification ([`node_splitter::NodeSplitter`]): every service is
//!    classified into the subcategories with one LLM call each, in
//!    parallel. Generic and unclassified services stay at the parent.
//! 3. Refinement: while not converged, the LLM adjusts the subcategories
//!    from classification feedback.
//! 4. Boundary handling: tiny subcategories are deleted.
//! 5. Cross-domain assignment ([`cross_domain_assigner::CrossDomainAssigner`]).
//!
//! [`taxonomy_builder::TaxonomyBuilder`] drives the loop, checkpoints after
//! every split and supports resume modes `no`, `keyword` and `yes`.

pub mod category_designer;
pub mod config;
pub mod cross_domain_assigner;
pub mod keyword_extractor;
pub mod node_splitter;
pub mod progress;
pub mod prompts;
pub mod taxonomy_builder;

pub use category_designer::CategoryDesigner;
pub use config::AutoHierarchicalConfig;
pub use cross_domain_assigner::CrossDomainAssigner;
pub use keyword_extractor::KeywordExtractor;
pub use node_splitter::NodeSplitter;
pub use progress::{BuildEvent, BuildPhase, BuildSink, LogLevel};
pub use prompts::{
    Assignment, Assignments, ClassificationResult, ClassificationStats, NodeInfo, NodeSplitResult,
    Subcategories, SubcategoryDef,
};
pub use taxonomy_builder::{BuildOutcome, BuildSummary, ResumeMode, TaxonomyBuilder, compute_service_hash};
