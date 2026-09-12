// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP-level tests of the `/api/cluster/*` router (`test_router.py`,
//! `test_read_merge.py`, `test_mutation_hook.py`, `test_forward_compat.py`
//! and the HTTP case of `test_session_token.py`).

pub mod common;

use a2x_cluster::testing::FakeAuth;
use a2x_cluster::testing::foreign_env;
use a2x_cluster::{MutationOp, SESSION_HEADER, dormant_router};
use a2x_common::AuthContext;
use ap_support::testing::{OptionExt, TestResult};
use common::{HttpNode, HttpNodeOptions, client, http_node, spawn};
use serde_json::{Value, json};

async fn node_b(dir: &std::path::Path) -> TestResult<HttpNode> {
    http_node(dir, "B", HttpNodeOptions::default()).await
}

#[tokio::test]
async fn state_endpoint_reports_node_id() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let r = client()?
        .get(format!("{}/api/cluster/state", b.base))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await?;
    assert_eq!(body["node_id"], "B");
    assert_eq!(body["advertise"], b.base);
    assert_eq!(body["peers"], json!([]));
    Ok(())
}

#[tokio::test]
async fn session_digest_pull_flow() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let c = client()?;
    b.registry.add_dataset("ds");
    let sid = b.registry.add_generic("ds", "b-svc", "hello", "api_config");

    let r = c
        .post(format!("{}/api/cluster/sessions", b.base))
        .json(&json!({"node_id": "A", "address": "http://a", "namespaces": [], "token": null}))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await?;
    assert!(body["accepted"].as_array().required()?.contains(&json!("ds")));
    assert_eq!(body["node_id"], "B");
    assert!(body["session_token"].is_null());

    let r = c
        .get(format!("{}/api/cluster/digest", b.base))
        .query(&[("from_node", "A"), ("namespaces", "ds")])
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let rows: Vec<Value> = r.json().await?;
    assert!(
        rows.iter()
            .any(|row| row[0] == "ds" && row[1] == "B" && row[2] == sid)
    );
    assert!(rows[0][3].is_array());

    let r = c
        .get(format!("{}/api/cluster/merkle", b.base))
        .query(&[("from_node", "A"), ("namespaces", "ds")])
        .send()
        .await?;
    let hashes: Value = r.json().await?;
    assert_eq!(hashes.as_object().required()?.len(), 1);

    let r = c
        .post(format!("{}/api/cluster/pulls", b.base))
        .json(&json!({"from_node": "A", "keys": [["ds", "B", sid]]}))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let envs: Vec<Value> = r.json().await?;
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0]["origin_id"], "B");
    assert_eq!(envs[0]["tombstone"], false);
    assert_eq!(envs[0]["payload"]["wrapped"]["name"], "b-svc");

    let r = c
        .post(format!("{}/api/cluster/keepalives", b.base))
        .json(&json!({"from_node": "A"}))
        .send()
        .await?;
    assert_eq!(r.json::<Value>().await?, json!({"ok": true}));

    let r = c.get(format!("{}/api/cluster/peers", b.base)).send().await?;
    let peers: Value = r.json().await?;
    assert_eq!(peers["peers"][0]["node_id"], "A");
    assert_eq!(peers["peers"][0]["address"], "http://a");

    let r = c.delete(format!("{}/api/cluster/peers/A", b.base)).send().await?;
    assert_eq!(r.json::<Value>().await?, json!({"node_id": "A", "removed": true}));
    Ok(())
}

