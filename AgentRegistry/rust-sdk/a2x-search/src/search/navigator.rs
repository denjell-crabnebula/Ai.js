// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Phase 1: LLM-guided recursive category navigation.

use a2x_common::{ChatMessage, LlmBackend};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};

use super::models::{NavigationStep, SearchMode, StatsCollector, TerminalNode};
use super::prompts::{CategoryEntry, build_category_prompt, parse_selection};
use crate::taxonomy::{ClassFile, TaxonomyFile};

/// Callback invoked for each navigation step.
pub type StepCallback = dyn Fn(NavigationStep) + Send + Sync;

/// What a navigation is looking for and where it reports progress.
#[derive(Clone, Copy)]
pub struct NavigationRequest<'a> {
    /// The user query.
    pub query: &'a str,
    /// How many categories to follow at each level.
    pub mode: SearchMode,
    /// Where LLM calls and visited nodes are counted.
    pub stats: &'a StatsCollector,
    /// Called for every navigation step, when set.
    pub step_callback: Option<&'a StepCallback>,
}

/// Recursively navigates the taxonomy tree, asking the LLM which children
/// are relevant at each level.
pub struct CategoryNavigator<'a> {
    llm: &'a dyn LlmBackend,
    taxonomy: &'a TaxonomyFile,
    classes: &'a ClassFile,
    max_workers: usize,
    parallel: bool,
}

impl<'a> CategoryNavigator<'a> {
    pub fn new(
        llm: &'a dyn LlmBackend,
        taxonomy: &'a TaxonomyFile,
        classes: &'a ClassFile,
        max_workers: usize,
        parallel: bool,
    ) -> Self {
        Self {
            llm,
            taxonomy,
            classes,
            max_workers,
            parallel,
        }
    }

    fn category_name<'b>(&'b self, id: &'b str) -> &'b str {
        self.classes.name_of(id)
    }

    /// Recursively navigate from `category_id`, returning terminal nodes.
    pub fn navigate<'b>(
        &'b self,
        req: &'b NavigationRequest<'b>,
        category_id: &'b str,
        path: String,
        depth: usize,
    ) -> BoxFuture<'b, Vec<TerminalNode>> {
        Box::pin(async move {
            req.stats.add_visited_id(category_id);

            let cat_name = self.category_name(category_id);
            let current_path = if path.is_empty() {
                cat_name.to_string()
            } else {
                format!("{path}/{cat_name}")
            };

            let direct_services = self.taxonomy.services(category_id);
            let children = self.taxonomy.children(category_id);

            let mut terminal_nodes = Vec::new();

            if !children.is_empty() {
                let selected = self
                    .select_categories(req, children, &current_path, category_id)
                    .await;
                if !selected.is_empty() {
                    let child_terminals = self
                        .navigate_children(req, &selected, &current_path, depth + 1)
                        .await;
                    terminal_nodes.extend(child_terminals);
                }
                if !direct_services.is_empty() {
                    terminal_nodes.push(TerminalNode {
                        category_id: category_id.to_string(),
                        service_ids: direct_services.to_vec(),
                    });
                }
            } else if !direct_services.is_empty() {
                terminal_nodes.push(TerminalNode {
                    category_id: category_id.to_string(),
                    service_ids: direct_services.to_vec(),
                });
            }

            terminal_nodes
        })
    }

    async fn navigate_children<'b>(
        &'b self,
        req: &'b NavigationRequest<'b>,
        child_ids: &'b [String],
        parent_path: &'b str,
        depth: usize,
    ) -> Vec<TerminalNode> {
        let mut terminal_nodes = Vec::new();
        if self.parallel && child_ids.len() > 1 {
            let concurrency = self.max_workers.clamp(1, child_ids.len());
            let futures: Vec<_> = child_ids
                .iter()
                .map(|child_id| self.navigate(req, child_id, parent_path.to_string(), depth))
                .collect();
            let mut results = stream::iter(futures).buffer_unordered(concurrency);
            while let Some(nodes) = results.next().await {
                terminal_nodes.extend(nodes);
            }
        } else {
            for child_id in child_ids {
                let nodes = self.navigate(req, child_id, parent_path.to_string(), depth).await;
                terminal_nodes.extend(nodes);
            }
        }
        terminal_nodes
    }

    /// Ask the LLM which child categories are relevant. Returns selected ids.
    async fn select_categories(
        &self,
        req: &NavigationRequest<'_>,
        child_ids: &[String],
        parent_path: &str,
        parent_id: &str,
    ) -> Vec<String> {
        let entries: Vec<CategoryEntry<'_>> = child_ids
            .iter()
            .map(|id| match self.classes.info(id) {
                Some(info) => CategoryEntry {
                    name: info.name_or(id),
                    description: info.description_or("No description"),
                    boundary: info.boundary_text(),
                },
                None => CategoryEntry {
                    name: id,
                    description: "",
                    boundary: "",
                },
            })
            .collect();
        let prompt = build_category_prompt(req.mode, req.query, &entries, parent_path);
        let response = self.llm.call(&[ChatMessage::user(prompt)], 0.0, Some(200)).await;
        req.stats.add_call(response.tokens);
        if !response.success {
            tracing::error!(
                "Error selecting categories: {}",
                response.error.as_deref().unwrap_or("unknown")
            );
        }

        let selected_indices = parse_selection(&response.content, child_ids.len(), req.mode);

        let mut selected_ids = Vec::new();
        let mut pruned_ids = Vec::new();
        let mut visited_paths = Vec::new();
        let mut pruned_paths = Vec::new();
        for (i, child_id) in child_ids.iter().enumerate() {
            let child_path = format!("{parent_path}/{}", self.category_name(child_id));
            if selected_indices.contains(&i) {
                selected_ids.push(child_id.clone());
                visited_paths.push(child_path);
            } else {
                pruned_ids.push(child_id.clone());
                pruned_paths.push(child_path);
            }
        }
        req.stats.add_paths(visited_paths, pruned_paths);

        if let Some(cb) = req.step_callback {
            cb(NavigationStep {
                parent_id: parent_id.to_string(),
                selected: selected_ids.clone(),
                pruned: pruned_ids,
            });
        }

        selected_ids
    }
}
