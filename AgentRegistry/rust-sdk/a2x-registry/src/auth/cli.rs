// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `a2x-registry auth init` and `a2x-registry auth reset-admin`.
//!
//! Both write to the auth data directory directly (no server). The admin
//! token is printed to stderr once inside a banner.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use super::store::{AuthStore, AuthStoreError, KEYS_FILE, PRINCIPALS_FILE, default_data_dir};

/// Auth subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Bootstrap the auth module (creates the first admin and key).
    Init(InitArgs),
    /// Wipe principals and keys and create a new bootstrap admin.
    ResetAdmin(ResetArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Handle for the bootstrap admin principal.
    #[arg(long, default_value = "root")]
    pub handle: String,
    /// Use this exact plaintext token (must start with `a2x_pat_`).
    #[arg(long)]
    pub admin_token: Option<String>,
    /// Override the auth data directory.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Required acknowledgement; without it nothing is touched.
    #[arg(long)]
    pub confirm: bool,
    #[arg(long, default_value = "root")]
    pub handle: String,
    #[arg(long)]
    pub admin_token: Option<String>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

fn banner(token: &str, data_dir: &Path) -> String {
    format!(
        "\n============================================================\n\
First-run bootstrap admin key (save now, will not be shown again):\n\n    {token}\n\n  \
scope:   admin  (namespaces=None -> all)\n  stored:  {dir}/{p}\n           {dir}/{k}\n\n\
Next:\n  - set this token in your client via 'a2x-registry-client login'\n  \
- or pass it explicitly: A2XRegistryClient(api_key='a2x_pat_...')\n\
============================================================\n",
        dir = data_dir.display(),
        p = PRINCIPALS_FILE,
        k = KEYS_FILE
    )
}

fn resolve_dir(explicit: Option<&PathBuf>) -> PathBuf {
    match explicit {
        Some(d) => std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()),
        None => default_data_dir(),
    }
}

/// Run `auth init`. Returns the process exit code.
pub fn cmd_init(args: &InitArgs) -> i32 {
    let data_dir = resolve_dir(args.data_dir.as_ref());
    match AuthStore::bootstrap(Some(&data_dir), args.admin_token.as_deref(), &args.handle) {
        Ok((_store, token)) => {
            eprint!("{}", banner(&token, &data_dir));
            0
        }
        Err(AuthStoreError::AlreadyInitialized(e)) => {
            eprintln!("\nauth init failed: {e}\n");
            1
        }
        Err(e) => {
            eprintln!("\nauth init failed: {e}\n");
            2
        }
    }
}

/// Run `auth reset-admin`. Destroys existing principals and keys; the
/// audit log is kept as history.
pub fn cmd_reset_admin(args: &ResetArgs) -> i32 {
    if !args.confirm {
        eprintln!(
            "\nauth reset-admin requires --confirm. This destroys all existing\n\
principals/keys (every issued token becomes invalid).\n"
        );
        return 2;
    }
    let data_dir = resolve_dir(args.data_dir.as_ref());
    for fname in [PRINCIPALS_FILE, KEYS_FILE] {
        let p = data_dir.join(fname);
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }
    match AuthStore::bootstrap(Some(&data_dir), args.admin_token.as_deref(), &args.handle) {
        Ok((_store, token)) => {
            eprint!("{}", banner(&token, &data_dir));
            0
        }
        Err(e) => {
            eprintln!("\nauth reset-admin failed: {e}\n");
            1
        }
    }
}

/// Dispatch an auth subcommand.
pub fn run(cmd: &AuthCommand) -> i32 {
    match cmd {
        AuthCommand::Init(a) => cmd_init(a),
        AuthCommand::ResetAdmin(a) => cmd_reset_admin(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn init_then_reset() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let init = InitArgs {
            handle: "root".into(),
            admin_token: None,
            data_dir: Some(tmp.path().to_path_buf()),
        };
        assert_eq!(cmd_init(&init), 0);
        assert_eq!(cmd_init(&init), 1);
        let bad = InitArgs {
            handle: "root".into(),
            admin_token: Some("bad".into()),
            data_dir: Some(tmp.path().join("other")),
        };
        assert_eq!(cmd_init(&bad), 2);
        let reset = ResetArgs {
            confirm: false,
            handle: "root".into(),
            admin_token: None,
            data_dir: Some(tmp.path().to_path_buf()),
        };
        assert_eq!(cmd_reset_admin(&reset), 2);
        let reset = ResetArgs {
            confirm: true,
            ..reset
        };
        assert_eq!(cmd_reset_admin(&reset), 0);
        assert!(tmp.path().join(PRINCIPALS_FILE).exists());
        Ok(())
    }
}
