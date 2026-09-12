// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client session, port of `src/client/client_session.*`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ap_jsonrpc::{Message, Notification, Request, RequestId, Response, RpcError};
use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};

use crate::error::{JsonRpcErrorCode, McpError};
use crate::protocol::{
    CompleteRequestParams, ElicitParams, InitializeRequestParams, LATEST_PROTOCOL_VERSION,
    LoggingMessageNotificationParams, ProgressNotificationParams, inject_progress_token,
    is_supported_protocol_version, methods,
};
use crate::sampling_validation::validate_tool_use_result_messages;
use crate::session::{PendingRequests, ProgressCallback, await_response};
use crate::transport::{ClientTransport, RequestContext, TransportCallback};
use crate::types::{
    CallToolResult, ClientCapabilities, ClientConfig, CompleteReference, CompleteResult, CompletionArgument,
    CompletionContext, CreateMessageParams, CreateMessageResult, DEFAULT_CLIENT_NAME, DEFAULT_TIMEOUT,
    DEFAULT_VERSION, ElicitResult, ElicitationCapability, EmptyResult, FormElicitationCapability,
    GetPromptResult, InitializeResult, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
    ListRootsResult, ListToolsResult, LoggingLevel, MetaMap, ProgressToken, ReadResourceResult,
    RootsCapability, SamplingCapability, ServerCapabilities, UrlElicitationCapability,
};

/// Serves `roots/list`.
pub type ListRootsCallback =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ListRootsResult, McpError>> + Send + Sync>;
/// Serves `elicitation/create` in form mode: `(message, requestedSchema)`.
pub type ElicitCallback =
    Arc<dyn Fn(String, MetaMap) -> BoxFuture<'static, Result<ElicitResult, McpError>> + Send + Sync>;
/// Serves `elicitation/create` in url mode: `(message, url, elicitationId)`.
pub type ElicitUrlCallback =
    Arc<dyn Fn(String, String, String) -> BoxFuture<'static, Result<ElicitResult, McpError>> + Send + Sync>;
/// Receives `notifications/message`: `(level, data, logger)`.
pub type LoggingCallback = Arc<dyn Fn(String, Value, String) + Send + Sync>;
/// Serves `sampling/createMessage`. `Ok(None)` means the user rejected the request.
pub type SamplingCreateMessageCallback = Arc<
    dyn Fn(CreateMessageParams) -> BoxFuture<'static, Result<Option<CreateMessageResult>, McpError>>
        + Send
        + Sync,
>;

/// Client side MCP session.
pub struct ClientSession {
    transport: Arc<dyn ClientTransport>,
    pending: PendingRequests,
    config: ClientConfig,
    initialized: AtomicBool,
    server_capabilities: Mutex<Option<ServerCapabilities>>,
    list_roots: RwLock<Option<ListRootsCallback>>,
    logging: RwLock<Option<LoggingCallback>>,
    elicit: RwLock<Option<ElicitCallback>>,
    elicit_url: RwLock<Option<ElicitUrlCallback>>,
    sampling: RwLock<Option<(SamplingCreateMessageCallback, SamplingCapability)>>,
    tool_output_schemas: Mutex<HashMap<String, Value>>,
    request_timeout: Duration,
}

impl ClientSession {
    /// Create a session on `transport` and install it as the transport callback.
    pub fn new(transport: Arc<dyn ClientTransport>, config: ClientConfig) -> Arc<Self> {
        let session = Arc::new(Self {
            transport,
            pending: PendingRequests::new(),
            config,
            initialized: AtomicBool::new(false),
            server_capabilities: Mutex::new(None),
            list_roots: RwLock::new(None),
            logging: RwLock::new(None),
            elicit: RwLock::new(None),
            elicit_url: RwLock::new(None),
            sampling: RwLock::new(None),
            tool_output_schemas: Mutex::new(HashMap::new()),
            request_timeout: Duration::from_millis(DEFAULT_TIMEOUT),
        });
        let weak: Weak<dyn TransportCallback> = Arc::downgrade(&session) as Weak<dyn TransportCallback>;
        session.transport.set_callback(Some(weak));
        session
    }

    /// True after a successful `initialize` handshake.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    /// Capabilities advertised by the server, empty before `initialize`.
    pub fn server_capabilities(&self) -> ServerCapabilities {
        self.server_capabilities.lock().clone().unwrap_or_default()
    }

    /// Number of requests waiting for a response.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Register the `roots/list` callback.
    pub fn set_list_roots_callback(&self, cb: ListRootsCallback) {
        *self.list_roots.write() = Some(cb);
    }

