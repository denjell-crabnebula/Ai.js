// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `HeartbeatSweeper`: background task driving lease state transitions.
//!
//! Each tick calls `HeartbeatStore::sweep_tick` and hard deletes the
//! returned entries through [`HardDeleter`], the same path an admin
//! `DELETE /services/{sid}` takes. Failures are logged, never raised.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinHandle;

use super::store::HeartbeatStore;
use super::system_ctx::system_ctx;
use crate::register::RegistryService;

/// Performs the hard delete of an expired service.
pub trait HardDeleter: Send + Sync {
    fn hard_delete(&self, dataset: &str, service_id: &str) -> Result<(), String>;
}

impl HardDeleter for RegistryService {
    fn hard_delete(&self, dataset: &str, service_id: &str) -> Result<(), String> {
        self.deregister(dataset, service_id, Some(&system_ctx()))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Background sweeper. `start` spawns a tokio task; `sweep_once` drives one
/// pass synchronously for tests.
pub struct HeartbeatSweeper {
    svc: Arc<dyn HardDeleter>,
    store: Arc<HeartbeatStore>,
    period: Duration,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl HeartbeatSweeper {
    pub fn new(svc: Arc<dyn HardDeleter>, store: Arc<HeartbeatStore>, period: Duration) -> Self {
        Self {
            svc,
            store,
            period,
            handle: Mutex::new(None),
        }
    }

    /// Spawn the background task. Idempotent.
    pub fn start(self: &Arc<Self>) {
        let mut guard = self.handle.lock();
        if guard.as_ref().map(|h| !h.is_finished()).unwrap_or(false) {
            return;
        }
        let me = self.clone();
        *guard = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(me.period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                me.sweep_once();
            }
        }));
        tracing::info!("heartbeat: sweeper started (period={:?})", self.period);
    }

    /// Abort the background task.
    pub fn stop(&self) {
        if let Some(h) = self.handle.lock().take() {
            h.abort();
        }
    }

    /// Run one sweep pass.
    pub fn sweep_once(&self) {
        self.sweep_once_at(None)
    }

    /// Run one sweep pass at an explicit monotonic `now`.
    pub fn sweep_once_at(&self, now: Option<f64>) {
        let (newly_unhealthy, to_hard_delete) = self.store.sweep_tick(now);
        for (dataset, sid) in newly_unhealthy {
            tracing::info!("heartbeat: service marked unhealthy ({}, {})", dataset, sid);
        }
        for (dataset, sid) in to_hard_delete {
            match self.svc.hard_delete(&dataset, &sid) {
                Ok(()) => tracing::info!(
                    "heartbeat: hard-deleted ({}, {}) after grace expired",
                    dataset,
                    sid
                ),
                Err(e) => tracing::warn!("heartbeat: hard-delete failed for ({}, {}): {}", dataset, sid, e),
            }
        }
    }
}

impl Drop for HeartbeatSweeper {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::store::LeaseConfig;
    use ap_support::testing::TestResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(AtomicUsize);
    impl HardDeleter for Counting {
        fn hard_delete(&self, _: &str, _: &str) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    struct Broken;
    impl HardDeleter for Broken {
        fn hard_delete(&self, _: &str, _: &str) -> Result<(), String> {
            Err("oops".into())
        }
    }

    fn store() -> Arc<HeartbeatStore> {
        Arc::new(HeartbeatStore::new(Arc::new(|_: &str| LeaseConfig {
            enabled: true,
            min_ttl: 1,
            max_ttl: 3,
            grace_period: 2,
            schema_version: 1,
        })))
    }

    #[tokio::test]
    async fn sweeper_hard_deletes_and_survives_errors() -> TestResult {
        let s = store();
        let deleter = Arc::new(Counting(AtomicUsize::new(0)));
        let sweeper = HeartbeatSweeper::new(deleter.clone(), s.clone(), Duration::from_secs(60));
        let lease = s.install("ds", "a", 1);
        sweeper.sweep_once_at(Some(lease.grace_deadline + 0.001));
        assert_eq!(deleter.0.load(Ordering::SeqCst), 1);
        assert!(s.get_lease("ds", "a").is_none());

        let broken = HeartbeatSweeper::new(Arc::new(Broken), s.clone(), Duration::from_secs(60));
        let lease = s.install("ds", "b", 1);
        broken.sweep_once_at(Some(lease.grace_deadline + 0.001));
        assert!(s.get_lease("ds", "b").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn start_is_idempotent() -> TestResult {
        let s = store();
        let sweeper = Arc::new(HeartbeatSweeper::new(
            Arc::new(Counting(AtomicUsize::new(0))),
            s,
            Duration::from_millis(5),
        ));
        sweeper.start();
        sweeper.start();
        tokio::time::sleep(Duration::from_millis(15)).await;
        sweeper.stop();
        Ok(())
    }
}
