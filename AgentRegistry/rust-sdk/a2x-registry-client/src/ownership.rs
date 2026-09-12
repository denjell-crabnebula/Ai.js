// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Persistent tracker for the services registered by this client.
//!
//! The ownership file is one JSON document shared across registries, keyed
//! by `base_url`. Every mutation re-reads the file under a file lock, merges
//! this client's segment and atomically rewrites it (`.tmp` + rename), so
//! concurrent SDK processes do not lose each other's updates.
//!
//! File format (schema version 1):
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "data": { "<base_url>": { "<dataset>": ["<sid>", "..."] } }
//! }
//! ```
//!
//! Legacy files without `schema_version` (flat `{base_url: ...}`) still load.
//! A `None` file path selects memory-only mode. Save failures are logged as
//! warnings: the HTTP call already succeeded, so failing would make callers
//! retry and create duplicates.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

const SCHEMA_VERSION: u64 = 1;
const LOCK_SUFFIX: &str = ".lock";

/// Upper bound for acquiring the ownership file lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// In-memory ownership map with optional file persistence.
#[derive(Debug)]
pub struct OwnershipStore {
    file_path: Option<PathBuf>,
    base_url: String,
    data: Mutex<BTreeMap<String, BTreeSet<String>>>,
}

impl OwnershipStore {
    /// Load the segment for `base_url` from `file_path` (if any) into memory.
    pub fn new(file_path: Option<PathBuf>, base_url: &str) -> Self {
        let store = OwnershipStore {
            file_path,
            base_url: base_url.to_string(),
            data: Mutex::new(BTreeMap::new()),
        };
        store.load();
        store
    }

    /// Path of the backing file, `None` in memory-only mode.
    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// True when `service_id` in `dataset` was registered by this client.
    pub fn contains(&self, dataset: &str, service_id: &str) -> bool {
        self.data
            .lock()
            .get(dataset)
            .is_some_and(|s| s.contains(service_id))
    }

    /// Datasets with at least one owned service (snapshot).
    pub fn datasets(&self) -> Vec<String> {
        self.data.lock().keys().cloned().collect()
    }

    /// Owned service ids in `dataset` (snapshot; empty when unknown).
    pub fn list(&self, dataset: &str) -> Vec<String> {
        self.data
            .lock()
            .get(dataset)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Record ownership and persist.
    pub fn add(&self, dataset: &str, service_id: &str) {
        let mut data = self.data.lock();
        data.entry(dataset.to_string())
            .or_default()
            .insert(service_id.to_string());
        self.save_locked(&data);
    }

    /// Forget one service and persist. Empty datasets are dropped.
    pub fn remove(&self, dataset: &str, service_id: &str) {
        let mut data = self.data.lock();
        let Some(bucket) = data.get_mut(dataset) else {
            return;
        };
        bucket.remove(service_id);
        if bucket.is_empty() {
            data.remove(dataset);
        }
        self.save_locked(&data);
    }

    /// Forget every service in `dataset` and persist.
    pub fn remove_dataset(&self, dataset: &str) {
        let mut data = self.data.lock();
        if data.remove(dataset).is_none() {
            return;
        }
        self.save_locked(&data);
    }

    fn load(&self) {
        let Some(path) = &self.file_path else {
            return;
        };
        if !path.exists() {
            return;
        }
        let raw: Value = match fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            Some(v) => v,
            None => return,
        };
        let Some(Value::Object(segment)) = self.extract_segment(&raw) else {
            return;
        };
        let mut data = self.data.lock();
        for (dataset, ids) in segment {
            if let Value::Array(items) = ids {
                let set: BTreeSet<String> = items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                data.insert(dataset.clone(), set);
            }
        }
    }

