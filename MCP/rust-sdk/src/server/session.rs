// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server session, port of `src/server/server_session.*`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ap_jsonrpc::{Message, Notification, Request, RequestId, Response, RpcError};
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};

use crate::error::{JsonRpcErrorCode, McpError};
use crate::protocol::{
    CancelledNotificationParams, ElicitParams, InitializeRequestParams, LATEST_PROTOCOL_VERSION,
    LoggingMessageNotificationParams, ProgressNotificationParams, is_supported_protocol_version, methods,
};
use crate::sampling_validation::validate_tool_use_result_messages;
use crate::session::{PendingRequests, await_response};
use crate::transport::{RequestContext, ServerTransport, TransportCallback};
use crate::types::{
    ClientCapabilities, CreateMessageParams, CreateMessageResult, DEFAULT_TIMEOUT, ElicitResult,
    Implementation, InitializeResult, ListRootsResult, MetaMap, ProgressToken, ServerCapabilities,
    ServerConfig,
};

/// Receives the requests and notifications a session does not handle itself.
#[async_trait]
pub trait IncomingRequestHandler: Send + Sync {
    /// Handle a request. The handler must answer through the session.
    async fn handle_request(
        &self,
        session: Arc<ServerSession>,
        id: RequestId,
        request: Request,
        ctx: RequestContext,
    );

    /// Handle a notification.
    async fn handle_notification(&self, _session: Arc<ServerSession>, _notification: Notification) {}
}

/// Server side MCP session (`ServerSession` and `McpServerSession`).
pub struct ServerSession {
    transport: Option<Arc<dyn ServerTransport>>,
    pending: PendingRequests,
    session_id: String,
    server_config: ServerConfig,
    capabilities: Mutex<ServerCapabilities>,
    client_capabilities: Mutex<Option<ClientCapabilities>>,
    is_initialized: AtomicBool,
    stateless: bool,
    handler: RwLock<Option<Arc<dyn IncomingRequestHandler>>>,
    session_requests: Mutex<HashMap<RequestId, RequestContext>>,
    request_timeout: Duration,
    weak_self: Weak<ServerSession>,
}

impl ServerSession {
    /// Create a session and install it as the transport callback.
    pub fn new(
        transport: Option<Arc<dyn ServerTransport>>,
        server_config: ServerConfig,
        session_id: impl Into<String>,
        stateless: bool,
    ) -> Arc<Self> {
        let session = Arc::new_cyclic(|weak| Self {
            transport,
            pending: PendingRequests::new(),
            session_id: session_id.into(),
            server_config,
            capabilities: Mutex::new(ServerCapabilities::default()),
            client_capabilities: Mutex::new(None),
            is_initialized: AtomicBool::new(false),
            stateless,
            handler: RwLock::new(None),
            session_requests: Mutex::new(HashMap::new()),
            request_timeout: Duration::from_millis(DEFAULT_TIMEOUT),
            weak_self: weak.clone(),
        });
        if let Some(transport) = &session.transport {
            let weak: Weak<dyn TransportCallback> = Arc::downgrade(&session) as Weak<dyn TransportCallback>;
            transport.set_callback(Some(weak));
        }
        session
    }

    /// A session without a transport, for handlers that only need a context.
    pub fn detached() -> Arc<Self> {
        Self::new(None, ServerConfig::default(), "", false)
    }

    /// The session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// True when the session runs in stateless mode.
    pub fn is_stateless(&self) -> bool {
        self.stateless
    }

    /// True after the `initialize` handshake.
    pub fn is_initialized(&self) -> bool {
        self.is_initialized.load(Ordering::SeqCst)
    }

    /// The transport, if any.
    pub fn transport(&self) -> Option<Arc<dyn ServerTransport>> {
        self.transport.clone()
    }

    /// Set the capabilities returned by `initialize`. Resource subscription is
    /// forced to `false` because the server does not implement it.
    pub fn set_server_capabilities(&self, capabilities: ServerCapabilities) {
        let mut caps = capabilities;
        if let Some(resources) = caps.resources.as_mut() {
            resources.subscribe = Some(false);
        }
        *self.capabilities.lock() = caps;
    }

