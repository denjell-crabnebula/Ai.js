// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `HeartbeatStore`: per namespace lease tracking on top of the shared
//! `LeaseTable`. Pure runtime state, no disk I/O.

use std::sync::Arc;

use a2x_common::lease::LeaseTable;

use super::errors::{HeartbeatError, HeartbeatErrorCode};
use super::models::HeartbeatLease;
use crate::register::store::LeaseConfig;

/// Source of per namespace lease configuration (typically the registry).
pub trait LeaseConfigProvider: Send + Sync {
    fn lease_config(&self, dataset: &str) -> LeaseConfig;
}

impl LeaseConfigProvider for crate::register::RegistryService {
    fn lease_config(&self, dataset: &str) -> LeaseConfig {
        self.get_lease_config(dataset)
    }
}

impl<F: Fn(&str) -> LeaseConfig + Send + Sync> LeaseConfigProvider for F {
    fn lease_config(&self, dataset: &str) -> LeaseConfig {
        self(dataset)
    }
}

type LeaseKey = (String, String);

impl crate::register::UnhealthyCheck for HeartbeatStore {
    fn is_unhealthy(&self, dataset: &str, service_id: &str) -> bool {
        HeartbeatStore::is_unhealthy(self, dataset, service_id)
    }
}

/// In-memory registry of active heartbeat leases keyed by
/// `(dataset, service_id)`.
pub struct HeartbeatStore {
    config_provider: Arc<dyn LeaseConfigProvider>,
    table: LeaseTable<LeaseKey>,
}

impl HeartbeatStore {
    pub fn new(config_provider: Arc<dyn LeaseConfigProvider>) -> Self {
        Self {
            config_provider,
            table: LeaseTable::new(),
        }
    }

    /// Apply the four corner matrix. Returns the validated TTL, or `None`
    /// for a permanent service. Pure check, no state change.
    pub fn validate(&self, dataset: &str, client_ttl: Option<i64>) -> Result<Option<i64>, HeartbeatError> {
        let cfg = self.config_provider.lease_config(dataset);
        if !cfg.enabled {
            return match client_ttl {
                None => Ok(None),
                Some(_) => Err(HeartbeatError::not_supported(format!(
                    "Namespace '{dataset}' does not enable heartbeat leases. Remove 'lease_ttl' from the request or ask admin to enable it."
                ))),
            };
        }
        let (min_ttl, max_ttl) = (cfg.min_ttl, cfg.max_ttl);
        let Some(ttl) = client_ttl else {
            return Err(HeartbeatError {
                code: HeartbeatErrorCode::TtlRequired,
                message: format!(
                    "Namespace '{dataset}' requires 'lease_ttl' on registration; allowed range [{min_ttl}, {max_ttl}]."
                ),
                min_ttl: Some(min_ttl),
                max_ttl: Some(max_ttl),
            });
        };
        if ttl < min_ttl || ttl > max_ttl {
            return Err(HeartbeatError {
                code: HeartbeatErrorCode::TtlOutOfRange,
                message: format!(
                    "lease_ttl={ttl} out of range [{min_ttl}, {max_ttl}] for namespace '{dataset}'."
                ),
                min_ttl: Some(min_ttl),
                max_ttl: Some(max_ttl),
            });
        }
        Ok(Some(ttl))
    }

    fn grace(&self, dataset: &str) -> u64 {
        self.config_provider.lease_config(dataset).grace_period.max(0) as u64
    }

    /// Install a pre-validated lease. Replaces any existing lease.
    pub fn install(&self, dataset: &str, service_id: &str, ttl: i64) -> HeartbeatLease {
        let lease = self.table.install(
            (dataset.to_string(), service_id.to_string()),
            ttl.max(0) as u64,
            self.grace(dataset),
            false,
            None,
        );
        tracing::debug!("heartbeat: installed ({}, {}) ttl={}", dataset, service_id, ttl);
        lease
    }

    /// Validate and install in one call.
    pub fn grant(
        &self,
        dataset: &str,
        service_id: &str,
        client_ttl: Option<i64>,
    ) -> Result<Option<HeartbeatLease>, HeartbeatError> {
        match self.validate(dataset, client_ttl)? {
            None => Ok(None),
            Some(ttl) => Ok(Some(self.install(dataset, service_id, ttl))),
        }
    }

    /// Extend the lease and restore `Healthy`. `None` when no lease exists.
    pub fn heartbeat(&self, dataset: &str, service_id: &str) -> Option<HeartbeatLease> {
        self.heartbeat_at(dataset, service_id, None)
    }

    /// [`Self::heartbeat`] with an explicit monotonic `now`.
    pub fn heartbeat_at(&self, dataset: &str, service_id: &str, now: Option<f64>) -> Option<HeartbeatLease> {
        let lease = self
            .table
            .renew(&(dataset.to_string(), service_id.to_string()), now)?;
        tracing::debug!("heartbeat: extended ({}, {})", dataset, service_id);
        Some(lease)
    }

    /// Mark a lease unhealthy (default) or drop it (`permanent`).
    /// Returns true when a lease existed.
    pub fn revoke(&self, dataset: &str, service_id: &str, permanent: bool) -> bool {
        self.table
            .revoke(&(dataset.to_string(), service_id.to_string()), permanent, None)
    }

