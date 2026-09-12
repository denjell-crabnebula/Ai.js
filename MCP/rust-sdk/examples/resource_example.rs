// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `example/client_example/resource_example/resource_example.cpp`.
//!
//! Run the server example first, then:
//! `cargo run -p mcp-sdk --example resource_example -- [--port=<1-65535>]`

use std::sync::Arc;

use mcp_sdk::log::{McpLogLevel, set_log_callback, set_log_level};
use mcp_sdk::mcp_log;
use mcp_sdk::prelude::*;

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
        name: "ResourceExampleClient".into(),
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

    // Example 1: List resources (paginated)
    mcp_log!(McpLogLevel::Info, "=== Example ListResources (paginated) ===");
    let mut target_uri = String::new();
    let mut cursor: Option<String> = None;
    let mut total = 0usize;
    loop {
        let page = client.list_resources(cursor.clone()).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "ListResources failed: {e}");
            e
        })?;
        mcp_log!(
            McpLogLevel::Info,
            "ListResources page fetched, resource count: {}",
            page.resources.len()
        );
        for resource in &page.resources {
            mcp_log!(
                McpLogLevel::Info,
                "  Resource: {} (uri: {})",
                resource.name,
                resource.uri
            );
        }
        if target_uri.is_empty() {
            if let Some(first) = page.resources.first() {
                target_uri = first.uri.clone();
            }
        }
        total += page.resources.len();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    mcp_log!(
        McpLogLevel::Info,
        "ListResources completed, total resource count: {total}"
    );

    // Example 2: Subscribe/Unsubscribe resource
    if !target_uri.is_empty() {
        mcp_log!(
            McpLogLevel::Info,
            "=== Example Subscribe/Unsubscribe Resource ==="
        );
        client.subscribe_resource(&target_uri).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Subscribe/Unsubscribe failed: {e}");
            e
        })?;
        mcp_log!(McpLogLevel::Info, "SubscribeResource success: {target_uri}");
        client.unsubscribe_resource(&target_uri).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "Subscribe/Unsubscribe failed: {e}");
            e
        })?;
        mcp_log!(McpLogLevel::Info, "UnsubscribeResource success: {target_uri}");
    }

    // Example 3: Read resource
    if !target_uri.is_empty() {
        mcp_log!(McpLogLevel::Info, "=== Example ReadResource ===");
        let result = client.read_resource(&target_uri).await.map_err(|e| {
            mcp_log!(McpLogLevel::Error, "ReadResource failed: {e}");
            e
        })?;
        mcp_log!(
            McpLogLevel::Info,
            "ReadResource success: {target_uri}, content count: {}",
            result.contents.len()
        );
    }

    // Example 4: List resource templates
    mcp_log!(McpLogLevel::Info, "=== Example ListResourcesTemplates ===");
    let templates = client.list_resources_templates().await.map_err(|e| {
        mcp_log!(McpLogLevel::Error, "ListResourcesTemplates failed: {e}");
        e
    })?;
    mcp_log!(
        McpLogLevel::Info,
        "ListResourcesTemplates success, template count: {}",
        templates.resource_templates.len()
    );
    for template in &templates.resource_templates {
        let mime = template.mime_type.clone().unwrap_or_else(|| "none".into());
        mcp_log!(
            McpLogLevel::Info,
            "  Template: {} (mimeType: {mime})",
            template.uri_template
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
