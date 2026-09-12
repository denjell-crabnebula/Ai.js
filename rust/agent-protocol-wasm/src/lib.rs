// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! WebAssembly bindings for the agent-protocol SDKs.
//!
//! The crate exposes two groups of functionality to JavaScript through
//! `wasm-bindgen`:
//!
//! - [`jsonrpc`]: JSON-RPC 2.0 message building and parsing plus an
//!   incremental Server-Sent Events parser. These are the wire primitives a
//!   browser or Node MCP or A2A client needs on top of `fetch`.
//! - [`a4p`]: the A4P User Authorizer side. The protocol places this component
//!   on the user's device, so it verifies Server-signed mandates against a
//!   local trust store, derives the WebAuthn challenge, and produces Ed25519
//!   user signatures without any private key leaving the device.
//!
//! JSON values cross the boundary as plain JavaScript objects. Errors are
//! thrown as JavaScript `Error` values whose message starts with the stable
//! A4P code when one exists, for example `MANDATE_EXPIRED: ...`.
//!
//! Build with `cargo build -p agent-protocol-wasm --target wasm32-unknown-unknown --release`
//! followed by `wasm-bindgen`, or run `examples/build.sh`.

pub mod a4p;
mod convert;
pub mod jsonrpc;

use wasm_bindgen::prelude::*;

/// Crate version, useful for checking which build a page loaded.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Install a panic hook that forwards Rust panics to `console.error`.
/// Runs automatically when the module is instantiated in a wasm host.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}
