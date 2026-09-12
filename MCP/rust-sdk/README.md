# mcp-sdk

Rust port of `MCP/cpp-sdk`, the C++ Model Context Protocol SDK. The crate provides an MCP
client and an MCP server over Streamable HTTP and stdio. JSON-RPC message types and the SSE
parser come from the `ap-jsonrpc` crate.

Supported protocol versions: `2025-03-26` (default) and `2025-06-18` (latest).

## Modules

| Module | Purpose |
| --- | --- |
| `types` | Wire types (`Tool`, `CallToolResult`, capabilities, sampling, completion, configs). |
| `protocol` | Version constants, method names, header names, typed params and message parsing. |
| `error` | `McpError`, `JsonRpcErrorCode` and the `ErrorResult` alias. |
| `client` | `McpClient`, `McpClientFactory` and the client session. |
| `server` | `McpServer`, `McpServerFactory`, `ServerContext` and the tool, prompt and resource managers. |
| `transport` | `ClientTransport` and `ServerTransport` traits plus the Streamable HTTP and stdio transports. |
| `auth` | Bearer token provider, authenticator, authorizer and token verifier traits. |
| `session` | Pending request table, progress callbacks and timeouts. |
| `schema` | Small JSON Schema validator used for tool input and output. |
| `sampling_validation` | Checks tool use and tool result pairing in sampling messages. |
| `log` | SDK diagnostic logging (`mcp_log!`, `set_log_level`, `set_log_callback`). |

## C++ to Rust API mapping

### Factories and client

| C++ | Rust |
| --- | --- |
| `Mcp::McpClientFactory::CreateStreamableHttpClient(config, transport, auth)` | `McpClientFactory::create_streamable_http_client(config, transport, auth) -> Result<McpClient, McpError>` |
| `Mcp::McpClientFactory::CreateStdioClient(config, transport)` | `McpClientFactory::create_stdio_client(config, transport) -> McpClient` |
| `McpClient::Initialize()` | `McpClient::initialize().await -> Result<InitializeResult, McpError>` |
| `McpClient::ListTools(cursor)` | `McpClient::list_tools(cursor).await` |
| `McpClient::CallTool(name, argsJson, timeoutSec, progressCb)` | `McpClient::call_tool(name, Option<Value>, timeout_ms, Option<ProgressCallback>).await` |
| `McpClient::ListResources(cursor)` | `McpClient::list_resources(cursor).await` |
| `McpClient::ReadResource(uri)` | `McpClient::read_resource(uri).await` |
| `McpClient::SubscribeResource(uri)` / `UnsubscribeResource(uri)` | `McpClient::subscribe_resource(uri).await` / `unsubscribe_resource(uri).await` |
| `McpClient::ListResourcesTemplates()` | `McpClient::list_resources_templates().await` |
| `McpClient::ListPrompts()` | `McpClient::list_prompts().await` |
| `McpClient::GetPrompt(name, argsJson)` | `McpClient::get_prompt(name, Option<Value>).await` |
| `McpClient::SendPing()` | `McpClient::send_ping().await` |
| `McpClient::Complete(ref, argument, context)` | `McpClient::complete(reference, argument, context).await` |
| `McpClient::SetLoggingLevel(level)` | `McpClient::set_logging_level(level).await` |
| `McpClient::SendRootsListChanged()` | `McpClient::send_roots_list_changed().await` |
| `McpClient::SendProgressNotification(token, progress, total, message)` | `McpClient::send_progress_notification(token, progress, total, message).await` |
| `McpClient::GetServerCapabilities()` | `McpClient::get_server_capabilities() -> Result<ServerCapabilities, McpError>` |
| `McpClient::CloseGracefully()` | `McpClient::close_gracefully().await` |
| `McpClient::SetListRootsCallback(cb)` | `McpClient::set_list_roots_callback(ListRootsCallback)` |
| `McpClient::SetLoggingCallback(cb)` | `McpClient::set_logging_callback(LoggingCallback)` |
| `McpClient::SetElicitCallback(cb)` / `SetElicitUrlCallback(cb)` | `McpClient::set_elicit_callback` / `set_elicit_url_callback` |
| `McpClient::SetSamplingCreateMessageCallback(cb, capability)` | `McpClient::set_sampling_create_message_callback(cb, SamplingCapability)` |

Callbacks are `Arc<dyn Fn(..) -> BoxFuture<..> + Send + Sync>` values. A sampling callback returns
`Ok(None)` where the C++ callback returned `std::nullopt` (user rejected the request).

### Server

