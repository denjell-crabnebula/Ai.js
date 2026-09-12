// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Stable A4P protocol errors shared by services and transports.

use thiserror::Error;

/// A fail-closed protocol error with a stable code and HTTP status.
///
/// This mirrors the Python `A4PProtocolError`. The HTTP server maps it to
/// `http_status` with a JSON body of `{"error": code, "message": message}`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct A4PProtocolError {
    /// Human readable message.
    pub message: String,
    /// Stable protocol code such as `CREDENTIAL_KEY_CONFLICT`.
    pub code: String,
    /// HTTP status returned by the HTTP transport.
    pub http_status: u16,
}

impl A4PProtocolError {
    /// Create a protocol error with an explicit code and status.
    pub fn new(message: impl Into<String>, code: impl Into<String>, http_status: u16) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            http_status,
        }
    }

    /// The registration endpoint does not match the configured signature method.
    pub fn signature_method_not_enabled(expected: Option<&str>, requested: &str) -> Self {
        let expected_text = match expected {
            Some(value) => format!("'{value}'"),
            None => "None".to_string(),
        };
        Self::new(
            format!("Signature method '{requested}' is not enabled; configured method is {expected_text}"),
            "SIGNATURE_METHOD_NOT_ENABLED",
            409,
        )
    }

    /// The same public key is already registered to another user.
    pub fn credential_key_conflict() -> Self {
        Self::new(
            "The public key is already registered to another user",
            "CREDENTIAL_KEY_CONFLICT",
            409,
        )
    }

    /// The user has no credential for the active signature method.
    pub fn user_credential_not_registered(user_id: &str, signature_method: &str) -> Self {
        Self::new(
            format!("No '{signature_method}' credential is registered for user: {user_id}"),
            "USER_CREDENTIAL_NOT_REGISTERED",
            400,
        )
    }

    /// True when this is the `USER_CREDENTIAL_NOT_REGISTERED` error.
    pub fn is_user_credential_not_registered(&self) -> bool {
        self.code == "USER_CREDENTIAL_NOT_REGISTERED"
    }
}

/// A fail-closed local mandate verification error with a stable code.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct MandateSecurityError {
    /// Stable code such as `SERVER_KEY_UNTRUSTED`.
    pub code: String,
    /// Human readable message.
    pub message: String,
}

impl MandateSecurityError {
    /// Create a mandate security error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Raised when a persisted credential file uses an unsupported schema.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct CredentialStoreFormatError(pub String);

/// Raised when token usage cannot be consumed safely.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct IntentTokenUsageStoreError(pub String);

/// The crate wide error type.
///
/// `Value` corresponds to a Python `ValueError` (HTTP 400), `Runtime` to a
/// Python `RuntimeError` (HTTP 500) and `Protocol` to `A4PProtocolError`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum A4PError {
    /// A stable protocol error with its own HTTP status.
    #[error("{0}")]
    Protocol(#[from] A4PProtocolError),
    /// A local mandate verification failure.
    #[error("{0}")]
    MandateSecurity(#[from] MandateSecurityError),
    /// An unsupported credential store file format.
    #[error("{0}")]
    CredentialStoreFormat(#[from] CredentialStoreFormatError),
    /// A usage store failure.
    #[error("{0}")]
    UsageStore(#[from] IntentTokenUsageStoreError),
    /// Invalid input, the equivalent of a Python `ValueError`.
    #[error("{0}")]
    Value(String),
    /// An unrecoverable runtime failure, the equivalent of a Python `RuntimeError`.
    #[error("{0}")]
    Runtime(String),
}

impl A4PError {
    /// Build a `Value` error.
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }

    /// Build a `Runtime` error.
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime(message.into())
    }

    /// Return the stable protocol code when this is a protocol error.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Protocol(error) => Some(&error.code),
            Self::MandateSecurity(error) => Some(&error.code),
            _ => None,
        }
    }

    /// True when the error maps to a Python `ValueError`.
    pub fn is_value_error(&self) -> bool {
        matches!(
            self,
            Self::Value(_) | Self::Protocol(_) | Self::MandateSecurity(_) | Self::CredentialStoreFormat(_)
        )
    }
}
