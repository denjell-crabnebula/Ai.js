// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! The blocking wrapper, ownership persistence across clients, and the CLI.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::io::Cursor;
use std::time::Duration;

use a2x_registry_client::cli::{Cli, CliEnv};
use a2x_registry_client::{
    ClientConfig, ClientError, ListOptions, OwnershipFile, RegisterOptions, ReserveOptions, blocking,
    read_cli_token, write_cli_token,
};
use clap::Parser;
use common::{MockResponse, MockServer, Recorded};
use serde_json::{Map, Value, json};

fn responder(req: &Recorded) -> TestResult<MockResponse> {
    Ok(match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/datasets/ds/services/a2a") => {
            let sid = req
                .body
                .as_ref()
                .and_then(|b| b.get("service_id"))
                .and_then(Value::as_str)
                .unwrap_or("sid");
            MockResponse::json(
                200,
                json!({"service_id": sid, "dataset": "ds", "status": "registered"}),
            )
        }
        ("GET", "/api/datasets/ds/services") => MockResponse::json(
            200,
            json!([{"id": "sid", "type": "a2a", "name": "n", "description": "d.", "metadata": {"description": "d"}}]),
        ),
        ("DELETE", "/api/datasets/ds/services/sid") => {
            MockResponse::json(200, json!({"service_id": "sid", "status": "deregistered"}))
        }
        ("POST", "/api/datasets/ds/reservations") => MockResponse::json(
            200,
            json!({"holder_id": "h", "ttl_seconds": 30, "expires_at_unix": 1.0, "reservations": []}),
        ),
        ("DELETE", "/api/datasets/ds/reservations/h") => MockResponse::json(200, json!({"released": []})),
        ("GET", "/api/auth/whoami") => {
            if req.headers.get("authorization").map(String::as_str) == Some("Bearer a2x_pat_good") {
                MockResponse::json(
                    200,
                    json!({"principal_id": "u_1", "handle": "root", "role": "admin"}),
                )
            } else {
                MockResponse::json(401, json!({"detail": "Missing token"}))
            }
        }
        ("GET", "/api/auth/keys") => MockResponse::json(
            200,
            json!([{"key_id": "k_1", "key_prefix": "a2x_pat_good", "name": "cli", "revoked_at": null}]),
        ),
        ("POST", "/api/auth/keys") => MockResponse::json(
            201,
            json!({"key_id": "k_2", "key_prefix": "a2x_pat_new", "token": "a2x_pat_newtoken", "name": req.body.as_ref().required()?["name"]}),
        ),
        ("DELETE", "/api/auth/keys/k_1") => MockResponse::json(
            200,
            json!({"key_id": "k_1", "revoked_at": "2026-01-01T00:00:00Z"}),
        ),
        ("DELETE", "/api/auth/keys/k_other") => MockResponse::json(403, json!({"detail": "not yours"})),
        _ => MockResponse::json(404, json!({"detail": "no route"})),
    })
}

#[test]
fn blocking_client_mirrors_async_api() -> TestResult {
    let server = MockServer::start(responder)?;
    let client = blocking::A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Disabled)
            .timeout(Duration::from_secs(5)),
    )?;
    assert_eq!(client.base_url(), format!("{}/", server.base_url));
    assert_eq!(client.timeout(), Duration::from_secs(5));
    assert_eq!(client.api_key(), None);

    let card = json!({"name": "n", "description": "d"})
        .as_object()
        .cloned()
        .required()?;
    let resp = client.register_agent("ds", &card, &RegisterOptions::default())?;
    assert_eq!(resp.service_id, "sid");
    let agents = client.list_agents("ds", &ListOptions::new())?;
    assert_eq!(agents[0]["description"], "d");
    assert!(
        client
            .update_agent("ds", "other", &Map::new())
            .err_or_fail()?
            .is_not_owned()
    );
    let dereg = client.deregister_agent("ds", "sid")?;
    assert_eq!(dereg.status, "deregistered");
    assert!(!client.async_client().ownership().contains("ds", "sid"));
    let err = client.get_agent("ds", "missing").err_or_fail()?;
    assert!(err.is_not_found());
    Ok(())
}

#[test]
fn blocking_reservation_guard_releases_on_drop() -> TestResult {
    let server = MockServer::start(responder)?;
    let client = blocking::A2xRegistryClient::connect(&server.base_url)?;
    {
        let guard = client.reserve_blank_agents_guarded("ds", &ReserveOptions::default())?;
        assert_eq!(guard.holder_id, "h");
        assert!(guard.agents.is_empty());
    }
    let last = server.last()?;
    assert_eq!(last.method, "DELETE");
    assert_eq!(last.path, "/api/datasets/ds/reservations/h");

    // Explicit release makes the drop a no-op.
    let before = server.request_count();
    {
        let mut guard = client.reserve_blank_agents_guarded("ds", &ReserveOptions::default())?;
        guard.release()?;
        assert!(guard.is_released());
        guard.release()?;
    }
    assert_eq!(server.request_count(), before + 2);

    // into_inner keeps the lease.
    let before = server.request_count();
    let r = client
        .reserve_blank_agents_guarded("ds", &ReserveOptions::default())?
        .into_inner();
    assert_eq!(r.holder_id, "h");
    assert_eq!(server.request_count(), before + 1);
    Ok(())
}

