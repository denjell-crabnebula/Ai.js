// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! [`ClusterStore`]: the cluster module's single stateful object.
//!
//! Holds node identity plus persisted local version/tombstone state, the
//! in-memory foreign-record overlay, and peer sessions. It exposes two kinds
//! of methods:
//!
//! - handlers (`handle_open`, `serve_digest`, `serve_pull`, `serve_updates`,
//!   `handle_keepalive`): invoked by the local router when a peer calls us.
//! - orchestration (`connect_peer`, `reconcile`, `disconnect_peer`,
//!   `emit_keepalive`, `check_hold`): invoked locally; reach peers through
//!   the injected [`Transport`].
//!
//! Replication model: origin-only writes, so the global identity of a record
//! is `(dataset, origin_id, service_id)` and there are no write-write
//! conflicts. LWW on `(updated_at_ms, node_id)` just dedups and orders
//! versions of the same record. Foreign records are read-only and memory-only.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::auth_handshake::authorize_namespaces;
use crate::config::ClusterConfig;
use crate::envelope::{Key, SyncEnvelope, Version, version_newer};
use crate::errors::{ClusterError, Result};
use crate::membership::{MembershipStore, Roster};
use crate::merkle;
use crate::peer::{Peer, PeerSummary};
use crate::registry_view::{AllowAll, EvictionSink, LocalRegistryView, PeerAuthVerifier};
use crate::state::{ClusterState, Tombstone, make_key, split_key, state_path};
use crate::sweepers::{AntiEntropySweeper, KeepaliveMonitor};
use crate::transport::{
    DigestRow, HttpTransport, OkResponse, OpenRequest, OpenResponse, Transport, TransportError,
    UpdatesResponse,
};

/// Monotonic clock in seconds, injectable for tests.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// A best-effort fan-out task.
pub type FanoutTask<'a> = Pin<Box<dyn Future<Output = std::result::Result<(), TransportError>> + Send + 'a>>;

/// Wall-clock milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Which local CRUD happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOp {
    Register,
    Update,
    Deregister,
}

impl MutationOp {
    /// Parse the Python hook's `op` string (`register|update|deregister`).
    pub fn parse(op: &str) -> Option<Self> {
        match op {
            "register" => Some(MutationOp::Register),
            "update" => Some(MutationOp::Update),
            "deregister" => Some(MutationOp::Deregister),
            _ => None,
        }
    }
}

/// Outcome of one [`ClusterStore::reconcile`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileResult {
    pub pulled: usize,
    pub pushed: usize,
}

/// A replicated live record for the list endpoint: `entry` feeds the filter
/// pipeline, `wrapped` is the output row with a namespaced id.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ForeignRow {
    pub entry: Value,
    pub wrapped: Value,
}

/// `GET /api/cluster/state` body.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateSummary {
    pub node_id: String,
    pub advertise: String,
    pub peers: Vec<PeerSummary>,
    pub foreign_records: usize,
    pub foreign_by_namespace: BTreeMap<String, usize>,
    pub local_records: usize,
    pub tombstones: usize,
}

struct Inner {
    state: ClusterState,
    /// (dataset, origin_id, sid) -> envelope (live or tombstone)
    foreign: IndexMap<Key, SyncEnvelope>,
    /// peer node_id -> session
    sessions: IndexMap<String, Peer>,
    /// origin -> monotonic deadline until which its records are suppressed
    evicted_until: HashMap<String, f64>,
}

/// Owns all cluster runtime state for this registry instance.
pub struct ClusterStore {
    inner: Mutex<Inner>,
    config: ClusterConfig,
    registry: Option<Arc<dyn LocalRegistryView>>,
    transport: Arc<dyn Transport>,
    advertise: String,
    auth: Arc<dyn PeerAuthVerifier>,
    clock: Clock,
    eviction_sink: Option<Arc<dyn EvictionSink>>,
    fanout_limit: usize,
    membership_roster: OnceLock<Arc<Mutex<Roster>>>,
}

/// Builder for [`ClusterStore`]. Every field has a default so a bare
/// `ClusterStore::builder().build(state)` yields a standalone node.
pub struct ClusterStoreBuilder {
    config: Option<ClusterConfig>,
    registry: Option<Arc<dyn LocalRegistryView>>,
    transport: Option<Arc<dyn Transport>>,
    advertise: String,
    auth: Option<Arc<dyn PeerAuthVerifier>>,
    clock: Option<Clock>,
    eviction_sink: Option<Arc<dyn EvictionSink>>,
    membership: bool,
}

impl Default for ClusterStoreBuilder {
    fn default() -> Self {
        Self {
            config: None,
            registry: None,
            transport: None,
            advertise: String::new(),
            auth: None,
            clock: None,
            eviction_sink: None,
            membership: true,
        }
    }
}

