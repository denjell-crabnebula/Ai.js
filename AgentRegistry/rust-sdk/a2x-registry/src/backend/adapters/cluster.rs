// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Adapter from the `a2x-cluster` crate (feature `cluster`): the registry
//! view and auth verifier the cluster store reads through, the mutation
//! hook that broadcasts local CRUD, foreign row merging for the dataset
//! router, the `/api/cluster/*` router and the CLI subcommands.

use std::sync::Arc;

use a2x_cluster::{ClusterConfig, ClusterStore, LocalEntry, LocalRegistryView, PeerAuthVerifier};
use serde_json::Value;

use crate::backend::cluster_hooks::{ClusterHooks, ForeignRow};
use crate::backend::state::AppState;
use crate::register::service::entry_to_output;
use crate::register::{MutationHook, MutationOp, RegistryEntry, RegistryService};

/// Read side view over the local registry.
pub struct RegistryView(pub Arc<RegistryService>);

fn local_entry(entry: &RegistryEntry) -> LocalEntry {
    LocalEntry {
        service_id: entry.service_id.clone(),
        source: entry.source.as_str().to_string(),
        entry: serde_json::to_value(entry).unwrap_or(Value::Null),
        wrapped: Some(Value::Object(entry_to_output(entry))),
    }
}

impl LocalRegistryView for RegistryView {
    fn list_datasets(&self) -> Vec<String> {
        self.0.list_datasets()
    }

    fn list_entries(&self, dataset: &str) -> Vec<LocalEntry> {
        self.0.list_entries(dataset).iter().map(local_entry).collect()
    }

    fn get_entry(&self, dataset: &str, service_id: &str) -> Option<LocalEntry> {
        self.0.get_entry(dataset, service_id).map(|e| local_entry(&e))
    }

    fn is_auth_required(&self, dataset: &str) -> bool {
        self.0.is_auth_required(dataset)
    }
}

/// Token verification for the peer handshake, backed by the auth store.
pub struct AuthVerifier(pub Arc<AppState>);

impl PeerAuthVerifier for AuthVerifier {
    fn auth_enabled(&self) -> bool {
        self.0.auth_store().is_some()
    }

    fn authenticate(&self, token: &str) -> Option<a2x_common::AuthContext> {
        self.0.auth_store()?.authenticate(token).ok()
    }
}

/// Broadcasts local CRUD to peers (installed as the registry mutation hook).
pub struct ReplicationHook {
    store: Arc<ClusterStore>,
    runtime: tokio::runtime::Handle,
}

impl MutationHook for ReplicationHook {
    fn on_mutation(&self, dataset: &str, service_id: &str, op: MutationOp, entry: Option<&RegistryEntry>) {
        let store = self.store.clone();
        let (dataset, service_id) = (dataset.to_string(), service_id.to_string());
        let payload = entry.map(|e| {
            (
                serde_json::to_value(e).unwrap_or(Value::Null),
                Value::Object(entry_to_output(e)),
            )
        });
        self.runtime.spawn(async move {
            let result = match (op, payload) {
                (MutationOp::Deregister, _) | (_, None) => store.on_local_delete(&dataset, &service_id).await,
                (_, Some((entry, wrapped))) => {
                    store
                        .on_local_upsert(&dataset, &service_id, entry, Some(wrapped))
                        .await
                }
            };
            if let Err(e) = result {
                tracing::warn!(
                    "cluster: on_mutation hook failed ({}, {}, {}): {}",
                    dataset,
                    service_id,
                    op.as_str(),
                    e
                );
            }
        });
    }
}

/// Foreign row reads for the dataset router.
pub struct ClusterReads(pub Arc<ClusterStore>);

impl ClusterHooks for ClusterReads {
    fn foreign_rows(&self, dataset: &str) -> Vec<ForeignRow> {
        self.0
            .foreign_rows(dataset)
            .into_iter()
            .filter_map(|row| {
                serde_json::from_value::<RegistryEntry>(row.entry)
                    .ok()
                    .map(|entry| ForeignRow {
                        entry,
                        wrapped: row.wrapped,
                    })
            })
            .collect()
    }

    fn foreign_entry(&self, dataset: &str, service_id: &str) -> Option<Value> {
        self.0.foreign_entry(dataset, service_id)
    }
}

/// Resolve `cluster_state.json`: `A2X_REGISTRY_CLUSTER_STATE` when set,
/// otherwise under the configured home.
pub fn state_path(state: &AppState) -> std::path::PathBuf {
    match ap_support::env::current().get_non_blank(a2x_cluster::state::ENV_STATE_PATH) {
        Some(_) => a2x_cluster::state::state_path(),
        None => state.config_home.join("cluster_state.json"),
    }
}

/// Initialize the cluster module during warmup. Dormant when
/// `cluster_state.json` is absent.
pub async fn init(state: &Arc<AppState>) {
    let config = ClusterConfig::from_env();
    let advertise = ap_support::env::current()
        .get(a2x_cluster::config::ENV_ADVERTISE)
        .unwrap_or_default();
    let view: Arc<dyn LocalRegistryView> = Arc::new(RegistryView(state.registry.clone()));
    let verifier: Arc<dyn PeerAuthVerifier> = Arc::new(AuthVerifier(state.clone()));
    let store = ClusterStore::builder()
        .config(config)
        .registry(view)
        .auth(verifier)
        .advertise(advertise)
        .load_or_none_at(&state_path(state));
    let Some(store) = store else {
        tracing::info!("  Cluster module not initialized (standalone)");
        return;
    };
    state.registry.set_on_mutation(Some(Arc::new(ReplicationHook {
        store: store.clone(),
        runtime: tokio::runtime::Handle::current(),
    })));
    state.set_cluster(Some(Arc::new(ClusterReads(store.clone()))));
    state.set_cluster_router(Some(a2x_cluster::router(store.clone())));
    let handle = store.start(tokio_util::sync::CancellationToken::new());
    let token = handle.cancellation_token();
    let closer = store.clone();
    state.register_shutdown(Box::new(move || {
        token.cancel();
        closer.close();
    }));
    tracing::info!("  Cluster module loaded (node_id={})", store.node_id());
}

/// `a2x-registry cluster ...` entry point.
pub async fn cli(cmd: a2x_cluster::cli::ClusterCommand) -> i32 {
    a2x_cluster::cli::run(cmd).await
}
