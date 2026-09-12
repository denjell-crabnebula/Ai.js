// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Unified BFS recursive taxonomy builder.
//!
//! Build modes:
//!
//! - `ResumeMode::No`: full rebuild, delete every intermediate result.
//! - `ResumeMode::Keyword`: rebuild but reuse cached root keywords.
//! - `ResumeMode::Yes`: smart resume. Skip when complete with a matching
//!   config, continue from the checkpoint when partial, otherwise rebuild.
//!
//! `build_status` in `taxonomy.json` tracks progress: `"bfs"`,
//! `"cross_domain"`, `"complete"`. Output is saved after every node split
//! so an interrupted build can resume.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use a2x_common::LlmBackend;
use indexmap::IndexMap;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::config::AutoHierarchicalConfig;
use super::cross_domain_assigner::CrossDomainAssigner;
use super::node_splitter::NodeSplitter;
use super::progress::{BuildPhase, BuildSink};
use super::prompts::{Assignments, NodeInfo, NodeSplitResult};
use crate::error::{Error, Result};
use crate::taxonomy::{
    CategoryInfo, CategoryNode, ClassFile, ROOT_ID, ServicesIndex, TaxonomyFile, index_services,
    load_services,
};
use crate::util::{read_json, write_json};

/// Build output files; only these are deleted on clean.
const BUILD_FILES: &[&str] = &[
    "taxonomy.json",
    "class.json",
    "keywords.json",
    "build_config.json",
    "assignments.json",
];

/// How to treat existing output when building.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResumeMode {
    /// Full rebuild (delete all intermediate results including keywords).
    #[default]
    No,
    /// Rebuild but reuse cached root keywords.
    Keyword,
    /// Skip if complete, continue if partial, rebuild on config mismatch.
    Yes,
}

impl ResumeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResumeMode::No => "no",
            ResumeMode::Keyword => "keyword",
            ResumeMode::Yes => "yes",
        }
    }
}

impl FromStr for ResumeMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "no" => Ok(ResumeMode::No),
            "keyword" => Ok(ResumeMode::Keyword),
            "yes" => Ok(ResumeMode::Yes),
            other => Err(Error::Invalid(format!(
                "Invalid resume mode: {other:?}. Must be 'no', 'keyword', or 'yes'."
            ))),
        }
    }
}

/// Decision made by smart resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAction {
    Skip,
    Resume,
    Rebuild,
}

/// Structural summary printed at the end of a build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildSummary {
    /// Categories excluding root.
    pub total_categories: usize,
    pub leaf_nodes: usize,
    pub max_depth: usize,
    /// `depth -> category count` (root excluded), sorted by depth.
    pub depth_distribution: Vec<(usize, usize)>,
    /// Sum of `services` list lengths over all categories.
    pub total_service_assignments: usize,
}

/// Result of [`TaxonomyBuilder::build`].
#[derive(Clone, Debug)]
pub struct BuildOutcome {
    /// True when smart resume found a complete build and did nothing.
    pub skipped: bool,
    pub nodes_split: usize,
    pub elapsed: Duration,
    pub output_dir: PathBuf,
    pub taxonomy: TaxonomyFile,
    pub class_data: ClassFile,
    pub summary: BuildSummary,
}

/// Unified recursive taxonomy builder.
pub struct TaxonomyBuilder {
    config: AutoHierarchicalConfig,
    llm: Arc<dyn LlmBackend>,
    taxonomy: TaxonomyFile,
    class_data: ClassFile,
    services_index: ServicesIndex,
    all_assignments: Assignments,
}

impl TaxonomyBuilder {
    pub fn new(config: AutoHierarchicalConfig, llm: Arc<dyn LlmBackend>) -> Self {
        Self {
            config,
            llm,
            taxonomy: TaxonomyFile {
                categories: IndexMap::new(),
                ..Default::default()
            },
            class_data: ClassFile::default(),
            services_index: ServicesIndex::new(),
            all_assignments: Assignments::new(),
        }
    }

    pub fn config(&self) -> &AutoHierarchicalConfig {
        &self.config
    }

    pub fn taxonomy(&self) -> &TaxonomyFile {
        &self.taxonomy
    }

    pub fn class_data(&self) -> &ClassFile {
        &self.class_data
    }

