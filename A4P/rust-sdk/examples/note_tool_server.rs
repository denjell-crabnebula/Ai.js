// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A note-management tool server protected by A4P authorization.
//!
//! This is the plain HTTP JSON equivalent of `note_mcp_server.py` (no MCP
//! dependency). Each tool is exposed as `POST /tools/{name}` with a JSON
//! object of arguments and returns the tool result as JSON.
//!
//! ```text
//! A4P_SERVER_BASE_URL=http://127.0.0.1:8961 cargo run -p a4p --example note_tool_server
//! ```

#[path = "note_a4p/notes.rs"]
pub mod notes;

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use notes::NoteService;
use serde_json::{Value, json};

#[derive(Parser, Debug)]
#[command(about = "A4P protected note tool server (HTTP JSON)")]
struct Args {
    /// Bind host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Bind port.
    #[arg(long, default_value_t = 8962)]
    port: u16,
}

async fn list_tools() -> Json<Value> {
    Json(json!({
        "tools": [
            {"name": "list_notes", "description": "List note ids and titles."},
            {"name": "get_note", "description": "Return one note by id.", "arguments": ["note_id"]},
            {"name": "add_note", "description": "Add a note and return the created entry.", "arguments": ["title", "body"]},
            {
                "name": "delete_note",
                "description": "Delete a note after A4P authorization.",
                "arguments": ["note_id", "operation_authorization?", "intent_token?"],
            },
        ]
    }))
}

async fn call_tool(
    State(service): State<Arc<NoteService>>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let arguments = body
        .and_then(|Json(value)| value.as_object().cloned())
        .unwrap_or_default();
    match service.call_tool(&name, &arguments).await {
        Ok(result) => (StatusCode::OK, Json(result)),
        Err(error) if error.starts_with("unknown tool") => {
            (StatusCode::NOT_FOUND, Json(json!({"error": error})))
        }
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({"error": error}))),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let service = Arc::new(NoteService::new(None));
    let router = Router::new()
        .route("/tools", get(list_tools))
        .route("/tools/{name}", post(call_tool))
        .with_state(service);
    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    println!("[Note Tool Server] http://{}", listener.local_addr()?);
    println!("[Note Tool Server] A4P server: {}", a4p::default_a4p_base_url());
    axum::serve(listener, router).await?;
    Ok(())
}
