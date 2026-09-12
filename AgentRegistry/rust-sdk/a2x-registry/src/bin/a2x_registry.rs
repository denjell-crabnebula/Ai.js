// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `a2x-registry`: start the backend API server or run admin subcommands.
//!
//! ```text
//! a2x-registry [--host 127.0.0.1] [--port 8000]
//! a2x-registry auth init [--handle root] [--admin-token T] [--data-dir D]
//! a2x-registry auth reset-admin --confirm
//! a2x-registry cluster ...   (requires the `cluster` feature)
//! ```

use clap::{Parser, Subcommand};

use a2x_registry::auth::cli::AuthCommand;
use a2x_registry::backend::startup::serve;
use a2x_registry::{AppConfig, AppState};

#[derive(Parser)]
#[command(name = "a2x-registry", about = "A2X Registry - backend API server", version)]
struct Cli {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    /// Port (default: 8000)
    #[arg(long, default_value_t = 8000)]
    port: u16,
    /// Host (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Accepted for compatibility; auto reload is not supported.
    #[arg(long, default_value_t = false)]
    reload: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Authentication module administration (no server needed).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Cluster (distributed sync) administration.
    #[cfg(feature = "cluster")]
    #[command(subcommand)]
    Cluster(a2x_cluster::cli::ClusterCommand),
    /// Cluster (distributed sync) administration (requires the `cluster` feature).
    #[cfg(not(feature = "cluster"))]
    Cluster {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Build the async runtime, or exit with a message when the OS refuses.
fn runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to start the async runtime: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli.logging.init() {
        eprintln!("{e}");
    }
    cli.env.apply_or_exit(a2x_registry::env_policy());
    match cli.command {
        Some(Command::Auth(cmd)) => std::process::exit(a2x_registry::auth::cli::run(&cmd)),
        #[cfg(feature = "cluster")]
        Some(Command::Cluster(cmd)) => {
            let rt = runtime();
            std::process::exit(rt.block_on(a2x_registry::backend::adapters::cluster::cli(cmd)));
        }
        #[cfg(not(feature = "cluster"))]
        Some(Command::Cluster { args: _ }) => {
            eprintln!("cluster subcommands require the 'cluster' feature of a2x-registry");
            std::process::exit(2);
        }
        None => {
            if cli.reload {
                eprintln!("  --reload is not supported by the Rust server; ignoring");
            }
            println!(
                "\n  A2X Registry\n  http://{}:{}\n  Docs: docs/backend_api.md\n",
                cli.host, cli.port
            );
            let rt = runtime();
            let state = AppState::new(configure());
            if let Err(e) = rt.block_on(serve(state, &cli.host, cli.port)) {
                eprintln!("server error: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn configure() -> AppConfig {
    let cfg = AppConfig::from_env();
    #[cfg(feature = "search")]
    let cfg = a2x_registry::backend::adapters::search::configure(cfg);
    cfg
}
