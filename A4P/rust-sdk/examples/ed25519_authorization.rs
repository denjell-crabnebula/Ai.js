// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Run a complete one-request Ed25519 enrollment and authorization flow.
//!
//! Starts a temporary A4P HTTP server on a loopback port, registers a fresh
//! Ed25519 key for `user-1`, prepares an operation mandate, signs it with the
//! caller-owned private key and completes the authorization.
//!
//! ```text
//! cargo run -p a4p --example ed25519_authorization
//! ```

use std::sync::Arc;

use a4p::JsonDict;
use a4p::credential_store::InMemoryCredentialStore;
use a4p::http_server::A4PHTTPServer;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::{Ed25519UserSigner, RegisteredEd25519Method, ed25519_public_jwk};
use a4p::{A4PClient, A4PServer, sign_user_mandate_with_signer};
use serde_json::{Value, json};

/// The map inside a `json!({ ... })` object literal.
fn object(value: Value) -> JsonDict {
    match value {
        Value::Object(map) => map,
        _ => JsonDict::new(),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Ed25519 authorization example failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let signature_method = Arc::new(RegisteredEd25519Method::new(Arc::new(
        InMemoryCredentialStore::new(),
    )));
    let a4p_server = A4PServer::builder()
        .server_id("local://ed25519-example")
        .user_signature_method(signature_method)
        .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
        .build()?;
    let http_server = A4PHTTPServer::new(Arc::new(a4p_server), Some("127.0.0.1"), Some(0))?;
    http_server.start().await?;
    let result = flow(&http_server).await;
    http_server.stop().await;
    result
}

async fn flow(http_server: &A4PHTTPServer) -> Result<(), Box<dyn std::error::Error>> {
    let client = A4PClient::with_base_url(&format!("http://127.0.0.1:{}", http_server.port()));

    // The caller owns this private key. The SDK neither persists nor unlocks it.
    let private_key = generate_ed25519_private_key();
    let registration = client
        .register_ed25519_credential(&object(json!({
            "userId": "user-1",
            "publicKey": ed25519_public_jwk(&private_key),
            "metadata": {"label": "temporary CLI example key"},
        })))
        .await?;
    let credential_id = registration["credential"]["credentialId"]
        .as_str()
        .ok_or("registration did not return a credentialId")?;
    let signer = Ed25519UserSigner::new(credential_id, private_key)?;

    let operation = json!({"action": "delete_note", "params": {"note_id": "note-1"}});
    let prepared = client
        .prepare_operation_authorization(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "operation": operation,
            "validitySeconds": 60,
        }))
        .await?;
    let mandate = prepared
        .mandate
        .ok_or_else(|| prepared.reject_reason.unwrap_or_else(|| "prepare failed".into()))?;
    let signed_mandate = sign_user_mandate_with_signer(&mandate, &signer, None)?;
    let completed = client
        .complete_operation_authorization(json!({"signedMandate": signed_mandate, "operation": operation}))
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "credentialId": signer.credential_id(),
            "approved": completed.approved,
            "operationId": completed.operation_id,
        }))?
    );
    Ok(())
}
