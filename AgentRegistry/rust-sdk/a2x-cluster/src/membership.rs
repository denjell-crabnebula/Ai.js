// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Declarative cluster membership control plane.
//!
//! Each node owns exactly one membership record (origin-only, LWW):
//! `MembershipRecord { node_id, cluster_id|None, address, version, removed }`.
//! `roster(C)` is every record with `cluster_id == C` and `!removed`.
//! Records are replicated as a small overlay kept separate from the
//! service-record overlay: membership must not flow through namespace-auth
//! gating or the service read path, and a removal tombstone must survive a
//! session drop.
//!
//! Replication has three deterministic paths: a bootstrap `join` push for
//! brand-new members, an immediate push of local changes to all roster
//! members, and delta anti-entropy over a compact `{node_id: version}` map.
//!
//! The control plane decides who is in the cluster and whom to connect; the
//! [`ClusterStore`] executes `connect_peer` / `disconnect_peer` and runs the
//! data plane.

use std::collections::BTreeMap;
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::envelope::{Version, version_newer};
use crate::peer::Peer;
use crate::store::{ClusterStore, FanoutTask, now_ms};
use crate::transport::{EvictRequest, JoinRequest, JoinResponse, LeaveRequest, OkResponse, SetSyncResponse};

/// A readable, globally unique cluster id (`clu-<12 hex>`). Minted once by
/// `set add` when a node first forms a cluster.
pub fn generate_cluster_id() -> String {
    format!("clu-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// One node's membership record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MembershipRecord {
    pub node_id: String,
    #[serde(default)]
    pub cluster_id: Option<String>,
    #[serde(default)]
    pub address: String,
    pub version: Version,
    #[serde(default)]
    pub removed: bool,
}

impl MembershipRecord {
    pub fn new(
        node_id: impl Into<String>,
        cluster_id: Option<String>,
        address: impl Into<String>,
        version: Version,
        removed: bool,
    ) -> Self {
        MembershipRecord {
            node_id: node_id.into(),
            cluster_id,
            address: address.into(),
            version,
            removed,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Parse a wire record. A legacy `{node_id, address}` hint without a
    /// version is rejected here; [`MembershipStore`] handles it on restore.
    pub fn from_value(v: &Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }
}

/// One entry of [`ShowResponse::roster`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub node_id: String,
    pub address: String,
    pub alive: bool,
}

/// `GET /api/cluster/set` body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShowResponse {
    pub cluster_id: Option<String>,
    pub node_id: String,
    pub roster: Vec<RosterEntry>,
}

/// One member of a `set add` / `set remove` request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemberSpec {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub node_id: Option<String>,
}

impl MemberSpec {
    pub fn address(address: impl Into<String>) -> Self {
        MemberSpec {
            address: Some(address.into()),
            node_id: None,
        }
    }

    pub fn node(node_id: impl Into<String>) -> Self {
        MemberSpec {
            address: None,
            node_id: Some(node_id.into()),
        }
    }
}

/// node_id -> record (own record plus peers; may be tombstones)
pub(crate) type Roster = IndexMap<String, MembershipRecord>;

/// The membership overlay plus the `set add/remove/leave` control plane.
///
/// The roster itself is owned by the [`ClusterStore`]; this is a handle that
/// pairs it with a strong reference to the store, so handles are cheap to
/// create and can never outlive the store they operate on.
pub struct MembershipStore {
    store: Arc<ClusterStore>,
    roster: Arc<Mutex<Roster>>,
}

impl MembershipStore {
    /// Create the control plane for `store`, seed it from persisted state
    /// and attach it. Returns the attached instance (an existing one if the
    /// store already had a membership plane).
    pub fn attach(store: &Arc<ClusterStore>) -> Arc<MembershipStore> {
        let roster = store.membership_roster_or_init(|| {
            let roster = Arc::new(Mutex::new(IndexMap::new()));
            MembershipStore::with_roster(store.clone(), roster.clone()).init_from_state(store);
            roster
        });
        Arc::new(MembershipStore::with_roster(store.clone(), roster))
    }

    pub(crate) fn with_roster(store: Arc<ClusterStore>, roster: Arc<Mutex<Roster>>) -> MembershipStore {
        MembershipStore { store, roster }
    }

