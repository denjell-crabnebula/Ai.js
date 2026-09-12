// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Non-interactive smoke tests for the A4P note tool example.
//!
//! Runs the operation and intent flows in-process against a temporary A4P
//! HTTP server with an explicit Ed25519 test signer. No browser, passkey or
//! user interaction is needed.
//!
//! ```text
//! cargo run -p a4p --example smoke_test
//! ```

#[path = "note_a4p/notes.rs"]
pub mod notes;

use std::sync::Arc;

use a4p::JsonDict;
use a4p::credential_store::InMemoryCredentialStore;
use a4p::http_server::A4PHTTPServer;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::{Ed25519UserSigner, RegisteredEd25519Method, ed25519_public_jwk};
use a4p::{A4PClient, A4PServer, sign_user_mandate_with_signer};
use notes::NoteService;
use serde_json::{Value, json};

type Error = Box<dyn std::error::Error>;

fn check(condition: bool, message: &str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(format!("smoke check failed: {message}").into())
    }
}

/// Start a temporary A4P HTTP server with a registered Ed25519 test signer.
async fn explicit_test_server() -> Result<(A4PHTTPServer, Ed25519UserSigner, NoteService), Error> {
    let private_key = generate_ed25519_private_key();
    let signature_method = RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new()));
    let registration = signature_method.register(&object(json!({
        "userId": "demo-user",
        "publicKey": ed25519_public_jwk(&private_key),
        "metadata": {"purpose": "non-interactive smoke test"},
    })))?;
    let signer = Ed25519UserSigner::new(
        registration["credential"]["credentialId"]
            .as_str()
            .unwrap_or_default(),
        private_key,
    )?;
    let server = A4PServer::builder()
        .server_id("local://note-a4p-demo")
        .user_signature_method(Arc::new(signature_method))
        .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
        .build()?;
    let http = A4PHTTPServer::new(Arc::new(server), Some("127.0.0.1"), Some(0))?;
    http.start().await?;
    let notes = NoteService::new(Some(format!("http://127.0.0.1:{}", http.port())));
    notes.reset_for_smoke_test();
    Ok((http, signer, notes))
}

async fn operation_delete_succeeds() -> Result<(), Error> {
    let (http, signer, notes) = explicit_test_server().await?;
    let result = async {
        let challenged = notes.delete_note("note-1", None, None).await;
        check(
            challenged["status"] == "authorization_required",
            "first call must challenge",
        )?;
        check(notes.has_note("note-1"), "challenge must not delete")?;
        let mandate = challenged["authorization"]["mandate"]
            .as_object()
            .ok_or("mandate missing")?;
        let signed = sign_user_mandate_with_signer(mandate, &signer, None)?;
        let authorization = object(json!({"signedMandate": signed}));
        let result = notes.delete_note("note-1", Some(&authorization), None).await;
        check(result["deleted"] == Value::Bool(true), "delete must succeed")?;
        check(
            result["operationId"] == signed["operationId"],
            "operationId must match",
        )?;
        check(!notes.has_note("note-1"), "note-1 must be gone")?;
        // Idempotent replay of the business result by operationId.
        let replay = notes.delete_note("note-1", Some(&authorization), None).await;
        check(
            replay["deleted"] == Value::Bool(false),
            "replayed authorization is consumed",
        )?;
        Ok::<(), Error>(())
    }
    .await;
    http.stop().await;
    result
}

async fn operation_params_mismatch_fails() -> Result<(), Error> {
    let (http, signer, notes) = explicit_test_server().await?;
    let result = async {
        let challenged = notes.delete_note("note-1", None, None).await;
        let mandate = challenged["authorization"]["mandate"]
            .as_object()
            .ok_or("mandate missing")?;
        let signed = sign_user_mandate_with_signer(mandate, &signer, None)?;
        let authorization = object(json!({"signedMandate": signed}));
        let result = notes.delete_note("note-2", Some(&authorization), None).await;
        check(
            result["deleted"] == Value::Bool(false),
            "mismatched operation must fail",
        )?;
        check(
            result["error"]
                .as_str()
                .unwrap_or_default()
                .contains("does not match pending"),
            "mismatch reason",
        )?;
        check(
            notes.has_note("note-1") && notes.has_note("note-2"),
            "no note deleted",
        )?;
        Ok::<(), Error>(())
    }
    .await;
    http.stop().await;
    result
}

