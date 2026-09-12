// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Cluster runtime tuning knobs.
//!
//! A single immutable config value passed to [`crate::ClusterStore`] and the
//! background sweepers. Operators override any knob at deploy time through
//! `A2X_REGISTRY_CLUSTER_*` environment variables (see
//! [`ClusterConfig::from_env`]). A malformed value logs a warning and keeps
//! the default.

/// Environment variable prefix for every knob in [`ClusterConfig`].
use ap_support::env::EnvSource;

pub const ENV_PREFIX: &str = "A2X_REGISTRY_CLUSTER_";

/// Environment variable holding the base URL peers use to reach this node.
/// Read by the backend, not by [`ClusterConfig::from_env`].
pub const ENV_ADVERTISE: &str = "A2X_REGISTRY_CLUSTER_ADVERTISE";

/// Tuning knobs, all in seconds unless noted.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterConfig {
    /// Direct-link keepalive period.
    pub keepalive_interval: f64,
    /// A peer silent past this window is dropped and its records evicted.
    pub hold_timeout: f64,
    /// Period of the anti-entropy reconcile plus GC pass.
    pub anti_entropy_interval: f64,
    /// Per-request HTTP timeout for peer calls.
    pub http_timeout: f64,
    /// Max concurrent peer calls per fan-out (broadcast / keepalive).
    pub broadcast_workers: usize,
    /// Number of Merkle anti-entropy buckets. Must match cluster-wide.
    pub merkle_buckets: u32,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            keepalive_interval: 10.0,
            hold_timeout: 30.0,
            anti_entropy_interval: 20.0,
            http_timeout: 5.0,
            broadcast_workers: 32,
            merkle_buckets: 256,
        }
    }
}

impl ClusterConfig {
    /// Retention of local tombstones and of the post-eviction suppression
    /// cooldown. Derived as `hold_timeout + keepalive_interval` so every peer
    /// has evicted its stale replica before the deletion is forgotten.
    pub fn tombstone_retention(&self) -> f64 {
        self.hold_timeout + self.keepalive_interval
    }

    /// Build a config from defaults, overriding any knob present as an
    /// `A2X_REGISTRY_CLUSTER_<FIELD>` environment variable. Blank values keep
    /// the default; a non-numeric value logs a warning and keeps the default.
    /// Integer knobs accept `"10"` or `"10.0"`.
    pub fn from_env() -> Self {
        Self::from_env_source(ap_support::env::current())
    }

    /// [`ClusterConfig::from_env`] reading from `env`.
    pub fn from_env_source(env: &dyn EnvSource) -> Self {
        let mut cfg = Self::default();
        cfg.keepalive_interval = float_env(env, "KEEPALIVE_INTERVAL", cfg.keepalive_interval);
        cfg.hold_timeout = float_env(env, "HOLD_TIMEOUT", cfg.hold_timeout);
        cfg.anti_entropy_interval = float_env(env, "ANTI_ENTROPY_INTERVAL", cfg.anti_entropy_interval);
        cfg.http_timeout = float_env(env, "HTTP_TIMEOUT", cfg.http_timeout);
        cfg.broadcast_workers =
            int_env(env, "BROADCAST_WORKERS", cfg.broadcast_workers as i64).max(0) as usize;
        cfg.merkle_buckets = int_env(env, "MERKLE_BUCKETS", cfg.merkle_buckets as i64).max(0) as u32;
        cfg
    }
}

fn raw_env(env: &dyn EnvSource, field: &str) -> Option<(String, String)> {
    let name = format!("{ENV_PREFIX}{field}");
    let raw = env.get(&name)?;
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some((name, raw))
}

fn float_env(env: &dyn EnvSource, field: &str, default: f64) -> f64 {
    match raw_env(env, field) {
        None => default,
        Some((name, raw)) => match raw.parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("cluster: ignoring invalid {name}={raw:?} (using default {default})");
                default
            }
        },
    }
}

fn int_env(env: &dyn EnvSource, field: &str, default: i64) -> i64 {
    match raw_env(env, field) {
        None => default,
        Some((name, raw)) => match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => v.trunc() as i64,
            _ => {
                tracing::warn!("cluster: ignoring invalid {name}={raw:?} (using default {default})");
                default
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn defaults_and_retention() -> TestResult {
        let cfg = ClusterConfig::default();
        assert_eq!(cfg.hold_timeout, 30.0);
        assert_eq!(cfg.keepalive_interval, 10.0);
        assert_eq!(cfg.tombstone_retention(), 40.0);
        assert_eq!(cfg.broadcast_workers, 32);
        assert_eq!(cfg.merkle_buckets, 256);
        Ok(())
    }
}
