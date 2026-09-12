// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Crate error type.

use std::path::PathBuf;

use crate::transport::TransportError;

/// Errors raised by the cluster module outside the transport layer.
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// `cluster init` was run on a node that already has a state file.
    #[error("Cluster already initialized at {0}. Delete it to re-init.")]
    AlreadyInitialized(PathBuf),

    /// Reading or writing `cluster_state.json` failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `cluster_state.json` is not valid JSON or lacks required fields.
    #[error("invalid cluster state in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// A peer call failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl ClusterError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ClusterError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        ClusterError::Json {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, ClusterError>;
