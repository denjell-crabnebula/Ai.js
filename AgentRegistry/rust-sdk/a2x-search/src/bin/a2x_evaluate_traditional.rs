// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! CLI: evaluate the traditional (MCP style) full-context search.
//!
//! Mirrors `python -m a2x_registry.traditional.evaluation`.

use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::llm_client::LlmClientOptions;
use a2x_common::{LlmClient, feature_flags, generate_output_dir};
use a2x_search::TraditionalEvaluator;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "a2x-evaluate-traditional",
    about = "Evaluate Traditional (MCP-style) search"
)]
struct Args {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
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
    /// Number of parallel workers
    #[arg(long, default_value_t = 10)]
    workers: usize,
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
    let output = match &args.output {
        Some(o) => o.clone(),
        None => match generate_output_dir(
            "traditional",
            &args.service_path,
            &args.query_file,
            args.max_queries,
            None,
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
    let evaluator = match TraditionalEvaluator::new(&args.service_path, args.workers, llm) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = evaluator
        .evaluate_batch(&args.query_file, args.max_queries, Some(&output))
        .await
    {
        eprintln!("Evaluation failed: {e}");
        std::process::exit(1);
    }
}
