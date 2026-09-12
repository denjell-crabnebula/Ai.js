// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Crate error type.

use std::path::PathBuf;

use a2x_common::A2xError;

/// Errors raised by build, search, vector and evaluation code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The build was cancelled through its `CancellationToken`.
    #[error("Build cancelled by user")]
    Cancelled,

    /// An LLM call that the algorithm cannot recover from failed.
    #[error("{0}")]
    Llm(String),

    /// A caller passed an invalid mode or option.
    #[error("{0}")]
    Invalid(String),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Error from the shared `a2x-common` crate (LLM config, features).
    #[error(transparent)]
    Common(#[from] A2xError),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Error::Json {
            path: path.into(),
            source,
        }
    }

    /// True when the error is a user cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Cancelled)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