| C++ | Rust |
| --- | --- |
| `Mcp::McpServerFactory::CreateStreamableHttpServer(config, transport)` | `McpServerFactory::create_streamable_http_server(config, transport) -> Result<McpServer, McpError>` |
| `Mcp::McpServerFactory::CreateStdioServer(config)` | `McpServerFactory::create_stdio_server(config) -> Result<McpServer, McpError>` |
| `McpServer::Run()` | `McpServer::run().await -> Result<(), McpError>` |
| `McpServer::Stop()` | `McpServer::stop().await` |
| `McpServer::IsRunning()` | `McpServer::is_running()` |
| `McpServer::AddTool(name, ToolFunc, AddToolOptionalParams)` | `McpServer::add_tool(name, async closure, AddToolOptionalParams)` |
| `McpServer::RemoveTool(name)` | `McpServer::remove_tool(name)` |
| `McpServer::AddPrompt(name, RenderPromptFunc, AddPromptOptionalParams)` | `McpServer::add_prompt(name, async closure, AddPromptOptionalParams)` |
| `McpServer::RemovePrompt(name)` | `McpServer::remove_prompt(name)` |
| `McpServer::AddResource(uri, name, ReadResourceFunc, AddResourceOptionalParams)` | `McpServer::add_resource(uri, name, async closure, AddResourceOptionalParams)` |
| `McpServer::RemoveResource(uri)` | `McpServer::remove_resource(uri)` |
| `McpServer::AddResourceTemplate(uriTemplate, name, params)` | `McpServer::add_resource_template(uri_template, name, params)` |
| `McpServer::RemoveResourceTemplate(uriTemplate)` | `McpServer::remove_resource_template(uri_template)` |
| `McpServer::RegisterSetLoggingLevelHandler(handler)` | `McpServer::register_set_logging_level_handler(Fn(&str) -> Result<(), McpError>)` |
| `McpServer::AddCompletion(CompleteFunc)` | `McpServer::add_completion(async closure)` |
| `ToolFunc` sync and async forms | One async closure `Fn(ServerContext, String, Value) -> Future<Result<CallToolResult, McpError>>` |
| `ToolReturn = std::variant<CallToolResult, std::string>` | `Err(McpError)` from the handler becomes an `isError` result |
| `RenderPromptFunc` | `Fn(ServerContext, String, Option<Value>) -> Future<Result<GetPromptResult, McpError>>` |
| `ReadResourceFunc` | `Fn(ServerContext, String) -> Future<Result<ReadResourceResult, McpError>>` |
| `ServerContext { session, meta, responseCallback }` | `ServerContext { session: Arc<ServerSession>, meta: Option<RequestParamsMeta> }` |
| `McpServerSession::SendToolListChangedNotification()` | `ServerSession::send_tool_list_changed_notification().await` |
| `McpServerSession::SendPromptListChangedNotification()` | `ServerSession::send_prompt_list_changed_notification().await` |
| `McpServerSession::SendResourceListChangedNotification()` | `ServerSession::send_resource_list_changed_notification().await` |
| `McpServerSession::ListRoots()` | `ServerSession::list_roots().await` |
| `McpServerSession::GetClientCapabilities()` | `ServerSession::get_client_capabilities()` |
| `McpServerSession::SendProgressNotification(token, progress, total, message)` | `ServerSession::send_progress_notification(token, progress, total, message).await` |
| `McpServerSession::SamplingCreateMessage(params)` | `ServerSession::sampling_create_message(params).await` |
| server side elicitation | `ServerSession::elicit(..)` and `ServerSession::elicit_url(..)` |
| `notifications/message` from the server | `ServerSession::send_log_message(level, data, logger).await` |
| `notifications/resources/updated` | `ServerSession::send_resource_updated_notification(uri).await` |

Async C++ handlers used `ctx.responseCallback` from a background thread. Rust handlers are async
functions, so a long running tool simply awaits and returns its result.

### Authentication

| C++ | Rust |
| --- | --- |
| `Mcp::AuthProvider::Apply(headers)` | `auth::AuthProvider::apply(&self, &mut HashMap<String, String>)` |
| `Mcp::BearerTokenProvider` | `auth::BearerTokenProvider` (`new`, `set_token`, `token`) |
| `Mcp::TokenVerifier::VerifyToken(token)` | `auth::TokenVerifier::verify_token(&self, &str) -> AuthenticationResult` |
| `Mcp::Authenticator::Authenticate(headers)` | `auth::Authenticator::authenticate(&self, &HashMap<String, String>)` |
| `Mcp::NoAuthAuthenticator` | `auth::NoAuthAuthenticator` |
| `Mcp::BearerTokenAuthenticator(verifier)` | `auth::BearerTokenAuthenticator::new(Option<Arc<dyn TokenVerifier>>)` |
| `Mcp::Authorizer::Authorize(result)` | `auth::Authorizer::authorize(&self, &AuthenticationResult) -> bool` |
| `Mcp::ScopeBasedAuthorizer(scopes)` | `auth::ScopeBasedAuthorizer::new(&str) -> Result<Self, McpError>` |
| example `SimpleTokenVerifier` | `auth::SimpleTokenVerifier::new(HashMap<String, String>)` |

