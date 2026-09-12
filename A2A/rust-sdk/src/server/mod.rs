// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2A server: HTTP server, request handling and task management.
//!
//! Build a server with [`HttpServerBuilder::build`], implement
//! [`AgentExecutor`] and drive task progress through [`TaskUpdater`].

pub mod builder;
pub mod call_context;
pub mod emitter;
pub mod executor;
pub mod http_transport;
pub mod jsonrpc_handler;
pub mod push_notification;
pub mod request_context;
pub mod request_handler;
pub mod server_impl;
pub mod task_manager;
pub mod task_store;
pub mod task_updater;

pub use builder::{HttpConfig, HttpServerBuilder};
pub use call_context::{RequestContextParam, ServerCallContext};
pub use emitter::{StreamServerEmitter, TransportEmitter};
pub use executor::AgentExecutor;
pub use http_transport::{
    HttpServerTransport, ServerTransport, ServerTransportCardHandler, ServerTransportRpcHandler,
};
pub use jsonrpc_handler::JsonRpcHandler;
pub use push_notification::{
    InMemoryPushNotificationConfigStore, PushNotificationConfigStore, PushNotificationSender,
};
pub use request_context::RequestContext;
pub use request_handler::{DefaultRequestHandler, RequestHandler, StreamEmitter};
pub use server_impl::{Server, ServerImpl};
pub use task_manager::{EventCb, TaskExecuteInfo, TaskManager};
pub use task_store::{InMemoryTaskStore, TaskStore};
pub use task_updater::{TaskArtifactParam, TaskUpdater, TaskUpdaterImpl};
