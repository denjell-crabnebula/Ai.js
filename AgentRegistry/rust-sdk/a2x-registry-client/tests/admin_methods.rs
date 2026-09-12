// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `test_admin_methods.py`: `create_dataset` with auth and lease
//! config inline, and `create_principal` for issuing API keys.

pub mod common;

use a2x_registry_client::{
    A2xRegistryClient, ClientConfig, ClientError, CreateDatasetOptions, Formats, OwnershipFile,
};
use ap_support::testing::{OptionExt, ResultExt, TestResult};
use common::{MockResponse, MockServer};
use serde_json::json;

fn client_for(server: &MockServer) -> TestResult<A2xRegistryClient> {
    Ok(A2xRegistryClient::new(
        ClientConfig::new()
            .base_url(&server.base_url)
            .api_key("a2x_pat_admin")
            .ownership_file(OwnershipFile::Disabled),
    )?)
}

fn dataset_reply(name: &str) -> serde_json::Value {
    json!({
        "dataset": name, "embedding_model": "all-MiniLM-L6-v2",
        "formats": {"a2a": "v0.0"}, "status": "created",
    })
}

#[tokio::test]
async fn create_dataset_legacy_body_byte_equal() -> TestResult {
    let server = MockServer::json(200, dataset_reply("ds1"))?;
    let client = client_for(&server)?;
    let resp = client
        .create_dataset("ds1", &CreateDatasetOptions::default())
        .await?;
    assert_eq!(resp.dataset, "ds1");
    assert_eq!(resp.status, "created");

    let req = server.last()?;
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/datasets");
    assert_eq!(req.headers["authorization"], "Bearer a2x_pat_admin");
    let body = req.body.required()?;
    assert_eq!(body["name"], "ds1");
    assert_eq!(body["embedding_model"], "all-MiniLM-L6-v2");
    assert_eq!(body["formats"], json!({"a2a": "v0.0"}));
    assert!(body.get("auth_required").is_none(), "{body}");
    assert!(body.get("lease_config").is_none(), "{body}");
    Ok(())
}

#[tokio::test]
async fn create_dataset_with_auth_required_only() -> TestResult {
    let server = MockServer::json(200, dataset_reply("secure"))?;
    let client = client_for(&server)?;
    let opts = CreateDatasetOptions {
        auth_required: true,
        ..Default::default()
    };
    client.create_dataset("secure", &opts).await?;
    let body = server.last()?.body.required()?;
    assert_eq!(body["auth_required"], true);
    assert!(body.get("lease_config").is_none());
    Ok(())
}

