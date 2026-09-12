// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Generic TTL lease state machine shared by the heartbeat and cluster modules.
//!
//! A [`LeaseTable`] tracks per-key countdown leases through a two-stage
//! expiry: `Healthy` -> (TTL elapsed) -> `Unhealthy` -> (grace elapsed) ->
//! hard delete. The table owns only the pure state transitions. TTL and
//! grace values are supplied by the caller on [`LeaseTable::install`] and the
//! hard-delete action is performed by the caller from the list that
//! [`LeaseTable::sweep_tick`] returns.
//!
//! All times are seconds on a monotonic clock (see [`monotonic_now`]) so the
//! state machine is immune to wall-clock jumps. Tests pass explicit `now`
//! values to drive time forward without sleeping.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

static EPOCH: Lazy<Instant> = Lazy::new(Instant::now);

/// Seconds elapsed on a process-local monotonic clock.
pub fn monotonic_now() -> f64 {
    EPOCH.elapsed().as_secs_f64()
}

/// Lease health state. Serializes as lowercase text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseState {
    /// Renewed recently; `expires_at` is in the future.
    Healthy,
    /// TTL elapsed but still inside the grace window; can recover.
    Unhealthy,
}

/// Per-key lease countdown state. Time fields are monotonic seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub ttl_seconds: u64,
    pub grace_period_seconds: u64,
    /// When the state flips `Healthy` -> `Unhealthy`.
    pub expires_at: f64,
    /// When the state flips `Unhealthy` -> hard delete.
    pub grace_deadline: f64,
    pub last_renew_at: f64,
    pub state: LeaseState,
}

impl Lease {
    /// Wall-clock expiry for client-facing responses.
    pub fn expires_at_wall(&self, now_wall: f64, now_monotonic: f64) -> f64 {
        now_wall + (self.expires_at - now_monotonic)
    }
}

/// In-memory table of active leases keyed by an opaque `K`.
#[derive(Debug)]
pub struct LeaseTable<K: Eq + Hash + Clone> {
    leases: Mutex<HashMap<K, Lease>>,
}

