// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! End to end tests over the stdio transport, porting the intent of
//! `tests/ut/stdio/*` and `tests/ut/transport/stdio_transport_test.cpp`.

pub mod common;

use ap_support::testing::{ResultExt, TestResult};
use parking_lot::Mutex;
use std::sync::Arc;

use common::*;
use mcp_sdk::prelude::*;
use mcp_sdk::transport::{ClientTransport, StdioClientTransport};
use serde_json::json;

/// Start a stdio server on in-memory streams and return a client attached to it.
async fn stdio_pair() -> TestResult<(McpServer, McpClient, Arc<Recorded>)> {
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);

    let server = McpServerFactory::create_stdio_server(ServerConfig {
        name: "StdioServer".into(),
        version: "1.0.0".into(),
        ..Default::default()
    })?;
    let recorded = Arc::new(Recorded::default());
    configure_server(&server, recorded.clone())?;
    server.set_stdio_streams(server_read, server_write);
    server.run().await?;
    assert!(server.is_running());
    assert!(server.stdio_session().is_some());

    let transport: Arc<dyn ClientTransport> = StdioClientTransport::with_streams(client_read, client_write);
    let client = McpClient::with_transport(ClientConfig::default(), transport);
    Ok((server, client, recorded))
}

#[tokio::test]
async fn full_session_over_in_memory_stdio() -> TestResult {
    let (server, client, recorded) = stdio_pair().await?;
    let sampled = Arc::new(Mutex::new(0usize));
    let counter = sampled.clone();
    client.set_sampling_create_message_callback(
        Arc::new(move |_params| {
            *counter.lock() += 1;
            Box::pin(async {
                Ok(Some(CreateMessageResult {
                    model: "stdio-model".into(),
                    role: RoleType::Assistant,
                    content: SamplingContent::text("stdio answer"),
                    stop_reason: None,
                    meta: None,
                }))
            })
        }),
        SamplingCapability::default(),
    );

    let init = client.initialize().await?;
    assert_eq!(init.server_info.name, "StdioServer");
    client.send_ping().await?;

    let tools = client.list_tools(None).await?;
    assert!(tools.tools.iter().any(|t| t.name == ECHO_TOOL_NAME));
    let result = client
        .call_tool(ECHO_TOOL_NAME, Some(json!({"user_query": "over stdio"})), 0, None)
        .await?;
    assert_eq!(result.content[0].as_text(), Some("Echo: over stdio"));

    let prompt = client
        .get_prompt("example_prompt", Some(json!({"name": "Bob"})))
        .await?;
    assert_eq!(
        prompt.messages[0].content.as_text(),
        Some("Hello, Bob! (language=English)")
    );
    let read = client.read_resource(RESOURCE_URI).await?;
    assert_eq!(read.contents.len(), 1);

    // Progress notifications and server initiated sampling both work over stdio.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let cb: ProgressCallback = Arc::new(move |p, _t, _m| sink.lock().push(p));
    let progress = client
        .call_tool(
            "progress_tool",
            Some(json!({"task_name": "T", "steps": 2})),
            0,
            Some(cb),
        )
        .await?;
    assert!(!progress.is_error);
    wait_for(|| seen.lock().len() == 3).await?;

    let sampling = client
        .call_tool("sampling_echo", Some(json!({"prompt": "hi"})), 0, None)
        .await?;
    assert!(!sampling.is_error, "{:?}", sampling.content);
    assert_eq!(
        sampling.content[0].as_text(),
        Some("Sampling response: stdio answer")
    );
    assert_eq!(*sampled.lock(), 1);

    client.set_logging_level(LoggingLevel::Warning).await?;
    assert_eq!(recorded.logging_level.lock().as_deref(), Some("warning"));

    let err = client.call_tool("missing", None, 0, None).await.err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));

    client.close_gracefully().await;
    server.stop().await;
    assert!(!server.is_running());
    Ok(())
}

#[tokio::test]
async fn stdio_client_spawns_subprocess_server() -> TestResult {
    // The child answers `initialize` with a canned response, then exits on EOF.
    let script = r#"
read line
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"ShellServer","version":"0.1"}}}\n'
read line
read line
printf '{"jsonrpc":"2.0","id":2,"result":{}}\n'
"#;
    let client = McpClientFactory::create_stdio_client(
        ClientConfig::default(),
        StdioClientConfig {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Default::default(),
        },
    );
    let init = client.initialize().await?;
    assert_eq!(init.server_info.name, "ShellServer");
    assert!(client.get_server_capabilities()?.tools.is_some());
    client.send_ping().await?;
    client.close_gracefully().await;
    Ok(())
}

#[tokio::test]
async fn stdio_child_exit_fails_pending_requests() -> TestResult {
    let client = McpClientFactory::create_stdio_client(
        ClientConfig::default(),
        StdioClientConfig {
            command: "sh".into(),
            args: vec!["-c".into(), "read line; exit 0".into()],
            env: Default::default(),
        },
    );
    let err = client.initialize().await.err_or_fail()?;
    assert!(err.message().contains("Transport disconnected"), "{err}");
    client.close_gracefully().await;
    Ok(())
}