### Logging

| C++ | Rust |
| --- | --- |
| `MCP_LOG(level, format, ...)` | `mcp_log!(McpLogLevel::Info, "format {}", arg)` |
| `SetLogLevel(level)` / `GetLogLevel()` | `log::set_log_level(u8) -> i32` / `log::get_log_level()` |
| `SetLogCallback(cb)` | `log::set_log_callback(Option<fn(McpLogLevel, String)>)` |
| `MCP_LOG_LEVEL_DEBUG .. MCP_LOG_LEVEL_FATAL` | `log::McpLogLevel::{Debug, Info, Warn, Error, Fatal}` |

Without a callback, log lines go to `tracing` and never to stdout.

### Types and errors

| C++ | Rust |
| --- | --- |
| `Mcp::JsonValue` (nlohmann) and JSON strings | `serde_json::Value` |
| `Mcp::MetaMap` | `types::MetaMap` (`serde_json::Map<String, Value>`) |
| `Mcp::ContentType` variant | `types::ContentBlock` enum (alias `ContentType`) |
| `Mcp::SamplingMessage::content` variant | `types::SamplingContent::{Single, Multiple}` |
| `Mcp::CompleteReference` variant | `types::CompleteReference::{Resource, Prompt}` |
| `Mcp::ProgressToken` variant | `types::ProgressToken::{Number, String}` |
| `Mcp::ClientInfo` / `Mcp::ServerInfo` | `types::Implementation` (aliases `ClientInfo`, `ServerInfo`) |
| `Mcp::ErrorResult` | `ErrorResult` alias for `ap_jsonrpc::RpcError` |
| `std::invalid_argument` | `McpError::InvalidArgument` |
| `std::runtime_error` (not initialized, closed) | `McpError::NotInitialized`, `McpError::Closed`, `McpError::InvalidState` |
| JSON-RPC error responses | `McpError::Rpc(RpcError)` with `code()`, `code_enum()`, `message()` |
| request timeout | `McpError::Timeout(ms)` |
| `Mcp::JsonRpcErrorCode` | `JsonRpcErrorCode` (`code()`, `from_code()`) |

Timeouts are given in milliseconds. `call_tool` takes `timeout_ms == 0` for the default of
30000 ms. The C++ `CallTool` took seconds.

## Quick start

### Streamable HTTP server

```rust,no_run
use mcp_sdk::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let config = ServerConfig {
        name: "ExampleServer".into(),
        version: "1.0.0".into(),
        ..Default::default()
    };
    let transport = StreamableHttpServerConfig::new("http://127.0.0.1:8000/mcp");
    let server = McpServerFactory::create_streamable_http_server(config, transport)?;

    server.add_tool(
        "echo",
        |_ctx, _name, args| async move {
            let text = args["user_query"].as_str().unwrap_or("").to_string();
            Ok(CallToolResult::text(format!("Echo: {text}")))
        },
        AddToolOptionalParams {
            description: Some("Echoes back the input message".into()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {"user_query": {"type": "string"}},
                "required": ["user_query"]
            })),
            ..Default::default()
        },
    )?;

    server.run().await?;
    tokio::signal::ctrl_c().await.ok();
    server.stop().await;
    Ok(())
}
```

Set `transport.is_json_response_enabled = false` for SSE responses to POST requests, and
`transport.stateless = true` to skip session tracking. Bearer token authentication is enabled by
setting `transport.authenticator` and `transport.authorizer`.

### Streamable HTTP client

```rust,no_run
use std::sync::Arc;
use mcp_sdk::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let config = ClientConfig {
        name: "ExampleClient".into(),
        version: "1.0.0".into(),
    };
    let http = StreamableHttpClientConfig {
        endpoint: "http://127.0.0.1:8000/mcp".into(),
        ..Default::default()
    };
    let auth: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new("your-token"));
    let client = McpClientFactory::create_streamable_http_client(config, http, Some(auth))?;

    client.initialize().await?;
    let tools = client.list_tools(None).await?;
    println!("{} tools", tools.tools.len());
    let result = client
        .call_tool("echo", Some(json!({"user_query": "hello"})), 0, None)
        .await?;
    println!("{:?}", result.content[0].as_text());
    client.close_gracefully().await;
    Ok(())
}
```

