// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! MCP server, port of `mcp_server.h`, `mcp_server_implement.*` and the
//! server factory.
//!
//! Tool, prompt and resource handlers are async closures. The C++ SDK offers
//! a synchronous and an asynchronous variant of each handler; an `async fn`
//! covers both, so the `responseCallback` of `ServerContext` is not needed.

pub mod manager;
pub mod prompt_manager;
pub mod resource_manager;
pub mod session;
pub mod tool_manager;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use ap_jsonrpc::{Notification, Request, RequestId, RpcError};
use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};

pub use manager::ServerManager;
pub use prompt_manager::PromptManager;
pub use resource_manager::ResourceManager;
pub use session::{IncomingRequestHandler, ServerSession};
pub use tool_manager::{ServerTool, ToolManager};

use crate::error::{JsonRpcErrorCode, McpError};
use crate::protocol::{
    CallToolParams, CompleteRequestParams, GetPromptParams, MAX_THREAD_NUM, PaginatedParams,
    SetLoggingLevelParams, UriParams, is_valid_streamable_http_endpoint, methods, progress_meta,
};
use crate::transport::RequestContext;
use crate::transport::stdio::{BoxedReader, BoxedWriter};
use crate::types::{
    Annotations, CallToolResult, CompleteReference, CompleteResult, CompletionArgument, CompletionContext,
    GetPromptResult, Icon, PromptArgument, PromptInfo, ReadResourceResult, RequestParamsMeta, ResourceInfo,
    ResourceTemplate, ServerConfig, StreamableHttpServerConfig, TlsConfig, ToolAnnotations,
};

/// Context handed to tool, prompt and resource handlers.
#[derive(Clone)]
pub struct ServerContext {
    /// The session that received the request. Use it to send notifications
    /// or server initiated requests such as `sampling/createMessage`.
    pub session: Arc<ServerSession>,
    /// `params._meta` of the request, for example the progress token.
    pub meta: Option<RequestParamsMeta>,
}

impl ServerContext {
    /// Build a context.
    pub fn new(session: Arc<ServerSession>, meta: Option<RequestParamsMeta>) -> Self {
        Self { session, meta }
    }

    /// The progress token sent by the client, if any.
    pub fn progress_token(&self) -> Option<crate::types::ProgressToken> {
        self.meta.as_ref().and_then(|m| m.progress_token.clone())
    }
}

/// Tool handler: `(ctx, name, arguments)`. Errors become `isError` results.
pub type ToolHandler = Arc<
    dyn Fn(ServerContext, String, Value) -> BoxFuture<'static, Result<CallToolResult, McpError>>
        + Send
        + Sync,
>;
/// Prompt handler: `(ctx, name, arguments)`.
pub type PromptHandler = Arc<
    dyn Fn(ServerContext, String, Option<Value>) -> BoxFuture<'static, Result<GetPromptResult, McpError>>
        + Send
        + Sync,
>;
/// Resource handler: `(ctx, uri)`.
pub type ResourceHandler = Arc<
    dyn Fn(ServerContext, String) -> BoxFuture<'static, Result<ReadResourceResult, McpError>> + Send + Sync,
>;
/// Completion handler: `(ref, argument, context)`.
pub type CompleteHandler = Arc<
    dyn Fn(
            CompleteReference,
            CompletionArgument,
            Option<CompletionContext>,
        ) -> BoxFuture<'static, Result<CompleteResult, McpError>>
        + Send
        + Sync,
>;
/// `logging/setLevel` handler receiving the level name.
pub type SetLoggingLevelHandler = Arc<dyn Fn(&str) -> Result<(), McpError> + Send + Sync>;

