// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Cluster identity and `load_or_none` (`test_identity.py`).

use a2x_cluster::ClusterStore;
use a2x_cluster::state::ClusterState;
use ap_support::testing::{OptionExt, TestResult};

#[test]
fn load_or_none_absent_corrupt_and_initialized() -> TestResult {
    let dir = tempfile::tempdir()?;
    assert!(
        ClusterStore::builder()
            .load_or_none_at(&dir.path().join("nope.json"))
            .is_none()
    );

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{ this is not valid json")?;
    assert!(ClusterStore::builder().load_or_none_at(&bad).is_none());

    let p = dir.path().join("cluster_state.json");
    ClusterState::init_at(Some("reg-loaded"), &p)?;
    let store = ClusterStore::builder().load_or_none_at(&p).required()?;
    assert_eq!(store.node_id(), "reg-loaded");
    let summary = store.state_summary();
    assert_eq!(summary.node_id, "reg-loaded");
    assert!(summary.peers.is_empty());
    assert!(store.membership().is_some());
    assert!(store.membership().required()?.cluster_id().is_none());
    Ok(())
}

#[test]
fn state_summary_wire_shape() -> TestResult {
    let dir = tempfile::tempdir()?;
    let p = dir.path().join("s.json");
    let store = ClusterStore::builder()
        .advertise("http://10.0.0.1:8000")
        .build(ClusterState::init_at(Some("reg-a1b2c3"), &p)?);
    let v = serde_json::to_value(store.state_summary())?;
    assert_eq!(
        v,
        serde_json::json!({
            "node_id": "reg-a1b2c3", "advertise": "http://10.0.0.1:8000", "peers": [],
            "foreign_records": 0, "foreign_by_namespace": {}, "local_records": 0, "tombstones": 0,
        })
    );
    Ok(())
}
