// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Traditional (MCP style) search evaluator.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use a2x_common::{LlmBackend, compute_set_metrics};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{f1, progress_line};
use crate::error::{Error, Result};
use crate::taxonomy::{QueryObject, load_queries};
use crate::traditional::TraditionalSearch;
use crate::util::{run_bounded, write_json};

/// Per-query metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TraditionalQueryMetrics {
    pub query_id: String,
    pub query: String,
    pub expected_count: usize,
    pub found_count: usize,
    pub correct_count: usize,
    pub precision: f64,
    pub recall: f64,
    pub hit: bool,
    pub llm_calls: u64,
    pub total_tokens: u64,
    pub expected_tools: Vec<String>,
    pub found_tools: Vec<String>,
    pub correct_tools: Vec<String>,
    pub missed_tools: Vec<String>,
    pub wrong_tools: Vec<String>,
}

/// Overall metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TraditionalOverallMetrics {
    pub total_queries: usize,
    pub avg_precision: f64,
    pub avg_recall: f64,
    pub hit_rate: f64,
    pub avg_f1: f64,
    pub total_llm_calls: u64,
    pub total_tokens: u64,
    pub avg_llm_calls: f64,
    pub avg_tokens: f64,
    pub avg_found_services: f64,
}

/// Evaluator for the full-context baseline.
pub struct TraditionalEvaluator {
    searcher: Arc<TraditionalSearch>,
    max_workers: usize,
}

impl TraditionalEvaluator {
    pub fn new(service_path: &Path, max_workers: usize, llm: Arc<dyn LlmBackend>) -> Result<Self> {
        Ok(Self::with_searcher(
            Arc::new(TraditionalSearch::new(service_path, llm)?),
            max_workers,
        ))
    }

    pub fn with_searcher(searcher: Arc<TraditionalSearch>, max_workers: usize) -> Self {
        Self {
            searcher,
            max_workers,
        }
    }

    pub async fn evaluate_single_query(&self, query_obj: &QueryObject) -> TraditionalQueryMetrics {
        evaluate_one(&self.searcher, query_obj).await
    }

    /// Evaluate a query file; writes result files when `output_dir` is set.
    pub async fn evaluate_batch(
        &self,
        query_file: &Path,
        max_queries: Option<usize>,
        output_dir: Option<&Path>,
    ) -> Result<(Vec<TraditionalQueryMetrics>, TraditionalOverallMetrics)> {
        let queries = load_queries(query_file, max_queries)?;
        tracing::info!("=== Traditional (MCP-style) Evaluation ===");
        tracing::info!("Services: {}", self.searcher.service_count());
        tracing::info!("Queries: {}", queries.len());
        tracing::info!("Workers: {}", self.max_workers);

        let start = Instant::now();
        let total = queries.len();
        let jobs: Vec<_> = queries
            .into_iter()
            .enumerate()
            .map(|(idx, q)| {
                let searcher = Arc::clone(&self.searcher);
                async move { (idx, evaluate_one(&searcher, &q).await) }
            })
            .collect();
        let mut results_with_idx: Vec<(usize, TraditionalQueryMetrics)> = Vec::new();
        run_bounded(self.max_workers, jobs, |(idx, m)| {
            results_with_idx.push((idx, m));
            let n = results_with_idx.len();
            let recall = results_with_idx.iter().map(|(_, m)| m.recall).sum::<f64>() / n as f64;
            let hits = results_with_idx.iter().filter(|(_, m)| m.hit).count();
            progress_line(
                "Evaluating",
                n,
                total,
                &format!("recall={:.2}% hits={hits}", recall * 100.0),
            );
        })
        .await;
        results_with_idx.sort_by_key(|(i, _)| *i);
        let query_metrics: Vec<TraditionalQueryMetrics> =
            results_with_idx.into_iter().map(|(_, m)| m).collect();
        let elapsed = start.elapsed().as_secs_f64();

        let overall = compute_overall(&query_metrics);
        print_results(&overall, elapsed);
        if let Some(dir) = output_dir {
            self.save_results(dir, &query_metrics, &overall, elapsed, query_file)?;
        }
        Ok((query_metrics, overall))
    }