/// Box an async tool closure into a [`ToolHandler`].
pub fn tool_handler<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(ServerContext, String, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<CallToolResult, McpError>> + Send + 'static,
{
    Arc::new(move |ctx, name, args| Box::pin(f(ctx, name, args)))
}

/// Box an async prompt closure into a [`PromptHandler`].
pub fn prompt_handler<F, Fut>(f: F) -> PromptHandler
where
    F: Fn(ServerContext, String, Option<Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<GetPromptResult, McpError>> + Send + 'static,
{
    Arc::new(move |ctx, name, args| Box::pin(f(ctx, name, args)))
}

/// Box an async resource closure into a [`ResourceHandler`].
pub fn resource_handler<F, Fut>(f: F) -> ResourceHandler
where
    F: Fn(ServerContext, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<ReadResourceResult, McpError>> + Send + 'static,
{
    Arc::new(move |ctx, uri| Box::pin(f(ctx, uri)))
}

/// Box an async completion closure into a [`CompleteHandler`].
pub fn complete_handler<F, Fut>(f: F) -> CompleteHandler
where
    F: Fn(CompleteReference, CompletionArgument, Option<CompletionContext>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<CompleteResult, McpError>> + Send + 'static,
{
    Arc::new(move |r, a, c| Box::pin(f(r, a, c)))
}

/// Optional parameters of [`McpServer::add_tool`].
#[derive(Clone, Debug, Default)]
pub struct AddToolOptionalParams {
    /// Title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// JSON schema validated against the arguments.
    pub input_schema: Option<Value>,
    /// JSON schema validated against `structuredContent`.
    pub output_schema: Option<Value>,
    /// Kept for parity with the C++ SDK; unused.
    pub structured_output: bool,
    /// Behaviour hints.
    pub annotations: Option<ToolAnnotations>,
    /// Icons.
    pub icons: Option<Vec<Icon>>,
}

/// Optional parameters of [`McpServer::add_prompt`].
#[derive(Clone, Debug, Default)]
pub struct AddPromptOptionalParams {
    /// Description.
    pub description: Option<String>,
    /// Title.
    pub title: Option<String>,
    /// Icons.
    pub icons: Option<Vec<Icon>>,
    /// Arguments.
    pub arguments: Option<Vec<PromptArgument>>,
}

/// Optional parameters of [`McpServer::add_resource`].
#[derive(Clone, Debug, Default)]
pub struct AddResourceOptionalParams {
    /// Title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: Option<i64>,
    /// Icons.
    pub icons: Option<Vec<Icon>>,
    /// Annotations.
    pub annotations: Option<Annotations>,
}

/// Optional parameters of [`McpServer::add_resource_template`].
#[derive(Clone, Debug, Default)]
pub struct AddResourceTemplateOptionalParams {
    /// Title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Icons.
    pub icons: Option<Vec<Icon>>,
    /// Annotations.
    pub annotations: Option<Annotations>,
}

/// Lifecycle state of a server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    /// Created but not started.
    Init,
    /// Running.
    Running,
    /// Stopped; cannot be restarted.
    Stopped,
}

impl ServerState {
    fn name(self) -> &'static str {
        match self {
            ServerState::Init => "INIT",
            ServerState::Running => "RUNNING",
            ServerState::Stopped => "STOPPED",
        }
    }
}

/// Shared server core: registries and request dispatch.
struct ServerInner {
    tools: ToolManager,
    prompts: PromptManager,
    resources: ResourceManager,
    set_level_handler: RwLock<Option<SetLoggingLevelHandler>>,
    complete_handler: RwLock<Option<CompleteHandler>>,
}

impl ServerInner {
    fn params<T: serde::de::DeserializeOwned>(request: &Request) -> Option<T> {
        request
            .params
            .clone()
            .filter(|p| p.is_object())
            .and_then(|p| serde_json::from_value(p).ok())
    }

    async fn send_result<T: serde::Serialize>(
        session: &ServerSession,
        id: RequestId,
        result: &T,
        ctx: &RequestContext,
    ) {
        match serde_json::to_value(result) {
            Ok(value) => {
                if let Err(e) = session.send_response(id, value, ctx).await {
                    tracing::error!("Failed to send response: {e}");
                }
            }
            Err(e) => {
                Self::send_error(session, id, JsonRpcErrorCode::InternalError, e.to_string(), ctx).await;
            }
        }
    }

    async fn send_error(
        session: &ServerSession,
        id: RequestId,
        code: JsonRpcErrorCode,
        message: String,
        ctx: &RequestContext,
    ) {
        let error = RpcError::new(code.code(), message);
        if let Err(e) = session.send_error_response(id, error, ctx).await {
            tracing::error!("Failed to send error response: {e}");
        }
    }

    async fn dispatch(
        &self,
        session: Arc<ServerSession>,
        id: RequestId,
        request: Request,
        ctx: RequestContext,
    ) {
        let method = request.method.clone();
        match method.as_str() {
            methods::TOOLS_LIST => {
                let cursor = Self::params::<PaginatedParams>(&request).and_then(|p| p.cursor);
                let result = self.tools.list_tools(cursor.as_deref());
                Self::send_result(&session, id, &result, &ctx).await;
            }
            methods::TOOLS_CALL => {
                let Some(params) = Self::params::<CallToolParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "Invalid params for tools/call".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                let server_ctx = ServerContext::new(session.clone(), progress_meta(&request.params));
                let args = params.arguments.unwrap_or_else(|| json!({}));
                match self.tools.call_tool(server_ctx, &params.name, args).await {
                    Ok(result) => Self::send_result(&session, id, &result, &ctx).await,
                    Err(e) => {
                        tracing::info!("CallTool: caught exception: {e}");
                        Self::send_error(&session, id, JsonRpcErrorCode::InvalidParams, e.message(), &ctx)
                            .await;
                    }
                }
            }
            methods::PROMPTS_LIST => {
                let result = self.prompts.list_prompts();
                Self::send_result(&session, id, &result, &ctx).await;
            }
            methods::PROMPTS_GET => {
                let Some(params) = Self::params::<GetPromptParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "Invalid params for prompts/get".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                let server_ctx = ServerContext::new(session.clone(), progress_meta(&request.params));
                match self
                    .prompts
                    .get_prompt(server_ctx, &params.name, params.arguments)
                    .await
                {
                    Ok(result) => Self::send_result(&session, id, &result, &ctx).await,
                    Err(e) => {
                        Self::send_error(&session, id, JsonRpcErrorCode::InvalidParams, e.message(), &ctx)
                            .await
                    }
                }
            }
            methods::RESOURCES_LIST => {
                let cursor = Self::params::<PaginatedParams>(&request).and_then(|p| p.cursor);
                let result = self.resources.list_resources(cursor.as_deref());
                Self::send_result(&session, id, &result, &ctx).await;
            }
            methods::RESOURCES_READ => {
                let Some(params) = Self::params::<UriParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "Invalid params for resources/read".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                if params.uri.is_empty() {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "URI cannot be empty".into(),
                        &ctx,
                    )
                    .await;
                    return;
                }
                let server_ctx = ServerContext::new(session.clone(), progress_meta(&request.params));
                match self.resources.read_resource(server_ctx, &params.uri).await {
                    Ok(result) => Self::send_result(&session, id, &result, &ctx).await,
                    Err(e) => {
                        Self::send_error(&session, id, JsonRpcErrorCode::InvalidParams, e.message(), &ctx)
                            .await
                    }
                }
            }
            methods::RESOURCES_SUBSCRIBE | methods::RESOURCES_UNSUBSCRIBE => {
                let Some(params) = Self::params::<UriParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        format!("Invalid params for {method}"),
                        &ctx,
                    )
                    .await;
                    return;
                };
                if params.uri.is_empty() {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "URI cannot be empty".into(),
                        &ctx,
                    )
                    .await;
                    return;
                }
                let outcome = if method == methods::RESOURCES_SUBSCRIBE {
                    self.resources.subscribe_resource(&params.uri)
                } else {
                    self.resources.unsubscribe_resource(&params.uri)
                };
                match outcome {
                    Ok(()) => Self::send_result(&session, id, &json!({}), &ctx).await,
                    Err(e) => {
                        Self::send_error(&session, id, JsonRpcErrorCode::ServerError, e.message(), &ctx).await
                    }
                }
            }
            methods::RESOURCES_TEMPLATES_LIST => {
                let result = self.resources.list_resource_templates();
                Self::send_result(&session, id, &result, &ctx).await;
            }
            methods::LOGGING_SET_LEVEL => {
                let Some(params) = Self::params::<SetLoggingLevelParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidParams,
                        "Invalid params for logging/setLevel".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                let handler = self.set_level_handler.read().clone();
                match handler {
                    None => {
                        Self::send_error(
                            &session,
                            id,
                            JsonRpcErrorCode::InvalidParams,
                            "not set LoggingLevelHandler".into(),
                            &ctx,
                        )
                        .await
                    }
                    Some(h) => match h(&params.level) {
                        Ok(()) => Self::send_result(&session, id, &json!({}), &ctx).await,
                        Err(e) => {
                            Self::send_error(&session, id, JsonRpcErrorCode::ServerError, e.message(), &ctx)
                                .await
                        }
                    },
                }
            }
            methods::PING => Self::send_result(&session, id, &json!({}), &ctx).await,
            methods::COMPLETION_COMPLETE => {
                let Some(params) = Self::params::<CompleteRequestParams>(&request) else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::InvalidRequest,
                        "Invalid complete request".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                let handler = self.complete_handler.read().clone();
                let Some(handler) = handler else {
                    Self::send_error(
                        &session,
                        id,
                        JsonRpcErrorCode::ServerError,
                        "Completion handler not registered".into(),
                        &ctx,
                    )
                    .await;
                    return;
                };
                match handler(params.reference, params.argument, params.context).await {
                    Ok(result) => Self::send_result(&session, id, &result, &ctx).await,
                    Err(e) => {
                        Self::send_error(
                            &session,
                            id,
                            JsonRpcErrorCode::ServerError,
                            format!("Complete handler error: {}", e.message()),
                            &ctx,
                        )
                        .await
                    }
                }
            }
            other => {
                Self::send_error(
                    &session,
                    id,
                    JsonRpcErrorCode::MethodNotFound,
                    format!("Method not found: {other}"),
                    &ctx,
                )
                .await
            }
        }
    }
}

