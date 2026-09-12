// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2A client: configuration, events, transports and the agent card resolver.
//!
//! Create a client with [`ClientFactory::create`] and resolve agent cards
//! with [`HttpCardResolverBuilder::build`].

pub mod card_resolver;
pub mod config;
pub mod default_client;
pub mod factory;
pub mod interceptor;
pub mod jsonrpc_transport;
pub mod task_manager;
pub mod transport;

pub use card_resolver::{A2ACardResolver, HttpCardResolver, HttpCardResolverBuilder};
pub use config::{ClientConfig, ClientEvent, Consumer, ResponseHandler, UpdateEvent};
pub use default_client::{Client, ClientEventStream, DefaultClient, send_message_stream};
pub use factory::ClientFactory;
pub use interceptor::{ClientCallInterceptor, ProtocolVersionInterceptor};
pub use jsonrpc_transport::{JsonRpcTransport, UserData};
pub use task_manager::ClientTaskManager;
pub use transport::{ClientTransport, TransportError, TransportEvent, TransportEventCallback};
