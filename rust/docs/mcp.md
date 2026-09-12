# MCP SDK

Crate `mcp-sdk` (`MCP/rust-sdk`) implements the Model Context Protocol client and server with
two transports: Streamable HTTP (with optional SSE responses, sessions and bearer-token
authentication) and stdio. It is wire compatible with the C++ SDK it ports and with any MCP
implementation that follows the specification.

Import everything you normally need with `use mcp_sdk::prelude::*;`.

## Server over Streamable HTTP

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

Transport options on `StreamableHttpServerConfig`:

| Field | Meaning |
|-------|---------|
| `is_json_response_enabled` | `true` answers POST with JSON; `false` answers with an SSE stream |
| `stateless` | Skip session tracking; every request is independent |
| `authenticator`, `authorizer` | Bearer-token authentication and scope authorization hooks |
| `tls` | `TlsConfig` with `enabled`, `cert_file`, `key_file` and an optional `ca_file` for mutual TLS |

Tools, prompts and resources are registered with `add_tool`, `add_prompt` and `add_resource`;
each handler is an async closure that receives the request context and the arguments. The
server also supports sampling requests to the client and server-initiated notifications.

## Server over stdio

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
    server.run().await?;
    tokio::signal::ctrl_c().await.ok();
    server.stop().await;
    Ok(())
}
```

The server reads requests from stdin and writes responses to stdout until stdin closes.
`McpServer::set_stdio_streams(reader, writer)` replaces the process streams, which is how the
tests drive a server through `tokio::io::duplex`.

## Client over Streamable HTTP

```rust,no_run
use std::sync::Arc;
use mcp_sdk::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let config = ClientConfig { name: "ExampleClient".into(), version: "1.0.0".into() };
    let http = StreamableHttpClientConfig {
        endpoint: "http://127.0.0.1:8000/mcp".into(),
        ..Default::default()
    };
    let auth: Arc<dyn AuthProvider> = Arc::new(BearerTokenProvider::new("your-token"));
    let client = McpClientFactory::create_streamable_http_client(config, http, Some(auth))?;

    client.initialize().await?;
    let tools = client.list_tools(None).await?;
    println!("{} tools", tools.tools.len());
    let result = client.call_tool("echo", Some(json!({"user_query": "hello"})), 0, None).await?;
    println!("{:?}", result.content[0].as_text());
    client.close_gracefully().await;
    Ok(())
}
```

`StreamableHttpClientConfig` carries the TLS settings for `https` endpoints (`ca_file`,
`cert_file`, `key_file`, `server_name`, `verify_peer`). Timeouts on the call methods are in
milliseconds; 0 uses the default.

## Client over stdio

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

An empty `command` uses the current process's own stdin and stdout.

## Logging

The SDK's log callback API from the C++ original is kept: `mcp_log!(level, ...)` and
`set_log_callback` route through the crate's log module, and every event is also emitted as a
`tracing` event under the `mcp_sdk` target. See [Logging](logging.md) for how to enable it.

## Examples

```bash
cargo run -p mcp-sdk --example server_example              # http://127.0.0.1:8000/mcp
cargo run -p mcp-sdk --example server_example -- --auth    # port 8001, bearer token valid-token-12345
cargo run -p mcp-sdk --example tool_example
cargo run -p mcp-sdk --example prompt_example
cargo run -p mcp-sdk --example resource_example
cargo run -p mcp-sdk --example sampling_example
```

Start the server example first and run a client example in a second terminal. The full table
with the `--port`, `--stateless` and `--isJsonResponseDisable` variants is in the crate README.

## Errors

Every fallible call returns `Result<_, McpError>`. `McpError` carries the JSON-RPC error code
where one applies (`error_codes::METHOD_NOT_FOUND` and friends from `ap-jsonrpc`), a message,
and optional data. Constructors such as `McpError::state`, `McpError::argument` and
`McpError::rpc` name the failure class, and `to_rpc_error` converts one back to the wire form.
