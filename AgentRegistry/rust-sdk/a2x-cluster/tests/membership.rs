// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Declarative membership control plane (`test_membership.py`).

use ap_support::testing::{OptionExt, TestResult};
use std::collections::BTreeSet;
use std::sync::Arc;

use a2x_cluster::membership::{MemberSpec, MembershipRecord};
use a2x_cluster::state::ClusterState;
use a2x_cluster::testing::*;
use a2x_cluster::transport::{EvictRequest, LeaveRequest};
use a2x_cluster::{ClusterStore, MutationOp, Version};
use a2x_common::AuthContext;
use serde_json::json;

fn ids(store: &Arc<ClusterStore>) -> TestResult<BTreeSet<String>> {
    Ok(store
        .membership()
        .required()?
        .show()
        .roster
        .into_iter()
        .map(|r| r.node_id)
        .collect())
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn addr(a: &str) -> MemberSpec {
    MemberSpec::address(a)
}

fn nid(n: &str) -> MemberSpec {
    MemberSpec::node(n)
}

fn restart(dir: &std::path::Path, name: &str, t: &Arc<InProcessTransport>) -> TestResult<Arc<ClusterStore>> {
    let state = ClusterState::load_from(&TestNode::state_file(dir, name))?.required()?;
    Ok(TestNode::new(dir, name, Arc::new(FakeRegistry::new()), t.clone()).build_from(state))
}

#[tokio::test]
async fn set_add_bootstrap() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    assert!(b.list_peers().is_empty());

    let res = a.membership().required()?.set_add(&[addr("B")], None).await;
    assert!(res["cluster_id"].as_str().required()?.starts_with("clu-"));
    assert!(
        res["results"]
            .as_array()
            .required()?
            .iter()
            .all(|r| r["ok"] == true)
    );

    let am = a.membership().required()?;
    let bm = b.membership().required()?;
    assert_eq!(am.cluster_id(), bm.cluster_id());
    assert_eq!(ids(&a)?, set(&["A", "B"]));
    assert_eq!(ids(&b)?, set(&["A", "B"]));
    assert!(peer_ids(&a).contains("B"));
    assert!(peer_ids(&b).contains("A"));
    Ok(())
}

#[tokio::test]
async fn set_add_three_full_mesh_and_service_visible() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (c, rc) = build_store(dir.path(), "C", &t)?;
    a.membership()
        .required()?
        .set_add(&[addr("B"), addr("C")], None)
        .await;
    settle(&[&a, &b, &c], 4).await;

    let cid = a.membership().required()?.cluster_id();
    for s in [&a, &b, &c] {
        assert_eq!(s.membership().required()?.cluster_id(), cid);
        assert_eq!(ids(s)?, set(&["A", "B", "C"]));
    }
    assert_eq!(peer_ids(&a), set(&["B", "C"]));
    assert_eq!(peer_ids(&b), set(&["A", "C"]));
    assert_eq!(peer_ids(&c), set(&["A", "B"]));

    let sid = rc.add("ds", "c-svc");
    c.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    settle(&[&a, &b, &c], 4).await;
    assert!(visible(&a, &ra, "ds").contains("c-svc"));
    Ok(())
}

#[tokio::test]
async fn membership_lww_concurrent_converges() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (m, _) = build_store(dir.path(), "M", &t)?;
    a.membership().required()?.set_add(&[addr("M")], None).await;
    b.membership().required()?.set_add(&[addr("M")], None).await;
    settle(&[&a, &b, &m], 4).await;
    let mc = m.membership().required()?.cluster_id();
    assert_eq!(mc, b.membership().required()?.cluster_id());
    assert_ne!(mc, a.membership().required()?.cluster_id());
    Ok(())
}

#[tokio::test]
async fn set_remove_tombstone_propagates() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (c, _) = build_store(dir.path(), "C", &t)?;
    a.membership()
        .required()?
        .set_add(&[addr("B"), addr("C")], None)
        .await;
    settle(&[&a, &b, &c], 4).await;

    let res = a.membership().required()?.set_remove(&[nid("C")]).await;
    assert_eq!(res["results"][0], json!({"node_id": "C", "ok": true}));
    settle(&[&a, &b, &c], 4).await;
    assert!(!ids(&a)?.contains("C"));
    assert!(!ids(&b)?.contains("C"));
    assert!(!peer_ids(&a).contains("C"));
    assert!(!peer_ids(&b).contains("C"));
    assert!(c.membership().required()?.cluster_id().is_none());
    Ok(())
}

#[tokio::test]
async fn leave_old_cluster_is_immediate() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (x, _) = build_store(dir.path(), "X", &t)?;
    a.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let cid1 = a.membership().required()?.cluster_id();

    x.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b, &x], 4).await;
    let bc = b.membership().required()?.cluster_id();
    assert_eq!(bc, x.membership().required()?.cluster_id());
    assert_ne!(bc, cid1);
    assert!(!ids(&a)?.contains("B"));
    assert!(!peer_ids(&a).contains("B"));
    assert!(peer_ids(&b).contains("X"));
    Ok(())
}

