// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Vector search evaluator with metrics at several K values.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use a2x_common::compute_set_metrics;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{f1, progress_line};
use crate::error::{Error, Result};
use crate::taxonomy::{QueryObject, load_queries};
use crate::util::write_json;
use crate::vector::{
    EmbeddingModel, VectorSearch, VectorSearchConfig, hit_at_k, mrr, ndcg_at_k, precision_at_k, recall_at_k,
};

/// Ranking metrics at one K.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsAtK {
    pub precision: f64,
    pub recall: f64,
    pub hit: f64,
    pub mrr: f64,
    pub ndcg: f64,
}

/// Per-query metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorQueryMetrics {
    pub query_id: String,
    pub query: String,
    pub expected_count: usize,
    pub found_count: usize,
    pub correct_count: usize,
    pub precision: f64,
    pub recall: f64,
    pub hit: bool,
    pub expected_tools: Vec<String>,
    pub found_tools: Vec<String>,
    pub correct_tools: Vec<String>,
    pub missed_tools: Vec<String>,
    pub wrong_tools: Vec<String>,
    /// Metrics keyed by K as a string (`"5"`, `"10"`).
    pub metrics_at_k: IndexMap<String, MetricsAtK>,
}

/// Overall metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorOverallMetrics {
    pub total_queries: usize,
    pub avg_precision: f64,
    pub avg_recall: f64,
    pub hit_rate: f64,
    pub avg_f1: f64,
    pub avg_found_services: f64,
    pub metrics_at_k: IndexMap<String, MetricsAtK>,
}

/// Evaluator for vector search.
pub struct VectorEvaluator {
    searcher: VectorSearch,
    top_k: usize,
    top_k_list: Vec<usize>,
}

impl VectorEvaluator {
    /// Build the searcher (and its index) from `config`.
    pub async fn new(
        config: VectorSearchConfig,
        top_k: usize,
        top_k_list: Option<Vec<usize>>,
        model: Arc<dyn EmbeddingModel>,
    ) -> Result<Self> {
        Ok(Self::with_searcher(
            VectorSearch::new(config, model).await?,
            top_k,
            top_k_list,
        ))
    }

    pub fn with_searcher(searcher: VectorSearch, top_k: usize, top_k_list: Option<Vec<usize>>) -> Self {
        Self {
            searcher,
            top_k,
            top_k_list: top_k_list.unwrap_or_else(|| vec![5, 10]),
        }
    }

    pub fn searcher(&self) -> &VectorSearch {
        &self.searcher
    }

    /// Evaluate one query; `top_k` overrides the default.
    pub async fn evaluate_single_query(
        &self,
        query_obj: &QueryObject,
        top_k: Option<usize>,
    ) -> Result<VectorQueryMetrics> {
        let k = top_k.unwrap_or(self.top_k);
        let expected_tools = query_obj.expected_tools();
        let max_k = self.top_k_list.iter().copied().max().unwrap_or(k);
        let (results, _) = self.searcher.search(&query_obj.query, max_k).await?;
        let all_found: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
        let found_tools: Vec<String> = all_found.iter().take(k).cloned().collect();
        let m = compute_set_metrics(&expected_tools, &found_tools);
        let expected_set: HashSet<String> = expected_tools.iter().cloned().collect();
        let mut metrics_at_k = IndexMap::new();
        for eval_k in &self.top_k_list {
            metrics_at_k.insert(
                eval_k.to_string(),
                MetricsAtK {
                    precision: precision_at_k(&all_found, &expected_set, *eval_k),
                    recall: recall_at_k(&all_found, &expected_set, *eval_k),
                    hit: hit_at_k(&all_found, &expected_set, *eval_k),
                    mrr: mrr(&all_found, &expected_set),
                    ndcg: ndcg_at_k(&all_found, &expected_set, *eval_k),
                },
            );
        }
        Ok(VectorQueryMetrics {
            query_id: query_obj.id.clone(),
            query: query_obj.query.clone(),
            expected_count: expected_tools.len(),
            found_count: found_tools.len(),
            correct_count: m.correct.len(),
            precision: m.precision,
            recall: m.recall,
            hit: m.hit,
            expected_tools,
            found_tools,
            correct_tools: m.correct,
            missed_tools: m.missed,
            wrong_tools: m.wrong,
            metrics_at_k,
        })
    }

    /// Evaluate a query file sequentially; writes result files when
    /// `output_dir` is set.
    pub async fn evaluate_batch(
        &self,
        query_file: &Path,
        max_queries: Option<usize>,
        output_dir: Option<&Path>,
    ) -> Result<(Vec<VectorQueryMetrics>, VectorOverallMetrics)> {
        let queries = load_queries(query_file, max_queries)?;
        tracing::info!("Evaluating {} queries (top_k={})...", queries.len(), self.top_k);
        let start = Instant::now();
        let mut query_metrics = Vec::with_capacity(queries.len());
        for (i, q) in queries.iter().enumerate() {
            query_metrics.push(self.evaluate_single_query(q, None).await?);
            progress_line("Evaluating", i + 1, queries.len(), "");
        }
        let elapsed = start.elapsed().as_secs_f64();
        let overall = compute_overall(&query_metrics);
        if !query_metrics.is_empty() {
            self.print_results(&overall, elapsed);
        }
        if let Some(dir) = output_dir {
            self.save_results(dir, &query_metrics, &overall, elapsed, query_file)?;
        }
        Ok((query_metrics, overall))
    }

