// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2X search evaluator with per-query checkpointing.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use a2x_common::{LlmBackend, compute_set_metrics};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::error_analysis::save_error_report;
use super::{f1, progress_line, today};
use crate::error::{Error, Result};
use crate::search::{A2xSearch, A2xSearchConfig};
use crate::taxonomy::{QueryObject, load_queries};
use crate::util::{run_bounded, write_json};

/// Metrics for a single query.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct A2xQueryMetrics {
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
    pub visited_categories: usize,
    pub pruned_categories: usize,
    pub expected_tools: Vec<String>,
    pub found_tools: Vec<String>,
    pub correct_tools: Vec<String>,
    pub missed_tools: Vec<String>,
    pub wrong_tools: Vec<String>,
    /// Category ids visited during search.
    #[serde(default)]
    pub visited_category_ids: Option<Vec<String>>,
}

/// Overall evaluation metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct A2xOverallMetrics {
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
    pub avg_visited_categories: f64,
    pub avg_pruned_categories: f64,
}

/// Options for [`A2xEvaluator::evaluate_batch`].
#[derive(Clone, Debug, Default)]
pub struct EvaluateOptions {
    pub query_file: PathBuf,
    pub max_queries: Option<usize>,
    /// Results and checkpoint directory; nothing is written when `None`.
    pub output_dir: Option<PathBuf>,
    pub experiment_id: Option<String>,
    pub notes: String,
}

impl EvaluateOptions {
    pub fn new(query_file: impl Into<PathBuf>) -> Self {
        Self {
            query_file: query_file.into(),
            ..Default::default()
        }
    }
}

/// Evaluator for A2X search.
pub struct A2xEvaluator {
    searcher: Arc<A2xSearch>,
    max_workers: usize,
}

impl A2xEvaluator {
    /// Build the searcher from `config`; `config.max_workers` is also the
    /// number of queries evaluated concurrently.
    pub fn new(config: A2xSearchConfig, llm: Arc<dyn LlmBackend>) -> Result<Self> {
        let max_workers = config.max_workers;
        Ok(Self::with_searcher(
            Arc::new(A2xSearch::new(config, llm)?),
            max_workers,
        ))
    }

    pub fn with_searcher(searcher: Arc<A2xSearch>, max_workers: usize) -> Self {
        Self {
            searcher,
            max_workers,
        }
    }

    pub fn searcher(&self) -> &Arc<A2xSearch> {
        &self.searcher
    }

    /// Evaluate one query.
    pub async fn evaluate_single_query(&self, query_obj: &QueryObject) -> A2xQueryMetrics {
        evaluate_one(&self.searcher, query_obj).await
    }

