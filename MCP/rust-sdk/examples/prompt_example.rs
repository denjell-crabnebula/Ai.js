// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `example/client_example/prompt_example/prompt_example.cpp`.
//!
//! Run the server example first, then:
//! `cargo run -p mcp-sdk --example prompt_example -- [--port=<1-65535>]`

use std::sync::Arc;

use mcp_sdk::log::{McpLogLevel, set_log_callback, set_log_level};
use mcp_sdk::mcp_log;
use mcp_sdk::prelude::*;
use serde_json::json;

const EXAMPLE_ENDPOINT: &str = "http://localhost:8000/mcp";
const EXAMPLE_TOKEN: &str = "your-token";

fn stdout_log(_level: McpLogLevel, message: String) {
    println!("{message}");
}

fn parse_endpoint() -> Result<String, String> {
    let mut endpoint = EXAMPLE_ENDPOINT.to_string();
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--port=") {
            let port: u16 = value.parse().map_err(|_| format!("Invalid port: {value}"))?;
            endpoint = format!("http://localhost:{port}/mcp");
        }
    }
    Ok(endpoint)
}

async fn run(endpoint: String) -> Result<(), McpError> {
    let config = ClientConfig {
        name: "PromptExampleClient".into(),
        version: "1.0.0".into(),
    };
    let http = StreamableHttpClientConfig {
        endpoint,
        tls_config: TlsConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let auth: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new(EXAMPLE_TOKEN));
    let client = McpClientFactory::create_streamable_http_client(config, http, Some(auth))?;

    mcp_log!(McpLogLevel::Info, "=== Example Initialize ===");
    client.initialize().await.map_err(|e| {
        mcp_log!(McpLogLevel::Error, "Initialize failed: {e}");
        e
    })?;
    mcp_log!(McpLogLevel::Info, "Initialize success");

    // Example 1: List prompts
    mcp_log!(McpLogLevel::Info, "=== Example ListPrompts ===");
    let prompts = client.list_prompts().await.map_err(|e| {
        mcp_log!(McpLogLevel::Error, "Prompt example failed: {e}");
        e
    })?;
    mcp_log!(
        McpLogLevel::Info,
        "ListPrompts success, prompt count: {}",
        prompts.prompts.len()
    );
    for prompt in &prompts.prompts {
        match &prompt.arguments {
            Some(args) => mcp_log!(
                McpLogLevel::Info,
                "  Prompt: {} (arguments: {})",
                prompt.name,
                args.len()
            ),
            None => mcp_log!(McpLogLevel::Info, "  Prompt: {} (no arguments)", prompt.name),
        }
    }

    // Example 2: Get prompt (if available)
    if let Some(first) = prompts.prompts.first() {
        mcp_log!(McpLogLevel::Info, "=== Example GetPrompt ===");
        let args = first
            .arguments
            .as_ref()
            .filter(|a| !a.is_empty())
            .map(|_| json!({"name": "friend", "language": "English"}));
        let detail = client.get_prompt(&first.name, args).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Prompt example failed: {e}");
            e
        })?;
        mcp_log!(
            McpLogLevel::Info,
            "GetPrompt success, prompt: {}, message count: {}",
            first.name,
            detail.messages.len()
        );
    }

    client.close_gracefully().await;
    mcp_log!(McpLogLevel::Info, "=== Example completed ===");
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    set_log_level(McpLogLevel::Info as u8);
    set_log_callback(Some(stdout_log));
    let endpoint = match parse_endpoint() {
        Ok(endpoint) => endpoint,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match run(endpoint).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
