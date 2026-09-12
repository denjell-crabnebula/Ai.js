// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! CLI: A2X hierarchical service search.
//!
//! Mirrors `python -m a2x_registry.a2x.search`.

use std::sync::Arc;
use std::time::Instant;

use a2x_common::LlmClient;
use a2x_common::llm_client::LlmClientOptions;
use a2x_search::{A2xSearch, A2xSearchConfig, SearchMode, llm_workers_from_env};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "a2x-search", about = "A2X Hierarchical Service Search")]
struct Args {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    /// Search query
    #[arg(short, long)]
    query: Option<String>,
    /// Navigate branches in parallel (default true; use --parallel=false to disable)
    #[arg(short, long, default_value_t = true, action = clap::ArgAction::Set)]
    parallel: bool,
    /// Maximum concurrent LLM calls (default 20, or A2X_REGISTRY_LLM_WORKERS)
    #[arg(short = 'w', long)]
    max_workers: Option<usize>,
    /// Search mode: get_all (default), get_one, or get_important
    #[arg(long, default_value = "get_all", value_parser = ["get_all", "get_one", "get_important"])]
    mode: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(e) = args.logging.init() {
        eprintln!("{e}");
    }
    args.env
        .apply_or_exit(ap_support::env::EnvPolicy::base().merge(a2x_search::env_policy()));
    let query = args
        .query
        .clone()
        .unwrap_or_else(|| "I need to book a flight to Tokyo".to_string());
    let max_workers = args.max_workers.unwrap_or_else(|| llm_workers_from_env(20));
    let mode: SearchMode = match args.mode.parse() {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    println!("\nQuery: {query}");
    println!(
        "Mode: {} (max_workers={max_workers}), search_mode={mode}",
        if args.parallel { "Parallel" } else { "Sequential" }
    );
    println!("{}", "=".repeat(80));

    let llm = match LlmClient::new(None, LlmClientOptions::default()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let config = A2xSearchConfig::default()
        .with_max_workers(max_workers)
        .with_parallel(args.parallel)
        .with_mode(mode);
    let start = Instant::now();
    let searcher = match A2xSearch::new(config, llm) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let (results, stats) = searcher.search(&query).await;
    let elapsed = start.elapsed().as_secs_f64();

    println!("\nFound {} services:", results.len());
    println!("{}", "-".repeat(80));
    for (i, r) in results.iter().enumerate() {
        println!("\n{}. {} (ID: {})", i + 1, r.name, r.id);
        let desc = if r.description.chars().count() > 200 {
            format!("{}...", r.description.chars().take(200).collect::<String>())
        } else {
            r.description.clone()
        };
        println!("   {desc}");
    }
    println!("\n{}", "=".repeat(80));
    println!("Search Statistics:");
    println!("  Time Elapsed: {elapsed:.2}s");
    println!("  LLM Calls: {}", stats.llm_calls);
    println!("  Total Tokens: {}", stats.total_tokens);
    println!("  Visited Categories: {}", stats.visited_categories.len());
    println!("  Pruned Categories: {}", stats.pruned_categories.len());
    if args.logging.verbose > 0 {
        println!("\nVisited Categories:");
        for c in &stats.visited_categories {
            println!("  + {c}");
        }
        println!("\nPruned Categories:");
        for c in &stats.pruned_categories {
            println!("  - {c}");
        }
    }
}
