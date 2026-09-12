// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Multi-node end-to-end over real HTTP (`test_integration_http.py`):
//! several in-process axum servers talking through `HttpTransport`.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;
use std::time::Duration;

use a2x_cluster::testing::FakeClock;
use a2x_cluster::{AntiEntropySweeper, ClusterConfig, MutationOp};
use common::{HttpNodeOptions, client, eventually, http_node};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn get(c: &reqwest::Client, url: String) -> TestResult<Value> {
    Ok(c.get(url).send().await?.json().await?)
}

#[tokio::test]
async fn two_node_http_sync() -> TestResult {
    let dir = tempfile::tempdir()?;
    let a = http_node(dir.path(), "A", HttpNodeOptions::default()).await?;
    let b = http_node(dir.path(), "B", HttpNodeOptions::default()).await?;
    a.registry.add("svc", "a-svc");
    b.registry.add("svc", "b-svc");
    let c = client()?;

    let r = c
        .post(format!("{}/api/cluster/peers", a.base))
        .json(&json!({"address": b.base}))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await?;
    assert_eq!(body["peer"]["node_id"], "B");
    assert_eq!(body["peer"]["address"], b.base);
    assert_eq!(body["peer"]["namespaces"], json!(["svc"]));

    // Handshake + reconcile is synchronous inside the request.
    assert!(
        a.store
            .foreign_wrapped("svc")
            .iter()
            .any(|x| x["origin_id"] == "B" && x["name"] == "b-svc")
    );
    assert!(
        b.store
            .foreign_wrapped("svc")
            .iter()
            .any(|x| x["origin_id"] == "A" && x["name"] == "a-svc")
    );
    assert_eq!(b.store.get_peer("A").required()?.address, a.base);

    // Incremental push over HTTP.
    let sid = a.registry.add("svc", "a-extra");
    a.store
        .on_local_mutation("svc", &sid, MutationOp::Register)
        .await?;
    assert!(
        b.store
            .foreign_wrapped("svc")
            .iter()
            .any(|x| x["name"] == "a-extra")
    );

    // Deregister propagates as a tombstone.
    a.registry.remove("svc", &sid);
    a.store.on_local_delete("svc", &sid).await?;
    assert!(
        b.store
            .foreign_wrapped("svc")
            .iter()
            .all(|x| x["name"] != "a-extra")
    );

    let st = get(&c, format!("{}/api/cluster/state", b.base)).await?;
    assert_eq!(st["foreign_records"], 1);
    assert_eq!(st["peers"][0]["node_id"], "A");
    Ok(())
}

#[tokio::test]
async fn three_node_membership_set_over_http() -> TestResult {
    let dir = tempfile::tempdir()?;
    let a = http_node(dir.path(), "A", HttpNodeOptions::default()).await?;
    let b = http_node(dir.path(), "B", HttpNodeOptions::default()).await?;
    let cn = http_node(dir.path(), "C", HttpNodeOptions::default()).await?;
    for n in [&a, &b, &cn] {
        n.registry.add_dataset("svc");
    }
    cn.registry.add("svc", "c-svc");
    let c = client()?;

    let r: Value = c
        .post(format!("{}/api/cluster/set/add", a.base))
        .json(&json!({"members": [{"address": b.base}, {"address": cn.base}]}))
        .send()
        .await?
        .json()
        .await?;
    assert!(r["cluster_id"].as_str().required()?.starts_with("clu-"));
    assert!(
        r["results"]
            .as_array()
            .required()?
            .iter()
            .all(|x| x["ok"] == true)
    );

    // Drive the sweepers by hand a few rounds.
    let sweepers: Vec<AntiEntropySweeper> = [&a, &b, &cn]
        .iter()
        .map(|n| AntiEntropySweeper::new(n.store.clone(), 999.0))
        .collect();
    for _ in 0..3 {
        for s in &sweepers {
            s.tick().await;
        }
    }
    let show = get(&c, format!("{}/api/cluster/set", a.base)).await?;
    let ids: std::collections::BTreeSet<String> = show["roster"]
        .as_array()
        .required()?
        .iter()
        .map(|m| -> TestResult<_> { Ok(m["node_id"].as_str().required()?.to_string()) })
        .collect::<TestResult<_>>()?;
    assert_eq!(ids, ["A", "B", "C"].iter().map(|s| s.to_string()).collect());
    assert!(
        show["roster"]
            .as_array()
            .required()?
            .iter()
            .all(|m| m["alive"] == true)
    );
    for n in [&a, &b, &cn] {
        assert_eq!(
            n.store.list_peers().len(),
            2,
            "{} is not fully meshed",
            n.store.node_id()
        );
    }
    assert!(
        a.store
            .foreign_wrapped("svc")
            .iter()
            .any(|x| x["origin_id"] == "C" && x["name"] == "c-svc")
    );
    assert!(
        b.store
            .foreign_wrapped("svc")
            .iter()
            .any(|x| x["origin_id"] == "C")
    );

    // Restart-style persistence: A's state file carries the roster.
    let st = a.store.state_snapshot();
    assert_eq!(st.cluster_id.as_deref(), r["cluster_id"].as_str());
    assert_eq!(st.last_roster.len(), 2);

    // Remove B: gone from A's roster, B reverts to standalone.
    let r = c
        .post(format!("{}/api/cluster/set/remove", a.base))
        .json(&json!({"members": [{"node_id": "B"}]}))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let show = get(&c, format!("{}/api/cluster/set", a.base)).await?;
    assert!(
        show["roster"]
            .as_array()
            .required()?
            .iter()
            .all(|m| m["node_id"] != "B")
    );
    let bshow = get(&c, format!("{}/api/cluster/set", b.base)).await?;
    assert!(bshow["cluster_id"].is_null());
    assert!(b.store.list_peers().is_empty());
    for s in &sweepers {
        s.tick().await;
    }
    assert!(cn.store.list_peers().iter().all(|p| p.node_id != "B"));
    Ok(())
}