#[test]
fn ownership_file_survives_client_restart() -> TestResult {
    let server = MockServer::start(responder)?;
    let dir = tempfile::tempdir()?;
    let owned = dir.path().join("owned.json");
    let config = || {
        ClientConfig::new()
            .base_url(&server.base_url)
            .ownership_file(OwnershipFile::Path(owned.clone()))
    };
    let card = json!({"name": "n", "description": "d"})
        .as_object()
        .cloned()
        .required()?;
    {
        let c = blocking::A2xRegistryClient::new(config())?;
        c.register_agent("ds", &card, &RegisterOptions::default())?;
        assert_eq!(c.async_client().ownership().file_path(), Some(owned.as_path()));
    }
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&owned)?)?;
    assert_eq!(raw["schema_version"], 1);
    assert_eq!(raw["data"][format!("{}/", server.base_url)]["ds"], json!(["sid"]));

    let c = blocking::A2xRegistryClient::new(config())?;
    assert!(c.async_client().ownership().contains("ds", "sid"));
    c.deregister_agent("ds", "sid")?;
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&owned)?)?;
    assert!(raw["data"].as_object().required()?.is_empty());

    // A client for a different registry never sees the other segment.
    let other = blocking::A2xRegistryClient::new(
        ClientConfig::new()
            .base_url("http://other:1")
            .ownership_file(OwnershipFile::Path(owned.clone())),
    )?;
    assert!(!other.async_client().ownership().contains("ds", "sid"));
    Ok(())
}

#[test]
fn client_reads_credentials_from_cli_token_file() -> TestResult {
    let server = MockServer::start(responder)?;
    let dir = tempfile::tempdir()?;
    let cfg = dir.path().join("cli_token.json");
    write_cli_token("a2x_pat_good", &server.base_url, Some(&cfg))?;
    let client = blocking::A2xRegistryClient::new(
        ClientConfig::new()
            .config_path(&cfg)
            .ownership_file(OwnershipFile::Disabled),
    )?;
    assert_eq!(client.api_key(), Some("a2x_pat_good"));
    assert_eq!(client.base_url(), format!("{}/", server.base_url));
    let who = client.whoami()?;
    assert_eq!(who["handle"], "root");
    assert_eq!(server.last()?.headers["authorization"], "Bearer a2x_pat_good");
    Ok(())
}

// ── CLI ───────────────────────────────────────────────────────────────────

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_cli(args: &[&str], cfg: &std::path::Path, stdin: &str) -> TestResult<Run> {
    let cli = Cli::try_parse_from(std::iter::once("a2x-registry-client").chain(args.iter().copied()))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let mut env = CliEnv {
        config_path: Some(cfg.to_path_buf()),
        stdout: &mut out,
        stderr: &mut err,
        stdin: &mut input,
    };
    let code = a2x_registry_client::cli::run_with(cli, &mut env);
    Ok(Run {
        code,
        stdout: String::from_utf8(out)?,
        stderr: String::from_utf8(err)?,
    })
}

