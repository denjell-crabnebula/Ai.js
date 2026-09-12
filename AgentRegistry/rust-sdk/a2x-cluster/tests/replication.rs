// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Incremental push, direct broadcast, dedup (`test_replication.py`),
//! tombstones (`test_tombstone.py`) and anti-entropy (`test_antientropy.py`).

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a2x_cluster::state::{ClusterState, make_key};
use a2x_cluster::testing::*;
use a2x_cluster::{AntiEntropySweeper, MutationOp, SyncEnvelope, Version};
use serde_json::json;

fn env(origin: &str, sid: &str, ver: Version, tombstone: bool, desc: &str) -> SyncEnvelope {
    SyncEnvelope {
        dataset: "ds".into(),
        service_id: sid.into(),
        origin_id: origin.into(),
        version: ver,
        tombstone,
        payload: if tombstone {
            None
        } else {
            Some(
                json!({"entry": {}, "wrapped": {"id": sid, "name": "x", "description": desc, "type": "generic", "metadata": {}}}),
            )
        },
    }
}

#[tokio::test]
async fn incremental_push_to_existing_namespace() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "seed");
    a.connect_peer("B", None, None).await?;
    let sid = ra.add("ds", "later");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    assert!(foreign_names(&b, "ds").contains("later"));
    Ok(())
}

#[tokio::test]
async fn full_mesh_direct_broadcast_reaches_all_peers() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    let (c, rc) = build_store(dir.path(), "C", &t)?;
    for r in [&ra, &rb, &rc] {
        r.add("ds", "seed");
    }
    a.connect_peer("B", None, None).await?;
    a.connect_peer("C", None, None).await?;
    let sid = ra.add("ds", "from-A");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    for node in [&b, &c] {
        assert!(
            node.foreign_wrapped("ds")
                .iter()
                .any(|r| r["origin_id"] == "A" && r["name"] == "from-A")
        );
    }
    assert!(a.foreign_wrapped("ds").iter().all(|r| r["origin_id"] != "A"));
    Ok(())
}

#[tokio::test]
async fn inbound_updates_are_not_relayed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    let (c, rc) = build_store(dir.path(), "C", &t)?;
    for r in [&ra, &rb, &rc] {
        r.add("ds", "seed");
    }
    a.connect_peer("B", None, None).await?;
    a.connect_peer("C", None, None).await?;
    let sid = rb.add("ds", "from-B");
    b.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    assert!(foreign_names(&a, "ds").contains("from-B"));
    assert!(!foreign_names(&c, "ds").contains("from-B"));
    Ok(())
}

#[tokio::test]
async fn self_origin_inbound_ignored_and_version_dedup() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    assert!(!b.apply_inbound(env("B", "generic_x", Version::new(999, "B"), false, "d")));
    assert!(b.foreign_wrapped("ds").is_empty());

    assert!(b.apply_inbound(env("A", "generic_x", Version::new(100, "A"), false, "v1")));
    assert!(!b.apply_inbound(env("A", "generic_x", Version::new(100, "A"), false, "v1")));
    assert!(!b.apply_inbound(env("A", "generic_x", Version::new(50, "A"), false, "older")));
    assert!(b.apply_inbound(env("A", "generic_x", Version::new(200, "A"), false, "v2")));
    assert_eq!(b.foreign_wrapped("ds")[0]["description"], "v2");
    Ok(())
}

#[tokio::test]
async fn deregister_tombstone_propagates() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "seed");
    a.connect_peer("B", None, None).await?;
    let sid = ra.add("ds", "todelete");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    assert!(foreign_names(&b, "ds").contains("todelete"));
    ra.remove("ds", &sid);
    a.on_local_delete("ds", &sid).await?;
    assert!(!foreign_names(&b, "ds").contains("todelete"));
    Ok(())
}

#[tokio::test]
async fn on_local_upsert_with_explicit_payload() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "seed");
    a.connect_peer("B", None, None).await?;
    a.on_local_upsert(
        "ds",
        "generic_direct",
        json!({"service_id": "generic_direct", "type": "generic", "source": "api_config"}),
        Some(json!({"id": "generic_direct", "name": "direct"})),
    )
    .await?;
    assert!(foreign_names(&b, "ds").contains("direct"));
    assert_eq!(a.state_summary().local_records, 2);
    Ok(())
}

