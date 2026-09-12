// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Simulate an AI agent that deletes notes through the note tool server after A4P authorization.
//!
//! The agent talks to the note tool server (`note_tool_server` example) over
//! HTTP, forwards mandates to the local User Authorizer (`run_user_authorizer`
//! example) and, in intent mode, obtains an intent token from the A4P Server.
//!
//! ```text
//! cargo run -p a4p --example agent_simulator -- --mode operation
//! cargo run -p a4p --example agent_simulator -- --mode intent
//! ```

use a4p::{A4PClient, JsonDict};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};

const AGENT_ID: &str = "demo-agent";
const USER_ID: &str = "demo-user";

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    Operation,
    Intent,
}

#[derive(Parser, Debug)]
#[command(about = "Simulated A4P agent")]
struct Args {
    /// Authorization mode used before deleting notes.
    #[arg(long, value_enum, default_value_t = Mode::Operation)]
    mode: Mode,
    /// Base URL of the note tool server.
    #[arg(long, env = "A4P_NOTE_TOOL_BASE_URL", default_value = "http://127.0.0.1:8962")]
    tool_base_url: String,
    /// Base URL of the local User Authorizer.
    #[arg(
        long,
        env = "A4P_USER_AUTHORIZER_BASE_URL",
        default_value = "http://localhost:8970"
    )]
    user_authorizer_base_url: String,
}

struct Agent {
    http: reqwest::Client,
    tool_base_url: String,
    user_authorizer_base_url: String,
}

impl Agent {
    async fn post_json(&self, url: &str, payload: &Value) -> Result<Value, String> {
        let response = self
            .http
            .post(url)
            .json(payload)
            .send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|error| error.to_string())
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let url = format!("{}/tools/{name}", self.tool_base_url.trim_end_matches('/'));
        self.post_json(&url, &arguments).await
    }

    async fn request_user_signature(
        &self,
        mandate: &JsonDict,
        signing_options: &JsonDict,
    ) -> Result<Value, String> {
        let url = format!(
            "{}/authorize",
            self.user_authorizer_base_url.trim_end_matches('/')
        );
        self.post_json(
            &url,
            &json!({"mandate": mandate, "signingOptions": signing_options}),
        )
        .await
    }

    async fn delete_with_operation(&self, notes: &[Value]) {
        for note in notes {
            let note_id = note["id"].as_str().unwrap_or_default().to_string();
            println!("\n[agent] requesting operation authorization for {note_id}");
            let challenged = match self.call_tool("delete_note", json!({"note_id": note_id})).await {
                Ok(value) => value,
                Err(error) => {
                    println!("[agent] operation challenge failed for {note_id}: {error}");
                    continue;
                }
            };
            let Some(mandate) = challenged["authorization"]["mandate"].as_object() else {
                println!("[agent] operation challenge failed for {note_id}: {challenged}");
                continue;
            };
            let signing_options = challenged["authorization"]["signingOptions"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let user_auth = match self.request_user_signature(mandate, &signing_options).await {
                Ok(value) => value,
                Err(error) => {
                    println!("[agent] user authorization failed for {note_id}: {error}");
                    continue;
                }
            };
            if user_auth["approved"] != Value::Bool(true) || !user_auth["signedMandate"].is_object() {
                println!(
                    "[agent] user authorization rejected for {note_id}: {}",
                    user_auth["rejectReason"]
                );
                continue;
            }
            let deleted = self
                .call_tool(
                    "delete_note",
                    json!({
                        "note_id": note_id,
                        "operation_authorization": {"signedMandate": user_auth["signedMandate"]},
                    }),
                )
                .await;
            println!("[agent] delete result: {deleted:?}");
        }
    }

    async fn delete_with_intent(&self, a4p_client: &A4PClient, notes: &[Value]) {
        println!("\n[agent] requesting one intent authorization for deleting notes");
        let prepared = match a4p_client
            .prepare_intent_authorization(json!({
                "agentId": AGENT_ID,
                "userId": USER_ID,
                "intent": {"actions": [{"name": "delete_note", "params": {"note_id": "*"}}]},
                "validitySeconds": 600,
                "metadata": {"reason": "The simulated agent wants to delete all listed notes."},
            }))
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                println!("[agent] intent prepare failed: {error}");
                return;
            }
        };
        let Some(mandate) = prepared.mandate else {
            println!("[agent] intent mandate rejected: {:?}", prepared.reject_reason);
            return;
        };
        let user_auth = match self
            .request_user_signature(&mandate, &prepared.signing_options)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                println!("[agent] user intent authorization failed: {error}");
                return;
            }
        };
        if user_auth["approved"] != Value::Bool(true) || !user_auth["signedMandate"].is_object() {
            println!(
                "[agent] user intent authorization rejected: {}",
                user_auth["rejectReason"]
            );
            return;
        }
        let auth = match a4p_client
            .complete_intent_authorization(json!({"signedMandate": user_auth["signedMandate"]}))
            .await
        {
            Ok(auth) => auth,
            Err(error) => {
                println!("[agent] intent complete failed: {error}");
                return;
            }
        };
        let Some(token) = auth.intent_token.filter(|_| auth.approved) else {
            println!("[agent] intent authorization rejected: {:?}", auth.reject_reason);
            return;
        };
        for note in notes {
            let note_id = note["id"].as_str().unwrap_or_default();
            let deleted = self
                .call_tool("delete_note", json!({"note_id": note_id, "intent_token": token}))
                .await;
            println!("[agent] delete result for {note_id}: {deleted:?}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let agent = Agent {
        http: reqwest::Client::new(),
        tool_base_url: args.tool_base_url,
        user_authorizer_base_url: args.user_authorizer_base_url,
    };
    let a4p_client = A4PClient::new();

    let notes = agent.call_tool("list_notes", json!({})).await?;
    println!("[agent] listed notes: {notes}");
    let notes = notes.as_array().cloned().unwrap_or_default();
    if notes.is_empty() {
        println!("[agent] no notes to delete");
        return Ok(());
    }
    match args.mode {
        Mode::Operation => agent.delete_with_operation(&notes).await,
        Mode::Intent => agent.delete_with_intent(&a4p_client, &notes).await,
    }
    let remaining = agent.call_tool("list_notes", json!({})).await?;
    println!("\n[agent] remaining notes: {remaining}");
    Ok(())
}