#[test]
fn cli_parses_all_subcommands_and_flags() -> TestResult {
    let cli = Cli::try_parse_from(["x", "--base-url", "http://a", "login", "--token", "t"])?;
    assert_eq!(cli.base_url.as_deref(), Some("http://a"));
    assert!(matches!(
        cli.command,
        a2x_registry_client::cli::Command::Login { token: Some(_) }
    ));
    let cli = Cli::try_parse_from(["x", "login", "--base-url", "http://b"])?;
    assert_eq!(
        cli.base_url.as_deref(),
        Some("http://b"),
        "--base-url is accepted after the subcommand too"
    );
    assert!(Cli::try_parse_from(["x", "logout"]).is_ok());
    assert!(Cli::try_parse_from(["x", "whoami"]).is_ok());
    assert!(Cli::try_parse_from(["x", "keys", "list"]).is_ok());
    let cli = Cli::try_parse_from(["x", "keys", "create"])?;
    match cli.command {
        a2x_registry_client::cli::Command::Keys {
            command: a2x_registry_client::cli::KeysCommand::Create { name },
        } => {
            assert_eq!(name, "cli")
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    assert!(Cli::try_parse_from(["x", "keys", "create", "--name", "laptop"]).is_ok());
    assert!(Cli::try_parse_from(["x", "keys", "revoke", "k_1"]).is_ok());
    assert!(
        Cli::try_parse_from(["x", "keys"]).is_err(),
        "keys needs a subcommand"
    );
    assert!(Cli::try_parse_from(["x"]).is_err(), "a subcommand is required");
    Ok(())
}

#[test]
fn cli_login_logout_whoami_keys_flow() -> TestResult {
    let server = MockServer::start(responder)?;
    let dir = tempfile::tempdir()?;
    let cfg = dir.path().join("cli_token.json");

    let r = run_cli(&["login", "--token", "wrong_prefix"], &cfg, "")?;
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("must start with 'a2x_pat_'"));
    assert!(!cfg.exists());

    // Interactive: URL and token read from stdin.
    let r = run_cli(&["login"], &cfg, &format!("{}\na2x_pat_good\n", server.base_url))?;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Registry URL ["));
    assert!(r.stdout.contains("Saved to"));
    assert!(r.stdout.contains("Logged in as \"root\" (role=admin)"));
    let saved = read_cli_token(Some(&cfg)).required()?;
    assert_eq!(saved.api_key.as_deref(), Some("a2x_pat_good"));
    assert_eq!(saved.base_url.as_deref(), Some(server.base_url.as_str()));

    // Bad token: saved, but the whoami poke reports 401 with exit 1.
    let r = run_cli(
        &["--base-url", &server.base_url, "login", "--token", "a2x_pat_bad"],
        &cfg,
        "",
    )?;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("401"));
    assert_eq!(
        read_cli_token(Some(&cfg)).required()?.api_key.as_deref(),
        Some("a2x_pat_bad")
    );

    // Unreachable registry: token saved, warning, exit 0.
    let r = run_cli(
        &[
            "--base-url",
            "http://127.0.0.1:1",
            "login",
            "--token",
            "a2x_pat_good",
        ],
        &cfg,
        "",
    )?;
    assert_eq!(r.code, 0);
    assert!(r.stderr.contains("whoami failed"));

    let r = run_cli(&["--base-url", &server.base_url, "whoami"], &cfg, "")?;
    assert_eq!(r.code, 0, "{}", r.stderr);
    let who: Value = serde_json::from_str(&r.stdout)?;
    assert_eq!(who["role"], "admin");

    let r = run_cli(&["--base-url", &server.base_url, "keys", "list"], &cfg, "")?;
    assert_eq!(r.code, 0);
    let keys: Value = serde_json::from_str(&r.stdout)?;
    assert_eq!(keys[0]["key_id"], "k_1");

    let r = run_cli(
        &[
            "--base-url",
            &server.base_url,
            "keys",
            "create",
            "--name",
            "laptop",
        ],
        &cfg,
        "",
    )?;
    assert_eq!(r.code, 0);
    let created: Value = serde_json::from_str(&r.stdout)?;
    assert_eq!(created["token"], "a2x_pat_newtoken");
    assert_eq!(server.last()?.body.required()?, json!({"name": "laptop"}));

    let r = run_cli(
        &["--base-url", &server.base_url, "keys", "revoke", "k_1"],
        &cfg,
        "",
    )?;
    assert_eq!(r.code, 0);
    assert_eq!(server.last()?.path, "/api/auth/keys/k_1");
    let r = run_cli(
        &["--base-url", &server.base_url, "keys", "revoke", "k_other"],
        &cfg,
        "",
    )?;
    assert_eq!(r.code, 3);
    assert!(r.stderr.contains("403"));

    let r = run_cli(&["logout"], &cfg, "")?;
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Removed"));
    assert!(!cfg.exists());
    let r = run_cli(&["logout"], &cfg, "")?;
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("no token file"));

    // Without a token, whoami is a 401 -> exit 1.
    let r = run_cli(&["--base-url", &server.base_url, "whoami"], &cfg, "")?;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("HTTP 401"));
    Ok(())
}

#[test]
fn cli_uses_base_url_from_token_file_when_flag_absent() -> TestResult {
    let server = MockServer::start(responder)?;
    let dir = tempfile::tempdir()?;
    let cfg = dir.path().join("cli_token.json");
    write_cli_token("a2x_pat_good", &server.base_url, Some(&cfg))?;
    let r = run_cli(&["whoami"], &cfg, "")?;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(server.last()?.path, "/api/auth/whoami");
    Ok(())
}

#[test]
fn error_display_is_stable_for_cli_users() -> TestResult {
    let e = ClientError::NotOwned {
        dataset: "d".into(),
        service_id: "s".into(),
    };
    assert!(e.to_string().contains("was not registered by this client"));
    Ok(())
}
