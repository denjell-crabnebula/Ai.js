// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `a2x-registry cluster ...` CLI.
//!
//! The user-facing way to drive distributed sync. [`ClusterCommand`] is a
//! clap `Subcommand` the registry binary embeds; [`run`] executes one
//! command and returns the process exit code. Commands that read live state
//! talk to the local server over HTTP, ignoring system proxies.

use clap::{Args, Subcommand};
use serde_json::Value;

use crate::state::{ClusterState, state_path};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8000";

/// Shared `--server` flag.
#[derive(Args, Clone, Debug)]
pub struct ServerArg {
    /// Registry base URL of this instance.
    #[arg(long, default_value = DEFAULT_SERVER)]
    pub server: String,
}

/// `a2x-registry cluster <command>`.
#[derive(Subcommand, Clone, Debug)]
pub enum ClusterCommand {
    /// Generate node id + cluster_state.json (offline, no server needed)
    Init {
        /// Explicit node id (default: auto-generated reg-`<uuid>`)
        #[arg(long)]
        node_id: Option<String>,
    },
    /// Show sync state from a running server
    Status {
        #[command(flatten)]
        server: ServerArg,
    },
    /// Connect to a peer and sync (internal primitive; prefer `set add`)
    AddPeer {
        /// Peer base URL, e.g. http://10.0.0.2:8000
        address: String,
        /// Comma-separated namespaces to sync (default: all)
        #[arg(long)]
        namespaces: Option<String>,
        /// API key for the peer's per-namespace authorization
        #[arg(long)]
        token: Option<String>,
        #[command(flatten)]
        server: ServerArg,
    },
    /// Drop a peer session + its replicated records
    RmPeer {
        /// Peer node id to disconnect
        node_id: String,
        #[command(flatten)]
        server: ServerArg,
    },
    /// Declaratively manage cluster membership
    Set {
        #[command(subcommand)]
        cmd: SetCommand,
    },
}

/// `a2x-registry cluster set <command>`.
#[derive(Subcommand, Clone, Debug)]
pub enum SetCommand {
    /// Add members to this node's cluster (auto full-mesh)
    Add {
        /// Member base URLs, e.g. http://10.0.0.2:8000
        #[arg(required = true)]
        addresses: Vec<String>,
        /// Admin API key, if the members require auth
        #[arg(long)]
        token: Option<String>,
        #[command(flatten)]
        server: ServerArg,
    },
    /// Remove members from the cluster
    Remove {
        /// Member node ids to remove
        #[arg(required = true)]
        node_ids: Vec<String>,
        #[command(flatten)]
        server: ServerArg,
    },
    /// Show this node's cluster + roster
    Show {
        #[command(flatten)]
        server: ServerArg,
    },
}

/// Outcome of a CLI command: the text to print and the exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOutput {
    pub text: String,
    pub code: i32,
}

impl CliOutput {
    fn ok(text: impl Into<String>) -> Self {
        CliOutput {
            text: text.into(),
            code: 0,
        }
    }

    fn fail(text: impl Into<String>) -> Self {
        CliOutput {
            text: text.into(),
            code: 1,
        }
    }
}

/// `cluster init`: create the state file at the default location.
pub fn cmd_init(node_id: Option<&str>) -> CliOutput {
    match ClusterState::init(node_id) {
        Ok(state) => CliOutput::ok(format!(
            "Cluster initialized.\n  node_id : {}\n  state   : {}\nRestart the registry server for the cluster module to load.",
            state.node_id,
            state.path.clone().unwrap_or_else(state_path).display()
        )),
        Err(err) => CliOutput::fail(format!("error: {err}")),
    }
}

async fn client_call(server: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> CliOutput {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(err) => return CliOutput::fail(format!("error: cannot build HTTP client: {err}")),
    };
    let mut req = client.request(method, &url);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(err) => return CliOutput::fail(format!("error: cannot reach server at {server}: {err}")),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.as_u16() == 404 {
        return CliOutput::fail(
            "Cluster module not initialized on the server (run 'a2x-registry cluster init', then restart the server).",
        );
    }
    if !status.is_success() {
        return CliOutput::fail(format!("error: server returned {}: {text}", status.as_u16()));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => CliOutput::ok(serde_json::to_string_pretty(&v).unwrap_or(text)),
        Err(_) => CliOutput::ok(text),
    }
}

pub async fn cmd_status(server: &str) -> CliOutput {
    client_call(server, reqwest::Method::GET, "/api/cluster/state", None).await
}

pub async fn cmd_add_peer(
    server: &str,
    address: &str,
    namespaces: Option<&str>,
    token: Option<&str>,
) -> CliOutput {
    let ns: Option<Vec<String>> = namespaces.map(|s| {
        s.split(',')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    });
    let body = serde_json::json!({"address": address, "namespaces": ns, "token": token});
    client_call(server, reqwest::Method::POST, "/api/cluster/peers", Some(body)).await
}

pub async fn cmd_rm_peer(server: &str, node_id: &str) -> CliOutput {
    client_call(
        server,
        reqwest::Method::DELETE,
        &format!("/api/cluster/peers/{node_id}"),
        None,
    )
    .await
}

pub async fn cmd_set_add(server: &str, addresses: &[String], token: Option<&str>) -> CliOutput {
    let members: Vec<Value> = addresses
        .iter()
        .map(|a| serde_json::json!({"address": a}))
        .collect();
    let body = serde_json::json!({"members": members, "token": token});
    client_call(server, reqwest::Method::POST, "/api/cluster/set/add", Some(body)).await
}

pub async fn cmd_set_remove(server: &str, node_ids: &[String]) -> CliOutput {
    let members: Vec<Value> = node_ids
        .iter()
        .map(|n| serde_json::json!({"node_id": n}))
        .collect();
    let body = serde_json::json!({"members": members});
    client_call(
        server,
        reqwest::Method::POST,
        "/api/cluster/set/remove",
        Some(body),
    )
    .await
}

pub async fn cmd_set_show(server: &str) -> CliOutput {
    client_call(server, reqwest::Method::GET, "/api/cluster/set", None).await
}

/// Execute one command without printing.
pub async fn execute(cmd: ClusterCommand) -> CliOutput {
    match cmd {
        ClusterCommand::Init { node_id } => cmd_init(node_id.as_deref()),
        ClusterCommand::Status { server } => cmd_status(&server.server).await,
        ClusterCommand::AddPeer {
            address,
            namespaces,
            token,
            server,
        } => cmd_add_peer(&server.server, &address, namespaces.as_deref(), token.as_deref()).await,
        ClusterCommand::RmPeer { node_id, server } => cmd_rm_peer(&server.server, &node_id).await,
        ClusterCommand::Set { cmd } => match cmd {
            SetCommand::Add {
                addresses,
                token,
                server,
            } => cmd_set_add(&server.server, &addresses, token.as_deref()).await,
            SetCommand::Remove { node_ids, server } => cmd_set_remove(&server.server, &node_ids).await,
            SetCommand::Show { server } => cmd_set_show(&server.server).await,
        },
    }
}

/// Execute one command, print its output to stdout and return the exit code.
pub async fn run(cmd: ClusterCommand) -> i32 {
    let out = execute(cmd).await;
    println!("{}", out.text);
    out.code
}