#[async_trait]
impl IncomingRequestHandler for ServerInner {
    async fn handle_request(
        &self,
        session: Arc<ServerSession>,
        id: RequestId,
        request: Request,
        ctx: RequestContext,
    ) {
        self.dispatch(session, id, request, ctx).await;
    }

    async fn handle_notification(&self, _session: Arc<ServerSession>, notification: Notification) {
        tracing::info!("Server received notification: {}", notification.method);
    }
}

/// MCP server (`Mcp::McpServer`).
pub struct McpServer {
    config: ServerConfig,
    transport_config: Option<StreamableHttpServerConfig>,
    inner: Arc<ServerInner>,
    state: Mutex<ServerState>,
    manager: Mutex<Option<Arc<ServerManager>>>,
    stdio_streams: Mutex<Option<(BoxedReader, BoxedWriter)>>,
}

fn validate_config(config: &ServerConfig, is_stdio: bool) -> Result<(), McpError> {
    let name_blank = config.name.trim().is_empty();
    if name_blank || config.version.is_empty() || !config.version.contains('.') {
        tracing::error!("Invalid Server name or version");
        return Err(McpError::argument("Invalid server configuration"));
    }
    if is_stdio {
        return Ok(());
    }
    if config.worker_threads == 0 || config.worker_threads > MAX_THREAD_NUM {
        tracing::error!(
            "Worker threads count ({}) should be in (0, {MAX_THREAD_NUM})",
            config.worker_threads
        );
        return Err(McpError::argument("Invalid server configuration"));
    }
    Ok(())
}

