// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `example/client_example/tool_example/tool_example.cpp`.
//!
//! Run the server example first, then:
//! `cargo run -p mcp-sdk --example tool_example -- [--auth] [--port=<1-65535>]`

use std::collections::HashMap;
use std::sync::Arc;

use mcp_sdk::log::{McpLogLevel, set_log_callback, set_log_level};
use mcp_sdk::mcp_log;
use mcp_sdk::prelude::*;
use serde_json::json;

const EXAMPLE_ENDPOINT: &str = "http://127.0.0.1:8000/mcp";
const AUTH_EXAMPLE_ENDPOINT: &str = "http://127.0.0.1:8001/mcp";
const VALID_TOKEN: &str = "valid-token-12345";
const EXAMPLE_TOOL_NAME: &str = "echo";

fn stdout_log(_level: McpLogLevel, message: String) {
    println!("{message}");
}

fn log_content(content: &[ContentBlock], prefix: &str) {
    for block in content {
        match block {
            ContentBlock::Text(text) => mcp_log!(McpLogLevel::Info, "  {prefix}TextContent: {}", text.text),
            ContentBlock::Image(image) => {
                mcp_log!(McpLogLevel::Info, "  ImageContent: mimeType={}", image.mime_type)
            }
            _ => {}
        }
    }
}

fn http_config(endpoint: &str) -> StreamableHttpClientConfig {
    StreamableHttpClientConfig {
        endpoint: endpoint.to_string(),
        tls_config: TlsConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn run_tool_demo(endpoint: &str) -> Result<(), McpError> {
    let config = ClientConfig {
        name: "ToolExampleClient".into(),
        version: "1.0.0".into(),
    };
    let client = McpClientFactory::create_streamable_http_client(config, http_config(endpoint), None)?;

    // Example 1: Initialize client
    mcp_log!(McpLogLevel::Info, "=== Example Initialize ===");
    client.initialize().await.map_err(|e| {
        mcp_log!(McpLogLevel::Error, "Initialize failed: {e}");
        e
    })?;
    mcp_log!(McpLogLevel::Info, "Initialize success");

    // Example 1.5: Ping
    mcp_log!(McpLogLevel::Info, "=== Example Ping ===");
    client.send_ping().await.map_err(|e| {
        mcp_log!(McpLogLevel::Error, "Ping failed: {e}");
        e
    })?;
    mcp_log!(McpLogLevel::Info, "Ping success");

    // Example 2: List tools (paginated)
    mcp_log!(McpLogLevel::Info, "=== Example ListTools (paginated) ===");
    let mut cursor: Option<String> = None;
    let mut total = 0usize;
    loop {
        let page = client.list_tools(cursor.clone()).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "ListTools failed: {e}");
            e
        })?;
        mcp_log!(
            McpLogLevel::Info,
            "ListTools page fetched, tool count: {}",
            page.tools.len()
        );
        for tool in &page.tools {
            let desc = tool.description.clone().unwrap_or_default();
            mcp_log!(McpLogLevel::Info, "  Tool: {} - {desc}", tool.name);
        }
        total += page.tools.len();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    mcp_log!(
        McpLogLevel::Info,
        "ListTools completed, total tool count: {total}"
    );

    // Example 3: Call tool
    mcp_log!(McpLogLevel::Info, "=== Example CallTool ===");
    let result = client
        .call_tool(
            EXAMPLE_TOOL_NAME,
            Some(json!({"user_query": "Shenzhen weather"})),
            30_000,
            None,
        )
        .await
        .map_err(|e| {
            mcp_log!(McpLogLevel::Error, "CallTool failed: {e}");
            e
        })?;
    mcp_log!(
        McpLogLevel::Info,
        "CallTool success, isError: {}, content count: {}",
        result.is_error as u8,
        result.content.len()
    );
    log_content(&result.content, "");

    // Example 3.5: Call tool with progress callback
    mcp_log!(
        McpLogLevel::Info,
        "=== Example CallTool with Progress Callback ==="
    );
    let progress: ProgressCallback = Arc::new(|progress, _total, message| {
        mcp_log!(McpLogLevel::Info, "=== PROGRESS CALLBACK CALLED ===");
        mcp_log!(McpLogLevel::Info, "Progress: {progress} ({}%)", progress * 100.0);
        if let Some(message) = message {
            mcp_log!(McpLogLevel::Info, "Message: {message}");
        }
    });
    let result = client
        .call_tool(
            "progress_tool",
            Some(json!({"task_name": "Data Processing", "steps": 5})),
            60_000,
            Some(progress),
        )
        .await
        .map_err(|e| {
            mcp_log!(McpLogLevel::Error, "CallTool with progress callback failed: {e}");
            e
        })?;
    mcp_log!(
        McpLogLevel::Info,
        "CallTool with progress callback success, isError: {}, content count: {}",
        result.is_error as u8,
        result.content.len()
    );
    log_content(&result.content, "");

    // Example 3.1: Call async echo tool
    mcp_log!(McpLogLevel::Info, "=== Example CallTool (Async Echo) ===");
    let result = client
        .call_tool(
            "async_echo",
            Some(json!({"user_query": "Hello from async echo"})),
            30_000,
            None,
        )
        .await
        .map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Async CallTool failed: {e}");
            e
        })?;
    mcp_log!(
        McpLogLevel::Info,
        "Async CallTool success, isError: {}, content count: {}",
        result.is_error as u8,
        result.content.len()
    );
    log_content(&result.content, "Async ");

    // Example 4: Complete - prompt argument completions
    mcp_log!(McpLogLevel::Info, "=== Example Complete (Prompt Completions) ===");
    let mut ctx_args = HashMap::new();
    ctx_args.insert("framework".to_string(), "django".to_string());
    let result = client
        .complete(
            CompleteReference::Prompt(PromptReference {
                name: "code_generator".into(),
            }),
            CompletionArgument {
                name: "language".into(),
                value: "py".into(),
            },
            Some(CompletionContext {
                arguments: Some(ctx_args),
            }),
        )
        .await
        .map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Complete failed: {e}");
            e
        })?;
    mcp_log!(
        McpLogLevel::Info,
        "Complete success, completion count: {}",
        result.completion.values.len()
    );
    for value in &result.completion.values {
        mcp_log!(McpLogLevel::Info, "  Suggestion: {value}");
    }

    // Example 5: Complete - resource template argument completions
    mcp_log!(
        McpLogLevel::Info,
        "=== Example Complete (Resource Template Completions) ==="
    );
    let mut ctx_args = HashMap::new();
    ctx_args.insert("extension".to_string(), "json".to_string());
    let result = client
        .complete(
            CompleteReference::Resource(ResourceTemplateReference {
                uri: "file:///path/to/template".into(),
            }),
            CompletionArgument {
                name: "format".into(),
                value: "j".into(),
            },
            Some(CompletionContext {
                arguments: Some(ctx_args),
            }),
        )
        .await
        .map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Complete failed: {e}");
            e
        })?;
    mcp_log!(
        McpLogLevel::Info,
        "Complete success, completion count: {}",
        result.completion.values.len()
    );
    for value in &result.completion.values {
        mcp_log!(McpLogLevel::Info, "  Suggestion: {value}");
    }

    client.close_gracefully().await;
    mcp_log!(McpLogLevel::Info, "=== Example completed ===");
    Ok(())
}

