// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Integration contract between the cluster module and the registry backend.
//!
//! The cluster crate never depends on the registry crate. The backend
//! implements these traits and hands them to [`crate::ClusterStoreBuilder`]:
//!
//! - [`LocalRegistryView`]: read-only access to local records, used for
//!   anti-entropy digests, pulls and the initial sync after a handshake.
//! - [`PeerAuthVerifier`]: verifies a namespace token during the OPEN
//!   handshake and `join`, mirroring how the Python module reuses the auth
//!   store. [`AllowAll`] models a registry with auth disabled.
//! - [`EvictionSink`]: optional callback fired when foreign records of an
//!   origin are evicted (session drop, HOLD timeout, membership removal).

use std::sync::Arc;

use a2x_common::AuthContext;
use serde_json::Value;

/// One local-origin record as the cluster module needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalEntry {
    pub service_id: String,
    /// `RegistryEntry.source`; `"ephemeral"` entries are never replicated.
    pub source: String,
    /// The `RegistryEntry` as JSON (the `entry` half of the payload).
    pub entry: Value,
    /// The list-endpoint row as JSON (the `wrapped` half of the payload).
    pub wrapped: Option<Value>,
}

impl LocalEntry {
    pub fn is_ephemeral(&self) -> bool {
        self.source == "ephemeral"
    }
}

/// Read-only view of the local registry.
pub trait LocalRegistryView: Send + Sync {
    /// Names of all local datasets (namespaces).
    fn list_datasets(&self) -> Vec<String>;

    /// All local entries of `dataset`, each with its wrapped list row.
    fn list_entries(&self, dataset: &str) -> Vec<LocalEntry>;

    /// One local entry, or `None` when it does not exist.
    fn get_entry(&self, dataset: &str, service_id: &str) -> Option<LocalEntry> {
        self.list_entries(dataset)
            .into_iter()
            .find(|e| e.service_id == service_id)
    }

    /// Whether `dataset` requires authentication for writes and replication.
    fn is_auth_required(&self, dataset: &str) -> bool;
}

impl<T: LocalRegistryView + ?Sized> LocalRegistryView for Arc<T> {
    fn list_datasets(&self) -> Vec<String> {
        (**self).list_datasets()
    }
    fn list_entries(&self, dataset: &str) -> Vec<LocalEntry> {
        (**self).list_entries(dataset)
    }
    fn get_entry(&self, dataset: &str, service_id: &str) -> Option<LocalEntry> {
        (**self).get_entry(dataset, service_id)
    }
    fn is_auth_required(&self, dataset: &str) -> bool {
        (**self).is_auth_required(dataset)
    }
}

/// Token verification for the handshake and membership `join`.
///
/// The Python module reads the auth store dynamically on every call, so the
/// answer of [`PeerAuthVerifier::auth_enabled`] may change at runtime.
pub trait PeerAuthVerifier: Send + Sync {
    /// `false` models `auth_store is None`: an open cluster with no token
    /// system at all.
    fn auth_enabled(&self) -> bool;

    /// Resolve a namespace token to a caller context. `None` on any failure.
    fn authenticate(&self, token: &str) -> Option<AuthContext>;
}

impl<T: PeerAuthVerifier + ?Sized> PeerAuthVerifier for Arc<T> {
    fn auth_enabled(&self) -> bool {
        (**self).auth_enabled()
    }
    fn authenticate(&self, token: &str) -> Option<AuthContext> {
        (**self).authenticate(token)
    }
}

/// Verifier for deployments without auth: every namespace syncs, no session
/// tokens are issued.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl PeerAuthVerifier for AllowAll {
    fn auth_enabled(&self) -> bool {
        false
    }
    fn authenticate(&self, _token: &str) -> Option<AuthContext> {
        None
    }
}

/// Notified when foreign records are evicted from the overlay.
pub trait EvictionSink: Send + Sync {
    /// `origin_id` lost its session; `evicted` lists `(dataset, service_id)`
    /// of every replica that was dropped (tombstones included).
    fn on_evicted(&self, origin_id: &str, evicted: &[(String, String)]);
}

impl<F> EvictionSink for F
where
    F: Fn(&str, &[(String, String)]) + Send + Sync,
{
    fn on_evicted(&self, origin_id: &str, evicted: &[(String, String)]) {
        self(origin_id, evicted)
    }
}