    /// Register the `notifications/message` callback.
    pub fn set_logging_callback(&self, cb: LoggingCallback) {
        *self.logging.write() = Some(cb);
    }

    /// Register the form mode `elicitation/create` callback.
    pub fn set_elicit_callback(&self, cb: ElicitCallback) {
        *self.elicit.write() = Some(cb);
    }

    /// Register the url mode `elicitation/create` callback.
    pub fn set_elicit_url_callback(&self, cb: ElicitUrlCallback) {
        *self.elicit_url.write() = Some(cb);
    }

    /// Register the `sampling/createMessage` callback and its capability flags.
    pub fn set_sampling_create_message_callback(
        &self,
        cb: SamplingCreateMessageCallback,
        capability: SamplingCapability,
    ) {
        *self.sampling.write() = Some((cb, capability));
    }

    /// Capabilities to advertise, derived from the registered callbacks.
    pub fn build_client_capabilities(&self) -> ClientCapabilities {
        let mut caps = ClientCapabilities::default();
        if self.list_roots.read().is_some() {
            caps.roots = Some(RootsCapability { list_changed: true });
        }
        let form = self.elicit.read().is_some();
        let url = self.elicit_url.read().is_some();
        if form || url {
            caps.elicitation = Some(ElicitationCapability {
                form: form.then_some(FormElicitationCapability {}),
                url: url.then_some(UrlElicitationCapability {}),
            });
        }
        if let Some((_, capability)) = self.sampling.read().as_ref() {
            caps.sampling = Some(*capability);
        }
        caps
    }

