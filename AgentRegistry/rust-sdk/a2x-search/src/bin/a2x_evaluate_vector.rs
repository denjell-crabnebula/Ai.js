// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! CLI: evaluate vector search.
//!
//! Mirrors `python -m a2x_registry.vector.evaluation`. Two extra flags
//! select the embedding backend and the store directory, since the Rust
//! port has no sentence-transformers or ChromaDB.

use std::path::PathBuf;

use a2x_common::{feature_flags, generate_output_dir};
use a2x_search::vector::{DEFAULT_EMBEDDING_MODEL, EmbeddingBackend, resolve_embedding_model};
use a2x_search::{VectorEvaluator, VectorSearchConfig};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "a2x-evaluate-vector", about = "Evaluate vector search")]
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
    /// Collection name
    #[arg(long, default_value = "toolret_new")]
    collection_name: String,
    /// Number of results to retrieve
    #[arg(long, default_value_t = 10)]
    top_k: usize,
    /// Max queries to evaluate (default: all)
    #[arg(long)]
    max_queries: Option<usize>,
    /// Output directory (auto-generated if omitted)
    #[arg(long)]
    output: Option<PathBuf>,
    /// Comma-separated K values for multi-K metrics (e.g. '5,10')
    #[arg(long)]
    top_k_list: Option<String>,
    /// Embedding model name (auto-read from vector_config.json if omitted)
    #[arg(long)]
    model_name: Option<String>,
    /// Force rebuild vector index
    #[arg(long)]
    force_rebuild: bool,
    /// Embedding backend: auto, hashing or openai
    #[arg(long, default_value = "auto", env = "A2X_REGISTRY_EMBEDDING_BACKEND")]
    embedding_backend: String,
    /// Directory holding the persisted vector collections
    #[arg(long, default_value = "database/chroma")]
    persist_dir: PathBuf,
}

#[tokio::main]
async fn main() {
    for f in [feature_flags::Feature::Vector, feature_flags::Feature::Evaluation] {
        if let Err(e) = feature_flags::require(f) {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
    let args = Args::parse();
    if let Err(e) = args.logging.init() {
        eprintln!("{e}");
    }
    args.env
        .apply_or_exit(ap_support::env::EnvPolicy::base().merge(a2x_search::env_policy()));

    let model_name = match &args.model_name {
        Some(m) => m.clone(),
        None => {
            let vc_path = args
                .service_path
                .parent()
                .map(|p| p.join("vector_config.json"))
                .unwrap_or_else(|| PathBuf::from("vector_config.json"));
            std::fs::read_to_string(&vc_path)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| {
                    v.get("embedding_model")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string())
        }
    };
    tracing::info!("Embedding model: {model_name}");

    let output = match &args.output {
        Some(o) => o.clone(),
        None => match generate_output_dir(
            "vector",
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
    let top_k_list = args.top_k_list.as_ref().map(|s| {
        s.split(',')
            .filter_map(|x| x.trim().parse::<usize>().ok())
            .collect::<Vec<_>>()
    });
    let backend: EmbeddingBackend = match args.embedding_backend.parse() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let model = match resolve_embedding_model(&model_name, backend) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let config = VectorSearchConfig::new(
        &args.service_path,
        &args.collection_name,
        &args.persist_dir,
        &model_name,
    )
    .with_force_rebuild(args.force_rebuild);
    let evaluator = match VectorEvaluator::new(config, args.top_k, top_k_list, model).await {
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
