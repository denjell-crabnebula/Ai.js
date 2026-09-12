// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! CLI: evaluate A2X taxonomy search.
//!
//! Mirrors `python -m a2x_registry.a2x.evaluation`.

use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::llm_client::LlmClientOptions;
use a2x_common::{LlmClient, feature_flags, generate_output_dir};
use a2x_search::evaluation::EvaluateOptions;
use a2x_search::{A2xEvaluator, A2xSearchConfig, SearchMode};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "a2x-evaluate-a2x", about = "Evaluate A2X taxonomy search")]
struct Args {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    /// Directory containing taxonomy/taxonomy.json and taxonomy/class.json
    #[arg(long)]
    data_dir: PathBuf,
    /// Path to query file
    #[arg(long, default_value = "database/ToolRet_clean/query/query.json")]
    query_file: PathBuf,
    /// Path to service.json
    #[arg(long, default_value = "database/ToolRet_clean/service.json")]
    service_path: PathBuf,
    /// Max queries to evaluate (default: all)
    #[arg(long)]
    max_queries: Option<usize>,
    /// Output directory (auto-generated if omitted)
    #[arg(long)]
    output: Option<PathBuf>,
    /// Parallel workers
    #[arg(long, default_value_t = 10)]
    workers: usize,
    /// Search mode: get_all (default), get_one (precision-focused), or get_important (confident-only)
    #[arg(long, default_value = "get_all", value_parser = ["get_all", "get_one", "get_important"])]
    mode: String,
    #[arg(long, default_value = "")]
    notes: String,
}

#[tokio::main]
async fn main() {
    if let Err(e) = feature_flags::require(feature_flags::Feature::Evaluation) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let args = Args::parse();
    if let Err(e) = args.logging.init() {
        eprintln!("{e}");
    }
    args.env
        .apply_or_exit(ap_support::env::EnvPolicy::base().merge(a2x_search::env_policy()));
    let mode: SearchMode = match args.mode.parse() {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let output = match &args.output {
        Some(o) => o.clone(),
        None => match generate_output_dir(
            "a2x",
            &args.service_path,
            &args.query_file,
            args.max_queries,
            Some(mode.as_str()),
        ) {
            Ok(o) => PathBuf::from(o),
            Err(e) => {
                eprintln!("cannot derive output directory: {e}");
                std::process::exit(1);
            }
        },
    };

    let llm = match LlmClient::new(None, LlmClientOptions::default()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let config = A2xSearchConfig {
        taxonomy_path: args.data_dir.join("taxonomy").join("taxonomy.json"),
        class_path: args.data_dir.join("taxonomy").join("class.json"),
        service_path: args.service_path.clone(),
        max_workers: args.workers,
        parallel: true,
        mode,
    };
    let evaluator = match A2xEvaluator::new(config, llm) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let experiment_id = output.file_name().map(|n| n.to_string_lossy().to_string());
    let opts = EvaluateOptions {
        query_file: args.query_file.clone(),
        max_queries: args.max_queries,
        output_dir: Some(output),
        experiment_id,
        notes: if args.notes.is_empty() {
            format!("workers={}", args.workers)
        } else {
            args.notes.clone()
        },
    };
    if let Err(e) = evaluator.evaluate_batch(&opts).await {
        eprintln!("Evaluation failed: {e}");
        std::process::exit(1);
    }
}
