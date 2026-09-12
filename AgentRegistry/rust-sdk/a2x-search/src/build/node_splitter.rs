// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Per-node classify/refine loop.
//!
//! 1. Design subcategories (via [`CategoryDesigner`])
//! 2. Classify all services (parallel, one LLM call each)
//! 3. Check convergence, refine if needed, repeat
//! 4. Handle generic, unclassified and tiny subcategories
//!
//! Rules (`generic_ratio` based):
//! - Normal: `1 <= n_cats <= N * generic_ratio` (accepted, multi-parent OK)
//! - Generic: `n_cats > N * generic_ratio` (left at parent)
//! - Unclassified: `n_cats == 0` (left at parent)
//! - Tiny subcategories: `<= delete_threshold` services after iterations
//!   are deleted

use std::collections::HashSet;
use std::sync::Arc;

use a2x_common::{ChatMessage, LlmBackend, parse_json_response};
use indexmap::IndexMap;
use serde_json::Value;

use super::category_designer::CategoryDesigner;
use super::config::AutoHierarchicalConfig;
use super::progress::BuildSink;
use super::prompts::{
    Assignment, Assignments, CLASSIFY_SERVICE_IN_NODE_TEMPLATE, ClassificationResult, ClassificationStats,
    NodeInfo, NodeSplitResult, SYSTEM_CLASSIFY_NODE, Subcategories, fill, format_categories_for_prompt,
    generic_threshold,
};
use crate::error::Result;
use crate::taxonomy::ServiceRecord;
use crate::util::run_bounded;

/// Orchestrates the full subdivision of a single taxonomy node.
pub struct NodeSplitter<'a> {
    llm: Arc<dyn LlmBackend>,
    config: &'a AutoHierarchicalConfig,
    sink: &'a BuildSink,
}

impl<'a> NodeSplitter<'a> {
    pub fn new(llm: Arc<dyn LlmBackend>, config: &'a AutoHierarchicalConfig, sink: &'a BuildSink) -> Self {
        Self { llm, config, sink }
    }

    /// Subdivide a node: design, classify/refine loop, finalize.
    pub async fn split_node(
        &self,
        node_id: &str,
        parent_info: &NodeInfo,
        services: &[ServiceRecord],
        is_root: bool,
    ) -> Result<NodeSplitResult> {
        let max_cats = self.config.get_max_categories();
        let designer = CategoryDesigner::new(self.llm.as_ref(), self.config, self.sink);

        self.sink.log(format!(
            "Designing subcategories for {node_id} ({} services, max_cats={max_cats})...",
            services.len()
        ));
        let subcategories = designer
            .design_categories(
                services,
                if is_root { None } else { Some(parent_info) },
                is_root,
                Some(max_cats),
            )
            .await?;
        self.sink.log(format!(
            "Designed {} subcategories:\n{}",
            subcategories.len(),
            subcategory_lines(&subcategories)
        ));

        let (mut subcategories, mut assignments, iteration) = self
            .classify_refine_loop(&designer, node_id, parent_info, services, subcategories, is_root)
            .await;

        let unclassified_ids = self.finalize_assignments(&mut assignments, &mut subcategories);
        let converged = iteration < self.config.max_refine_iterations;
        self.sink.log(format!(
            "Node {node_id} complete: {} subcategories, {} unclassified",
            subcategories.len(),
            unclassified_ids.len()
        ));

        Ok(NodeSplitResult {
            node_id: node_id.to_string(),
            subcategories,
            assignments,
            unclassified_service_ids: unclassified_ids,
            iterations_used: iteration,
            converged,
        })
    }

    async fn classify_refine_loop(
        &self,
        designer: &CategoryDesigner<'_>,
        node_id: &str,
        parent_info: &NodeInfo,
        services: &[ServiceRecord],
        mut subcategories: Subcategories,
        is_root: bool,
    ) -> (Subcategories, Assignments, usize) {
        let max_refine = self.config.max_refine_iterations;
        let mut assignments = Assignments::new();
        let mut iteration = 0;
        for iter in 1..=max_refine {
            iteration = iter;
            self.sink.log(format!("--- Iteration {iter}/{max_refine} ---"));
            self.sink
                .log(format!("Classifying {} services...", services.len()));
            assignments = self.classify_all(services, &subcategories, parent_info).await;

            let stats = self.compute_stats(&assignments, &subcategories);
            self.sink.log(format!(
                "Results: {} normal, {} generic, {} unclassified",
                stats.n_normal, stats.n_generic, stats.n_unclassified
            ));
            self.log_distribution(&subcategories, &stats);

            if should_terminate(&stats, iter, max_refine) {
                self.sink
                    .log(format!("Node {node_id} converged at iteration {iter}"));
                break;
            }
            if iter < max_refine {
                self.sink.log("Refining subcategories...");
                subcategories = designer
                    .refine_categories(
                        parent_info,
                        &subcategories,
                        &assignments,
                        services,
                        &stats,
                        is_root,
                    )
                    .await;
                self.sink.log(format!(
                    "After refinement: {} subcategories\n{}",
                    subcategories.len(),
                    subcategory_lines(&subcategories)
                ));
            }
        }
        (subcategories, assignments, iteration)
    }

