// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Run the standalone A4P authorization server for the note demo.
//!
//! Uses `WebAuthnSignatureMethod` with a JSON file credential store and
//! writes the Server's public trust configuration for the local User
//! Authorizer.
//!
//! ```text
//! cargo run -p a4p --example run_authorization_server -- --port 8961
//! ```

use std::sync::Arc;

use a4p::A4PServer;
use a4p::credential_store::JsonFileCredentialStore;
use a4p::http_server::A4PHTTPServer;
use a4p::user_signature::webauthn::WebAuthnSignatureMethod;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Standalone A4P authorization server for the note demo")]
struct Args {
    /// A4P server host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// A4P server port.
    #[arg(long, default_value_t = 8961)]
    port: u16,
    /// Write the demo Server's public trust configuration to this JSON file.
    #[arg(long, default_value = ".a4p/trusted_server_keys.json")]
    trusted_keys_output: String,
    /// JSON credential store path.
    #[arg(long, default_value = ".a4p/webauthn_credentials.json")]
    credential_store: String,
    /// WebAuthn relying party id.
    #[arg(long, default_value = "localhost")]
    rp_id: String,
    /// Expected browser origin of the User Authorizer page.
    #[arg(long, default_value = "http://localhost:8970")]
    expected_origin: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ap_support::logging::init_from_env();
    let args = Args::parse();
    let credential_store = Arc::new(JsonFileCredentialStore::new(&args.credential_store)?);
    let signature_method = WebAuthnSignatureMethod::new(credential_store)
        .with_rp_id(&args.rp_id)
        .with_rp_name("A4P Note Demo")
        .with_expected_origin(&args.expected_origin);
    let a4p_server = A4PServer::builder()
        .server_id("local://note-a4p-demo")
        .user_signature_method(Arc::new(signature_method))
        .build()?;
    let trusted_keys_path = std::path::Path::new(&args.trusted_keys_output);
    if let Some(parent) = trusted_keys_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let trust_config = a4p_server.server_trust_config()?;
    std::fs::write(
        trusted_keys_path,
        format!("{}\n", serde_json::to_string_pretty(&trust_config)?),
    )?;
    let http = A4PHTTPServer::new(Arc::new(a4p_server), Some(&args.host), Some(args.port))?;
    http.start().await?;
    println!("[A4P Server] HTTP server: http://{}:{}", http.host(), http.port());
    println!(
        "[A4P Server] Local trust configuration: {}",
        args.trusted_keys_output
    );
    println!("[A4P Server] User Authorizer is external; use prepare/complete authorization flow.");
    tokio::signal::ctrl_c().await?;
    http.stop().await;
    Ok(())
}