### stdio server

```rust,no_run
use mcp_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let server = McpServerFactory::create_stdio_server(ServerConfig {
        name: "StdioServer".into(),
        version: "1.0.0".into(),
        ..Default::default()
    })?;
    server.add_tool(
        "echo",
        |_ctx, _name, args| async move { Ok(CallToolResult::text(args.to_string())) },
        AddToolOptionalParams::default(),
    )?;
    // Reads requests from stdin and writes responses to stdout until stdin closes.
    server.run().await?;
    tokio::signal::ctrl_c().await.ok();
    server.stop().await;
    Ok(())
}
```

`McpServer::set_stdio_streams(reader, writer)` replaces the process streams, which the tests use
with `tokio::io::duplex`.

### stdio client

```rust,no_run
use mcp_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let client = McpClientFactory::create_stdio_client(
        ClientConfig::default(),
        StdioClientConfig {
            command: "python3".into(),
            args: vec!["my_server.py".into()],
            env: Default::default(),
        },
    );
    let init = client.initialize().await?;
    println!("connected to {}", init.server_info.name);
    client.close_gracefully().await;
    Ok(())
}
```

An empty `command` uses the current process stdin and stdout.

### TLS

Set `TlsConfig` on the server (`enabled`, `cert_file`, `key_file`, optional `ca_file` for
mutual TLS). On the client, `ca_file`, `cert_file`, `key_file`, `server_name` and
`verify_peer` apply to every `https` endpoint.

## Examples

The C++ examples are ported to cargo examples. Start the server first, then run a client in a
second terminal.

```sh
cargo run -p mcp-sdk --example server_example
cargo run -p mcp-sdk --example server_example -- --auth
cargo run -p mcp-sdk --example server_example -- --port=9000 --stateless
cargo run -p mcp-sdk --example server_example -- --isJsonResponseDisable

cargo run -p mcp-sdk --example tool_example
cargo run -p mcp-sdk --example tool_example -- --auth
cargo run -p mcp-sdk --example tool_example -- --port=9000
cargo run -p mcp-sdk --example prompt_example
cargo run -p mcp-sdk --example resource_example
cargo run -p mcp-sdk --example sampling_example
```

`server_example` listens on `http://127.0.0.1:8000/mcp`, or on port 8001 with `--auth`. It writes
its log to `server_example.log` in the working directory. The auth mode accepts the bearer token
`valid-token-12345` with scopes `read write`.

## Tests

```sh
cargo test -p mcp-sdk
```

Unit tests live next to the code. Integration tests are `tests/http_st.rs` (Streamable HTTP,
JSON and SSE modes, stateless mode, raw HTTP status codes, auth), `tests/stdio_st.rs` (in-memory
streams and a subprocess) and `tests/tls_st.rs`. The TLS tests generate certificates with the
`openssl` command line tool and skip themselves when it is missing.

## Deviations from the C++ SDK

- Futures and threads became `async fn` on tokio. `ioThreads` and `workerThreads` are validated
  but the runtime is the caller's tokio runtime.
- Tool arguments, prompt arguments, schemas and `structuredContent` are `serde_json::Value`
  instead of JSON strings.
- Sync and async handler variants collapse into one async closure per handler type.
- Exceptions became `Result<_, McpError>`. Error codes and messages follow the C++ SDK.
- `call_tool` takes milliseconds; the C++ version took seconds.
- JSON Schema validation uses a small built-in validator (`schema` module) instead of
  `nlohmann/json-schema-validator`. It covers type, enum, const, numeric and string bounds,
  pattern, items, required, properties, additionalProperties and the boolean combinators.
- `resources.subscribe` is always advertised as `false`, as in the C++ server session.
- The HTTP server is axum plus axum-server with rustls. TLS uses the ring provider.
- Progress callbacks stay registered for five seconds after their request completes so
  notifications delivered on the GET stream after the response are not lost.
- The C++ `SetLogCallback` default printed to stdout. Without a callback, this crate logs through
  `tracing`.

## Intentionally left out

- Python interop scripts (`python_mcp_client.py`, `python_mcp_server.py`) and shell runners.
- The internal event system, thread pools, HTTP parser and networking layer of the C++ SDK
  (`tests/ut/event`, `tests/ut/net`, `tests/ut/http`). tokio, hyper and reqwest replace them.
- CMake packaging and install rules.
- The `MCP_LOG` file, function and thread id prefix uses the Rust thread id instead of the
  Linux `gettid` value.
