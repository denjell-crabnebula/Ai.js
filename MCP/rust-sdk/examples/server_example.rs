// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `example/server_example/server_example.cpp`.
//!
//! Streamable HTTP server with tools, prompts, resources, a completion
//! handler and optional bearer token authentication.
//!
//! Run:
//! `cargo run -p mcp-sdk --example server_example -- [--auth] [--port=<1-65535>] [--stateless] [--isJsonResponseDisable]`

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mcp_sdk::log::{McpLogLevel, set_log_callback, set_log_level};
use mcp_sdk::mcp_log;
use mcp_sdk::prelude::*;
use serde_json::{Value, json};

const SERVER_NAME: &str = "TestMCPServer";
const SERVER_VERSION: &str = "1.0.0";
const IO_THREADS: u32 = 2;
const WORKER_THREADS: u32 = 2;
const ENDPOINT: &str = "http://127.0.0.1:8000/mcp";
const AUTH_ENDPOINT: &str = "http://127.0.0.1:8001/mcp";
const LOG_FILE_NAME: &str = "server_example.log";
const ECHO_TOOL_NAME: &str = "echo";
const ECHO_TOOL_TITLE: &str = "Echo Tool";
const ECHO_TOOL_DESCRIPTION: &str = "Echoes back the input message";
const PROMPT_NAME: &str = "example_prompt";
const PROMPT_DESCRIPTION: &str = "Generate a personalized greeting message";
const RESOURCE_URI: &str = "http://example.com/resource";
const RESOURCE_NAME: &str = "Test Resource";
const RESOURCE_DESCRIPTION: &str = "A test resource for demonstration";
const RESOURCE_MIME_TYPE: &str = "text/plain";
const PROGRESS_TOOL_SIMULATION_DELAY_MS: u64 = 500;
const ASYNC_PROMPT_PROCESSING_DELAY_MS: u64 = 300;
const ASYNC_RESOURCE_PROCESSING_DELAY_MS: u64 = 300;
const RESOURCE_TEMPLATE_URI: &str = "http://example.com/resourceTemplate/{id}";
const RESOURCE_TEMPLATE_NAME: &str = "Test Resource Template";
const RESOURCE_TEMPLATE_DESCRIPTION: &str = "A test resource template for demonstration";
const RESOURCE_TEMPLATE_MIME_TYPE: &str = "text/plain";
const HEARTBEAT_INTERVAL_SECONDS: u64 = 5;
const LOG_INTERVAL_COUNT: u64 = 6;
const EXAMPLE_BULK_COUNT: usize = 120;
const VALID_TOKEN: &str = "valid-token-12345";
const REQUIRED_SCOPES: &str = "read write";

static LOG_FILE: parking_lot::Mutex<Option<File>> = parking_lot::Mutex::new(None);

/// Append every SDK log line to the log file.
fn file_log_callback(_level: McpLogLevel, message: String) {
    {
        let mut guard = LOG_FILE.lock();
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{message}");
            let _ = file.flush();
        }
    }
}

fn string_arg(args: &Value, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

fn text_resource(uri: String, text: String) -> ReadResourceResult {
    ReadResourceResult {
        contents: vec![ResourceContents::Text(TextResourceContents {
            uri,
            text,
            mime_type: Some(RESOURCE_MIME_TYPE.into()),
        })],
        meta: None,
    }
}

fn greeting(args: Option<&Value>) -> GetPromptResult {
    let who = args
        .map(|a| string_arg(a, "name", "friend"))
        .unwrap_or_else(|| "friend".into());
    let lang = args
        .map(|a| string_arg(a, "language", "English"))
        .unwrap_or_else(|| "English".into());
    GetPromptResult {
        description: Some(PROMPT_NAME.into()),
        messages: vec![PromptMessage {
            role: RoleType::Assistant,
            content: ContentBlock::text(format!("Hello, {who}! (language={lang})")),
        }],
        meta: None,
    }
}

/// Ask the client for a completion and wrap the answer into a tool result.
async fn run_sampling_request(ctx: &ServerContext, prompt: String) -> CallToolResult {
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
            let mut out = CallToolResult::text(format!("Sampling response: {text}"));
            out.structured_content = Some(json!({"result": text}));
            out
        }
        Err(e) => CallToolResult::error(format!("Sampling error: {e}")),
    }
}