fn validate_file_path(path: &str, what: &str) -> Result<(), McpError> {
    if path.is_empty() {
        return Ok(());
    }
    match std::fs::canonicalize(path) {
        Ok(canonical) => {
            tracing::debug!("{what} validated: {}", canonical.display());
            Ok(())
        }
        Err(e) => {
            tracing::error!("{what} realpath failed: {path} ({e})");
            Err(McpError::argument(
                "Invalid Streamable HTTP transport configuration",
            ))
        }
    }
}

/// Validate a server TLS configuration like `McpServerImplement::ValidateTlsConfig`.
pub fn validate_tls_config(config: &TlsConfig) -> Result<(), McpError> {
    if !config.enabled {
        return Ok(());
    }
    const MAX_HOSTNAME_LENGTH: usize = 253;
    if !config.server_name.is_empty() {
        if config.server_name.len() > MAX_HOSTNAME_LENGTH {
            tracing::error!(
                "TLS server name too long: {} characters",
                config.server_name.len()
            );
            return Err(McpError::argument(
                "Invalid Streamable HTTP transport configuration",
            ));
        }
        if config
            .server_name
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
        {
            tracing::error!("TLS server name contains invalid whitespace characters");
            return Err(McpError::argument(
                "Invalid Streamable HTTP transport configuration",
            ));
        }
        if config.server_name.contains("..") {
            tracing::error!("TLS server name contains invalid path traversal sequence");
            return Err(McpError::argument(
                "Invalid Streamable HTTP transport configuration",
            ));
        }
    }
    validate_file_path(&config.cert_file, "TLS certificate file")?;
    validate_file_path(&config.key_file, "TLS key file")?;
    validate_file_path(&config.ca_file, "TLS CA file")?;
    Ok(())
}

fn validate_streamable_http_config(config: &StreamableHttpServerConfig) -> Result<(), McpError> {
    if let Err(e) = is_valid_streamable_http_endpoint(&config.endpoint) {
        tracing::error!("Invalid Streamable HTTP endpoint: {e}");
        return Err(McpError::argument(
            "Invalid Streamable HTTP transport configuration",
        ));
    }
    if config.io_threads == 0 || config.io_threads > MAX_THREAD_NUM {
        tracing::error!(
            "IO threads count ({}) should be in (0, {MAX_THREAD_NUM})",
            config.io_threads
        );
        return Err(McpError::argument(
            "Invalid Streamable HTTP transport configuration",
        ));
    }
    validate_tls_config(&config.tls_config)
}