impl ClusterStoreBuilder {
    pub fn config(mut self, config: ClusterConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn registry(mut self, registry: Arc<dyn LocalRegistryView>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// This node's peer-reachable base URL (`A2X_REGISTRY_CLUSTER_ADVERTISE`).
    pub fn advertise(mut self, advertise: impl Into<String>) -> Self {
        self.advertise = advertise.into();
        self
    }

    pub fn auth(mut self, auth: Arc<dyn PeerAuthVerifier>) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = Some(clock);
        self
    }

    pub fn eviction_sink(mut self, sink: Arc<dyn EvictionSink>) -> Self {
        self.eviction_sink = Some(sink);
        self
    }

    /// Attach the declarative membership control plane (default `true`).
    pub fn membership(mut self, enabled: bool) -> Self {
        self.membership = enabled;
        self
    }

    /// Build the store around an existing state.
    pub fn build(self, state: ClusterState) -> Arc<ClusterStore> {
        let config = self.config.unwrap_or_default();
        let transport = self
            .transport
            .unwrap_or_else(|| Arc::new(HttpTransport::new(config.http_timeout)));
        let clock: Clock = self
            .clock
            .unwrap_or_else(|| Arc::new(a2x_common::lease::monotonic_now));
        let store = Arc::new(ClusterStore {
            inner: Mutex::new(Inner {
                state,
                foreign: IndexMap::new(),
                sessions: IndexMap::new(),
                evicted_until: HashMap::new(),
            }),
            fanout_limit: config.broadcast_workers.max(4),
            config,
            registry: self.registry,
            transport,
            advertise: self.advertise,
            auth: self.auth.unwrap_or_else(|| Arc::new(AllowAll)),
            clock,
            eviction_sink: self.eviction_sink,
            membership_roster: OnceLock::new(),
        });
        if self.membership {
            MembershipStore::attach(&store);
        }
        store
    }

    /// Build from the persisted `cluster_state.json` at the default
    /// location, or `None` when the file is absent or unreadable. A corrupt
    /// file is logged and treated like an absent one so boot never fails.
    pub fn load_or_none(self) -> Option<Arc<ClusterStore>> {
        self.load_or_none_at(&state_path())
    }

    /// Like [`Self::load_or_none`] with an explicit file path.
    pub fn load_or_none_at(self, path: &Path) -> Option<Arc<ClusterStore>> {
        match ClusterState::load_from(path) {
            Ok(Some(state)) => Some(self.build(state)),
            Ok(None) => None,
            Err(err) => {
                tracing::error!("cluster: failed to load cluster_state.json ({err}); staying standalone");
                None
            }
        }
    }
}

/// Background tasks spawned by [`ClusterStore::start`].
pub struct ClusterHandle {
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl ClusterHandle {
    /// Cancel the sweepers and wait for them to exit.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        for t in self.tasks {
            let _ = t.await;
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl ClusterStore {
    pub fn builder() -> ClusterStoreBuilder {
        ClusterStoreBuilder::default()
    }

    // ── identity / config ────────────────────────────────────────────────

    pub fn node_id(&self) -> String {
        self.inner.lock().state.node_id.clone()
    }

    pub fn config(&self) -> &ClusterConfig {
        &self.config
    }

    /// This node's peer-reachable base URL (empty until configured).
    pub fn advertise(&self) -> &str {
        &self.advertise
    }

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    pub fn registry(&self) -> Option<&Arc<dyn LocalRegistryView>> {
        self.registry.as_ref()
    }

    /// The handshake verifier; `auth_enabled() == false` means open cluster.
    pub fn auth(&self) -> &Arc<dyn PeerAuthVerifier> {
        &self.auth
    }

    /// The membership control plane, if attached.
    pub fn membership(self: &Arc<Self>) -> Option<Arc<MembershipStore>> {
        let roster = self.membership_roster.get()?.clone();
        Some(Arc::new(MembershipStore::with_roster(self.clone(), roster)))
    }

    /// The shared membership roster, creating it with `init` on first use.
    pub(crate) fn membership_roster_or_init(
        &self,
        init: impl FnOnce() -> Arc<Mutex<Roster>>,
    ) -> Arc<Mutex<Roster>> {
        self.membership_roster.get_or_init(init).clone()
    }

    /// Current monotonic clock reading (seconds).
    pub fn clock_now(&self) -> f64 {
        (self.clock)()
    }

    /// Snapshot of the persisted state.
    pub fn state_snapshot(&self) -> ClusterState {
        self.inner.lock().state.clone()
    }

    /// Public wrapper over the session-auth check, for the membership
    /// control plane's RPC handlers.
    pub fn authed(&self, from_node: &str, token: Option<&str>) -> bool {
        self.is_authed(from_node, token)
    }

    // ── versioning (monotonic, survives clock step-back) ─────────────────

    fn next_ts_locked(state: &mut ClusterState) -> i64 {
        let ts = now_ms().max(state.version_clock + 1);
        state.version_clock = ts;
        ts
    }

    /// A fresh LWW version `(updated_at_ms, node_id)` from the shared
    /// monotonic clock.
    pub fn next_version(&self) -> Version {
        let mut g = self.inner.lock();
        let ts = Self::next_ts_locked(&mut g.state);
        Version::new(ts, g.state.node_id.clone())
    }

    /// A fresh version guaranteed strictly newer than `floor_ms` under LWW,
    /// so a tombstone for a record minted by another node always wins.
    pub fn next_version_after(&self, floor_ms: i64) -> Version {
        let mut g = self.inner.lock();
        let ts = now_ms().max(g.state.version_clock + 1).max(floor_ms + 1);
        g.state.version_clock = ts;
        Version::new(ts, g.state.node_id.clone())
    }

    /// Advance the version clock to at least `ms`.
    pub fn observe_version(&self, ms: i64) {
        let mut g = self.inner.lock();
        if ms > g.state.version_clock {
            g.state.version_clock = ms;
        }
    }

    /// Persist cluster state under the store lock.
    pub fn save_state(&self) -> Result<()> {
        self.inner.lock().state.save()
    }

    /// Update the membership fields of the persisted state and save.
    pub(crate) fn persist_membership(
        &self,
        cluster_id: Option<String>,
        last_roster: Vec<Value>,
        my_membership_version: Option<Version>,
    ) -> Result<()> {
        let mut g = self.inner.lock();
        g.state.cluster_id = cluster_id;
        g.state.last_roster = last_roster;
        g.state.my_membership_version = my_membership_version;
        g.state.save()
    }

    fn save_logged(state: &ClusterState) {
        if let Err(err) = state.save() {
            tracing::warn!("cluster: failed to persist cluster state: {err}");
        }
    }

    /// Assign a version to every local-origin record that lacks one (records
    /// that existed before cluster init). Persists once if anything changed.
    fn ensure_local_versions(&self) {
        let Some(reg) = &self.registry else { return };
        let mut present: Vec<(String, String)> = Vec::new();
        for ds in reg.list_datasets() {
            for e in reg.list_entries(&ds) {
                if !e.is_ephemeral() {
                    present.push((ds.clone(), e.service_id));
                }
            }
        }
        let mut g = self.inner.lock();
        let mut changed = false;
        for (ds, sid) in present {
            let k = make_key(&ds, &sid);
            if !g.state.local_versions.contains_key(&k) && !g.state.tombstones.contains_key(&k) {
                let ts = Self::next_ts_locked(&mut g.state);
                let node = g.state.node_id.clone();
                g.state.local_versions.insert(k, Version::new(ts, node));
                changed = true;
            }
        }
        if changed {
            Self::save_logged(&g.state);
        }
    }

    // ── index / envelope helpers ─────────────────────────────────────────

    /// Versions of all local-origin live records plus local tombstones,
    /// scoped to `namespaces` (`None` or empty = all local datasets).
    fn local_index(&self, namespaces: Option<&[String]>) -> HashMap<Key, Version> {
        self.ensure_local_versions();
        let mut idx = HashMap::new();
        let Some(reg) = &self.registry else { return idx };
        let scope: Option<HashSet<&String>> = match namespaces {
            Some(ns) if !ns.is_empty() => Some(ns.iter().collect()),
            _ => None,
        };
        let datasets: Vec<String> = reg
            .list_datasets()
            .into_iter()
            .filter(|d| scope.as_ref().map(|s| s.contains(d)).unwrap_or(true))
            .collect();
        let entries: Vec<(String, Vec<String>)> = datasets
            .into_iter()
            .map(|ds| {
                let sids = reg
                    .list_entries(&ds)
                    .into_iter()
                    .filter(|e| !e.is_ephemeral())
                    .map(|e| e.service_id)
                    .collect();
                (ds, sids)
            })
            .collect();
        let g = self.inner.lock();
        let me = g.state.node_id.clone();
        for (ds, sids) in entries {
            for sid in sids {
                if let Some(v) = g.state.local_versions.get(&make_key(&ds, &sid)) {
                    idx.insert((ds.clone(), me.clone(), sid), v.clone());
                }
            }
        }
        for (k, t) in &g.state.tombstones {
            let (ds, sid) = split_key(k);
            if scope.as_ref().map(|s| !s.contains(&ds)).unwrap_or(false) {
                continue;
            }
            idx.insert((ds, me.clone(), sid), t.version.clone());
        }
        idx
    }

    /// Local plus foreign record versions, scoped to `namespaces`.
    fn full_index(&self, namespaces: Option<&[String]>) -> HashMap<Key, Version> {
        let mut idx = self.local_index(namespaces);
        let scope: Option<HashSet<&String>> = match namespaces {
            Some(ns) if !ns.is_empty() => Some(ns.iter().collect()),
            _ => None,
        };
        let g = self.inner.lock();
        for (key, env) in &g.foreign {
            if scope.as_ref().map(|s| !s.contains(&key.0)).unwrap_or(false) {
                continue;
            }
            idx.insert(key.clone(), env.version.clone());
        }
        idx
    }

    fn build_local_envelope(&self, dataset: &str, sid: &str) -> Option<SyncEnvelope> {
        let (me, version) = {
            let g = self.inner.lock();
            let k = make_key(dataset, sid);
            if let Some(tomb) = g.state.tombstones.get(&k) {
                return Some(SyncEnvelope {
                    dataset: dataset.to_string(),
                    service_id: sid.to_string(),
                    origin_id: g.state.node_id.clone(),
                    version: tomb.version.clone(),
                    tombstone: true,
                    payload: None,
                });
            }
            (g.state.node_id.clone(), g.state.local_versions.get(&k).cloned()?)
        };
        let reg = self.registry.as_ref()?;
        let entry = reg.get_entry(dataset, sid)?;
        Some(SyncEnvelope {
            dataset: dataset.to_string(),
            service_id: sid.to_string(),
            origin_id: me,
            version,
            tombstone: false,
            payload: Some(json!({"entry": entry.entry, "wrapped": entry.wrapped})),
        })
    }

    fn build_envelope_for_key(&self, key: &Key) -> Option<SyncEnvelope> {
        let me = self.node_id();
        if key.1 == me {
            return self.build_local_envelope(&key.0, &key.2);
        }
        self.inner.lock().foreign.get(key).cloned()
    }

    // ── inbound apply (LWW dedup; no relay) ──────────────────────────────

    /// Accept `env` into the foreign overlay iff strictly newer than what we
    /// have. Returns `true` if stored. Self-origin envelopes are always
    /// ignored: our own state (including tombstones) is authoritative.
    pub fn apply_inbound(&self, env: SyncEnvelope) -> bool {
        let now = self.clock_now();
        let mut g = self.inner.lock();
        if env.origin_id == g.state.node_id {
            return false;
        }
        if let Some(until) = g.evicted_until.get(&env.origin_id) {
            if now < *until {
                return false;
            }
        }
        let key = env.key();
        let cur = g.foreign.get(&key).map(|e| e.version.clone());
        if !version_newer(&env.version, cur.as_ref()) {
            return false;
        }
        g.foreign.insert(key, env);
        true
    }

    // ── handlers (peer -> us) ────────────────────────────────────────────

    /// Receive an OPEN: authorize per namespace, record the session, return
    /// our node id plus the accepted namespaces. The candidate set is the
    /// union of what the caller offered and our own datasets.
    pub fn handle_open(&self, req: OpenRequest) -> OpenResponse {
        let mut candidate: BTreeSet<String> = req.namespaces.iter().cloned().collect();
        if let Some(reg) = &self.registry {
            candidate.extend(reg.list_datasets());
        }
        let candidate: Vec<String> = candidate.into_iter().collect();
        let auth_on = self.auth.auth_enabled();
        let out = authorize_namespaces(
            self.registry.as_deref(),
            self.auth.as_ref(),
            &candidate,
            req.token.as_deref(),
        );
        let session_token = if auth_on {
            Some(hex::encode(rand::random::<[u8; 16]>()))
        } else {
            None
        };
        let now = self.clock_now();
        let me = {
            let mut g = self.inner.lock();
            let peer = Peer {
                node_id: req.node_id.clone(),
                address: req.address.clone(),
                namespaces: out.accepted.iter().cloned().collect(),
                last_seen: now,
                token: session_token.clone(),
            };
            g.sessions.insert(req.node_id.clone(), peer);
            g.evicted_until.remove(&req.node_id);
            g.state.node_id.clone()
        };
        tracing::info!(
            "cluster: session opened with {} (ns={:?})",
            req.node_id,
            out.accepted
        );
        OpenResponse {
            node_id: me,
            accepted: out.accepted,
            ephemeral: out.ephemeral,
            session_token,
        }
    }

    /// Refresh the direct-link HOLD timer for a peer we just heard from.
    fn touch_peer(&self, node_id: &str) {
        let now = self.clock_now();
        if let Some(p) = self.inner.lock().sessions.get_mut(node_id) {
            p.last_seen = now;
        }
    }

    fn public_namespaces(&self) -> BTreeSet<String> {
        let Some(reg) = &self.registry else {
            return BTreeSet::new();
        };
        let auth_on = self.auth.auth_enabled();
        reg.list_datasets()
            .into_iter()
            .filter(|ds| !auth_on || !reg.is_auth_required(ds))
            .collect()
    }

    /// Is the `from_node` claim authenticated for this call? Open cluster:
    /// always. Auth on: a session must exist and `token` must equal its secret.
    fn is_authed(&self, from_node: &str, token: Option<&str>) -> bool {
        if !self.auth.auth_enabled() {
            return true;
        }
        let g = self.inner.lock();
        match g.sessions.get(from_node) {
            Some(sess) => match (&sess.token, token) {
                (Some(t), Some(given)) => t == given,
                _ => false,
            },
            None => false,
        }
    }

    fn allowed_for(
        &self,
        from_node: &str,
        requested: Option<&[String]>,
        token: Option<&str>,
    ) -> BTreeSet<String> {
        let authed = self.is_authed(from_node, token);
        let sess_ns = self
            .inner
            .lock()
            .sessions
            .get(from_node)
            .map(|s| s.namespaces.clone());
        let mut base = match (authed, sess_ns) {
            (true, Some(ns)) => ns,
            _ => self.public_namespaces(),
        };
        if let Some(req) = requested {
            if !req.is_empty() {
                let req: BTreeSet<&String> = req.iter().collect();
                base.retain(|ns| req.contains(ns));
            }
        }
        base
    }

    /// `[dataset, origin_id, service_id, version]` rows visible to
    /// `from_node`. With `buckets`, only rows in those Merkle buckets.
    pub fn serve_digest(
        &self,
        from_node: &str,
        namespaces: Option<&[String]>,
        token: Option<&str>,
        buckets: Option<&[u32]>,
    ) -> Vec<DigestRow> {
        self.touch_peer(from_node);
        let allowed = self.allowed_for(from_node, namespaces, token);
        let scope: Vec<String> = allowed.iter().cloned().collect();
        let idx = self.full_index(Some(&scope));
        let bset: Option<BTreeSet<u32>> = buckets.map(|b| b.iter().copied().collect());
        let n = self.config.merkle_buckets;
        let mut rows: Vec<DigestRow> = idx
            .into_iter()
            .filter(|(k, _)| allowed.contains(&k.0))
            .filter(|(k, _)| {
                bset.as_ref()
                    .map(|b| b.contains(&merkle::bucket_of(k, n)))
                    .unwrap_or(true)
            })
            .map(|(k, v)| (k.0, k.1, k.2, v))
            .collect();
        rows.sort();
        rows
    }

    /// `{bucket_index: hash}` for the records visible to `from_node`.
    pub fn serve_merkle(
        &self,
        from_node: &str,
        namespaces: Option<&[String]>,
        token: Option<&str>,
    ) -> BTreeMap<String, String> {
        self.touch_peer(from_node);
        let allowed = self.allowed_for(from_node, namespaces, token);
        let scope: Vec<String> = allowed.iter().cloned().collect();
        let idx: HashMap<Key, Version> = self
            .full_index(Some(&scope))
            .into_iter()
            .filter(|(k, _)| allowed.contains(&k.0))
            .collect();
        merkle::bucket_hashes(&idx, self.config.merkle_buckets)
    }

    /// Full envelopes for the requested keys (session scoped).
    pub fn serve_pull(&self, from_node: &str, keys: &[Key], token: Option<&str>) -> Vec<SyncEnvelope> {
        self.touch_peer(from_node);
        let allowed = self.allowed_for(from_node, None, token);
        keys.iter()
            .filter(|k| allowed.contains(&k.0))
            .filter_map(|k| self.build_envelope_for_key(k))
            .collect()
    }

    /// Inbound authorization for a record's namespace. Mirrors the handshake
    /// gate so a peer cannot bypass it by posting straight to `/updates`.
    fn may_accept(&self, from_node: &str, dataset: &str, token: Option<&str>) -> bool {
        if !self.auth.auth_enabled() {
            return true;
        }
        if self.is_authed(from_node, token) {
            let g = self.inner.lock();
            if let Some(sess) = g.sessions.get(from_node) {
                if sess.namespaces.contains(dataset) {
                    return true;
                }
            }
        }
        if let Some(reg) = &self.registry {
            if reg.list_datasets().iter().any(|d| d == dataset) {
                return !reg.is_auth_required(dataset);
            }
        }
        false
    }

    /// Apply a batch of inbound envelopes (LWW dedup, namespace gated). No
    /// relay: each node receives a record directly from its origin.
    pub fn serve_updates(
        &self,
        from_node: &str,
        envelopes: Vec<SyncEnvelope>,
        token: Option<&str>,
    ) -> UpdatesResponse {
        self.touch_peer(from_node);
        let received = envelopes.len();
        let mut accepted = 0;
        let mut rejected = 0;
        for env in envelopes {
            if !self.may_accept(from_node, &env.dataset, token) {
                rejected += 1;
                continue;
            }
            if self.apply_inbound(env) {
                accepted += 1;
            }
        }
        UpdatesResponse {
            accepted,
            received,
            rejected,
        }
    }

    // ── outbound replication ─────────────────────────────────────────────

    /// Run best-effort peer calls concurrently (bounded by
    /// `broadcast_workers`) and wait for all of them. Errors are swallowed;
    /// periodic anti-entropy heals anything dropped.
    pub async fn fan_out(&self, tasks: Vec<FanoutTask<'_>>) {
        if tasks.is_empty() {
            return;
        }
        stream::iter(tasks)
            .for_each_concurrent(self.fanout_limit, |t| async move {
                if let Err(err) = t.await {
                    tracing::debug!("cluster: fan-out task failed: {err}");
                }
            })
            .await;
    }

    /// Release pooled transport connections. Safe to call more than once.
    pub fn close(&self) {
        self.transport.close();
    }

    /// Send `env` concurrently to every session that syncs its dataset.
    pub async fn broadcast(&self, env: &SyncEnvelope) {
        let me = self.node_id();
        let peers: Vec<Peer> = self
            .inner
            .lock()
            .sessions
            .values()
            .filter(|p| p.namespaces.contains(&env.dataset))
            .cloned()
            .collect();
        let payload = vec![env.clone()];
        let tasks: Vec<FanoutTask<'_>> = peers
            .into_iter()
            .map(|p| {
                let me = me.clone();
                let payload = &payload;
                let transport = self.transport.clone();
                Box::pin(async move {
                    transport
                        .updates(&p.address, &me, payload, p.token.as_deref())
                        .await
                        .map(|_| ())
                }) as FanoutTask<'_>
            })
            .collect();
        self.fan_out(tasks).await;
    }

    // ── liveness: direct-link keepalive / HOLD ───────────────────────────

    /// Direct-link keepalive: refresh the HOLD timer (authenticated when
    /// auth is on).
    pub fn handle_keepalive(&self, from_node: &str, token: Option<&str>) -> OkResponse {
        if !self.is_authed(from_node, token) {
            return OkResponse { ok: false };
        }
        self.touch_peer(from_node);
        OkResponse { ok: true }
    }

    /// Send a keepalive to every peer.
    pub async fn emit_keepalive(&self) {
        let me = self.node_id();
        let peers: Vec<Peer> = self.inner.lock().sessions.values().cloned().collect();
        let tasks: Vec<FanoutTask<'_>> = peers
            .into_iter()
            .map(|p| {
                let me = me.clone();
                let transport = self.transport.clone();
                Box::pin(async move {
                    transport
                        .keepalive(&p.address, &me, p.token.as_deref())
                        .await
                        .map(|_| ())
                }) as FanoutTask<'_>
            })
            .collect();
        self.fan_out(tasks).await;
    }

    /// Drop sessions whose direct link has been silent past `hold_timeout`.
    /// Returns the dropped node ids.
    pub fn check_hold(&self, now: Option<f64>) -> Vec<String> {
        let now = now.unwrap_or_else(|| self.clock_now());
        let stale: Vec<String> = self
            .inner
            .lock()
            .sessions
            .values()
            .filter(|p| now - p.last_seen > self.config.hold_timeout)
            .map(|p| p.node_id.clone())
            .collect();
        for node_id in &stale {
            tracing::info!("cluster: peer {node_id} HOLD expired; dropping session");
            self.disconnect_peer(node_id);
        }
        stale
    }

    /// Drop expired post-eviction suppression entries (memory hygiene).
    pub fn prune_suppression(&self, now: Option<f64>) {
        let now = now.unwrap_or_else(|| self.clock_now());
        self.inner.lock().evicted_until.retain(|_, t| now < *t);
    }

    /// Origins currently under post-eviction suppression.
    pub fn suppressed_origins(&self) -> Vec<String> {
        self.inner.lock().evicted_until.keys().cloned().collect()
    }

    // ── orchestration (us -> peer) ───────────────────────────────────────

    /// Initiate a session with the peer at `address` and run an initial
    /// full reconcile. `namespaces` defaults to our own datasets.
    pub async fn connect_peer(
        &self,
        address: &str,
        namespaces: Option<Vec<String>>,
        token: Option<&str>,
    ) -> std::result::Result<Peer, TransportError> {
        let offered = match namespaces {
            Some(ns) if !ns.is_empty() => ns,
            _ => self
                .registry
                .as_ref()
                .map(|r| r.list_datasets())
                .unwrap_or_default(),
        };
        let body = OpenRequest {
            node_id: self.node_id(),
            address: self.advertise.clone(),
            namespaces: offered,
            token: token.map(str::to_string),
        };
        let resp = self.transport.open(address, &body).await?;
        let peer = Peer {
            node_id: resp.node_id.clone(),
            address: address.to_string(),
            namespaces: resp.accepted.iter().cloned().collect(),
            last_seen: self.clock_now(),
            token: resp.session_token.clone(),
        };
        {
            let mut g = self.inner.lock();
            g.sessions.insert(peer.node_id.clone(), peer.clone());
            g.evicted_until.remove(&peer.node_id);
        }
        tracing::info!(
            "cluster: connected to {} (ns={:?})",
            peer.node_id,
            peer.namespaces
        );
        self.reconcile(&peer).await?;
        Ok(peer)
    }

    /// Bidirectional anti-entropy with `peer`, Merkle fast path first:
    /// compare bucket hashes; if identical, stop. Otherwise fetch and diff
    /// only the differing buckets, pull what is newer and push what we have
    /// newer. Transport errors propagate to the caller.
    pub async fn reconcile(&self, peer: &Peer) -> std::result::Result<ReconcileResult, TransportError> {
        let me = self.node_id();
        let ns: Vec<String> = peer.namespaces.iter().cloned().collect();
        let local_index = self.full_index(Some(&ns));
        let n = self.config.merkle_buckets;

        let local_buckets = merkle::bucket_hashes(&local_index, n);
        let remote_buckets = self
            .transport
            .merkle(&peer.address, &me, &ns, peer.token.as_deref())
            .await?;
        let diff = merkle::differing_buckets(&local_buckets, &remote_buckets);
        if diff.is_empty() {
            return Ok(ReconcileResult::default());
        }

        let diff_list: Vec<u32> = diff.iter().copied().collect();
        let remote_rows = self
            .transport
            .digest(&peer.address, &me, &ns, peer.token.as_deref(), Some(&diff_list))
            .await?;
        let remote_index: HashMap<Key, Version> = remote_rows
            .into_iter()
            .map(|(d, o, s, v)| ((d, o, s), v))
            .collect();
        let local_sub: HashMap<Key, Version> = local_index
            .into_iter()
            .filter(|(k, _)| diff.contains(&merkle::bucket_of(k, n)))
            .collect();

        let to_pull: Vec<Key> = remote_index
            .iter()
            .filter(|(k, rv)| version_newer(rv, local_sub.get(*k)))
            .map(|(k, _)| k.clone())
            .collect();
        let mut pulled = 0;
        if !to_pull.is_empty() {
            let envs = self
                .transport
                .pull(&peer.address, &me, &to_pull, peer.token.as_deref())
                .await?;
            for env in envs {
                if self.apply_inbound(env) {
                    pulled += 1;
                }
            }
        }

        let push_envs: Vec<SyncEnvelope> = local_sub
            .iter()
            .filter(|(k, lv)| version_newer(lv, remote_index.get(*k)))
            .filter_map(|(k, _)| self.build_envelope_for_key(k))
            .collect();
        let mut pushed = 0;
        if !push_envs.is_empty() {
            let res = self
                .transport
                .updates(&peer.address, &me, &push_envs, peer.token.as_deref())
                .await?;
            pushed = res.accepted;
        }
        tracing::info!(
            "cluster: reconciled with {} (pulled={pulled} pushed={pushed})",
            peer.node_id
        );
        Ok(ReconcileResult { pulled, pushed })
    }

    pub fn list_peers(&self) -> Vec<Peer> {
        self.inner.lock().sessions.values().cloned().collect()
    }

    pub fn get_peer(&self, node_id: &str) -> Option<Peer> {
        self.inner.lock().sessions.get(node_id).cloned()
    }

    /// Drop tombstones older than the retention window, local (persisted)
    /// and foreign (overlay). Returns the number removed.
    pub fn gc_tombstones(&self, now_ms_override: Option<i64>) -> usize {
        let now = now_ms_override.unwrap_or_else(now_ms);
        let retention_ms = (self.config.tombstone_retention() * 1000.0) as i64;
        let mut g = self.inner.lock();
        let before = g.state.tombstones.len();
        g.state
            .tombstones
            .retain(|_, t| now - t.deleted_at_ms <= retention_ms);
        let mut removed = before - g.state.tombstones.len();
        if removed > 0 {
            Self::save_logged(&g.state);
        }
        let before_f = g.foreign.len();
        g.foreign
            .retain(|_, env| !(env.tombstone && now - env.version.0 > retention_ms));
        removed += before_f - g.foreign.len();
        removed
    }

    /// The single eviction path. Drop the session, evict every record that
    /// originated at that peer, and start a suppression cooldown so
    /// anti-entropy cannot re-pull them from a peer that has not evicted yet.
    /// Returns `true` if a session existed.
    pub fn disconnect_peer(&self, node_id: &str) -> bool {
        let deadline = self.clock_now() + self.config.tombstone_retention();
        let (existed, evicted) = {
            let mut g = self.inner.lock();
            let existed = g.sessions.shift_remove(node_id).is_some();
            let mut evicted = Vec::new();
            g.foreign.retain(|k, _| {
                if k.1 == node_id {
                    evicted.push((k.0.clone(), k.2.clone()));
                    false
                } else {
                    true
                }
            });
            g.evicted_until.insert(node_id.to_string(), deadline);
            (existed, evicted)
        };
        if let Some(sink) = &self.eviction_sink {
            sink.on_evicted(node_id, &evicted);
        }
        existed
    }

    // ── read seams (dataset router merge) ────────────────────────────────

    /// Wrapped output rows for replicated live records in `dataset`, with a
    /// namespaced `id` (`origin_id:service_id`) plus `origin_id`.
    pub fn foreign_wrapped(&self, dataset: &str) -> Vec<Value> {
        let g = self.inner.lock();
        let mut out = Vec::new();
        for (key, env) in &g.foreign {
            if key.0 != dataset || env.tombstone {
                continue;
            }
            let Some(w) = env.wrapped() else { continue };
            if w.is_empty() {
                continue;
            }
            let mut row = w.clone();
            row.insert("id".into(), Value::String(format!("{}:{}", key.1, key.2)));
            row.insert("origin_id".into(), Value::String(key.1.clone()));
            out.push(Value::Object(row));
        }
        out
    }

    /// Replicated live records in `dataset` as `{entry, wrapped}` pairs.
    /// Rows without a `wrapped` object are skipped.
    pub fn foreign_rows(&self, dataset: &str) -> Vec<ForeignRow> {
        let g = self.inner.lock();
        let mut out = Vec::new();
        for (key, env) in &g.foreign {
            if key.0 != dataset || env.tombstone || env.payload.is_none() {
                continue;
            }
            let Some(w) = env.wrapped() else { continue };
            let entry = env.entry().cloned().unwrap_or(Value::Null);
            let mut wrapped = w.clone();
            wrapped.insert("id".into(), Value::String(format!("{}:{}", key.1, key.2)));
            wrapped.insert("origin_id".into(), Value::String(key.1.clone()));
            out.push(ForeignRow {
                entry,
                wrapped: Value::Object(wrapped),
            });
        }
        out
    }

    /// Resolve a namespaced `origin_id:service_id` to its replicated wrapped
    /// record, or `None`.
    pub fn foreign_entry(&self, dataset: &str, display_id: &str) -> Option<Value> {
        let (origin, sid) = display_id.split_once(':')?;
        let g = self.inner.lock();
        let env = g
            .foreign
            .get(&(dataset.to_string(), origin.to_string(), sid.to_string()))?;
        if env.tombstone {
            return None;
        }
        let mut wrapped = env.wrapped()?.clone();
        wrapped.insert("id".into(), Value::String(display_id.to_string()));
        wrapped.insert("origin_id".into(), Value::String(origin.to_string()));
        Some(Value::Object(wrapped))
    }

    // ── local mutation hooks ─────────────────────────────────────────────

    /// Stamp a new version (or tombstone) on a local record and persist.
    fn stamp_local(&self, dataset: &str, service_id: &str, op: MutationOp) -> Result<Version> {
        let mut g = self.inner.lock();
        let ts = Self::next_ts_locked(&mut g.state);
        let node = g.state.node_id.clone();
        let version = Version::new(ts, node);
        let k = make_key(dataset, service_id);
        match op {
            MutationOp::Deregister => {
                g.state.tombstones.insert(
                    k.clone(),
                    Tombstone {
                        version: version.clone(),
                        deleted_at_ms: ts,
                    },
                );
                g.state.local_versions.remove(&k);
            }
            MutationOp::Register | MutationOp::Update => {
                g.state.local_versions.insert(k.clone(), version.clone());
                g.state.tombstones.remove(&k);
            }
        }
        g.state.save()?;
        Ok(version)
    }

    /// Called after every successful local CRUD. Stamps a new version on the
    /// record (a tombstone on deregister), persists, and pushes the delta to
    /// all peers that sync this namespace. The envelope payload is read back
    /// from the [`LocalRegistryView`].
    pub async fn on_local_mutation(&self, dataset: &str, service_id: &str, op: MutationOp) -> Result<()> {
        self.stamp_local(dataset, service_id, op)?;
        if let Some(env) = self.build_local_envelope(dataset, service_id) {
            self.broadcast(&env).await;
        }
        Ok(())
    }

    /// Register/update hook with the payload supplied by the caller, so the
    /// backend need not expose the record through the view before the push.
    pub async fn on_local_upsert(
        &self,
        dataset: &str,
        service_id: &str,
        entry: Value,
        wrapped: Option<Value>,
    ) -> Result<()> {
        let version = self.stamp_local(dataset, service_id, MutationOp::Update)?;
        let env = SyncEnvelope {
            dataset: dataset.to_string(),
            service_id: service_id.to_string(),
            origin_id: self.node_id(),
            version,
            tombstone: false,
            payload: Some(json!({"entry": entry, "wrapped": wrapped})),
        };
        self.broadcast(&env).await;
        Ok(())
    }

    /// Deregister hook: writes a tombstone and broadcasts it.
    pub async fn on_local_delete(&self, dataset: &str, service_id: &str) -> Result<()> {
        self.on_local_mutation(dataset, service_id, MutationOp::Deregister)
            .await
    }

    // ── lifecycle ────────────────────────────────────────────────────────

    /// Spawn the anti-entropy sweeper and keepalive monitor as tokio tasks.
    pub fn start(self: &Arc<Self>, cancel: CancellationToken) -> ClusterHandle {
        let ae = AntiEntropySweeper::new(self.clone(), self.config.anti_entropy_interval);
        let km = KeepaliveMonitor::new(self.clone(), self.config.keepalive_interval);
        let tasks = vec![ae.spawn(cancel.clone()), km.spawn(cancel.clone())];
        ClusterHandle { cancel, tasks }
    }

    // ── observability ────────────────────────────────────────────────────

    /// Snapshot of sync state for the CLI and `GET /api/cluster/state`.
    pub fn state_summary(&self) -> StateSummary {
        let g = self.inner.lock();
        let mut foreign_by_namespace: BTreeMap<String, usize> = BTreeMap::new();
        for (key, env) in &g.foreign {
            if !env.tombstone {
                *foreign_by_namespace.entry(key.0.clone()).or_default() += 1;
            }
        }
        StateSummary {
            node_id: g.state.node_id.clone(),
            advertise: self.advertise.clone(),
            peers: g.sessions.values().map(Peer::to_summary).collect(),
            foreign_records: foreign_by_namespace.values().sum(),
            foreign_by_namespace,
            local_records: g.state.local_versions.len(),
            tombstones: g.state.tombstones.len(),
        }
    }

    /// Alias of [`Self::state_summary`] for the backend's status command.
    pub fn status(&self) -> StateSummary {
        self.state_summary()
    }
}

impl std::fmt::Debug for ClusterStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterStore")
            .field("node_id", &self.node_id())
            .field("advertise", &self.advertise)
            .finish()
    }
}

impl From<ClusterError> for TransportError {
    fn from(e: ClusterError) -> Self {
        TransportError::new(e.to_string())
    }
}
