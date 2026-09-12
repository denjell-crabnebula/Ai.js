// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Persistent auth models: `Principal` and `ApiKey`.
//!
//! These mirror the JSON written to `auth_data/principals.json` and
//! `auth_data/api_keys.json`. Field order matches the Python model dumps.

use std::collections::HashSet;

use a2x_common::{AuthContext, Role};
use serde::{Deserialize, Serialize};

pub const VALID_ROLES: &[&str] = &["admin", "provider", "user"];

/// A registered identity that may hold one or more API keys.
///
/// `namespaces = None` means all namespaces (admin only). An empty list is
/// legal and means "no access anywhere".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub handle: String,
    pub role: Role,
    pub namespaces: Option<Vec<String>>,
    pub created_at: String,
    pub disabled_at: Option<String>,
    #[serde(default)]
    pub note: String,
}

impl Principal {
    /// Convert into the neutral [`AuthContext`] consumed by the registry.
    pub fn to_context(&self) -> AuthContext {
        let ns: Option<HashSet<String>> = self.namespaces.as_ref().map(|v| v.iter().cloned().collect());
        AuthContext::new(self.id.clone(), self.role, ns)
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }

    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

/// A single API key bound to a principal. Only the sha256 hash is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey {
    pub key_id: String,
    pub principal_id: String,
    pub key_hash: String,
    pub key_prefix: String,
    #[serde(default)]
    pub name: String,
    pub created_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
}

impl ApiKey {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}
