// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `example/client_example/sampling_example/sampling_example.cpp`.
//!
//! The server calls back into this client with `sampling/createMessage`
//! while it runs the `sampling_echo` tool.
//!
//! Run the server example first, then:
//! `cargo run -p mcp-sdk --example sampling_example -- [--port=<1-65535>]`

use std::sync::Arc;

use mcp_sdk::log::{McpLogLevel, set_log_callback, set_log_level};
use mcp_sdk::mcp_log;
use mcp_sdk::prelude::*;
use serde_json::json;

const DEFAULT_ENDPOINT: &str = "http://localhost:8000/mcp";
const AUTH_TOKEN: &str = "your-token";
const DEFAULT_MAX_TOKENS: u64 = 100;
const SAMPLE_PROMPT: &str = "Tell me a joke about programming";
const SAMPLE_TOOL_NAME: &str = "sampling_echo";
const MODEL_NAME: &str = "demo-client-model";

fn stdout_log(_level: McpLogLevel, message: String) {
    println!("{message}");
}

fn print_header() {
    mcp_log!(McpLogLevel::Info, "=== Sampling Client Example ===");
    mcp_log!(
        McpLogLevel::Info,
        "This example demonstrates server-to-client sampling."
    );
    mcp_log!(
        McpLogLevel::Info,
        "The server will send sampling requests to this client."
    );
    mcp_log!(McpLogLevel::Info, "");
}

fn log_sampling_block(block: &SamplingMessageContentBlock) {
    match block {
        SamplingMessageContentBlock::Text(text) => mcp_log!(McpLogLevel::Info, "    Text: {}", text.text),
        SamplingMessageContentBlock::Image(image) => {
            mcp_log!(McpLogLevel::Info, "    Image: {}", image.mime_type)
        }
        SamplingMessageContentBlock::Audio(audio) => {
            mcp_log!(McpLogLevel::Info, "    Audio: {}", audio.mime_type)
        }
        _ => {}
    }
}

fn log_sampling_message(msg: &SamplingMessage) {
    let role = match msg.role {
        RoleType::User => "USER",
        RoleType::Assistant => "ASSISTANT",
    };
    mcp_log!(McpLogLevel::Info, "  Message [{role}]:");
    match &msg.content {
        SamplingContent::Single(block) => log_sampling_block(block),
        SamplingContent::Multiple(blocks) => {
            mcp_log!(McpLogLevel::Info, "    Content blocks count: {}", blocks.len())
        }
    }
}

/// Serve `sampling/createMessage` with a canned answer.
fn handle_sampling_request(params: CreateMessageParams) -> CreateMessageResult {
    mcp_log!(
        McpLogLevel::Info,
        "Received sampling/createMessage request from server"
    );
    mcp_log!(McpLogLevel::Info, "  maxTokens: {}", params.max_tokens);
    for msg in &params.messages {
        log_sampling_message(msg);
    }
    let result = CreateMessageResult {
        model: MODEL_NAME.into(),
        role: RoleType::Assistant,
        stop_reason: Some("stop".into()),
        content: SamplingContent::Single(SamplingMessageContentBlock::Text(TextContent::new(
            "This is a simulated LLM response from sampling client.",
        ))),
        ..Default::default()
    };
    mcp_log!(McpLogLevel::Info, "Returning sampling response to server");
    result
}

fn parse_endpoint() -> Result<Option<String>, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut endpoint = DEFAULT_ENDPOINT.to_string();
    for arg in args.iter().skip(1) {
        if arg == "--help" || arg == "-h" {
            mcp_log!(
                McpLogLevel::Info,
                "Usage: {} [--port=<1-65535>]",
                args.first().map(String::as_str).unwrap_or("SamplingExample")
            );
            return Ok(None);
        }
        if let Some(value) = arg.strip_prefix("--port=") {
            let port: u16 = value.parse().map_err(|_| format!("Invalid port: {value}"))?;
            endpoint = format!("http://localhost:{port}/mcp");
        }
    }
    Ok(Some(endpoint))
}

fn create_client(endpoint: String) -> Result<McpClient, McpError> {
    let config = ClientConfig {
        name: "SamplingExampleClient".into(),
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
    let auth: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new(AUTH_TOKEN));
    McpClientFactory::create_streamable_http_client(config, http, Some(auth))
}