    /// Mark generic/unclassified services and delete tiny subcategories.
    /// Returns the ids that stay at the parent node.
    pub fn finalize_assignments(
        &self,
        assignments: &mut Assignments,
        subcategories: &mut Subcategories,
    ) -> Vec<String> {
        let threshold = generic_threshold(subcategories.len(), self.config.generic_ratio);
        let mut unclassified_ids: Vec<String> = Vec::new();
        for (svc_id, r) in assignments.iter_mut() {
            let n_cats = r.category_ids.len();
            if n_cats == 0 {
                unclassified_ids.push(svc_id.clone());
            } else if n_cats > threshold {
                r.category_ids.clear();
                unclassified_ids.push(svc_id.clone());
            }
        }

        let final_stats = self.compute_stats(assignments, subcategories);
        let mut tiny_to_delete: Vec<String> = final_stats
            .cat_counts
            .iter()
            .filter(|(sub_id, count)| {
                **count <= self.config.delete_threshold && subcategories.contains_key(*sub_id)
            })
            .map(|(sub_id, _)| sub_id.clone())
            .collect();
        tiny_to_delete.sort();

        if !tiny_to_delete.is_empty() {
            let del_lines: Vec<String> = tiny_to_delete
                .iter()
                .map(|sub_id| {
                    format!(
                        "  DELETE: {sub_id} ({}): {} services",
                        subcategories[sub_id].name,
                        final_stats.cat_counts.get(sub_id).copied().unwrap_or(0)
                    )
                })
                .collect();
            self.sink.log(format!(
                "Deleting {} tiny subcategories (<={} services):\n{}",
                tiny_to_delete.len(),
                self.config.delete_threshold,
                del_lines.join("\n")
            ));
            let tiny_set: HashSet<&String> = tiny_to_delete.iter().collect();
            for sub_id in &tiny_to_delete {
                let tiny_svc_ids: Vec<String> = assignments
                    .iter()
                    .filter(|(_, r)| r.category_ids.contains(sub_id))
                    .map(|(id, _)| id.clone())
                    .collect();
                for svc_id in tiny_svc_ids {
                    let r = &mut assignments[&svc_id];
                    r.category_ids.retain(|cid| !tiny_set.contains(cid));
                    if r.category_ids.is_empty() && !unclassified_ids.contains(&svc_id) {
                        unclassified_ids.push(svc_id);
                    }
                }
                subcategories.shift_remove(sub_id);
            }
        }
        unclassified_ids
    }

    fn log_distribution(&self, subcategories: &Subcategories, stats: &ClassificationStats) {
        let mut items: Vec<_> = subcategories.iter().collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        let lines: Vec<String> = items
            .into_iter()
            .map(|(sub_id, info)| {
                let count = stats.cat_counts.get(sub_id).copied().unwrap_or(0);
                let tiny = if count <= self.config.min_leaf_size {
                    " [TINY]"
                } else {
                    ""
                };
                format!("  {sub_id} ({}): {count} services{tiny}", info.name)
            })
            .collect();
        self.sink
            .log(format!("Per-subcategory distribution:\n{}", lines.join("\n")));
    }

    // =========================================================================
    // Classification
    // =========================================================================

