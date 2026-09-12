// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Test harness: in-process transport, fake registry, fake auth and a
//! manual clock. Public so the registry crate's tests can reuse it.
//!
//! [`InProcessTransport`] routes a peer `address` straight to the target
//! store's handler methods (the same methods the HTTP router calls), so
//! several nodes can be exercised in one process without real servers.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use a2x_common::AuthContext;
use ap_support::testing::TestResult;
use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::ClusterConfig;
use crate::envelope::{Key, SyncEnvelope, Version};
use crate::membership::MembershipRecord;
use crate::registry_view::{EvictionSink, LocalEntry, LocalRegistryView, PeerAuthVerifier};
use crate::state::ClusterState;
use crate::store::{Clock, ClusterStore};
use crate::transport::{
    DigestRow, EvictRequest, JoinRequest, JoinResponse, LeaveRequest, OkResponse, OpenRequest, OpenResponse,
    SetSyncResponse, Transport, TransportError, UpdatesResponse,
};

/// Deterministic service id from type prefix and name, like the registry.
pub fn generate_service_id(type_prefix: &str, name: &str) -> String {
    let h = hex::encode(Sha256::digest(name.as_bytes()));
    format!("{type_prefix}_{}", &h[..16])
}

/// Minimal registry surface the cluster store depends on.
type Dataset = IndexMap<String, (Value, Value)>;

#[derive(Default)]
pub struct FakeRegistry {
    data: Mutex<IndexMap<String, Dataset>>,
    auth_required: Mutex<HashSet<String>>,
}

