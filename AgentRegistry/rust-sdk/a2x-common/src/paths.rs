// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Runtime path resolution.
//!
//! Two external resources must be resolvable at runtime:
//!
//! - `llm_apikey.json`: credentials for external LLM providers. Default is
//!   `~/.a2x_registry/llm_apikey.json`; `A2X_REGISTRY_HOME` overrides the
//!   directory. The current directory is intentionally not probed so that
//!   credentials stay out of project trees.
//! - `database/`: per-dataset service, taxonomy and query files. Lookup order
//!   is `<A2X_REGISTRY_HOME>/database`, then `./database` under the current
//!   directory, then `~/.a2x_registry/database`.
//!
//! Nothing here creates directories; callers do that on write.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the data root.
pub const ENV_VAR: &str = "A2X_REGISTRY_HOME";

/// Bundled template for `llm_apikey.json`.
pub const LLM_APIKEY_EXAMPLE: &str = include_str!("../llm_apikey.example.json");

/// `~/.a2x_registry`, or `.a2x_registry` under the current directory when no
/// home directory can be determined.
pub fn default_user_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".a2x_registry")
}

fn env_home() -> Option<PathBuf> {
    ap_support::env::current().get_non_blank(ENV_VAR).map(|v| {
        let p = PathBuf::from(v);
        std::fs::canonicalize(&p).unwrap_or(p)
    })
}

/// Home directory used by [`database_dir`].
///
/// Lookup: `A2X_REGISTRY_HOME`, then the current directory if it contains
/// `./database/`, then `~/.a2x_registry/`.
pub fn get_home() -> PathBuf {
    if let Some(env) = env_home() {
        return env;
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("database").is_dir() {
            return cwd;
        }
    }
    default_user_home()
}

pub fn database_dir() -> PathBuf {
    get_home().join("database")
}

pub fn dataset_dir(dataset: &str) -> PathBuf {
    database_dir().join(dataset)
}

/// Resolved `llm_apikey.json` path: `<A2X_REGISTRY_HOME>/llm_apikey.json`
/// when the variable is set, otherwise `~/.a2x_registry/llm_apikey.json`.
pub fn llm_apikey_path() -> PathBuf {
    env_home()
        .unwrap_or_else(default_user_home)
        .join("llm_apikey.json")
}

/// Write the bundled template to `path` if it does not exist yet.
pub fn ensure_llm_apikey_template(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, LLM_APIKEY_EXAMPLE)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn template_is_valid_json() -> TestResult {
        let v: serde_json::Value = serde_json::from_str(LLM_APIKEY_EXAMPLE)?;
        assert!(v["providers"].is_array());
        Ok(())
    }

    #[test]
    fn dataset_dir_is_under_database_dir() -> TestResult {
        let d = dataset_dir("ToolRet_clean");
        assert!(d.ends_with("database/ToolRet_clean"));
        Ok(())
    }
}