#[tokio::test]
async fn updates_endpoint_accepts_foreign_record_and_read_merge() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let c = client()?;
    b.registry.add_dataset("ds");
    let local_sid = b.registry.add("ds", "local-svc");

    let push = |env: Value| {
        let c = c.clone();
        let url = format!("{}/api/cluster/updates", b.base);
        async move {
            let r = c
                .post(url)
                .json(&json!({"from_node": "A", "envelopes": [env]}))
                .send()
                .await?;
            assert_eq!(r.status(), 200);
            Ok::<Value, ap_support::testing::TestError>(r.json::<Value>().await?)
        }
    };
    let env = serde_json::to_value(foreign_env("A", "ds", "remote-svc", "generic_remote", 1000))?;
    assert_eq!(push(env.clone()).await?["accepted"], 1);
    assert_eq!(
        push(env).await?,
        json!({"accepted": 0, "received": 1, "rejected": 0})
    );

    let st: Value = c
        .get(format!("{}/api/cluster/state", b.base))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(st["foreign_records"], 1);
    assert_eq!(st["foreign_by_namespace"], json!({"ds": 1}));

    // Read merge: list rows carry a namespaced id and origin_id.
    let rows = b.store.foreign_wrapped("ds");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "A:generic_remote");
    assert_eq!(rows[0]["origin_id"], "A");
    assert_eq!(rows[0]["name"], "remote-svc");
    assert!(b.registry.names("ds").contains("local-svc"));
    let fr = b.store.foreign_rows("ds");
    assert_eq!(fr[0].entry["service_id"], "generic_remote");

    // Single get by namespaced id; unknown one is None.
    let one = b.store.foreign_entry("ds", "A:generic_remote").required()?;
    assert_eq!(one["name"], "remote-svc");
    assert_eq!(one["origin_id"], "A");
    assert!(b.store.foreign_entry("ds", "A:nope").is_none());
    assert!(b.store.foreign_entry("ds", "generic_remote").is_none());

    // Same service id from another origin coexists with the local record.
    push(serde_json::to_value(foreign_env(
        "A",
        "ds",
        "local-svc",
        &local_sid,
        1000,
    ))?)
    .await?;
    let ids: Vec<String> = b
        .store
        .foreign_wrapped("ds")
        .iter()
        .map(|r| -> TestResult<_> { Ok(r["id"].as_str().required()?.to_string()) })
        .collect::<TestResult<Vec<_>>>()?;
    assert!(ids.contains(&format!("A:{local_sid}")));

    // A tombstone with a higher version removes the row from the read view.
    push(
        json!({"dataset": "ds", "service_id": "generic_remote", "origin_id": "A",
                "version": [2000, "A"], "tombstone": true, "payload": null}),
    )
    .await?;
    assert!(
        b.store
            .foreign_wrapped("ds")
            .iter()
            .all(|r| r["id"] != "A:generic_remote")
    );
    Ok(())
}

#[tokio::test]
async fn mutation_hook_stamps_versions_and_tombstones() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let c = client()?;
    let state = || async {
        Ok::<Value, ap_support::testing::TestError>(
            c.get(format!("{}/api/cluster/state", b.base))
                .send()
                .await?
                .json::<Value>()
                .await?,
        )
    };
    let before = state().await?["local_records"].as_u64().required()?;
    let sid = b.registry.add("ds", "svc");
    b.store
        .on_local_mutation("ds", &sid, MutationOp::Register)
        .await?;
    assert_eq!(state().await?["local_records"].as_u64().required()?, before + 1);

    b.store.on_local_mutation("ds", &sid, MutationOp::Update).await?;
    let st = state().await?;
    assert!(st["local_records"].as_u64().required()? >= 1);
    let tomb_before = st["tombstones"].as_u64().required()?;
    b.registry.remove("ds", &sid);
    b.store.on_local_delete("ds", &sid).await?;
    let st2 = state().await?;
    assert_eq!(st2["tombstones"].as_u64().required()?, tomb_before + 1);
    assert_eq!(st2["local_records"].as_u64().required()?, before);
    Ok(())
}

