// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Background tasks for the cluster module.
//!
//! [`AntiEntropySweeper`] periodically reconciles with each peer (healing
//! pushes dropped while a link was flaky) and GCs expired tombstones.
//! [`KeepaliveMonitor`] sends direct-link keepalives and drops peers past
//! their HOLD timer. Each tick is guarded so one failure never kills the
//! loop. `tick()` is public so tests drive it synchronously.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::store::ClusterStore;

fn period_duration(period: f64) -> Duration {
    if period.is_finite() && period > 0.0 {
        Duration::from_secs_f64(period)
    } else {
        Duration::from_secs(1)
    }
}

/// Periodic reconcile plus tombstone GC.
pub struct AntiEntropySweeper {
    store: Arc<ClusterStore>,
    period: f64,
}

impl AntiEntropySweeper {
    pub fn new(store: Arc<ClusterStore>, period: f64) -> Self {
        Self { store, period }
    }

    /// One pass: reconcile membership (connect/disconnect to match the
    /// roster), reconcile each peer's records and membership deltas
    /// (best-effort), GC tombstones, and prune suppression entries.
    pub async fn tick(&self) {
        let membership = self.store.membership();
        if let Some(m) = &membership {
            m.reconcile_connections().await;
        }
        for peer in self.store.list_peers() {
            if let Err(err) = self.store.reconcile(&peer).await {
                tracing::debug!(
                    "cluster: anti-entropy reconcile with {} failed: {err}",
                    peer.node_id
                );
                continue;
            }
            if let Some(m) = &membership {
                m.reconcile_with(&peer).await;
            }
        }
        self.store.gc_tombstones(None);
        self.store.prune_suppression(None);
        if let Some(m) = &membership {
            m.gc_membership(None);
        }
    }

    /// Run `tick` every `period` seconds until `cancel` fires.
    pub fn spawn(self, cancel: CancellationToken) -> JoinHandle<()> {
        let period = period_duration(self.period);
        tracing::info!("cluster: anti-entropy sweeper started (period={}s)", self.period);
        tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                self.tick().await;
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(period) => {}
                }
            }
        })
    }
}

/// Sends direct-link keepalives and drops peers past their HOLD timer.
pub struct KeepaliveMonitor {
    store: Arc<ClusterStore>,
    period: f64,
}

impl KeepaliveMonitor {
    pub fn new(store: Arc<ClusterStore>, period: f64) -> Self {
        Self { store, period }
    }

    pub async fn tick(&self) {
        self.store.emit_keepalive().await;
        self.store.check_hold(None);
    }

    /// Run `tick` every `period` seconds until `cancel` fires.
    pub fn spawn(self, cancel: CancellationToken) -> JoinHandle<()> {
        let period = period_duration(self.period);
        tracing::info!("cluster: keepalive monitor started (period={}s)", self.period);
        tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                self.tick().await;
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(period) => {}
                }
            }
        })
    }
}
