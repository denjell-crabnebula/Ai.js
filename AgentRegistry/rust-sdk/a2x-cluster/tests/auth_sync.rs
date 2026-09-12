// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! End-to-end sync under various auth states (`test_auth_sync.py`).

use ap_support::testing::TestResult;
use std::sync::Arc;

use a2x_cluster::testing::*;
use a2x_common::{AuthContext, Role};

fn provider(ns: &[&str]) -> AuthContext {
    AuthContext::new(
        "p",
        Role::Provider,
        Some(ns.iter().map(|s| s.to_string()).collect()),
    )
}

fn set(v: &[&str]) -> std::collections::BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn no_auth_everywhere_full_sync() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    ra.add("open", "a-svc");
    rb.add("open", "b-svc");
    rb.add("other", "x-svc");
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;
    assert_eq!(visible(&a, &ra, "open"), set(&["a-svc", "b-svc"]));
    assert_eq!(visible(&b, &rb, "open"), set(&["a-svc", "b-svc"]));
    assert_eq!(foreign_names(&a, "other"), set(&["x-svc"]));
    Ok(())
}

#[tokio::test]
async fn no_auth_three_node_chain_full_sync() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    let (c, rc) = build_store(dir.path(), "C", &t)?;
    ra.add("open", "A-svc");
    rb.add("open", "B-svc");
    rc.add("open", "C-svc");
    b.connect_peer("A", None, None).await?;
    b.connect_peer("C", None, None).await?;
    converge(&[&a, &b, &c], 4).await;
    // Anti-entropy exchanges the full index (foreign replicas included), so a
    // chain still reaches any-to-any visibility; only pushes skip relay.
    let everyone = set(&["A-svc", "B-svc", "C-svc"]);
    assert_eq!(visible(&a, &ra, "open"), everyone);
    assert_eq!(visible(&b, &rb, "open"), everyone);
    assert_eq!(visible(&c, &rc, "open"), everyone);
    Ok(())
}

#[tokio::test]
async fn authrequired_blocked_without_token_anon_still_syncs() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    ra.add("secure", "a-sec");
    ra.add("open", "a-open");
    let rb = Arc::new(FakeRegistry::new());
    rb.add("secure", "b-sec");
    rb.add("open", "b-open");
    rb.set_auth_required("secure", true);
    let b = TestNode::new(dir.path(), "B", rb, t.clone())
        .auth(FakeAuth::on(&[("p", provider(&["secure"]))]))
        .build()?;
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;
    assert_eq!(foreign_names(&b, "open"), set(&["a-open"]));
    assert_eq!(foreign_names(&a, "open"), set(&["b-open"]));
    assert!(foreign_names(&b, "secure").is_empty());
    assert!(foreign_names(&a, "secure").is_empty());
    Ok(())
}

#[tokio::test]
async fn authrequired_syncs_with_provider_token() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    ra.add("secure", "a-sec");
    let rb = Arc::new(FakeRegistry::new());
    rb.add("secure", "b-sec");
    rb.set_auth_required("secure", true);
    let b = TestNode::new(dir.path(), "B", rb, t.clone())
        .auth(FakeAuth::on(&[("p", provider(&["secure"]))]))
        .build()?;
    a.connect_peer("B", None, Some("p")).await?;
    converge(&[&a, &b], 4).await;
    assert_eq!(foreign_names(&b, "secure"), set(&["a-sec"]));
    assert_eq!(foreign_names(&a, "secure"), set(&["b-sec"]));
    Ok(())
}

#[tokio::test]
async fn anon_namespace_syncs_on_auth_enabled_registry() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    ra.add("open", "a-open");
    let rb = Arc::new(FakeRegistry::new());
    rb.add("open", "b-open");
    let b = TestNode::new(dir.path(), "B", rb, t.clone())
        .auth(FakeAuth::on(&[("adm", AuthContext::admin("adm"))]))
        .build()?;
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;
    assert_eq!(foreign_names(&a, "open"), set(&["b-open"]));
    assert_eq!(foreign_names(&b, "open"), set(&["a-open"]));
    Ok(())
}

#[tokio::test]
async fn updates_push_rejected_for_protected_namespace_without_session() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let rb = Arc::new(FakeRegistry::new());
    rb.add("secure", "b-sec");
    rb.set_auth_required("secure", true);
    rb.add("open", "b-open");
    let b = TestNode::new(dir.path(), "B", rb, t.clone())
        .auth(FakeAuth::on(&[("p", provider(&["secure"]))]))
        .build()?;
    let res = b.serve_updates(
        "X",
        vec![foreign_env("X", "secure", "evil", "generic_evil", 1)],
        None,
    );
    assert_eq!((res.accepted, res.rejected, res.received), (0, 1, 1));
    assert!(foreign_names(&b, "secure").is_empty());
    let res = b.serve_updates("X", vec![foreign_env("X", "open", "ok", "generic_ok", 1)], None);
    assert_eq!(res.accepted, 1);
    assert!(foreign_names(&b, "open").contains("ok"));
    Ok(())
}

#[tokio::test]
async fn updates_push_open_when_no_auth_anywhere() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (b, _rb) = build_store(dir.path(), "B", &t)?;
    let res = b.serve_updates(
        "X",
        vec![foreign_env("X", "brand_new_ns", "y", "generic_y", 1)],
        None,
    );
    assert_eq!(res.accepted, 1);
    assert!(foreign_names(&b, "brand_new_ns").contains("y"));
    Ok(())
}

#[tokio::test]
async fn admin_token_creates_ephemeral_namespace() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = InProcessTransport::new();
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    ra.add("newns", "a-only");
    let rb = Arc::new(FakeRegistry::new());
    rb.add("open", "b-open");
    let b = TestNode::new(dir.path(), "B", rb, t.clone())
        .auth(FakeAuth::on(&[("adm", AuthContext::admin("adm"))]))
        .build()?;
    a.connect_peer("B", None, None).await?;
    converge(&[&a, &b], 4).await;
    assert!(foreign_names(&b, "newns").is_empty());

    a.connect_peer("B", None, Some("adm")).await?;
    converge(&[&a, &b], 4).await;
    assert_eq!(foreign_names(&b, "newns"), set(&["a-only"]));
    Ok(())
}