    fn save_results(
        &self,
        output_dir: &Path,
        query_metrics: &[TraditionalQueryMetrics],
        overall: &TraditionalOverallMetrics,
        elapsed: f64,
        query_file: &Path,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir).map_err(|e| Error::io(output_dir, e))?;
        let summary = json!({
            "method": "traditional",
            "total_queries": overall.total_queries,
            "precision": overall.avg_precision,
            "recall": overall.avg_recall,
            "hit_rate": overall.hit_rate,
            "f1": overall.avg_f1,
            "avg_llm_calls": overall.avg_llm_calls,
            "avg_tokens": overall.avg_tokens,
            "avg_found_services": overall.avg_found_services,
            "elapsed_time": elapsed,
        });
        write_json(&output_dir.join("summary.json"), &summary)?;
        let detailed = json!({
            "overall_metrics": overall,
            "query_metrics": query_metrics,
            "elapsed_time": elapsed,
        });
        write_json(&output_dir.join("evaluation_results.json"), &detailed)?;
        let config = json!({
            "method": "traditional",
            "query_file": query_file.to_string_lossy(),
            "service_count": self.searcher.service_count(),
            "query_count": overall.total_queries,
        });
        write_json(&output_dir.join("config.json"), &config)?;
        tracing::info!("Results saved to: {}", output_dir.display());
        Ok(())
    }
}

async fn evaluate_one(searcher: &TraditionalSearch, query_obj: &QueryObject) -> TraditionalQueryMetrics {
    let expected_tools = query_obj.expected_tools();
    let (results, stats) = searcher.search(&query_obj.query).await;
    let found_tools: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let m = compute_set_metrics(&expected_tools, &found_tools);
    TraditionalQueryMetrics {
        query_id: query_obj.id.clone(),
        query: query_obj.query.clone(),
        expected_count: expected_tools.len(),
        found_count: found_tools.len(),
        correct_count: m.correct.len(),
        precision: m.precision,
        recall: m.recall,
        hit: m.hit,
        llm_calls: stats.llm_calls,
        total_tokens: stats.total_tokens,
        expected_tools,
        found_tools,
        correct_tools: m.correct,
        missed_tools: m.missed,
        wrong_tools: m.wrong,
    }
}

/// Aggregate per-query metrics.
pub fn compute_overall(query_metrics: &[TraditionalQueryMetrics]) -> TraditionalOverallMetrics {
    let n = query_metrics.len();
    if n == 0 {
        return TraditionalOverallMetrics::default();
    }
    let nf = n as f64;
    let total_llm_calls: u64 = query_metrics.iter().map(|m| m.llm_calls).sum();
    let total_tokens: u64 = query_metrics.iter().map(|m| m.total_tokens).sum();
    let avg_precision = query_metrics.iter().map(|m| m.precision).sum::<f64>() / nf;
    let avg_recall = query_metrics.iter().map(|m| m.recall).sum::<f64>() / nf;
    TraditionalOverallMetrics {
        total_queries: n,
        avg_precision,
        avg_recall,
        hit_rate: query_metrics.iter().filter(|m| m.hit).count() as f64 / nf,
        avg_f1: f1(avg_precision, avg_recall),
        total_llm_calls,
        total_tokens,
        avg_llm_calls: total_llm_calls as f64 / nf,
        avg_tokens: total_tokens as f64 / nf,
        avg_found_services: query_metrics.iter().map(|m| m.found_count as f64).sum::<f64>() / nf,
    }
}

fn print_results(overall: &TraditionalOverallMetrics, elapsed: f64) {
    let sep = "=".repeat(80);
    tracing::info!("");
    tracing::info!("{sep}");
    tracing::info!("Traditional (MCP-style) Evaluation Results");
    tracing::info!("{sep}");
    tracing::info!("Total Queries: {}", overall.total_queries);
    tracing::info!("Time Elapsed: {elapsed:.2}s");
    tracing::info!("");
    tracing::info!("Performance Metrics:");
    tracing::info!("  Precision: {:.4}", overall.avg_precision);
    tracing::info!("  Recall:    {:.4}", overall.avg_recall);
    tracing::info!("  Hit Rate:  {:.4}", overall.hit_rate);
    tracing::info!("  F1 Score:  {:.4}", overall.avg_f1);
    tracing::info!("");
    tracing::info!("LLM Usage:");
    tracing::info!("  Avg LLM Calls: {:.2}", overall.avg_llm_calls);
    tracing::info!("  Avg Tokens/Query: {:.0}", overall.avg_tokens);
    tracing::info!("");
    tracing::info!("Search Stats:");
    tracing::info!("  Avg Found Services: {:.2}", overall.avg_found_services);
    tracing::info!("{sep}");
}