impl<K: Eq + Hash + Clone> Default for LeaseTable<K> {
    fn default() -> Self {
        Self {
            leases: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash + Clone> LeaseTable<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create or replace a lease for `key`. Idempotent.
    ///
    /// `expired = true` seeds the lease directly in `Unhealthy` with the
    /// grace window already running. This is used for restart recovery where
    /// the holder gets one grace window to renew before hard delete.
    pub fn install(&self, key: K, ttl: u64, grace: u64, expired: bool, now: Option<f64>) -> Lease {
        let now = now.unwrap_or_else(monotonic_now);
        let lease = if expired {
            Lease {
                ttl_seconds: ttl,
                grace_period_seconds: grace,
                expires_at: now,
                grace_deadline: now + grace as f64,
                last_renew_at: now,
                state: LeaseState::Unhealthy,
            }
        } else {
            Lease {
                ttl_seconds: ttl,
                grace_period_seconds: grace,
                expires_at: now + ttl as f64,
                grace_deadline: now + ttl as f64 + grace as f64,
                last_renew_at: now,
                state: LeaseState::Healthy,
            }
        };
        self.leases.lock().insert(key, lease.clone());
        lease
    }

    /// Extend the lease and restore `Healthy`. Returns `None` when no lease
    /// exists for `key`.
    pub fn renew(&self, key: &K, now: Option<f64>) -> Option<Lease> {
        let now = now.unwrap_or_else(monotonic_now);
        let mut guard = self.leases.lock();
        let lease = guard.get_mut(key)?;
        lease.last_renew_at = now;
        lease.expires_at = now + lease.ttl_seconds as f64;
        lease.grace_deadline = lease.expires_at + lease.grace_period_seconds as f64;
        lease.state = LeaseState::Healthy;
        Some(lease.clone())
    }

    /// Mark a lease `Unhealthy` (default) or hard-drop it (`permanent`).
    /// Returns true if a lease was found. Idempotent.
    pub fn revoke(&self, key: &K, permanent: bool, now: Option<f64>) -> bool {
        let now = now.unwrap_or_else(monotonic_now);
        let mut guard = self.leases.lock();
        let Some(lease) = guard.get_mut(key) else {
            return false;
        };
        if permanent {
            guard.remove(key);
        } else {
            lease.state = LeaseState::Unhealthy;
            lease.expires_at = now;
            lease.grace_deadline = now + lease.grace_period_seconds as f64;
        }
        true
    }

    /// Remove the lease without touching state. Idempotent.
    pub fn drop_key(&self, key: &K) {
        self.leases.lock().remove(key);
    }

    /// Snapshot read.
    pub fn get(&self, key: &K) -> Option<Lease> {
        self.leases.lock().get(key).cloned()
    }

    /// True only when a lease exists and its state is `Unhealthy`.
    pub fn is_unhealthy(&self, key: &K) -> bool {
        self.leases
            .lock()
            .get(key)
            .map(|l| l.state == LeaseState::Unhealthy)
            .unwrap_or(false)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.leases.lock().contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.leases.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.leases.lock().is_empty()
    }

    /// All `(key, lease)` pairs.
    pub fn items(&self) -> Vec<(K, Lease)> {
        self.leases
            .lock()
            .iter()
            .map(|(k, l)| (k.clone(), l.clone()))
            .collect()
    }

    /// Single sweep pass. Returns `(newly_unhealthy, to_delete)`. Entries in
    /// `to_delete` are removed from the table; the caller performs the
    /// hard-delete side effects.
    pub fn sweep_tick(&self, now: Option<f64>) -> (Vec<K>, Vec<K>) {
        let now = now.unwrap_or_else(monotonic_now);
        let mut newly_unhealthy = Vec::new();
        let mut to_delete = Vec::new();
        let mut guard = self.leases.lock();
        for (key, lease) in guard.iter_mut() {
            if lease.state == LeaseState::Healthy && now >= lease.expires_at {
                lease.state = LeaseState::Unhealthy;
                newly_unhealthy.push(key.clone());
            }
            if lease.state == LeaseState::Unhealthy && now >= lease.grace_deadline {
                to_delete.push(key.clone());
            }
        }
        for key in &to_delete {
            guard.remove(key);
        }
        (newly_unhealthy, to_delete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn healthy_to_unhealthy_to_delete() -> TestResult {
        let t: LeaseTable<&str> = LeaseTable::new();
        t.install("a", 10, 5, false, Some(0.0));
        assert_eq!(t.sweep_tick(Some(9.9)), (vec![], vec![]));
        assert_eq!(t.sweep_tick(Some(10.0)), (vec!["a"], vec![]));
        assert!(t.is_unhealthy(&"a"));
        assert_eq!(t.sweep_tick(Some(14.9)), (vec![], vec![]));
        assert_eq!(t.sweep_tick(Some(15.0)), (vec![], vec!["a"]));
        assert!(t.get(&"a").is_none());
        Ok(())
    }

    #[test]
    fn renew_recovers_within_grace() -> TestResult {
        let t: LeaseTable<u32> = LeaseTable::new();
        t.install(1, 10, 5, false, Some(0.0));
        t.sweep_tick(Some(12.0));
        assert!(t.is_unhealthy(&1));
        let l = t.renew(&1, Some(12.0)).required()?;
        assert_eq!(l.state, LeaseState::Healthy);
        assert_eq!(l.expires_at, 22.0);
        assert_eq!(l.grace_deadline, 27.0);
        assert!(t.renew(&2, None).is_none());
        Ok(())
    }

    #[test]
    fn install_expired_and_revoke() -> TestResult {
        let t: LeaseTable<String> = LeaseTable::new();
        let l = t.install("x".into(), 10, 5, true, Some(100.0));
        assert_eq!(l.state, LeaseState::Unhealthy);
        assert_eq!(l.grace_deadline, 105.0);
        assert!(t.revoke(&"x".to_string(), false, Some(101.0)));
        assert_eq!(t.get(&"x".to_string()).required()?.grace_deadline, 106.0);
        assert!(t.revoke(&"x".to_string(), true, None));
        assert!(!t.revoke(&"x".to_string(), true, None));
        assert!(t.is_empty());
        Ok(())
    }
}