// ── tombstones ───────────────────────────────────────────────────────────

#[tokio::test]
async fn local_delete_not_resurrected_by_self_origin_push() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let sid = ra.add("ds", "x");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    ra.remove("ds", &sid);
    a.on_local_mutation("ds", &sid, MutationOp::Deregister).await?;
    assert!(!a.apply_inbound(env("A", &sid, Version::new(1, "A"), false, "d")));
    Ok(())
}

#[tokio::test]
async fn foreign_tombstone_blocks_stale_resurrection() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    assert!(b.apply_inbound(env("A", "generic_x", Version::new(100, "A"), false, "v1")));
    assert!(b.apply_inbound(env("A", "generic_x", Version::new(200, "A"), true, "")));
    assert!(b.foreign_wrapped("ds").is_empty());
    assert!(!b.apply_inbound(env("A", "generic_x", Version::new(150, "A"), false, "stale")));
    assert!(b.foreign_wrapped("ds").is_empty());
    Ok(())
}

#[tokio::test]
async fn local_tombstone_gc_after_retention_and_persists() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let sid = ra.add("ds", "x");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    ra.remove("ds", &sid);
    a.on_local_mutation("ds", &sid, MutationOp::Deregister).await?;
    let state = a.state_snapshot();
    assert_eq!(state.tombstones.len(), 1);
    let deleted_at = state.tombstones[&make_key("ds", &sid)].deleted_at_ms;

    // Reload from disk: tombstone (and its version) survived.
    let reloaded = ClusterState::load_from(&TestNode::state_file(dir.path(), "A"))?.required()?;
    assert!(reloaded.tombstones.contains_key(&make_key("ds", &sid)));
    assert!(!reloaded.local_versions.contains_key(&make_key("ds", &sid)));

    let retention_ms = (a.config().tombstone_retention() * 1000.0) as i64;
    assert_eq!(a.gc_tombstones(Some(deleted_at + retention_ms - 1)), 0);
    assert_eq!(a.state_snapshot().tombstones.len(), 1);
    assert_eq!(a.gc_tombstones(Some(deleted_at + retention_ms + 1)), 1);
    assert_eq!(a.state_snapshot().tombstones.len(), 0);
    Ok(())
}

#[tokio::test]
async fn foreign_tombstone_gc_after_retention() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    b.apply_inbound(env("A", "generic_x", Version::new(1000, "A"), true, ""));
    let retention_ms = (b.config().tombstone_retention() * 1000.0) as i64;
    assert_eq!(b.gc_tombstones(Some(1000 + retention_ms - 1)), 0);
    assert_eq!(b.gc_tombstones(Some(1000 + retention_ms + 1)), 1);
    Ok(())
}

// ── anti-entropy ─────────────────────────────────────────────────────────

#[tokio::test]
async fn reconcile_recovers_dropped_push() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "seed");
    a.connect_peer("B", None, None).await?;

    t.cut("A", "B");
    let sid = ra.add("ds", "missed");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    assert!(!foreign_names(&b, "ds").contains("missed"));

    t.heal();
    a.reconcile(&a.get_peer("B").required()?).await?;
    assert!(foreign_names(&b, "ds").contains("missed"));
    Ok(())
}

#[tokio::test]
async fn sweeper_tick_reconciles_and_survives_unreachable_peer() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds", "seed");
    a.connect_peer("B", None, None).await?;

    t.cut("A", "B");
    let sid = ra.add("ds", "missed");
    a.on_local_mutation("ds", &sid, MutationOp::Register).await?;
    // Tick must not fail while B is unreachable.
    let sweeper = AntiEntropySweeper::new(Arc::clone(&a), 999.0);
    sweeper.tick().await;
    assert!(!foreign_names(&b, "ds").contains("missed"));

    t.heal();
    sweeper.tick().await;
    assert!(foreign_names(&b, "ds").contains("missed"));
    Ok(())
}
