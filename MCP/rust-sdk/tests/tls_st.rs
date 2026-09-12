// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! TLS round trips, porting `tests/ut/client/tls_test.cpp`. Certificates are
//! generated with the `openssl` command line tool; the tests are skipped when
//! it is not installed.

pub mod common;

use ap_support::testing::{ResultExt, TestResult};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::*;
use mcp_sdk::prelude::*;
use serde_json::json;

struct Certs {
    _dir: tempfile::TempDir,
    ca: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    client_cert: PathBuf,
    client_key: PathBuf,
}

fn run(dir: &Path, args: &[&str]) -> bool {
    Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn generate_certs() -> Option<Certs> {
    if Command::new("openssl").arg("version").output().is_err() {
        return None;
    }
    let dir = tempfile::tempdir().ok()?;
    let d = dir.path();
    std::fs::write(
        d.join("server.ext"),
        "subjectAltName=IP:127.0.0.1,DNS:localhost\n",
    )
    .ok()?;
    std::fs::write(d.join("client.ext"), "extendedKeyUsage=clientAuth\n").ok()?;
    let steps: &[&[&str]] = &[
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-days",
            "1",
            "-subj",
            "/CN=Test CA",
        ],
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-subj",
            "/CN=localhost",
        ],
        &[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "server.crt",
            "-days",
            "1",
            "-extfile",
            "server.ext",
        ],
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "client.key",
            "-out",
            "client.csr",
            "-subj",
            "/CN=client",
        ],
        &[
            "x509",
            "-req",
            "-in",
            "client.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "client.crt",
            "-days",
            "1",
            "-extfile",
            "client.ext",
        ],
    ];
    for step in steps {
        if !run(d, step) {
            return None;
        }
    }
    Some(Certs {
        ca: d.join("ca.crt"),
        server_cert: d.join("server.crt"),
        server_key: d.join("server.key"),
        client_cert: d.join("client.crt"),
        client_key: d.join("client.key"),
        _dir: dir,
    })
}

fn path(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

async fn start_tls_server(certs: &Certs, mutual: bool) -> TestResult<(McpServer, String)> {
    let port = free_port()?;
    let endpoint = format!("https://127.0.0.1:{port}/mcp");
    let transport = StreamableHttpServerConfig {
        endpoint: endpoint.clone(),
        is_json_response_enabled: true,
        io_threads: 1,
        tls_config: TlsConfig {
            enabled: true,
            cert_file: path(&certs.server_cert),
            key_file: path(&certs.server_key),
            ca_file: if mutual { path(&certs.ca) } else { String::new() },
            server_name: "localhost".into(),
            verify_peer: true,
        },
        ..Default::default()
    };
    let server = McpServerFactory::create_streamable_http_server(ServerConfig::default(), transport)?;
    configure_server(&server, Arc::new(Recorded::default()))?;
    server.run().await?;
    Ok((server, endpoint))
}

fn tls_client(endpoint: &str, tls: TlsConfig) -> TestResult<McpClient> {
    let http = StreamableHttpClientConfig {
        endpoint: endpoint.to_string(),
        timeout: std::time::Duration::from_millis(5000),
        sse_timeout: std::time::Duration::from_millis(5000),
        tls_config: tls,
        ..Default::default()
    };
    Ok(McpClientFactory::create_streamable_http_client(
        ClientConfig::default(),
        http,
        None,
    )?)
}

async fn echo_round_trip(client: &McpClient) -> TestResult {
    client.initialize().await?;
    let result = client
        .call_tool(ECHO_TOOL_NAME, Some(json!({"user_query": "tls"})), 5000, None)
        .await?;
    assert_eq!(result.content[0].as_text(), Some("Echo: tls"));
    client.close_gracefully().await;
    Ok(())
}

#[tokio::test]
async fn one_way_tls_connection() -> TestResult {
    let Some(certs) = generate_certs() else {
        eprintln!("openssl not available, skipping TLS test");
        return Ok(());
    };
    let (server, endpoint) = start_tls_server(&certs, false).await?;

    // Trusting the CA works.
    let client = tls_client(
        &endpoint,
        TlsConfig {
            ca_file: path(&certs.ca),
            ..Default::default()
        },
    )?;
    echo_round_trip(&client).await?;

    // Disabling peer verification works without the CA.
    let insecure = tls_client(
        &endpoint,
        TlsConfig {
            verify_peer: false,
            ..Default::default()
        },
    )?;
    echo_round_trip(&insecure).await?;

    // SNI override: present "localhost" while connecting to the IP address.
    let sni = tls_client(
        &endpoint,
        TlsConfig {
            ca_file: path(&certs.ca),
            server_name: "localhost".into(),
            ..Default::default()
        },
    )?;
    echo_round_trip(&sni).await?;

    // An untrusted certificate is rejected.
    let untrusted = tls_client(&endpoint, TlsConfig::default())?;
    let err = untrusted.initialize().await.err_or_fail()?;
    assert!(err.message().starts_with("HTTP request failed"), "{err}");
    untrusted.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn mutual_tls_connection() -> TestResult {
    let Some(certs) = generate_certs() else {
        eprintln!("openssl not available, skipping TLS test");
        return Ok(());
    };
    let (server, endpoint) = start_tls_server(&certs, true).await?;

    let client = tls_client(
        &endpoint,
        TlsConfig {
            ca_file: path(&certs.ca),
            cert_file: path(&certs.client_cert),
            key_file: path(&certs.client_key),
            ..Default::default()
        },
    )?;
    echo_round_trip(&client).await?;

    // Without a client certificate the handshake fails.
    let anonymous = tls_client(
        &endpoint,
        TlsConfig {
            ca_file: path(&certs.ca),
            ..Default::default()
        },
    )?;
    let err = anonymous.initialize().await.err_or_fail()?;
    assert!(err.message().starts_with("HTTP request failed"), "{err}");
    anonymous.close_gracefully().await;
    server.stop().await;
    Ok(())
}
