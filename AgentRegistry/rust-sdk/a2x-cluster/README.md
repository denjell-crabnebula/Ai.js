# a2x-cluster

Rust port of `AgentRegistry/a2x_registry/cluster` from
[openJiuwen-ai/agent-protocol](https://github.com/openJiuwen-ai/agent-protocol):
opt-in distributed sync for the A2X registry. Several registry instances form a
full mesh and replicate their flat registries, so a query to any member returns
every member's services. A member that goes silent is dropped by its peers'
HOLD timers, which evict its records.

Design (see `docs/cluster_design.md` in the original repository):

- **Topology**: full mesh, no relay. Every member holds a direct session with
  every other member; the declarative membership control plane keeps the mesh
  in sync with the roster (`cluster set add|remove`).
- **Consistency**: AP, eventually consistent. LWW on the version
  `(updated_at_ms, node_id)` compared lexicographically. Origin-only writes;
  the global key is `(dataset, origin_id, service_id)`, so same-named services
  on two nodes never collide.
- **Propagation**: a local CRUD is pushed directly to all peers. Inbound
  updates are stored, never forwarded. Merkle bucketed anti-entropy heals
  dropped pushes (256 buckets by default, only differing buckets transfer rows).
- **Deletion**: tombstones carry a version and win LWW over stale live values;
  they are kept for `tombstone_retention = hold_timeout + keepalive_interval`.
- **Liveness**: per registry node, one path. A peer silent past `hold_timeout`
  is disconnected, all its replicas are evicted, and its origin is suppressed
  for `tombstone_retention` so anti-entropy cannot resurrect it. A new
  handshake lifts the suppression.
- **Persistence**: only `cluster_state.json` (node identity, version clock,
  local versions, tombstones, cluster id, last roster, own membership version),
  written with `a2x_common::atomic::atomic_write_json`. Foreign replicas stay in
  memory and are re-synced on reconnect.

The crate does not depend on the registry crate. The backend implements three
small traits, mounts the router and calls the mutation hooks.

## Quick start (embedding in the backend)

```rust,ignore
use std::sync::Arc;
use a2x_cluster::{router, dormant_router, ClusterConfig, ClusterStore, HttpTransport};
use tokio_util::sync::CancellationToken;

let config = ClusterConfig::from_env();               // A2X_REGISTRY_CLUSTER_* overrides
let advertise = std::env::var("A2X_REGISTRY_CLUSTER_ADVERTISE").unwrap_or_default();
let store = ClusterStore::builder()
    .config(config.clone())
    .registry(registry_view)                          // Arc<dyn LocalRegistryView>
    .auth(auth_verifier)                              // Arc<dyn PeerAuthVerifier>, or Arc::new(AllowAll)
    .transport(Arc::new(HttpTransport::new(config.http_timeout)))
    .advertise(advertise)
    .load_or_none();                                  // None when cluster_state.json is absent

let app = match &store {
    Some(store) => axum::Router::new().merge(router(store.clone())),
    None => axum::Router::new().merge(dormant_router()),   // every /api/cluster/* answers 404
};
let handle = store.as_ref().map(|s| s.start(CancellationToken::new()));

// after every successful local CRUD:
//   store.on_local_upsert(dataset, service_id, entry_json, Some(wrapped_row)).await?;
//   store.on_local_delete(dataset, service_id).await?;
// list endpoint merge:  store.foreign_rows(dataset)  (rows carry id "origin:sid", origin_id; add source="cluster")
// single get fallback:  store.foreign_entry(dataset, "origin:sid")
// shutdown:             handle.shutdown().await; store.close();
```

`ClusterStoreBuilder::build(state)` attaches the membership control plane by
default (`.membership(false)` disables it, like a Python store without
`MembershipStore`).

## API mapping

| Python (`a2x_registry.cluster`) | Rust |
|---|---|
| `config.ClusterConfig` / `.from_env()` / `.tombstone_retention` | `ClusterConfig` / `ClusterConfig::from_env()` / `tombstone_retention()` |
| `state.state_path()` / `ENV_STATE_PATH` | `state::state_path()` / `state::ENV_STATE_PATH` |
| `state.make_key` / `split_key` / `generate_node_id` | `state::make_key` / `split_key` / `generate_node_id` |
| `state.Tombstone`, `state.ClusterState` | `Tombstone`, `ClusterState` |
| `ClusterState.load(path)` / `.init(node_id, path)` / `.save()` / `.to_dict()` | `ClusterState::load()` / `load_from(path)` / `init(node_id)` / `init_at(node_id, path)` / `save()` / `to_json()` |
| `envelope.Version`, `SyncEnvelope`, `version_newer` | `Version(i64, String)`, `SyncEnvelope`, `version_newer(&a, Option<&b>)` |
| `merkle.bucket_of` / `bucket_hashes` / `differing_buckets` / `keys_in_buckets` | `merkle::bucket_of` / `bucket_hashes` / `differing_buckets` / `keys_in_buckets` |
| `peer.Peer` / `.to_summary()` | `Peer` / `Peer::to_summary() -> PeerSummary` |
| `auth_handshake.authorize_namespaces(registry, auth_store, requested, token)` | `auth_handshake::authorize_namespaces(registry, verifier, requested, token) -> Authorized {accepted, ephemeral}` |
| `transport.Transport` (abstract) / `TransportError` / `SESSION_HEADER` | `Transport` (async trait) / `TransportError` / `SESSION_HEADER` |
| `transport.HttpTransport(timeout)` / `.close()` | `HttpTransport::new(timeout)` / `close()` |
| `store.ClusterStore(state, config, registry_svc, transport, advertise, auth_store_getter, clock)` | `ClusterStore::builder().config().registry().transport().advertise().auth().clock().eviction_sink().build(state) -> Arc<ClusterStore>` |
| `ClusterStore.load_or_none(...)` | `ClusterStoreBuilder::load_or_none()` / `load_or_none_at(path)` |
| `.node_id` / `.config` / `.advertise` / `.transport` / `.state` / `.auth_store` / `.membership` | `node_id()` / `config()` / `advertise()` / `transport()` / `state_snapshot()` / `auth()` / `membership()` |
| `.authed(from_node, token)` | `authed(from_node, token)` |
| `.next_version()` / `.next_version_after(ms)` / `.observe_version(ms)` / `.save_state()` | same names |
| `.apply_inbound(env)` | `apply_inbound(env) -> bool` |
| `.handle_open(body)` | `handle_open(OpenRequest) -> OpenResponse` |
| `.serve_digest(from, ns, token, buckets)` | `serve_digest(from, Option<&[String]>, Option<&str>, Option<&[u32]>) -> Vec<DigestRow>` |
| `.serve_merkle(from, ns, token)` | `serve_merkle(...) -> BTreeMap<String, String>` |
| `.serve_pull(from, keys, token)` | `serve_pull(from, &[Key], token) -> Vec<SyncEnvelope>` |
| `.serve_updates(from, envelopes, token)` | `serve_updates(from, Vec<SyncEnvelope>, token) -> UpdatesResponse` |
| `.handle_keepalive(from, token)` | `handle_keepalive(from, token) -> OkResponse` |
| `.fan_out(thunks)` / `._broadcast(env)` / `.emit_keepalive()` | `async fan_out(Vec<FanoutTask>)` / `async broadcast(&env)` / `async emit_keepalive()` |
| `.check_hold(now)` / `.prune_suppression(now)` / `._evicted_until` | `check_hold(Option<f64>) -> Vec<String>` / `prune_suppression(Option<f64>)` / `suppressed_origins()` |
| `.connect_peer(address, namespaces, token)` | `async connect_peer(address, Option<Vec<String>>, Option<&str>) -> Result<Peer, TransportError>` |
| `.reconcile(peer)` | `async reconcile(&Peer) -> Result<ReconcileResult {pulled, pushed}, TransportError>` |
| `.list_peers()` / `._sessions[id]` | `list_peers()` / `get_peer(id)` |
| `.gc_tombstones(now_ms)` | `gc_tombstones(Option<i64>) -> usize` |
| `.disconnect_peer(node_id)` | `disconnect_peer(node_id) -> bool` |
| `.foreign_wrapped(ds)` / `.foreign_rows(ds)` / `.foreign_entry(ds, id)` | `foreign_wrapped(ds) -> Vec<Value>` / `foreign_rows(ds) -> Vec<ForeignRow {entry, wrapped}>` / `foreign_entry(ds, id) -> Option<Value>` |
| `.on_local_mutation(ds, sid, op, entry)` | `async on_local_mutation(ds, sid, MutationOp)`; `async on_local_upsert(ds, sid, entry, wrapped)`; `async on_local_delete(ds, sid)` |
| `.state_summary()` | `state_summary()` / `status()` -> `StateSummary` |
| `.close()` | `close()` |
| `sweepers.AntiEntropySweeper(store, period)` / `.tick()` / `.start()` / `.stop()` | `AntiEntropySweeper::new(store, period)` / `async tick()` / `spawn(CancellationToken) -> JoinHandle` |
| `sweepers.KeepaliveMonitor` | `KeepaliveMonitor` (same shape) |
| backend startup: sweeper start / app shutdown | `ClusterStore::start(&Arc<Self>, CancellationToken) -> ClusterHandle`; `ClusterHandle::shutdown().await` |
| `membership.generate_cluster_id` / `MembershipRecord` / `.to_dict()` / `.from_dict()` | `membership::generate_cluster_id` / `MembershipRecord` / `to_value()` / `from_value()` |
| `MembershipStore(store)` | `MembershipStore::attach(&Arc<ClusterStore>) -> Arc<MembershipStore>` (done by the builder) |
| `.cluster_id` / `.my_record()` / `._roster` | `cluster_id()` / `my_record()` / `roster_snapshot()` |
| `.merge(records)` / `.version_map()` / `.records_for(ids)` | `merge(&[Value]) -> bool` / `version_map()` / `records_for(&[String])` |
| `.reconcile_with(peer)` / `.push_to_roster(records)` | `async reconcile_with(&Peer)` / `async push_to_roster(Option<Vec<MembershipRecord>>)` |
| `.serve_set_digest` / `.serve_set_pull` / `.serve_set_sync` | same names (`SetSyncResponse` for sync) |
| `.set_add(members, token)` / `.set_remove(members)` / `.show()` | `async set_add(&[MemberSpec], Option<&str>) -> Value` / `async set_remove(&[MemberSpec]) -> Value` / `show() -> ShowResponse` |
| `.handle_join(body)` / `.adopt(cid, roster)` / `.leave_old(cid)` | `async handle_join(JoinRequest) -> JoinResponse` / `async adopt(cid, &[Value])` / `async leave_old(cid)` |
| `.handle_evict_self(body)` / `.handle_evicted(body)` | `async handle_evict_self(LeaveRequest) -> OkResponse` / `handle_evicted(EvictRequest) -> OkResponse` |
| `.desired_peers()` / `.is_removed_or_absent(id)` / `.reconcile_connections()` / `.gc_membership(now_ms)` | same names (`reconcile_connections` is async) |
| `router.router` (FastAPI) / `deps.require_cluster_store` (404) | `router(Arc<ClusterStore>) -> axum::Router` / `dormant_router() -> axum::Router` |
| `deps.get_cluster_store` / `set_cluster_store` | not ported; the backend holds `Option<Arc<ClusterStore>>` itself |
| `cli.build_parser()` / `cli.main(argv)` / `cmd_*` | `cli::ClusterCommand` (clap `Subcommand`) / `cli::run(cmd) -> i32` / `cli::execute(cmd) -> CliOutput` / `cli::cmd_*` |
| tests `helpers.FakeRegistry` / `InProcessTransport` / `FakeClock` / `build_store` / `converge` / `settle` / `visible` | `testing::FakeRegistry` / `InProcessTransport` / `FakeClock` / `TestNode` + `build_store` / `converge` / `settle` / `visible` |

## HTTP endpoints (`/api/cluster/*`)

Same paths, methods, query parameters and JSON bodies as `router.py`. Errors
use FastAPI's `{"detail": "..."}` shape. When the cluster module is not
initialised, mount `dormant_router()`: every path answers `404` with
`"Cluster module not initialized on this registry. Run 'a2x-registry cluster init' to enable distributed sync."`.

| Method and path | Purpose | Request | Response |
|---|---|---|---|
| `POST /api/cluster/peers` | Connect to a peer and reconcile (internal primitive; `add-peer`) | `{address, namespaces?, token?}` | `{"peer": {node_id, address, namespaces}}`; `502 {"detail": "peer unreachable: ..."}` |
| `GET /api/cluster/peers` | List sessions | | `{"peers": [...]}` |
| `DELETE /api/cluster/peers/{node_id}` | Drop a session and its replicas (`rm-peer`) | | `{node_id, removed}` |
| `POST /api/cluster/sessions` | OPEN handshake with per-namespace authorization | `{node_id, address?, namespaces?, token?}` | `{node_id, accepted, ephemeral, session_token}` |
| `GET /api/cluster/merkle?from_node&namespaces` | Bucket hashes (anti-entropy fast path) | header `X-Cluster-Session` | `{"<bucket>": "<sha256 hex>"}` |
| `GET /api/cluster/digest?from_node&namespaces&buckets` | Version rows, optionally only for some buckets | header `X-Cluster-Session` | `[[dataset, origin_id, service_id, [ms, node_id]], ...]` |
| `POST /api/cluster/pulls` | Full envelopes by key | `{from_node, keys: [[ds, origin, sid]]}` | `[SyncEnvelope, ...]` |
| `POST /api/cluster/updates` | Inbound delta (LWW dedup, namespace gated, no relay) | `{from_node, envelopes: [SyncEnvelope]}` | `{accepted, received, rejected}` |
| `POST /api/cluster/keepalives` | Refresh the HOLD timer | `{from_node}` | `{"ok": bool}` |
| `POST /api/cluster/set/add` | Declaratively add members (mints a cluster id on first use) | `{members: [{address}], token?}` | `{cluster_id, results: [{address, node_id?, ok, error?}]}` |
| `POST /api/cluster/set/remove` | Deterministic removal (tombstone + evict) | `{members: [{node_id}]}` | `{results: [{node_id, ok} or {ok: false, error}]}` |
| `GET /api/cluster/set` | Cluster id and roster with liveness | | `{cluster_id, node_id, roster: [{node_id, address, alive}]}` |
| `POST /api/cluster/join` | Being pulled into a cluster (admin token when auth is on) | `{cluster_id, roster, token?, from_node, from_address}` | `{accepted, node_id, cluster_id, version}` or `{accepted: false, error}` |
| `POST /api/cluster/evicted` | Removed from the cluster: revert to standalone | `{from_node, cluster_id?, token?}` | `{"ok": bool}` |
| `POST /api/cluster/leave` | A peer leaves gracefully: tombstone and drop it | `{from_node, token?}` | `{"ok": bool}` |
| `GET /api/cluster/set/digest?from_node` | Roster version map | header `X-Cluster-Session` | `{"<node_id>": [ms, node_id]}` |
| `POST /api/cluster/set/pull` | Full membership records | `{from_node, node_ids}` | `[MembershipRecord, ...]` |
| `POST /api/cluster/set/sync` | Pushed membership batch (LWW merge) | `{from_node, records}` | `{"accepted": bool}` |
| `GET /api/cluster/state` | Sync state snapshot | | `{node_id, advertise, peers, foreign_records, foreign_by_namespace, local_records, tombstones}` |

`SyncEnvelope` on the wire: `{dataset, service_id, origin_id, version: [ms, node_id], tombstone, payload: {entry, wrapped} | null}`.
`MembershipRecord`: `{node_id, cluster_id | null, address, version: [ms, node_id], removed}`.

## Integration contract for the backend

```rust,ignore
// AgentRegistry/rust-sdk/a2x-cluster/src/registry_view.rs

pub struct LocalEntry {
    pub service_id: String,
    pub source: String,            // "ephemeral" entries are never replicated
    pub entry: serde_json::Value,  // RegistryEntry as JSON (payload.entry)
    pub wrapped: Option<serde_json::Value>, // list row (payload.wrapped)
}

pub trait LocalRegistryView: Send + Sync {
    fn list_datasets(&self) -> Vec<String>;
    fn list_entries(&self, dataset: &str) -> Vec<LocalEntry>;
    fn get_entry(&self, dataset: &str, service_id: &str) -> Option<LocalEntry>; // default: scan list_entries
    fn is_auth_required(&self, dataset: &str) -> bool;
}

pub trait PeerAuthVerifier: Send + Sync {
    fn auth_enabled(&self) -> bool;                                    // false == Python `auth_store is None`
    fn authenticate(&self, token: &str) -> Option<a2x_common::AuthContext>;
}
pub struct AllowAll;               // auth_enabled() == false

pub trait EvictionSink: Send + Sync {
    fn on_evicted(&self, origin_id: &str, evicted: &[(String, String)]); // (dataset, service_id)
}
// any `Fn(&str, &[(String, String)]) + Send + Sync` implements EvictionSink.
```

```rust,ignore
// AgentRegistry/rust-sdk/a2x-cluster/src/store.rs (hooks the backend calls)

impl ClusterStoreBuilder {
    pub fn config(self, config: ClusterConfig) -> Self;
    pub fn registry(self, registry: Arc<dyn LocalRegistryView>) -> Self;
    pub fn transport(self, transport: Arc<dyn Transport>) -> Self;   // default HttpTransport::new(http_timeout)
    pub fn advertise(self, advertise: impl Into<String>) -> Self;
    pub fn auth(self, auth: Arc<dyn PeerAuthVerifier>) -> Self;       // default AllowAll
    pub fn clock(self, clock: Clock) -> Self;                         // Clock = Arc<dyn Fn() -> f64 + Send + Sync>
    pub fn eviction_sink(self, sink: Arc<dyn EvictionSink>) -> Self;
    pub fn membership(self, enabled: bool) -> Self;                   // default true
    pub fn build(self, state: ClusterState) -> Arc<ClusterStore>;
    pub fn load_or_none(self) -> Option<Arc<ClusterStore>>;           // reads state_path()
    pub fn load_or_none_at(self, path: &Path) -> Option<Arc<ClusterStore>>;
}

impl ClusterStore {
    pub async fn on_local_upsert(&self, dataset: &str, service_id: &str, entry: Value, wrapped: Option<Value>) -> Result<(), ClusterError>;
    pub async fn on_local_delete(&self, dataset: &str, service_id: &str) -> Result<(), ClusterError>;
    pub async fn on_local_mutation(&self, dataset: &str, service_id: &str, op: MutationOp) -> Result<(), ClusterError>; // payload read back through the view
    pub fn foreign_rows(&self, dataset: &str) -> Vec<ForeignRow>;     // ForeignRow { entry: Value, wrapped: Value }
    pub fn foreign_wrapped(&self, dataset: &str) -> Vec<Value>;       // wrapped rows only
    pub fn foreign_entry(&self, dataset: &str, display_id: &str) -> Option<Value>; // "origin_id:service_id"
    pub fn status(&self) -> StateSummary;                             // alias of state_summary()
    pub fn start(self: &Arc<Self>, cancel: CancellationToken) -> ClusterHandle; // spawns both sweepers
    pub fn close(&self);                                              // release transport pool
}
impl ClusterHandle {
    pub async fn shutdown(self);                                      // cancel + join the sweepers
}

pub fn router(store: Arc<ClusterStore>) -> axum::Router;              // mounts /api/cluster/*
pub fn dormant_router() -> axum::Router;                              // 404 for /api/cluster/*
```

The backend should call `on_local_upsert` / `on_local_delete` after every
successful local register, update and deregister, exactly where the Python
`RegistryService.set_on_mutation` hook fired. The list endpoint appends
`foreign_rows(dataset)` (with `"source": "cluster"`) after the local rows and
the single-get endpoint falls back to `foreign_entry` for ids containing `:`.

## `cluster_state.json`

Map keys are `"{dataset}\u0000{service_id}"` (NUL separated, as in Python):

```json
{
  "node_id": "reg-99f79b5f9d04",
  "version_clock": 0,
  "local_versions": {"dataset\u0000service_id": [1710000000000, "reg-99f79b5f9d04"]},
  "tombstones": {"dataset\u0000service_id": {"version": [1710000001000, "reg-99f79b5f9d04"], "deleted_at_ms": 1710000001000}},
  "cluster_id": null,
  "last_roster": [],
  "my_membership_version": null
}
```

Location: `A2X_REGISTRY_CLUSTER_STATE` when set, otherwise
`<A2X_REGISTRY_HOME>/cluster_state.json` (resolved by `a2x_common::paths::get_home`).
A file written before the membership feature loads with standalone defaults.

## Configuration

| Field | Default | Environment variable |
|---|---|---|
| `keepalive_interval` | 10 s | `A2X_REGISTRY_CLUSTER_KEEPALIVE_INTERVAL` |
| `hold_timeout` | 30 s | `A2X_REGISTRY_CLUSTER_HOLD_TIMEOUT` |
| `anti_entropy_interval` | 20 s | `A2X_REGISTRY_CLUSTER_ANTI_ENTROPY_INTERVAL` |
| `http_timeout` | 5 s | `A2X_REGISTRY_CLUSTER_HTTP_TIMEOUT` |
| `broadcast_workers` | 32 | `A2X_REGISTRY_CLUSTER_BROADCAST_WORKERS` |
| `merkle_buckets` | 256 (must match cluster-wide) | `A2X_REGISTRY_CLUSTER_MERKLE_BUCKETS` |
| advertise URL | empty | `A2X_REGISTRY_CLUSTER_ADVERTISE` (read by the backend, `config::ENV_ADVERTISE`) |

Derived: `tombstone_retention() = hold_timeout + keepalive_interval` (40 s).
Invalid values log a warning and keep the default; integer knobs accept
`"10"` or `"10.0"`.

## CLI

`cli::ClusterCommand` is a clap `Subcommand` the `a2x-registry` binary embeds:

```
a2x-registry cluster init [--node-id ID]
a2x-registry cluster status [--server URL]
a2x-registry cluster add-peer <address> [--namespaces a,b] [--token T] [--server URL]
a2x-registry cluster rm-peer <node_id> [--server URL]
a2x-registry cluster set add <address>... [--token T] [--server URL]
a2x-registry cluster set remove <node_id>... [--server URL]
a2x-registry cluster set show [--server URL]
```

`--server` defaults to `http://127.0.0.1:8000`. `cli::run(cmd).await` prints the
result and returns the exit code (`0`, or `1` on error / not initialised).
`cli::execute(cmd).await` returns a `CliOutput { text, code }` without printing.

A standalone binary is included for testing the module without the registry:

```
a2x-cluster init --node-id A
A2X_REGISTRY_CLUSTER_ADVERTISE=http://127.0.0.1:8001 a2x-cluster serve --bind 127.0.0.1:8001
a2x-cluster set add http://127.0.0.1:8002 --server http://127.0.0.1:8001
a2x-cluster status --server http://127.0.0.1:8001
```

`serve` runs an in-memory node with no local records; it accepts peers,
replicates their services and reports them under `/api/cluster/state`.

## Tests

`cargo test -p a2x-cluster` runs 19 unit tests and 87 integration tests. All
23 original test files are covered:

| Original | Rust |
|---|---|
| `conftest.py`, `helpers.py` | `src/testing.rs` (public harness) |
| `test_antientropy.py`, `test_replication.py`, `test_tombstone.py` | `tests/replication.rs` |
| `test_auth_handshake.py` | unit tests in `src/auth_handshake.rs` |
| `test_auth_sync.py` | `tests/auth_sync.rs` |
| `test_config.py` | `tests/config_env.rs` |
| `test_forward_compat.py`, `test_router.py`, `test_read_merge.py`, `test_mutation_hook.py` | `tests/router_http.rs` (one axum server, real HTTP) |
| `test_identity.py` | unit tests in `src/state.rs`, `tests/identity.rs` |
| `test_integration_http.py` | `tests/integration_http.rs` (two and three in-process axum servers over `HttpTransport`) |
| `test_lease.py` | `tests/lease.rs` (exercises `a2x_common::LeaseTable`) |
| `test_liveness.py`, `test_topologies.py` | `tests/liveness.rs` |
| `test_membership.py` | `tests/membership.rs` |
| `test_reconcile.py`, `test_session.py`, `test_session_token.py` | `tests/session.rs` |
| `test_scale.py` | `tests/scale.rs` |

Multi-node tests bind `127.0.0.1:0`; liveness tests drive a `FakeClock` with
explicit `now` values, so no test sleeps longer than the 200 ms concurrency
check.

## Deviations and intentionally left out

- `deps.py` (module singleton `get_cluster_store` / `set_cluster_store`) is not
  ported. The backend owns `Option<Arc<ClusterStore>>` and mounts either
  `router` or `dormant_router`.
- The Python store reads the registry through duck typing (`RegistryService`
  and `AuthStore`). The port defines `LocalRegistryView`, `PeerAuthVerifier`
  and `EvictionSink` instead; `foreign_rows` returns the replicated `entry` as
  raw JSON rather than a validated `RegistryEntry`, and skips a replica whose
  payload has no `wrapped` object (Python would raise on it).
- Orchestration methods are `async` (`connect_peer`, `reconcile`,
  `emit_keepalive`, `on_local_*`, membership pushes). Handlers stay
  synchronous. Fan-out uses a bounded concurrent stream instead of a thread
  pool; `broadcast_workers` still caps concurrency (minimum 4).
- The sweepers are tokio tasks stopped through a `CancellationToken`
  (`ClusterStore::start` / `ClusterHandle::shutdown`) instead of daemon
  threads with `start()` / `stop()`.
- Request validation errors answer `422` (axum JSON rejection) for bodies and
  `422 {"detail": ...}` for a missing `from_node` query parameter, close to
  FastAPI's `422`. Digest rows are returned sorted (the Python order is dict
  insertion order); the wire format is unchanged.
- `HttpTransport::close()` marks the client closed (later calls fail with
  `transport closed`); reqwest has no explicit pool close.
- `a2x_common::LeaseTable` is not used for peer HOLD timers because the original
  cluster module does not use it either: `Peer.last_seen` plus `hold_timeout`
  is ported as is (fractional timeouts stay exact). `test_lease.py` is covered
  by `tests/lease.rs` against the shared crate.
- The two-process subprocess smoke test (`test_integration_http.py`) is ported
  as in-process axum servers, since the Python backend binary is not part of
  this crate.
