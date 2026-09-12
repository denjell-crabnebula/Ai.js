// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Persisted cluster state: node identity, local version map and tombstones.
//!
//! This is the only thing the cluster module writes to disk. Foreign
//! (replicated) records are memory-only and re-synced on reconnect, but a few
//! things must survive a restart:
//!
//! - `node_id`: stable global identity for this instance.
//! - `version_clock`: last emitted version timestamp (ms). Seeds the
//!   monotonic guard so a wall-clock step-back cannot make a new local write
//!   look older than a previous one.
//! - `local_versions` / `tombstones`: version of every local-origin record
//!   and of every local deletion, so digests stay authoritative after a
//!   restart and a not-yet-propagated delete still wins LWW.
//! - `cluster_id` / `last_roster` / `my_membership_version`: the membership
//!   control plane's persisted view, used to rejoin the mesh after restart.
//!
//! Presence of the file is what makes the module opt-in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use a2x_common::atomic::atomic_write_json;
use a2x_common::paths::get_home;

use crate::envelope::Version;
use crate::errors::{ClusterError, Result};

/// Environment variable pointing directly at the `cluster_state.json` file.
pub const ENV_STATE_PATH: &str = "A2X_REGISTRY_CLUSTER_STATE";

/// Composite map key separator: `"{dataset}\x00{service_id}"`.
const SEP: char = '\x00';

/// Resolve the `cluster_state.json` location: `A2X_REGISTRY_CLUSTER_STATE`
/// when set, otherwise `<get_home()>/cluster_state.json`.
pub fn state_path() -> PathBuf {
    if let Some(env) = ap_support::env::current().get_non_blank(ENV_STATE_PATH) {
        return expand_user(&env);
    }
    get_home().join("cluster_state.json")
}