    /// Classify all services into subcategories (one call each, parallel).
    pub async fn classify_all(
        &self,
        services: &[ServiceRecord],
        subcategories: &Subcategories,
        parent_info: &NodeInfo,
    ) -> Assignments {
        let subcategories_text = Arc::new(format_categories_for_prompt(subcategories));
        let valid_ids: Arc<HashSet<String>> = Arc::new(subcategories.keys().cloned().collect());
        let total = services.len();
        let mut results = Assignments::with_capacity(total);
        let parent_name = parent_info.name_or("Unknown").to_string();
        let parent_description = parent_info.description_or("").to_string();

        let jobs: Vec<_> = services
            .iter()
            .map(|svc| {
                let ctx = ClassifyContext {
                    llm: Arc::clone(&self.llm),
                    subcategories_text: Arc::clone(&subcategories_text),
                    valid_ids: Arc::clone(&valid_ids),
                    parent_name: parent_name.clone(),
                    parent_description: parent_description.clone(),
                    retries: self.config.classification_retries,
                    temperature: self.config.temperature_classify,
                    max_tokens: self.config.max_tokens_classify,
                };
                let service = svc.clone();
                async move { classify_single(&ctx, &service).await }
            })
            .collect();

        let mut completed = 0usize;
        let step = (total / 100).max(1);
        run_bounded(self.config.workers, jobs, |result: ClassificationResult| {
            results.insert(
                result.service_id.clone(),
                Assignment {
                    category_ids: result.category_ids,
                    reasoning: result.reasoning,
                },
            );
            completed += 1;
            if completed % step == 0 || completed == total {
                let n_ok = results.values().filter(|r| !r.category_ids.is_empty()).count();
                self.sink
                    .progress(completed, total, "", &format!("{n_ok} assigned"));
            }
        })
        .await;
        results
    }

    /// Classification statistics using `generic_ratio`.
    pub fn compute_stats(
        &self,
        assignments: &Assignments,
        subcategories: &Subcategories,
    ) -> ClassificationStats {
        let threshold = generic_threshold(subcategories.len(), self.config.generic_ratio);
        let mut stats = ClassificationStats {
            cat_counts: subcategories
                .keys()
                .map(|k| (k.clone(), 0usize))
                .collect::<IndexMap<_, _>>(),
            ..Default::default()
        };
        for result in assignments.values() {
            let n_cats = result.category_ids.len();
            if n_cats == 0 {
                stats.n_unclassified += 1;
            } else if n_cats > threshold {
                stats.n_generic += 1;
            } else {
                stats.n_normal += 1;
            }
            for cat_id in &result.category_ids {
                if let Some(c) = stats.cat_counts.get_mut(cat_id) {
                    *c += 1;
                }
            }
        }
        stats
    }
}

/// Terminate when max iterations are reached or nothing is generic or
/// unclassified.
pub fn should_terminate(stats: &ClassificationStats, iteration: usize, max_refine: usize) -> bool {
    iteration >= max_refine || (stats.n_generic == 0 && stats.n_unclassified == 0)
}

fn subcategory_lines(subcategories: &Subcategories) -> String {
    let mut items: Vec<_> = subcategories.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    items
        .into_iter()
        .map(|(id, info)| format!("  {id}: {}", info.name))
        .collect::<Vec<_>>()
        .join("\n")
}

struct ClassifyContext {
    llm: Arc<dyn LlmBackend>,
    subcategories_text: Arc<String>,
    valid_ids: Arc<HashSet<String>>,
    parent_name: String,
    parent_description: String,
    retries: u32,
    temperature: f32,
    max_tokens: u32,
}

