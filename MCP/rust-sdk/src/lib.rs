// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Rust port of the MCP C++ SDK (`MCP/cpp-sdk` of openJiuwen-ai/agent-protocol).
//!
//! The crate provides an MCP client ([`McpClient`]) and server ([`McpServer`])
//! over two transports: Streamable HTTP and stdio. Wire formats, method names,
//! error codes and HTTP status codes follow the original so a Rust peer can
//! talk to the C++ implementation.
//!
//! # Quick start
//!
//! ```no_run
//! use mcp_sdk::prelude::*;
//!
//! # async fn run() -> Result<(), McpError> {
//! let client = McpClientFactory::create_streamable_http_client(
//!     ClientConfig::default(),
//!     StreamableHttpClientConfig { endpoint: "http://127.0.0.1:8000/mcp".into(), ..Default::default() },
//!     None,
//! )?;
//! client.initialize().await?;
//! let tools = client.list_tools(None).await?;
//! println!("{} tools", tools.tools.len());
//! client.close_gracefully().await;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod error;
pub mod log;
pub mod protocol;
pub mod sampling_validation;
pub mod schema;
pub mod server;
pub mod session;
pub mod transport;
pub mod types;

pub use ap_jsonrpc::{self, Message, Notification, Request, RequestId, Response, RpcError};

pub use client::{McpClient, McpClientFactory};
pub use error::{ErrorResult, JsonRpcErrorCode, McpError};
pub use server::{McpServer, McpServerFactory, ServerContext};
pub use types::*;

/// Commonly used items.
pub mod prelude {
    pub use crate::auth::{
        AuthProvider, Authenticator, Authorizer, BearerTokenAuthenticator, BearerTokenProvider,
        NoAuthAuthenticator, ScopeBasedAuthorizer, SimpleTokenVerifier, TokenVerifier,
    };
    pub use crate::client::{
        ElicitCallback, ElicitUrlCallback, ListRootsCallback, LoggingCallback, McpClient, McpClientFactory,
        SamplingCreateMessageCallback,
    };
    pub use crate::error::{JsonRpcErrorCode, McpError};
    pub use crate::server::{
        AddPromptOptionalParams, AddResourceOptionalParams, AddResourceTemplateOptionalParams,
        AddToolOptionalParams, McpServer, McpServerFactory, ServerContext,
    };
    pub use crate::session::ProgressCallback;
    pub use crate::types::*;
}