    fn check_cancelled(&self, cancel: &CancellationToken) -> Result<()> {
        if cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Execute the build. Events go to `sink`; `cancel` is checked at
    /// phase boundaries and between node splits.
    pub async fn build(
        &mut self,
        resume: ResumeMode,
        sink: BuildSink,
        cancel: CancellationToken,
    ) -> Result<BuildOutcome> {
        let start = Instant::now();

        // Phase 0: decide action
        match resume {
            ResumeMode::Yes => match self.evaluate_resume() {
                ResumeAction::Skip => {
                    sink.log("Taxonomy already complete with matching config, skipping build");
                    self.load_existing_output()?;
                    self.load_services_index(&sink)?;
                    return Ok(self.outcome(true, 0, start.elapsed()));
                }
                ResumeAction::Resume => {
                    sink.log("Resuming build from checkpoint");
                    self.load_existing_output()?;
                }
                ResumeAction::Rebuild => {
                    sink.log("Config mismatch or no checkpoint — full rebuild");
                    self.clean_output(false, &sink)?;
                }
            },
            ResumeMode::Keyword => self.clean_output(true, &sink)?,
            ResumeMode::No => self.clean_output(false, &sink)?,
        }

        // Phase 1: load services, initialise taxonomy
        self.check_cancelled(&cancel)?;
        self.load_services_index(&sink)?;
        if self.taxonomy.categories.is_empty() {
            self.init_taxonomy();
        }

        // Phase 2: BFS splitting
        self.check_cancelled(&cancel)?;
        let status = self.taxonomy.build_status.clone();
        let total_split = if !matches!(status.as_deref(), Some("cross_domain") | Some("complete")) {
            self.taxonomy.build_status = Some(BuildPhase::Bfs.as_str().to_string());
            self.save_output()?;
            sink.phase(BuildPhase::Bfs, "BFS phase started");
            let queue = self.find_pending_nodes();
            let total_split = self.run_bfs(queue, &sink, &cancel).await?;
            self.check_cancelled(&cancel)?;
            self.taxonomy.build_status = Some(BuildPhase::CrossDomain.as_str().to_string());
            self.save_output()?;
            sink.log("BFS phase complete");
            total_split
        } else {
            sink.log("BFS phase already complete, skipping");
            0
        };

        // Phase 3: cross-domain assignment
        self.check_cancelled(&cancel)?;
        if self.config.enable_cross_domain && self.taxonomy.build_status.as_deref() != Some("complete") {
            sink.phase(BuildPhase::CrossDomain, "Cross-Domain Multi-Parent Assignment");
            self.apply_cross_domain(&sink).await;
        }

        // Phase 4: finalize
        self.taxonomy.build_status = Some(BuildPhase::Complete.as_str().to_string());
        self.save_output()?;
        sink.phase(BuildPhase::Complete, "BUILD COMPLETE");

        let elapsed = start.elapsed();
        let outcome = self.outcome(false, total_split, elapsed);
        self.print_summary(&sink, &outcome);
        Ok(outcome)
    }

    // =========================================================================
    // Resume decision
    // =========================================================================

    /// Decide what smart resume should do.
    pub fn evaluate_resume(&self) -> ResumeAction {
        let output_dir = &self.config.output_dir;
        let taxonomy_path = output_dir.join("taxonomy.json");
        let config_path = output_dir.join("build_config.json");
        if !taxonomy_path.exists() || !config_path.exists() {
            return ResumeAction::Rebuild;
        }
        if !self.config.matches_saved_config(&config_path) {
            tracing::info!("Build config mismatch — cannot resume");
            return ResumeAction::Rebuild;
        }
        match read_json::<TaxonomyFile>(&taxonomy_path) {
            Ok(t) => match t.build_status.as_deref().unwrap_or("complete") {
                "complete" => ResumeAction::Skip,
                _ => ResumeAction::Resume,
            },
            Err(_) => ResumeAction::Rebuild,
        }
    }

    // =========================================================================
    // Setup helpers
    // =========================================================================

    fn clean_output(&self, preserve_keywords: bool, sink: &BuildSink) -> Result<()> {
        let output_dir = &self.config.output_dir;
        if !output_dir.exists() {
            return Ok(());
        }
        for name in BUILD_FILES {
            if preserve_keywords && *name == "keywords.json" {
                continue;
            }
            let p = output_dir.join(name);
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
            }
        }
        sink.log(format!(
            "Cleaned build outputs{}: {}",
            if preserve_keywords {
                " (preserved keywords.json)"
            } else {
                ""
            },
            output_dir.display()
        ));
        Ok(())
    }