async fn run_tool_demo_with_auth(endpoint: &str) -> Result<(), McpError> {
    mcp_log!(
        McpLogLevel::Info,
        "=== Example with Authentication and Authorization ==="
    );
    let config = ClientConfig {
        name: "TestAuthClient".into(),
        version: "1.0.0".into(),
    };

    mcp_log!(
        McpLogLevel::Info,
        "\n=== Example: Initialize and call tool with 'read write' token ==="
    );
    let auth: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new(VALID_TOKEN));
    let client = McpClientFactory::create_streamable_http_client(config, http_config(endpoint), Some(auth))?;
    match client.initialize().await {
        Ok(_) => {
            mcp_log!(McpLogLevel::Info, "Initialize succeeded");
            match client
                .call_tool(
                    EXAMPLE_TOOL_NAME,
                    Some(json!({"user_query": "Hello from echo tool!"})),
                    30_000,
                    None,
                )
                .await
            {
                Ok(result) => {
                    mcp_log!(
                        McpLogLevel::Info,
                        "echo success, isError: {}",
                        result.is_error as u8
                    );
                    for block in &result.content {
                        if let Some(text) = block.as_text() {
                            mcp_log!(McpLogLevel::Info, "  Result: {text}");
                        }
                    }
                }
                Err(e) => mcp_log!(McpLogLevel::Error, "Unexpected error: {e}"),
            }
        }
        Err(e) => mcp_log!(McpLogLevel::Error, "Initialize failed: {e}"),
    }
    client.close_gracefully().await;

    mcp_log!(McpLogLevel::Info, "\n=== Example completed ===");
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    set_log_level(McpLogLevel::Info as u8);
    set_log_callback(Some(stdout_log));

    let mut enable_auth = false;
    let mut endpoint = EXAMPLE_ENDPOINT.to_string();
    for arg in std::env::args().skip(1) {
        if arg == "--auth" {
            enable_auth = true;
            endpoint = AUTH_EXAMPLE_ENDPOINT.to_string();
            continue;
        }
        if let Some(value) = arg.strip_prefix("--port=") {
            match value.parse::<u16>() {
                Ok(port) => endpoint = format!("http://127.0.0.1:{port}/mcp"),
                Err(_) => {
                    eprintln!("Invalid port: {value}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
    }

    let outcome = if enable_auth {
        run_tool_demo_with_auth(&endpoint).await
    } else {
        run_tool_demo(&endpoint).await
    };
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