/// Classify one service with retry logic.
async fn classify_single(ctx: &ClassifyContext, service: &ServiceRecord) -> ClassificationResult {
    let mut prompt = fill(
        CLASSIFY_SERVICE_IN_NODE_TEMPLATE,
        &[
            ("parent_name", &ctx.parent_name),
            ("parent_description", &ctx.parent_description),
            ("subcategories_text", &ctx.subcategories_text),
            ("service_description", service.description_or("No description")),
        ],
    );
    for attempt in 0..=ctx.retries {
        let response = ctx
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_CLASSIFY_NODE),
                    ChatMessage::user(prompt.clone()),
                ],
                ctx.temperature,
                Some(ctx.max_tokens),
            )
            .await;
        if !response.success {
            if attempt < ctx.retries {
                continue;
            }
            return ClassificationResult {
                service_id: service.id.clone(),
                success: false,
                error: Some(format!(
                    "LLM call failed: {}",
                    response.error.as_deref().unwrap_or("unknown")
                )),
                ..Default::default()
            };
        }
        if let Some(result) = parse_json_response(&response.content) {
            let cat_ids: Vec<String> = match result.get("category_ids") {
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                _ => Vec::new(),
            };
            let cat_ids = cat_ids
                .into_iter()
                .filter(|c| ctx.valid_ids.contains(c))
                .collect();
            return ClassificationResult {
                service_id: service.id.clone(),
                category_ids: cat_ids,
                reasoning: result
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                success: true,
                error: None,
            };
        }
        if attempt < ctx.retries {
            prompt = format!(
                "Your previous response was not valid JSON. Please output ONLY a JSON object, nothing else.\n\n{prompt}"
            );
        }
    }
    ClassificationResult {
        service_id: service.id.clone(),
        success: false,
        error: Some("Failed to parse response after retries".into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::prompts::SubcategoryDef;
    use crate::taxonomy::CategoryInfo;
    use crate::testing::FakeLlm;
    use ap_support::testing::TestResult;

    fn subs(ids: &[&str]) -> Subcategories {
        ids.iter()
            .map(|id| {
                (
                    id.to_string(),
                    SubcategoryDef {
                        name: id.to_uppercase(),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn classify_with_retry_and_string_ids() -> TestResult {
        let llm = FakeLlm::new()
            .on(
                "SERVICE DESCRIPTION:\nalpha",
                r#"{"reasoning": "r", "category_ids": "cat_sub1"}"#,
            )
            .on(
                "previous response was not valid JSON",
                r#"{"category_ids": ["cat_sub2", "bogus"]}"#,
            )
            .on("SERVICE DESCRIPTION:\nbeta", "not json at all");
        let mut config = AutoHierarchicalConfig::new("db/DS/service.json");
        config.workers = 2;
        let sink = BuildSink::null();
        let splitter = NodeSplitter::new(llm.clone_arc(), &config, &sink);
        let services = vec![
            ServiceRecord::new("a", "A", "alpha"),
            ServiceRecord::new("b", "B", "beta"),
        ];
        let parent = NodeInfo::new("root", CategoryInfo::new("All", "root"));
        let assignments = splitter
            .classify_all(&services, &subs(&["cat_sub1", "cat_sub2"]), &parent)
            .await;
        assert_eq!(assignments["a"].category_ids, vec!["cat_sub1"]);
        assert_eq!(assignments["a"].reasoning, "r");
        assert_eq!(assignments["b"].category_ids, vec!["cat_sub2"]);
        let stats = splitter.compute_stats(&assignments, &subs(&["cat_sub1", "cat_sub2"]));
        assert_eq!((stats.n_normal, stats.n_generic, stats.n_unclassified), (2, 0, 0));
        assert!(should_terminate(&stats, 1, 3));
        Ok(())
    }

    trait CloneArc {
        fn clone_arc(self) -> Arc<dyn LlmBackend>;
    }
    impl CloneArc for FakeLlm {
        fn clone_arc(self) -> Arc<dyn LlmBackend> {
            Arc::new(self)
        }
    }

    #[test]
    fn finalize_marks_generic_and_deletes_tiny() -> TestResult {
        let mut config = AutoHierarchicalConfig::new("db/DS/service.json");
        config.delete_threshold = 1;
        let sink = BuildSink::null();
        let llm: Arc<dyn LlmBackend> = Arc::new(FakeLlm::new());
        let splitter = NodeSplitter::new(llm, &config, &sink);
        let mut subcategories = subs(&["cat_sub1", "cat_sub2", "cat_sub3", "cat_sub4"]);
        let mut assignments = Assignments::new();
        let put = |a: &mut Assignments, id: &str, cats: &[&str]| {
            a.insert(
                id.into(),
                Assignment {
                    category_ids: cats.iter().map(|c| c.to_string()).collect(),
                    reasoning: String::new(),
                },
            );
        };
        put(&mut assignments, "s1", &["cat_sub1"]);
        put(&mut assignments, "s2", &["cat_sub1"]);
        put(&mut assignments, "s3", &["cat_sub1", "cat_sub2", "cat_sub3"]); // generic (>1)
        put(&mut assignments, "s4", &[]); // unclassified
        put(&mut assignments, "s5", &["cat_sub4"]); // tiny category
        let unclassified = splitter.finalize_assignments(&mut assignments, &mut subcategories);
        assert_eq!(unclassified, vec!["s3", "s4", "s5"]);
        assert!(assignments["s3"].category_ids.is_empty());
        assert!(assignments["s5"].category_ids.is_empty());
        assert_eq!(subcategories.keys().collect::<Vec<_>>(), vec!["cat_sub1"]);
        Ok(())
    }
}
