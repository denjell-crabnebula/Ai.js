// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! End to end tests over Streamable HTTP, porting `tests/ut/client/client_test.cpp`,
//! `tests/ut/st/test_st.cpp` and the HTTP level checks of the server tests.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use mcp_sdk::prelude::*;
use serde_json::{Value, json};

type ProgressEvents = Arc<Mutex<Vec<(f64, Option<f64>, Option<String>)>>>;

async fn init_client(endpoint: &str) -> TestResult<McpClient> {
    let client = http_client(endpoint, None)?;
    client.initialize().await?;
    Ok(client)
}

#[tokio::test]
async fn initialize_ping_and_capabilities() -> TestResult {
    for json_mode in [true, false] {
        let (server, endpoint, _rec) = start_http_server(json_mode, false).await?;
        let client = http_client(&endpoint, None)?;
        let init = client.initialize().await?;
        assert_eq!(init.server_info.name, "TestMCPServer");
        assert_eq!(init.protocol_version, mcp_sdk::protocol::LATEST_PROTOCOL_VERSION);
        let caps = client.get_server_capabilities()?;
        assert!(caps.tools.is_some());
        assert_eq!(caps.resources.required()?.subscribe, Some(false));
        assert!(matches!(
            client.initialize().await,
            Err(McpError::AlreadyInitialized)
        ));
        client.send_ping().await?;
        assert_eq!(server.session_count(), 1);
        client.close_gracefully().await;
        wait_for(|| server.session_count() == 0).await?;
        assert!(matches!(client.send_ping().await, Err(McpError::NotInitialized)));
        server.stop().await;
    }
    Ok(())
}