async fn intent_token_deletes_multiple_notes() -> Result<(), Error> {
    let (http, signer, notes) = explicit_test_server().await?;
    let result = async {
        let client = A4PClient::with_base_url(&format!("http://127.0.0.1:{}", http.port()));
        let prepared = client
            .prepare_intent_authorization(json!({
                "agentId": "demo-agent",
                "userId": "demo-user",
                "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]},
                "validitySeconds": 60,
            }))
            .await?;
        let mandate = prepared.mandate.ok_or("intent mandate missing")?;
        let signed = sign_user_mandate_with_signer(&mandate, &signer, None)?;
        let auth = client
            .complete_intent_authorization(json!({"signedMandate": signed}))
            .await?;
        let token = auth.intent_token.ok_or("intent token missing")?;
        let first = notes.delete_note("note-1", None, Some(&token)).await;
        let second = notes.delete_note("note-2", None, Some(&token)).await;
        check(first["deleted"] == Value::Bool(true), "first intent delete")?;
        check(second["deleted"] == Value::Bool(true), "second intent delete")?;
        check(notes.list_notes() == json!([]), "all notes deleted")?;
        Ok::<(), Error>(())
    }
    .await;
    http.stop().await;
    result
}

async fn intent_constraints_reject_mismatches() -> Result<(), Error> {
    let (http, signer, _notes) = explicit_test_server().await?;
    let result = async {
        let client = A4PClient::with_base_url(&format!("http://127.0.0.1:{}", http.port()));
        let prepared = client
            .prepare_intent_authorization(json!({
                "agentId": "demo-agent",
                "userId": "demo-user",
                "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "note-1"}}]},
                "validitySeconds": 60,
            }))
            .await?;
        let mandate = prepared.mandate.ok_or("intent mandate missing")?;
        let signed = sign_user_mandate_with_signer(&mandate, &signer, None)?;
        let auth = client
            .complete_intent_authorization(json!({"signedMandate": signed}))
            .await?;
        let token = auth.intent_token.ok_or("intent token missing")?;
        let expected = |action: &str, params: Value| {
            json!({
                "token": token,
                "expected": {
                    "action": action,
                    "params": params,
                    "agentId": "agent:demo-agent",
                    "userId": "demo-user",
                },
            })
        };
        let missing = client
            .verify_intent_token(expected("delete_note", json!({})))
            .await?;
        let extra = client
            .verify_intent_token(expected(
                "delete_note",
                json!({"note_id": "note-1", "force": true}),
            ))
            .await?;
        let action_mismatch = client
            .verify_intent_token(expected("archive_note", json!({"note_id": "note-1"})))
            .await?;
        let matching = client
            .verify_intent_token(expected("delete_note", json!({"note_id": "note-1"})))
            .await?;
        check(!missing.valid, "missing params rejected")?;
        check(!extra.valid, "extra params rejected")?;
        check(!action_mismatch.valid, "action mismatch rejected")?;
        check(matching.valid, "matching call accepted")?;
        Ok::<(), Error>(())
    }
    .await;
    http.stop().await;
    result
}

async fn run_step(name: &str, step: impl std::future::Future<Output = Result<(), Error>>) -> bool {
    match step.await {
        Ok(()) => {
            println!("[smoke] {name}: ok");
            true
        }
        Err(error) => {
            eprintln!("[smoke] {name}: FAILED: {error}");
            false
        }
    }
}

/// The map inside a `json!({ ... })` object literal.
fn object(value: Value) -> JsonDict {
    match value {
        Value::Object(map) => map,
        _ => JsonDict::new(),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let passed = run_step("operation delete succeeds", operation_delete_succeeds()).await
        && run_step(
            "operation params mismatch fails",
            operation_params_mismatch_fails(),
        )
        .await
        && run_step(
            "intent token deletes multiple notes",
            intent_token_deletes_multiple_notes(),
        )
        .await
        && run_step(
            "intent constraints reject mismatches",
            intent_constraints_reject_mismatches(),
        )
        .await;
    if !passed {
        return std::process::ExitCode::FAILURE;
    }
    println!("note_a4p smoke tests passed");
    std::process::ExitCode::SUCCESS
}