fn expand_user(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Composite key for `local_versions` and `tombstones`.
pub fn make_key(dataset: &str, service_id: &str) -> String {
    format!("{dataset}{SEP}{service_id}")
}

/// Inverse of [`make_key`]. A key without a separator yields an empty
/// service id, like Python's `str.partition`.
pub fn split_key(key: &str) -> (String, String) {
    match key.split_once(SEP) {
        Some((ds, sid)) => (ds.to_string(), sid.to_string()),
        None => (key.to_string(), String::new()),
    }
}

/// A stable, readable, globally unique node id (`reg-<12 hex>`).
pub fn generate_node_id() -> String {
    format!("reg-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// A local deletion: the deletion's LWW version plus when it happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub version: Version,
    pub deleted_at_ms: i64,
}

/// In-memory mirror of `cluster_state.json`. Mutations call [`ClusterState::save`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusterState {
    pub node_id: String,
    #[serde(default)]
    pub version_clock: i64,
    /// composite key -> version of the local-origin record
    #[serde(default)]
    pub local_versions: BTreeMap<String, Version>,
    /// composite key -> tombstone
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
    /// The cluster this node belongs to (`None` = standalone).
    #[serde(default)]
    pub cluster_id: Option<String>,
    /// Last known roster as membership-record objects (live members plus
    /// recent removal tombstones).
    #[serde(default)]
    pub last_roster: Vec<Value>,
    /// Version of this node's own membership record.
    #[serde(default)]
    pub my_membership_version: Option<Version>,
    /// File the state is persisted to. Not part of the JSON.
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl ClusterState {
    /// Fresh in-memory state for `node_id` bound to `path`.
    pub fn new(node_id: impl Into<String>, path: Option<PathBuf>) -> Self {
        ClusterState {
            node_id: node_id.into(),
            version_clock: 0,
            local_versions: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            cluster_id: None,
            last_roster: Vec::new(),
            my_membership_version: None,
            path,
        }
    }

    /// Load from the default location, or `Ok(None)` if the file is absent.
    pub fn load() -> Result<Option<ClusterState>> {
        Self::load_from(&state_path())
    }

    /// Load from `path`, or `Ok(None)` if the file does not exist. A file
    /// written before the membership feature loads with standalone defaults.
    pub fn load_from(path: &Path) -> Result<Option<ClusterState>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path).map_err(|e| ClusterError::io(path, e))?;
        let mut state: ClusterState = serde_json::from_str(&text).map_err(|e| ClusterError::json(path, e))?;
        state.path = Some(path.to_path_buf());
        Ok(Some(state))
    }

    /// Create and persist a fresh state file at the default location.
    pub fn init(node_id: Option<&str>) -> Result<ClusterState> {
        Self::init_at(node_id, &state_path())
    }

    /// Create and persist a fresh state file. Fails if one already exists.
    pub fn init_at(node_id: Option<&str>, path: &Path) -> Result<ClusterState> {
        if path.exists() {
            return Err(ClusterError::AlreadyInitialized(path.to_path_buf()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ClusterError::io(parent, e))?;
        }
        let node_id = node_id.map(str::to_string).unwrap_or_else(generate_node_id);
        let state = ClusterState::new(node_id, Some(path.to_path_buf()));
        state.save()?;
        Ok(state)
    }

    /// Atomically persist the current state to `path` (default location when
    /// unset).
    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(state_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ClusterError::io(parent, e))?;
        }
        atomic_write_json(&path, &self.to_json()).map_err(|e| ClusterError::io(&path, e))
    }

    /// The on-disk JSON object (same layout as the Python `to_dict`).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn key_roundtrip() -> TestResult {
        let k = make_key("ds1", "generic_abc");
        assert_eq!(split_key(&k), ("ds1".to_string(), "generic_abc".to_string()));
        assert_eq!(split_key("nosep"), ("nosep".to_string(), String::new()));
        Ok(())
    }

    #[test]
    fn node_ids_unique_and_prefixed() -> TestResult {
        let ids: std::collections::HashSet<String> = (0..100).map(|_| generate_node_id()).collect();
        assert_eq!(ids.len(), 100);
        assert!(ids.iter().all(|i| i.starts_with("reg-") && i.len() == 16));
        Ok(())
    }

    #[test]
    fn init_writes_exact_layout() -> TestResult {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("cluster_state.json");
        let state = ClusterState::init_at(Some("reg-fixed"), &p)?;
        assert_eq!(state.node_id, "reg-fixed");
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
        assert_eq!(
            raw,
            serde_json::json!({
                "node_id": "reg-fixed",
                "version_clock": 0,
                "local_versions": {},
                "tombstones": {},
                "cluster_id": null,
                "last_roster": [],
                "my_membership_version": null,
            })
        );
        assert!(matches!(
            ClusterState::init_at(Some("reg-2"), &p),
            Err(ClusterError::AlreadyInitialized(_))
        ));
        Ok(())
    }

    #[test]
    fn load_absent_and_roundtrip() -> TestResult {
        let dir = tempfile::tempdir()?;
        assert!(ClusterState::load_from(&dir.path().join("missing.json"))?.is_none());
        let p = dir.path().join("s.json");
        let mut state = ClusterState::init_at(Some("reg-rt"), &p)?;
        state.version_clock = 42;
        state
            .local_versions
            .insert(make_key("ds", "sid1"), Version::new(1000, "reg-rt"));
        state.tombstones.insert(
            make_key("ds", "sid2"),
            Tombstone {
                version: Version::new(2000, "reg-rt"),
                deleted_at_ms: 2000,
            },
        );
        state.cluster_id = Some("clu-abc123".into());
        state.last_roster = vec![
            serde_json::json!({"node_id": "B", "cluster_id": "clu-abc123", "address": "http://b:8000", "version": [10, "B"], "removed": false}),
            serde_json::json!({"node_id": "C", "cluster_id": "clu-abc123", "address": "http://c:8000", "version": [20, "A"], "removed": true}),
        ];
        state.my_membership_version = Some(Version::new(5, "reg-m"));
        state.save()?;

        let loaded = ClusterState::load_from(&p)?.required()?;
        assert_eq!(loaded.version_clock, 42);
        assert_eq!(
            loaded.local_versions[&make_key("ds", "sid1")],
            Version::new(1000, "reg-rt")
        );
        let tomb = &loaded.tombstones[&make_key("ds", "sid2")];
        assert_eq!(tomb.version, Version::new(2000, "reg-rt"));
        assert_eq!(tomb.deleted_at_ms, 2000);
        assert_eq!(loaded.cluster_id.as_deref(), Some("clu-abc123"));
        assert_eq!(loaded.my_membership_version, Some(Version::new(5, "reg-m")));
        assert_eq!(loaded.last_roster[1]["removed"], true);
        assert_eq!(loaded.path.as_deref(), Some(p.as_path()));
        Ok(())
    }

    #[test]
    fn forward_compat_old_state_loads_standalone() -> TestResult {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("old.json");
        std::fs::write(
            &p,
            r#"{"node_id": "reg-old", "version_clock": 5, "local_versions": {}, "tombstones": {}}"#,
        )?;
        let state = ClusterState::load_from(&p)?.required()?;
        assert_eq!(state.version_clock, 5);
        assert!(state.cluster_id.is_none());
        assert!(state.last_roster.is_empty());
        assert!(state.my_membership_version.is_none());
        Ok(())
    }

    #[test]
    fn corrupt_file_is_an_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("bad.json");
        std::fs::write(&p, "{ this is not valid json")?;
        assert!(matches!(
            ClusterState::load_from(&p),
            Err(ClusterError::Json { .. })
        ));
        Ok(())
    }
}