    /// Install the handler for non initialization messages.
    pub fn set_incoming_request_handler(&self, handler: Arc<dyn IncomingRequestHandler>) {
        *self.handler.write() = Some(handler);
    }

    /// Capabilities advertised by the client, empty before `initialize`.
    pub fn get_client_capabilities(&self) -> ClientCapabilities {
        self.client_capabilities.lock().clone().unwrap_or_default()
    }

    /// Number of outstanding server initiated requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn arc(&self) -> Option<Arc<ServerSession>> {
        self.weak_self.upgrade()
    }

    async fn transport_send(&self, message: Message, ctx: &RequestContext) -> Result<(), McpError> {
        match &self.transport {
            Some(t) => t.send_message(message, ctx).await,
            None => Err(McpError::state("Transport not set")),
        }
    }

    /// Send a request to the client and wait for the typed result.
    pub async fn send_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        op_name: &str,
    ) -> Result<T, McpError> {
        if self.transport.is_none() {
            return Err(McpError::state("Transport not set"));
        }
        let (id, rx) = self.pending.register(None);
        let request = Request::new(id.clone(), method, params);
        let ctx = RequestContext::server_initiated(&self.session_id, method);
        if let Err(e) = self.transport_send(Message::Request(request), &ctx).await {
            self.pending.remove(&id);
            return Err(e);
        }
        let value = await_response(&self.pending, &id, rx, self.request_timeout)
            .await
            .map_err(|e| match e {
                McpError::Rpc(err) => McpError::state(format!("{op_name} failed: {}", err.message)),
                other => other,
            })?;
        if value.is_null() {
            return Err(McpError::state(format!("{op_name} failed: null result")));
        }
        serde_json::from_value(value)
            .map_err(|e| McpError::state(format!("{op_name} failed: result type mismatch ({e})")))
    }

    /// Send a successful response for `id`. Suppressed when the request was cancelled.
    pub async fn send_response(
        &self,
        id: RequestId,
        result: Value,
        ctx: &RequestContext,
    ) -> Result<(), McpError> {
        let cancelled = {
            let mut map = self.session_requests.lock();
            map.remove(&id).map(|c| c.is_cancelled).unwrap_or(false)
        };
        if cancelled || ctx.is_cancelled {
            return Ok(());
        }
        self.transport_send(Message::Response(Response::success(id, result)), ctx)
            .await
    }

    /// Send an error response for `id`.
    pub async fn send_error_response(
        &self,
        id: RequestId,
        error: RpcError,
        ctx: &RequestContext,
    ) -> Result<(), McpError> {
        self.session_requests.lock().remove(&id);
        self.transport_send(Message::Response(Response::error(Some(id), error)), ctx)
            .await
    }

    /// Send a notification to the client.
    pub async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        if self.transport.is_none() {
            return Ok(());
        }
        let mut ctx = RequestContext::server_initiated(&self.session_id, method);
        ctx.is_get_stream = true;
        self.transport_send(Message::Notification(Notification::new(method, params)), &ctx)
            .await
    }

    /// `notifications/tools/list_changed`
    pub async fn send_tool_list_changed_notification(&self) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_TOOLS_LIST_CHANGED, Some(json!({})))
            .await
    }

    /// `notifications/prompts/list_changed`
    pub async fn send_prompt_list_changed_notification(&self) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_PROMPTS_LIST_CHANGED, Some(json!({})))
            .await
    }

    /// `notifications/resources/list_changed`
    pub async fn send_resource_list_changed_notification(&self) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_RESOURCES_LIST_CHANGED, Some(json!({})))
            .await
    }

    /// `notifications/resources/updated`
    pub async fn send_resource_updated_notification(&self, uri: &str) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_RESOURCES_UPDATED, Some(json!({"uri": uri})))
            .await
    }

    /// `notifications/message`
    pub async fn send_log_message(&self, level: &str, data: Value, logger: &str) -> Result<(), McpError> {
        let params = LoggingMessageNotificationParams {
            level: level.to_string(),
            logger: logger.to_string(),
            data,
        };
        self.send_notification(methods::NOTIFICATION_MESSAGE, Some(serde_json::to_value(params)?))
            .await
    }

    /// `notifications/progress`
    pub async fn send_progress_notification(
        &self,
        progress_token: ProgressToken,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Result<(), McpError> {
        let params = ProgressNotificationParams {
            progress_token,
            progress,
            total,
            message,
        };
        self.send_notification(
            methods::NOTIFICATION_PROGRESS,
            Some(serde_json::to_value(params)?),
        )
        .await
    }

    fn check_can_request(&self, what: &str) -> Result<(), McpError> {
        if self.stateless {
            return Err(McpError::state(format!(
                "{what} is not supported in stateless server mode"
            )));
        }
        if !self.is_initialized() {
            return Err(McpError::state("Session is not initialized"));
        }
        Ok(())
    }

    /// `roots/list`. Requires the client to advertise `roots`.
    pub async fn list_roots(&self) -> Result<ListRootsResult, McpError> {
        self.check_can_request(methods::ROOTS_LIST)?;
        if self.get_client_capabilities().roots.is_none() {
            return Err(McpError::state("Client does not support roots/list"));
        }
        self.send_request(methods::ROOTS_LIST, Some(Value::Null), "ListRoots")
            .await
    }

    /// `sampling/createMessage`. Requires the client to advertise `sampling`.
    pub async fn sampling_create_message(
        &self,
        params: CreateMessageParams,
    ) -> Result<CreateMessageResult, McpError> {
        self.check_can_request(methods::SAMPLING_CREATE_MESSAGE)?;
        let caps = self.get_client_capabilities();
        let Some(sampling) = caps.sampling else {
            return Err(McpError::state("Client does not support sampling/createMessage"));
        };
        if params.tools.is_some() && !sampling.tools {
            return Err(McpError::state(
                "Tool-enabled sampling requested but client did not advertise sampling.tools capability",
            ));
        }
        if let Some(v) = &params.include_context {
            if (v == "thisServer" || v == "allServers") && !sampling.context {
                return Err(McpError::state(
                    "includeContext requires sampling.context capability",
                ));
            }
        }
        validate_tool_use_result_messages(&params.messages).map_err(|e| McpError::state(e.message()))?;
        self.send_request(
            methods::SAMPLING_CREATE_MESSAGE,
            Some(serde_json::to_value(&params)?),
            "SamplingCreateMessage",
        )
        .await
    }

    /// `elicitation/create` in form mode.
    pub async fn elicit(&self, message: &str, requested_schema: MetaMap) -> Result<ElicitResult, McpError> {
        self.check_can_request(methods::ELICITATION_CREATE)?;
        let params = ElicitParams::Form {
            message: message.to_string(),
            requested_schema,
        };
        self.send_request(
            methods::ELICITATION_CREATE,
            Some(serde_json::to_value(params)?),
            "Elicit",
        )
        .await
    }

    /// `elicitation/create` in url mode.
    pub async fn elicit_url(
        &self,
        message: &str,
        url: &str,
        elicitation_id: &str,
    ) -> Result<ElicitResult, McpError> {
        self.check_can_request(methods::ELICITATION_CREATE)?;
        let params = ElicitParams::Url {
            message: message.to_string(),
            url: url.to_string(),
            elicitation_id: elicitation_id.to_string(),
        };
        self.send_request(
            methods::ELICITATION_CREATE,
            Some(serde_json::to_value(params)?),
            "ElicitUrl",
        )
        .await
    }

    async fn handle_initialize(&self, id: RequestId, params: Option<Value>, ctx: &RequestContext) {
        let params: InitializeRequestParams = match params {
            Some(v) if v.is_object() => serde_json::from_value(v).unwrap_or_default(),
            _ => InitializeRequestParams::default(),
        };
        *self.client_capabilities.lock() = Some(params.capabilities.clone());
        let protocol_version = if is_supported_protocol_version(&params.protocol_version) {
            params.protocol_version.clone()
        } else {
            LATEST_PROTOCOL_VERSION.to_string()
        };
        let result = InitializeResult::new(
            protocol_version,
            self.capabilities.lock().clone(),
            Implementation::new(&self.server_config.name, &self.server_config.version),
            None,
        );
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        if let Err(e) = self.send_response(id, value, ctx).await {
            tracing::error!("Failed to send initialize response: {e}");
        }
        self.is_initialized.store(true, Ordering::SeqCst);
    }

    async fn handle_cancelled(&self, params: Option<Value>) {
        let Ok(params) = serde_json::from_value::<CancelledNotificationParams>(params.unwrap_or(Value::Null))
        else {
            return;
        };
        let ctx = {
            let mut map = self.session_requests.lock();
            match map.get_mut(&params.request_id) {
                Some(entry) => {
                    entry.is_cancelled = true;
                    Some(entry.clone())
                }
                None => None,
            }
        };
        if let Some(ctx) = ctx {
            let error = RpcError::new(0, "Request cancelled");
            let message = Message::Response(Response::error(Some(params.request_id.clone()), error));
            if let Err(e) = self.transport_send(message, &ctx).await {
                tracing::debug!("Failed to send cancellation response: {e}");
            }
        }
    }

    async fn received_request(&self, request: Request, mut ctx: RequestContext) {
        ctx.method = request.method.clone();
        if request.method == methods::INITIALIZE {
            self.handle_initialize(request.id, request.params, &ctx).await;
            return;
        }
        if !self.stateless && !self.is_initialized() {
            tracing::error!("Received request before initialization: {}", request.method);
            let error = RpcError::new(
                JsonRpcErrorCode::InvalidRequest.code(),
                format!("Received request before initialization: {}", request.method),
            );
            let _ = self.send_error_response(request.id, error, &ctx).await;
            return;
        }
        let handler = self.handler.read().clone();
        let (Some(handler), Some(me)) = (handler, self.arc()) else {
            return;
        };
        self.session_requests
            .lock()
            .insert(request.id.clone(), ctx.clone());
        handler.handle_request(me, request.id.clone(), request, ctx).await;
    }

    async fn received_notification(&self, notification: Notification) {
        match notification.method.as_str() {
            methods::NOTIFICATION_INITIALIZED => self.is_initialized.store(true, Ordering::SeqCst),
            methods::NOTIFICATION_CANCELLED => self.handle_cancelled(notification.params).await,
            methods::NOTIFICATION_ROOTS_LIST_CHANGED => {
                tracing::info!("Server received notification: {}", notification.method);
            }
            _ => {
                let handler = self.handler.read().clone();
                if let (Some(handler), Some(me)) = (handler, self.arc()) {
                    handler.handle_notification(me, notification).await;
                }
            }
        }
    }

    fn handle_response(&self, response: Response) {
        let Some(id) = response.id.clone() else {
            return;
        };
        let result = match response.error {
            Some(e) => Err(e),
            None => Ok(response.result.unwrap_or(Value::Null)),
        };
        self.pending.complete(&id, result);
    }
}

