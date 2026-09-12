// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `test_lease.py`: the shared `LeaseTable` state machine, exercised from
//! this crate through `a2x_common` (the port lives there).

use a2x_common::{LeaseState, LeaseTable};
use ap_support::testing::{OptionExt, TestResult};

#[test]
fn install_healthy_then_expire_then_delete() -> TestResult {
    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("k", 10, 5, false, Some(100.0));
    let lease = t.get(&"k").required()?;
    assert_eq!(lease.state, LeaseState::Healthy);
    assert_eq!((lease.expires_at, lease.grace_deadline), (110.0, 115.0));
    assert_eq!(t.sweep_tick(Some(109.0)), (vec![], vec![]));
    assert_eq!(t.sweep_tick(Some(110.0)), (vec!["k"], vec![]));
    assert_eq!(t.get(&"k").required()?.state, LeaseState::Unhealthy);
    assert_eq!(t.sweep_tick(Some(115.0)).1, vec!["k"]);
    assert!(t.get(&"k").is_none());
    Ok(())
}

#[test]
fn renew_restores_and_extends_and_missing_is_none() -> TestResult {
    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("k", 10, 5, false, Some(100.0));
    t.sweep_tick(Some(110.0));
    let lease = t.renew(&"k", Some(112.0)).required()?;
    assert_eq!(lease.state, LeaseState::Healthy);
    assert_eq!(
        (lease.expires_at, lease.grace_deadline, lease.last_renew_at),
        (122.0, 127.0, 112.0)
    );
    assert!(t.renew(&"nope", None).is_none());
    Ok(())
}

#[test]
fn revoke_soft_and_permanent() -> TestResult {
    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("k", 10, 5, false, Some(100.0));
    assert!(t.revoke(&"k", false, Some(103.0)));
    let lease = t.get(&"k").required()?;
    assert_eq!(lease.state, LeaseState::Unhealthy);
    assert_eq!((lease.expires_at, lease.grace_deadline), (103.0, 108.0));
    assert!(!t.revoke(&"missing", false, None));
    assert!(t.revoke(&"k", true, None));
    assert!(t.get(&"k").is_none());
    Ok(())
}

#[test]
fn install_expired_seeds_grace_window() -> TestResult {
    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("k", 10, 5, true, Some(100.0));
    let lease = t.get(&"k").required()?;
    assert_eq!(lease.state, LeaseState::Unhealthy);
    assert_eq!((lease.expires_at, lease.grace_deadline), (100.0, 105.0));
    t.renew(&"k", Some(104.0));
    assert_eq!(t.get(&"k").required()?.state, LeaseState::Healthy);
    Ok(())
}

#[test]
fn is_unhealthy_items_drop_and_same_tick_delete() -> TestResult {
    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("a", 10, 5, false, Some(100.0));
    t.install("b", 10, 5, false, Some(100.0));
    assert!(!t.is_unhealthy(&"a"));
    assert!(!t.is_unhealthy(&"missing"));
    t.revoke(&"a", false, Some(101.0));
    assert!(t.is_unhealthy(&"a"));
    let keys: std::collections::BTreeSet<&str> = t.items().into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, ["a", "b"].into());
    t.drop_key(&"a");
    assert!(t.get(&"a").is_none());
    t.drop_key(&"a");

    let t: LeaseTable<&str> = LeaseTable::new();
    t.install("k", 10, 5, false, Some(100.0));
    assert_eq!(t.sweep_tick(Some(200.0)), (vec!["k"], vec!["k"]));
    assert!(t.get(&"k").is_none());
    Ok(())
}
