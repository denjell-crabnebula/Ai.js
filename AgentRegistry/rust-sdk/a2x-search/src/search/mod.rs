// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2X hierarchical taxonomy search.
//!
//! Two-phase search:
//!
//! - Phase 1 ([`navigator::CategoryNavigator`]): LLM-guided recursive
//!   category navigation from the root.
//! - Phase 2 ([`selector::ServiceSelector`]): deduplicate services from the
//!   terminal nodes, merge small groups, LLM selects services per group.
//!
//! Modes: `get_all` (high recall), `get_important` (balanced) and `get_one`
//! (single best match, falls back to `get_important` when empty).

pub mod judge;
pub mod models;
pub mod navigator;
pub mod prompts;
pub mod selector;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use a2x_common::paths::dataset_dir;
use a2x_common::{LlmBackend, SearchResult};
use futures::future::BoxFuture;
use tokio::sync::mpsc;

pub use judge::{build_judge_prompt, judge_relevance};
pub use models::{
    NavigationStep, SearchMode, SearchStats, ServiceGroup, StatsCollector, StreamMessage, StreamStats,
    TerminalNode,
};
pub use navigator::{NavigationRequest, StepCallback};

use crate::error::Result;
use crate::taxonomy::{ClassFile, ServicesIndex, TaxonomyFile, TreeIndex, index_services, load_services};
use navigator::CategoryNavigator;
use selector::ServiceSelector;

/// Configuration for [`A2xSearch`].
#[derive(Clone, Debug, PartialEq)]
pub struct A2xSearchConfig {
    pub taxonomy_path: PathBuf,
    pub class_path: PathBuf,
    pub service_path: PathBuf,
    /// Maximum concurrent LLM calls per navigation level and per selection.
    pub max_workers: usize,
    /// Navigate branches and select groups concurrently.
    pub parallel: bool,
    pub mode: SearchMode,
}

impl Default for A2xSearchConfig {
    /// Defaults to the `ToolRet_clean` dataset under the resolved registry home.
    fn default() -> Self {
        Self::for_dataset_dir(&dataset_dir("ToolRet_clean"))
    }
}

impl A2xSearchConfig {
    /// Paths for a dataset directory laid out as `<dir>/service.json` and
    /// `<dir>/taxonomy/{taxonomy,class}.json`.
    pub fn for_dataset_dir(dir: &Path) -> Self {
        Self {
            taxonomy_path: dir.join("taxonomy").join("taxonomy.json"),
            class_path: dir.join("taxonomy").join("class.json"),
            service_path: dir.join("service.json"),
            max_workers: 20,
            parallel: true,
            mode: SearchMode::GetAll,
        }
    }

    pub fn with_mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_max_workers(mut self, workers: usize) -> Self {
        self.max_workers = workers;
        self
    }

    pub fn with_parallel(mut self, parallel: bool) -> Self {
        self.parallel = parallel;
        self
    }
}

/// Two-phase hierarchical taxonomy search.
pub struct A2xSearch {
    config: A2xSearchConfig,
    taxonomy: TaxonomyFile,
    classes: ClassFile,
    services: ServicesIndex,
    tree: TreeIndex,
    llm: Arc<dyn LlmBackend>,
}

impl A2xSearch {
    /// Load taxonomy, class and service files named in `config`.
    pub fn new(config: A2xSearchConfig, llm: Arc<dyn LlmBackend>) -> Result<Self> {
        let taxonomy = TaxonomyFile::load(&config.taxonomy_path)?;
        let classes = ClassFile::load(&config.class_path)?;
        let services = index_services(load_services(&config.service_path)?);
        Ok(Self::from_parts(config, taxonomy, classes, services, llm))
    }

    /// Build from already loaded data. The paths in `config` are only
    /// reported back by [`A2xSearch::config`].
    pub fn from_parts(
        config: A2xSearchConfig,
        taxonomy: TaxonomyFile,
        classes: ClassFile,
        services: ServicesIndex,
        llm: Arc<dyn LlmBackend>,
    ) -> Self {
        let tree = TreeIndex::new(&taxonomy);
        Self {
            config,
            taxonomy,
            classes,
            services,
            tree,
            llm,
        }
    }

