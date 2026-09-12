// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Scale-path behaviour (`test_scale.py`): concurrent fan-out and Merkle
//! anti-entropy transferring rows only on change.

use ap_support::testing::TestResult;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use a2x_cluster::membership::MembershipRecord;
use a2x_cluster::state::ClusterState;
use a2x_cluster::testing::*;
use a2x_cluster::transport::{
    DigestRow, EvictRequest, JoinRequest, JoinResponse, LeaveRequest, OkResponse, OpenRequest, OpenResponse,
    SetSyncResponse, Transport, TransportError, UpdatesResponse,
};
use a2x_cluster::{ClusterConfig, ClusterStore, Key, MutationOp, SyncEnvelope, Version};
use async_trait::async_trait;
use serde_json::Value;

/// Every `updates` call sleeps, simulating slow peers.
struct SleepTransport {
    delay: Duration,
    hits: AtomicUsize,
}

#[async_trait]
impl Transport for SleepTransport {
    async fn open(&self, address: &str, _b: &OpenRequest) -> Result<OpenResponse, TransportError> {
        Ok(OpenResponse {
            node_id: address.to_string(),
            accepted: vec!["ds".into()],
            ephemeral: vec![],
            session_token: None,
        })
    }
    async fn merkle(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&str>,
    ) -> Result<BTreeMap<String, String>, TransportError> {
        Ok(BTreeMap::new())
    }
    async fn digest(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&str>,
        _: Option<&[u32]>,
    ) -> Result<Vec<DigestRow>, TransportError> {
        Ok(vec![])
    }
    async fn pull(
        &self,
        _: &str,
        _: &str,
        _: &[Key],
        _: Option<&str>,
    ) -> Result<Vec<SyncEnvelope>, TransportError> {
        Ok(vec![])
    }
    async fn updates(
        &self,
        _: &str,
        _: &str,
        envelopes: &[SyncEnvelope],
        _: Option<&str>,
    ) -> Result<UpdatesResponse, TransportError> {
        tokio::time::sleep(self.delay).await;
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(UpdatesResponse {
            accepted: envelopes.len(),
            received: envelopes.len(),
            rejected: 0,
        })
    }
    async fn keepalive(&self, _: &str, _: &str, _: Option<&str>) -> Result<OkResponse, TransportError> {
        Ok(OkResponse { ok: true })
    }
    async fn join(&self, _: &str, _: &JoinRequest) -> Result<JoinResponse, TransportError> {
        Err(TransportError::new("n/a"))
    }
    async fn evict(&self, _: &str, _: &EvictRequest) -> Result<OkResponse, TransportError> {
        Err(TransportError::new("n/a"))
    }
    async fn evict_self(&self, _: &str, _: &LeaveRequest) -> Result<OkResponse, TransportError> {
        Err(TransportError::new("n/a"))
    }
    async fn set_digest(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<BTreeMap<String, Version>, TransportError> {
        Err(TransportError::new("n/a"))
    }
    async fn set_pull(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&str>,
    ) -> Result<Vec<Value>, TransportError> {
        Err(TransportError::new("n/a"))
    }
    async fn set_sync(
        &self,
        _: &str,
        _: &str,
        _: &[MembershipRecord],
        _: Option<&str>,
    ) -> Result<SetSyncResponse, TransportError> {
        Err(TransportError::new("n/a"))
    }
}

#[tokio::test]
async fn broadcast_runs_concurrently() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (delay, k) = (Duration::from_millis(200), 8usize);
    let tr = Arc::new(SleepTransport {
        delay,
        hits: AtomicUsize::new(0),
    });
    let store = ClusterStore::builder()
        .config(ClusterConfig {
            broadcast_workers: 32,
            ..ClusterConfig::default()
        })
        .transport(tr.clone())
        .advertise("A")
        .build(ClusterState::init_at(Some("A"), &dir.path().join("A.json"))?);
    for i in 0..k {
        store.connect_peer(&format!("P{i}"), None, None).await?;
    }
    let env = SyncEnvelope {
        dataset: "ds".into(),
        service_id: "x".into(),
        origin_id: "A".into(),
        version: Version::new(1, "A"),
        tombstone: false,
        payload: Some(Value::Object(Default::default())),
    };
    let t0 = Instant::now();
    store.broadcast(&env).await;
    let elapsed = t0.elapsed();
    assert_eq!(tr.hits.load(Ordering::SeqCst), k);
    assert!(elapsed < delay * (k as u32) / 2, "elapsed {elapsed:?}");
    store.close();
    store.close();
    Ok(())
}

#[tokio::test]
async fn merkle_skips_row_transfer_when_in_sync() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "a-svc");
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;

    let before = (
        t.n_merkle.load(Ordering::SeqCst),
        t.n_digest.load(Ordering::SeqCst),
    );
    a.reconcile(&a.list_peers()[0]).await?;
    assert_eq!(t.n_merkle.load(Ordering::SeqCst), before.0 + 1);
    assert_eq!(t.n_digest.load(Ordering::SeqCst), before.1);
    Ok(())
}

#[tokio::test]
async fn merkle_transfers_only_on_change() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    rb.add("ds", "seed");
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;

    t.cut("B", "A");
    let sid = rb.add("ds", "b-new");
    b.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    t.heal();

    let d0 = t.n_digest.load(Ordering::SeqCst);
    let res = a.reconcile(&a.list_peers()[0]).await?;
    assert_eq!(t.n_digest.load(Ordering::SeqCst), d0 + 1);
    assert_eq!(res.pulled, 1);
    assert!(foreign_names(&a, "ds").contains("b-new"));
    Ok(())
}

#[tokio::test]
async fn digest_bucket_filter_matches_merkle_partition() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    for i in 0..20 {
        rb.add("ds", &format!("svc-{i}"));
    }
    b.handle_open(OpenRequest {
        node_id: "A".into(),
        ..Default::default()
    });
    let all = b.serve_digest("A", None, None, None);
    assert_eq!(all.len(), 20);
    let hashes = b.serve_merkle("A", None, None);
    let buckets: BTreeSet<u32> = hashes
        .keys()
        .map(|k| -> TestResult<_> { Ok(k.parse()?) })
        .collect::<TestResult<_>>()?;
    let first: Vec<u32> = buckets.iter().take(3).copied().collect();
    let subset = b.serve_digest("A", None, None, Some(&first));
    assert!(!subset.is_empty() && subset.len() < 20);
    for row in &subset {
        let key = (row.0.clone(), row.1.clone(), row.2.clone());
        assert!(first.contains(&a2x_cluster::merkle::bucket_of(&key, 256)));
    }
    Ok(())
}