fn register_sampling_callback(client: &McpClient) {
    mcp_log!(McpLogLevel::Info, "=== Registering sampling callback ===");
    let callback: SamplingCreateMessageCallback =
        Arc::new(|params| Box::pin(async move { Ok(Some(handle_sampling_request(params))) }));
    client.set_sampling_create_message_callback(callback, SamplingCapability::default());
}

async fn initialize_client(client: &McpClient) -> bool {
    mcp_log!(McpLogLevel::Info, "=== Initialize ===");
    match client.initialize().await {
        Ok(result) => {
            mcp_log!(McpLogLevel::Info, "Initialize success");
            mcp_log!(McpLogLevel::Info, "  Server name: {}", result.server_info.name);
            mcp_log!(
                McpLogLevel::Info,
                "  Server version: {}",
                result.server_info.version
            );
            true
        }
        Err(e) => {
            mcp_log!(McpLogLevel::Error, "Initialize failed: {e}");
            false
        }
    }
}

async fn list_tools(client: &McpClient) -> bool {
    mcp_log!(McpLogLevel::Info, "=== ListTools ===");
    match client.list_tools(None).await {
        Ok(result) => {
            mcp_log!(
                McpLogLevel::Info,
                "ListTools success, tool count: {}",
                result.tools.len()
            );
            for tool in &result.tools {
                mcp_log!(McpLogLevel::Info, "  Tool: {}", tool.name);
                mcp_log!(
                    McpLogLevel::Info,
                    "    Description: {}",
                    tool.description.clone().unwrap_or_default()
                );
            }
            true
        }
        Err(e) => {
            mcp_log!(McpLogLevel::Error, "ListTools failed: {e}");
            false
        }
    }
}

async fn call_sampling_echo_tool(client: &McpClient) -> bool {
    mcp_log!(McpLogLevel::Info, "=== CallTool (sampling_echo) ===");
    mcp_log!(McpLogLevel::Info, "This will trigger server-to-client sampling.");
    mcp_log!(
        McpLogLevel::Info,
        "Watch for sampling callback invocations above."
    );
    mcp_log!(McpLogLevel::Info, "");
    mcp_log!(
        McpLogLevel::Info,
        "Calling sampling_echo with prompt: '{SAMPLE_PROMPT}'"
    );
    match client
        .call_tool(
            SAMPLE_TOOL_NAME,
            Some(json!({"prompt": SAMPLE_PROMPT})),
            DEFAULT_MAX_TOKENS * 1000,
            None,
        )
        .await
    {
        Ok(result) => {
            mcp_log!(McpLogLevel::Info, "");
            mcp_log!(
                McpLogLevel::Info,
                "CallTool complete, isError: {}, content count: {}",
                result.is_error as u8,
                result.content.len()
            );
            for block in &result.content {
                match block {
                    ContentBlock::Text(text) => mcp_log!(McpLogLevel::Info, "  Response: {}", text.text),
                    ContentBlock::Image(image) => {
                        mcp_log!(McpLogLevel::Info, "  Image: mimeType={}", image.mime_type)
                    }
                    _ => {}
                }
            }
            true
        }
        Err(e) => {
            mcp_log!(McpLogLevel::Error, "CallTool failed: {e}");
            false
        }
    }
}

fn print_success_message() {
    mcp_log!(McpLogLevel::Info, "");
    mcp_log!(McpLogLevel::Info, "=== Example completed successfully ===");
    mcp_log!(McpLogLevel::Info, "The sampling callback was invoked by server,");
    mcp_log!(
        McpLogLevel::Info,
        "and this client provided a simulated LLM response."
    );
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    set_log_level(McpLogLevel::Info as u8);
    set_log_callback(Some(stdout_log));
    print_header();

    let endpoint = match parse_endpoint() {
        Ok(Some(endpoint)) => endpoint,
        Ok(None) => return std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let client = match create_client(endpoint) {
        Ok(client) => client,
        Err(e) => {
            mcp_log!(McpLogLevel::Error, "Failed to create client: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // The callback must be registered before initialize so that the
    // sampling capability is advertised.
    register_sampling_callback(&client);

    if !initialize_client(&client).await
        || !list_tools(&client).await
        || !call_sampling_echo_tool(&client).await
    {
        return std::process::ExitCode::FAILURE;
    }
    print_success_message();
    client.close_gracefully().await;
    std::process::ExitCode::SUCCESS
}
