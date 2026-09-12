// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client SDK and CLI for the A2X agent service registry.
//!
//! Port of the Python `a2x_registry_client` package (`AgentRegistry/client`).
//!
//! - [`A2xRegistryClient`]: async client, one method per Python method.
//! - [`blocking::A2xRegistryClient`]: synchronous wrapper with a private runtime.
//! - [`ClientError`]: one enum mirroring the Python exception hierarchy.
//! - [`auth`]: `cli_token.json` IO and credential precedence.
//! - [`ownership`]: local record of the services this client registered.
//! - [`heartbeat`]: background lease renewal tasks.
//! - [`cli`]: the `a2x-registry-client` command line.
//!
//! Wire compatibility with the Python backend is mandatory: paths, JSON field
//! names, headers and status code handling match the original SDK.

#![warn(missing_docs)]
// `ClientError` mirrors a rich exception hierarchy and carries the response

pub mod auth;
pub mod blocking;
pub mod cli;
pub mod client;
pub mod errors;
pub mod heartbeat;
pub mod internal;
pub mod models;
pub mod ownership;
pub mod transport;

pub use auth::{
    CliToken, DEFAULT_BASE_URL, default_config_path, read_cli_token, remove_cli_token, resolve_credentials,
    write_cli_token,
};
pub use client::{
    A2xRegistryClient, ClientConfig, CreateDatasetOptions, DEFAULT_TIMEOUT, ListOptions, OwnershipFile,
    RegisterOptions, ReserveOptions, ShutdownOptions,
};
pub use errors::ClientError;
pub use heartbeat::{HeartbeatFn, HeartbeatRegistry, HeartbeatRenewer};
pub use internal::Formats;
pub use models::{
    AgentDetail, DatasetCreateResponse, DatasetDeleteResponse, DeregisterResponse, JsonObject, PatchResponse,
    PrincipalCreateResponse, RegisterResponse, Reservation, ShutdownReport,
};
pub use ownership::OwnershipStore;
pub use transport::{HttpMethod, Response, Transport};

/// Crate version, mirrors `a2x_registry_client.__version__`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
