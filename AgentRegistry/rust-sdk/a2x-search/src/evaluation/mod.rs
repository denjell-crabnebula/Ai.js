// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Evaluators for the three search methods, plus error analysis.
//!
//! Each evaluator reads `query.json`, runs its searcher, computes
//! precision / recall / hit through `a2x_common::compute_set_metrics` and
//! writes `config.json`, `evaluation_results.json` and `summary.json` to
//! an output directory. File shapes match the Python evaluators.

pub mod a2x_evaluator;
pub mod error_analysis;
pub mod traditional_evaluator;
pub mod vector_evaluator;

use std::io::Write;

pub use a2x_evaluator::{A2xEvaluator, A2xOverallMetrics, A2xQueryMetrics, EvaluateOptions};
pub use error_analysis::{generate_error_report, save_error_report};
pub use traditional_evaluator::{TraditionalEvaluator, TraditionalOverallMetrics, TraditionalQueryMetrics};
pub use vector_evaluator::{MetricsAtK, VectorEvaluator, VectorOverallMetrics, VectorQueryMetrics};

/// Harmonic mean of averaged precision and recall, like the Python code.
pub fn f1(avg_precision: f64, avg_recall: f64) -> f64 {
    if avg_precision + avg_recall > 0.0 {
        2.0 * (avg_precision * avg_recall) / (avg_precision + avg_recall)
    } else {
        0.0
    }
}

/// Minimal in-place progress line on stderr (stand-in for `tqdm`).
pub(crate) fn progress_line(desc: &str, done: usize, total: usize, postfix: &str) {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r{desc}: {done}/{total} {postfix}          ");
    if done >= total {
        let _ = writeln!(err);
    }
    let _ = err.flush();
}

/// Today's date as `YYYY-MM-DD`.
pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