#[tokio::test]
async fn create_dataset_with_inline_lease_config() -> TestResult {
    let server = MockServer::json(200, dataset_reply("hb"))?;
    let client = client_for(&server)?;
    let lease = json!({"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60});
    let opts = CreateDatasetOptions {
        lease_config: lease.as_object().cloned(),
        ..Default::default()
    };
    client.create_dataset("hb", &opts).await?;
    let cfg = server.last()?.body.required()?["lease_config"].clone();
    assert_eq!(cfg["enabled"], true);
    assert_eq!(cfg["min_ttl"], 10);
    assert_eq!(cfg["grace_period"], 60);
    Ok(())
}

#[tokio::test]
async fn create_dataset_with_both_auth_and_lease() -> TestResult {
    let server = MockServer::json(200, dataset_reply("translators"))?;
    let client = client_for(&server)?;
    let lease = json!({"enabled": true, "min_ttl": 10, "max_ttl": 600, "grace_period": 60});
    let opts = CreateDatasetOptions {
        auth_required: true,
        lease_config: lease.as_object().cloned(),
        ..Default::default()
    };
    client.create_dataset("translators", &opts).await?;
    let body = server.last()?.body.required()?;
    assert_eq!(body["auth_required"], true);
    assert_eq!(body["lease_config"]["enabled"], true);
    Ok(())
}

#[tokio::test]
async fn create_dataset_formats_omit_and_explicit() -> TestResult {
    let server = MockServer::json(200, dataset_reply("f"))?;
    let client = client_for(&server)?;
    let opts = CreateDatasetOptions {
        formats: Formats::Omit,
        ..Default::default()
    };
    client.create_dataset("f", &opts).await?;
    assert!(server.last()?.body.required()?.get("formats").is_none());

    let explicit = json!({"generic": "v0.0", "skill": "v0.0"});
    let opts = CreateDatasetOptions {
        formats: Formats::Explicit(explicit.as_object().cloned().required()?),
        embedding_model: "bge-small-zh-v1.5".into(),
        ..Default::default()
    };
    client.create_dataset("f", &opts).await?;
    let body = server.last()?.body.required()?;
    assert_eq!(body["formats"], explicit);
    assert_eq!(body["embedding_model"], "bge-small-zh-v1.5");
    Ok(())
}

#[tokio::test]
async fn create_principal_provider_role() -> TestResult {
    let server = MockServer::json(
        201,
        json!({
            "principal_id": "u_abc123", "handle": "alice", "role": "provider",
            "namespaces": ["translators"], "key_id": "k_xyz789", "key_prefix": "a2x_pat_yfVE",
            "token": "a2x_pat_yfVEn1qRbHfeomIHxwAoa74ImKh_Y286OhEWORcRC1U",
        }),
    )?;
    let client = client_for(&server)?;
    let ns = vec!["translators".to_string()];
    let resp = client
        .create_principal("alice", "provider", Some(&ns), None)
        .await?;
    assert_eq!(resp.principal_id, "u_abc123");
    assert_eq!(resp.role, "provider");
    assert_eq!(resp.namespaces, Some(ns));
    assert!(resp.token.starts_with("a2x_pat_"));

    let req = server.last()?;
    assert_eq!(req.path, "/api/auth/principals");
    let body = req.body.required()?;
    assert_eq!(body["handle"], "alice");
    assert_eq!(body["role"], "provider");
    assert_eq!(body["namespaces"], json!(["translators"]));
    assert!(body.get("note").is_none());
    Ok(())
}

#[tokio::test]
async fn create_principal_admin_role_no_namespaces() -> TestResult {
    let server = MockServer::json(
        201,
        json!({
            "principal_id": "u_root2", "handle": "root2", "role": "admin",
            "namespaces": null, "key_id": "k_a", "key_prefix": "a2x_pat_aaaa", "token": "a2x_pat_aaaaaaaa",
        }),
    )?;
    let client = client_for(&server)?;
    let resp = client.create_principal("root2", "admin", None, None).await?;
    assert_eq!(resp.namespaces, None);
    assert!(server.last()?.body.required()?.get("namespaces").is_none());
    Ok(())
}

#[tokio::test]
async fn create_principal_with_note() -> TestResult {
    let server = MockServer::json(
        201,
        json!({
            "principal_id": "u_x", "handle": "ops", "role": "user", "namespaces": ["ds1"],
            "key_id": "k_x", "key_prefix": "a2x_pat_xxxx", "token": "a2x_pat_xxxxxxxx",
        }),
    )?;
    let client = client_for(&server)?;
    let ns = vec!["ds1".to_string()];
    client
        .create_principal("ops", "user", Some(&ns), Some("ticket-1234"))
        .await?;
    assert_eq!(server.last()?.body.required()?["note"], "ticket-1234");
    Ok(())
}

#[tokio::test]
async fn create_principal_propagates_validation_error() -> TestResult {
    let server = MockServer::start(|_| {
        Ok(MockResponse::json(
            400,
            json!({"detail": "handle 'dup' already in use"}),
        ))
    })?;
    let client = client_for(&server)?;
    let ns = vec!["ds1".to_string()];
    let err = client
        .create_principal("dup", "user", Some(&ns), None)
        .await
        .err_or_fail()?;
    assert!(
        matches!(err, ClientError::Validation { status: 400, .. }),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "HTTP 400: handle 'dup' already in use");
    Ok(())
}
