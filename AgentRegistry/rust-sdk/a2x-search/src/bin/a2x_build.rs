// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! CLI: fully automatic hierarchical taxonomy builder.
//!
//! Mirrors `python -m a2x_registry.a2x.build`.

use std::path::PathBuf;
use std::sync::Arc;

use a2x_common::LlmClient;
use a2x_common::llm_client::LlmClientOptions;
use a2x_search::{AutoHierarchicalConfig, BuildSink, ResumeMode, TaxonomyBuilder};
use clap::Parser;
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(
    name = "a2x-build",
    about = "Fully-automatic hierarchical taxonomy builder: unified BFS recursive category design -> classify/refine -> subdivision"
)]
struct Args {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    /// Path to service.json
    #[arg(long, default_value = "database/ToolRet_clean/service.json")]
    service_path: PathBuf,
    /// Output directory for taxonomy (default: database/{dataset_name}/taxonomy)
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Services per batch for keyword extraction
    #[arg(long, default_value_t = 50)]
    keyword_batch_size: usize,
    /// Service count threshold: >threshold uses keyword extraction, <=threshold uses direct descriptions
    #[arg(long, default_value_t = 500)]
    keyword_threshold: usize,
    /// Max services per node
    #[arg(long, default_value_t = 40)]
    max_service_size: usize,
    /// Max subcategories per node
    #[arg(long, default_value_t = 20)]
    max_categories_size: usize,
    /// Services matching > this ratio of subcategories are 'generic'
    #[arg(long, default_value_t = 0.333)]
    generic_ratio: f64,
    /// After iterations, subcategories with <= this many services are deleted
    #[arg(long, default_value_t = 2)]
    delete_threshold: usize,
    /// Max taxonomy depth (use 1 to skip subdivision, 0 for unlimited)
    #[arg(long, default_value_t = 3)]
    max_depth: u32,
    /// Parallel workers for classification
    #[arg(long, default_value_t = 20)]
    workers: usize,
    /// Max refine iterations per node
    #[arg(long, default_value_t = 3)]
    max_refine_iterations: usize,
    /// Disable cross-domain multi-parent assignment phase
    #[arg(long)]
    no_cross_domain: bool,
    /// Build mode: no=full rebuild, keyword=reuse cached keywords, yes=smart resume
    #[arg(long, default_value = "no", value_parser = ["no", "keyword", "yes"])]
    resume: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(e) = args.logging.init() {
        eprintln!("{e}");
    }
    args.env
        .apply_or_exit(ap_support::env::EnvPolicy::base().merge(a2x_search::env_policy()));

    let mut config = AutoHierarchicalConfig::new(&args.service_path);
    if let Some(out) = &args.output_dir {
        config.output_dir = out.clone();
    }
    config.keyword_batch_size = args.keyword_batch_size;
    config.keyword_threshold = args.keyword_threshold;
    config.generic_ratio = args.generic_ratio;
    config.delete_threshold = args.delete_threshold;
    config.max_service_size = args.max_service_size;
    config.max_categories_size = args.max_categories_size;
    config.max_depth = if args.max_depth == 0 {
        None
    } else {
        Some(args.max_depth)
    };
    config.workers = args.workers;
    config.max_refine_iterations = args.max_refine_iterations;
    config.enable_cross_domain = !args.no_cross_domain;

    println!("Auto-Hierarchical Taxonomy Builder");
    println!("{}", "=".repeat(60));
    println!("  Dataset: {}", config.dataset_name());
    println!("  Services: {}", config.service_path.display());
    println!("  Output: {}", config.output_dir.display());
    println!("  Resume: {}", args.resume);
    println!("  Keyword threshold: {}", config.keyword_threshold);
    println!("  Keyword batch size: {}", config.keyword_batch_size);
    println!("  Generic ratio: {:.3}", config.generic_ratio);
    println!("  Delete threshold: {}", config.delete_threshold);
    println!("  Max service size: {}", config.max_service_size);
    println!("  Max categories size: {}", config.max_categories_size);
    println!(
        "  Max depth: {}",
        config
            .max_depth
            .map(|d| d.to_string())
            .unwrap_or_else(|| "unlimited".into())
    );
    println!("  Workers: {}", config.workers);
    println!("  Max refine iterations: {}", config.max_refine_iterations);
    println!("  Cross-domain: {}", config.enable_cross_domain);

    let resume: ResumeMode = match args.resume.parse() {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let llm = match LlmClient::new(None, LlmClientOptions::default()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let cancel = CancellationToken::new();
    let ctrl_c_token = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nCancellation requested, stopping at the next checkpoint...");
            ctrl_c_token.cancel();
        }
    });

    let mut builder = TaxonomyBuilder::new(config, llm);
    match builder.build(resume, BuildSink::stdout(), cancel).await {
        Ok(outcome) if outcome.skipped => println!("Taxonomy already complete, nothing to do."),
        Ok(_) => {}
        Err(e) => {
            eprintln!("Build failed: {e}");
            std::process::exit(1);
        }
    }
}
