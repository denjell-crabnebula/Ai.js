// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Peer/session model.
//!
//! A [`Peer`] is an established sync session with another instance: its node
//! id, the base address to reach it, and the set of namespaces both sides
//! agreed to sync. Sessions are keyed by peer node id in the store, so
//! re-handshaking the same peer updates rather than duplicates the session.

use std::collections::BTreeSet;

use serde::Serialize;

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub node_id: String,
    pub address: String,
    pub namespaces: BTreeSet<String>,
    /// Monotonic timestamp (seconds) of the last inbound contact from this
    /// peer; drives the direct-link HOLD timer.
    pub last_seen: f64,
    /// Shared per-session secret, established at handshake only when the
    /// receiver has auth enabled. `None` on open clusters.
    pub token: Option<String>,
}

/// Wire summary of a session, as returned by `/api/cluster/state`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PeerSummary {
    pub node_id: String,
    pub address: String,
    pub namespaces: Vec<String>,
}

impl Peer {
    pub fn new(node_id: impl Into<String>, address: impl Into<String>, namespaces: BTreeSet<String>) -> Self {
        Peer {
            node_id: node_id.into(),
            address: address.into(),
            namespaces,
            last_seen: 0.0,
            token: None,
        }
    }

    pub fn to_summary(&self) -> PeerSummary {
        PeerSummary {
            node_id: self.node_id.clone(),
            address: self.address.clone(),
            namespaces: self.namespaces.iter().cloned().collect(),
        }
    }
}