    fn init_taxonomy(&mut self) {
        let mut all_ids: Vec<String> = self.services_index.keys().cloned().collect();
        all_ids.sort();
        let mut categories = IndexMap::new();
        categories.insert(
            ROOT_ID.to_string(),
            CategoryNode {
                children: Vec::new(),
                services: all_ids,
            },
        );
        self.taxonomy = TaxonomyFile {
            categories,
            ..Default::default()
        };
        let mut class_categories = IndexMap::new();
        class_categories.insert(
            ROOT_ID.to_string(),
            CategoryInfo::new(
                "All API Services",
                "Root node containing all API services across all functional domains",
            ),
        );
        self.class_data = ClassFile {
            categories: class_categories,
            ..Default::default()
        };
        self.all_assignments = Assignments::new();
    }

    fn load_existing_output(&mut self) -> Result<()> {
        let output_dir = &self.config.output_dir;
        self.taxonomy = read_json(&output_dir.join("taxonomy.json"))?;
        self.class_data = read_json(&output_dir.join("class.json"))?;
        let assignments_path = output_dir.join("assignments.json");
        if assignments_path.exists() {
            self.all_assignments = read_json(&assignments_path)?;
        }
        tracing::info!(
            "Loaded existing state: {} categories",
            self.taxonomy.categories.len()
        );
        Ok(())
    }

    fn load_services_index(&mut self, sink: &BuildSink) -> Result<()> {
        sink.log(format!(
            "Loading services from {}...",
            self.config.service_path.display()
        ));
        let services = load_services(&self.config.service_path)?;
        self.services_index = index_services(services);
        sink.log(format!("Loaded {} services", self.services_index.len()));
        Ok(())
    }

    // =========================================================================
    // BFS splitting
    // =========================================================================

    /// Oversized leaf nodes that still need splitting, with their depth.
    pub fn find_pending_nodes(&self) -> VecDeque<(String, u32)> {
        let mut queue = VecDeque::new();
        self.scan_pending(ROOT_ID, 0, &mut queue);
        queue
    }

    fn scan_pending(&self, node_id: &str, depth: u32, queue: &mut VecDeque<(String, u32)>) {
        let children = self.taxonomy.children(node_id);
        if !children.is_empty() {
            for child in children {
                self.scan_pending(child, depth + 1, queue);
            }
        } else if self.taxonomy.services(node_id).len() > self.config.max_service_size
            && self.config.max_depth.is_none_or(|m| depth < m)
        {
            queue.push_back((node_id.to_string(), depth));
        }
    }

