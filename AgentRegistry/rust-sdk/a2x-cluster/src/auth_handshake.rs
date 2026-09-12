// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Per-namespace authorization for the cluster handshake.
//!
//! Reuses the registry's auth semantics through [`PeerAuthVerifier`]; no new
//! auth logic. When an instance receives an OPEN it decides, per requested
//! namespace, whether to sync it:
//!
//! - a namespace the receiver does not have needs an `admin` token (so it
//!   can host an ephemeral copy); allowed outright when auth is off. Such
//!   accepted namespaces are "ephemeral".
//! - a namespace the receiver has is allowed when it is not `auth_required`;
//!   otherwise it needs a `provider` (or admin) token scoped to it.

use std::collections::HashSet;

use a2x_common::AuthContext;

use crate::registry_view::{LocalRegistryView, PeerAuthVerifier};

/// Result of [`authorize_namespaces`]: `accepted` contains `ephemeral`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Authorized {
    pub accepted: Vec<String>,
    pub ephemeral: Vec<String>,
}

/// Decide which of `requested` namespaces the receiver will sync.
pub fn authorize_namespaces(
    registry: Option<&dyn LocalRegistryView>,
    auth: &dyn PeerAuthVerifier,
    requested: &[String],
    token: Option<&str>,
) -> Authorized {
    let auth_on = auth.auth_enabled();
    let ctx: Option<AuthContext> = match (auth_on, token) {
        (true, Some(t)) if !t.is_empty() => auth.authenticate(t),
        _ => None,
    };
    let existing: HashSet<String> = registry
        .map(|r| r.list_datasets().into_iter().collect())
        .unwrap_or_default();

    let mut out = Authorized::default();
    for ns in requested {
        if existing.contains(ns) {
            let required = registry.map(|r| r.is_auth_required(ns)).unwrap_or(false);
            if !auth_on || !required {
                out.accepted.push(ns.clone());
            } else if let Some(c) = &ctx {
                let scoped = c.namespaces.as_ref().map(|s| s.contains(ns)).unwrap_or(false);
                if c.is_admin() || scoped {
                    out.accepted.push(ns.clone());
                }
            }
        } else {
            // Receiver must create an ephemeral copy: admin gated.
            let admin = ctx.as_ref().map(|c| c.is_admin()).unwrap_or(false);
            if !auth_on || admin {
                out.accepted.push(ns.clone());
                out.ephemeral.push(ns.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeAuth, FakeRegistry};
    use a2x_common::Role;
    use ap_support::testing::TestResult;

    fn reg(datasets: &[&str], auth_required: &[&str]) -> FakeRegistry {
        let r = FakeRegistry::new();
        for d in datasets {
            r.add_dataset(d);
        }
        for d in auth_required {
            r.set_auth_required(d, true);
        }
        r
    }

    fn admin_ctx() -> AuthContext {
        AuthContext::admin("adm")
    }

    fn provider_ctx(ns: &[&str]) -> AuthContext {
        AuthContext::new(
            "p",
            Role::Provider,
            Some(ns.iter().map(|s| s.to_string()).collect()),
        )
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_auth_store_accepts_everything() -> TestResult {
        let r = reg(&["have"], &[]);
        let out = authorize_namespaces(Some(&r), &FakeAuth::off(), &strs(&["have", "new"]), None);
        assert_eq!(out.accepted, strs(&["have", "new"]));
        assert_eq!(out.ephemeral, strs(&["new"]));
        Ok(())
    }

    #[test]
    fn existing_anon_namespace_accepted_without_token() -> TestResult {
        let r = reg(&["have"], &[]);
        let out = authorize_namespaces(Some(&r), &FakeAuth::on(&[]), &strs(&["have"]), None);
        assert_eq!(out.accepted, strs(&["have"]));
        assert!(out.ephemeral.is_empty());
        Ok(())
    }

    #[test]
    fn existing_authreq_namespace_needs_provider() -> TestResult {
        let r = reg(&["secure"], &["secure"]);
        let auth = FakeAuth::on(&[("tok", provider_ctx(&["secure"]))]);
        assert_eq!(
            authorize_namespaces(Some(&r), &auth, &strs(&["secure"]), Some("tok")).accepted,
            strs(&["secure"])
        );
        assert!(
            authorize_namespaces(Some(&r), &auth, &strs(&["secure"]), None)
                .accepted
                .is_empty()
        );
        let other = FakeAuth::on(&[("tok", provider_ctx(&["other"]))]);
        assert!(
            authorize_namespaces(Some(&r), &other, &strs(&["secure"]), Some("tok"))
                .accepted
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn missing_namespace_needs_admin() -> TestResult {
        let r = reg(&["have"], &[]);
        let admin = FakeAuth::on(&[("adm", admin_ctx())]);
        let prov = FakeAuth::on(&[("p", provider_ctx(&["have"]))]);
        let out = authorize_namespaces(Some(&r), &admin, &strs(&["new"]), Some("adm"));
        assert_eq!(out.accepted, strs(&["new"]));
        assert_eq!(out.ephemeral, strs(&["new"]));
        let out = authorize_namespaces(Some(&r), &prov, &strs(&["new"]), Some("p"));
        assert!(out.accepted.is_empty() && out.ephemeral.is_empty());
        Ok(())
    }

    #[test]
    fn admin_accepts_authrequired_existing() -> TestResult {
        let r = reg(&["secure"], &["secure"]);
        let admin = FakeAuth::on(&[("adm", admin_ctx())]);
        let out = authorize_namespaces(Some(&r), &admin, &strs(&["secure"]), Some("adm"));
        assert_eq!(out.accepted, strs(&["secure"]));
        Ok(())
    }
}
