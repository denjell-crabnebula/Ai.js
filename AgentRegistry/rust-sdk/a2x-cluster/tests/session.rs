// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Session handshake (`test_session.py`), session tokens
//! (`test_session_token.py`) and initial reconcile (`test_reconcile.py`).

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a2x_cluster::MutationOp;
use a2x_cluster::testing::*;
use a2x_cluster::transport::OpenRequest;
use a2x_common::{AuthContext, Role};

fn pair() -> TestResult<(tempfile::TempDir, Arc<InProcessTransport>)> {
    Ok((tempfile::tempdir()?, InProcessTransport::new()))
}

#[tokio::test]
async fn handshake_creates_symmetric_sessions() -> TestResult {
    let (dir, t) = pair()?;
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds1", "a-svc");
    rb.add("ds2", "b-svc");

    let peer = a.connect_peer("B", None, None).await?;
    assert_eq!(peer.node_id, "B");
    assert_eq!(peer_ids(&a), ["B".to_string()].into());
    assert_eq!(peer_ids(&b), ["A".to_string()].into());
    // Accepted namespaces are the union of both sides' datasets.
    assert_eq!(peer.namespaces, ["ds1".to_string(), "ds2".to_string()].into());
    Ok(())
}

#[tokio::test]
async fn handshake_idempotent_and_no_duplicates() -> TestResult {
    let (dir, t) = pair()?;
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    ra.add("ds1", "a-svc");
    rb.add("ds2", "b-svc");
    a.connect_peer("B", None, None).await?;
    a.connect_peer("B", None, None).await?;
    b.connect_peer("A", None, None).await?;
    assert_eq!(a.state_summary().peers.len(), 1);
    assert_eq!(b.state_summary().peers.len(), 1);
    assert_eq!(peer_ids(&a), ["B".to_string()].into());
    assert_eq!(peer_ids(&b), ["A".to_string()].into());
    Ok(())
}

#[tokio::test]
async fn disconnect_drops_session_and_foreign() -> TestResult {
    let (dir, t) = pair()?;
    let (a, _ra) = build_store(dir.path(), "A", &t)?;
    let (_b, rb) = build_store(dir.path(), "B", &t)?;
    rb.add("ds2", "b-svc");
    a.connect_peer("B", None, None).await?;
    assert_eq!(a.state_summary().foreign_records, 1);

    assert!(a.disconnect_peer("B"));
    assert!(a.state_summary().peers.is_empty());
    assert_eq!(a.state_summary().foreign_records, 0);
    assert!(!a.disconnect_peer("B"));
    Ok(())
}

// ── reconcile ────────────────────────────────────────────────────────────

#[tokio::test]
async fn reconcile_bidirectional_convergence() -> TestResult {
    let (dir, t) = pair()?;
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    let sid_a = ra.add("ds1", "a-svc");
    let sid_b = rb.add("ds2", "b-svc");
    a.connect_peer("B", None, None).await?;

    let af = a.foreign_wrapped("ds2");
    assert_eq!(af.len(), 1);
    assert_eq!(af[0]["id"], format!("B:{sid_b}"));
    assert_eq!(af[0]["origin_id"], "B");
    assert_eq!(af[0]["name"], "b-svc");
    let bf = b.foreign_wrapped("ds1");
    assert_eq!(bf.len(), 1);
    assert_eq!(bf[0]["id"], format!("A:{sid_a}"));
    assert_eq!(bf[0]["origin_id"], "A");
    Ok(())
}

#[tokio::test]
async fn same_name_service_no_collision() -> TestResult {
    let (dir, t) = pair()?;
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (_b, rb) = build_store(dir.path(), "B", &t)?;
    let sid_a = ra.add("shared", "translator");
    let sid_b = rb.add("shared", "translator");
    assert_eq!(sid_a, sid_b);
    a.connect_peer("B", None, None).await?;
    let rows = a.foreign_wrapped("shared");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], format!("B:{sid_b}"));
    assert!(ra.names("shared").contains("translator"));
    Ok(())
}