    async fn run_bfs(
        &mut self,
        mut queue: VecDeque<(String, u32)>,
        sink: &BuildSink,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let mut total_split = 0usize;
        let mut queue_processed = 0usize;
        let max_depth = self.config.max_depth;

        while let Some((node_id, depth)) = queue.pop_front() {
            self.check_cancelled(cancel)?;
            let service_ids = self.taxonomy.services(&node_id).to_vec();
            let node_services: Vec<_> = service_ids
                .iter()
                .filter_map(|sid| self.services_index.get(sid).cloned())
                .collect();
            if node_services.is_empty() {
                sink.log(format!("Skipping {node_id}: no valid services found in index"));
                continue;
            }

            let is_root = node_id == ROOT_ID;
            let parent_info = self.get_node_info(&node_id);
            queue_processed += 1;
            sink.log(format!(
                "SPLITTING [{queue_processed}, {} queued]: {node_id} ({}) - {} services (depth={depth})",
                queue.len(),
                parent_info.name_or("Unknown"),
                node_services.len()
            ));

            let result = {
                let splitter = NodeSplitter::new(Arc::clone(&self.llm), &self.config, sink);
                splitter
                    .split_node(&node_id, &parent_info, &node_services, is_root)
                    .await?
            };

            self.apply_split_result(&node_id, &result, sink);
            self.all_assignments.extend(result.assignments.clone());
            total_split += 1;

            self.save_output()?;
            sink.log("Intermediate save complete");

            let child_depth = depth + 1;
            let mut sub_ids: Vec<&String> = result.subcategories.keys().collect();
            sub_ids.sort();
            for sub_id in sub_ids {
                let n = self.taxonomy.services(sub_id).len();
                if n > self.config.max_service_size && max_depth.is_none_or(|m| child_depth < m) {
                    queue.push_back((sub_id.clone(), child_depth));
                    sink.log(format!(
                        "-> {sub_id} has {n} services, queued for splitting (depth={child_depth})"
                    ));
                }
            }
        }
        Ok(total_split)
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn get_node_info(&self, node_id: &str) -> NodeInfo {
        NodeInfo::new(
            node_id,
            self.class_data.info(node_id).cloned().unwrap_or_default(),
        )
    }

    fn apply_split_result(&mut self, node_id: &str, result: &NodeSplitResult, sink: &BuildSink) {
        let mut sub_ids: Vec<String> = result.subcategories.keys().cloned().collect();
        sub_ids.sort();

        let mut sub_services: IndexMap<String, Vec<String>> =
            sub_ids.iter().map(|id| (id.clone(), Vec::new())).collect();
        for (svc_id, assignment) in &result.assignments {
            for cat_id in &assignment.category_ids {
                if let Some(list) = sub_services.get_mut(cat_id) {
                    list.push(svc_id.clone());
                }
            }
        }

        let mut unclassified = result.unclassified_service_ids.clone();
        unclassified.sort();
        let parent = self.taxonomy.categories.entry(node_id.to_string()).or_default();
        parent.children = sub_ids.clone();
        parent.services = unclassified;

        for sub_id in &sub_ids {
            let mut services = sub_services.get(sub_id).cloned().unwrap_or_default();
            services.sort();
            self.taxonomy.categories.insert(
                sub_id.clone(),
                CategoryNode {
                    children: Vec::new(),
                    services,
                },
            );
            let def = &result.subcategories[sub_id];
            let mut info = CategoryInfo::new(def.name.clone(), def.description.clone());
            if !def.boundary.is_empty() {
                info.boundary = Some(def.boundary.clone());
            }
            if !def.decision_rule.is_empty() {
                info.decision_rule = Some(def.decision_rule.clone());
            }
            self.class_data.categories.insert(sub_id.clone(), info);
        }

        sink.log(format!(
            "Split result for {node_id}: {} subcategories created, {} unclassified remain at parent",
            sub_ids.len(),
            result.unclassified_service_ids.len()
        ));
        for sub_id in &sub_ids {
            let n = sub_services.get(sub_id).map(Vec::len).unwrap_or(0);
            sink.log(format!(
                "  {sub_id} ({}): {n} services",
                result.subcategories[sub_id].name
            ));
        }
    }

    // =========================================================================
    // Cross-domain
    // =========================================================================

    async fn apply_cross_domain(&mut self, sink: &BuildSink) {
        sink.log("Cross-Domain Multi-Parent Assignment");
        let additions = {
            let assigner = CrossDomainAssigner::new(Arc::clone(&self.llm), self.config.workers, sink);
            assigner
                .assign(&self.taxonomy, &self.class_data, &self.services_index)
                .await
        };
        if additions.is_empty() {
            sink.log("No cross-domain additions");
            return;
        }
        let mut added_count = 0usize;
        for (svc_id, target_cat_ids) in &additions {
            for cat_id in target_cat_ids {
                if let Some(node) = self.taxonomy.categories.get_mut(cat_id) {
                    if !node.services.contains(svc_id) {
                        node.services.push(svc_id.clone());
                        node.services.sort();
                        added_count += 1;
                    }
                }
            }
        }
        sink.log(format!(
            "Cross-domain: added {added_count} service-category links for {} services",
            additions.len()
        ));
    }

    // =========================================================================
    // Output
    // =========================================================================

    /// Save taxonomy.json, class.json, assignments.json and build_config.json.
    fn save_output(&mut self) -> Result<()> {
        let output_dir = self.config.output_dir.clone();
        std::fs::create_dir_all(&output_dir).map_err(|e| Error::io(&output_dir, e))?;
        write_json(&output_dir.join("taxonomy.json"), &self.taxonomy)?;
        write_json(&output_dir.join("class.json"), &self.class_data)?;
        if !self.all_assignments.is_empty() {
            write_json(&output_dir.join("assignments.json"), &self.all_assignments)?;
        }
        self.config.service_hash = Some(compute_service_hash(self.services_index.values()));
        self.config.save(&output_dir.join("build_config.json"))
    }

    fn outcome(&self, skipped: bool, nodes_split: usize, elapsed: Duration) -> BuildOutcome {
        BuildOutcome {
            skipped,
            nodes_split,
            elapsed,
            output_dir: self.config.output_dir.clone(),
            taxonomy: self.taxonomy.clone(),
            class_data: self.class_data.clone(),
            summary: summarize(&self.taxonomy),
        }
    }

    fn print_summary(&self, sink: &BuildSink, outcome: &BuildOutcome) {
        let s = &outcome.summary;
        sink.log("BUILD COMPLETE");
        sink.log(format!("Nodes split: {}", outcome.nodes_split));
        sink.log(format!(
            "Total categories: {} (including {} leaf nodes)",
            s.total_categories, s.leaf_nodes
        ));
        sink.log(format!("Max depth: {}", s.max_depth));
        let dist = s
            .depth_distribution
            .iter()
            .map(|(d, n)| format!("{d}: {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        sink.log(format!("Depth distribution: {{{dist}}}"));
        sink.log(format!(
            "Total service assignments: {}",
            s.total_service_assignments
        ));
        sink.log(format!("Elapsed: {:.1}s", outcome.elapsed.as_secs_f64()));

        sink.log("Top-level categories:");
        let mut root_children = self.taxonomy.children(ROOT_ID).to_vec();
        root_children.sort();
        for cat_id in root_children {
            let name = self.class_data.name_of(&cat_id);
            let n_direct = self.taxonomy.services(&cat_id).len();
            let children = self.taxonomy.children(&cat_id);
            if children.is_empty() {
                sink.log(format!("  {name}: {n_direct} services (leaf)"));
            } else {
                let n_sub: usize = children.iter().map(|c| self.taxonomy.services(c).len()).sum();
                sink.log(format!(
                    "  {name}: {} subcategories, {n_direct} direct + {n_sub} in subs",
                    children.len()
                ));
            }
        }
    }
}

/// Structural summary of a taxonomy (root excluded from counts).
pub fn summarize(taxonomy: &TaxonomyFile) -> BuildSummary {
    let mut depth_counts: IndexMap<usize, usize> = IndexMap::new();
    let mut total_services = 0usize;
    fn walk(
        t: &TaxonomyFile,
        id: &str,
        depth: usize,
        counts: &mut IndexMap<usize, usize>,
        total: &mut usize,
    ) {
        *total += t.services(id).len();
        if id != ROOT_ID {
            *counts.entry(depth).or_insert(0) += 1;
        }
        for child in t.children(id) {
            walk(t, child, depth + 1, counts, total);
        }
    }
    walk(taxonomy, ROOT_ID, 0, &mut depth_counts, &mut total_services);
    let n_cats = taxonomy.categories.len().saturating_sub(1);
    let n_leaf = taxonomy
        .categories
        .iter()
        .filter(|(id, node)| *id != ROOT_ID && node.children.is_empty())
        .count();
    let max_depth = depth_counts.keys().copied().max().unwrap_or(0);
    let mut depth_distribution: Vec<(usize, usize)> = depth_counts.into_iter().collect();
    depth_distribution.sort();
    BuildSummary {
        total_categories: n_cats,
        leaf_nodes: n_leaf,
        max_depth,
        depth_distribution,
        total_service_assignments: total_services,
    }
}

/// SHA256 over sorted `(name, description)` pairs, order independent.
///
/// Byte-compatible with `_compute_build_hash` in the Python registry:
/// the hashed text is `json.dumps(pairs, ensure_ascii=False)`.
pub fn compute_service_hash<'a>(
    services: impl IntoIterator<Item = &'a crate::taxonomy::ServiceRecord>,
) -> String {
    let mut pairs: Vec<(String, String)> = services
        .into_iter()
        .map(|s| (s.name.clone(), s.description_text().to_string()))
        .collect();
    pairs.sort();
    let encoded = pairs
        .iter()
        .map(|(n, d)| {
            format!(
                "[{}, {}]",
                serde_json::to_string(n).unwrap_or_default(),
                serde_json::to_string(d).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!("[{encoded}]");
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taxonomy::ServiceRecord;
    use ap_support::testing::TestResult;

    #[test]
    fn service_hash_matches_python_encoding() -> TestResult {
        // python: hashlib.sha256(json.dumps([["A", "x"], ["B", "y \"q\""]], ensure_ascii=False).encode()).hexdigest()
        let services = vec![
            ServiceRecord::new("B", "B", "y \"q\""),
            ServiceRecord::new("A", "A", "x"),
        ];
        let h = compute_service_hash(&services);
        let expected = hex::encode(Sha256::digest(r#"[["A", "x"], ["B", "y \"q\""]]"#.as_bytes()));
        assert_eq!(h, expected);
        assert_eq!(
            compute_service_hash(&[]),
            hex::encode(Sha256::digest("[]".as_bytes()))
        );
        let cn = vec![ServiceRecord::new("1", "天气", "查询\n")];
        assert_eq!(
            compute_service_hash(&cn),
            hex::encode(Sha256::digest("[[\"天气\", \"查询\\n\"]]".as_bytes()))
        );
        Ok(())
    }

    #[test]
    fn resume_mode_parsing() -> TestResult {
        assert_eq!("keyword".parse::<ResumeMode>()?, ResumeMode::Keyword);
        assert!("maybe".parse::<ResumeMode>().is_err());
        Ok(())
    }
}
