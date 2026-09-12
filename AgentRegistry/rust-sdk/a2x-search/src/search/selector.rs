// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Phase 2 and 3: service deduplication, group merging and LLM selection.

use std::collections::HashSet;

use a2x_common::{ChatMessage, LlmBackend, SearchResult};
use futures::stream::{self, StreamExt};

use super::models::{SearchMode, ServiceGroup, StatsCollector, TerminalNode};
use super::prompts::{ServiceEntry, build_service_prompt, parse_selection};
use crate::taxonomy::{ServicesIndex, TreeIndex};

/// Groups smaller than this are merged with their nearest neighbour.
pub const MIN_GROUP_SIZE: usize = 30;

/// Deduplicates, groups and selects services from terminal nodes.
pub struct ServiceSelector<'a> {
    llm: &'a dyn LlmBackend,
    services: &'a ServicesIndex,
    tree: &'a TreeIndex,
    max_workers: usize,
    parallel: bool,
}

impl<'a> ServiceSelector<'a> {
    pub fn new(
        llm: &'a dyn LlmBackend,
        services: &'a ServicesIndex,
        tree: &'a TreeIndex,
        max_workers: usize,
        parallel: bool,
    ) -> Self {
        Self {
            llm,
            services,
            tree,
            max_workers,
            parallel,
        }
    }

    /// Remove duplicate services across terminal nodes (first come first served).
    pub fn deduplicate(&self, terminal_nodes: Vec<TerminalNode>) -> Vec<TerminalNode> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut result = Vec::new();
        for node in terminal_nodes {
            let unique: Vec<String> = node
                .service_ids
                .into_iter()
                .filter(|sid| seen.insert(sid.clone()))
                .collect();
            if !unique.is_empty() {
                result.push(TerminalNode {
                    category_id: node.category_id,
                    service_ids: unique,
                });
            }
        }
        result
    }

    /// Merge groups with fewer than [`MIN_GROUP_SIZE`] services into the
    /// group whose leaves share the deepest common ancestor.
    pub fn merge_small_groups(&self, terminal_nodes: Vec<TerminalNode>) -> Vec<ServiceGroup> {
        if terminal_nodes.is_empty() {
            return Vec::new();
        }
        let mut groups: Vec<ServiceGroup> = Vec::new();
        let mut group_paths: Vec<Vec<Vec<String>>> = Vec::new();
        for node in terminal_nodes {
            group_paths.push(vec![self.tree.ancestors(&node.category_id)]);
            let mut leaf_ids = HashSet::new();
            leaf_ids.insert(node.category_id);
            groups.push(ServiceGroup {
                leaf_ids,
                service_ids: node.service_ids,
            });
        }

        while groups.len() > 1 {
            let mut small_idx: Option<usize> = None;
            let mut small_size = usize::MAX;
            for (i, g) in groups.iter().enumerate() {
                if g.service_ids.len() < MIN_GROUP_SIZE && g.service_ids.len() < small_size {
                    small_size = g.service_ids.len();
                    small_idx = Some(i);
                }
            }
            let Some(small_idx) = small_idx else { break };

            let mut best_idx: Option<usize> = None;
            let mut best_lca: i64 = -1;
            for (j, _) in groups.iter().enumerate() {
                if j == small_idx {
                    continue;
                }
                let mut max_lca: Option<i64> = None;
                for pa in &group_paths[small_idx] {
                    for pb in &group_paths[j] {
                        let d = TreeIndex::lca_depth(pa, pb);
                        max_lca = Some(max_lca.map_or(d, |m| m.max(d)));
                    }
                }
                let Some(max_lca) = max_lca else { continue };
                if max_lca > best_lca {
                    best_lca = max_lca;
                    best_idx = Some(j);
                }
            }
            let Some(best_idx) = best_idx else { break };

            let source = groups.remove(small_idx);
            let source_paths = group_paths.remove(small_idx);
            let target_idx = if best_idx > small_idx {
                best_idx - 1
            } else {
                best_idx
            };
            let target = &mut groups[target_idx];
            let mut existing: HashSet<String> = target.service_ids.iter().cloned().collect();
            for sid in source.service_ids {
                if existing.insert(sid.clone()) {
                    target.service_ids.push(sid);
                }
            }
            target.leaf_ids.extend(source.leaf_ids);
            group_paths[target_idx].extend(source_paths);
        }
        groups
    }

    /// Select relevant services from all groups, in parallel when enabled.
    pub async fn select_services(
        &self,
        query: &str,
        groups: &[ServiceGroup],
        mode: SearchMode,
        stats: &StatsCollector,
    ) -> Vec<SearchResult> {
        if groups.is_empty() {
            return Vec::new();
        }
        let mut results = Vec::new();
        if self.parallel && groups.len() > 1 {
            let concurrency = self.max_workers.clamp(1, groups.len());
            let futures: Vec<_> = groups
                .iter()
                .map(|group| self.select_for_group(query, group, mode, stats))
                .collect();
            let mut stream = stream::iter(futures).buffer_unordered(concurrency);
            while let Some(r) = stream.next().await {
                results.extend(r);
            }
        } else {
            for group in groups {
                results.extend(self.select_for_group(query, group, mode, stats).await);
            }
        }
        results
    }

    async fn select_for_group(
        &self,
        query: &str,
        group: &ServiceGroup,
        mode: SearchMode,
        stats: &StatsCollector,
    ) -> Vec<SearchResult> {
        if group.service_ids.is_empty() {
            return Vec::new();
        }
        let entries: Vec<ServiceEntry<'_>> = group
            .service_ids
            .iter()
            .map(|sid| match self.services.get(sid) {
                Some(svc) => ServiceEntry {
                    name: &svc.name,
                    description: svc.description_or("No description"),
                },
                None => ServiceEntry {
                    name: sid,
                    description: "",
                },
            })
            .collect();
        let prompt = build_service_prompt(mode, query, &entries);
        let response = self.llm.call(&[ChatMessage::user(prompt)], 0.0, Some(200)).await;
        stats.add_call(response.tokens);
        if !response.success {
            tracing::error!(
                "Error selecting services: {}",
                response.error.as_deref().unwrap_or("unknown")
            );
        }
        let selected = parse_selection(&response.content, group.service_ids.len(), mode);
        selected
            .into_iter()
            .map(|idx| {
                let sid = &group.service_ids[idx];
                match self.services.get(sid) {
                    Some(svc) => SearchResult::new(sid.clone(), svc.name.clone(), svc.description_text()),
                    None => SearchResult::new(sid.clone(), sid.clone(), ""),
                }
            })
            .collect()
    }
}