#[tokio::test]
async fn restart_rejoin_from_persisted_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    a.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let cid = a.membership().required()?.cluster_id();

    let state = ClusterState::load_from(&TestNode::state_file(dir.path(), "A"))?.required()?;
    assert_eq!(state.cluster_id, cid);
    let roster_ids: BTreeSet<String> = state
        .last_roster
        .iter()
        .map(|r| -> TestResult<_> { Ok(r["node_id"].as_str().required()?.to_string()) })
        .collect::<TestResult<_>>()?;
    assert_eq!(roster_ids, set(&["B"]));

    drop(a);
    let a2 = restart(dir.path(), "A", &t)?;
    assert_eq!(a2.membership().required()?.cluster_id(), cid);
    assert!(a2.list_peers().is_empty());
    a2.membership().required()?.reconcile_connections().await;
    assert!(peer_ids(&a2).contains("B"));
    Ok(())
}

#[tokio::test]
async fn membership_records_isolated_from_service_read_path() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    a.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let cid = a.membership().required()?.cluster_id().required()?;
    for ds in ["ds", "__cluster__", cid.as_str()] {
        assert!(a.foreign_rows(ds).is_empty());
        assert!(a.foreign_entry(ds, "A:x").is_none());
    }
    assert_eq!(a.state_summary().foreign_records, 0);
    Ok(())
}

#[tokio::test]
async fn join_requires_admin_token_when_auth_on() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let b = TestNode::new(dir.path(), "B", Arc::new(FakeRegistry::new()), t.clone())
        .auth(FakeAuth::on(&[("admin-tok", AuthContext::admin("adm"))]))
        .build()?;
    let res = a.membership().required()?.set_add(&[addr("B")], None).await;
    assert_eq!(res["results"][0]["ok"], false);
    assert_eq!(res["results"][0]["error"], "unauthorized");
    assert!(b.membership().required()?.cluster_id().is_none());

    let res = a
        .membership()
        .required()?
        .set_add(&[addr("B")], Some("admin-tok"))
        .await;
    assert_eq!(res["results"][0]["ok"], true);
    assert_eq!(
        b.membership().required()?.cluster_id(),
        a.membership().required()?.cluster_id()
    );
    Ok(())
}

#[tokio::test]
async fn evict_and_leave_require_auth() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let auth = FakeAuth::on(&[("adm", AuthContext::admin("adm"))]);
    let a = TestNode::new(dir.path(), "A", Arc::new(FakeRegistry::new()), t.clone())
        .auth(auth.clone())
        .build()?;
    let b = TestNode::new(dir.path(), "B", Arc::new(FakeRegistry::new()), t.clone())
        .auth(auth)
        .build()?;
    a.membership()
        .required()?
        .set_add(&[addr("B")], Some("adm"))
        .await;
    let bm = b.membership().required()?;
    assert!(bm.cluster_id().is_some());

    assert!(
        !bm.handle_evicted(EvictRequest {
            from_node: "A".into(),
            cluster_id: None,
            token: None
        })
        .ok
    );
    assert!(
        !bm.handle_evicted(EvictRequest {
            from_node: "A".into(),
            cluster_id: None,
            token: Some("WRONG".into())
        })
        .ok
    );
    assert!(
        !bm.handle_evict_self(LeaveRequest {
            from_node: "A".into(),
            token: Some("WRONG".into())
        })
        .await
        .ok
    );
    assert!(bm.cluster_id().is_some());

    a.membership().required()?.set_remove(&[nid("B")]).await;
    assert!(bm.cluster_id().is_none());
    Ok(())
}

#[tokio::test]
async fn membership_tombstone_gc() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let am = a.membership().required()?;
    am.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    am.set_remove(&[nid("B")]).await;
    let tomb = am.roster_snapshot()["B"].clone();
    assert!(tomb.removed);
    let ret = (a.config().tombstone_retention() * 1000.0) as i64;
    assert_eq!(am.gc_membership(Some(tomb.version.0 + ret - 1)), 0);
    assert!(am.roster_snapshot().contains_key("B"));
    assert_eq!(am.gc_membership(Some(tomb.version.0 + ret + 1)), 1);
    assert!(!am.roster_snapshot().contains_key("B"));
    Ok(())
}

#[tokio::test]
async fn restart_keeps_tombstone_no_resurrection() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (c, _) = build_store(dir.path(), "C", &t)?;
    a.membership()
        .required()?
        .set_add(&[addr("B"), addr("C")], None)
        .await;
    settle(&[&a, &b, &c], 4).await;
    a.membership().required()?.set_remove(&[nid("C")]).await;
    settle(&[&a, &b, &c], 4).await;

    drop(a);
    let a2 = restart(dir.path(), "A", &t)?;
    let m = a2.membership().required()?;
    assert!(m.roster_snapshot()["C"].removed);
    m.merge(&[json!({
        "node_id": "C", "cluster_id": m.cluster_id(), "address": "C",
        "version": [1, "C"], "removed": false,
    })]);
    assert!(!ids(&a2)?.contains("C"));
    Ok(())
}

