# A2A SDK

Crate `a2a-sdk` (`A2A/rust-sdk`) implements the Agent-to-Agent protocol v1.0 over HTTP
JSON-RPC with Server-Sent Events for streaming. It ports the C++ SDK and interoperates with it.

## Server

An agent implements `AgentExecutor`; the HTTP server exposes it under the JSON-RPC endpoint
and serves its agent card.

```rust,no_run
use std::sync::Arc;
use a2a_sdk::server::{AgentExecutor, HttpConfig, HttpServerBuilder, RequestContext, TaskUpdater};
use a2a_sdk::types::{AgentCapabilities, AgentCard, AgentInterface, Part};
use a2a_sdk::A2aServerError;
use async_trait::async_trait;

struct Echo;

#[async_trait]
impl AgentExecutor for Echo {
    async fn execute(&self, ctx: Arc<RequestContext>, updater: Arc<dyn TaskUpdater>) -> Result<(), A2aServerError> {
        updater.start_work(None);
        let reply = updater.new_agent_message(vec![Part::text(ctx.get_user_input("\n"))], None);
        updater.send_response_message(&reply);
        Ok(())
    }
    async fn cancel(&self, _ctx: Arc<RequestContext>, updater: Arc<dyn TaskUpdater>) -> Result<(), A2aServerError> {
        updater.cancel(None);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let card = AgentCard {
        name: "Echo".into(),
        version: "1.0.0".into(),
        capabilities: AgentCapabilities { streaming: Some(true), ..Default::default() },
        supported_interfaces: vec![AgentInterface::jsonrpc("http://127.0.0.1:8080/jsonrpc")],
        ..Default::default()
    };
    let server = HttpServerBuilder::build(
        &HttpConfig::new("127.0.0.1", 8080), &card, &AgentCard::default(), Arc::new(Echo), None,
    )?;
    server.start().await?;
    tokio::signal::ctrl_c().await?;
    server.stop().await;
    Ok(())
}
```

Routes served:

| Route | Purpose |
|-------|---------|
| `POST <endpoint>` | JSON-RPC methods (`message/send`, `message/stream`, task methods, push notification config) |
| `GET /.well-known/agent-card.json` | Public agent card |
| `GET /agent/authenticatedExtendedCard` | Extended card, when one is configured |
| `GET <endpoint>` | Public card |

The endpoint path is taken from the first interface URL and defaults to `/jsonrpc`. Unknown
paths answer `404 Endpoint not found`.

`TaskUpdater` is how an executor reports progress: `start_work`, `send_response_message`,
`add_artifact`, `complete`, `fail`, `cancel`, `require_input`. Task state and history are
persisted in the `TaskStore` (in-memory by default) and streamed to clients subscribed to the
task.

## Client

```rust,no_run
use std::collections::BTreeMap;
use a2a_sdk::client::{ClientConfig, ClientEvent, ClientFactory, HttpCardResolverBuilder};
use a2a_sdk::types::{Message, Part, Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resolver = HttpCardResolverBuilder::build(
        "http://127.0.0.1:8080", "/.well-known/agent-card.json", &BTreeMap::new(),
    ).ok_or("invalid resolver arguments")?;
    let card = resolver.get_agent_card(None).await?;
    let client = ClientFactory::create(&card, &ClientConfig::default(), Vec::new(), Vec::new())
        .ok_or("no matching transport")?;
    let msg = Message { message_id: "1".into(), role: Role::User, parts: vec![Part::text("hello")], ..Default::default() };
    client.send_message(&msg, None, Box::new(|ev: &ClientEvent, _card| println!("{ev:?}")), 0).await?;
    client.close();
    Ok(())
}
```

`send_message` calls the handler once for `message/send` and once per event for
`message/stream`. `client::send_message_stream` returns the same events as a `futures::Stream`.
Streaming is used when both `ClientConfig::streaming` and the card's `capabilities.streaming`
are true. Timeouts are in seconds; 0 means the default of 60 s per request and 30 s to connect.

Request and event middleware (`add_request_middleware`, `add_event_consumer`) intercept
outgoing requests and incoming events, for example to add headers or record traffic.

## Examples

```bash
cargo run -p a2a-sdk --example helloworld_server -- -i 127.0.0.1 -p 8080
cargo run -p a2a-sdk --example helloworld_client -- -i 127.0.0.1 -p 8080
cargo run -p a2a-sdk --example streaming_server  -- -i 127.0.0.1 -p 8090
cargo run -p a2a-sdk --example streaming_client  -- -i 127.0.0.1 -p 8090
```

## Errors

Servers return `A2aServerError` and clients `A2aClientError`; both carry the A2A error code
(`A2AErrorCode`) that maps to the JSON-RPC error on the wire, and `A2aClientError::try_parse`
recovers a typed error from a server's error text. Serialization errors are `JsonResult` values
from the `types` module.
