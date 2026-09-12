// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Standalone cluster node and CLI for testing the cluster module without
//! the full registry. `serve` runs an in-memory node (no local records)
//! that peers can join; the other subcommands mirror
//! `a2x-registry cluster ...`.

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use a2x_cluster::cli::ClusterCommand;
use a2x_cluster::{ClusterConfig, ClusterStore, router};

#[derive(Parser)]
#[command(name = "a2x-cluster", about = "A2X registry cluster module (standalone)")]
struct Cli {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve an in-memory cluster node (needs a prior `init`)
    Serve {
        /// Bind address
        #[arg(long, default_value = "127.0.0.1:8000")]
        bind: String,
        /// Base URL peers use to reach this node (default: http://<bind>)
        #[arg(long)]
        advertise: Option<String>,
    },
    #[command(flatten)]
    Cluster(ClusterCommand),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli.logging.init() {
        eprintln!("{e}");
    }
    cli.env.apply_or_exit(
        ap_support::env::EnvPolicy::base()
            .merge(a2x_common::env_policy())
            .merge(a2x_cluster::env_policy()),
    );
    let code = match cli.cmd {
        Command::Cluster(cmd) => a2x_cluster::cli::run(cmd).await,
        Command::Serve { bind, advertise } => serve(&bind, advertise).await,
    };
    std::process::exit(code);
}

async fn serve(bind: &str, advertise: Option<String>) -> i32 {
    let advertise = advertise
        .or_else(|| ap_support::env::current().get(a2x_cluster::config::ENV_ADVERTISE))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("http://{bind}"));
    let Some(store) = ClusterStore::builder()
        .config(ClusterConfig::from_env())
        .advertise(advertise)
        .load_or_none()
    else {
        eprintln!("cluster_state.json not found; run `a2x-cluster init` first");
        return 1;
    };
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("cannot bind {bind}: {err}");
            return 1;
        }
    };
    let cancel = CancellationToken::new();
    let handle = store.start(cancel.clone());
    println!("a2x-cluster node {} listening on {bind}", store.node_id());
    let app = router(Arc::clone(&store));
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    let code = match serve.await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("server error: {err}");
            1
        }
    };
    handle.shutdown().await;
    store.close();
    code
}