    /// Send a request and wait for the typed result.
    pub async fn send_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Option<Duration>,
        progress: Option<ProgressCallback>,
    ) -> Result<T, McpError> {
        let value = self.send_request_raw(method, params, timeout, progress).await?;
        serde_json::from_value(value)
            .map_err(|e| McpError::state(format!("Result type mismatch for {method}: {e}")))
    }

    /// Send a request and wait for the raw result value.
    pub async fn send_request_raw(
        &self,
        method: &str,
        mut params: Option<Value>,
        timeout: Option<Duration>,
        progress: Option<ProgressCallback>,
    ) -> Result<Value, McpError> {
        let has_progress = progress.is_some();
        let (id, rx) = self.pending.register(progress);
        if has_progress {
            inject_progress_token(&mut params, &ProgressToken::from(id.clone()));
        }
        let request = Request::new(id.clone(), method, params);
        if let Err(e) = self.transport.send_message(Message::Request(request)).await {
            self.pending.remove(&id);
            return Err(McpError::Rpc(RpcError::internal_error(format!(
                "Transport error: {e}"
            ))));
        }
        let timeout = timeout.unwrap_or(self.request_timeout);
        await_response(&self.pending, &id, rx, timeout).await
    }

    /// Send a notification.
    pub async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        self.transport
            .send_message(Message::Notification(Notification::new(method, params)))
            .await
    }

    /// Perform the `initialize` handshake and send `notifications/initialized`.
    pub async fn initialize(&self) -> Result<InitializeResult, McpError> {
        let mut client_info = crate::types::Implementation::new(&self.config.name, &self.config.version);
        if client_info.name.is_empty() {
            client_info.name = DEFAULT_CLIENT_NAME.to_string();
        }
        if client_info.version.is_empty() {
            client_info.version = DEFAULT_VERSION.to_string();
        }
        let params = InitializeRequestParams {
            protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
            capabilities: self.build_client_capabilities(),
            client_info,
            meta: None,
        };
        let result: InitializeResult = self
            .send_request(
                methods::INITIALIZE,
                Some(serde_json::to_value(&params)?),
                None,
                None,
            )
            .await?;
        if !is_supported_protocol_version(&result.protocol_version) {
            return Err(McpError::state(format!(
                "Unsupported protocol version from the server: {}",
                result.protocol_version
            )));
        }
        *self.server_capabilities.lock() = Some(result.capabilities.clone());
        self.send_initialized_notification().await?;
        self.initialized.store(true, Ordering::SeqCst);
        Ok(result)
    }

    /// Send `notifications/initialized`.
    pub async fn send_initialized_notification(&self) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_INITIALIZED, Some(json!({})))
            .await
    }

    /// `ping`
    pub async fn send_ping(&self) -> Result<EmptyResult, McpError> {
        self.send_request(methods::PING, None, None, None).await
    }

    /// `tools/list`
    pub async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, McpError> {
        let params = cursor.map(|c| json!({"cursor": c})).unwrap_or(Value::Null);
        let result: ListToolsResult = self
            .send_request(methods::TOOLS_LIST, Some(params), None, None)
            .await?;
        self.cache_tool_schemas(&result);
        Ok(result)
    }

    fn cache_tool_schemas(&self, result: &ListToolsResult) {
        let mut cache = self.tool_output_schemas.lock();
        for tool in &result.tools {
            if let Some(schema) = &tool.output_schema {
                cache.insert(tool.name.clone(), schema.clone());
            }
        }
    }

    fn validate_tool_result(&self, name: &str, result: &CallToolResult) -> Result<(), McpError> {
        let Some(structured) = &result.structured_content else {
            return Ok(());
        };
        let schema = self.tool_output_schemas.lock().get(name).cloned();
        let Some(schema) = schema else {
            return Ok(());
        };
        if !structured.is_object() {
            return Err(McpError::Validation(format!(
                "Invalid structured content for tool {name}: structuredContent must be a JSON object"
            )));
        }
        crate::schema::validate(&schema, structured)
            .map_err(|e| McpError::Validation(format!("Invalid structured content for tool {name}: {e}")))
    }

    /// `tools/call`. `timeout_ms == 0` uses the default timeout.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<Value>,
        timeout_ms: u64,
        progress: Option<ProgressCallback>,
    ) -> Result<CallToolResult, McpError> {
        let mut params = json!({"name": name});
        if let Some(args) = arguments {
            params["arguments"] = args;
        }
        let timeout = (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms));
        let result: CallToolResult = self
            .send_request(methods::TOOLS_CALL, Some(params), timeout, progress)
            .await?;
        if !result.is_error {
            self.validate_tool_result(name, &result)?;
        }
        Ok(result)
    }

    /// `prompts/list`
    pub async fn list_prompts(&self) -> Result<ListPromptsResult, McpError> {
        self.send_request(methods::PROMPTS_LIST, Some(Value::Null), None, None)
            .await
    }

    /// `prompts/get`
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<GetPromptResult, McpError> {
        let mut params = json!({"name": name});
        if let Some(args) = arguments {
            params["arguments"] = args;
        }
        self.send_request(methods::PROMPTS_GET, Some(params), None, None)
            .await
    }

    /// `resources/list`
    pub async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, McpError> {
        let params = cursor.map(|c| json!({"cursor": c})).unwrap_or(Value::Null);
        self.send_request(methods::RESOURCES_LIST, Some(params), None, None)
            .await
    }

    /// `resources/templates/list`
    pub async fn list_resource_templates(&self) -> Result<ListResourceTemplatesResult, McpError> {
        self.send_request(methods::RESOURCES_TEMPLATES_LIST, Some(Value::Null), None, None)
            .await
    }

    /// `resources/read`
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        self.send_request(methods::RESOURCES_READ, Some(json!({"uri": uri})), None, None)
            .await
    }

    /// `resources/subscribe`
    pub async fn subscribe_resource(&self, uri: &str) -> Result<EmptyResult, McpError> {
        self.send_request(
            methods::RESOURCES_SUBSCRIBE,
            Some(json!({"uri": uri})),
            None,
            None,
        )
        .await
    }

    /// `resources/unsubscribe`
    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<EmptyResult, McpError> {
        self.send_request(
            methods::RESOURCES_UNSUBSCRIBE,
            Some(json!({"uri": uri})),
            None,
            None,
        )
        .await
    }

    /// `completion/complete`
    pub async fn complete(
        &self,
        reference: CompleteReference,
        argument: CompletionArgument,
        context: Option<CompletionContext>,
    ) -> Result<CompleteResult, McpError> {
        let params = CompleteRequestParams {
            reference,
            argument,
            context,
        };
        self.send_request(
            methods::COMPLETION_COMPLETE,
            Some(serde_json::to_value(params)?),
            None,
            None,
        )
        .await
    }

    /// `logging/setLevel`
    pub async fn set_logging_level(&self, level: LoggingLevel) -> Result<EmptyResult, McpError> {
        self.send_request(
            methods::LOGGING_SET_LEVEL,
            Some(json!({"level": level.as_str()})),
            None,
            None,
        )
        .await
    }

    /// `notifications/roots/list_changed`
    pub async fn send_roots_list_changed(&self) -> Result<(), McpError> {
        self.send_notification(methods::NOTIFICATION_ROOTS_LIST_CHANGED, Some(json!({})))
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

    async fn send_response(&self, id: RequestId, result: Value) {
        if let Err(e) = self
            .transport
            .send_message(Message::Response(Response::success(id, result)))
            .await
        {
            tracing::error!("Failed to send response: {e}");
        }
    }

    async fn send_error(&self, id: RequestId, code: i64, message: String) {
        let response = Response::error(Some(id), RpcError::new(code, message));
        if let Err(e) = self.transport.send_message(Message::Response(response)).await {
            tracing::error!("Failed to send error response: {e}");
        }
    }

    async fn send_method_not_found(&self, id: RequestId, method: &str) {
        self.send_error(
            id,
            JsonRpcErrorCode::MethodNotFound.code(),
            format!("Method not found: {method}"),
        )
        .await;
    }

    async fn send_internal_error(&self, id: RequestId, message: String) {
        self.send_error(id, JsonRpcErrorCode::InternalError.code(), message)
            .await;
    }

    async fn handle_roots_list(&self, id: RequestId, method: &str) {
        let cb = self.list_roots.read().clone();
        let Some(cb) = cb else {
            self.send_method_not_found(id, method).await;
            return;
        };
        match cb().await {
            Ok(result) => match serde_json::to_value(result) {
                Ok(v) => self.send_response(id, v).await,
                Err(e) => self.send_internal_error(id, e.to_string()).await,
            },
            Err(e) => self.send_internal_error(id, e.message()).await,
        }
    }

    async fn handle_elicit(&self, id: RequestId, method: &str, params: Option<Value>) {
        let parsed: Result<ElicitParams, _> = serde_json::from_value(params.unwrap_or(Value::Null));
        let outcome = match parsed {
            Ok(ElicitParams::Form {
                message,
                requested_schema,
            }) => {
                let cb = self.elicit.read().clone();
                let Some(cb) = cb else {
                    self.send_method_not_found(id, method).await;
                    return;
                };
                cb(message, requested_schema).await
            }
            Ok(ElicitParams::Url {
                message,
                url,
                elicitation_id,
            }) => {
                let cb = self.elicit_url.read().clone();
                let Some(cb) = cb else {
                    self.send_method_not_found(id, method).await;
                    return;
                };
                cb(message, url, elicitation_id).await
            }
            Err(e) => {
                self.send_error(
                    id,
                    JsonRpcErrorCode::InvalidParams.code(),
                    format!("Invalid params for elicitation/create: {e}"),
                )
                .await;
                return;
            }
        };
        match outcome {
            Ok(result) => match serde_json::to_value(result) {
                Ok(v) => self.send_response(id, v).await,
                Err(e) => self.send_internal_error(id, e.to_string()).await,
            },
            Err(e) => self.send_internal_error(id, e.message()).await,
        }
    }

    async fn handle_sampling_create_message(&self, id: RequestId, method: &str, params: Option<Value>) {
        let registered = self.sampling.read().clone();
        let Some((cb, capability)) = registered else {
            self.send_method_not_found(id, method).await;
            return;
        };
        let params: CreateMessageParams = match params {
            Some(v) if v.is_object() => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(_) => {
                    self.send_error(
                        id,
                        JsonRpcErrorCode::InvalidParams.code(),
                        "Invalid params for sampling/createMessage".into(),
                    )
                    .await;
                    return;
                }
            },
            _ => {
                self.send_error(
                    id,
                    JsonRpcErrorCode::InvalidParams.code(),
                    "Invalid params for sampling/createMessage".into(),
                )
                .await;
                return;
            }
        };
        if params.tools.is_some() && !capability.tools {
            self.send_error(
                id,
                JsonRpcErrorCode::InvalidParams.code(),
                "Tool-enabled sampling request received but client did not advertise sampling.tools capability".into(),
            )
            .await;
            return;
        }
        if let Some(v) = &params.include_context {
            if (v == "thisServer" || v == "allServers") && !capability.context {
                self.send_error(
                    id,
                    JsonRpcErrorCode::InvalidParams.code(),
                    "includeContext requires sampling.context capability".into(),
                )
                .await;
                return;
            }
        }
        if let Err(e) = validate_tool_use_result_messages(&params.messages) {
            self.send_error(id, JsonRpcErrorCode::InvalidParams.code(), e.message())
                .await;
            return;
        }
        match cb(params).await {
            Ok(Some(result)) => match serde_json::to_value(result) {
                Ok(v) => self.send_response(id, v).await,
                Err(e) => self.send_internal_error(id, e.to_string()).await,
            },
            // The specification recommends code -1 for user rejection.
            Ok(None) => {
                self.send_error(id, -1, "User rejected sampling request".into())
                    .await
            }
            Err(e) => self.send_internal_error(id, e.message()).await,
        }
    }

    async fn received_request(&self, request: Request) {
        let id = request.id.clone();
        match request.method.as_str() {
            methods::ROOTS_LIST => self.handle_roots_list(id, &request.method).await,
            methods::ELICITATION_CREATE => self.handle_elicit(id, &request.method, request.params).await,
            methods::SAMPLING_CREATE_MESSAGE => {
                self.handle_sampling_create_message(id, &request.method, request.params)
                    .await
            }
            other => self.send_method_not_found(id, other).await,
        }
    }

    async fn received_notification(&self, notification: Notification) {
        match notification.method.as_str() {
            methods::NOTIFICATION_PROGRESS => {
                let Ok(params) = serde_json::from_value::<ProgressNotificationParams>(
                    notification.params.unwrap_or(Value::Null),
                ) else {
                    return;
                };
                if let Some(cb) = self.pending.progress_callback(&params.progress_token) {
                    cb(params.progress, params.total, params.message);
                }
            }
            methods::NOTIFICATION_MESSAGE => {
                let Ok(params) = serde_json::from_value::<LoggingMessageNotificationParams>(
                    notification.params.unwrap_or(Value::Null),
                ) else {
                    tracing::error!("notifications/message get no params");
                    return;
                };
                let cb = self.logging.read().clone();
                match cb {
                    Some(cb) => cb(params.level, params.data, params.logger),
                    None => tracing::error!(
                        "Received Server logging. level is {}, data is {}, logger is {}",
                        params.level,
                        params.data,
                        params.logger
                    ),
                }
            }
            methods::NOTIFICATION_TOOLS_LIST_CHANGED
            | methods::NOTIFICATION_PROMPTS_LIST_CHANGED
            | methods::NOTIFICATION_RESOURCES_LIST_CHANGED => {
                tracing::info!("Client received notification: {}", notification.method);
            }
            other => tracing::error!("undefined notification: {other}"),
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
impl TransportCallback for ClientSession {
    async fn on_message_received(&self, message: Message, _ctx: RequestContext) {
        match message {
            Message::Request(r) => self.received_request(r).await,
            Message::Notification(n) => self.received_notification(n).await,
            Message::Response(r) => self.handle_response(r),
        }
    }

    async fn on_disconnected(&self, reason: String) {
        tracing::info!("client transport disconnected: {reason}");
        self.pending.fail_all(RpcError::internal_error(format!(
            "Transport disconnected: {reason}"
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::CallbackHandle;
    use crate::types::{
        RoleType, Root, SamplingContent, SamplingMessage, SamplingMessageContentBlock, Tool,
        ToolResultContent,
    };
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use std::sync::atomic::AtomicUsize;

    /// Fake transport recording sent messages and allowing incoming injection.
    struct FakeTransport {
        sent: Mutex<Vec<Message>>,
        callback: RwLock<Option<CallbackHandle>>,
        fail_send: AtomicBool,
    }

    impl FakeTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::new(Vec::new()),
                callback: RwLock::new(None),
                fail_send: AtomicBool::new(false),
            })
        }

        fn last(&self) -> TestResult<Message> {
            Ok(self.sent.lock().last().cloned().required()?)
        }

        fn count(&self) -> usize {
            self.sent.lock().len()
        }

        async fn emit(&self, message: Message) -> TestResult {
            let cb = self.callback.read().as_ref().and_then(Weak::upgrade).required()?;
            cb.on_message_received(message, RequestContext::default()).await;
            Ok(())
        }
    }

    #[async_trait]
    impl ClientTransport for FakeTransport {
        async fn connect(&self) -> Result<(), McpError> {
            Ok(())
        }
        async fn terminate(&self) {}
        async fn send_message(&self, message: Message) -> Result<(), McpError> {
            if self.fail_send.load(Ordering::SeqCst) {
                return Err(McpError::Transport("boom".into()));
            }
            self.sent.lock().push(message);
            Ok(())
        }
        fn set_callback(&self, callback: Option<CallbackHandle>) {
            *self.callback.write() = callback;
        }
    }

    fn setup() -> (Arc<FakeTransport>, Arc<ClientSession>) {
        let transport = FakeTransport::new();
        let session = ClientSession::new(transport.clone(), ClientConfig::default());
        (transport, session)
    }

    fn minimal_sampling_params() -> Value {
        json!({"maxTokens": 1, "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}]})
    }

    #[tokio::test]
    async fn call_tool_with_progress_injects_token_and_dispatches_progress() -> TestResult {
        let (transport, session) = setup();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(None));
        let (c, s) = (calls.clone(), seen.clone());
        let cb: ProgressCallback = Arc::new(move |p, t, m| {
            c.fetch_add(1, Ordering::SeqCst);
            *s.lock() = Some((p, t, m));
        });
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.call_tool("test_tool", None, 0, Some(cb)).await });
        tokio::task::yield_now().await;
        for _ in 0..10 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert_eq!(req.method, "tools/call");
        let params = req.params.clone().required()?;
        assert_eq!(params["_meta"]["progressToken"], json!(1));
        assert_eq!(params["name"], "test_tool");

        transport
            .emit(Message::Notification(Notification::new(
                "notifications/progress",
                Some(json!({"progressToken": 1, "progress": 0.5, "total": 1.0, "message": "half done"})),
            )))
            .await?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            seen.lock().clone().required()?,
            (0.5, Some(1.0), Some("half done".to_string()))
        );

        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"content": [{"type": "text", "text": "done"}], "isError": false}),
            )))
            .await?;
        let result = task.await??;
        assert_eq!(result.content[0].as_text(), Some("done"));
        assert_eq!(session.pending_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn send_progress_notification_wire_shape() -> TestResult {
        let (transport, session) = setup();
        session
            .send_progress_notification(
                ProgressToken::from("my-token"),
                0.5,
                Some(1.0),
                Some("50%".into()),
            )
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["method"], "notifications/progress");
        assert_eq!(v["params"]["progressToken"], "my-token");
        assert_eq!(v["params"]["progress"], 0.5);
        assert_eq!(v["params"]["total"], 1.0);
        assert_eq!(v["params"]["message"], "50%");
        session
            .send_progress_notification(ProgressToken::Number(42), 1.0, None, None)
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["params"]["progressToken"], 42);
        assert!(v["params"].get("total").is_none());
        assert!(v["params"].get("message").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn roots_list_request_and_roots_changed_notification() -> TestResult {
        let (transport, session) = setup();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        session.set_list_roots_callback(Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
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
        transport
            .emit(Message::Request(Request::new(1, "roots/list", None)))
            .await?;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let Message::Response(resp) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert_eq!(resp.id, Some(RequestId::Number(1)));
        assert_eq!(resp.result.required()?["roots"][0]["uri"], "file:///tmp");

        session.send_roots_list_changed().await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["method"], "notifications/roots/list_changed");
        assert!(v["params"].is_object());

        let caps = session.build_client_capabilities();
        assert!(caps.roots.required()?.list_changed);
        assert!(caps.sampling.is_none());
        assert!(caps.elicitation.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_request_and_missing_callbacks_reply_method_not_found() -> TestResult {
        let (transport, session) = setup();
        transport
            .emit(Message::Request(Request::new(1, "roots/list", None)))
            .await?;
        let Message::Response(resp) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert_eq!(resp.error.required()?.code, -32601);
        transport
            .emit(Message::Request(Request::new(
                2,
                "sampling/createMessage",
                Some(minimal_sampling_params()),
            )))
            .await?;
        assert_eq!(transport.last()?.to_value()["error"]["code"], -32601);
        transport
            .emit(Message::Request(Request::new(
                3,
                "elicitation/create",
                Some(json!({"mode": "form", "message": "m", "requestedSchema": {}})),
            )))
            .await?;
        assert_eq!(transport.last()?.to_value()["error"]["code"], -32601);
        transport
            .emit(Message::Request(Request::new(4, "whatever", None)))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["error"]["code"], -32601);
        assert_eq!(v["error"]["message"], "Method not found: whatever");
        assert_eq!(session.pending_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn sampling_capability_gating_and_validation() -> TestResult {
        let (transport, session) = setup();
        session.set_sampling_create_message_callback(
            Arc::new(|_| Box::pin(async { Ok(None) })),
            SamplingCapability {
                context: true,
                tools: false,
            },
        );
        let mut with_tools = minimal_sampling_params();
        with_tools["tools"] = json!([]);
        transport
            .emit(Message::Request(Request::new(
                1,
                "sampling/createMessage",
                Some(with_tools),
            )))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["id"], 1);
        assert_eq!(v["error"]["code"], -32602);

        session.set_sampling_create_message_callback(
            Arc::new(|_| Box::pin(async { Ok(None) })),
            SamplingCapability {
                context: false,
                tools: true,
            },
        );
        let mut with_context = minimal_sampling_params();
        with_context["includeContext"] = json!("thisServer");
        transport
            .emit(Message::Request(Request::new(
                2,
                "sampling/createMessage",
                Some(with_context),
            )))
            .await?;
        assert_eq!(transport.last()?.to_value()["error"]["code"], -32602);

        session.set_sampling_create_message_callback(
            Arc::new(|_| Box::pin(async { Ok(None) })),
            SamplingCapability {
                context: true,
                tools: true,
            },
        );
        let invalid = json!({"maxTokens": 1, "messages": [{"role": "user", "content": {"type": "tool_result", "toolUseId": "missing"}}]});
        transport
            .emit(Message::Request(Request::new(
                3,
                "sampling/createMessage",
                Some(invalid),
            )))
            .await?;
        assert_eq!(transport.last()?.to_value()["error"]["code"], -32602);

        transport
            .emit(Message::Request(Request::new(
                4,
                "sampling/createMessage",
                Some(minimal_sampling_params()),
            )))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["error"]["code"], -1);
        assert_eq!(v["error"]["message"], "User rejected sampling request");

        transport
            .emit(Message::Request(Request::new(
                5,
                "sampling/createMessage",
                Some(json!(5)),
            )))
            .await?;
        assert_eq!(
            transport.last()?.to_value()["error"]["message"],
            "Invalid params for sampling/createMessage"
        );

        session.set_sampling_create_message_callback(
            Arc::new(|params| {
                Box::pin(async move {
                    assert_eq!(params.max_tokens, 1);
                    Ok(Some(CreateMessageResult {
                        model: "ut-model".into(),
                        role: RoleType::Assistant,
                        content: SamplingContent::text("ok"),
                        stop_reason: None,
                        meta: None,
                    }))
                })
            }),
            SamplingCapability::default(),
        );
        transport
            .emit(Message::Request(Request::new(
                6,
                "sampling/createMessage",
                Some(minimal_sampling_params()),
            )))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["result"]["model"], "ut-model");
        assert_eq!(v["result"]["role"], "assistant");
        let caps = session.build_client_capabilities();
        assert_eq!(caps.sampling, Some(SamplingCapability::default()));

        let _ = (
            SamplingMessage::default(),
            SamplingMessageContentBlock::text(""),
            ToolResultContent::default(),
            Tool::default(),
        );
        Ok(())
    }

    #[tokio::test]
    async fn elicitation_form_and_url() -> TestResult {
        let (transport, session) = setup();
        session.set_elicit_callback(Arc::new(|message, schema| {
            Box::pin(async move {
                let mut content = MetaMap::new();
                content.insert("message".into(), json!(message));
                content.insert("schema".into(), Value::Object(schema));
                Ok(ElicitResult {
                    action: "accept".into(),
                    content,
                    meta: None,
                })
            })
        }));
        session.set_elicit_url_callback(Arc::new(|_m, url, id| {
            Box::pin(async move {
                let mut content = MetaMap::new();
                content.insert("url".into(), json!(url));
                content.insert("id".into(), json!(id));
                Ok(ElicitResult {
                    action: "accept".into(),
                    content,
                    meta: None,
                })
            })
        }));
        let caps = session.build_client_capabilities();
        let e = caps.elicitation.required()?;
        assert!(e.form.is_some() && e.url.is_some());

        transport
            .emit(Message::Request(Request::new(
                1,
                "elicitation/create",
                Some(json!({"mode": "form", "message": "name?", "requestedSchema": {"type": "object"}})),
            )))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["result"]["action"], "accept");
        assert_eq!(v["result"]["content"]["message"], "name?");
        transport
            .emit(Message::Request(Request::new(
                2,
                "elicitation/create",
                Some(json!({"mode": "url", "message": "go", "url": "https://x", "elicitationId": "e1"})),
            )))
            .await?;
        let v = transport.last()?.to_value();
        assert_eq!(v["result"]["content"]["url"], "https://x");
        transport
            .emit(Message::Request(Request::new(
                3,
                "elicitation/create",
                Some(json!({"mode": "other"})),
            )))
            .await?;
        assert_eq!(transport.last()?.to_value()["error"]["code"], -32602);
        Ok(())
    }

    #[tokio::test]
    async fn initialize_handshake_and_notifications() -> TestResult {
        let (transport, session) = setup();
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.initialize().await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert_eq!(req.method, "initialize");
        let params = req.params.clone().required()?;
        assert_eq!(params["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert_eq!(params["clientInfo"]["name"], "MCP Client");
        assert_eq!(params["capabilities"], json!({}));
        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "S", "version": "1"}}),
            )))
            .await?;
        let result = task.await??;
        assert_eq!(result.server_info.name, "S");
        assert!(session.is_initialized());
        assert!(session.server_capabilities().tools.is_some());
        let v = transport.last()?.to_value();
        assert_eq!(v["method"], "notifications/initialized");
        assert!(v["params"].is_object());

        // Logging notification with and without callback.
        let logs = Arc::new(Mutex::new(Vec::new()));
        let l = logs.clone();
        session.set_logging_callback(Arc::new(move |level, data, logger| {
            l.lock().push((level, data, logger));
        }));
        transport
            .emit(Message::Notification(Notification::new(
                "notifications/message",
                Some(json!({"level": "info", "data": "hello", "logger": "srv"})),
            )))
            .await?;
        assert_eq!(
            logs.lock()[0],
            ("info".to_string(), json!("hello"), "srv".to_string())
        );
        transport
            .emit(Message::Notification(Notification::new(
                "notifications/tools/list_changed",
                None,
            )))
            .await?;
        transport
            .emit(Message::Notification(Notification::new(
                "notifications/unknown",
                None,
            )))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_server_version_fails_initialize() -> TestResult {
        let (transport, session) = setup();
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.initialize().await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"protocolVersion": "1999-01-01"}),
            )))
            .await?;
        let err = task.await?.err_or_fail()?;
        assert!(
            err.to_string()
                .contains("Unsupported protocol version from the server: 1999-01-01")
        );
        assert!(!session.is_initialized());
        Ok(())
    }

    #[tokio::test]
    async fn transport_failure_and_rpc_errors_surface() -> TestResult {
        let (transport, session) = setup();
        transport.fail_send.store(true, Ordering::SeqCst);
        let err = session.send_ping().await.err_or_fail()?;
        assert_eq!(err.code(), Some(-32603));
        assert!(err.message().starts_with("Transport error:"));
        assert_eq!(session.pending_count(), 0);
        transport.fail_send.store(false, Ordering::SeqCst);

        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.list_prompts().await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert!(req.params.as_ref().required()?.is_null());
        transport
            .emit(Message::Response(Response::error(
                Some(req.id.clone()),
                RpcError::invalid_params("bad"),
            )))
            .await?;
        let err = task.await?.err_or_fail()?;
        assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::InvalidParams));
        assert_eq!(err.message(), "bad");
        Ok(())
    }

    #[tokio::test]
    async fn output_schema_validation_after_list_tools() -> TestResult {
        let (transport, session) = setup();
        let s2 = session.clone();
        let task = tokio::spawn(async move { s2.list_tools(Some("10".into())).await });
        for _ in 0..50 {
            if transport.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        assert_eq!(req.params.as_ref().required()?["cursor"], "10");
        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"tools": [{"name": "echo", "outputSchema": {"type": "object", "required": ["result"]}}],
                    "nextCursor": "20"}),
            )))
            .await?;
        let list = task.await??;
        assert_eq!(list.next_cursor.as_deref(), Some("20"));

        let s3 = session.clone();
        let task = tokio::spawn(async move { s3.call_tool("echo", Some(json!({"x": 1})), 0, None).await });
        for _ in 0..50 {
            if transport.count() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"content": [], "structuredContent": {"other": 1}, "isError": false}),
            )))
            .await?;
        let err = task.await?.err_or_fail()?;
        assert!(
            err.to_string()
                .starts_with("Invalid structured content for tool echo")
        );

        let s4 = session.clone();
        let task = tokio::spawn(async move { s4.call_tool("echo", None, 0, None).await });
        for _ in 0..50 {
            if transport.count() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let Message::Request(req) = transport.last()? else {
            return Err(ap_support::testing::TestFailure::new("unexpected value").into());
        };
        transport
            .emit(Message::Response(Response::success(
                req.id.clone(),
                json!({"content": [], "structuredContent": {"other": 1}, "isError": true}),
            )))
            .await?;
        assert!(task.await??.is_error);
        Ok(())
    }

    #[tokio::test]
    async fn request_timeout_is_reported() -> TestResult {
        let (_transport, session) = setup();
        let err = session.call_tool("slow", None, 5, None).await.err_or_fail()?;
        assert!(matches!(err, McpError::Timeout(5)));
        assert_eq!(session.pending_count(), 0);
        Ok(())
    }
}
