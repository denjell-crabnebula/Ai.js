// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! User-facing errors for missing or unavailable subsystems.
//!
//! Each variant carries an actionable message. CLI and HTTP handlers can
//! print `err.to_string()` directly to surface remediation steps.

use std::path::PathBuf;

/// Library-level user-facing error.
#[derive(Debug, thiserror::Error)]
pub enum A2xError {
    /// Embedding backend failed to load or is not configured.
    #[error("{0}")]
    VectorSearchUnavailable(String),

    /// LLM API key config is missing or invalid; A2X build/search unavailable.
    #[error("{0}")]
    LlmNotConfigured(String),

    /// A feature was requested but is not available in this deployment.
    /// `feature` is the logical capability name, `extras` the enablement hint.
    #[error("This endpoint requires '{feature}', which is not available. {hint}")]
    FeatureNotInstalled {
        feature: String,
        extras: String,
        hint: String,
    },

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

    #[error("{0}")]
    Other(String),
}

impl A2xError {
    pub fn feature_not_installed(feature: &str, extras: &str) -> Self {
        A2xError::FeatureNotInstalled {
            feature: feature.to_string(),
            extras: extras.to_string(),
            hint: format!(
                "Enable the '{extras}' feature (configure the backend for it) and restart the registry."
            ),
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        A2xError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        A2xError::Json {
            path: path.into(),
            source,
        }
    }

    /// Structured body used by HTTP handlers for 503 responses.
    pub fn feature_body(&self) -> Option<serde_json::Value> {
        match self {
            A2xError::FeatureNotInstalled { feature, extras, .. } => Some(serde_json::json!({
                "feature": feature,
                "extras": extras,
                "detail": self.to_string(),
            })),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, A2xError>;