#[tokio::test]
async fn cluster_routes_404_when_not_initialized() -> TestResult {
    let base = spawn(dormant_router()).await?;
    let c = client()?;
    for path in ["/api/cluster/state", "/api/cluster/set", "/api/cluster/peers"] {
        let r = c.get(format!("{base}{path}")).send().await?;
        assert_eq!(r.status(), 404);
        let body: Value = r.json().await?;
        assert!(
            body["detail"]
                .as_str()
                .required()?
                .to_lowercase()
                .contains("not initialized")
        );
    }
    let r = c
        .post(format!("{base}/api/cluster/set/add"))
        .json(&json!({"members": []}))
        .send()
        .await?;
    assert_eq!(r.status(), 404);
    Ok(())
}

#[tokio::test]
async fn add_peer_unreachable_is_502_and_membership_absent_is_404() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = a2x_cluster::ClusterStore::builder()
        .membership(false)
        .transport(std::sync::Arc::new(a2x_cluster::HttpTransport::new(1.0)))
        .build(a2x_cluster::ClusterState::init_at(
            Some("Z"),
            &dir.path().join("Z.json"),
        )?);
    let base = spawn(a2x_cluster::router(store)).await?;
    let c = client()?;
    let r = c
        .post(format!("{base}/api/cluster/peers"))
        .json(&json!({"address": "http://127.0.0.1:9"}))
        .send()
        .await?;
    assert_eq!(r.status(), 502);
    let body: Value = r.json().await?;
    assert!(
        body["detail"]
            .as_str()
            .required()?
            .starts_with("peer unreachable")
    );

    let r = c.get(format!("{base}/api/cluster/set")).send().await?;
    assert_eq!(r.status(), 404);
    let body: Value = r.json().await?;
    assert_eq!(body["detail"], "Membership control plane not available");
    Ok(())
}

