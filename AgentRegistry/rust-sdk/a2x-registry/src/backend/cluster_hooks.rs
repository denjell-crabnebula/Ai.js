// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Cluster integration points consumed by the dataset router.
//!
//! The replication engine lives in the `a2x-cluster` crate. The backend
//! only needs to merge replicated (foreign) rows into list responses and to
//! resolve a namespaced id on the single service endpoint.

use serde_json::Value;

use crate::register::RegistryEntry;

/// A replicated service record from a peer registry.
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignRow {
    /// Entry used for filter matching (same rules as local entries).
    pub entry: RegistryEntry,
    /// Wrapped `service.json` shaped row with the namespaced id.
    pub wrapped: Value,
}

/// Read side hooks of the cluster module.
pub trait ClusterHooks: Send + Sync {
    /// Foreign rows for `dataset`, merged after local entries.
    fn foreign_rows(&self, dataset: &str) -> Vec<ForeignRow>;

    /// Resolve a namespaced id (`origin_id:service_id`) to its wrapped row.
    fn foreign_entry(&self, dataset: &str, service_id: &str) -> Option<Value>;
}