    fn extract_segment<'a>(&self, raw: &'a Value) -> Option<&'a Value> {
        let obj = raw.as_object()?;
        if obj.get("schema_version").and_then(Value::as_u64) == Some(SCHEMA_VERSION) {
            return obj.get("data")?.as_object()?.get(&self.base_url);
        }
        obj.get(&self.base_url)
    }

    fn save_locked(&self, data: &BTreeMap<String, BTreeSet<String>>) {
        let Some(path) = &self.file_path else {
            return;
        };
        if let Err(e) = self.save_to_disk(path, data) {
            tracing::warn!(
                "a2x-client: failed to persist ownership to {}: {e}. In-memory state is correct; a later successful write will catch up.",
                path.display()
            );
        }
    }

    fn save_to_disk(&self, path: &Path, data: &BTreeMap<String, BTreeSet<String>>) -> std::io::Result<()> {
        let _guard = FileLock::acquire(path)?;
        let mut existing = read_existing(path);
        let mut section = match existing.remove("data") {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        if data.is_empty() {
            section.remove(&self.base_url);
        } else {
            let mut segment = Map::new();
            for (ds, ids) in data {
                segment.insert(
                    ds.clone(),
                    Value::Array(ids.iter().map(|s| Value::String(s.clone())).collect()),
                );
            }
            section.insert(self.base_url.clone(), Value::Object(segment));
        }
        existing.insert("data".to_string(), Value::Object(section));
        let tmp = with_suffix(path, ".tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(serde_json::to_string_pretty(&Value::Object(existing))?.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn read_existing(path: &Path) -> Map<String, Value> {
    let fresh = || {
        let mut m = Map::new();
        m.insert("schema_version".into(), json!(SCHEMA_VERSION));
        m.insert("data".into(), Value::Object(Map::new()));
        m
    };
    if !path.exists() {
        return fresh();
    }
    let raw: Value = match fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(v) => v,
        None => return fresh(),
    };
    let Value::Object(mut obj) = raw else {
        return fresh();
    };
    if obj.get("schema_version").and_then(Value::as_u64) == Some(SCHEMA_VERSION) {
        if !obj.get("data").is_some_and(Value::is_object) {
            obj.insert("data".into(), Value::Object(Map::new()));
        }
        return obj;
    }
    // Migrate the legacy v0 flat shape into the v1 wrapper.
    let mut data = Map::new();
    for (key, value) in obj {
        if value.is_object() {
            data.insert(key, value);
        }
    }
    let mut migrated = fresh();
    migrated.insert("data".into(), Value::Object(data));
    migrated
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Exclusive lock on a `<file>.lock` sibling that survives atomic rewrites of the data file.
struct FileLock {
    file: fs::File,
}

impl FileLock {
    fn acquire(data_path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = data_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = with_suffix(data_path, LOCK_SUFFIX);
        let file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&lock_path)?;
        if file.metadata()?.len() == 0 {
            (&file).write_all(b"\0")?;
        }
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(FileLock { file }),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "timeout acquiring ownership file lock",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn memory_only_roundtrip() -> TestResult {
        let s = OwnershipStore::new(None, "http://x/");
        assert!(!s.contains("ds", "a"));
        s.add("ds", "a");
        s.add("ds", "b");
        assert!(s.contains("ds", "a"));
        assert_eq!(s.list("ds"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(s.datasets(), vec!["ds".to_string()]);
        s.remove("ds", "a");
        s.remove("ds", "b");
        assert!(s.datasets().is_empty());
        s.remove("missing", "x");
        s.remove_dataset("missing");
        Ok(())
    }

    #[test]
    fn persists_per_base_url_and_reloads() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested").join("owned.json");
        let a = OwnershipStore::new(Some(path.clone()), "http://a/");
        a.add("ds", "s1");
        let b = OwnershipStore::new(Some(path.clone()), "http://b/");
        b.add("other", "s2");

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        assert_eq!(raw["schema_version"], 1);
        assert_eq!(raw["data"]["http://a/"]["ds"], json!(["s1"]));
        assert_eq!(raw["data"]["http://b/"]["other"], json!(["s2"]));

        let a2 = OwnershipStore::new(Some(path.clone()), "http://a/");
        assert!(a2.contains("ds", "s1"));
        assert!(!a2.contains("other", "s2"));

        a2.remove_dataset("ds");
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        assert!(raw["data"].get("http://a/").is_none());
        assert!(raw["data"].get("http://b/").is_some());
        Ok(())
    }

    #[test]
    fn legacy_flat_file_is_loaded_and_migrated() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("owned.json");
        fs::write(&path, r#"{"http://a/": {"ds": ["s1", 5]}, "junk": 1}"#)?;
        let a = OwnershipStore::new(Some(path.clone()), "http://a/");
        assert!(a.contains("ds", "s1"));
        a.add("ds", "s2");
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        assert_eq!(raw["schema_version"], 1);
        assert_eq!(raw["data"]["http://a/"]["ds"], json!(["s1", "s2"]));
        assert!(raw.get("junk").is_none());
        Ok(())
    }

    #[test]
    fn corrupt_file_starts_clean() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("owned.json");
        fs::write(&path, "garbage{{")?;
        let a = OwnershipStore::new(Some(path.clone()), "http://a/");
        assert!(a.datasets().is_empty());
        a.add("ds", "s1");
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        assert_eq!(raw["data"]["http://a/"]["ds"], json!(["s1"]));
        Ok(())
    }
}
