// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Command line interface, port of `cli.py`.
//!
//! ```text
//! a2x-registry-client login [--base-url URL] [--token T]   save a token to cli_token.json
//! a2x-registry-client logout                               remove the token file
//! a2x-registry-client whoami                               GET /api/auth/whoami
//! a2x-registry-client keys list                            list own keys
//! a2x-registry-client keys create --name NAME              issue a new key
//! a2x-registry-client keys revoke KEY_ID                   revoke a key
//! ```
//!
//! Every command honours `--base-url` to override `cli_token.json`. Server
//! responses are printed as JSON on stdout; errors go to stderr.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::auth::{
    DEFAULT_BASE_URL, TOKEN_PREFIX, default_config_path, read_cli_token, remove_cli_token, write_cli_token,
};
use crate::blocking;
use crate::client::{ClientConfig, OwnershipFile};
use crate::errors::ClientError;

/// `a2x-registry-client` command line.
#[derive(Debug, Parser)]
#[command(name = "a2x-registry-client", about = "A2X Registry client SDK CLI", version)]
pub struct Cli {
    /// Override the registry URL for this invocation (else uses cli_token.json)
    #[arg(long, global = true)]
    pub base_url: Option<String>,
    /// Logging options.
    #[command(flatten)]
    pub logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options.
    #[command(flatten)]
    pub env: ap_support::env::EnvArgs,
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive paste of a token, saved to cli_token.json
    Login {
        /// Use this token instead of prompting (for scripts; less secure).
        #[arg(long)]
        token: Option<String>,
    },
    /// Remove cli_token.json
    Logout,
    /// GET /api/auth/whoami
    Whoami,
    /// Manage API keys for the current principal
    Keys {
        /// Key operation.
        #[command(subcommand)]
        command: KeysCommand,
    },
}

/// `keys` subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// List own keys
    List,
    /// Issue a new key
    Create {
        /// Human label, e.g. 'laptop'
        #[arg(long, default_value = "cli")]
        name: String,
    },
    /// Revoke a key
    Revoke {
        /// Key id to revoke
        key_id: String,
    },
}

/// Streams and file locations the CLI works with; swappable for tests.
pub struct CliEnv<'a> {
    /// Where `cli_token.json` lives; `None` means the default home location.
    pub config_path: Option<PathBuf>,
    /// Standard output sink.
    pub stdout: &'a mut dyn Write,
    /// Standard error sink.
    pub stderr: &'a mut dyn Write,
    /// Standard input source for interactive prompts.
    pub stdin: &'a mut dyn BufRead,
}

/// Run the CLI against the real process streams and the default config path.
pub fn run(cli: Cli) -> i32 {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let stdin = std::io::stdin();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let mut input = stdin.lock();
    let mut env = CliEnv {
        config_path: None,
        stdout: &mut out,
        stderr: &mut err,
        stdin: &mut input,
    };
    run_with(cli, &mut env)
}

/// Run the CLI with explicit streams and config location. Returns the exit code.
pub fn run_with(cli: Cli, env: &mut CliEnv<'_>) -> i32 {
    let base_url = cli.base_url;
    match cli.command {
        Command::Login { token } => cmd_login(base_url, token, env),
        Command::Logout => cmd_logout(env),
        Command::Whoami => run_client(base_url, env, |c| c.whoami()),
        Command::Keys { command } => match command {
            KeysCommand::List => run_client(base_url, env, |c| c.list_keys()),
            KeysCommand::Create { name } => run_client(base_url, env, move |c| c.create_key(&name)),
            KeysCommand::Revoke { key_id } => run_revoke(base_url, key_id, env),
        },
    }
}

fn config_display(env: &CliEnv<'_>) -> String {
    env.config_path
        .clone()
        .or_else(default_config_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "cli_token.json".to_string())
}

