// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Domain errors for the auth module. The extractor layer maps them to
//! HTTP 401 and 403.

/// Caller failed to prove identity (HTTP 401).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AuthenticationError(pub String);

/// Caller is identified but lacks permission (HTTP 403).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AuthorizationError(pub String);
