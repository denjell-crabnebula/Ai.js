// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Neutral authorization context, dependency-free.
//!
//! The registry mutation methods accept caller identity without depending on
//! the auth module. The auth module builds [`AuthContext`] values; the
//! registry only consumes the read-only fields for owner, namespace and role
//! checks.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Principal role. Serializes as lowercase text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Provider,
    User,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Provider => "provider",
            Role::User => "user",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "provider" => Some(Role::Provider),
            "user" => Some(Role::User),
            _ => None,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Opaque caller identity for service-layer authorization checks.
///
/// `principal_id` is the stable id from the auth store and is what gets
/// written to `RegistryEntry.owner_id` and lease holder ids. `namespaces`
/// is the set of dataset names the principal may act in; `None` means all
/// namespaces and is only legal for admins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthContext {
    pub principal_id: String,
    pub role: Role,
    #[serde(default)]
    pub namespaces: Option<HashSet<String>>,
}

impl AuthContext {
    pub fn new(principal_id: impl Into<String>, role: Role, namespaces: Option<HashSet<String>>) -> Self {
        Self {
            principal_id: principal_id.into(),
            role,
            namespaces,
        }
    }

    /// Admin context with access to every namespace.
    pub fn admin(principal_id: impl Into<String>) -> Self {
        Self::new(principal_id, Role::Admin, None)
    }

    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    /// True when the caller may act in `dataset`. Admins always may.
    pub fn allows_namespace(&self, dataset: &str) -> bool {
        match &self.namespaces {
            None => self.is_admin(),
            Some(set) => set.contains(dataset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn admin_allows_all() -> TestResult {
        let ctx = AuthContext::admin("u_1");
        assert!(ctx.is_admin());
        assert!(ctx.allows_namespace("anything"));
        Ok(())
    }

    #[test]
    fn scoped_principal() -> TestResult {
        let ctx = AuthContext::new(
            "u_2",
            Role::Provider,
            Some(["team".to_string()].into_iter().collect()),
        );
        assert!(ctx.allows_namespace("team"));
        assert!(!ctx.allows_namespace("other"));
        assert_eq!(serde_json::to_value(&ctx)?["role"], "provider");
        Ok(())
    }
}