#[async_trait]
impl TransportCallback for ServerSession {
    async fn on_message_received(&self, message: Message, ctx: RequestContext) {
        match message {
            Message::Request(r) => self.received_request(r, ctx).await,
            Message::Notification(n) => self.received_notification(n).await,
            Message::Response(r) => self.handle_response(r),
        }
    }

    async fn on_disconnected(&self, reason: String) {
        tracing::info!("server transport disconnected: {reason}");
        self.pending.fail_all(RpcError::internal_error(format!(
            "Transport disconnected: {reason}"
        )));
    }
}

impl std::fmt::Debug for ServerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSession")
            .field("session_id", &self.session_id)
            .field("stateless", &self.stateless)
            .field("initialized", &self.is_initialized())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::CallbackHandle;
    use crate::types::{
        RoleType, SamplingCapability, SamplingContent, SamplingMessage, SamplingMessageContentBlock,
        ToolResultContent, ToolUseContent,
    };
    use ap_support::testing::{OptionExt, ResultExt, TestResult};

    struct Capturing {
        sent: Mutex<Vec<(Message, RequestContext)>>,
        callback: RwLock<Option<CallbackHandle>>,
    }

    impl Capturing {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::new(Vec::new()),
                callback: RwLock::new(None),
            })
        }
        fn count(&self) -> usize {
            self.sent.lock().len()
        }
        fn last(&self) -> TestResult<Value> {
            Ok(self.sent.lock().last().map(|(m, _)| m.to_value()).required()?)
        }
        fn clear(&self) {
            self.sent.lock().clear();
        }
        async fn emit(&self, message: Message, ctx: RequestContext) -> TestResult {
            let cb = self.callback.read().as_ref().and_then(Weak::upgrade).required()?;
            cb.on_message_received(message, ctx).await;
            Ok(())
        }
    }

    #[async_trait]
    impl ServerTransport for Capturing {
        async fn listen(&self) -> Result<(), McpError> {
            Ok(())
        }
        async fn terminate(&self) {}
        async fn send_message(&self, message: Message, ctx: &RequestContext) -> Result<(), McpError> {
            self.sent.lock().push((message, ctx.clone()));
            Ok(())
        }
        fn set_callback(&self, callback: Option<CallbackHandle>) {
            *self.callback.write() = callback;
        }
    }

    fn ctx() -> RequestContext {
        RequestContext {
            connection_id: 1,
            session_id: "ut".into(),
            ..Default::default()
        }
    }

    async fn initialize(transport: &Capturing, caps: ClientCapabilities) -> TestResult {
        let params = InitializeRequestParams {
            capabilities: caps,
            client_info: Implementation::new("ut-client", "0.0.0"),
            ..Default::default()
        };
        transport
            .emit(
                Message::Request(Request::new(1, "initialize", Some(serde_json::to_value(params)?))),
                ctx(),
            )
            .await?;
        transport
            .emit(
                Message::Notification(Notification::new("notifications/initialized", None)),
                ctx(),
            )
            .await?;
        transport.clear();
        Ok(())
    }

    fn setup() -> (Arc<Capturing>, Arc<ServerSession>) {
        let transport = Capturing::new();
        let session = ServerSession::new(Some(transport.clone()), ServerConfig::default(), "ut", false);
        (transport, session)
    }

    #[tokio::test]
    async fn initialize_negotiates_version_and_stores_client_caps() -> TestResult {
        let (transport, session) = setup();
        let params = json!({"protocolVersion": "1999-01-01", "capabilities": {"roots": {"listChanged": true}},
            "clientInfo": {"name": "c", "version": "1"}});
        transport
            .emit(
                Message::Request(Request::new(1, "initialize", Some(params))),
                ctx(),
            )
            .await?;
        let v = transport.last()?;
        assert_eq!(v["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert_eq!(v["result"]["serverInfo"]["name"], "MCP Server");
        assert!(session.is_initialized());
        assert!(session.get_client_capabilities().roots.required()?.list_changed);

        let (transport, _session) = setup();
        transport
            .emit(
                Message::Request(Request::new(
                    1,
                    "initialize",
                    Some(json!({"protocolVersion": "2025-03-26"})),
                )),
                ctx(),
            )
            .await?;
        assert_eq!(transport.last()?["result"]["protocolVersion"], "2025-03-26");
        Ok(())
    }

    #[tokio::test]
    async fn capabilities_force_subscribe_false() -> TestResult {
        let (_transport, session) = setup();
        session.set_server_capabilities(ServerCapabilities {
            resources: Some(crate::types::ResourcesCapabilities {
                subscribe: Some(true),
                list_changed: Some(true),
            }),
            ..Default::default()
        });
        assert_eq!(
            session
                .capabilities
                .lock()
                .resources
                .as_ref()
                .required()?
                .subscribe,
            Some(false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn request_before_initialize_is_rejected() -> TestResult {
        let (transport, _session) = setup();
        transport
            .emit(Message::Request(Request::new(5, "tools/list", None)), ctx())
            .await?;
        let v = transport.last()?;
        assert_eq!(v["id"], 5);
        assert_eq!(v["error"]["code"], -32600);
        Ok(())
    }

    #[tokio::test]
    async fn list_roots_round_trip_and_capability_check() -> TestResult {
        let (transport, session) = setup();
        initialize(&transport, ClientCapabilities::default()).await?;
        let err = session.list_roots().await.err_or_fail()?;
        assert_eq!(err.to_string(), "Client does not support roots/list");
        assert_eq!(transport.count(), 0);

        let (transport, session) = setup();
        initialize(
            &transport,
            ClientCapabilities {
                roots: Some(crate::types::RootsCapability { list_changed: false }),
                ..Default::default()
            },
        )
        .await?;
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.list_roots().await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let v = transport.last()?;
        assert_eq!(v["method"], "roots/list");
        assert!(v["params"].is_null());
        let id = v["id"].as_i64().required()?;
        assert!(id > 0);
        let sent_ctx = transport.sent.lock()[0].1.clone();
        assert_eq!(sent_ctx.connection_id, 0);
        transport
            .emit(
                Message::Response(Response::success(
                    RequestId::Number(id),
                    json!({"roots": [{"uri": "file:///tmp", "name": "tmp"}]}),
                )),
                ctx(),
            )
            .await?;
        let roots = task.await??;
        assert_eq!(roots.roots[0].uri, "file:///tmp");
        Ok(())
    }

    fn minimal() -> CreateMessageParams {
        CreateMessageParams {
            messages: vec![SamplingMessage::text(RoleType::User, "hello")],
            max_tokens: 3,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn sampling_preconditions() -> TestResult {
        let detached = ServerSession::detached();
        assert!(detached.sampling_create_message(minimal()).await.is_err());

        let (transport, session) = setup();
        assert_eq!(
            session
                .sampling_create_message(minimal())
                .await
                .err_or_fail()?
                .to_string(),
            "Session is not initialized"
        );
        initialize(&transport, ClientCapabilities::default()).await?;
        assert_eq!(
            session
                .sampling_create_message(minimal())
                .await
                .err_or_fail()?
                .to_string(),
            "Client does not support sampling/createMessage"
        );

        let (transport, session) = setup();
        initialize(
            &transport,
            ClientCapabilities {
                sampling: Some(SamplingCapability {
                    tools: false,
                    context: true,
                }),
                ..Default::default()
            },
        )
        .await?;
        let mut p = minimal();
        p.tools = Some(vec![]);
        assert!(session.sampling_create_message(p).await.is_err());

        let (transport, session) = setup();
        initialize(
            &transport,
            ClientCapabilities {
                sampling: Some(SamplingCapability {
                    tools: true,
                    context: false,
                }),
                ..Default::default()
            },
        )
        .await?;
        let mut p = minimal();
        p.include_context = Some("thisServer".into());
        assert_eq!(
            session
                .sampling_create_message(p)
                .await
                .err_or_fail()?
                .to_string(),
            "includeContext requires sampling.context capability"
        );

        // tool_use / tool_result validation failures.
        let (transport, session) = setup();
        initialize(
            &transport,
            ClientCapabilities {
                sampling: Some(SamplingCapability::default()),
                ..Default::default()
            },
        )
        .await?;
        let tool_result = SamplingMessage {
            role: RoleType::User,
            content: SamplingContent::Single(SamplingMessageContentBlock::ToolResult(ToolResultContent {
                tool_use_id: "id-1".into(),
                ..Default::default()
            })),
            meta: None,
        };
        let only_result = CreateMessageParams {
            messages: vec![tool_result.clone()],
            ..Default::default()
        };
        assert!(session.sampling_create_message(only_result).await.is_err());
        let mismatch = CreateMessageParams {
            messages: vec![
                SamplingMessage {
                    role: RoleType::Assistant,
                    content: SamplingContent::Single(SamplingMessageContentBlock::ToolUse(ToolUseContent {
                        id: "use-1".into(),
                        name: "t".into(),
                        ..Default::default()
                    })),
                    meta: None,
                },
                tool_result,
            ],
            ..Default::default()
        };
        assert!(session.sampling_create_message(mismatch).await.is_err());
        assert_eq!(transport.count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn sampling_round_trip_and_error_paths() -> TestResult {
        let (transport, session) = setup();
        initialize(
            &transport,
            ClientCapabilities {
                sampling: Some(SamplingCapability::default()),
                ..Default::default()
            },
        )
        .await?;
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.sampling_create_message(minimal()).await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let v = transport.last()?;
        assert_eq!(v["method"], "sampling/createMessage");
        assert_eq!(v["params"]["maxTokens"], 3);
        assert!(v["params"]["messages"].is_array());
        let id = v["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::success(
                    RequestId::Number(id),
                    json!({"model": "ut-model", "role": "assistant", "content": {"type": "text", "text": "ok"}}),
                )),
                ctx(),
            )
            .await?;
        let result = task.await??;
        assert_eq!(result.model, "ut-model");
        assert_eq!(result.role, RoleType::Assistant);

        // Error response.
        let s3 = session.clone();
        let task = tokio::spawn(async move { s3.sampling_create_message(minimal()).await });
        for _ in 0..50 {
            if transport.count() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let id = transport.last()?["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::error(
                    Some(RequestId::Number(id)),
                    RpcError::internal_error("err"),
                )),
                ctx(),
            )
            .await?;
        let err = task.await?.err_or_fail()?;
        assert!(err.to_string().contains("SamplingCreateMessage"));

        // Null result and type mismatch.
        let s4 = session.clone();
        let task = tokio::spawn(async move { s4.sampling_create_message(minimal()).await });
        for _ in 0..50 {
            if transport.count() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let id = transport.last()?["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::success(RequestId::Number(id), Value::Null)),
                ctx(),
            )
            .await?;
        assert!(task.await?.is_err());
        let s5 = session.clone();
        let task = tokio::spawn(async move { s5.sampling_create_message(minimal()).await });
        for _ in 0..50 {
            if transport.count() == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let id = transport.last()?["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::success(RequestId::Number(id), json!([1, 2]))),
                ctx(),
            )
            .await?;
        let err = task.await?.err_or_fail()?;
        assert!(err.to_string().contains("result type mismatch"));
        Ok(())
    }

    #[tokio::test]
    async fn progress_and_list_changed_notifications() -> TestResult {
        let (transport, session) = setup();
        session
            .send_progress_notification(
                ProgressToken::from("my-task-123"),
                0.5,
                Some(1.0),
                Some("50% complete".into()),
            )
            .await?;
        let v = transport.last()?;
        assert_eq!(v["method"], "notifications/progress");
        assert_eq!(v["params"]["progressToken"], "my-task-123");
        assert_eq!(v["params"]["progress"], 0.5);
        assert_eq!(v["params"]["total"], 1.0);
        assert_eq!(v["params"]["message"], "50% complete");
        let sent_ctx = transport.sent.lock()[0].1.clone();
        assert!(sent_ctx.is_get_stream);
        assert_eq!(sent_ctx.connection_id, 0);

        session
            .send_progress_notification(ProgressToken::Number(12345), 0.75, None, None)
            .await?;
        let v = transport.last()?;
        assert_eq!(v["params"]["progressToken"], 12345);
        assert!(v["params"].get("total").is_none());
        assert!(v["params"].get("message").is_none());

        session.send_tool_list_changed_notification().await?;
        assert_eq!(transport.last()?["method"], "notifications/tools/list_changed");
        assert!(transport.last()?["params"].is_object());
        session.send_prompt_list_changed_notification().await?;
        assert_eq!(transport.last()?["method"], "notifications/prompts/list_changed");
        session.send_resource_list_changed_notification().await?;
        assert_eq!(
            transport.last()?["method"],
            "notifications/resources/list_changed"
        );
        session.send_resource_updated_notification("u").await?;
        assert_eq!(transport.last()?["params"]["uri"], "u");
        session.send_log_message("info", json!("hello"), "srv").await?;
        let v = transport.last()?;
        assert_eq!(v["method"], "notifications/message");
        assert_eq!(v["params"]["level"], "info");
        assert_eq!(v["params"]["logger"], "srv");

        // No transport: notifications are silently dropped.
        let detached = ServerSession::detached();
        detached
            .send_progress_notification(ProgressToken::from("test"), 0.5, Some(1.0), Some("test".into()))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn elicitation_requests() -> TestResult {
        let (transport, session) = setup();
        initialize(&transport, ClientCapabilities::default()).await?;
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.elicit("name?", MetaMap::new()).await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let v = transport.last()?;
        assert_eq!(v["method"], "elicitation/create");
        assert_eq!(v["params"]["mode"], "form");
        let id = v["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::success(
                    RequestId::Number(id),
                    json!({"action": "accept", "content": {"name": "x"}}),
                )),
                ctx(),
            )
            .await?;
        let r = task.await??;
        assert_eq!(r.action, "accept");

        let s3 = session.clone();
        let task = tokio::spawn(async move { s3.elicit_url("go", "https://x", "e1").await });
        for _ in 0..50 {
            if transport.count() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let v = transport.last()?;
        assert_eq!(v["params"]["mode"], "url");
        assert_eq!(v["params"]["elicitationId"], "e1");
        let id = v["id"].as_i64().required()?;
        transport
            .emit(
                Message::Response(Response::success(
                    RequestId::Number(id),
                    json!({"action": "cancel"}),
                )),
                ctx(),
            )
            .await?;
        assert_eq!(task.await??.action, "cancel");

        let stateless = ServerSession::new(Some(transport.clone()), ServerConfig::default(), "", true);
        assert!(
            stateless
                .elicit("x", MetaMap::new())
                .await
                .err_or_fail()?
                .to_string()
                .contains("stateless")
        );
        Ok(())
    }

    struct EchoHandler {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl IncomingRequestHandler for EchoHandler {
        async fn handle_request(
            &self,
            session: Arc<ServerSession>,
            id: RequestId,
            request: Request,
            ctx: RequestContext,
        ) {
            self.seen.lock().push(request.method.clone());
            // The test inspects the transport output, so a send failure shows up there.
            let _ = session
                .send_response(id, json!({"echo": request.method}), &ctx)
                .await;
        }
        async fn handle_notification(&self, _session: Arc<ServerSession>, notification: Notification) {
            self.seen.lock().push(format!("n:{}", notification.method));
        }
    }

    #[tokio::test]
    async fn dispatch_to_handler_and_cancellation() -> TestResult {
        let (transport, session) = setup();
        let handler = Arc::new(EchoHandler {
            seen: Mutex::new(Vec::new()),
        });
        session.set_incoming_request_handler(handler.clone());
        initialize(&transport, ClientCapabilities::default()).await?;
        transport
            .emit(Message::Request(Request::new(7, "tools/list", None)), ctx())
            .await?;
        assert_eq!(transport.last()?["result"]["echo"], "tools/list");
        transport
            .emit(
                Message::Notification(Notification::new("notifications/custom", None)),
                ctx(),
            )
            .await?;
        assert_eq!(handler.seen.lock().last().required()?, "n:notifications/custom");

        // Cancellation: the pending context is marked and an error with code 0 is sent.
        session
            .session_requests
            .lock()
            .insert(RequestId::Number(9), ctx());
        transport
            .emit(
                Message::Notification(Notification::new(
                    "notifications/cancelled",
                    Some(json!({"requestId": 9, "reason": "user"})),
                )),
                ctx(),
            )
            .await?;
        let v = transport.last()?;
        assert_eq!(v["id"], 9);
        assert_eq!(v["error"]["code"], 0);
        assert_eq!(v["error"]["message"], "Request cancelled");
        let before = transport.count();
        session
            .send_response(RequestId::Number(9), json!({}), &ctx())
            .await?;
        assert_eq!(transport.count(), before);
        assert!(session.session_requests.lock().is_empty());
        Ok(())
    }
}
