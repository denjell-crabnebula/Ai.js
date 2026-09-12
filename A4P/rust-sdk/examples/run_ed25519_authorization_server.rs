// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Run an A4P Server configured for the Ed25519 user signature method.
//!
//! The server keeps credentials, pending authorizations and token usage in
//! memory, and writes its public trust configuration to a JSON file so a
//! local User Authorizer (for example the wasm example in
//! `rust/agent-protocol-wasm/examples`) can verify Server-signed mandates.
//!
//! ```text
//! cargo run -p a4p --example run_ed25519_authorization_server -- --port 8961
//! ```
//!
//! Stop it with Ctrl-C.

use std::sync::Arc;

use a4p::A4PServer;
use a4p::credential_store::InMemoryCredentialStore;
use a4p::http_server::A4PHTTPServer;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::user_signature::ed25519::RegisteredEd25519Method;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "A4P Server with the Ed25519 user signature method")]
struct Args {
    /// A4P server host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// A4P server port (0 picks a free port).
    #[arg(long, default_value_t = 8961)]
    port: u16,
    /// Server id written into every mandate and used for trust lookup.
    #[arg(long, default_value = "local://a4p")]
    server_id: String,
    /// Write the Server's public trust configuration to this JSON file.
    #[arg(long, default_value = ".a4p/ed25519_trusted_server_keys.json")]
    trusted_keys_output: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let signature_method = RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new()));
    let a4p_server = A4PServer::builder()
        .server_id(&args.server_id)
        .user_signature_method(Arc::new(signature_method))
        .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
        .build()?;

    let trust_config = a4p_server.server_trust_config()?;
    let trusted_keys_path = std::path::Path::new(&args.trusted_keys_output);
    if let Some(parent) = trusted_keys_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(
        trusted_keys_path,
        format!("{}\n", serde_json::to_string_pretty(&trust_config)?),
    )?;

    let http = A4PHTTPServer::new(Arc::new(a4p_server), Some(&args.host), Some(args.port))?;
    http.start().await?;
    println!("[A4P Server] HTTP server: http://{}:{}", http.host(), http.port());
    println!("[A4P Server] Signature method: ed25519");
    println!(
        "[A4P Server] Local trust configuration: {}",
        args.trusted_keys_output
    );
    println!("[A4P Server] Press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    http.stop().await;
    Ok(())
}
