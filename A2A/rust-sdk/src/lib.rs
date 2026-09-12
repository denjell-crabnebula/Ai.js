// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Agent-to-Agent (A2A) protocol v1.0 client and server SDK.
//!
//! This crate is a Rust port of the `A2A/cpp-sdk` of the
//! openJiuwen-ai `agent-protocol` monorepo. It speaks JSON-RPC 2.0 over HTTP
//! with Server-Sent Events for streaming and interoperates with the C++ SDK.
//!
//! - [`types`] holds the protocol data types with their exact wire format.
//! - [`client`] offers [`client::ClientFactory`], the [`client::Client`] trait
//!   and the HTTP agent card resolver.
//! - [`server`] offers [`server::HttpServerBuilder`], the
//!   [`server::AgentExecutor`] trait and the task machinery.
//!
//! See the crate README for a quick start and the API mapping table.

pub mod client;
pub mod error;
pub mod log;
pub mod protocol;
pub mod server;
pub mod types;
pub mod utils;

pub use error::{A2AErrorCode, A2aClientError, A2aServerError};
pub use types::*;