    pub fn config(&self) -> &A2xSearchConfig {
        &self.config
    }

    pub fn mode(&self) -> SearchMode {
        self.config.mode
    }

    pub fn taxonomy(&self) -> &TaxonomyFile {
        &self.taxonomy
    }

    pub fn classes(&self) -> &ClassFile {
        &self.classes
    }

    pub fn services(&self) -> &ServicesIndex {
        &self.services
    }

    pub fn llm(&self) -> &Arc<dyn LlmBackend> {
        &self.llm
    }

    /// Search for services matching `query` in the configured mode.
    pub async fn search(&self, query: &str) -> (Vec<SearchResult>, SearchStats) {
        self.search_with_callback(query, None).await
    }

    /// Search and report each [`NavigationStep`] through `on_step`.
    pub async fn search_with_callback(
        &self,
        query: &str,
        on_step: Option<&StepCallback>,
    ) -> (Vec<SearchResult>, SearchStats) {
        let (results, stats) = self.search_internal(query, self.config.mode, on_step).await;
        (results, stats.into_inner())
    }

    /// Streaming search for the WebSocket API.
    ///
    /// Runs the search on a tokio task and yields a `Step` message for
    /// every navigation step, then one `Result` message.
    pub fn search_streaming(
        self: &Arc<Self>,
        query: impl Into<String>,
    ) -> mpsc::UnboundedReceiver<StreamMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        let this = Arc::clone(self);
        let query = query.into();
        tokio::spawn(async move {
            let step_tx = tx.clone();
            let on_step = move |step: NavigationStep| {
                let _ = step_tx.send(step.into());
            };
            let (results, stats) = this.search_with_callback(&query, Some(&on_step)).await;
            let _ = tx.send(StreamMessage::Result {
                results,
                stats: stats.summary(),
            });
        });
        rx
    }

    /// Core pipeline: navigate, deduplicate, merge, select, final dedup,
    /// `get_one` fallback.
    fn search_internal<'b>(
        &'b self,
        query: &'b str,
        mode: SearchMode,
        on_step: Option<&'b StepCallback>,
    ) -> BoxFuture<'b, (Vec<SearchResult>, StatsCollector)> {
        Box::pin(async move {
            let stats = StatsCollector::new();
            let navigator = CategoryNavigator::new(
                self.llm.as_ref(),
                &self.taxonomy,
                &self.classes,
                self.config.max_workers,
                self.config.parallel,
            );
            let selector = ServiceSelector::new(
                self.llm.as_ref(),
                &self.services,
                &self.tree,
                self.config.max_workers,
                self.config.parallel,
            );

            // Phase 1: category navigation
            let request = NavigationRequest {
                query,
                mode,
                stats: &stats,
                step_callback: on_step,
            };
            let terminal_nodes = navigator
                .navigate(&request, &self.taxonomy.root, String::new(), 0)
                .await;

            if let Some(cb) = on_step {
                cb(NavigationStep::marker(NavigationStep::PHASE2));
            }

            // Phase 2: dedup + merge + select
            let terminal_nodes = selector.deduplicate(terminal_nodes);
            let groups = selector.merge_small_groups(terminal_nodes);
            let results = selector.select_services(query, &groups, mode, &stats).await;

            // Final dedup
            let mut seen: HashSet<String> = HashSet::new();
            let mut unique_results: Vec<SearchResult> = results
                .into_iter()
                .filter(|r| seen.insert(r.id.clone()))
                .collect();

            // get_one fallback: re-run as get_important and take the first result
            if mode == SearchMode::GetOne && unique_results.is_empty() {
                tracing::info!("get_one returned empty, falling back to get_important");
                if let Some(cb) = on_step {
                    cb(NavigationStep::marker(NavigationStep::FALLBACK));
                }
                let (fb_results, fb_stats) = self
                    .search_internal(query, SearchMode::GetImportant, on_step)
                    .await;
                let fb = fb_stats.into_inner();
                stats.add_counts(fb.llm_calls, fb.total_tokens);
                if let Some(first) = fb_results.into_iter().next() {
                    unique_results = vec![first];
                }
            }

            (unique_results, stats)
        })
    }
}