impl McpServer {
    fn build(config: ServerConfig, transport_config: Option<StreamableHttpServerConfig>) -> Self {
        let inner = Arc::new(ServerInner {
            tools: ToolManager::new(true, config.tools_page_size),
            prompts: PromptManager::new(true),
            resources: ResourceManager::new(true, config.resources_page_size),
            set_level_handler: RwLock::new(None),
            complete_handler: RwLock::new(None),
        });
        Self {
            config,
            transport_config,
            inner,
            state: Mutex::new(ServerState::Init),
            manager: Mutex::new(None),
            stdio_streams: Mutex::new(None),
        }
    }

    /// Create a stdio server. Fails with [`McpError::InvalidArgument`] for an invalid configuration.
    pub fn new_stdio(config: ServerConfig) -> Result<Self, McpError> {
        validate_config(&config, true)?;
        Ok(Self::build(config, None))
    }

    /// Create a Streamable HTTP server. Fails with [`McpError::InvalidArgument`]
    /// for an invalid server or transport configuration.
    pub fn new_streamable_http(
        config: ServerConfig,
        transport_config: StreamableHttpServerConfig,
    ) -> Result<Self, McpError> {
        validate_config(&config, false)?;
        validate_streamable_http_config(&transport_config)?;
        Ok(Self::build(config, Some(transport_config)))
    }

    /// The server configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The transport configuration for HTTP servers.
    pub fn transport_config(&self) -> Option<&StreamableHttpServerConfig> {
        self.transport_config.as_ref()
    }

