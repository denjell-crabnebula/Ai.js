// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Distributed sync module for the A2X registry (port of `a2x_registry.cluster`).
//!
//! Opt-in: a registry instance runs standalone until `a2x-registry cluster init`
//! creates `cluster_state.json`. Once initialised, instances connect into a
//! full mesh and replicate their flat registries so a query to any member
//! returns every member's services. A member that goes silent is dropped by
//! its peers' HOLD timers, which evict its records.
//!
//! Design model (see `docs/cluster_design.md` in the original repository):
//!
//! - full mesh: each member holds a direct session with every other member,
//!   maintained by the declarative membership control plane ([`membership`]).
//! - AP / eventually consistent: a local CRUD broadcasts directly to all peers
//!   (no relay); periodic Merkle anti-entropy heals dropped pushes.
//! - LWW versioning with origin-only writes; foreign records are read-only,
//!   memory-only replicas.
//! - liveness is direct keepalive / HOLD; a HOLD-evicted origin is suppressed
//!   briefly so anti-entropy cannot resurrect it before every peer evicts.
//!
//! The crate does not depend on the registry crate. The backend implements
//! [`LocalRegistryView`] (read access to local records), [`PeerAuthVerifier`]
//! (token verification during the OPEN handshake) and optionally
//! [`EvictionSink`], and mounts [`router()`](crate::router()) under `/api/cluster`.

pub mod auth_handshake;
pub mod cli;
pub mod config;
pub mod envelope;
pub mod errors;
pub mod membership;
pub mod merkle;
pub mod peer;
pub mod registry_view;
pub mod router;
pub mod state;
pub mod store;
pub mod sweepers;
pub mod testing;
pub mod transport;

pub use config::ClusterConfig;
pub use envelope::{Key, SyncEnvelope, Version, version_newer};
pub use errors::ClusterError;
pub use membership::{MembershipRecord, MembershipStore};
pub use peer::Peer;
pub use registry_view::{AllowAll, EvictionSink, LocalEntry, LocalRegistryView, PeerAuthVerifier};
pub use router::{dormant_router, router};
pub use state::{ClusterState, Tombstone};
pub use store::{ClusterHandle, ClusterStore, ClusterStoreBuilder, MutationOp, ReconcileResult};
pub use sweepers::{AntiEntropySweeper, KeepaliveMonitor};
pub use transport::{HttpTransport, SESSION_HEADER, Transport, TransportError};

/// The environment variables this crate reads, with the values a safe
/// deployment accepts. Merge it into a binary's [`ap_support::env::EnvPolicy`].
pub fn env_policy() -> ap_support::env::EnvPolicy {
    use ap_support::env::{EnvPolicy, Kind, Rule, VarSpec};
    let seconds = |name: &'static str, description: &'static str, default: &'static str| {
        VarSpec::new(name, Kind::Float, description)
            .default(default)
            .rule(Rule::float_range(0.1, 86400.0))
    };
    EnvPolicy::new()
        .prefix("A2X_REGISTRY_CLUSTER_")
        .var(
            VarSpec::new(
                config::ENV_ADVERTISE,
                Kind::Url,
                "URL peers use to reach this node",
            )
            .rule(Rule::https_remote().from(ap_support::env::Lockdown::Locked)),
        )
        .var(
            VarSpec::new(state::ENV_STATE_PATH, Kind::Path, "Path of cluster_state.json")
                .rule(Rule::safe_path()),
        )
        .var(seconds(
            "A2X_REGISTRY_CLUSTER_KEEPALIVE_INTERVAL",
            "Gossip keepalive period in seconds",
            "10",
        ))
        .var(seconds(
            "A2X_REGISTRY_CLUSTER_HOLD_TIMEOUT",
            "Seconds before an unresponsive peer is dropped",
            "30",
        ))
        .var(seconds(
            "A2X_REGISTRY_CLUSTER_ANTI_ENTROPY_INTERVAL",
            "Merkle reconciliation period in seconds",
            "20",
        ))
        .var(seconds(
            "A2X_REGISTRY_CLUSTER_HTTP_TIMEOUT",
            "Peer request timeout in seconds",
            "5",
        ))
        .var(
            VarSpec::new(
                "A2X_REGISTRY_CLUSTER_BROADCAST_WORKERS",
                Kind::Integer,
                "Parallel broadcast fan-out",
            )
            .default("32")
            .rule(Rule::int_range(1, 1024)),
        )
        .var(
            VarSpec::new(
                "A2X_REGISTRY_CLUSTER_MERKLE_BUCKETS",
                Kind::Integer,
                "Merkle tree buckets, equal across the cluster",
            )
            .default("256")
            .rule(Rule::int_range(1, 65536)),
        )
}