fn register_tools(server: &McpServer) {
    let echo_input = json!({"type": "object", "properties": {"user_query": {"type": "string",
        "description": "The user query."}}, "required": ["user_query"]});
    let echo_output = json!({"type": "object", "properties": {"result": {"type": "string",
        "description": "The echoed message"}}});
    let echo = |_ctx: ServerContext, _name: String, args: Value| async move {
        let user_query = string_arg(&args, "user_query", "");
        let mut result = CallToolResult::text(format!("Echo: {user_query}"));
        result.structured_content = Some(json!({"result": user_query}));
        Ok(result)
    };
    match server.add_tool(
        ECHO_TOOL_NAME,
        echo,
        AddToolOptionalParams {
            title: Some(ECHO_TOOL_TITLE.into()),
            description: Some(ECHO_TOOL_DESCRIPTION.into()),
            input_schema: Some(echo_input.clone()),
            output_schema: Some(echo_output.clone()),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add tool success: {ECHO_TOOL_NAME}"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add tool failed: {e}"),
    }

    // Register more tools so that tools/list needs several pages.
    let mut bulk_ok = true;
    for i in 0..EXAMPLE_BULK_COUNT {
        let params = AddToolOptionalParams {
            input_schema: Some(json!({"type": "object", "properties": {}, "additionalProperties": true})),
            ..Default::default()
        };
        if let Err(e) = server.add_tool(&format!("echo_{i}"), echo, params) {
            mcp_log!(McpLogLevel::Error, "bulk add tools failed: {e}");
            bulk_ok = false;
            break;
        }
    }
    if bulk_ok {
        mcp_log!(
            McpLogLevel::Info,
            "bulk add tools completed: {EXAMPLE_BULK_COUNT}"
        );
    }

    // Async echo tool: answers after a short delay.
    match server.add_tool(
        "async_echo",
        |_ctx, _name, args| async move {
            let user_query = string_arg(&args, "user_query", "");
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(CallToolResult::text(format!("Async Echo: {user_query}")))
        },
        AddToolOptionalParams {
            title: Some("Async Echo Tool".into()),
            description: Some(
                "Async version of echo tool - echoes back the input after a short delay".into(),
            ),
            input_schema: Some(echo_input),
            output_schema: Some(echo_output),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add async tool success: async_echo"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add async tool failed: {e}"),
    }

    // Sampling echo tool: server to client sampling.
    match server.add_tool(
        "sampling_echo",
        |ctx, _name, args| async move {
            let prompt = string_arg(&args, "prompt", "Hello");
            Ok(run_sampling_request(&ctx, prompt).await)
        },
        AddToolOptionalParams {
            title: Some("Sampling Echo Tool".into()),
            description: Some(
                "Demonstrates server-to-client sampling: requests sampling from client and returns the response"
                    .into(),
            ),
            input_schema: Some(json!({"type": "object", "properties": {"prompt": {"type": "string",
                "description": "The prompt to send for sampling"}}, "required": ["prompt"]})),
            output_schema: Some(json!({"type": "object", "properties": {"result": {"type": "string",
                "description": "The sampling response from client"}}})),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add sampling echo tool success: sampling_echo"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add sampling echo tool failed: {e}"),
    }

    // Progress tool: sends notifications/progress while it works.
    match server.add_tool(
        "progress_tool",
        |ctx, _name, args| async move {
            let task_name = string_arg(&args, "task_name", "Unknown Task");
            let steps = args
                .get("steps")
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .clamp(1, 100);
            let start = Instant::now();
            let token = ctx.progress_token().unwrap_or_else(|| {
                ProgressToken::from(format!(
                    "progress-{task_name}-{}",
                    chrono::Utc::now().timestamp()
                ))
            });
            for i in 0..=steps {
                let progress = i as f64 / steps as f64;
                let message = format!("Processing {task_name}: step {i} of {steps}");
                if let Err(e) = ctx
                    .session
                    .send_progress_notification(token.clone(), progress, Some(steps as f64), Some(message))
                    .await
                {
                    mcp_log!(McpLogLevel::Warn, "progress notification failed: {e}");
                }
                if i < steps {
                    tokio::time::sleep(Duration::from_millis(PROGRESS_TOOL_SIMULATION_DELAY_MS)).await;
                }
            }
            let elapsed = start.elapsed().as_millis();
            let mut result = CallToolResult::text(format!(
                "Task '{task_name}' completed successfully in {elapsed}ms"
            ));
            result.structured_content = Some(json!({
                "result": format!("Task '{task_name}' completed"),
                "total_time_ms": elapsed as i64
            }));
            Ok(result)
        },
        AddToolOptionalParams {
            title: Some("Progress Notification Tool".into()),
            description: Some("Demonstrates progress notifications during long-running operations".into()),
            input_schema: Some(json!({"type": "object", "properties": {"task_name": {"type": "string",
                "description": "Name of the task to process"}, "steps": {"type": "integer",
                "description": "Number of steps to simulate", "minimum": 1}}, "required": ["task_name", "steps"]})),
            output_schema: Some(json!({"type": "object", "properties": {"result": {"type": "string",
                "description": "Final result message"}, "total_time_ms": {"type": "integer",
                "description": "Total processing time in milliseconds"}}})),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add progress tool success: progress_tool"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add progress tool failed: {e}"),
    }
}