#[tokio::test]
async fn session_token_over_http_with_auth() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = http_node(
        dir.path(),
        "B",
        HttpNodeOptions {
            auth: Some(FakeAuth::on(&[("admin", AuthContext::admin("adm"))])),
            ..Default::default()
        },
    )
    .await?;
    let c = client()?;
    let ds = "secure_ds";
    b.registry.add_dataset(ds);
    b.registry.set_auth_required(ds, true);
    let sid = b.registry.add(ds, "sec-svc");

    let resp: Value = c
        .post(format!("{}/api/cluster/sessions", b.base))
        .json(&json!({"node_id": "P", "address": "http://p", "namespaces": [ds], "token": "admin"}))
        .send()
        .await?
        .json()
        .await?;
    assert!(resp["accepted"].as_array().required()?.contains(&json!(ds)));
    let token = resp["session_token"].as_str().required()?.to_string();
    assert!(!token.is_empty());

    let rows: Vec<Value> = c
        .get(format!("{}/api/cluster/digest", b.base))
        .query(&[("from_node", "P"), ("namespaces", ds)])
        .header(SESSION_HEADER, &token)
        .send()
        .await?
        .json()
        .await?;
    assert!(rows.iter().any(|r| r[0] == ds && r[2] == sid));

    let rows: Vec<Value> = c
        .get(format!("{}/api/cluster/digest", b.base))
        .query(&[("from_node", "P"), ("namespaces", ds)])
        .send()
        .await?
        .json()
        .await?;
    assert!(rows.iter().all(|r| r[0] != ds));

    // Keepalive needs the session header too.
    let ok: Value = c
        .post(format!("{}/api/cluster/keepalives", b.base))
        .json(&json!({"from_node": "P"}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(ok, json!({"ok": false}));
    let ok: Value = c
        .post(format!("{}/api/cluster/keepalives", b.base))
        .header(SESSION_HEADER, &token)
        .json(&json!({"from_node": "P"}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(ok, json!({"ok": true}));
    Ok(())
}

#[tokio::test]
async fn membership_endpoints_wire_shapes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let c = client()?;
    let show: Value = c
        .get(format!("{}/api/cluster/set", b.base))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(show, json!({"cluster_id": null, "node_id": "B", "roster": []}));

    let r: Value = c
        .post(format!("{}/api/cluster/set/add", b.base))
        .json(&json!({"members": [{"address": "http://127.0.0.1:9"}]}))
        .send()
        .await?
        .json()
        .await?;
    assert!(r["cluster_id"].as_str().required()?.starts_with("clu-"));
    assert_eq!(r["results"][0]["ok"], false);

    let r: Value = c
        .post(format!("{}/api/cluster/set/remove", b.base))
        .json(&json!({"members": [{}]}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        r,
        json!({"results": [{"ok": false, "error": "node_id required"}]})
    );

    let r: Value = c
        .get(format!("{}/api/cluster/set/digest", b.base))
        .query(&[("from_node", "A")])
        .send()
        .await?
        .json()
        .await?;
    assert!(r["B"].is_array());

    let r: Value = c
        .post(format!("{}/api/cluster/set/pull", b.base))
        .json(&json!({"from_node": "A", "node_ids": ["B"]}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(r[0]["node_id"], "B");
    assert_eq!(r[0]["removed"], false);

    let r: Value = c
        .post(format!("{}/api/cluster/set/sync", b.base))
        .json(&json!({"from_node": "A", "records": []}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(r, json!({"accepted": true}));

    let r: Value = c
        .post(format!("{}/api/cluster/join", b.base))
        .json(&json!({"roster": []}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(r, json!({"accepted": false, "error": "cluster_id required"}));

    let r: Value = c
        .post(format!("{}/api/cluster/leave", b.base))
        .json(&json!({"from_node": ""}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(r, json!({"ok": false}));
    Ok(())
}

// ── CLI ──────────────────────────────────────────────────────────────────

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: a2x_cluster::cli::ClusterCommand,
}

#[tokio::test]
async fn cli_commands_against_live_and_dormant_servers() -> TestResult {
    use a2x_cluster::cli::{ClusterCommand, SetCommand, execute};
    use clap::Parser;

    let dir = tempfile::tempdir()?;
    let b = node_b(dir.path()).await?;
    let dormant = spawn(dormant_router()).await?;

    // clap parsing of the embedded subcommand tree.
    let cli = Cli::parse_from([
        "x",
        "set",
        "add",
        "http://h:1",
        "http://h:2",
        "--token",
        "t",
        "--server",
        &b.base,
    ]);
    let ClusterCommand::Set {
        cmd: SetCommand::Add { addresses, token, .. },
    } = &cli.cmd
    else {
        return Err(ap_support::testing::TestFailure::new("unexpected value").into());
    };
    assert_eq!(addresses, &["http://h:1", "http://h:2"]);
    assert_eq!(token.as_deref(), Some("t"));

    let out = execute(Cli::parse_from(["x", "status", "--server", &b.base]).cmd).await;
    assert_eq!(out.code, 0);
    let v: Value = serde_json::from_str(&out.text)?;
    assert_eq!(v["node_id"], "B");

    let out = execute(Cli::parse_from(["x", "set", "show", "--server", &b.base]).cmd).await;
    assert_eq!(out.code, 0);
    assert!(out.text.contains("\"cluster_id\": null"));

    let out = execute(Cli::parse_from(["x", "status", "--server", &dormant]).cmd).await;
    assert_eq!(out.code, 1);
    assert!(out.text.contains("not initialized"));

    let out = execute(Cli::parse_from(["x", "rm-peer", "nobody", "--server", &b.base]).cmd).await;
    assert_eq!(out.code, 0);
    assert!(out.text.contains("\"removed\": false"));

    let out = execute(Cli::parse_from(["x", "status", "--server", "http://127.0.0.1:9"]).cmd).await;
    assert_eq!(out.code, 1);
    assert!(out.text.starts_with("error: cannot reach server"));

    let out = execute(Cli::parse_from(["x", "set", "remove", "B", "--server", &b.base]).cmd).await;
    assert_eq!(out.code, 0);
    assert!(out.text.contains("\"ok\": true"));
    let _ = cli;
    Ok(())
}
