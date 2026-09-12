// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared fixture: the test server used by the C++ `client_test.cpp` and
//! `st/test_st.cpp`, ported to Rust.

use ap_support::testing::TestResult;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

use mcp_sdk::prelude::*;
use mcp_sdk::server::ServerContext;
use serde_json::{Value, json};

pub const ECHO_TOOL_NAME: &str = "echo";
pub const ECHO_TOOL_DESCRIPTION: &str = "Echoes back the input message";
pub const RESOURCE_URI: &str = "http://example.com/resource";
pub const BULK_COUNT: usize = 120;

/// Pick a free TCP port on the loopback interface.
pub fn free_port() -> TestResult<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// State shared with the test server handlers.
#[derive(Default)]
pub struct Recorded {
    pub logging_level: Mutex<Option<String>>,
}

fn echo_result(args: &Value) -> CallToolResult {
    let user_query = args
        .get("user_query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    CallToolResult {
        content: vec![
            ContentBlock::text(format!("Echo: {user_query}")),
            ContentBlock::Image(ImageContent {
                data: "test".into(),
                mime_type: "image".into(),
                annotations: None,
            }),
            ContentBlock::ResourceLink(ResourceLink {
                uri: "test".into(),
                name: "test".into(),
                title: Some("test".into()),
                description: Some("test".into()),
                mime_type: Some("resource_link".into()),
                size: Some(1024),
                ..Default::default()
            }),
            ContentBlock::Audio(AudioContent {
                data: "test".into(),
                mime_type: "audio".into(),
                annotations: None,
            }),
            ContentBlock::EmbeddedResource(EmbeddedResource {
                resource: ResourceContents::Blob(BlobResourceContents {
                    uri: "test".into(),
                    blob: "test".into(),
                    mime_type: Some("blob".into()),
                }),
                annotations: None,
            }),
        ],
        structured_content: Some(json!({"result": user_query})),
        is_error: false,
        meta: None,
    }
}

/// Register every tool, prompt and resource of the test server.
pub fn configure_server(server: &McpServer, recorded: Arc<Recorded>) -> TestResult {
    let echo_input = json!({"type": "object", "properties": {"user_query": {"type": "string",
        "description": "The user query."}}, "required": ["user_query"]});
    let echo_output = json!({"type": "object", "properties": {"result": {"type": "string",
        "description": "The echoed message"}}});
    server.add_tool(
        ECHO_TOOL_NAME,
        |_ctx, _name, args| async move { Ok(echo_result(&args)) },
        AddToolOptionalParams {
            title: Some("Echo Tool".into()),
            description: Some(ECHO_TOOL_DESCRIPTION.into()),
            input_schema: Some(echo_input),
            output_schema: Some(echo_output),
            ..Default::default()
        },
    )?;

    for i in 0..BULK_COUNT {
        server.add_tool(
            &format!("echo_{i}"),
            |_ctx, _name, args| async move { Ok(echo_result(&args)) },
            AddToolOptionalParams {
                input_schema: Some(json!({"type": "object", "properties": {}, "additionalProperties": true})),
                ..Default::default()
            },
        )?;
    }

    // Async style tool: waits a little before answering.
    server.add_tool(
        "async_echo",
        |_ctx, _name, args| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let q = args
                .get("user_query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(CallToolResult::text(format!("Async Echo: {q}")))
        },
        AddToolOptionalParams::default(),
    )?;

    // Progress tool: sends notifications/progress with the client's token.
    server.add_tool(
        "progress_tool",
        |ctx, _name, args| async move {
            let task_name = args
                .get("task_name")
                .and_then(Value::as_str)
                .unwrap_or("Unknown Task")
                .to_string();
            let steps = args
                .get("steps")
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .clamp(1, 100);
            let token = ctx
                .progress_token()
                .unwrap_or_else(|| ProgressToken::from(format!("progress-{task_name}")));
            for i in 0..=steps {
                let progress = i as f64 / steps as f64;
                ctx.session
                    .send_progress_notification(
                        token.clone(),
                        progress,
                        Some(steps as f64),
                        Some(format!("Processing {task_name}: step {i} of {steps}")),
                    )
                    .await?;
            }
            Ok(CallToolResult::text(format!(
                "Task '{task_name}' completed successfully"
            )))
        },
        AddToolOptionalParams::default(),
    )?;

    // Sampling tool: asks the client for a completion.
    server.add_tool(
        "sampling_echo",
        |ctx, _name, args| async move {
            let prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("Hello")
                .to_string();
            let params = CreateMessageParams {
                messages: vec![SamplingMessage::text(RoleType::User, prompt)],
                max_tokens: 100,
                ..Default::default()
            };
            match ctx.session.sampling_create_message(params).await {
                Ok(result) => {
                    let text = result
                        .content
                        .first_text()
                        .unwrap_or_else(|| "<no text content>".into());
                    Ok(CallToolResult::text(format!("Sampling response: {text}")))
                }
                Err(e) => Ok(CallToolResult::error(format!("Sampling error: {e}"))),
            }
        },
        AddToolOptionalParams::default(),
    )?;

    // Roots tool: asks the client for its roots.
    server.add_tool(
        "roots_tool",
        |ctx, _name, _args| async move {
            match ctx.session.list_roots().await {
                Ok(result) => {
                    let uris: Vec<String> = result.roots.iter().map(|r| r.uri.clone()).collect();
                    Ok(CallToolResult::text(uris.join(",")))
                }
                Err(e) => Ok(CallToolResult::error(format!("Roots error: {e}"))),
            }
        },
        AddToolOptionalParams::default(),
    )?;

    server.add_prompt(
        "example_prompt",
        |_ctx, _name, args| async move {
            let who = args
                .as_ref()
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("friend")
                .to_string();
            let lang = args
                .as_ref()
                .and_then(|a| a.get("language"))
                .and_then(Value::as_str)
                .unwrap_or("English")
                .to_string();
            Ok(GetPromptResult {
                description: Some("example_prompt".into()),
                messages: vec![PromptMessage {
                    role: RoleType::Assistant,
                    content: ContentBlock::text(format!("Hello, {who}! (language={lang})")),
                }],
                meta: None,
            })
        },
        AddPromptOptionalParams {
            description: Some("Generate a personalized greeting message".into()),
            arguments: Some(vec![
                PromptArgument::new("name", "The name of the person to greet", true),
                PromptArgument::new("language", "Language for the greeting (default: English)", false),
            ]),
            ..Default::default()
        },
    )?;

    let read = |_ctx: ServerContext, uri: String| async move {
        Ok(ReadResourceResult {
            contents: vec![ResourceContents::Text(TextResourceContents {
                text: format!("hello, {uri}"),
                uri,
                mime_type: Some("text/plain".into()),
            })],
            meta: None,
        })
    };
    server.add_resource(
        RESOURCE_URI,
        "Test Resource",
        read,
        AddResourceOptionalParams {
            description: Some("A test resource for demonstration".into()),
            mime_type: Some("text/plain".into()),
            icons: Some(vec![Icon {
                src: "https://example.com/icons/document.png".into(),
                mime_type: Some("image/png".into()),
                sizes: Some(vec!["16x16".into(), "32x32".into()]),
                theme: Some("light".into()),
            }]),
            annotations: Some(Annotations {
                audience: Some(vec![RoleType::User, RoleType::Assistant]),
                last_modified: Some("2025-01-15T10:30:00Z".into()),
                priority: Some(1.0),
            }),
            ..Default::default()
        },
    )?;
    for i in 0..BULK_COUNT {
        server.add_resource(
            &format!("http://example.com/resource/{i}"),
            &format!("res_{i}"),
            read,
            AddResourceOptionalParams::default(),
        )?;
    }
    server.add_resource_template(
        "http://example.com/resourceTemplate/{id}",
        "Test Resource Template",
        AddResourceTemplateOptionalParams {
            description: Some("A test resource template for demonstration".into()),
            mime_type: Some("text/plain".into()),
            ..Default::default()
        },
    )?;

    server.add_completion(|reference, _argument, context| async move {
        let mut values: Vec<String> = match reference {
            CompleteReference::Prompt(_) => vec!["python".into(), "pytorch".into(), "pydantic".into()],
            CompleteReference::Resource(_) => vec!["json".into(), "yaml".into(), "txt".into()],
        };
        if let Some(framework) = context
            .and_then(|c| c.arguments)
            .and_then(|a| a.get("framework").cloned())
        {
            values.push(framework);
        }
        let total = values.len() as i64;
        Ok(CompleteResult {
            completion: Completion {
                values,
                total: Some(total),
                has_more: Some(false),
            },
            meta: None,
        })
    });

    let level_sink = recorded.clone();
    server.register_set_logging_level_handler(move |level| {
        *level_sink.logging_level.lock() = Some(level.to_string());
        Ok(())
    });
    Ok(())
}