fn register_prompts(server: &McpServer) {
    match server.add_prompt(
        PROMPT_NAME,
        |_ctx, _name, args| async move { Ok(greeting(args.as_ref())) },
        AddPromptOptionalParams {
            description: Some(PROMPT_DESCRIPTION.into()),
            arguments: Some(vec![
                PromptArgument::new("name", "The name of the person to greet", true),
                PromptArgument::new("language", "Language for the greeting (default: English)", false),
            ]),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add prompt success: {PROMPT_NAME}"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add prompt failed: {e}"),
    }

    let async_name = format!("async_{PROMPT_NAME}");
    match server.add_prompt(
        &async_name,
        |_ctx, _name, args| async move {
            tokio::time::sleep(Duration::from_millis(ASYNC_PROMPT_PROCESSING_DELAY_MS)).await;
            let who = args
                .as_ref()
                .map(|a| string_arg(a, "name", "friend"))
                .unwrap_or_else(|| "friend".into());
            Ok(GetPromptResult {
                description: Some("async greeting".into()),
                messages: vec![PromptMessage {
                    role: RoleType::Assistant,
                    content: ContentBlock::text(format!("Hello, {who}! (async)")),
                }],
                meta: None,
            })
        },
        AddPromptOptionalParams {
            description: Some("Async version of greeting prompt".into()),
            arguments: Some(vec![
                PromptArgument::new("name", "The name of the person to greet", true),
                PromptArgument::new("language", "Language for the greeting", false),
            ]),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add async prompt success: {async_name}"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add async prompt failed: {e}"),
    }
}

