// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client-side credential resolution and `cli_token.json` file IO.
//!
//! Credentials come from two places only, matching `docs/auth_design.md`:
//!
//! 1. Explicit constructor arguments (`api_key`, `base_url`).
//! 2. `~/.a2x_registry_client/cli_token.json`, written by the `login` CLI command.
//!
//! There is deliberately no environment-variable code path; the Python SDK
//! does not read one either.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::ClientError;

/// Registry URL used when neither the caller nor the config file supplies one.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000";

/// Prefix every personal access token starts with.
pub const TOKEN_PREFIX: &str = "a2x_pat_";

/// Default config file: `~/.a2x_registry_client/cli_token.json`.
///
/// `None` when the home directory cannot be determined.
pub fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".a2x_registry_client").join("cli_token.json"))
}

/// Parsed contents of `cli_token.json`. Unknown keys are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliToken {
    /// Registry URL saved at login time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Personal access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

fn resolve_path(path: Option<&Path>) -> Option<PathBuf> {
    match path {
        Some(p) => Some(p.to_path_buf()),
        None => default_config_path(),
    }
}

/// Read `cli_token.json`.
///
/// Returns `None` when the file is missing, malformed, or not a JSON object.
/// Problems are logged as warnings instead of failing: degraded credential
/// resolution is preferred over a hard crash at construction time.
/// On POSIX a group- or world-readable file also logs a warning.
pub fn read_cli_token(path: Option<&Path>) -> Option<CliToken> {
    let path = resolve_path(path)?;
    if !path.exists() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&path) {
            if meta.permissions().mode() & 0o044 != 0 {
                tracing::warn!(
                    "{} is readable by group/other; consider 'chmod 600 {}'",
                    path.display(),
                    path.display()
                );
            }
        }
    }
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("Failed to read {}: {e}", path.display());
            return None;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to read {}: {e}", path.display());
            return None;
        }
    };
    if !value.is_object() {
        tracing::warn!("{} is not a JSON object; ignoring", path.display());
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Persist `(base_url, api_key)` to `cli_token.json` with mode 0600.
///
/// The write is atomic: a `.tmp` sibling is written, fsynced and renamed
/// over the target. Returns the final path.
pub fn write_cli_token(api_key: &str, base_url: &str, path: Option<&Path>) -> Result<PathBuf, ClientError> {
    if api_key.trim().is_empty() {
        return Err(ClientError::invalid("api_key must be a non-empty string"));
    }
    let path = resolve_path(path)
        .ok_or_else(|| ClientError::invalid("cannot determine the home directory for cli_token.json"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_sibling(&path);
    let payload = CliToken {
        base_url: Some(base_url.to_string()),
        api_key: Some(api_key.to_string()),
    };
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| ClientError::decode(format!("cannot serialise cli token: {e}")))?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            tracing::warn!("Could not chmod 0600 on {}: {e}", path.display());
        }
    }
    Ok(path)
}

/// Idempotent delete. Returns true when a file was removed, false when absent.
pub fn remove_cli_token(path: Option<&Path>) -> bool {
    let Some(path) = resolve_path(path) else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    match fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("Could not remove {}: {e}", path.display());
            false
        }
    }
}

/// Resolve `(api_key, base_url)` with the SDK precedence rules.
///
/// 1. Explicit arguments (highest).
/// 2. Values from `cli_token.json`, if present.
/// 3. Defaults: `api_key = None`, `base_url = DEFAULT_BASE_URL`.
///
/// The file is not read at all when both arguments are explicit.
pub fn resolve_credentials(
    api_key: Option<&str>,
    base_url: Option<&str>,
    config_path: Option<&Path>,
) -> (Option<String>, String) {
    if let (Some(k), Some(u)) = (api_key, base_url) {
        return (Some(k.to_string()), u.to_string());
    }
    let cfg = read_cli_token(config_path).unwrap_or_default();
    let non_blank = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    let resolved_key = match api_key {
        Some(k) => Some(k.to_string()),
        None => non_blank(cfg.api_key),
    };
    let resolved_url = match base_url {
        Some(u) => u.to_string(),
        None => non_blank(cfg.base_url).unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
    };
    (resolved_key, resolved_url)
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}