/// Build a running HTTP test server on a free port.
pub async fn start_http_server(
    json_mode: bool,
    stateless: bool,
) -> TestResult<(McpServer, String, Arc<Recorded>)> {
    let port = free_port()?;
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    let config = ServerConfig {
        name: "TestMCPServer".into(),
        version: "1.0.0".into(),
        worker_threads: 1,
        capabilities: ServerCapabilities {
            logging: Some(LoggingCapabilities {}),
            tools: Some(ToolsCapabilities {
                list_changed: Some(true),
            }),
            prompts: Some(PromptsCapabilities {
                list_changed: Some(true),
            }),
            resources: Some(ResourcesCapabilities {
                subscribe: Some(true),
                list_changed: Some(true),
            }),
            experimental: None,
        },
        ..Default::default()
    };
    let transport = StreamableHttpServerConfig {
        endpoint: endpoint.clone(),
        is_json_response_enabled: json_mode,
        stateless,
        io_threads: 1,
        ..Default::default()
    };
    let server = McpServerFactory::create_streamable_http_server(config, transport)?;
    let recorded = Arc::new(Recorded::default());
    configure_server(&server, recorded.clone())?;
    server.run().await?;
    Ok((server, endpoint, recorded))
}

/// Build a client for `endpoint`.
pub fn http_client(endpoint: &str, auth: Option<Arc<dyn AuthProvider>>) -> TestResult<McpClient> {
    let config = ClientConfig {
        name: "TestClient".into(),
        version: "1.0.0".into(),
    };
    let http = StreamableHttpClientConfig {
        endpoint: endpoint.to_string(),
        timeout: Duration::from_millis(5000),
        sse_timeout: Duration::from_millis(5000),
        ..Default::default()
    };
    Ok(McpClientFactory::create_streamable_http_client(
        config, http, auth,
    )?)
}

/// Wait until `f` returns true, or panic after two seconds.
pub async fn wait_for(f: impl Fn() -> bool) -> TestResult {
    for _ in 0..400 {
        if f() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(ap_support::testing::TestFailure::new("condition not met within 2s".to_string()).into())
}
