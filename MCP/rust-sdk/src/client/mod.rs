// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! MCP client, port of `mcp_client.h`, `mcp_client_implement.*` and the
//! client factory.

pub mod session;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;

pub use session::{
    ClientSession, ElicitCallback, ElicitUrlCallback, ListRootsCallback, LoggingCallback,
    SamplingCreateMessageCallback,
};

use crate::auth::AuthProvider;
use crate::error::McpError;
use crate::protocol::is_valid_streamable_http_endpoint;
use crate::session::ProgressCallback;
use crate::transport::{ClientTransport, StdioClientTransport, StreamableHttpClientTransport};
use crate::types::{
    CallToolResult, ClientConfig, CompleteReference, CompleteResult, CompletionArgument, CompletionContext,
    EmptyResult, GetPromptResult, InitializeResult, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, LoggingLevel, ProgressToken, ReadResourceResult,
    SamplingCapability, ServerCapabilities, StdioClientConfig, StreamableHttpClientConfig,
};

#[derive(Default)]
struct PreInitCallbacks {
    list_roots: Option<ListRootsCallback>,
    logging: Option<LoggingCallback>,
    elicit: Option<ElicitCallback>,
    elicit_url: Option<ElicitUrlCallback>,
    sampling: Option<(SamplingCreateMessageCallback, SamplingCapability)>,
}

/// MCP client (`Mcp::McpClient`).
///
/// Create it with [`McpClientFactory`], register callbacks, call
/// [`McpClient::initialize`] and then use the RPC methods. Every RPC method
/// fails with [`McpError::NotInitialized`] before `initialize`.
pub struct McpClient {
    config: ClientConfig,
    transport: Arc<dyn ClientTransport>,
    session: Mutex<Option<Arc<ClientSession>>>,
    initialized: AtomicBool,
    pre_init: Mutex<PreInitCallbacks>,
}

impl McpClient {
    /// Build a client on a custom transport.
    pub fn with_transport(config: ClientConfig, transport: Arc<dyn ClientTransport>) -> Self {
        Self {
            config,
            transport,
            session: Mutex::new(None),
            initialized: AtomicBool::new(false),
            pre_init: Mutex::new(PreInitCallbacks::default()),
        }
    }

