# a2a-sdk

Rust port of `A2A/cpp-sdk` from the openJiuwen-ai `agent-protocol` monorepo: an
Agent-to-Agent (A2A) protocol v1.0 client and server speaking JSON-RPC 2.0 over
HTTP, with Server-Sent Events for streaming. The wire format (field names, enum
strings, method names, error codes and messages, HTTP paths) matches the C++ SDK,
so a Rust client talks to the C++ server and vice versa.

The JSON-RPC envelope and the SSE parser come from the shared `ap-jsonrpc` crate.

## Quick start: server

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
    let server = HttpServerBuilder::build(&HttpConfig::new("127.0.0.1", 8080), &card, &AgentCard::default(), Arc::new(Echo), None)?;
    server.start().await?;
    tokio::signal::ctrl_c().await?;
    server.stop().await;
    Ok(())
}
```

The server serves `POST <endpoint>` (the path of the first interface URL, default
`/jsonrpc`), `GET /.well-known/agent-card.json`, `GET /agent/authenticatedExtendedCard`
and `GET <endpoint>` (the public card). Unknown paths answer `404 Endpoint not found`.

## Quick start: client

```rust,no_run
use std::collections::BTreeMap;
use a2a_sdk::client::{ClientConfig, ClientEvent, ClientFactory, HttpCardResolverBuilder};
use a2a_sdk::types::{Message, Part, Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resolver = HttpCardResolverBuilder::build("http://127.0.0.1:8080", "/.well-known/agent-card.json", &BTreeMap::new())
        .ok_or("invalid resolver arguments")?;
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
`message/stream`. `client::send_message_stream` returns the same events as a
`futures::Stream`. Streaming is used when `ClientConfig::streaming` and the card's
`capabilities.streaming` are both true. Timeouts are in seconds; 0 means the default
(60 s per request, 30 s to connect).

## Examples

```sh
cargo run -p a2a-sdk --example helloworld_server -- -i 127.0.0.1 -p 8080
cargo run -p a2a-sdk --example helloworld_client -- -i 127.0.0.1 -p 8080
cargo run -p a2a-sdk --example streaming_server  -- -i 127.0.0.1 -p 8090
cargo run -p a2a-sdk --example streaming_client  -- -i 127.0.0.1 -p 8090
```

## API mapping

| C++ symbol | Rust symbol |
|---|---|
| `A2A::Role`, `TaskState`, `Part`, `Artifact`, `Message`, `TaskStatus`, `Task` | `types::Role`, `TaskState`, `Part`, `Artifact`, `Message`, `TaskStatus`, `Task` |
| `PushNotificationConfig`, `PushNotificationAuthenticationInfo`, `TaskPushNotificationConfig` | same names in `types` |
| `MessageSendConfiguration`, `MessageSendParams`, `TaskIdParams`, `TaskQueryParams`, `*PushNotificationConfigParams` | same names in `types` |
| `TaskStatusUpdateEvent`, `TaskArtifactUpdateEvent` | same names in `types` |
| `AgentCard`, `AgentSkill`, `AgentCapabilities`, `AgentExtension`, `AgentProvider`, `AgentInterface`, `SecurityRequirement`, `AgentCardSignature` | same names in `types` |
| `SecurityScheme` variant + `APIKeySecurityScheme` etc. | `types::SecurityScheme` enum (tagged by `type`) + same struct names |
| `A2AError` and `InvalidRequestError` .. `VersionNotSupportedError` | `types::A2AError` and `A2AError::invalid_request()` .. `A2AError::version_not_supported()` |
| `SendMessageSuccessResponse` / `SendStreamingMessageSuccessResponse` | `ap_jsonrpc::Response` with `types::SendMessageResult` / `types::StreamEvent` result |
| `A2AErrorCode` | `error::A2AErrorCode` (`.code()` gives the numeric value) |
| `A2AClientException`, `A2AClientHTTPError`, `A2AClientJSONError`, `A2AClientTimeoutError` | `error::A2aClientError::{Other, Http, Json, Timeout}`, `A2aClientError::make` |
| `A2AClientException::TryParse` | `A2aClientError::try_parse` |
| `A2AServerError`, `MethodNotImplementedError` | `error::A2aServerError::{Server, MethodNotImplemented}` |
| `common_types.h` method names, `DEFAULT_PROTOCOL_VERSION` | `protocol::*` |
| `A2A_LOG`, `SetLogLevel`, `SetLogCallback`, `GetLogLevelName` | `a2a_log!`, `log::set_log_level`, `log::set_log_callback`, `log::get_log_level_name` |
| `GenerateUuid`, `IDGenerator`, `UUIDGenerator` | `utils::generate_uuid`, `utils::IdGenerator`, `utils::UuidGenerator` |
| `NewAgentTextMessage`, `NewAgentPartsMessage`, `GetTextParts`, `GetMessageText` | `utils::new_agent_text_message`, ... |
| `NewArtifact`, `NewTextArtifact`, `NewDataArtifact` | `utils::new_artifact`, `utils::new_text_artifact`, `utils::new_data_artifact` |
| `utils_helpers.h` (`IsFinal`, `IsFinalEvent`, `AppendArtifactToTask`, `MakeError`, ...) | `utils::is_final`, `utils::is_final_event`, `utils::append_artifact_to_task`, `utils::make_error`, ... |
| `Client::ClientConfig`, `ClientEvent`, `UpdateEvent`, `Consumer`, `ResponseHandler` | `client::ClientConfig`, `ClientEvent`, `UpdateEvent`, `Consumer`, `ResponseHandler` |
| `Client::Client` (`SendMessage`, `GetTask`, `CancelTask`, `Resubscribe`, `GetCard`, ...) | `client::Client` trait (`send_message`, `get_task`, `cancel_task`, `resubscribe`, `get_card`, ...) |
| `DefaultClient` | `client::DefaultClient` |
| `ClientFactory::Create(card, config, consumers, interceptors)` | `client::ClientFactory::create(card, config, consumers, interceptors)` |
| `ClientFactory::Create(card, config, transport, consumers)` | `ClientFactory::create_with_transport` |
| `ClientTransport`, `TransportEvent`, `TransportError`, `TransportEventCallback` | `client::ClientTransport`, `TransportEvent`, `TransportError`, `TransportEventCallback` |
| `JsonRpcTransport` (libcurl) | `client::JsonRpcTransport` (reqwest) |
| `ClientCallInterceptor`, `ProtocolVersionInterceptor` | `client::ClientCallInterceptor`, `client::ProtocolVersionInterceptor` |
| `ClientTaskManager` | `client::ClientTaskManager` |
| `A2ACardResolver`, `HttpCardResolver`, `HttpCardResolverBuilder::Build` | `client::A2ACardResolver`, `HttpCardResolver`, `HttpCardResolverBuilder::build` |
| `Server::Server` (`Start`, `Stop`) | `server::Server` trait (`start`, `stop`, `local_addr`) |
| `HttpConfig`, `HttpServerBuilder::Build` | `server::HttpConfig`, `server::HttpServerBuilder::build` |
| `ServerImpl` | `server::ServerImpl` |
| `AgentExecutor` | `server::AgentExecutor` (async) |
| `RequestContext`, `RequestContextParam`, `ServerCallContext` | `server::RequestContext`, `RequestContextParam`, `ServerCallContext` |
| `TaskStore`, `InMemoryTaskStore` | `server::TaskStore`, `server::InMemoryTaskStore` |
| `TaskUpdater`, `TaskArtifactParam`, `TaskUpdaterImpl` | `server::TaskUpdater`, `TaskArtifactParam`, `TaskUpdaterImpl` |
| `TaskManager`, `TaskExecuteInfo`, `EventCb` | `server::TaskManager`, `TaskExecuteInfo`, `EventCb` |
| `PushNotificationConfigStore`, `InMemoryPushNotificationConfigStore`, `PushNotificationSender` | same names in `server` |
| `RequestHandler`, `DefaultRequestHandler`, `StreamEmitter` | `server::RequestHandler`, `DefaultRequestHandler`, `StreamEmitter` |
| `JSONRPCHandler` | `server::JsonRpcHandler` |
| `Transport::ServerTransport`, `HttpServerTransport` | `server::ServerTransport`, `server::HttpServerTransport` (axum) |
| `Transport::TransportEmitter`, `StreamServerEmitter` | `server::TransportEmitter`, `server::StreamServerEmitter` |

## Behaviour notes and deviations

- `std::future<T>` plus exceptions became `async fn -> Result<T, A2aClientError>`.
  `Client::resubscribe` completes when the stream ends instead of returning at once.
- Metadata fields, `Part::data`, `AgentExtension::params` and
  `AgentCardSignature::header` are `serde_json::Value` instead of JSON text. The wire
  format is unchanged. A `Part::data` string is sent as a string; the C++ SDK tries
  to parse data strings as JSON first.
- `TaskStore::get` returns an owned `Task`. Places where the C++ SDK relied on
  mutating a shared pointer (cancel, message completion) save explicitly.
- A stream that ends without a final status completes `send_message` with `Ok(())`.
  The C++ SDK reports an error with code 0 in that case.
- Connection failures map to `A2A_TRANSPORT_EXCEPTION` (-32102) and timeouts to
  `A2A_REQUEST_TIMEOUT` (-32101). The C++ SDK reports both as `HTTP_PARSE_ERROR` (-1).
- The request timeout (seconds, 0 = 60 s) is honoured per request. For streams it is
  an idle timeout between SSE chunks; the C++ SDK applies a fixed 60 s total limit.
- Responses whose result fails to parse produce `A2A_INVALID_FORMAT` (-32106);
  the C++ SDK logs and leaves the request pending.
- `HttpCardResolver` sends `http_kwargs` as extra request headers; the C++ SDK
  stores them without using them. `get_agent_card(path)` ignores the path argument
  like the original and uses the path given to the builder.
- `GET <endpoint>` returns the agent card so that `Client::get_card` works against
  this server. `/agent/authenticatedExtendedCard` serves the extended card;
  `ServerImpl::on_get_authenticated_extended_card` returns the extended card, where
  the C++ placeholder returned the public card.
- `HttpConfig::io_thread_num` is informational; tokio schedules the work.
- `Server::start` returns `Result` instead of an `int`. Builder failures are
  `A2aServerError` values instead of `std::runtime_error`.
- After the executor's `cancel` returns, the task is re-read from the store before
  the manager marks it canceled, so the updater's cancel timestamp is kept.

## Intentionally left out

- The raw socket layer (`src/server/net/*`, `http_server.cpp`, `http_server_manager`),
  libevent `event_system`, `a2a_timer`, the lock-free and MPSC queues and
  `thread_utils`. tokio and axum replace them.
- TLS configuration on the server; use a reverse proxy or `axum-server` if needed.
- The bearer token and timeout setters on `HttpServerTransport` are kept for API
  parity but are not enforced, as in the original.
- A default `PushNotificationSender`; the C++ SDK never installs one either. Install
  one with `DefaultRequestHandler::with_stores`.
- Tests that only exercise sockets, libevent, libcurl internals or the C++ queues.