    fn store(&self) -> &Arc<ClusterStore> {
        &self.store
    }

    // ── identity ─────────────────────────────────────────────────────────

    pub fn node_id(&self) -> String {
        self.store().node_id()
    }

    pub fn cluster_id(&self) -> Option<String> {
        let me = self.node_id();
        self.roster.lock().get(&me).and_then(|r| r.cluster_id.clone())
    }

    /// This node's own record, created standalone on first access.
    pub fn my_record(&self) -> MembershipRecord {
        let store = self.store();
        let me = store.node_id();
        let mut roster = self.roster.lock();
        if let Some(own) = roster.get(&me) {
            return own.clone();
        }
        let own = MembershipRecord::new(me.clone(), None, store.advertise(), store.next_version(), false);
        roster.insert(me, own.clone());
        own
    }

    /// Snapshot of the whole overlay (tests and diagnostics).
    pub fn roster_snapshot(&self) -> IndexMap<String, MembershipRecord> {
        self.roster.lock().clone()
    }

    /// Insert a record directly into the overlay (tests only).
    pub fn insert_record(&self, rec: MembershipRecord) {
        self.roster.lock().insert(rec.node_id.clone(), rec);
    }

    fn init_from_state(&self, store: &Arc<ClusterStore>) {
        let state = store.state_snapshot();
        let Some(cid) = state.cluster_id.clone() else {
            return;
        };
        let me = state.node_id.clone();
        if let Some(mmv) = &state.my_membership_version {
            store.observe_version(mmv.0);
        }
        let ver = state
            .my_membership_version
            .clone()
            .unwrap_or_else(|| store.next_version());
        {
            let mut roster = self.roster.lock();
            roster.insert(
                me.clone(),
                MembershipRecord::new(me.clone(), Some(cid.clone()), store.advertise(), ver, false),
            );
            for d in &state.last_roster {
                let rec = if d.get("version").is_some() {
                    match MembershipRecord::from_value(d) {
                        Some(r) => r,
                        None => continue,
                    }
                } else {
                    // legacy {node_id, address} hint
                    let Some(nid) = d.get("node_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let addr = d.get("address").and_then(Value::as_str).unwrap_or("");
                    MembershipRecord::new(nid, Some(cid.clone()), addr, Version::new(0, nid), false)
                };
                if rec.node_id != me && !roster.contains_key(&rec.node_id) {
                    roster.insert(rec.node_id.clone(), rec);
                }
            }
        }
        self.gc_membership(None);
    }

    // ── LWW merge (inbound) ──────────────────────────────────────────────

    /// LWW-merge incoming membership records. Returns `true` if anything
    /// changed. Our own record is authoritative and ignored, except a newer
    /// removal tombstone for ourselves, which means we were evicted.
    pub fn merge(&self, records: &[Value]) -> bool {
        let me = self.node_id();
        let mut changed = false;
        let mut evicted_self = false;
        {
            let mut roster = self.roster.lock();
            for d in records {
                let Some(rec) = MembershipRecord::from_value(d) else {
                    continue;
                };
                let cur = roster.get(&rec.node_id).map(|r| r.version.clone());
                if !version_newer(&rec.version, cur.as_ref()) {
                    continue;
                }
                if rec.node_id == me {
                    if rec.removed {
                        evicted_self = true;
                    }
                    continue;
                }
                roster.insert(rec.node_id.clone(), rec);
                changed = true;
            }
        }
        if evicted_self {
            self.become_standalone();
            return true;
        }
        if changed {
            self.persist();
        }
        changed
    }

    // ── digest / pull (delta anti-entropy) ───────────────────────────────

    pub fn version_map(&self) -> BTreeMap<String, Version> {
        self.roster
            .lock()
            .iter()
            .map(|(n, r)| (n.clone(), r.version.clone()))
            .collect()
    }

    pub fn records_for(&self, node_ids: &[String]) -> Vec<MembershipRecord> {
        let roster = self.roster.lock();
        node_ids.iter().filter_map(|n| roster.get(n).cloned()).collect()
    }

    /// Pull membership deltas from one peer (best-effort).
    pub async fn reconcile_with(&self, peer: &Peer) {
        if self.cluster_id().is_none() {
            return;
        }
        let store = self.store();
        let me = store.node_id();
        let remote = match store
            .transport()
            .set_digest(&peer.address, &me, peer.token.as_deref())
            .await
        {
            Ok(r) => r,
            Err(_) => return,
        };
        let mine = self.version_map();
        let want: Vec<String> = remote
            .iter()
            .filter(|(nid, rv)| version_newer(rv, mine.get(*nid)))
            .map(|(nid, _)| nid.clone())
            .collect();
        if want.is_empty() {
            return;
        }
        let recs = match store
            .transport()
            .set_pull(&peer.address, &me, &want, peer.token.as_deref())
            .await
        {
            Ok(r) => r,
            Err(_) => return,
        };
        self.merge(&recs);
    }

    // ── serve side (peer -> us) ──────────────────────────────────────────

    pub fn serve_set_digest(&self, from_node: &str, token: Option<&str>) -> BTreeMap<String, Version> {
        if !self.store().authed(from_node, token) {
            return BTreeMap::new();
        }
        self.version_map()
    }

    pub fn serve_set_pull(
        &self,
        from_node: &str,
        node_ids: &[String],
        token: Option<&str>,
    ) -> Vec<MembershipRecord> {
        if !self.store().authed(from_node, token) {
            return Vec::new();
        }
        self.records_for(node_ids)
    }

    /// Receive a pushed batch (immediate-push path) and LWW-merge it.
    pub fn serve_set_sync(&self, from_node: &str, records: &[Value], token: Option<&str>) -> SetSyncResponse {
        if !self.store().authed(from_node, token) {
            return SetSyncResponse { accepted: false };
        }
        self.merge(records);
        SetSyncResponse { accepted: true }
    }

    // ── immediate push (outbound) ────────────────────────────────────────

    /// Push `records` (default: our own record) to every live roster member.
    pub async fn push_to_roster(&self, records: Option<Vec<MembershipRecord>>) {
        let records = records.unwrap_or_else(|| vec![self.my_record()]);
        let store = self.store();
        let me = store.node_id();
        let cid = self.cluster_id();
        let targets: Vec<(String, String)> = self
            .roster
            .lock()
            .values()
            .filter(|r| r.node_id != me && !r.removed && r.cluster_id == cid && !r.address.is_empty())
            .map(|r| (r.node_id.clone(), r.address.clone()))
            .collect();
        let records = &records;
        let tasks: Vec<FanoutTask<'_>> = targets
            .into_iter()
            .map(|(nid, addr)| {
                let token = self.peer_token(&nid);
                let me = me.clone();
                let transport = store.transport().clone();
                Box::pin(async move {
                    transport
                        .set_sync(&addr, &me, records, token.as_deref())
                        .await
                        .map(|_| ())
                }) as FanoutTask<'_>
            })
            .collect();
        store.fan_out(tasks).await;
    }

    // ── control plane (user -> us) ───────────────────────────────────────

    /// Add members to this node's cluster, minting a `cluster_id` if we do
    /// not have one. Bootstraps each new member with a `join` push carrying
    /// the current roster. Returns `{"cluster_id", "results": [...]}`.
    pub async fn set_add(&self, members: &[MemberSpec], token: Option<&str>) -> Value {
        let store = self.store();
        let me = store.node_id();
        let (cid, roster_snapshot) = {
            let mut roster = self.roster.lock();
            let has_cluster = roster.get(&me).and_then(|r| r.cluster_id.clone()).is_some();
            if !has_cluster {
                let cid = generate_cluster_id();
                roster.insert(
                    me.clone(),
                    MembershipRecord::new(
                        me.clone(),
                        Some(cid),
                        store.advertise(),
                        store.next_version(),
                        false,
                    ),
                );
            }
            let cid = roster.get(&me).and_then(|r| r.cluster_id.clone());
            let snapshot: Vec<Value> = roster
                .values()
                .filter(|r| !r.removed)
                .map(|r| r.to_value())
                .collect();
            (cid, snapshot)
        };
        let mut results = Vec::new();
        for m in members {
            let Some(addr) = m.address.as_ref().filter(|a| !a.is_empty()) else {
                results.push(json!({"address": m.address, "ok": false, "error": "address required"}));
                continue;
            };
            let body = JoinRequest {
                cluster_id: cid.clone(),
                roster: roster_snapshot.clone(),
                token: token.map(str::to_string),
                from_node: me.clone(),
                from_address: store.advertise().to_string(),
            };
            let resp = match store.transport().join(addr, &body).await {
                Ok(r) => r,
                Err(err) => {
                    results
                        .push(json!({"address": addr, "ok": false, "error": format!("unreachable: {err}")}));
                    continue;
                }
            };
            if !resp.accepted {
                results.push(json!({
                    "address": addr, "ok": false,
                    "error": resp.error.unwrap_or_else(|| "rejected".to_string()),
                }));
                continue;
            }
            let nid = resp.node_id.clone().unwrap_or_default();
            let ver = resp.version.clone().unwrap_or_else(|| store.next_version());
            self.roster.lock().insert(
                nid.clone(),
                MembershipRecord::new(nid.clone(), cid.clone(), addr.clone(), ver, false),
            );
            results.push(json!({"address": addr, "node_id": nid, "ok": true}));
        }
        self.persist();
        self.reconcile_connections().await;
        // Propagate the grown roster to everyone (new members already have it).
        let full: Vec<MembershipRecord> = self.roster.lock().values().cloned().collect();
        self.push_to_roster(Some(full)).await;
        json!({"cluster_id": self.cluster_id(), "results": results})
    }

    /// Remove members (each `{node_id}`): write a removal tombstone
    /// (deterministic, not HOLD based), notify each removed node, and
    /// disconnect it. Returns `{"results": [...]}`.
    pub async fn set_remove(&self, members: &[MemberSpec]) -> Value {
        let store = self.store();
        let me = store.node_id();
        let mut results = Vec::new();
        let mut tombstones = Vec::new();
        for m in members {
            let Some(nid) = m.node_id.as_ref().filter(|n| !n.is_empty()) else {
                results.push(json!({"ok": false, "error": "node_id required"}));
                continue;
            };
            let cid = self.cluster_id();
            let (addr, tomb) = {
                let mut roster = self.roster.lock();
                let cur = roster.get(nid).cloned();
                let addr = cur
                    .as_ref()
                    .map(|c| c.address.clone())
                    .unwrap_or_else(|| m.address.clone().unwrap_or_default());
                // The tombstone must out-version the foreign-minted live record.
                let ver = store.next_version_after(cur.as_ref().map(|c| c.version.0).unwrap_or(0));
                let tomb = MembershipRecord::new(nid.clone(), cid.clone(), addr.clone(), ver, true);
                roster.insert(nid.clone(), tomb.clone());
                (addr, tomb)
            };
            tombstones.push(tomb);
            let body = EvictRequest {
                from_node: me.clone(),
                cluster_id: cid,
                token: self.peer_token(nid),
            };
            let _ = store.transport().evict(&addr, &body).await;
            store.disconnect_peer(nid);
            results.push(json!({"node_id": nid, "ok": true}));
        }
        self.persist();
        if !tombstones.is_empty() {
            self.push_to_roster(Some(tombstones)).await;
        }
        json!({"results": results})
    }

    /// This node's cluster id plus its live roster with liveness flags.
    pub fn show(&self) -> ShowResponse {
        let store = self.store();
        let me = store.node_id();
        let live: std::collections::HashSet<String> =
            store.list_peers().into_iter().map(|p| p.node_id).collect();
        let roster = self
            .roster
            .lock()
            .values()
            .filter(|r| !r.removed)
            .map(|r| RosterEntry {
                node_id: r.node_id.clone(),
                address: r.address.clone(),
                alive: live.contains(&r.node_id) || r.node_id == me,
            })
            .collect();
        ShowResponse {
            cluster_id: self.cluster_id(),
            node_id: me,
            roster,
        }
    }

    // ── adopt / leave (peer -> us) ───────────────────────────────────────

    /// A peer is pulling us into `cluster_id`. Auth-gate (admin token when
    /// auth is on), then adopt.
    pub async fn handle_join(&self, body: JoinRequest) -> JoinResponse {
        if !self.join_authorized(body.token.as_deref()) {
            return JoinResponse {
                accepted: false,
                error: Some("unauthorized".into()),
                ..Default::default()
            };
        }
        let Some(cid) = body.cluster_id.filter(|c| !c.is_empty()) else {
            return JoinResponse {
                accepted: false,
                error: Some("cluster_id required".into()),
                ..Default::default()
            };
        };
        self.adopt(&cid, &body.roster).await
    }

    /// Adopt `cluster_id` (leaving any old cluster first), learn the roster,
    /// connect the mesh, announce ourselves.
    pub async fn adopt(&self, cluster_id: &str, roster: &[Value]) -> JoinResponse {
        let old = self.cluster_id();
        if let Some(old) = old {
            if old != cluster_id {
                self.leave_old(&old).await;
            }
        }
        let store = self.store();
        let me = store.node_id();
        let ver = store.next_version();
        self.roster.lock().insert(
            me.clone(),
            MembershipRecord::new(
                me.clone(),
                Some(cluster_id.to_string()),
                store.advertise(),
                ver.clone(),
                false,
            ),
        );
        self.merge(roster);
        self.persist();
        self.reconcile_connections().await;
        self.push_to_roster(None).await;
        JoinResponse {
            accepted: true,
            node_id: Some(me),
            cluster_id: Some(cluster_id.to_string()),
            version: Some(ver),
            error: None,
        }
    }

    /// Gracefully leave the old cluster: tell each old member (so they drop
    /// us now, not via HOLD), then disconnect. Sent before disconnecting so
    /// the leave RPC can still authenticate over the existing session.
    pub async fn leave_old(&self, old_cluster_id: &str) {
        let store = self.store();
        let me = store.node_id();
        let members: Vec<(String, String)> = self
            .roster
            .lock()
            .values()
            .filter(|r| r.node_id != me && r.cluster_id.as_deref() == Some(old_cluster_id) && !r.removed)
            .map(|r| (r.node_id.clone(), r.address.clone()))
            .collect();
        for (nid, addr) in &members {
            let body = LeaveRequest {
                from_node: me.clone(),
                token: self.peer_token(nid),
            };
            let _ = store.transport().evict_self(addr, &body).await;
        }
        for (nid, _) in &members {
            store.disconnect_peer(nid);
        }
        // Drop only the old cluster's records; keep ourselves and anything a
        // concurrent inbound merge learned about another cluster.
        self.roster
            .lock()
            .retain(|n, r| *n == me || r.cluster_id.as_deref() != Some(old_cluster_id));
    }

    /// A peer is gracefully leaving our cluster: tombstone it and drop it.
    /// Authenticated so a spoofer cannot forge a removal for a victim.
    pub async fn handle_evict_self(&self, body: LeaveRequest) -> OkResponse {
        let store = self.store();
        let nid = body.from_node.clone();
        if nid.is_empty() || !store.authed(&nid, body.token.as_deref()) {
            return OkResponse { ok: false };
        }
        let tomb = {
            let mut roster = self.roster.lock();
            let cur = roster.get(&nid).cloned();
            let ver = store.next_version_after(cur.as_ref().map(|c| c.version.0).unwrap_or(0));
            let cid = match &cur {
                Some(c) => c.cluster_id.clone(),
                None => roster.get(&store.node_id()).and_then(|r| r.cluster_id.clone()),
            };
            let addr = cur.as_ref().map(|c| c.address.clone()).unwrap_or_default();
            let tomb = MembershipRecord::new(nid.clone(), cid, addr, ver, true);
            roster.insert(nid.clone(), tomb.clone());
            tomb
        };
        store.disconnect_peer(&nid);
        self.persist();
        self.push_to_roster(Some(vec![tomb])).await;
        OkResponse { ok: true }
    }

    /// We were removed from our cluster: revert to standalone. Only a peer
    /// with a valid session can force this.
    pub fn handle_evicted(&self, body: EvictRequest) -> OkResponse {
        if !self.store().authed(&body.from_node, body.token.as_deref()) {
            return OkResponse { ok: false };
        }
        self.become_standalone();
        OkResponse { ok: true }
    }

    // ── reconcile-loop input ─────────────────────────────────────────────

    /// `{node_id: address}` we should hold a session with: the live roster
    /// of our cluster minus ourselves. Empty when standalone.
    pub fn desired_peers(&self) -> BTreeMap<String, String> {
        let Some(cid) = self.cluster_id() else {
            return BTreeMap::new();
        };
        let me = self.node_id();
        self.roster
            .lock()
            .values()
            .filter(|r| {
                r.node_id != me
                    && r.cluster_id.as_deref() == Some(cid.as_str())
                    && !r.removed
                    && !r.address.is_empty()
            })
            .map(|r| (r.node_id.clone(), r.address.clone()))
            .collect()
    }

    /// True if `node_id` is not a wanted member (tombstoned, gone, or in
    /// another cluster), so the reconcile loop may disconnect it.
    pub fn is_removed_or_absent(&self, node_id: &str) -> bool {
        let cid = self.cluster_id();
        match self.roster.lock().get(node_id) {
            None => true,
            Some(r) => r.removed || r.cluster_id != cid,
        }
    }

    /// Drive the session set toward [`Self::desired_peers`]: connect
    /// missing members, disconnect removed or absent ones. No-op when
    /// standalone, so manual `add-peer` is untouched.
    pub async fn reconcile_connections(&self) {
        if self.cluster_id().is_none() {
            return;
        }
        let store = self.store();
        let desired = self.desired_peers();
        let actual: std::collections::HashSet<String> =
            store.list_peers().into_iter().map(|p| p.node_id).collect();
        for (nid, addr) in &desired {
            if !actual.contains(nid) {
                if let Err(err) = store.connect_peer(addr, None, None).await {
                    tracing::debug!("cluster: membership connect to {addr} failed: {err}");
                }
            }
        }
        for nid in actual {
            if !desired.contains_key(&nid) && self.is_removed_or_absent(&nid) {
                store.disconnect_peer(&nid);
            }
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    fn become_standalone(&self) {
        let store = self.store();
        let me = store.node_id();
        let members: Vec<String> = {
            let mut roster = self.roster.lock();
            let members = roster.keys().filter(|n| **n != me).cloned().collect();
            roster.clear();
            members
        };
        for nid in members {
            store.disconnect_peer(&nid);
        }
        self.persist();
    }

    fn join_authorized(&self, token: Option<&str>) -> bool {
        let store = self.store();
        let auth = store.auth();
        if !auth.auth_enabled() {
            return true;
        }
        let Some(token) = token.filter(|t| !t.is_empty()) else {
            return false;
        };
        auth.authenticate(token).map(|c| c.is_admin()).unwrap_or(false)
    }

    fn peer_token(&self, node_id: &str) -> Option<String> {
        self.store().get_peer(node_id).and_then(|p| p.token)
    }

    /// Drop removal tombstones older than the retention window so the roster
    /// overlay stays bounded. Returns the count pruned.
    pub fn gc_membership(&self, now_ms_override: Option<i64>) -> usize {
        let now = now_ms_override.unwrap_or_else(now_ms);
        let retention_ms = (self.store().config().tombstone_retention() * 1000.0) as i64;
        let mut roster = self.roster.lock();
        let before = roster.len();
        roster.retain(|_, r| !(r.removed && now - r.version.0 > retention_ms));
        before - roster.len()
    }

    fn persist(&self) {
        let store = self.store();
        let me = store.node_id();
        let (cid, records, mmv) = {
            let roster = self.roster.lock();
            let own = roster.get(&me);
            let cid = own.and_then(|r| r.cluster_id.clone());
            // Persist live members and recent tombstones so a restart keeps
            // the removal window.
            let records: Vec<Value> = roster
                .values()
                .filter(|r| r.node_id != me && r.cluster_id == cid)
                .map(|r| r.to_value())
                .collect();
            (cid, records, own.map(|r| r.version.clone()))
        };
        if let Err(err) = store.persist_membership(cid, records, mmv) {
            tracing::warn!("cluster: failed to persist membership state: {err}");
        }
    }
}

impl std::fmt::Debug for MembershipStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipStore")
            .field("roster", &self.roster.lock().len())
            .finish()
    }
}
