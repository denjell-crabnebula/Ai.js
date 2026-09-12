// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Registration domain errors.
//!
//! The service layer carries no HTTP semantics. The backend maps each
//! variant to a status code the same way the FastAPI router did:
//! `NotFound` and `FileNotFound` become 404, `Invalid` becomes 400,
//! `Permission` becomes 403 and everything else becomes 500.

/// Error returned by [`crate::register::RegistryService`] and the store.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// A dataset, service or skill does not exist (`RegistryNotFoundError`).
    #[error("{0}")]
    NotFound(String),
    /// Validation failure or a forbidden source (`ValueError`).
    #[error("{0}")]
    Invalid(String),
    /// Caller lacks owner or admin rights (`PermissionError`).
    #[error("{0}")]
    Permission(String),
    /// A file or folder is missing on disk (`FileNotFoundError`).
    #[error("{0}")]
    FileNotFound(String),
    /// Fetching a remote agent card failed.
    #[error("{0}")]
    Fetch(String),
    /// Unexpected I/O failure.
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            RegistryError::FileNotFound(e.to_string())
        } else {
            RegistryError::Io(e.to_string())
        }
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;
