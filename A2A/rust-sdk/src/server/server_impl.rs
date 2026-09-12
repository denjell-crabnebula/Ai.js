// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server implementation, the port of `include/server/server.h` and `server_impl.*`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ap_jsonrpc::{RequestId, Response};
use async_trait::async_trait;
use serde_json::Value;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aServerError};
use crate::log::A2aLogLevel;
use crate::protocol::{
    JSON_FIELD_METHOD, JSONRPC_TRANSPORT, METHOD_AGENT_CARD_GET, METHOD_MESSAGE_SEND, METHOD_MESSAGE_STREAM,
    METHOD_TASK_CANCEL, METHOD_TASK_GET, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET, METHOD_TASK_RESUBSCRIBE,
};
use crate::types::{AgentCard, SendMessageResult, StreamEvent, success_response};
use crate::utils::{is_final_event, make_error};

use super::builder::HttpConfig;
use super::emitter::TransportEmitter;
use super::executor::AgentExecutor;
use super::http_transport::{HttpServerTransport, ServerTransport};
use super::jsonrpc_handler::{JsonRpcHandler, request_id_of};
use super::request_handler::{DefaultRequestHandler, RequestHandler, StreamEmitter};
use super::task_store::TaskStore;

/// A running A2A server.
#[async_trait]
pub trait Server: Send + Sync {
    /// Start serving. Calling it twice is a no-op.
    async fn start(&self) -> Result<(), A2aServerError>;
    /// Stop serving.
    async fn stop(&self);
    /// Address the server listens on once started.
    fn local_addr(&self) -> Option<SocketAddr> {
        None
    }
}

/// Server configuration alias.
pub type ServerConfig = HttpConfig;

fn is_supported_streaming_method(method: &str) -> bool {
    method == METHOD_MESSAGE_STREAM || method == METHOD_TASK_RESUBSCRIBE
}

fn build_agent_card_transport(
    agent_card: &AgentCard,
    config: &HttpConfig,
) -> Option<Arc<dyn ServerTransport>> {
    let first = agent_card.supported_interfaces.first()?;
    if first.protocol_binding == JSONRPC_TRANSPORT {
        return Some(Arc::new(HttpServerTransport::new(config.clone())));
    }
    None
}

struct Inner {
    agent_card: Arc<AgentCard>,
    extended_agent_card: Arc<AgentCard>,
    jsonrpc_handler: Arc<JsonRpcHandler>,
    transport: Option<Arc<dyn ServerTransport>>,
    started: AtomicBool,
}

/// Default [`Server`] wiring a transport, a JSON-RPC handler and a request handler.
#[derive(Clone)]
pub struct ServerImpl {
    inner: Arc<Inner>,
}