    fn print_results(&self, overall: &VectorOverallMetrics, elapsed: f64) {
        let sep = "=".repeat(80);
        tracing::info!("");
        tracing::info!("{sep}");
        tracing::info!("Total Queries: {}", overall.total_queries);
        tracing::info!("Time Elapsed: {elapsed:.2}s");
        tracing::info!("");
        tracing::info!("Performance Metrics (top_k={}):", self.top_k);
        tracing::info!("  Precision: {:.4}", overall.avg_precision);
        tracing::info!("  Recall:    {:.4}", overall.avg_recall);
        tracing::info!("  Hit Rate:  {:.4}", overall.hit_rate);
        tracing::info!("  F1 Score:  {:.4}", overall.avg_f1);
        tracing::info!("");
        tracing::info!(
            "{:>5} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10}",
            "K",
            "P@K",
            "R@K",
            "Hit@K",
            "MRR",
            "NDCG"
        );
        tracing::info!("{}", "-".repeat(70));
        let mut keys: Vec<&String> = overall.metrics_at_k.keys().collect();
        keys.sort_by_key(|k| k.parse::<usize>().unwrap_or(0));
        for k in keys {
            let m = &overall.metrics_at_k[k];
            tracing::info!(
                "{:>5} | {:>10.4} | {:>10.4} | {:>10.4} | {:>10.4} | {:>10.4}",
                k,
                m.precision,
                m.recall,
                m.hit,
                m.mrr,
                m.ndcg
            );
        }
        tracing::info!("{sep}");
    }

    fn save_results(
        &self,
        output_dir: &Path,
        query_metrics: &[VectorQueryMetrics],
        overall: &VectorOverallMetrics,
        elapsed: f64,
        query_file: &Path,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir).map_err(|e| Error::io(output_dir, e))?;
        let summary = json!({
            "total_queries": overall.total_queries,
            "top_k": self.top_k,
            "precision": overall.avg_precision,
            "recall": overall.avg_recall,
            "hit_rate": overall.hit_rate,
            "f1": overall.avg_f1,
            "elapsed_time": elapsed,
            "metrics_at_k": overall.metrics_at_k,
        });
        write_json(&output_dir.join("summary.json"), &summary)?;
        let detailed = json!({
            "overall_metrics": overall,
            "query_metrics": query_metrics,
            "elapsed_time": elapsed,
        });
        write_json(&output_dir.join("evaluation_results.json"), &detailed)?;
        let config = json!({
            "query_file": query_file.to_string_lossy(),
            "top_k": self.top_k,
            "top_k_list": self.top_k_list,
            "embedding_model": self.searcher.model_name(),
            "query_count": overall.total_queries,
        });
        write_json(&output_dir.join("config.json"), &config)?;
        tracing::info!("Results saved to: {}", output_dir.display());
        Ok(())
    }
}

/// Aggregate per-query metrics (all zero for an empty list).
pub fn compute_overall(query_metrics: &[VectorQueryMetrics]) -> VectorOverallMetrics {
    let n = query_metrics.len();
    if n == 0 {
        return VectorOverallMetrics::default();
    }
    let nf = n as f64;
    let avg_precision = query_metrics.iter().map(|m| m.precision).sum::<f64>() / nf;
    let avg_recall = query_metrics.iter().map(|m| m.recall).sum::<f64>() / nf;
    let mut agg = IndexMap::new();
    for k in query_metrics[0].metrics_at_k.keys() {
        let get = |f: fn(&MetricsAtK) -> f64| {
            query_metrics
                .iter()
                .map(|m| m.metrics_at_k.get(k).map(f).unwrap_or(0.0))
                .sum::<f64>()
                / nf
        };
        agg.insert(
            k.clone(),
            MetricsAtK {
                precision: get(|m| m.precision),
                recall: get(|m| m.recall),
                hit: get(|m| m.hit),
                mrr: get(|m| m.mrr),
                ndcg: get(|m| m.ndcg),
            },
        );
    }
    VectorOverallMetrics {
        total_queries: n,
        avg_precision,
        avg_recall,
        hit_rate: query_metrics.iter().filter(|m| m.hit).count() as f64 / nf,
        avg_f1: f1(avg_precision, avg_recall),
        avg_found_services: query_metrics.iter().map(|m| m.found_count as f64).sum::<f64>() / nf,
        metrics_at_k: agg,
    }
}