impl FakeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty dataset.
    pub fn add_dataset(&self, dataset: &str) {
        self.data.lock().entry(dataset.to_string()).or_default();
    }

    /// Add (or replace) a generic service; returns its service id.
    pub fn add_generic(&self, dataset: &str, name: &str, description: &str, source: &str) -> String {
        let sid = generate_service_id("generic", name);
        let entry = json!({
            "service_id": sid, "type": "generic", "source": source,
            "service_data": {"name": name, "description": description, "inputSchema": {}, "url": null},
        });
        let wrapped = json!({
            "id": sid, "type": "generic", "name": name, "description": description, "metadata": {},
        });
        self.data
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .insert(sid.clone(), (entry, wrapped));
        sid
    }

    /// `add_generic` with description `"d"` and source `"api_config"`.
    pub fn add(&self, dataset: &str, name: &str) -> String {
        self.add_generic(dataset, name, "d", "api_config")
    }

    pub fn remove(&self, dataset: &str, sid: &str) {
        if let Some(ds) = self.data.lock().get_mut(dataset) {
            ds.shift_remove(sid);
        }
    }

    pub fn set_auth_required(&self, dataset: &str, required: bool) {
        let mut g = self.auth_required.lock();
        if required {
            g.insert(dataset.to_string());
        } else {
            g.remove(dataset);
        }
    }

    /// Service names in `dataset`.
    pub fn names(&self, dataset: &str) -> BTreeSet<String> {
        self.data
            .lock()
            .get(dataset)
            .map(|ds| {
                ds.values()
                    .filter_map(|(e, _)| e["service_data"]["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl LocalRegistryView for FakeRegistry {
    fn list_datasets(&self) -> Vec<String> {
        self.data.lock().keys().cloned().collect()
    }

    fn list_entries(&self, dataset: &str) -> Vec<LocalEntry> {
        self.data
            .lock()
            .get(dataset)
            .map(|ds| {
                ds.iter()
                    .map(|(sid, (e, w))| LocalEntry {
                        service_id: sid.clone(),
                        source: e["source"].as_str().unwrap_or("").to_string(),
                        entry: e.clone(),
                        wrapped: Some(w.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_auth_required(&self, dataset: &str) -> bool {
        self.auth_required.lock().contains(dataset)
    }
}

/// Token table verifier. `enabled == false` models a registry without auth.
#[derive(Clone, Debug, Default)]
pub struct FakeAuth {
    pub enabled: bool,
    pub tokens: HashMap<String, AuthContext>,
}

impl FakeAuth {
    /// No auth configured (open cluster).
    pub fn off() -> Self {
        Self::default()
    }

    /// Auth configured with the given `(token, context)` pairs.
    pub fn on(tokens: &[(&str, AuthContext)]) -> Self {
        FakeAuth {
            enabled: true,
            tokens: tokens.iter().map(|(t, c)| (t.to_string(), c.clone())).collect(),
        }
    }
}

impl PeerAuthVerifier for FakeAuth {
    fn auth_enabled(&self) -> bool {
        self.enabled
    }

    fn authenticate(&self, token: &str) -> Option<AuthContext> {
        self.tokens.get(token).cloned()
    }
}

/// Manually advanced monotonic clock for deterministic liveness tests.
#[derive(Clone, Default)]
pub struct FakeClock(Arc<Mutex<f64>>);

impl FakeClock {
    pub fn new(t: f64) -> Self {
        FakeClock(Arc::new(Mutex::new(t)))
    }

    pub fn now(&self) -> f64 {
        *self.0.lock()
    }

    pub fn advance(&self, dt: f64) {
        *self.0.lock() += dt;
    }

    /// The clock as the store expects it.
    pub fn clock(&self) -> Clock {
        let c = self.clone();
        Arc::new(move || c.now())
    }
}

/// One eviction event: `(origin_id, [(dataset, service_id)])`.
pub type EvictionEvent = (String, Vec<(String, String)>);

/// Records every eviction callback.
#[derive(Default)]
pub struct RecordingSink {
    pub events: Mutex<Vec<EvictionEvent>>,
}

impl EvictionSink for RecordingSink {
    fn on_evicted(&self, origin_id: &str, evicted: &[(String, String)]) {
        self.events.lock().push((origin_id.to_string(), evicted.to_vec()));
    }
}

/// Routes peer calls to the target store's handlers in-process.
#[derive(Default)]
pub struct InProcessTransport {
    stores: Mutex<HashMap<String, Weak<ClusterStore>>>,
    /// `(caller, target)` links that are cut (both directions via `cut`).
    dropped: Mutex<HashSet<(String, String)>>,
    pub n_merkle: AtomicUsize,
    pub n_digest: AtomicUsize,
    pub n_updates: AtomicUsize,
}

impl InProcessTransport {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn register(&self, address: &str, store: &Arc<ClusterStore>) {
        self.stores
            .lock()
            .insert(address.to_string(), Arc::downgrade(store));
    }

    /// Simulate a partition between two addresses (both directions).
    pub fn cut(&self, a: &str, b: &str) {
        let mut d = self.dropped.lock();
        d.insert((a.to_string(), b.to_string()));
        d.insert((b.to_string(), a.to_string()));
    }

    /// Heal every partition.
    pub fn heal(&self) {
        self.dropped.lock().clear();
    }

    fn resolve(&self, address: &str) -> Result<Arc<ClusterStore>, TransportError> {
        self.stores
            .lock()
            .get(address)
            .and_then(Weak::upgrade)
            .ok_or_else(|| TransportError::new(format!("unreachable: {address}")))
    }

    fn target(&self, caller: &str, address: &str) -> Result<Arc<ClusterStore>, TransportError> {
        if self
            .dropped
            .lock()
            .contains(&(caller.to_string(), address.to_string()))
        {
            return Err(TransportError::new(format!("partitioned: {caller} -> {address}")));
        }
        self.resolve(address)
    }

    fn membership_of(
        &self,
        address: &str,
    ) -> Result<Arc<crate::membership::MembershipStore>, TransportError> {
        self.resolve(address)?
            .membership()
            .ok_or_else(|| TransportError::new(format!("no membership plane at {address}")))
    }
}

fn opt(ns: &[String]) -> Option<&[String]> {
    if ns.is_empty() { None } else { Some(ns) }
}

#[async_trait]
impl Transport for InProcessTransport {
    // `open` is never partitioned: the caller's advertise is unknown here.
    async fn open(&self, address: &str, body: &OpenRequest) -> Result<OpenResponse, TransportError> {
        Ok(self.resolve(address)?.handle_open(body.clone()))
    }

    async fn merkle(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
    ) -> Result<BTreeMap<String, String>, TransportError> {
        self.n_merkle.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .target(from_node, address)?
            .serve_merkle(from_node, opt(namespaces), token))
    }

    async fn digest(
        &self,
        address: &str,
        from_node: &str,
        namespaces: &[String],
        token: Option<&str>,
        buckets: Option<&[u32]>,
    ) -> Result<Vec<DigestRow>, TransportError> {
        self.n_digest.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .target(from_node, address)?
            .serve_digest(from_node, opt(namespaces), token, buckets))
    }

    async fn pull(
        &self,
        address: &str,
        from_node: &str,
        keys: &[Key],
        token: Option<&str>,
    ) -> Result<Vec<SyncEnvelope>, TransportError> {
        Ok(self
            .target(from_node, address)?
            .serve_pull(from_node, keys, token))
    }

    async fn updates(
        &self,
        address: &str,
        from_node: &str,
        envelopes: &[SyncEnvelope],
        token: Option<&str>,
    ) -> Result<UpdatesResponse, TransportError> {
        self.n_updates.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .target(from_node, address)?
            .serve_updates(from_node, envelopes.to_vec(), token))
    }

    async fn keepalive(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<OkResponse, TransportError> {
        Ok(self
            .target(from_node, address)?
            .handle_keepalive(from_node, token))
    }

    async fn join(&self, address: &str, body: &JoinRequest) -> Result<JoinResponse, TransportError> {
        Ok(self.membership_of(address)?.handle_join(body.clone()).await)
    }

    async fn evict(&self, address: &str, body: &EvictRequest) -> Result<OkResponse, TransportError> {
        Ok(self.membership_of(address)?.handle_evicted(body.clone()))
    }

    async fn evict_self(&self, address: &str, body: &LeaveRequest) -> Result<OkResponse, TransportError> {
        Ok(self.membership_of(address)?.handle_evict_self(body.clone()).await)
    }

    async fn set_digest(
        &self,
        address: &str,
        from_node: &str,
        token: Option<&str>,
    ) -> Result<BTreeMap<String, Version>, TransportError> {
        let store = self.target(from_node, address)?;
        let m = store
            .membership()
            .ok_or_else(|| TransportError::new("no membership plane"))?;
        Ok(m.serve_set_digest(from_node, token))
    }

    async fn set_pull(
        &self,
        address: &str,
        from_node: &str,
        node_ids: &[String],
        token: Option<&str>,
    ) -> Result<Vec<Value>, TransportError> {
        let store = self.target(from_node, address)?;
        let m = store
            .membership()
            .ok_or_else(|| TransportError::new("no membership plane"))?;
        Ok(m.serve_set_pull(from_node, node_ids, token)
            .iter()
            .map(MembershipRecord::to_value)
            .collect())
    }

    async fn set_sync(
        &self,
        address: &str,
        from_node: &str,
        records: &[MembershipRecord],
        token: Option<&str>,
    ) -> Result<SetSyncResponse, TransportError> {
        let store = self.target(from_node, address)?;
        let m = store
            .membership()
            .ok_or_else(|| TransportError::new("no membership plane"))?;
        let recs: Vec<Value> = records.iter().map(MembershipRecord::to_value).collect();
        Ok(m.serve_set_sync(from_node, &recs, token))
    }
}

/// Builder for one in-process test node. The node name doubles as its
/// address on the [`InProcessTransport`].
pub struct TestNode {
    dir: std::path::PathBuf,
    name: String,
    registry: Arc<FakeRegistry>,
    transport: Arc<dyn Transport>,
    in_process: Option<Arc<InProcessTransport>>,
    auth: Option<Arc<dyn PeerAuthVerifier>>,
    config: Option<ClusterConfig>,
    clock: Option<Clock>,
    membership: bool,
    sink: Option<Arc<dyn EvictionSink>>,
}

impl TestNode {
    pub fn new(
        dir: &Path,
        name: &str,
        registry: Arc<FakeRegistry>,
        transport: Arc<InProcessTransport>,
    ) -> Self {
        TestNode {
            dir: dir.to_path_buf(),
            name: name.to_string(),
            registry,
            transport: transport.clone(),
            in_process: Some(transport),
            auth: None,
            config: None,
            clock: None,
            membership: true,
            sink: None,
        }
    }

    /// Use an arbitrary transport (not registered anywhere).
    pub fn with_transport(
        dir: &Path,
        name: &str,
        registry: Arc<FakeRegistry>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        TestNode {
            dir: dir.to_path_buf(),
            name: name.to_string(),
            registry,
            transport,
            in_process: None,
            auth: None,
            config: None,
            clock: None,
            membership: true,
            sink: None,
        }
    }

    pub fn auth(mut self, auth: FakeAuth) -> Self {
        self.auth = Some(Arc::new(auth));
        self
    }

    pub fn config(mut self, config: ClusterConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn clock(mut self, clock: &FakeClock) -> Self {
        self.clock = Some(clock.clock());
        self
    }

    pub fn membership(mut self, enabled: bool) -> Self {
        self.membership = enabled;
        self
    }

    pub fn sink(mut self, sink: Arc<dyn EvictionSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Path of this node's state file (`<dir>/<name>.json`).
    pub fn state_file(dir: &Path, name: &str) -> std::path::PathBuf {
        dir.join(format!("{name}.json"))
    }

    /// Create a fresh state file and build the store.
    pub fn build(self) -> TestResult<Arc<ClusterStore>> {
        let state = ClusterState::init_at(Some(&self.name), &Self::state_file(&self.dir, &self.name))?;
        Ok(self.build_from(state))
    }

    /// Build the store around an existing state (restart simulation).
    pub fn build_from(self, state: ClusterState) -> Arc<ClusterStore> {
        let mut b = ClusterStore::builder()
            .config(self.config.unwrap_or_default())
            .registry(self.registry)
            .transport(self.transport)
            .advertise(self.name.clone())
            .membership(self.membership);
        if let Some(a) = self.auth {
            b = b.auth(a);
        }
        if let Some(c) = self.clock {
            b = b.clock(c);
        }
        if let Some(s) = self.sink {
            b = b.eviction_sink(s);
        }
        let store = b.build(state);
        if let Some(t) = &self.in_process {
            t.register(&self.name, &store);
        }
        store
    }
}

/// Convenience: a node with a fresh empty registry and default settings.
pub fn build_store(
    dir: &Path,
    name: &str,
    transport: &Arc<InProcessTransport>,
) -> TestResult<(Arc<ClusterStore>, Arc<FakeRegistry>)> {
    let reg = Arc::new(FakeRegistry::new());
    let store = TestNode::new(dir, name, reg.clone(), transport.clone()).build()?;
    Ok((store, reg))
}

/// Drive anti-entropy: each node reconciles each of its peers (records and
/// membership deltas), several rounds, to a fixed point.
pub async fn converge(stores: &[&Arc<ClusterStore>], rounds: usize) {
    for _ in 0..rounds {
        for s in stores {
            for p in s.list_peers() {
                if s.reconcile(&p).await.is_err() {
                    continue;
                }
                if let Some(m) = s.membership() {
                    m.reconcile_with(&p).await;
                }
            }
        }
    }
}

/// Drive the membership control loop to a fixed point: reconcile
/// connections, then exchange record and membership deltas. Mirrors one
/// `AntiEntropySweeper` tick per node per round.
pub async fn settle(stores: &[&Arc<ClusterStore>], rounds: usize) {
    for _ in 0..rounds {
        for s in stores {
            if let Some(m) = s.membership() {
                m.reconcile_connections().await;
            }
        }
        converge(stores, 1).await;
    }
}

/// Service names a node can serve from `dataset`: local entries plus
/// replicated foreign rows.
pub fn visible(store: &ClusterStore, registry: &FakeRegistry, dataset: &str) -> BTreeSet<String> {
    let mut out = registry.names(dataset);
    for r in store.foreign_rows(dataset) {
        if let Some(n) = r.wrapped["name"].as_str() {
            out.insert(n.to_string());
        }
    }
    out
}

/// Names of the foreign rows a node holds for `dataset`.
pub fn foreign_names(store: &ClusterStore, dataset: &str) -> BTreeSet<String> {
    store
        .foreign_rows(dataset)
        .into_iter()
        .filter_map(|r| r.wrapped["name"].as_str().map(str::to_string))
        .collect()
}

/// Node ids of a store's sessions.
pub fn peer_ids(store: &ClusterStore) -> BTreeSet<String> {
    store.list_peers().into_iter().map(|p| p.node_id).collect()
}

/// Wire-shaped foreign envelope for a generic service.
pub fn foreign_env(origin: &str, dataset: &str, name: &str, sid: &str, ver: i64) -> SyncEnvelope {
    SyncEnvelope {
        dataset: dataset.to_string(),
        service_id: sid.to_string(),
        origin_id: origin.to_string(),
        version: Version::new(ver, origin),
        tombstone: false,
        payload: Some(json!({
            "entry": {"service_id": sid, "type": "generic", "source": "api_config",
                      "service_data": {"name": name, "description": "d", "inputSchema": {}, "url": null}},
            "wrapped": {"id": sid, "type": "generic", "name": name, "description": "d", "metadata": {}},
        })),
    }
}
