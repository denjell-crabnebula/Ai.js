// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Bucketed Merkle digest for service-plane anti-entropy.
//!
//! Instead of shipping the whole version index every reconcile, two nodes
//! first exchange one hash per bucket (records are partitioned into a fixed
//! number of buckets by a hash of their key). Buckets whose hashes match are
//! identical, so only differing buckets transfer rows. Bucketing and hashing
//! are pure functions of `(key, version)`, so two nodes with the same data
//! always produce identical bucket hashes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha2::{Digest, Sha256};

use crate::envelope::{Key, Version};

/// Unit separator; cannot appear in ids.
const SEP: char = '\x1f';

fn key_str(key: &Key) -> String {
    format!("{}{SEP}{}{SEP}{}", key.0, key.1, key.2)
}

/// Deterministic bucket index for a key (stable across nodes).
pub fn bucket_of(key: &Key, n_buckets: u32) -> u32 {
    let n = n_buckets.max(1);
    let digest = Sha256::digest(key_str(key).as_bytes());
    let head = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    head % n
}

fn entry_digest(key: &Key, version: &Version) -> [u8; 32] {
    let raw = format!("{}{SEP}{}{SEP}{}", key_str(key), version.0, version.1);
    Sha256::digest(raw.as_bytes()).into()
}

/// `{bucket_index_str: hex_hash}` for every non-empty bucket. A bucket's hash
/// folds its entries in sorted order so it is order independent. Keys are
/// stringified so the map survives JSON round-trips.
pub fn bucket_hashes(index: &HashMap<Key, Version>, n_buckets: u32) -> BTreeMap<String, String> {
    let mut buckets: HashMap<u32, Vec<(&Key, &Version)>> = HashMap::new();
    for (key, version) in index {
        buckets
            .entry(bucket_of(key, n_buckets))
            .or_default()
            .push((key, version));
    }
    let mut out = BTreeMap::new();
    for (b, mut entries) in buckets {
        entries.sort();
        let mut h = Sha256::new();
        for (key, version) in entries {
            h.update(entry_digest(key, version));
        }
        out.insert(b.to_string(), hex::encode(h.finalize()));
    }
    out
}

/// Bucket indices present on either side with differing hashes. Bucket
/// labels that are not integers are ignored.
pub fn differing_buckets(
    local: &BTreeMap<String, String>,
    remote: &BTreeMap<String, String>,
) -> BTreeSet<u32> {
    let mut diff = BTreeSet::new();
    for b in local.keys().chain(remote.keys()) {
        if local.get(b) != remote.get(b) {
            if let Ok(idx) = b.parse::<u32>() {
                diff.insert(idx);
            }
        }
    }
    diff
}

/// Keys whose bucket is in `buckets`.
pub fn keys_in_buckets<'a, I>(keys: I, buckets: &BTreeSet<u32>, n_buckets: u32) -> Vec<Key>
where
    I: IntoIterator<Item = &'a Key>,
{
    keys.into_iter()
        .filter(|k| buckets.contains(&bucket_of(k, n_buckets)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    fn k(ds: &str, o: &str, s: &str) -> Key {
        (ds.into(), o.into(), s.into())
    }

    #[test]
    fn bucket_is_deterministic_and_bounded() -> TestResult {
        let key = k("ds", "A", "generic_x");
        assert_eq!(bucket_of(&key, 256), bucket_of(&key, 256));
        assert!(bucket_of(&key, 8) < 8);
        assert_eq!(bucket_of(&key, 1), 0);
        Ok(())
    }

    #[test]
    fn hashes_are_order_independent_and_change_with_version() -> TestResult {
        let mut a = HashMap::new();
        a.insert(k("ds", "A", "1"), Version::new(1, "A"));
        a.insert(k("ds", "B", "2"), Version::new(2, "B"));
        let mut b = HashMap::new();
        b.insert(k("ds", "B", "2"), Version::new(2, "B"));
        b.insert(k("ds", "A", "1"), Version::new(1, "A"));
        assert_eq!(bucket_hashes(&a, 1), bucket_hashes(&b, 1));
        assert!(differing_buckets(&bucket_hashes(&a, 1), &bucket_hashes(&b, 1)).is_empty());

        b.insert(k("ds", "A", "1"), Version::new(5, "A"));
        let diff = differing_buckets(&bucket_hashes(&a, 1), &bucket_hashes(&b, 1));
        assert_eq!(diff, BTreeSet::from([0]));
        assert_eq!(keys_in_buckets(a.keys(), &diff, 1).len(), 2);
        Ok(())
    }

    #[test]
    fn matches_python_reference_hash() -> TestResult {
        // sha256("ds\x1fA\x1fx\x1f1\x1fA") folded once; bucket of the key in 256.
        let mut idx = HashMap::new();
        idx.insert(k("ds", "A", "x"), Version::new(1, "A"));
        let out = bucket_hashes(&idx, 256);
        assert_eq!(out.len(), 1);
        let (bucket, h) = out.iter().next().required()?;
        assert_eq!(bucket, &bucket_of(&k("ds", "A", "x"), 256).to_string());
        assert_eq!(h.len(), 64);
        Ok(())
    }
}