#[tokio::test]
async fn list_and_call_tools() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(true, false).await?;
    let client = init_client(&endpoint).await?;

    let mut cursor: Option<String> = None;
    let mut total = 0;
    let mut pages = 0;
    let mut found_echo = false;
    loop {
        let page = client.list_tools(cursor.clone()).await?;
        pages += 1;
        total += page.tools.len();
        assert!(page.tools.len() <= DEFAULT_TOOLS_PAGE_SIZE);
        if let Some(tool) = page.tools.iter().find(|t| t.name == ECHO_TOOL_NAME) {
            found_echo = true;
            assert_eq!(tool.description.as_deref(), Some(ECHO_TOOL_DESCRIPTION));
            assert!(tool.input_schema.is_some());
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(found_echo);
    assert_eq!(total, BULK_COUNT + 5);
    assert!(pages >= 3);

    let result = client
        .call_tool(
            ECHO_TOOL_NAME,
            Some(json!({"user_query": "Shenzhen weather"})),
            5000,
            None,
        )
        .await?;
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 5);
    assert_eq!(result.content[0].as_text(), Some("Echo: Shenzhen weather"));
    assert!(matches!(&result.content[1], ContentBlock::Image(i) if i.mime_type == "image"));
    assert!(matches!(&result.content[2], ContentBlock::ResourceLink(l) if l.size == Some(1024)));
    assert!(matches!(&result.content[3], ContentBlock::Audio(_)));
    assert!(matches!(&result.content[4], ContentBlock::EmbeddedResource(_)));
    assert_eq!(
        result.structured_content,
        Some(json!({"result": "Shenzhen weather"}))
    );

    // Missing required argument: input schema validation fails on the server.
    let err = client
        .call_tool(ECHO_TOOL_NAME, Some(json!({})), 5000, None)
        .await
        .err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));
    assert!(err.message().contains("Input validation failed"));

    let err = client
        .call_tool("__not_exist_tool__", Some(json!({"user_query": "x"})), 5000, None)
        .await
        .err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));
    assert_eq!(err.message(), "Tool not found: __not_exist_tool__");

    let async_result = client
        .call_tool(
            "async_echo",
            Some(json!({"user_query": "Hello from async echo"})),
            5000,
            None,
        )
        .await?;
    assert_eq!(
        async_result.content[0].as_text(),
        Some("Async Echo: Hello from async echo")
    );

    // A very short timeout completes either way without hanging.
    let quick = tokio::time::timeout(
        Duration::from_millis(1000),
        client.call_tool(
            ECHO_TOOL_NAME,
            Some(json!({"user_query": "Timeout test"})),
            1,
            None,
        ),
    )
    .await;
    assert!(quick.is_ok());

    client.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn prompts_resources_templates_and_completion() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(true, false).await?;
    let client = init_client(&endpoint).await?;

    let prompts = client.list_prompts().await?;
    assert_eq!(prompts.prompts.len(), 1);
    assert_eq!(prompts.prompts[0].name, "example_prompt");
    assert_eq!(prompts.prompts[0].arguments.as_ref().required()?.len(), 2);

    let prompt = client.get_prompt("example_prompt", None).await?;
    assert_eq!(prompt.messages.len(), 1);
    assert_eq!(prompt.messages[0].role, RoleType::Assistant);
    assert_eq!(
        prompt.messages[0].content.as_text(),
        Some("Hello, friend! (language=English)")
    );
    let prompt = client
        .get_prompt(
            "example_prompt",
            Some(json!({"name": "Alice", "language": "French"})),
        )
        .await?;
    assert_eq!(
        prompt.messages[0].content.as_text(),
        Some("Hello, Alice! (language=French)")
    );
    let err = client
        .get_prompt("__not_exist_prompt__", None)
        .await
        .err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));
    assert_eq!(err.message(), "Prompt not found: __not_exist_prompt__");

    let mut cursor: Option<String> = None;
    let mut total = 0;
    let mut first_uri = None;
    loop {
        let page = client.list_resources(cursor.clone()).await?;
        if first_uri.is_none() {
            first_uri = page.resources.first().map(|r| r.uri.clone());
        }
        total += page.resources.len();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(total, BULK_COUNT + 1);
    let first_uri = first_uri.required()?;
    let detailed = client.list_resources(None).await?;
    let info = detailed
        .resources
        .iter()
        .find(|r| r.uri == RESOURCE_URI)
        .required()?;
    assert_eq!(
        info.icons.as_ref().required()?[0].src,
        "https://example.com/icons/document.png"
    );
    assert_eq!(info.annotations.as_ref().required()?.priority, Some(1.0));

    let read = client.read_resource(RESOURCE_URI).await?;
    assert!(
        matches!(&read.contents[0], ResourceContents::Text(t) if t.text == format!("hello, {RESOURCE_URI}"))
    );
    let err = client
        .read_resource("http://example.com/__not_exist_resource__")
        .await
        .err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));

    client.subscribe_resource(&first_uri).await?;
    client.unsubscribe_resource(&first_uri).await?;
    let err = client.subscribe_resource("nope://x").await.err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::ServerError));

    let templates = client.list_resources_templates().await?;
    assert_eq!(templates.resource_templates.len(), 1);
    assert_eq!(
        templates.resource_templates[0].uri_template,
        "http://example.com/resourceTemplate/{id}"
    );
    assert_eq!(
        templates.resource_templates[0].mime_type.as_deref(),
        Some("text/plain")
    );

    let mut args = std::collections::HashMap::new();
    args.insert("framework".to_string(), "django".to_string());
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
                arguments: Some(args),
            }),
        )
        .await?;
    assert_eq!(
        result.completion.values,
        vec!["python", "pytorch", "pydantic", "django"]
    );
    assert_eq!(result.completion.total, Some(4));
    let result = client
        .complete(
            CompleteReference::Resource(ResourceTemplateReference {
                uri: "file:///path/to/template".into(),
            }),
            CompletionArgument {
                name: "format".into(),
                value: "j".into(),
            },
            None,
        )
        .await?;
    assert_eq!(result.completion.values, vec!["json", "yaml", "txt"]);

    client.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn set_logging_level_reaches_handler() -> TestResult {
    let (server, endpoint, recorded) = start_http_server(true, false).await?;
    let client = init_client(&endpoint).await?;
    client.set_logging_level(LoggingLevel::Debug).await?;
    assert_eq!(recorded.logging_level.lock().as_deref(), Some("debug"));
    client.close_gracefully().await;
    server.stop().await;

    // Without a handler the server answers INVALID_PARAMS.
    let port = free_port()?;
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    let bare = McpServerFactory::create_streamable_http_server(
        ServerConfig::default(),
        StreamableHttpServerConfig::new(endpoint.clone()),
    )?;
    bare.run().await?;
    let client = init_client(&endpoint).await?;
    let err = client.set_logging_level(LoggingLevel::Info).await.err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));
    assert_eq!(err.message(), "not set LoggingLevelHandler");
    let err = client.list_tools(None).await?;
    assert!(err.tools.is_empty());
    let err = client
        .complete(
            CompleteReference::Prompt(PromptReference { name: "p".into() }),
            CompletionArgument::default(),
            None,
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::ServerError));
    assert_eq!(err.message(), "Completion handler not registered");
    client.close_gracefully().await;
    bare.stop().await;
    Ok(())
}