fn client_from_config(
    base_url: Option<String>,
    config_path: Option<&Path>,
) -> Result<blocking::A2xRegistryClient, ClientError> {
    let cfg = read_cli_token(config_path).unwrap_or_default();
    let mut config = ClientConfig::new().ownership_file(OwnershipFile::Disabled);
    config.base_url = base_url.or(cfg.base_url);
    config.api_key = cfg.api_key;
    config.config_path = config_path.map(Path::to_path_buf);
    blocking::A2xRegistryClient::new(config)
}

fn prompt(env: &mut CliEnv<'_>, text: &str) -> String {
    let _ = write!(env.stdout, "{text}");
    let _ = env.stdout.flush();
    let mut line = String::new();
    let _ = env.stdin.read_line(&mut line);
    line.trim().to_string()
}

fn cmd_login(base_url: Option<String>, token: Option<String>, env: &mut CliEnv<'_>) -> i32 {
    let base_url = match base_url {
        Some(u) => u,
        None => {
            let typed = prompt(env, &format!("Registry URL [{DEFAULT_BASE_URL}]: "));
            if typed.is_empty() {
                DEFAULT_BASE_URL.to_string()
            } else {
                typed
            }
        }
    };
    let token = match token {
        Some(t) => t,
        None => prompt(env, "API key: "),
    };
    if !token.starts_with(TOKEN_PREFIX) {
        let _ = writeln!(env.stderr, "error: token must start with '{TOKEN_PREFIX}'");
        return 2;
    }
    let path = match write_cli_token(&token, &base_url, env.config_path.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(env.stderr, "error: failed to write {}: {e}", config_display(env));
            return 1;
        }
    };
    // Poke /whoami so a bad token fails loudly now rather than at the first business call.
    let config = ClientConfig::new()
        .base_url(base_url)
        .api_key(token)
        .ownership_file(OwnershipFile::Disabled);
    let client = match blocking::A2xRegistryClient::new(config) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(env.stderr, "warning: token saved but whoami failed: {e}");
            return 0;
        }
    };
    match client.whoami() {
        Ok(who) => {
            let handle = who.get("handle").and_then(|v| v.as_str()).unwrap_or("");
            let role = who.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let _ = writeln!(env.stdout, "\u{2713} Saved to {}", path.display());
            let _ = writeln!(env.stdout, "  Logged in as {handle:?} (role={role})");
            0
        }
        Err(e @ ClientError::Authentication { .. }) => {
            let _ = writeln!(env.stderr, "warning: token saved but server says 401: {e}");
            1
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "warning: token saved but whoami failed: {e}");
            0
        }
    }
}

fn cmd_logout(env: &mut CliEnv<'_>) -> i32 {
    let shown = config_display(env);
    if remove_cli_token(env.config_path.as_deref()) {
        let _ = writeln!(env.stdout, "\u{2713} Removed {shown}");
    } else {
        let _ = writeln!(env.stdout, "(no token file at {shown})");
    }
    0
}

fn run_client<F>(base_url: Option<String>, env: &mut CliEnv<'_>, call: F) -> i32
where
    F: FnOnce(&blocking::A2xRegistryClient) -> Result<serde_json::Value, ClientError>,
{
    let client = match client_from_config(base_url, env.config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(env.stderr, "error: {e}");
            return 1;
        }
    };
    match call(&client) {
        Ok(body) => {
            let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
            let _ = writeln!(env.stdout, "{text}");
            0
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "error: {e}");
            1
        }
    }
}

fn run_revoke(base_url: Option<String>, key_id: String, env: &mut CliEnv<'_>) -> i32 {
    let client = match client_from_config(base_url, env.config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(env.stderr, "error: {e}");
            return 1;
        }
    };
    match client.revoke_key(&key_id) {
        Ok(body) => {
            let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
            let _ = writeln!(env.stdout, "{text}");
            0
        }
        Err(e @ ClientError::Authorization { .. }) => {
            let _ = writeln!(env.stderr, "error: 403 {e}");
            3
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "error: {e}");
            1
        }
    }
}
