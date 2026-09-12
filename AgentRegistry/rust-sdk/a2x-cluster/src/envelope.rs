// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Sync envelope and LWW version helpers.
//!
//! A [`SyncEnvelope`] wraps one registry record (or a deletion tombstone)
//! with the metadata the replication layer needs. `origin_id` is the node
//! that owns the record; with origin-only writes the global identity is
//! `(dataset, origin_id, service_id)`. `version` is the LWW key
//! `(updated_at_ms, node_id)`, compared lexicographically.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// LWW version `(updated_at_ms, node_id)`. Serialises as a two-element JSON
/// array, exactly like the Python tuple.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Version(pub i64, pub String);

impl Version {
    pub fn new(ms: i64, node_id: impl Into<String>) -> Self {
        Version(ms, node_id.into())
    }

    pub fn ms(&self) -> i64 {
        self.0
    }

    pub fn node_id(&self) -> &str {
        &self.1
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0).then_with(|| self.1.cmp(&other.1))
    }
}

/// Global identity key `(dataset, origin_id, service_id)`.
pub type Key = (String, String, String);

/// One replicated record or tombstone on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncEnvelope {
    pub dataset: String,
    pub service_id: String,
    pub origin_id: String,
    pub version: Version,
    #[serde(default)]
    pub tombstone: bool,
    /// `{"entry": <RegistryEntry>, "wrapped": <list row>}` or `null` for a
    /// tombstone.
    #[serde(default)]
    pub payload: Option<Value>,
}

impl SyncEnvelope {
    /// Global identity key.
    pub fn key(&self) -> Key {
        (
            self.dataset.clone(),
            self.origin_id.clone(),
            self.service_id.clone(),
        )
    }

    /// The `wrapped` list row inside the payload, if present and an object.
    pub fn wrapped(&self) -> Option<&serde_json::Map<String, Value>> {
        self.payload.as_ref()?.get("wrapped")?.as_object()
    }

    /// The `entry` record inside the payload, if present.
    pub fn entry(&self) -> Option<&Value> {
        self.payload.as_ref()?.get("entry")
    }
}

/// True if version `a` should win over `b` under LWW.
///
/// `b == None` (record never seen) always loses. Otherwise strict
/// lexicographic greater-than on `(updated_at_ms, node_id)`. Equal versions
/// are not newer, so re-applying the same envelope is idempotent.
pub fn version_newer(a: &Version, b: Option<&Version>) -> bool {
    match b {
        None => true,
        Some(b) => a > b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn version_ordering_is_lexicographic() -> TestResult {
        assert!(version_newer(&Version::new(2, "A"), Some(&Version::new(1, "Z"))));
        assert!(version_newer(&Version::new(1, "B"), Some(&Version::new(1, "A"))));
        assert!(!version_newer(&Version::new(1, "A"), Some(&Version::new(1, "A"))));
        assert!(!version_newer(&Version::new(1, "A"), Some(&Version::new(1, "B"))));
        assert!(version_newer(&Version::new(0, ""), None));
        Ok(())
    }

    #[test]
    fn envelope_wire_shape() -> TestResult {
        let raw = serde_json::json!({
            "dataset": "ds", "service_id": "s", "origin_id": "A",
            "version": [5, "A"]
        });
        let env: SyncEnvelope = serde_json::from_value(raw)?;
        assert!(!env.tombstone);
        assert!(env.payload.is_none());
        let out = serde_json::to_value(&env)?;
        assert_eq!(out["version"], serde_json::json!([5, "A"]));
        assert_eq!(out["tombstone"], false);
        assert!(out.get("payload").required()?.is_null());
        Ok(())
    }
}
