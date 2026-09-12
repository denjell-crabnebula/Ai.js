// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client-side heartbeat renewal: one tokio task per `(dataset, service_id)`.
//!
//! Used by the client when `register_agent` is called with `auto_renew` and
//! the server granted a lease. The renewer wakes every `ttl / 3` seconds
//! (Eureka convention) and calls the heartbeat function. On failure it backs
//! off exponentially, capped at `ttl`; it never gives up on its own so the
//! lease gets one more chance if the network returns during the grace period.
//!
//! The first attempt happens after the first period, not at start, matching
//! Eureka's "first heartbeat at t + interval" behaviour.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::errors::ClientError;

/// Callback invoked on every tick: `(dataset, service_id)`.
///
/// The client injects a closure that performs the HTTP heartbeat, which keeps
/// this module transport-agnostic and testable with a plain function.
pub type HeartbeatFn =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<(), ClientError>> + Send + Sync>;

/// State shared between the handle and the running task.
struct Shared {
    dataset: String,
    service_id: String,
    ttl: u64,
    period: Duration,
    fn_: HeartbeatFn,
    stop: AtomicBool,
    notify: Notify,
}

/// Owns the join handle. Held only by handles, never by the task itself, so
/// dropping the last handle aborts a task that was never stopped explicitly.
struct TaskSlot {
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for TaskSlot {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

/// Single-service heartbeat task. Cloning yields another handle to the same task.
#[derive(Clone)]
pub struct HeartbeatRenewer {
    shared: Arc<Shared>,
    slot: Arc<TaskSlot>,
}

impl std::fmt::Debug for HeartbeatRenewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatRenewer")
            .field("dataset", &self.shared.dataset)
            .field("service_id", &self.shared.service_id)
            .field("ttl", &self.shared.ttl)
            .field("period", &self.shared.period)
            .field("stopped", &self.is_stopped())
            .finish()
    }
}

impl HeartbeatRenewer {
    /// Create a renewer. `period` defaults to `max(1s, ttl / 3)`; tests override it.
    ///
    /// Fails with [`ClientError::InvalidArgument`] when `ttl_seconds < 1`.
    pub fn new(
        dataset: &str,
        service_id: &str,
        ttl_seconds: i64,
        heartbeat_fn: HeartbeatFn,
        period: Option<Duration>,
    ) -> Result<Self, ClientError> {
        if ttl_seconds < 1 {
            return Err(ClientError::invalid(format!(
                "ttl_seconds must be >= 1, got {ttl_seconds}"
            )));
        }
        let ttl = ttl_seconds as u64;
        let period = period.unwrap_or_else(|| Duration::from_secs_f64((ttl as f64 / 3.0).max(1.0)));
        Ok(HeartbeatRenewer {
            shared: Arc::new(Shared {
                dataset: dataset.to_string(),
                service_id: service_id.to_string(),
                ttl,
                period,
                fn_: heartbeat_fn,
                stop: AtomicBool::new(false),
                notify: Notify::new(),
            }),
            slot: Arc::new(TaskSlot {
                task: Mutex::new(None),
            }),
        })
    }

    /// Dataset this renewer serves.
    pub fn dataset(&self) -> &str {
        &self.shared.dataset
    }

    /// Service id this renewer serves.
    pub fn service_id(&self) -> &str {
        &self.shared.service_id
    }

    /// Lease TTL in seconds.
    pub fn ttl(&self) -> u64 {
        self.shared.ttl
    }

    /// Nominal renewal period.
    pub fn period(&self) -> Duration {
        self.shared.period
    }

    /// True once `stop` or `signal_stop` was called.
    pub fn is_stopped(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
    }

    /// True while the background task exists and has not finished.
    pub fn is_running(&self) -> bool {
        self.slot.task.lock().as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Spawn the task on the current tokio runtime. Idempotent.
    ///
    /// Must be called from within a tokio runtime context.
    pub fn start(&self) {
        let mut slot = self.slot.task.lock();
        if slot.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        self.shared.stop.store(false, Ordering::SeqCst);
        let shared = Arc::clone(&self.shared);
        *slot = Some(tokio::spawn(async move { run(shared).await }));
    }

    /// Ask the task to exit without waiting for it. Idempotent.
    pub fn signal_stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.notify.notify_waiters();
        self.shared.notify.notify_one();
    }