    /// The client configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// True between `initialize` and `close_gracefully`.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    fn session(&self) -> Result<Arc<ClientSession>, McpError> {
        if !self.is_initialized() {
            tracing::error!("client is not initialized.");
            return Err(McpError::NotInitialized);
        }
        self.session
            .lock()
            .clone()
            .ok_or_else(|| McpError::state("client session is not created."))
    }

    /// Connect the transport and perform the `initialize` handshake.
    ///
    /// Fails with [`McpError::AlreadyInitialized`] on a second call.
    pub async fn initialize(&self) -> Result<InitializeResult, McpError> {
        if self.is_initialized() {
            return Err(McpError::AlreadyInitialized);
        }
        self.transport.connect().await?;
        let session = ClientSession::new(self.transport.clone(), self.config.clone());
        {
            let mut pre = self.pre_init.lock();
            if let Some(cb) = pre.list_roots.take() {
                session.set_list_roots_callback(cb);
            }
            if let Some(cb) = pre.logging.take() {
                session.set_logging_callback(cb);
            }
            if let Some(cb) = pre.elicit.take() {
                session.set_elicit_callback(cb);
            }
            if let Some(cb) = pre.elicit_url.take() {
                session.set_elicit_url_callback(cb);
            }
            if let Some((cb, capability)) = pre.sampling.take() {
                session.set_sampling_create_message_callback(cb, capability);
            }
        }
        *self.session.lock() = Some(session.clone());
        // Like the C++ SDK the client counts as initialized once the handshake starts.
        self.initialized.store(true, Ordering::SeqCst);
        session.initialize().await
    }

    /// `tools/list`
    pub async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, McpError> {
        self.session()?.list_tools(cursor).await
    }

    /// `tools/call`. `timeout_ms == 0` means the default timeout. With a
    /// progress callback the request carries `params._meta.progressToken`.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<Value>,
        timeout_ms: u64,
        progress_callback: Option<ProgressCallback>,
    ) -> Result<CallToolResult, McpError> {
        self.session()?
            .call_tool(name, arguments, timeout_ms, progress_callback)
            .await
    }

    /// `resources/list`
    pub async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, McpError> {
        self.session()?.list_resources(cursor).await
    }

    /// `resources/read`
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        self.session()?.read_resource(uri).await
    }

    /// `resources/subscribe`
    pub async fn subscribe_resource(&self, uri: &str) -> Result<EmptyResult, McpError> {
        self.session()?.subscribe_resource(uri).await
    }

    /// `resources/unsubscribe`
    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<EmptyResult, McpError> {
        self.session()?.unsubscribe_resource(uri).await
    }

    /// `resources/templates/list`
    pub async fn list_resources_templates(&self) -> Result<ListResourceTemplatesResult, McpError> {
        self.session()?.list_resource_templates().await
    }

    /// `prompts/list`
    pub async fn list_prompts(&self) -> Result<ListPromptsResult, McpError> {
        self.session()?.list_prompts().await
    }

    /// `prompts/get`
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<GetPromptResult, McpError> {
        self.session()?.get_prompt(name, arguments).await
    }

    /// `ping`
    pub async fn send_ping(&self) -> Result<EmptyResult, McpError> {
        self.session()?.send_ping().await
    }

    /// Terminate the remote session, close the transport and forget the session.
    /// Does nothing before `initialize`. Never fails.
    pub async fn close_gracefully(&self) {
        if !self.is_initialized() {
            return;
        }
        self.transport
            .terminate_session(Duration::from_millis(1000))
            .await;
        self.transport.terminate().await;
        *self.session.lock() = None;
        self.initialized.store(false, Ordering::SeqCst);
        tracing::info!("Client closed gracefully.");
    }

    /// `notifications/roots/list_changed`
    pub async fn send_roots_list_changed(&self) -> Result<(), McpError> {
        self.session()?.send_roots_list_changed().await
    }

    /// `logging/setLevel`
    pub async fn set_logging_level(&self, level: LoggingLevel) -> Result<EmptyResult, McpError> {
        self.session()?.set_logging_level(level).await
    }

    /// Capabilities the server advertised in `initialize`.
    pub fn get_server_capabilities(&self) -> Result<ServerCapabilities, McpError> {
        Ok(self.session()?.server_capabilities())
    }

    /// `notifications/progress`
    pub async fn send_progress_notification(
        &self,
        progress_token: ProgressToken,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Result<(), McpError> {
        self.session()?
            .send_progress_notification(progress_token, progress, total, message)
            .await
    }

    /// `completion/complete`
    pub async fn complete(
        &self,
        reference: CompleteReference,
        argument: CompletionArgument,
        context: Option<CompletionContext>,
    ) -> Result<CompleteResult, McpError> {
        self.session()?.complete(reference, argument, context).await
    }

    /// Register the `roots/list` callback. Call it before `initialize` so the
    /// `roots` capability is advertised.
    pub fn set_list_roots_callback(&self, cb: ListRootsCallback) {
        match self.session.lock().as_ref() {
            Some(session) => session.set_list_roots_callback(cb),
            None => self.pre_init.lock().list_roots = Some(cb),
        }
    }

    /// Register the `notifications/message` callback.
    pub fn set_logging_callback(&self, cb: LoggingCallback) {
        match self.session.lock().as_ref() {
            Some(session) => session.set_logging_callback(cb),
            None => self.pre_init.lock().logging = Some(cb),
        }
    }

    /// Register the form mode `elicitation/create` callback.
    pub fn set_elicit_callback(&self, cb: ElicitCallback) {
        match self.session.lock().as_ref() {
            Some(session) => session.set_elicit_callback(cb),
            None => self.pre_init.lock().elicit = Some(cb),
        }
    }

    /// Register the url mode `elicitation/create` callback.
    pub fn set_elicit_url_callback(&self, cb: ElicitUrlCallback) {
        match self.session.lock().as_ref() {
            Some(session) => session.set_elicit_url_callback(cb),
            None => self.pre_init.lock().elicit_url = Some(cb),
        }
    }

    /// Register the `sampling/createMessage` callback. Call it before
    /// `initialize` so the `sampling` capability is advertised.
    pub fn set_sampling_create_message_callback(
        &self,
        cb: SamplingCreateMessageCallback,
        capability: SamplingCapability,
    ) {
        match self.session.lock().as_ref() {
            Some(session) => session.set_sampling_create_message_callback(cb, capability),
            None => self.pre_init.lock().sampling = Some((cb, capability)),
        }
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("config", &self.config)
            .field("initialized", &self.is_initialized())
            .finish()
    }
}

/// Factory for clients (`Mcp::McpClientFactory`).
pub struct McpClientFactory;

impl McpClientFactory {
    /// Create a client using the Streamable HTTP transport.
    ///
    /// Fails with [`McpError::InvalidArgument`] for an invalid endpoint or timeout.
    pub fn create_streamable_http_client(
        config: ClientConfig,
        transport_config: StreamableHttpClientConfig,
        auth_provider: Option<Arc<dyn AuthProvider>>,
    ) -> Result<McpClient, McpError> {
        is_valid_streamable_http_endpoint(&transport_config.endpoint).map_err(McpError::InvalidArgument)?;
        let transport = StreamableHttpClientTransport::new(
            transport_config.endpoint,
            transport_config.headers,
            transport_config.timeout,
            transport_config.sse_timeout,
            transport_config.tls_config,
            auth_provider,
        )?;
        Ok(McpClient::with_transport(config, transport))
    }