#[tokio::test]
async fn set_add_unreachable_member_reports_failure() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (_b, _) = build_store(dir.path(), "B", &t)?;
    let res = a
        .membership()
        .required()?
        .set_add(&[addr("B"), addr("GHOST")], None)
        .await;
    let results = res["results"].as_array().required()?;
    let by_addr: std::collections::HashMap<&str, &serde_json::Value> = results
        .iter()
        .map(|r| -> TestResult<_> { Ok((r["address"].as_str().required()?, r)) })
        .collect::<TestResult<_>>()?;
    assert_eq!(by_addr["B"]["ok"], true);
    assert_eq!(by_addr["GHOST"]["ok"], false);
    assert!(
        by_addr["GHOST"]["error"]
            .as_str()
            .required()?
            .starts_with("unreachable")
    );
    assert_eq!(ids(&a)?, set(&["A", "B"]));

    // A member without an address is reported too.
    let res = a
        .membership()
        .required()?
        .set_add(&[MemberSpec::default()], None)
        .await;
    assert_eq!(
        res["results"][0],
        json!({"address": null, "ok": false, "error": "address required"})
    );
    Ok(())
}

#[tokio::test]
async fn set_add_to_existing_cluster_keeps_id() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (c, _) = build_store(dir.path(), "C", &t)?;
    a.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let cid = a.membership().required()?.cluster_id();
    a.membership().required()?.set_add(&[addr("C")], None).await;
    settle(&[&a, &b, &c], 4).await;
    assert_eq!(a.membership().required()?.cluster_id(), cid);
    for s in [&a, &b, &c] {
        assert_eq!(s.membership().required()?.cluster_id(), cid);
        assert_eq!(ids(s)?, set(&["A", "B", "C"]));
    }
    Ok(())
}

#[tokio::test]
async fn set_remove_then_re_add_rejoins() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let am = a.membership().required()?;
    am.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    am.set_remove(&[nid("B")]).await;
    settle(&[&a, &b], 4).await;
    assert!(!ids(&a)?.contains("B"));
    assert!(b.membership().required()?.cluster_id().is_none());

    am.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    assert!(ids(&a)?.contains("B"));
    assert!(!am.roster_snapshot()["B"].removed);
    assert_eq!(b.membership().required()?.cluster_id(), am.cluster_id());
    Ok(())
}

#[tokio::test]
async fn leave_old_preserves_other_cluster_records() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let am = a.membership().required()?;
    am.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let old = am.cluster_id().required()?;
    am.insert_record(MembershipRecord::new(
        "Z",
        Some("clu-other".into()),
        "Z",
        Version::new(99, "Z"),
        false,
    ));
    am.leave_old(&old).await;
    let roster = am.roster_snapshot();
    assert!(!roster.contains_key("B"));
    assert!(roster.contains_key("A"));
    assert!(roster.contains_key("Z"));
    Ok(())
}

#[tokio::test]
async fn node_learns_removal_via_anti_entropy() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    a.membership().required()?.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 4).await;
    let bm = b.membership().required()?;
    let cid = bm.cluster_id();
    let tomb_ver = bm.roster_snapshot()["B"].version.0 + 1000;
    let changed = bm.merge(&[json!({
        "node_id": "B", "cluster_id": cid, "address": "B",
        "version": [tomb_ver, "A"], "removed": true,
    })]);
    assert!(changed);
    assert!(bm.cluster_id().is_none());
    assert!(b.list_peers().is_empty());
    Ok(())
}

#[tokio::test]
async fn reconcile_keeps_unreachable_member_drops_removed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let (c, _) = build_store(dir.path(), "C", &t)?;
    let am = a.membership().required()?;
    am.set_add(&[addr("B"), addr("C")], None).await;
    settle(&[&a, &b, &c], 4).await;
    assert!(peer_ids(&a).is_superset(&set(&["B", "C"])));

    t.cut("A", "C");
    am.reconcile_connections().await;
    assert!(peer_ids(&a).contains("C"));

    t.heal();
    am.set_remove(&[nid("C")]).await;
    am.reconcile_connections().await;
    assert!(!peer_ids(&a).contains("C"));
    Ok(())
}

#[tokio::test]
async fn show_reports_liveness_and_wire_shape() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, _) = build_store(dir.path(), "A", &t)?;
    let (b, _) = build_store(dir.path(), "B", &t)?;
    let am = a.membership().required()?;
    let show = serde_json::to_value(am.show())?;
    assert_eq!(show, json!({"cluster_id": null, "node_id": "A", "roster": []}));
    am.set_add(&[addr("B")], None).await;
    settle(&[&a, &b], 2).await;
    let show = am.show();
    assert!(show.roster.iter().all(|r| r.alive));
    a.disconnect_peer("B");
    let show = am.show();
    let b_row = show.roster.iter().find(|r| r.node_id == "B").required()?;
    assert!(!b_row.alive);
    assert_eq!(b_row.address, "B");
    Ok(())
}
