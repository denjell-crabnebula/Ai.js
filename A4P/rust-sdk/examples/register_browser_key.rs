// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Register a browser WebAuthn credential for the local A4P demo.
//!
//! Relays one registration ceremony: creation options from the A4P Server,
//! the browser credential from the local User Authorizer, and the verified
//! registration back to the A4P Server.
//!
//! ```text
//! cargo run -p a4p --example register_browser_key
//! ```

use a4p::A4PClient;
use a4p::JsonDict;
use serde_json::{Value, json};

const USER_ID: &str = "demo-user";

fn user_authorizer_base_url() -> String {
    ap_support::env::current()
        .get_non_blank("A4P_USER_AUTHORIZER_BASE_URL")
        .unwrap_or_else(|| "http://localhost:8970".to_string())
        .trim_end_matches('/')
        .to_string()
}

async fn post_user_authorizer(path: &str, payload: &Value) -> Result<Value, String> {
    let url = format!("{}{path}", user_authorizer_base_url());
    let response = reqwest::Client::new()
        .post(&url)
        .json(payload)
        .send()
        .await
        .map_err(|error| format!("User Authorizer HTTP request failed: {error}"))?;
    let status = response.status();
    let raw = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("User Authorizer HTTP {}: {raw}", status.as_u16()));
    }
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

/// The map inside a `json!({ ... })` object literal.
fn object(value: Value) -> JsonDict {
    match value {
        Value::Object(map) => map,
        _ => JsonDict::new(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a4p_client = A4PClient::new();
    let options_payload = a4p_client
        .webauthn_registration_options(&object(json!({
            "userId": USER_ID,
            "userName": USER_ID,
            "userDisplayName": "A4P Note Demo User",
        })))
        .await?;
    let registration_request_id = options_payload["registrationRequestId"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let creation_options = options_payload["options"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let local_result = post_user_authorizer(
        "/register",
        &json!({
            "registrationRequestId": registration_request_id,
            "userId": USER_ID,
            "creationOptions": creation_options,
        }),
    )
    .await?;
    let Some(credential) = local_result.get("credential").filter(|value| value.is_object()) else {
        return Err(format!("Local browser key registration failed: {local_result}").into());
    };
    let verified = a4p_client
        .verify_webauthn_registration(&object(json!({
            "registrationRequestId": registration_request_id,
            "userId": USER_ID,
            "credential": credential,
        })))
        .await?;
    println!(
        "[enrollment] registered browser credential: {}",
        verified["credential"]["credentialId"]
    );
    Ok(())
}