#[tokio::test]
async fn keepalive_and_hold_over_http() -> TestResult {
    let dir = tempfile::tempdir()?;
    let cfg = ClusterConfig {
        keepalive_interval: 5.0,
        hold_timeout: 10.0,
        ..ClusterConfig::default()
    };
    let clk = FakeClock::new(0.0);
    let a = http_node(
        dir.path(),
        "A",
        HttpNodeOptions {
            config: cfg.clone(),
            clock: Some(clk.clock()),
            ..Default::default()
        },
    )
    .await?;
    let b = http_node(
        dir.path(),
        "B",
        HttpNodeOptions {
            config: cfg,
            ..Default::default()
        },
    )
    .await?;
    b.registry.add("svc", "b-svc");
    a.store.connect_peer(&b.base, None, None).await?;
    assert_eq!(a.store.state_summary().foreign_records, 1);

    // B's keepalive over HTTP refreshes A's HOLD timer.
    clk.advance(8.0);
    b.store.emit_keepalive().await;
    assert!(a.store.check_hold(None).is_empty());

    // Silence past hold_timeout evicts B and its record on A.
    clk.advance(11.0);
    assert_eq!(a.store.check_hold(None), vec!["B".to_string()]);
    assert_eq!(a.store.state_summary().foreign_records, 0);

    // A sweeper tick on B (still holding a session to A) heals: B reconnects
    // via membership? No cluster here, so B's session to A stays, and A's
    // suppression blocks B's records until B re-opens a session.
    let peer = b.store.get_peer("A").required()?;
    b.store.reconcile(&peer).await?;
    assert_eq!(a.store.state_summary().foreign_records, 0);
    b.store.connect_peer(&a.base, None, None).await?;
    assert_eq!(a.store.state_summary().foreign_records, 1);
    Ok(())
}

#[tokio::test]
async fn background_sweepers_converge_and_shutdown() -> TestResult {
    let dir = tempfile::tempdir()?;
    let cfg = ClusterConfig {
        keepalive_interval: 0.05,
        hold_timeout: 5.0,
        anti_entropy_interval: 0.05,
        ..ClusterConfig::default()
    };
    let a = http_node(
        dir.path(),
        "A",
        HttpNodeOptions {
            config: cfg.clone(),
            ..Default::default()
        },
    )
    .await?;
    let b = http_node(
        dir.path(),
        "B",
        HttpNodeOptions {
            config: cfg,
            ..Default::default()
        },
    )
    .await?;
    a.registry.add("svc", "a-svc");
    b.registry.add("svc", "b-svc");

    let cancel = CancellationToken::new();
    let ha = a.store.start(cancel.clone());
    let hb = b.store.start(cancel.clone());

    // Declare the cluster; the sweepers build the mesh and sync records.
    let r = a
        .store
        .membership()
        .required()?
        .set_add(&[a2x_cluster::membership::MemberSpec::address(&b.base)], None)
        .await;
    assert_eq!(r["results"][0]["ok"], true);
    let a2 = Arc::clone(&a.store);
    let b2 = Arc::clone(&b.store);
    assert!(
        eventually(
            || async {
                a2.foreign_wrapped("svc").iter().any(|x| x["name"] == "b-svc")
                    && b2.foreign_wrapped("svc").iter().any(|x| x["name"] == "a-svc")
                    && b2.membership().is_some_and(|m| m.cluster_id().is_some())
            },
            Duration::from_secs(5),
        )
        .await
    );

    // A push dropped? Simulate by writing a version only, then let the
    // anti-entropy sweeper heal it.
    let sid = a.registry.add("svc", "a-late");
    a.store
        .on_local_mutation("svc", &sid, MutationOp::Register)
        .await?;
    let b3 = Arc::clone(&b.store);
    assert!(
        eventually(
            || async { b3.foreign_wrapped("svc").iter().any(|x| x["name"] == "a-late") },
            Duration::from_secs(5),
        )
        .await
    );

    ha.shutdown().await;
    hb.shutdown().await;
    assert!(cancel.is_cancelled());
    a.store.close();
    b.store.close();
    Ok(())
}
