// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Full-mesh liveness (`test_liveness.py`) and multi-node topologies
//! (`test_topologies.py`): HOLD eviction, suppression, no resurrection.

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a2x_cluster::testing::*;
use a2x_cluster::{ClusterConfig, ClusterStore, MutationOp};

const DS: &str = "svc";

fn short_cfg() -> ClusterConfig {
    ClusterConfig {
        keepalive_interval: 5.0,
        hold_timeout: 10.0,
        ..ClusterConfig::default()
    }
}

fn node(
    dir: &std::path::Path,
    t: &Arc<InProcessTransport>,
    name: &str,
    services: &[&str],
    clock: Option<&FakeClock>,
) -> TestResult<(Arc<ClusterStore>, Arc<FakeRegistry>)> {
    let reg = Arc::new(FakeRegistry::new());
    for s in services {
        reg.add(DS, s);
    }
    let mut b = TestNode::new(dir, name, reg.clone(), t.clone()).config(short_cfg());
    if let Some(c) = clock {
        b = b.clock(c);
    }
    Ok((b.build()?, reg))
}

async fn full_mesh(stores: &[&Arc<ClusterStore>]) -> TestResult {
    for i in 0..stores.len() {
        for j in (i + 1)..stores.len() {
            stores[i].connect_peer(stores[j].advertise(), None, None).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn hold_drops_silent_peer_and_evicts_its_records() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let sink = Arc::new(RecordingSink::default());
    let ra = Arc::new(FakeRegistry::new());
    let a = TestNode::new(dir.path(), "A", ra, t.clone())
        .config(short_cfg())
        .clock(&clk)
        .sink(sink.clone())
        .build()?;
    let (_b, rb) = node(dir.path(), &t, "B", &[], None)?;
    let sid = rb.add("ds", "b-svc");
    a.connect_peer("B", None, None).await?;
    assert_eq!(a.state_summary().foreign_records, 1);
    assert!(peer_ids(&a).contains("B"));

    clk.advance(20.0);
    assert_eq!(a.check_hold(None), vec!["B".to_string()]);
    assert_eq!(a.state_summary().foreign_records, 0);
    assert!(a.state_summary().peers.is_empty());
    let events = sink.events.lock().clone();
    assert_eq!(events, vec![("B".to_string(), vec![("ds".to_string(), sid)])]);
    Ok(())
}

#[tokio::test]
async fn keepalive_refreshes_hold() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, ra) = node(dir.path(), &t, "A", &["seed"], Some(&clk))?;
    let _ = ra;
    let (_b, _rb) = node(dir.path(), &t, "B", &[], None)?;
    a.connect_peer("B", None, None).await?;

    clk.advance(8.0);
    a.handle_keepalive("B", None);
    assert!(a.check_hold(None).is_empty());
    clk.advance(8.0);
    assert!(a.check_hold(None).is_empty());
    clk.advance(20.0);
    assert_eq!(a.check_hold(None), vec!["B".to_string()]);
    Ok(())
}

#[tokio::test]
async fn hold_eviction_arms_suppression_no_resurrection() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, _ra) = node(dir.path(), &t, "A", &[], Some(&clk))?;
    let (b, _rb) = node(dir.path(), &t, "B", &[], None)?;
    let (c, _rc) = node(dir.path(), &t, "C", &["c-svc"], None)?;
    a.connect_peer("B", None, None).await?;
    a.connect_peer("C", None, None).await?;
    b.connect_peer("C", None, None).await?;
    assert!(a.foreign_wrapped(DS).iter().any(|r| r["origin_id"] == "C"));
    assert!(b.foreign_wrapped(DS).iter().any(|r| r["origin_id"] == "C"));

    clk.advance(8.0);
    b.emit_keepalive().await;
    clk.advance(4.0);
    assert!(a.check_hold(None).contains(&"C".to_string()));
    assert!(a.foreign_wrapped(DS).iter().all(|r| r["origin_id"] != "C"));
    assert!(a.suppressed_origins().contains(&"C".to_string()));

    a.reconcile(&a.get_peer("B").required()?).await?;
    assert!(a.foreign_wrapped(DS).iter().all(|r| r["origin_id"] != "C"));
    let _ = c;
    Ok(())
}

#[tokio::test]
async fn reconnect_lifts_suppression() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, _ra) = node(dir.path(), &t, "A", &[], Some(&clk))?;
    let (_b, rb) = node(dir.path(), &t, "B", &[], None)?;
    rb.add("ds", "b-svc");
    a.connect_peer("B", None, None).await?;
    clk.advance(20.0);
    a.check_hold(None);
    assert_eq!(a.state_summary().foreign_records, 0);
    a.connect_peer("B", None, None).await?;
    assert_eq!(a.state_summary().foreign_records, 1);
    assert!(a.suppressed_origins().is_empty());
    Ok(())
}