#[tokio::test]
async fn reconcile_newer_version_wins() -> TestResult {
    let (dir, t) = pair()?;
    let (a, _ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    rb.add_generic("ds", "svc", "old", "api_config");
    a.connect_peer("B", None, None).await?;
    assert_eq!(a.foreign_wrapped("ds")[0]["description"], "old");

    // B updates the record with a bumped version while the push is dropped.
    t.cut("B", "A");
    let sid = rb.add_generic("ds", "svc", "new", "api_config");
    b.on_local_mutation("ds", &sid, MutationOp::Update).await?;
    assert_eq!(a.foreign_wrapped("ds")[0]["description"], "old");
    t.heal();
    let peer = a.get_peer("B").required()?;
    let res = a.reconcile(&peer).await?;
    assert_eq!(res.pulled, 1);
    assert_eq!(a.foreign_wrapped("ds")[0]["description"], "new");
    Ok(())
}

#[tokio::test]
async fn idempotent_updates_no_change_on_reconcile() -> TestResult {
    let (dir, t) = pair()?;
    let (a, _ra) = build_store(dir.path(), "A", &t)?;
    let (_b, rb) = build_store(dir.path(), "B", &t)?;
    rb.add("ds", "svc");
    a.connect_peer("B", None, None).await?;
    let before = a.state_summary().foreign_records;
    let res = a.reconcile(&a.get_peer("B").required()?).await?;
    assert_eq!(res.pulled, 0);
    assert_eq!(a.state_summary().foreign_records, before);
    Ok(())
}

// ── session token ────────────────────────────────────────────────────────

fn open_body(node: &str) -> OpenRequest {
    OpenRequest {
        node_id: node.into(),
        address: node.into(),
        namespaces: vec![],
        token: None,
    }
}

#[tokio::test]
async fn token_issued_only_when_auth_on() -> TestResult {
    let (dir, t) = pair()?;
    let open = TestNode::new(dir.path(), "Bopen", Arc::new(FakeRegistry::new()), t.clone()).build()?;
    assert!(open.handle_open(open_body("A")).session_token.is_none());
    let authed = TestNode::new(dir.path(), "Bauth", Arc::new(FakeRegistry::new()), t.clone())
        .auth(FakeAuth::on(&[]))
        .build()?;
    let tok = authed.handle_open(open_body("A")).session_token.required()?;
    assert!(tok.len() >= 16);
    Ok(())
}

fn provider(ns: &[&str]) -> AuthContext {
    AuthContext::new(
        "p",
        Role::Provider,
        Some(ns.iter().map(|s| s.to_string()).collect()),
    )
}

async fn authed_receiver(
    dir: &std::path::Path,
    t: &Arc<InProcessTransport>,
) -> TestResult<(Arc<a2x_cluster::ClusterStore>, String)> {
    let rb = Arc::new(FakeRegistry::new());
    rb.add("secure", "b-sec");
    rb.add("open", "b-open");
    rb.set_auth_required("secure", true);
    let b = TestNode::new(dir, "B", rb, t.clone())
        .auth(FakeAuth::on(&[("p", provider(&["secure"]))]))
        .build()?;
    let resp = b.handle_open(OpenRequest {
        node_id: "P".into(),
        address: "P".into(),
        namespaces: vec!["secure".into(), "open".into()],
        token: Some("p".into()),
    });
    let accepted: std::collections::BTreeSet<String> = resp.accepted.iter().cloned().collect();
    assert_eq!(accepted, ["secure".to_string(), "open".to_string()].into());
    Ok((b, resp.session_token.required()?))
}

fn datasets_of(rows: &[a2x_cluster::transport::DigestRow]) -> std::collections::BTreeSet<String> {
    rows.iter().map(|r| r.0.clone()).collect()
}

#[tokio::test]
async fn valid_token_grants_session_wrong_token_public_only() -> TestResult {
    let (dir, t) = pair()?;
    let (b, sess) = authed_receiver(dir.path(), &t).await?;
    let ns = vec!["secure".to_string(), "open".to_string()];
    let rows = b.serve_digest("P", Some(&ns), Some(&sess), None);
    assert_eq!(
        datasets_of(&rows),
        ["secure".to_string(), "open".to_string()].into()
    );
    let rows = b.serve_digest("P", Some(&ns), Some("WRONG"), None);
    assert_eq!(datasets_of(&rows), ["open".to_string()].into());
    let rows = b.serve_digest("P", Some(&ns), None, None);
    assert_eq!(datasets_of(&rows), ["open".to_string()].into());
    Ok(())
}

#[tokio::test]
async fn spoofed_push_to_protected_namespace_rejected() -> TestResult {
    let (dir, t) = pair()?;
    let (b, sess) = authed_receiver(dir.path(), &t).await?;
    let res = b.serve_updates(
        "P",
        vec![foreign_env("P", "secure", "evil", "generic_evil", 1)],
        Some("WRONG"),
    );
    assert_eq!((res.accepted, res.rejected), (0, 1));
    assert!(!foreign_names(&b, "secure").contains("evil"));
    let res = b.serve_updates(
        "P",
        vec![foreign_env("P", "secure", "good", "generic_good", 1)],
        Some(&sess),
    );
    assert_eq!(res.accepted, 1);
    assert!(foreign_names(&b, "secure").contains("good"));
    Ok(())
}

#[tokio::test]
async fn keepalive_requires_token_when_auth_on() -> TestResult {
    let (dir, t) = pair()?;
    let (b, sess) = authed_receiver(dir.path(), &t).await?;
    assert!(!b.handle_keepalive("P", Some("WRONG")).ok);
    assert!(b.handle_keepalive("P", Some(&sess)).ok);
    Ok(())
}

#[tokio::test]
async fn no_auth_cluster_ignores_token() -> TestResult {
    let (dir, t) = pair()?;
    let (a, ra) = build_store(dir.path(), "A", &t)?;
    let (b, rb) = build_store(dir.path(), "B", &t)?;
    ra.add("open", "a-svc");
    rb.add("open", "b-svc");
    a.connect_peer("B", None, None).await?;
    assert!(a.get_peer("B").required()?.token.is_none());
    assert!(foreign_names(&a, "open").contains("b-svc"));
    assert!(foreign_names(&b, "open").contains("a-svc"));
    Ok(())
}