    /// Use explicit streams instead of the process stdio. Call before [`McpServer::run`].
    pub fn set_stdio_streams<R, W>(&self, reader: R, writer: W)
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
        W: tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        *self.stdio_streams.lock() = Some((Box::new(reader), Box::new(writer)));
    }

    /// Current lifecycle state.
    pub fn state(&self) -> ServerState {
        *self.state.lock()
    }

    /// True while running.
    pub fn is_running(&self) -> bool {
        self.state() == ServerState::Running
    }

    /// Address the HTTP server listens on, once started. Useful with port `0`.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.manager.lock().as_ref().and_then(|m| m.local_addr())
    }

    /// Number of live HTTP sessions.
    pub fn session_count(&self) -> usize {
        self.manager
            .lock()
            .as_ref()
            .map(|m| m.session_count())
            .unwrap_or(0)
    }

    /// The stdio session, once started.
    pub fn stdio_session(&self) -> Option<Arc<ServerSession>> {
        self.manager.lock().as_ref().and_then(|m| m.stdio_session())
    }

    /// Start the server. Fails when the server is not in the initial state or
    /// the transport cannot start.
    pub async fn run(&self) -> Result<(), McpError> {
        let current = self.state();
        if current != ServerState::Init {
            let message = format!(
                "run failed. Server is not in init state, current state: {}",
                current.name()
            );
            tracing::error!("{message}");
            return Err(McpError::state(message));
        }
        let handler: Arc<dyn IncomingRequestHandler> = self.inner.clone();
        let manager = ServerManager::new(self.config.clone(), self.transport_config.clone(), handler);
        if let Some((r, w)) = self.stdio_streams.lock().take() {
            manager.set_stdio_streams(r, w);
        }
        if let Err(e) = manager.start().await {
            tracing::error!("Exception while creating ServerManager: {e}");
            return Err(e);
        }
        *self.manager.lock() = Some(manager);
        *self.state.lock() = ServerState::Running;
        tracing::info!("MCP Server started successfully");
        Ok(())
    }

    /// Stop the server. Does nothing when it is not running.
    pub async fn stop(&self) {
        if self.state() != ServerState::Running {
            tracing::warn!("Server is not running");
            return;
        }
        *self.state.lock() = ServerState::Stopped;
        let manager = self.manager.lock().clone();
        if let Some(manager) = manager {
            manager.stop().await;
        }
        tracing::info!("MCP Server stopped");
    }

    fn check_state(&self) -> Result<(), McpError> {
        if self.state() == ServerState::Stopped {
            return Err(McpError::state(
                "Cannot perform operation: server has been stopped",
            ));
        }
        Ok(())
    }

    /// Register a tool.
    pub fn add_tool<F, Fut>(
        &self,
        name: &str,
        handler: F,
        params: AddToolOptionalParams,
    ) -> Result<(), McpError>
    where
        F: Fn(ServerContext, String, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CallToolResult, McpError>> + Send + 'static,
    {
        self.add_tool_handler(name, tool_handler(handler), params)
    }

    /// Register a tool with an already boxed handler.
    pub fn add_tool_handler(
        &self,
        name: &str,
        handler: ToolHandler,
        params: AddToolOptionalParams,
    ) -> Result<(), McpError> {
        self.check_state()?;
        if name.is_empty() {
            return Err(McpError::argument("Tool name cannot be empty"));
        }
        let tool = ServerTool {
            name: name.to_string(),
            handler,
            title: params.title,
            description: params.description,
            input_schema: params.input_schema,
            output_schema: params.output_schema,
            structured_output: params.structured_output,
            annotations: params.annotations,
            icons: params.icons,
        };
        self.inner.tools.add_tool(tool)
    }

    /// Remove a tool.
    pub fn remove_tool(&self, name: &str) -> Result<(), McpError> {
        self.check_state()?;
        self.inner.tools.remove_tool(name)
    }

    /// Register a prompt.
    pub fn add_prompt<F, Fut>(
        &self,
        name: &str,
        handler: F,
        params: AddPromptOptionalParams,
    ) -> Result<(), McpError>
    where
        F: Fn(ServerContext, String, Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<GetPromptResult, McpError>> + Send + 'static,
    {
        self.add_prompt_handler(name, prompt_handler(handler), params)
    }

    /// Register a prompt with an already boxed handler.
    pub fn add_prompt_handler(
        &self,
        name: &str,
        handler: PromptHandler,
        params: AddPromptOptionalParams,
    ) -> Result<(), McpError> {
        self.check_state()?;
        let info = PromptInfo {
            name: name.to_string(),
            description: params.description,
            title: params.title,
            icons: params.icons,
            arguments: params.arguments,
        };
        self.inner.prompts.add_prompt(info, handler)
    }

    /// Remove a prompt.
    pub fn remove_prompt(&self, name: &str) -> Result<(), McpError> {
        self.check_state()?;
        self.inner.prompts.remove_prompt(name);
        Ok(())
    }

    /// Register a resource.
    pub fn add_resource<F, Fut>(
        &self,
        uri: &str,
        name: &str,
        handler: F,
        params: AddResourceOptionalParams,
    ) -> Result<(), McpError>
    where
        F: Fn(ServerContext, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ReadResourceResult, McpError>> + Send + 'static,
    {
        self.add_resource_handler(uri, name, resource_handler(handler), params)
    }

    /// Register a resource with an already boxed handler.
    pub fn add_resource_handler(
        &self,
        uri: &str,
        name: &str,
        handler: ResourceHandler,
        params: AddResourceOptionalParams,
    ) -> Result<(), McpError> {
        self.check_state()?;
        let info = ResourceInfo {
            uri: uri.to_string(),
            name: name.to_string(),
            title: params.title,
            description: params.description,
            mime_type: params.mime_type,
            size: params.size,
            icons: params.icons,
            annotations: params.annotations,
        };
        self.inner.resources.add_resource(info, handler)
    }

    /// Remove a resource.
    pub fn remove_resource(&self, uri: &str) -> Result<(), McpError> {
        self.check_state()?;
        self.inner.resources.remove_resource(uri)
    }

    /// Register a resource template.
    pub fn add_resource_template(
        &self,
        uri_template: &str,
        name: &str,
        params: AddResourceTemplateOptionalParams,
    ) -> Result<(), McpError> {
        self.check_state()?;
        let template = ResourceTemplate {
            uri_template: uri_template.to_string(),
            name: name.to_string(),
            title: params.title,
            description: params.description,
            mime_type: params.mime_type,
            icons: params.icons,
            annotations: params.annotations,
        };
        self.inner.resources.add_resource_template(template)
    }

    /// Remove a resource template.
    pub fn remove_resource_template(&self, uri_template: &str) -> Result<(), McpError> {
        self.check_state()?;
        self.inner.resources.remove_resource_template(uri_template)
    }

    /// Register the `logging/setLevel` handler.
    pub fn register_set_logging_level_handler<F>(&self, handler: F)
    where
        F: Fn(&str) -> Result<(), McpError> + Send + Sync + 'static,
    {
        *self.inner.set_level_handler.write() = Some(Arc::new(handler));
    }

    /// Register the `completion/complete` handler.
    pub fn add_completion<F, Fut>(&self, handler: F)
    where
        F: Fn(CompleteReference, CompletionArgument, Option<CompletionContext>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<CompleteResult, McpError>> + Send + 'static,
    {
        *self.inner.complete_handler.write() = Some(complete_handler(handler));
        tracing::info!("Completion handler registered");
    }

    /// The tool registry.
    pub fn tools(&self) -> &ToolManager {
        &self.inner.tools
    }

    /// The prompt registry.
    pub fn prompts(&self) -> &PromptManager {
        &self.inner.prompts
    }

    /// The resource registry.
    pub fn resources(&self) -> &ResourceManager {
        &self.inner.resources
    }
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("config", &self.config)
            .field("transport_config", &self.transport_config)
            .field("state", &self.state())
            .finish()
    }
}

/// Factory for servers (`Mcp::McpServerFactory`).
pub struct McpServerFactory;

impl McpServerFactory {
    /// Create a Streamable HTTP server.
    pub fn create_streamable_http_server(
        config: ServerConfig,
        transport_config: StreamableHttpServerConfig,
    ) -> Result<McpServer, McpError> {
        McpServer::new_streamable_http(config, transport_config)
    }

    /// Create a stdio server.
    pub fn create_stdio_server(config: ServerConfig) -> Result<McpServer, McpError> {
        McpServer::new_stdio(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ResourceContents;
    use ap_support::testing::{ResultExt, TestResult};

    fn stdio_config() -> ServerConfig {
        ServerConfig {
            name: "TestServer".into(),
            version: "1.0.0".into(),
            worker_threads: 1,
            ..Default::default()
        }
    }

    fn http_config() -> ServerConfig {
        ServerConfig {
            worker_threads: 4,
            ..stdio_config()
        }
    }

    fn free_port() -> TestResult<u16> {
        Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
    }

    fn transport_config() -> TestResult<StreamableHttpServerConfig> {
        Ok(StreamableHttpServerConfig::new(format!(
            "http://127.0.0.1:{}/mcp",
            free_port()?
        )))
    }

    async fn echo(_ctx: ServerContext, _name: String, args: Value) -> Result<CallToolResult, McpError> {
        let q = args
            .get("user_query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok(CallToolResult::text(format!("Echo: {q}")))
    }

    #[test]
    fn constructor_validation() -> TestResult {
        assert!(McpServer::new_stdio(stdio_config()).is_ok());
        let mut bad = stdio_config();
        bad.name = String::new();
        assert!(matches!(
            McpServer::new_stdio(bad),
            Err(McpError::InvalidArgument(_))
        ));
        let mut blank = stdio_config();
        blank.name = "   ".into();
        assert!(McpServer::new_stdio(blank).is_err());
        let mut no_version = stdio_config();
        no_version.version = String::new();
        assert!(McpServer::new_stdio(no_version).is_err());
        let mut no_dot = stdio_config();
        no_dot.version = "123".into();
        assert!(McpServer::new_stdio(no_dot).is_err());
        let mut spaces = stdio_config();
        spaces.name = "Test Server".into();
        assert!(McpServer::new_stdio(spaces).is_ok());
        let mut ok = stdio_config();
        ok.version = "1.2.3".into();
        assert!(McpServer::new_stdio(ok).is_ok());

        let empty = StreamableHttpServerConfig::new("");
        assert!(McpServer::new_streamable_http(http_config(), empty).is_err());
        for endpoint in [
            "not-a-url",
            "http://local#host:8080",
            "http://localhost:8080/path#frag",
            "ftp://localhost:8080",
            "http://localhost:99999",
            "http://localhost:0",
            "http://localhost:8080 ",
        ] {
            let cfg = StreamableHttpServerConfig::new(endpoint);
            assert!(
                McpServer::new_streamable_http(http_config(), cfg).is_err(),
                "{endpoint}"
            );
        }
        for endpoint in [
            "http://localhost:8080",
            "https://127.0.0.1:8001/mcp",
            "http://example.com:443/path?x=1",
            "http://localhost",
            "https://example.com/mcp",
        ] {
            let cfg = StreamableHttpServerConfig::new(endpoint);
            assert!(
                McpServer::new_streamable_http(http_config(), cfg).is_ok(),
                "{endpoint}"
            );
        }
        let mut threads = http_config();
        threads.worker_threads = 65;
        assert!(McpServer::new_streamable_http(threads, transport_config()?).is_err());
        let mut io = transport_config()?;
        io.io_threads = 65;
        assert!(McpServer::new_streamable_http(http_config(), io).is_err());
        let mut zero_io = transport_config()?;
        zero_io.io_threads = 0;
        assert!(McpServer::new_streamable_http(http_config(), zero_io).is_err());
        Ok(())
    }

    #[test]
    fn tls_validation() -> TestResult {
        assert!(validate_tls_config(&TlsConfig::default()).is_ok());
        let long_name = TlsConfig {
            enabled: true,
            server_name: "a".repeat(254),
            ..Default::default()
        };
        assert!(validate_tls_config(&long_name).is_err());
        let spaces = TlsConfig {
            enabled: true,
            server_name: "bad name".into(),
            ..Default::default()
        };
        assert!(validate_tls_config(&spaces).is_err());
        let traversal = TlsConfig {
            enabled: true,
            server_name: "..evil".into(),
            ..Default::default()
        };
        assert!(validate_tls_config(&traversal).is_err());
        let missing = TlsConfig {
            enabled: true,
            cert_file: "/nonexistent/cert.pem".into(),
            ..Default::default()
        };
        assert!(validate_tls_config(&missing).is_err());
        let tmp = tempfile::NamedTempFile::new()?;
        let present = TlsConfig {
            enabled: true,
            server_name: "example.com".into(),
            cert_file: tmp.path().to_string_lossy().to_string(),
            key_file: tmp.path().to_string_lossy().to_string(),
            ca_file: tmp.path().to_string_lossy().to_string(),
            ..Default::default()
        };
        assert!(validate_tls_config(&present).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_and_registration() -> TestResult {
        let server = McpServer::new_streamable_http(http_config(), transport_config()?)?;
        assert!(!server.is_running());
        assert_eq!(server.state(), ServerState::Init);
        server.stop().await;
        assert!(!server.is_running());
        server.run().await?;
        assert!(server.is_running());
        assert!(server.local_addr().is_some());
        assert!(server.run().await.is_err());
        server.stop().await;
        assert!(!server.is_running());
        assert!(server.run().await.is_err());
        let err = server
            .add_tool("echo", echo, AddToolOptionalParams::default())
            .err_or_fail()?;
        assert_eq!(
            err.to_string(),
            "Cannot perform operation: server has been stopped"
        );
        assert!(server.remove_tool("echo").is_err());
        assert!(
            server
                .add_resource_template("t", "n", AddResourceTemplateOptionalParams::default())
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn registration_api() -> TestResult {
        let server = McpServer::new_stdio(stdio_config())?;
        assert_eq!(
            server
                .add_tool("", echo, AddToolOptionalParams::default())
                .err_or_fail()?
                .to_string(),
            "Tool name cannot be empty"
        );
        server.add_tool(
            "echo",
            echo,
            AddToolOptionalParams {
                description: Some("Echoes back the input message".into()),
                input_schema: Some(json!({"type": "object"})),
                ..Default::default()
            },
        )?;
        server.add_tool("echo", echo, AddToolOptionalParams::default())?;
        for i in 0..3 {
            server.add_tool(&format!("tool_{i}"), echo, AddToolOptionalParams::default())?;
        }
        assert_eq!(server.tools().len(), 4);
        server.remove_tool("echo")?;
        assert!(server.remove_tool("echo").is_err());

        server.add_prompt(
            "test_prompt",
            |_ctx, _name, _args| async { Ok(GetPromptResult::default()) },
            AddPromptOptionalParams {
                description: Some("d".into()),
                arguments: Some(vec![PromptArgument::new("name", "The name", true)]),
                ..Default::default()
            },
        )?;
        assert!(
            server
                .add_prompt(
                    "",
                    |_c, _n, _a| async { Ok(GetPromptResult::default()) },
                    AddPromptOptionalParams::default()
                )
                .is_err()
        );
        server.remove_prompt("test_prompt")?;
        server.remove_prompt("missing")?;

        let read = |_ctx: ServerContext, uri: String| async move {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::Text(crate::types::TextResourceContents {
                    uri,
                    text: "hello".into(),
                    mime_type: Some("text/plain".into()),
                })],
                meta: None,
            })
        };
        server.add_resource(
            "test://resource",
            "Test Resource",
            read,
            AddResourceOptionalParams::default(),
        )?;
        server.add_resource(
            "test://resource",
            "Test Resource",
            read,
            AddResourceOptionalParams::default(),
        )?;
        assert!(
            server
                .add_resource("", "n", read, AddResourceOptionalParams::default())
                .is_err()
        );
        server.remove_resource("test://resource")?;
        server.add_resource_template(
            "test://resource/{id}",
            "Test Template",
            AddResourceTemplateOptionalParams::default(),
        )?;
        server.remove_resource_template("test://resource/{id}")?;
        server.register_set_logging_level_handler(|_level| Ok(()));
        server.add_completion(|_r, _a, _c| async { Ok(CompleteResult::default()) });
        assert!(server.stdio_session().is_none());
        assert_eq!(server.session_count(), 0);
        assert!(server.transport_config().is_none());
        assert_eq!(server.config().name, "TestServer");
        Ok(())
    }
}