fn register_resources(server: &McpServer) {
    let read = |_ctx: ServerContext, uri: String| async move {
        Ok(text_resource(uri.clone(), format!("hello, {uri}")))
    };
    match server.add_resource(
        RESOURCE_URI,
        RESOURCE_NAME,
        read,
        AddResourceOptionalParams {
            description: Some(RESOURCE_DESCRIPTION.into()),
            mime_type: Some(RESOURCE_MIME_TYPE.into()),
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
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add resource success: {RESOURCE_URI}"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add resource failed: {e}"),
    }

    let async_uri = format!("async_{RESOURCE_URI}");
    match server.add_resource(
        &async_uri,
        &format!("async_{RESOURCE_NAME}"),
        |_ctx, uri| async move {
            tokio::time::sleep(Duration::from_millis(ASYNC_RESOURCE_PROCESSING_DELAY_MS)).await;
            Ok(text_resource(uri.clone(), format!("async hello, {uri}")))
        },
        AddResourceOptionalParams {
            description: Some("Async version of the test resource".into()),
            mime_type: Some(RESOURCE_MIME_TYPE.into()),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(McpLogLevel::Info, "add async resource success: {async_uri}"),
        Err(e) => mcp_log!(McpLogLevel::Error, "add async resource failed: {e}"),
    }

    let mut bulk_ok = true;
    for i in 0..EXAMPLE_BULK_COUNT {
        let uri = format!("http://example.com/resource/{i}");
        let params = AddResourceOptionalParams {
            mime_type: Some(RESOURCE_MIME_TYPE.into()),
            ..Default::default()
        };
        if let Err(e) = server.add_resource(&uri, &format!("res_{i}"), read, params) {
            mcp_log!(McpLogLevel::Error, "bulk add resources failed: {e}");
            bulk_ok = false;
            break;
        }
    }
    if bulk_ok {
        mcp_log!(
            McpLogLevel::Info,
            "bulk add resources completed: {EXAMPLE_BULK_COUNT}"
        );
    }

    match server.add_resource_template(
        RESOURCE_TEMPLATE_URI,
        RESOURCE_TEMPLATE_NAME,
        AddResourceTemplateOptionalParams {
            description: Some(RESOURCE_TEMPLATE_DESCRIPTION.into()),
            mime_type: Some(RESOURCE_TEMPLATE_MIME_TYPE.into()),
            ..Default::default()
        },
    ) {
        Ok(()) => mcp_log!(
            McpLogLevel::Info,
            "add resource template success: {RESOURCE_TEMPLATE_URI}"
        ),
        Err(e) => mcp_log!(McpLogLevel::Error, "add resource template failed: {e}"),
    }
}

fn register_completion(server: &McpServer) {
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
}

struct Options {
    enable_auth: bool,
    stateless: bool,
    json_response: bool,
    endpoint: String,
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = std::env::args().collect();
    let enable_auth = args.iter().any(|a| a == "--auth");
    let mut options = Options {
        enable_auth,
        stateless: false,
        json_response: true,
        endpoint: if enable_auth { AUTH_ENDPOINT } else { ENDPOINT }.to_string(),
    };
    for arg in args.iter().skip(1) {
        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: {} [--auth] [--port=<1-65535>] [--stateless] [--isJsonResponseDisable]",
                args.first().map(String::as_str).unwrap_or("ServerExample")
            );
            return None;
        }
        if arg == "--stateless" {
            options.stateless = true;
        } else if arg == "--isJsonResponseDisable" {
            options.json_response = false;
        } else if let Some(value) = arg.strip_prefix("--port=") {
            match value.parse::<u16>() {
                Ok(port) => options.endpoint = format!("http://127.0.0.1:{port}/mcp"),
                Err(_) => {
                    eprintln!("Invalid port: {value}");
                    return None;
                }
            }
        }
    }
    Some(options)
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Some(options) = parse_args() else {
        return std::process::ExitCode::FAILURE;
    };
    println!("=== MCP Server Example ===");
    if options.enable_auth {
        println!("Mode: Authentication and Authorization (Bearer token + Scopes)");
    }
    if options.stateless && !options.json_response {
        eprintln!("Error: --stateless requires JSON responses; do not use --isJsonResponseDisable");
        return std::process::ExitCode::FAILURE;
    }

    match File::options().append(true).create(true).open(LOG_FILE_NAME) {
        Ok(file) => *LOG_FILE.lock() = Some(file),
        Err(e) => {
            println!("Failed to open log file: {LOG_FILE_NAME} ({e})");
            return std::process::ExitCode::FAILURE;
        }
    }
    set_log_level(McpLogLevel::Debug as u8);
    set_log_callback(Some(file_log_callback));
    mcp_log!(McpLogLevel::Info, "Starting MCP Server test...");

    let config = ServerConfig {
        name: SERVER_NAME.into(),
        version: SERVER_VERSION.into(),
        worker_threads: WORKER_THREADS,
        ..Default::default()
    };
    let mut transport = StreamableHttpServerConfig::new(options.endpoint.clone());
    transport.io_threads = IO_THREADS;
    transport.is_json_response_enabled = options.json_response;
    transport.stateless = options.stateless;

    if options.enable_auth {
        let mut token_scopes = HashMap::new();
        token_scopes.insert(VALID_TOKEN.to_string(), "read write".to_string());
        let verifier: Arc<dyn TokenVerifier> = Arc::new(SimpleTokenVerifier::new(token_scopes));
        transport.authenticator = Some(Arc::new(BearerTokenAuthenticator::new(Some(verifier))));
        match ScopeBasedAuthorizer::new(REQUIRED_SCOPES) {
            Ok(authorizer) => transport.authorizer = Some(Arc::new(authorizer)),
            Err(e) => {
                mcp_log!(McpLogLevel::Error, "Configuration error: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        mcp_log!(McpLogLevel::Info, "Authentication and authorization configured:");
        mcp_log!(McpLogLevel::Info, "  - Required scopes: {REQUIRED_SCOPES}");
        mcp_log!(
            McpLogLevel::Info,
            "  - Valid token: {VALID_TOKEN} (scopes: read write)"
        );
    }

    let server = match McpServerFactory::create_streamable_http_server(config, transport) {
        Ok(server) => server,
        Err(e) => {
            mcp_log!(McpLogLevel::Error, "Failed to create MCP server instance: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    mcp_log!(McpLogLevel::Info, "MCP server instance created successfully");

    register_tools(&server);
    register_prompts(&server);
    register_resources(&server);
    register_completion(&server);

    if let Err(e) = server.run().await {
        mcp_log!(McpLogLevel::Error, "Failed to start MCP server: {e}");
        return std::process::ExitCode::FAILURE;
    }
    if server.is_running() {
        mcp_log!(McpLogLevel::Info, "Server status check: RUNNING");
    } else {
        mcp_log!(McpLogLevel::Warn, "Server status check: NOT RUNNING");
        return std::process::ExitCode::FAILURE;
    }
    mcp_log!(
        McpLogLevel::Info,
        "MCP server started successfully on {}",
        options.endpoint
    );

    println!("Server is now running. Press Ctrl+C to stop gracefully...");
    println!("  - Name: {SERVER_NAME}");
    println!("  - Version: {SERVER_VERSION}");
    println!("  - IO Threads: {IO_THREADS}");
    println!("  - Worker Threads: {WORKER_THREADS}");
    println!("  - Endpoint: {}", options.endpoint);
    if options.enable_auth {
        println!("  - Required Scopes: {REQUIRED_SCOPES}");
    }
    println!("Test endpoints:");
    println!("  - MCP endpoint: {}", options.endpoint);
    if options.enable_auth {
        println!("Valid tokens for testing:");
        println!("  - {VALID_TOKEN} (scopes: read write) - will succeed");
    }

    let mut counter: u64 = 0;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECONDS));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                mcp_log!(McpLogLevel::Info, "Received shutdown signal, stopping server...");
                break;
            }
            _ = heartbeat.tick() => {
                if !server.is_running() {
                    break;
                }
                counter += 1;
                mcp_log!(McpLogLevel::Debug, "Server heartbeat {counter} - still running...");
                if counter % LOG_INTERVAL_COUNT == 0 {
                    mcp_log!(McpLogLevel::Info, "Server active for {} seconds", counter * HEARTBEAT_INTERVAL_SECONDS);
                }
            }
        }
    }

    server.stop().await;
    mcp_log!(McpLogLevel::Info, "MCP server shutdown completed successfully");
    mcp_log!(McpLogLevel::Info, "MCP Server test completed");
    *LOG_FILE.lock() = None;
    println!("=== Test completed successfully ===");
    std::process::ExitCode::SUCCESS
}