impl ServerImpl {
    /// Create a server with the HTTP transport selected from the agent card.
    pub fn new(
        agent_card: Arc<AgentCard>,
        extended_agent_card: Arc<AgentCard>,
        agent_executor: Option<Arc<dyn AgentExecutor>>,
        config: HttpConfig,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Self {
        let transport = build_agent_card_transport(&agent_card, &config);
        Self::build(
            agent_card,
            extended_agent_card,
            agent_executor,
            transport,
            task_store,
        )
    }

    /// Create a server with a custom transport.
    pub fn with_transport(
        agent_card: Arc<AgentCard>,
        extended_agent_card: Arc<AgentCard>,
        agent_executor: Option<Arc<dyn AgentExecutor>>,
        transport: Arc<dyn ServerTransport>,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Self {
        Self::build(
            agent_card,
            extended_agent_card,
            agent_executor,
            Some(transport),
            task_store,
        )
    }

    fn build(
        agent_card: Arc<AgentCard>,
        extended_agent_card: Arc<AgentCard>,
        agent_executor: Option<Arc<dyn AgentExecutor>>,
        transport: Option<Arc<dyn ServerTransport>>,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Self {
        let handler: Arc<dyn RequestHandler> = Arc::new(DefaultRequestHandler::new(
            agent_executor,
            agent_card.clone(),
            task_store,
        ));
        let jsonrpc_handler = Arc::new(JsonRpcHandler::new(handler));
        ServerImpl {
            inner: Arc::new(Inner {
                agent_card,
                extended_agent_card,
                jsonrpc_handler,
                transport,
                started: AtomicBool::new(false),
            }),
        }
    }

    /// Whether `start` succeeded and `stop` was not called.
    pub fn is_started(&self) -> bool {
        self.inner.started.load(Ordering::SeqCst)
    }

    /// The public agent card.
    pub fn on_get_card(&self) -> AgentCard {
        (*self.inner.agent_card).clone()
    }

    /// The authenticated extended agent card.
    pub fn on_get_authenticated_extended_card(&self) -> AgentCard {
        (*self.inner.extended_agent_card).clone()
    }

    /// Handle one JSON-RPC request body. Returns the response body for
    /// non-streaming requests answered directly.
    pub async fn handle_request(
        &self,
        req_body: String,
        emitter: Arc<dyn TransportEmitter>,
    ) -> Option<String> {
        self.inner.handle_request(req_body, emitter).await
    }
}

impl Inner {
    async fn handle_request(&self, req_body: String, emitter: Arc<dyn TransportEmitter>) -> Option<String> {
        if !self.started.load(Ordering::SeqCst) {
            a2a_log!(A2aLogLevel::Warn, "Server stopped, will not process request");
            return None;
        }
        let req: Value = match serde_json::from_str(&req_body) {
            Ok(v) => v,
            Err(e) => {
                a2a_log!(A2aLogLevel::Error, "Parse request failed: {e}");
                let err = make_error(
                    None,
                    A2AErrorCode::JsonrpcInternalError.code(),
                    &format!("Internal error: {e}"),
                );
                return Some(err.to_json().unwrap_or_default());
            }
        };
        let method = req
            .get(JSON_FIELD_METHOD)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if is_supported_streaming_method(&method) {
            self.handle_streaming_request(&req, &method, emitter).await;
            None
        } else {
            self.handle_non_streaming_request(&req, &req_body, &method, emitter)
                .await
        }
    }

    async fn handle_streaming_request(&self, req: &Value, method: &str, emitter: Arc<dyn TransportEmitter>) {
        let id = request_id_of(req);
        let stream_emit = Self::create_stream_emitter(id.clone(), emitter.clone());
        let result = if !self.agent_card.capabilities.streaming.unwrap_or(false) {
            Err(A2aServerError::from_code(
                "Streaming is not supported by the agent",
                A2AErrorCode::UnsupportedOperation,
            ))
        } else if method == METHOD_MESSAGE_STREAM {
            self.jsonrpc_handler
                .on_message_send_streaming(req, stream_emit)
                .await
        } else if method == METHOD_TASK_RESUBSCRIBE {
            self.jsonrpc_handler
                .on_resubscribe_to_task(req, stream_emit)
                .await
        } else {
            Err(A2aServerError::from_code(
                format!("Unsupported streaming method: {method}"),
                A2AErrorCode::JsonrpcMethodNotFound,
            ))
        };
        if let Err(e) = result {
            let err = make_error(id, e.status_code(), &e.message());
            emitter.write_streaming_data(&err.to_json().unwrap_or_default());
            emitter.write_done();
        }
    }

    fn create_stream_emitter(id: Option<RequestId>, emitter: Arc<dyn TransportEmitter>) -> StreamEmitter {
        Arc::new(move |event: &StreamEvent| {
            match success_response(id.clone(), event) {
                Ok(resp) => emitter.write_streaming_data(&resp.to_json().unwrap_or_default()),
                Err(e) => a2a_log!(A2aLogLevel::Error, "Failed to serialize stream event: {e}"),
            }
            if is_final_event(event) {
                emitter.write_done();
            }
        })
    }

    fn create_non_stream_emitter(id: Option<RequestId>, emitter: Arc<dyn TransportEmitter>) -> StreamEmitter {
        Arc::new(move |event: &StreamEvent| {
            let result = match event {
                StreamEvent::Task(t) => SendMessageResult::Task(Box::new(t.clone())),
                StreamEvent::Message(m) => SendMessageResult::Message(Box::new(m.clone())),
                _ => {
                    a2a_log!(
                        A2aLogLevel::Error,
                        "Unexpected event type for non-streaming response"
                    );
                    return;
                }
            };
            match success_response(id.clone(), &result) {
                Ok(resp) => emitter.write_non_streaming_data(&resp.to_json().unwrap_or_default()),
                Err(e) => a2a_log!(A2aLogLevel::Error, "Failed to serialize response: {e}"),
            }
        })
    }

    async fn handle_non_streaming_request(
        &self,
        req: &Value,
        req_body: &str,
        method: &str,
        emitter: Arc<dyn TransportEmitter>,
    ) -> Option<String> {
        if req_body == "{}" {
            let card_req = serde_json::json!({
                "jsonrpc": "2.0",
                "method": METHOD_AGENT_CARD_GET,
                "id": 1
            });
            let resp = self.jsonrpc_handler.on_get_agent_card(&card_req);
            return Some(resp.to_json().unwrap_or_default());
        }
        self.process_standard_json_rpc(req, method, emitter).await
    }

    async fn process_standard_json_rpc(
        &self,
        req: &Value,
        method: &str,
        emitter: Arc<dyn TransportEmitter>,
    ) -> Option<String> {
        let h = &self.jsonrpc_handler;
        let resp: Response = match method {
            METHOD_AGENT_CARD_GET => h.on_get_agent_card(req),
            METHOD_MESSAGE_SEND => {
                let emit = Self::create_non_stream_emitter(request_id_of(req), emitter);
                return h
                    .on_message_send(req, emit, method)
                    .await
                    .map(|r| r.to_json().unwrap_or_default());
            }
            METHOD_TASK_GET => h.on_get_task(req).await,
            METHOD_TASK_CANCEL => h.on_cancel_task(req).await,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET => h.on_set_push_notification_config(req).await,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET => h.on_get_push_notification_config(req).await,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST => h.on_list_push_notification_config(req).await,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE => h.on_delete_push_notification_config(req).await,
            _ => {
                a2a_log!(A2aLogLevel::Error, "Method not found");
                make_error(
                    request_id_of(req),
                    A2AErrorCode::JsonrpcMethodNotFound.code(),
                    &format!("Method not found: {method}"),
                )
            }
        };
        Some(resp.to_json().unwrap_or_default())
    }
}

#[async_trait]
impl Server for ServerImpl {
    async fn start(&self) -> Result<(), A2aServerError> {
        if self.inner.started.load(Ordering::SeqCst) {
            a2a_log!(
                A2aLogLevel::Warn,
                "Server already started, ignoring duplicate Start() call"
            );
            return Ok(());
        }
        let Some(transport) = &self.inner.transport else {
            a2a_log!(
                A2aLogLevel::Error,
                "Start server failed, server transport is null"
            );
            return Err(A2aServerError::new(
                "Start server failed, server transport is null",
            ));
        };
        let inner = self.inner.clone();
        transport.set_rpc_handler(Arc::new(move |req_body, emitter| {
            let inner = inner.clone();
            Box::pin(async move { inner.handle_request(req_body, emitter).await })
        }));
        let card = self.inner.agent_card.clone();
        transport.set_card_handler(Arc::new(move || {
            serde_json::to_string(&*card).unwrap_or_default()
        }));
        let extended = self.inner.extended_agent_card.clone();
        transport.set_extended_card_handler(Arc::new(move || {
            serde_json::to_string(&*extended).unwrap_or_default()
        }));
        // Mark started before the transport accepts connections.
        self.inner.started.store(true, Ordering::SeqCst);
        if let Err(e) = transport.start().await {
            self.inner.started.store(false, Ordering::SeqCst);
            return Err(e);
        }
        Ok(())
    }

    async fn stop(&self) {
        if let Some(transport) = &self.inner.transport {
            transport.stop().await;
        }
        self.inner.started.store(false, Ordering::SeqCst);
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        self.inner.transport.as_ref().and_then(|t| t.local_addr())
    }
}