    /// Ask the task to exit and wait for it to finish. Idempotent.
    pub async fn stop(&self) {
        self.signal_stop();
        let task = self.slot.task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

async fn run(shared: Arc<Shared>) {
    let mut cur_period = shared.period;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(cur_period) => {}
            _ = shared.notify.notified() => {}
        }
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        match (shared.fn_)(shared.dataset.clone(), shared.service_id.clone()).await {
            Ok(()) => cur_period = shared.period,
            Err(err) => {
                let doubled = cur_period.as_secs_f64() * 2.0;
                cur_period = Duration::from_secs_f64(doubled.min(shared.ttl as f64));
                tracing::warn!(
                    "heartbeat renewer for ({}, {}) failed; backing off to {}s: {err}",
                    shared.dataset,
                    shared.service_id,
                    cur_period.as_secs_f64()
                );
                if cur_period.as_secs_f64() >= shared.ttl as f64 {
                    tracing::warn!(
                        "heartbeat renewer for ({}, {}) surrendering: lease will expire on the server. Caller should re-register or rely on grace_period.",
                        shared.dataset,
                        shared.service_id
                    );
                }
            }
        }
    }
}

/// Per-client registry of active renewers, keyed by `(dataset, service_id)`.
///
/// Registering the same key twice stops the old renewer first. Dropping the
/// registry aborts every task it still holds.
#[derive(Default)]
pub struct HeartbeatRegistry {
    renewers: Mutex<HashMap<(String, String), HeartbeatRenewer>>,
}

impl std::fmt::Debug for HeartbeatRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatRegistry")
            .field("count", &self.len())
            .finish()
    }
}

impl HeartbeatRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of renewers currently tracked.
    pub fn len(&self) -> usize {
        self.renewers.lock().len()
    }

    /// True when no renewer is tracked.
    pub fn is_empty(&self) -> bool {
        self.renewers.lock().is_empty()
    }

    /// True when a renewer exists for `(dataset, service_id)`.
    pub fn contains(&self, dataset: &str, service_id: &str) -> bool {
        self.renewers
            .lock()
            .contains_key(&(dataset.to_string(), service_id.to_string()))
    }

    /// Install and start `renewer`, stopping any previous one for the same key.
    pub fn add(&self, renewer: HeartbeatRenewer) {
        let key = (renewer.dataset().to_string(), renewer.service_id().to_string());
        let old = self.renewers.lock().insert(key, renewer.clone());
        if let Some(old) = old {
            old.signal_stop();
        }
        renewer.start();
    }

    /// Stop and forget the renewer for this key. Idempotent.
    pub async fn remove(&self, dataset: &str, service_id: &str) {
        let renewer = self
            .renewers
            .lock()
            .remove(&(dataset.to_string(), service_id.to_string()));
        if let Some(r) = renewer {
            r.stop().await;
        }
    }

    /// Stop every renewer and wait for each, bounded by `timeout` per renewer.
    pub async fn shutdown_all(&self, timeout: Duration) {
        let all: Vec<HeartbeatRenewer> = self.renewers.lock().drain().map(|(_, r)| r).collect();
        for r in all {
            if tokio::time::timeout(timeout, r.stop()).await.is_err() {
                tracing::warn!(
                    "heartbeat renewer for ({}, {}) did not stop within {timeout:?}; aborting",
                    r.dataset(),
                    r.service_id()
                );
            }
        }
    }

    /// Signal every renewer to stop without waiting (used from `Drop` paths).
    pub fn signal_stop_all(&self) {
        let all: Vec<HeartbeatRenewer> = self.renewers.lock().drain().map(|(_, r)| r).collect();
        for r in all {
            r.signal_stop();
        }
    }
}