#[tokio::test]
async fn progress_notifications_over_get_stream() -> TestResult {
    for json_mode in [false, true] {
        let (server, endpoint, _rec) = start_http_server(json_mode, false).await?;
        let client = init_client(&endpoint).await?;
        let seen: ProgressEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let cb: ProgressCallback = Arc::new(move |p, t, m| sink.lock().push((p, t, m)));
        let result = client
            .call_tool(
                "progress_tool",
                Some(json!({"task_name": "Data Processing", "steps": 4})),
                5000,
                Some(cb),
            )
            .await?;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(
            result.content[0].as_text(),
            Some("Task 'Data Processing' completed successfully")
        );
        wait_for(|| seen.lock().len() == 5).await?;
        let events = seen.lock().clone();
        assert_eq!(events[0].0, 0.0);
        assert_eq!(events[4].0, 1.0);
        assert_eq!(events[4].1, Some(4.0));
        assert_eq!(
            events[2].2.as_deref(),
            Some("Processing Data Processing: step 2 of 4")
        );
        client.close_gracefully().await;
        server.stop().await;
    }
    Ok(())
}

#[tokio::test]
async fn server_initiated_sampling_and_roots() -> TestResult {
    for json_mode in [true, false] {
        let (server, endpoint, _rec) = start_http_server(json_mode, false).await?;
        let client = http_client(&endpoint, None)?;
        let sampled: Arc<Mutex<Vec<CreateMessageParams>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = sampled.clone();
        client.set_sampling_create_message_callback(
            Arc::new(move |params| {
                sink.lock().push(params);
                Box::pin(async {
                    Ok(Some(CreateMessageResult {
                        model: "demo-client-model".into(),
                        role: RoleType::Assistant,
                        content: SamplingContent::text("This is a simulated LLM response."),
                        stop_reason: Some("stop".into()),
                        meta: None,
                    }))
                })
            }),
            SamplingCapability::default(),
        );
        client.set_list_roots_callback(Arc::new(|| {
            Box::pin(async {
                Ok(ListRootsResult {
                    roots: vec![Root {
                        uri: "file:///tmp".into(),
                        name: Some("tmp".into()),
                    }],
                    meta: None,
                })
            })
        }));
        client.initialize().await?;

        let result = client
            .call_tool(
                "sampling_echo",
                Some(json!({"prompt": "Tell me a joke"})),
                5000,
                None,
            )
            .await?;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(
            result.content[0].as_text(),
            Some("Sampling response: This is a simulated LLM response.")
        );
        {
            let params = sampled.lock();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].max_tokens, 100);
            assert_eq!(
                params[0].messages[0].content.first_text().as_deref(),
                Some("Tell me a joke")
            );
        }

        let roots = client.call_tool("roots_tool", None, 5000, None).await?;
        assert!(!roots.is_error);
        assert_eq!(roots.content[0].as_text(), Some("file:///tmp"));

        client.send_roots_list_changed().await?;
        client.close_gracefully().await;
        server.stop().await;
    }

    // Without the callbacks the server reports the missing capability as a tool error.
    let (server, endpoint, _rec) = start_http_server(true, false).await?;
    let client = init_client(&endpoint).await?;
    let result = client
        .call_tool("sampling_echo", Some(json!({"prompt": "x"})), 5000, None)
        .await?;
    assert!(result.is_error);
    assert!(
        result.content[0]
            .as_text()
            .required()?
            .contains("Client does not support sampling/createMessage")
    );
    let roots = client.call_tool("roots_tool", None, 5000, None).await?;
    assert!(roots.is_error);
    client.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn server_logging_notification_reaches_client() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(false, false).await?;
    server.add_tool(
        "log_tool",
        |ctx, _name, _args| async move {
            ctx.session
                .send_log_message("info", json!("tool ran"), "test-server")
                .await?;
            ctx.session.send_tool_list_changed_notification().await?;
            Ok(CallToolResult::text("logged"))
        },
        AddToolOptionalParams::default(),
    )?;
    let client = http_client(&endpoint, None)?;
    let logs: Arc<Mutex<Vec<(String, Value, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    client.set_logging_callback(Arc::new(move |level, data, logger| {
        sink.lock().push((level, data, logger));
    }));
    client.initialize().await?;
    let result = client.call_tool("log_tool", None, 5000, None).await?;
    assert!(!result.is_error, "{:?}", result.content);
    wait_for(|| !logs.lock().is_empty()).await?;
    let entry = logs.lock()[0].clone();
    assert_eq!(
        entry,
        ("info".to_string(), json!("tool ran"), "test-server".to_string())
    );
    client.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn stateless_mode() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(true, true).await?;
    let client = init_client(&endpoint).await?;
    let result = client
        .call_tool(
            ECHO_TOOL_NAME,
            Some(json!({"user_query": "stateless"})),
            5000,
            None,
        )
        .await?;
    assert_eq!(result.content[0].as_text(), Some("Echo: stateless"));
    assert_eq!(server.session_count(), 0);
    // Server initiated requests are refused in stateless mode.
    let sampling = client
        .call_tool("sampling_echo", Some(json!({"prompt": "x"})), 5000, None)
        .await?;
    assert!(sampling.is_error);
    assert!(sampling.content[0].as_text().required()?.contains("stateless"));
    client.close_gracefully().await;

    // Raw requests: no session header needed, GET and DELETE are 405.
    let http = reqwest::Client::new();
    let response = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await?;
    assert_eq!(body["result"], json!({}));
    let get = http
        .get(&endpoint)
        .header("accept", "text/event-stream")
        .send()
        .await?;
    assert_eq!(get.status(), 405);
    assert_eq!(get.headers().get("allow").required()?, "POST");
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn raw_http_status_codes() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(true, false).await?;
    let http = reqwest::Client::new();

    let put = http.put(&endpoint).send().await?;
    assert_eq!(put.status(), 405);
    assert_eq!(put.headers().get("allow").required()?, "GET, POST, DELETE");

    let unknown = http.post(endpoint.replace("/mcp", "/other")).send().await?;
    assert_eq!(unknown.status(), 404);
    assert_eq!(unknown.text().await?, "Endpoint not found");

    let wrong_accept = http
        .post(&endpoint)
        .header("accept", "text/plain")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
        .send()
        .await?;
    assert_eq!(wrong_accept.status(), 406);
    let body: Value = wrong_accept.json().await?;
    assert_eq!(body["id"], "server-error");
    assert_eq!(body["error"]["code"], -32600);

    let bad_type = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await?;
    assert_eq!(bad_type.status(), 415);

    let bad_json = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body("not json")
        .send()
        .await?;
    assert_eq!(bad_json.status(), 400);
    let body: Value = bad_json.json().await?;
    assert_eq!(body["error"]["code"], -32700);

    // A non-initialize request on a fresh session is rejected until initialize.
    let init = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#)
        .send()
        .await
        ?;
    assert_eq!(init.status(), 200);
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .required()?
        .to_str()?
        .to_string();
    assert!(!session_id.is_empty());
    let body: Value = init.json().await?;
    assert_eq!(body["result"]["serverInfo"]["name"], "TestMCPServer");

    let unknown_session = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-session-id", "does-not-exist")
        .body(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .send()
        .await?;
    assert_eq!(unknown_session.status(), 404);

    let bad_version = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session_id)
        .header("mcp-protocol-version", "1999-01-01")
        .body(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .send()
        .await?;
    assert_eq!(bad_version.status(), 400);

    let notification = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session_id)
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .await?;
    assert_eq!(notification.status(), 202);

    let batch = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session_id)
        .body(r#"[{"jsonrpc":"2.0","id":3,"method":"ping"},{"jsonrpc":"2.0","id":4,"method":"nope"}]"#)
        .send()
        .await?;
    assert_eq!(batch.status(), 200);
    let body: Value = batch.json().await?;
    let items = body.as_array().required()?;
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i["id"] == 3 && i["result"] == json!({})));
    assert!(items.iter().any(|i| i["id"] == 4 && i["error"]["code"] == -32601));

    let delete_wrong = http
        .delete(&endpoint)
        .header("mcp-session-id", "other")
        .send()
        .await?;
    assert_eq!(delete_wrong.status(), 404);
    let delete = http
        .delete(&endpoint)
        .header("mcp-session-id", &session_id)
        .send()
        .await?;
    assert_eq!(delete.status(), 200);
    wait_for(|| server.session_count() == 0).await?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn sse_mode_raw_stream() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(false, false).await?;
    let http = reqwest::Client::new();
    let init = http
        .post(&endpoint)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#)
        .send()
        .await?;
    assert_eq!(init.status(), 200);
    assert_eq!(
        init.headers().get("content-type").required()?,
        "text/event-stream"
    );
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .required()?
        .to_str()?
        .to_string();
    let text = init.text().await?;
    let events = ap_jsonrpc::sse::parse_all(&text);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.as_deref(), Some("message"));
    let message: Value = serde_json::from_str(&events[0].data)?;
    assert_eq!(message["result"]["protocolVersion"], "2025-06-18");

    // JSON-only accept is rejected in SSE mode.
    let json_only = http
        .post(&endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session_id)
        .body(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .send()
        .await?;
    assert_eq!(json_only.status(), 406);

    // Only one GET stream per session.
    let first = http
        .get(&endpoint)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", &session_id)
        .send()
        .await?;
    assert_eq!(first.status(), 200);
    let second = http
        .get(&endpoint)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", &session_id)
        .send()
        .await?;
    assert_eq!(second.status(), 409);
    drop(first);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn authentication_and_authorization() -> TestResult {
    let port = free_port()?;
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    let mut tokens = std::collections::HashMap::new();
    tokens.insert("valid-token-12345".to_string(), "read write".to_string());
    tokens.insert("read-only".to_string(), "read".to_string());
    let verifier: Arc<dyn TokenVerifier> = Arc::new(SimpleTokenVerifier::new(tokens));
    let transport = StreamableHttpServerConfig {
        endpoint: endpoint.clone(),
        is_json_response_enabled: true,
        io_threads: 1,
        authenticator: Some(Arc::new(BearerTokenAuthenticator::new(Some(verifier)))),
        authorizer: Some(Arc::new(ScopeBasedAuthorizer::new("read write")?)),
        ..Default::default()
    };
    let server = McpServerFactory::create_streamable_http_server(ServerConfig::default(), transport)?;
    configure_server(&server, Arc::new(Recorded::default()))?;
    server.run().await?;

    let http = reqwest::Client::new();
    let anonymous = http.post(&endpoint).send().await?;
    assert_eq!(anonymous.status(), 401);
    let body: Value = anonymous.json().await?;
    assert_eq!(body["error"], "Missing Authorization header");
    let forbidden = http
        .post(&endpoint)
        .header("authorization", "Bearer read-only")
        .send()
        .await?;
    assert_eq!(forbidden.status(), 403);
    let body: Value = forbidden.json().await?;
    assert_eq!(body["error"], "Insufficient permissions");

    let no_auth = http_client(&endpoint, None)?;
    let err = no_auth.initialize().await.err_or_fail()?;
    assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidRequest));
    assert_eq!(err.message(), "HTTP error: 401");
    no_auth.close_gracefully().await;

    let provider: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new("valid-token-12345"));
    let client = http_client(&endpoint, Some(provider))?;
    client.initialize().await?;
    let result = client
        .call_tool(
            ECHO_TOOL_NAME,
            Some(json!({"user_query": "Hello from echo tool!"})),
            5000,
            None,
        )
        .await?;
    assert_eq!(result.content[0].as_text(), Some("Echo: Hello from echo tool!"));
    client.close_gracefully().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn concurrent_clients_and_requests() -> TestResult {
    let (server, endpoint, _rec) = start_http_server(true, false).await?;
    let mut tasks = Vec::new();
    for i in 0..4 {
        let endpoint = endpoint.clone();
        tasks.push(tokio::spawn(async move {
            let client = init_client(&endpoint).await?;
            let mut calls = Vec::new();
            for j in 0..3 {
                calls.push(client.call_tool(
                    ECHO_TOOL_NAME,
                    Some(json!({"user_query": format!("{i}-{j}")})),
                    5000,
                    None,
                ));
            }
            for (j, result) in futures::future::join_all(calls).await.into_iter().enumerate() {
                let result = result?;
                assert_eq!(
                    result.content[0].as_text(),
                    Some(format!("Echo: {i}-{j}").as_str())
                );
            }
            client.close_gracefully().await;
            Ok::<(), ap_support::testing::TestError>(())
        }));
    }
    for t in tasks {
        t.await??;
    }
    wait_for(|| server.session_count() == 0).await?;
    server.stop().await;
    Ok(())
}