    /// Remove the lease without state transitions. Idempotent.
    pub fn drop_lease(&self, dataset: &str, service_id: &str) {
        self.table
            .drop_key(&(dataset.to_string(), service_id.to_string()));
    }

    /// True only when a lease exists and is unhealthy.
    pub fn is_unhealthy(&self, dataset: &str, service_id: &str) -> bool {
        self.table
            .is_unhealthy(&(dataset.to_string(), service_id.to_string()))
    }

    pub fn get_lease(&self, dataset: &str, service_id: &str) -> Option<HeartbeatLease> {
        self.table.get(&(dataset.to_string(), service_id.to_string()))
    }

    /// All `(dataset, service_id, lease)` triples.
    pub fn list_leases(&self) -> Vec<(String, String, HeartbeatLease)> {
        self.table
            .items()
            .into_iter()
            .map(|((ds, sid), l)| (ds, sid, l))
            .collect()
    }

    /// Single sweep pass: `(newly_unhealthy, to_hard_delete)`. Entries in
    /// the second list are removed from the table; the caller deletes them.
    pub fn sweep_tick(&self, now: Option<f64>) -> (Vec<LeaseKey>, Vec<LeaseKey>) {
        self.table.sweep_tick(now)
    }

    /// Re-grant grace window leases for entries persisted with `lease_ttl`.
    pub fn recover_from_persisted(&self, entries: &[(String, String, i64)]) {
        for (dataset, sid, ttl) in entries {
            self.table.install(
                (dataset.clone(), sid.clone()),
                (*ttl).max(0) as u64,
                self.grace(dataset),
                true,
                None,
            );
        }
        tracing::info!(
            "heartbeat: recovered {} leases from disk into grace window",
            entries.len()
        );
    }

    /// Wall clock expiry for a lease (client display).
    pub fn expires_at_wall(lease: &HeartbeatLease) -> f64 {
        lease.expires_at_wall(crate::util::now_wall(), a2x_common::lease::monotonic_now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2x_common::LeaseState;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};

    fn store(enabled: bool) -> HeartbeatStore {
        let provider = move |_: &str| LeaseConfig {
            enabled,
            min_ttl: 5,
            max_ttl: 60,
            grace_period: 10,
            schema_version: 1,
        };
        HeartbeatStore::new(Arc::new(provider))
    }

    #[test]
    fn four_corner_matrix() -> TestResult {
        let off = store(false);
        assert_eq!(off.validate("ds", None)?, None);
        assert_eq!(
            off.validate("ds", Some(30)).err_or_fail()?.code,
            HeartbeatErrorCode::NotSupported
        );
        let on = store(true);
        let e = on.validate("ds", None).err_or_fail()?;
        assert_eq!(e.code, HeartbeatErrorCode::TtlRequired);
        assert_eq!(e.body()["min_ttl"], 5);
        assert_eq!(
            on.validate("ds", Some(1)).err_or_fail()?.code,
            HeartbeatErrorCode::TtlOutOfRange
        );
        assert_eq!(
            on.validate("ds", Some(9999)).err_or_fail()?.code,
            HeartbeatErrorCode::TtlOutOfRange
        );
        assert_eq!(on.validate("ds", Some(5))?, Some(5));
        assert_eq!(on.validate("ds", Some(60))?, Some(60));
        Ok(())
    }

    #[test]
    fn lifecycle_and_recovery() -> TestResult {
        let s = store(true);
        let lease = s.grant("ds", "sid", Some(10))?.required()?;
        assert_eq!(lease.state, LeaseState::Healthy);
        let (nu, td) = s.sweep_tick(Some(lease.expires_at + 0.01));
        assert_eq!(nu, vec![("ds".to_string(), "sid".to_string())]);
        assert!(td.is_empty());
        assert!(s.is_unhealthy("ds", "sid"));
        let (nu, _) = s.sweep_tick(Some(lease.expires_at + 0.02));
        assert!(nu.is_empty());
        assert!(s.heartbeat("ds", "sid").is_some());
        assert!(!s.is_unhealthy("ds", "sid"));
        assert!(s.heartbeat("ds", "nope").is_none());
        assert!(s.revoke("ds", "sid", false));
        assert!(s.is_unhealthy("ds", "sid"));
        let l = s.get_lease("ds", "sid").required()?;
        let (_, td) = s.sweep_tick(Some(l.grace_deadline + 0.001));
        assert_eq!(td.len(), 1);
        assert!(s.get_lease("ds", "sid").is_none());
        assert!(!s.revoke("ds", "sid", true));
        s.recover_from_persisted(&[("ds".into(), "r".into(), 30)]);
        let r = s.get_lease("ds", "r").required()?;
        assert_eq!(r.state, LeaseState::Unhealthy);
        assert!(r.grace_deadline > a2x_common::lease::monotonic_now());
        assert_eq!(s.list_leases().len(), 1);
        s.drop_lease("ds", "r");
        assert!(s.list_leases().is_empty());
        Ok(())
    }
}