#[tokio::test]
async fn prune_suppression_clears_expired_entries() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, _ra) = node(dir.path(), &t, "A", &[], Some(&clk))?;
    let (_b, rb) = node(dir.path(), &t, "B", &[], None)?;
    rb.add("ds", "b-svc");
    a.connect_peer("B", None, None).await?;
    clk.advance(20.0);
    a.check_hold(None);
    assert!(!a.suppressed_origins().is_empty());
    clk.advance(a.config().tombstone_retention() + 1.0);
    a.prune_suppression(None);
    assert!(a.suppressed_origins().is_empty());
    Ok(())
}

// ── topologies ───────────────────────────────────────────────────────────

#[tokio::test]
async fn full_mesh_any_to_any_three_and_four_nodes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = node(dir.path(), &t, "A", &["a-svc"], None)?;
    let (b, rb) = node(dir.path(), &t, "B", &["b-svc"], None)?;
    let (c, rc) = node(dir.path(), &t, "C", &["c-svc"], None)?;
    let (d, rd) = node(dir.path(), &t, "D", &["d-svc"], None)?;
    full_mesh(&[&a, &b, &c, &d]).await?;
    converge(&[&a, &b, &c, &d], 4).await;
    let everyone: std::collections::BTreeSet<String> = ["a-svc", "b-svc", "c-svc", "d-svc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (s, r) in [(&a, &ra), (&b, &rb), (&c, &rc), (&d, &rd)] {
        assert_eq!(visible(s, r, DS), everyone);
    }
    Ok(())
}

#[tokio::test]
async fn new_register_and_update_propagate() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = node(dir.path(), &t, "A", &["a-svc"], None)?;
    let (b, _rb) = node(dir.path(), &t, "B", &["b-svc"], None)?;
    let (c, rc) = node(dir.path(), &t, "C", &["c-svc"], None)?;
    full_mesh(&[&a, &b, &c]).await?;
    converge(&[&a, &b, &c], 4).await;

    let sid = ra.add(DS, "a-new");
    a.on_local_mutation(DS, &sid, MutationOp::Register).await?;
    assert!(visible(&c, &rc, DS).contains("a-new"));

    let sid = rc.add_generic(DS, "c-svc", "v2", "api_config");
    c.on_local_mutation(DS, &sid, MutationOp::Update).await?;
    let a_view: std::collections::HashMap<String, String> = a
        .foreign_rows(DS)
        .into_iter()
        .map(|r| -> TestResult<_> {
            Ok({
                (
                    r.wrapped["name"].as_str().required()?.to_string(),
                    r.wrapped["description"].as_str().required()?.to_string(),
                )
            })
        })
        .collect::<TestResult<_>>()?;
    assert_eq!(a_view.get("c-svc").map(String::as_str), Some("v2"));
    Ok(())
}

#[tokio::test]
async fn all_depart_each_keeps_only_local() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, ra) = node(dir.path(), &t, "A", &["a-svc"], Some(&clk))?;
    let (b, rb) = node(dir.path(), &t, "B", &["b-svc"], Some(&clk))?;
    let (c, rc) = node(dir.path(), &t, "C", &["c-svc"], Some(&clk))?;
    full_mesh(&[&a, &b, &c]).await?;
    converge(&[&a, &b, &c], 4).await;
    assert_eq!(visible(&a, &ra, DS).len(), 3);

    clk.advance(20.0);
    for s in [&a, &b, &c] {
        s.check_hold(None);
    }
    assert_eq!(visible(&a, &ra, DS), ["a-svc".to_string()].into());
    assert_eq!(visible(&b, &rb, DS), ["b-svc".to_string()].into());
    assert_eq!(visible(&c, &rc, DS), ["c-svc".to_string()].into());
    Ok(())
}

#[tokio::test]
async fn only_departed_origin_evicted_and_not_resurrected_by_anti_entropy() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let clk = FakeClock::new(0.0);
    let (a, ra) = node(dir.path(), &t, "A", &["a-svc"], Some(&clk))?;
    let (b, rb) = node(dir.path(), &t, "B", &["b-svc"], Some(&clk))?;
    let (c, _rc) = node(dir.path(), &t, "C", &["c-svc"], Some(&clk))?;
    full_mesh(&[&a, &b, &c]).await?;
    converge(&[&a, &b, &c], 4).await;
    assert!(visible(&a, &ra, DS).contains("c-svc"));

    clk.advance(8.0);
    b.emit_keepalive().await;
    clk.advance(4.0);
    a.check_hold(None);
    assert!(!visible(&a, &ra, DS).contains("c-svc"));
    assert!(visible(&a, &ra, DS).contains("b-svc"));

    // B still holds c-svc; suppression blocks the re-pull.
    a.reconcile(&a.get_peer("B").required()?).await?;
    assert!(!visible(&a, &ra, DS).contains("c-svc"));

    b.check_hold(None);
    converge(&[&a, &b], 2).await;
    assert!(!visible(&a, &ra, DS).contains("c-svc"));
    assert!(!visible(&b, &rb, DS).contains("c-svc"));
    let _ = c;
    Ok(())
}