    /// Create a client using the stdio transport.
    pub fn create_stdio_client(config: ClientConfig, transport_config: StdioClientConfig) -> McpClient {
        McpClient::with_transport(config, StdioClientTransport::new(transport_config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn factory_validates_endpoint() -> TestResult {
        let config = ClientConfig::default();
        let ok = StreamableHttpClientConfig {
            endpoint: "http://127.0.0.1:8001/mcp".into(),
            timeout: Duration::from_millis(1000),
            sse_timeout: Duration::from_millis(1000),
            ..Default::default()
        };
        assert!(McpClientFactory::create_streamable_http_client(config.clone(), ok, None).is_ok());
        for endpoint in [
            "",
            "not-a-url",
            "http://local#host:8080",
            "http://localhost:8080/path#frag",
            "ftp://localhost:8080",
            "http://localhost:99999",
            "http://localhost:8080 ",
        ] {
            let cfg = StreamableHttpClientConfig {
                endpoint: endpoint.into(),
                ..Default::default()
            };
            let err =
                McpClientFactory::create_streamable_http_client(config.clone(), cfg, None).err_or_fail()?;
            assert!(matches!(err, McpError::InvalidArgument(_)), "{endpoint}");
        }
        let bad_timeout = StreamableHttpClientConfig {
            endpoint: "http://localhost:1".into(),
            timeout: Duration::from_millis(0),
            ..Default::default()
        };
        assert!(McpClientFactory::create_streamable_http_client(config, bad_timeout, None).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn methods_before_initialize_fail_and_close_is_noop() -> TestResult {
        let client = McpClientFactory::create_stdio_client(
            ClientConfig::default(),
            StdioClientConfig {
                command: "true".into(),
                ..Default::default()
            },
        );
        assert!(matches!(
            client.list_tools(None).await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.call_tool("t", None, 0, None).await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(client.send_ping().await, Err(McpError::NotInitialized)));
        assert!(matches!(
            client.get_server_capabilities(),
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.send_roots_list_changed().await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.list_resources(None).await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.read_resource("u").await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.subscribe_resource("u").await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.unsubscribe_resource("u").await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.list_resources_templates().await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.list_prompts().await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.get_prompt("p", None).await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client.set_logging_level(LoggingLevel::Info).await,
            Err(McpError::NotInitialized)
        ));
        assert!(matches!(
            client
                .send_progress_notification(ProgressToken::from(""), 0.0, None, None)
                .await,
            Err(McpError::NotInitialized)
        ));
        let reference = CompleteReference::Prompt(crate::types::PromptReference { name: String::new() });
        assert!(matches!(
            client
                .complete(reference, CompletionArgument::default(), None)
                .await,
            Err(McpError::NotInitialized)
        ));
        client.close_gracefully().await;
        assert!(!client.is_initialized());
        // Callbacks may be registered before initialize.
        client.set_logging_callback(Arc::new(|_, _, _| {}));
        client.set_list_roots_callback(Arc::new(|| Box::pin(async { Ok(Default::default()) })));
        client.set_elicit_callback(Arc::new(|_, _| Box::pin(async { Ok(Default::default()) })));
        client.set_elicit_url_callback(Arc::new(|_, _, _| Box::pin(async { Ok(Default::default()) })));
        client.set_sampling_create_message_callback(
            Arc::new(|_| Box::pin(async { Ok(None) })),
            SamplingCapability::default(),
        );
        Ok(())
    }

    #[tokio::test]
    async fn initialize_against_unreachable_http_server_fails_and_double_initialize_is_rejected() -> TestResult
    {
        let cfg = StreamableHttpClientConfig {
            endpoint: "http://127.0.0.1:1/mcp".into(),
            timeout: Duration::from_millis(500),
            sse_timeout: Duration::from_millis(500),
            ..Default::default()
        };
        let client = McpClientFactory::create_streamable_http_client(ClientConfig::default(), cfg, None)?;
        let err = client.initialize().await.err_or_fail()?;
        assert_eq!(err.code(), Some(-32603));
        assert!(err.message().starts_with("HTTP request failed"));
        assert!(matches!(
            client.initialize().await,
            Err(McpError::AlreadyInitialized)
        ));
        client.close_gracefully().await;
        assert!(matches!(
            client.list_tools(None).await,
            Err(McpError::NotInitialized)
        ));
        Ok(())
    }
}