    fn load_checkpoint(output_dir: &Path) -> (Vec<(usize, A2xQueryMetrics)>, HashSet<String>) {
        let path = output_dir.join("partial_results.jsonl");
        let mut completed = Vec::new();
        let mut ids = HashSet::new();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (completed, ids);
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parsed: std::result::Result<Value, _> = serde_json::from_str(line);
            let Ok(Value::Object(mut obj)) = parsed else {
                tracing::warn!("Failed to load checkpoint: invalid line");
                return (Vec::new(), HashSet::new());
            };
            let idx = obj.remove("_index").and_then(|v| v.as_u64()).map(|v| v as usize);
            let metrics: std::result::Result<A2xQueryMetrics, _> = serde_json::from_value(Value::Object(obj));
            match (idx, metrics) {
                (Some(idx), Ok(m)) => {
                    ids.insert(m.query_id.clone());
                    completed.push((idx, m));
                }
                _ => {
                    tracing::warn!("Failed to load checkpoint: invalid record");
                    return (Vec::new(), HashSet::new());
                }
            }
        }
        if !completed.is_empty() {
            tracing::info!(
                "Resumed from checkpoint: {} queries already completed",
                completed.len()
            );
        }
        (completed, ids)
    }

    fn append_checkpoint(output_dir: &Path, idx: usize, metrics: &A2xQueryMetrics) -> Result<()> {
        let path = output_dir.join("partial_results.jsonl");
        let mut value = serde_json::to_value(metrics).map_err(|e| Error::Other(e.to_string()))?;
        if let Value::Object(obj) = &mut value {
            obj.insert("_index".into(), json!(idx));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        writeln!(file, "{value}").map_err(|e| Error::io(&path, e))
    }

    /// Evaluate a query file with checkpointing and result files.
    pub async fn evaluate_batch(
        &self,
        opts: &EvaluateOptions,
    ) -> Result<(Vec<A2xQueryMetrics>, A2xOverallMetrics)> {
        let queries = load_queries(&opts.query_file, opts.max_queries)?;

        let mut results_with_idx: Vec<(usize, A2xQueryMetrics)> = Vec::new();
        let mut completed_ids = HashSet::new();
        if let Some(dir) = &opts.output_dir {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
            let (c, ids) = Self::load_checkpoint(dir);
            results_with_idx = c;
            completed_ids = ids;
        }
        let pending: Vec<(usize, QueryObject)> = queries
            .iter()
            .enumerate()
            .filter(|(_, q)| !completed_ids.contains(&q.id))
            .map(|(i, q)| (i, q.clone()))
            .collect();
        tracing::info!(
            "Evaluating {} queries ({} from checkpoint, {} remaining)...",
            queries.len(),
            results_with_idx.len(),
            pending.len()
        );
        tracing::info!("Max workers: {}", self.max_workers);

        let start = Instant::now();
        let total = queries.len();
        let jobs: Vec<_> = pending
            .into_iter()
            .map(|(idx, q)| {
                let searcher = Arc::clone(&self.searcher);
                async move { (idx, evaluate_one(&searcher, &q).await) }
            })
            .collect();
        let mut checkpoint_error: Option<Error> = None;
        run_bounded(self.max_workers, jobs, |(idx, metrics)| {
            if let Some(dir) = &opts.output_dir {
                if let Err(e) = Self::append_checkpoint(dir, idx, &metrics) {
                    checkpoint_error.get_or_insert(e);
                }
            }
            results_with_idx.push((idx, metrics));
            let hits = results_with_idx.iter().filter(|(_, m)| m.hit).count();
            let avg_prec = results_with_idx.iter().map(|(_, m)| m.precision).sum::<f64>()
                / results_with_idx.len() as f64;
            progress_line(
                "Evaluating queries",
                results_with_idx.len(),
                total,
                &format!("hits={hits} avg_prec={avg_prec:.3}"),
            );
        })
        .await;
        if let Some(e) = checkpoint_error {
            return Err(e);
        }

        results_with_idx.sort_by_key(|(i, _)| *i);
        let query_metrics: Vec<A2xQueryMetrics> = results_with_idx.into_iter().map(|(_, m)| m).collect();
        let elapsed = start.elapsed().as_secs_f64();
        let overall = compute_overall(&query_metrics);
        print_results(&overall, elapsed);

        if let Some(dir) = &opts.output_dir {
            self.save_results(dir, opts, &query_metrics, &overall, elapsed)?;
        }
        Ok((query_metrics, overall))
    }

    fn save_results(
        &self,
        output_dir: &Path,
        opts: &EvaluateOptions,
        query_metrics: &[A2xQueryMetrics],
        overall: &A2xOverallMetrics,
        elapsed: f64,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir).map_err(|e| Error::io(output_dir, e))?;
        let cfg = self.searcher.config();
        let experiment_id = opts.experiment_id.clone().unwrap_or_else(|| {
            output_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        let config = json!({
            "experiment_id": experiment_id,
            "date": today(),
            "query_file": opts.query_file.to_string_lossy(),
            "taxonomy_path": cfg.taxonomy_path.to_string_lossy(),
            "class_path": cfg.class_path.to_string_lossy(),
            "service_path": cfg.service_path.to_string_lossy(),
            "search_config": {
                "parallel": cfg.parallel,
                "max_workers": self.max_workers,
                "mode": cfg.mode.as_str(),
            },
            "query_count": overall.total_queries,
            "notes": opts.notes,
        });
        write_json(&output_dir.join("config.json"), &config)?;

        let detailed = json!({
            "overall_metrics": overall,
            "query_metrics": query_metrics,
            "elapsed_time": elapsed,
        });
        write_json(&output_dir.join("evaluation_results.json"), &detailed)?;

        let dataset_name = cfg
            .service_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let summary = json!({
            "dataset": dataset_name,
            "query_file": opts.query_file.to_string_lossy(),
            "mode": cfg.mode.as_str(),
            "total_queries": overall.total_queries,
            "precision": overall.avg_precision,
            "recall": overall.avg_recall,
            "hit_rate": overall.hit_rate,
            "f1": overall.avg_f1,
            "avg_llm_calls": overall.avg_llm_calls,
            "avg_tokens": overall.avg_tokens,
            "elapsed_time": elapsed,
        });
        write_json(&output_dir.join("summary.json"), &summary)?;
        tracing::info!("Results saved to: {}", output_dir.display());

        match save_error_report(output_dir) {
            Ok(p) => tracing::info!("Error analysis saved to: {}", p.display()),
            Err(e) => tracing::warn!("Failed to generate error report: {e}"),
        }
        Ok(())
    }
}

async fn evaluate_one(searcher: &A2xSearch, query_obj: &QueryObject) -> A2xQueryMetrics {
    let expected_tools = query_obj.expected_tools();
    let (results, stats) = searcher.search(&query_obj.query).await;
    let found_tools: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let m = compute_set_metrics(&expected_tools, &found_tools);
    A2xQueryMetrics {
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
        visited_categories: stats.visited_categories.len(),
        pruned_categories: stats.pruned_categories.len(),
        expected_tools,
        found_tools,
        correct_tools: m.correct,
        missed_tools: m.missed,
        wrong_tools: m.wrong,
        visited_category_ids: Some(stats.visited_category_ids),
    }
}

/// Aggregate per-query metrics.
pub fn compute_overall(query_metrics: &[A2xQueryMetrics]) -> A2xOverallMetrics {
    let n = query_metrics.len();
    if n == 0 {
        return A2xOverallMetrics::default();
    }
    let nf = n as f64;
    let total_llm_calls: u64 = query_metrics.iter().map(|m| m.llm_calls).sum();
    let total_tokens: u64 = query_metrics.iter().map(|m| m.total_tokens).sum();
    let avg_precision = query_metrics.iter().map(|m| m.precision).sum::<f64>() / nf;
    let avg_recall = query_metrics.iter().map(|m| m.recall).sum::<f64>() / nf;
    let hit_rate = query_metrics.iter().filter(|m| m.hit).count() as f64 / nf;
    A2xOverallMetrics {
        total_queries: n,
        avg_precision,
        avg_recall,
        hit_rate,
        avg_f1: f1(avg_precision, avg_recall),
        total_llm_calls,
        total_tokens,
        avg_llm_calls: total_llm_calls as f64 / nf,
        avg_tokens: total_tokens as f64 / nf,
        avg_found_services: query_metrics.iter().map(|m| m.found_count as f64).sum::<f64>() / nf,
        avg_visited_categories: query_metrics
            .iter()
            .map(|m| m.visited_categories as f64)
            .sum::<f64>()
            / nf,
        avg_pruned_categories: query_metrics
            .iter()
            .map(|m| m.pruned_categories as f64)
            .sum::<f64>()
            / nf,
    }
}

fn print_results(overall: &A2xOverallMetrics, elapsed: f64) {
    let sep = "=".repeat(80);
    tracing::info!("");
    tracing::info!("{sep}");
    tracing::info!("Evaluation Results");
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
    tracing::info!("  Total LLM Calls: {}", overall.total_llm_calls);
    tracing::info!("  Total Tokens:    {}", overall.total_tokens);
    tracing::info!("  Avg LLM Calls:   {:.2}", overall.avg_llm_calls);
    tracing::info!("  Avg Tokens:      {:.2}", overall.avg_tokens);
    tracing::info!("");
    tracing::info!("Search Stats:");
    tracing::info!("  Avg Found Services:       {:.2}", overall.avg_found_services);
    tracing::info!(
        "  Avg Visited Categories:   {:.2}",
        overall.avg_visited_categories
    );
    tracing::info!("  Avg Pruned Categories:    {:.2}", overall.avg_pruned_categories);
    tracing::info!("{sep}");
}
