# Porting conventions

The Rust crates in this repository are ports of the C++ and Python SDKs beside them (MCP C++,
A2A C++, A2X registry Python, A4P Python). This document records the rules every crate in the
workspace follows, so the ports stay consistent.

## Workspace layout

| Crate | Path | Replaces |
|-------|------|----------|
| `ap-jsonrpc` | `rust/ap-jsonrpc` | JSON-RPC 2.0 and SSE helpers duplicated in the MCP and A2A C++ SDKs |
| `mcp-sdk` | `MCP/rust-sdk` | `MCP/cpp-sdk` |
| `a2a-sdk` | `A2A/rust-sdk` | `A2A/cpp-sdk` |
| `a4p` | `A4P/rust-sdk` | `A4P` (Python) |
| `a2x-common` | `AgentRegistry/rust-sdk/a2x-common` | `AgentRegistry/a2x_registry/common` |
| `a2x-search` | `AgentRegistry/rust-sdk/a2x-search` | `AgentRegistry/a2x_registry/{a2x,vector,traditional}` |
| `a2x-cluster` | `AgentRegistry/rust-sdk/a2x-cluster` | `AgentRegistry/a2x_registry/cluster` |
| `a2x-registry` | `AgentRegistry/rust-sdk/a2x-registry` | `AgentRegistry/a2x_registry/{register,auth,heartbeat,backend}` and the `a2x-registry` CLI |
| `a2x-registry-client` | `AgentRegistry/rust-sdk/a2x-registry-client` | `AgentRegistry/client` |
| `agent-protocol-wasm` | `rust/agent-protocol-wasm` | new: wasm-bindgen surface over `ap-jsonrpc` and the `a4p` core |

The React front end under `AgentRegistry/ui` is not part of the Rust port. It is a TypeScript
application that talks to the backend over HTTP and can be served unchanged by the Rust backend.

## Fidelity rules

1. **Wire compatibility is mandatory.** JSON field names, JSON-RPC method names, error codes and
   messages, HTTP paths, status codes, headers and file formats on disk must match the original.
   A Rust client must interoperate with the original C++ or Python server and vice versa.
2. **Public API is idiomatic Rust, named after the original.** A C++ `std::future<T>` plus
   exceptions becomes `async fn -> Result<T, Error>`. Abstract classes become traits. Optional
   parameter structs become builder-style config structs with `Default`. Keep names close to the
   source so readers can map `McpClient::ListTools` to `McpClient::list_tools`.
3. **Behaviour is ported, not reinterpreted.** Edge cases, validation order, default values,
   timeouts and error precedence follow the original code. When the original has a documented
   limitation, keep it and document it.
4. **Every crate has a README** with a table mapping the original API to the Rust API, a quick
   start, and a list of anything intentionally left out.

## Technology choices

| Concern | Choice |
|---------|--------|
| Async runtime | `tokio` |
| HTTP server | `axum` (TLS via `axum-server` with rustls) |
| HTTP client | `reqwest` with rustls, streaming bodies for SSE |
| JSON | `serde` and `serde_json` |
| Errors | `thiserror` enums per crate; no panics on network or user input |
| Logging | `tracing`; the original log callback APIs map to `tracing` subscribers |
| IDs | `uuid` v4 |
| Time | `chrono` for wall-clock text, `std::time::Instant` for monotonic countdowns |

Shared dependencies are pinned in the root `Cargo.toml` under `[workspace.dependencies]`.

## Error handling and logging

No Rust source in the workspace, tests included, contains `unwrap()`, `expect()`, `panic!`,
`unreachable!` or `unsafe`. The workspace lints deny `clippy::unwrap_used`,
`clippy::expect_used` and `clippy::panic` and forbid unsafe code, so `cargo clippy --all-targets`
fails on a new occurrence. Assertions (`assert!`, `assert_eq!`, `matches!`) are the accepted way
for a test to stop on a failed expectation.

Patterns to reach for instead of a panic:

- Propagate with `?` into the crate's `thiserror` enum, adding a variant when none fits, or use
  the `ap-support` macros `bail!`, `ensure!`, `some_or_bail!` and `ok_or_bail!`.
- `let Some(x) = ... else { return Err(...) }` where a lookup was "checked above".
- `parking_lot` mutexes, whose guards cannot be poisoned, instead of `std::sync::Mutex`.
- `checked_*` arithmetic and `Option` chains for arithmetic and date conversions.
- Plain string scanning instead of a `Regex::new(...)` that would have to be unwrapped.
- `try_or_log!(result, "context", fallback)` on best-effort paths that must not stop the caller.
- Binaries print the error to stderr and exit with a non-zero status; they never unwrap.
- Configuration is read through `ap_support::env::current()`, never `std::env` directly, and
  every reader has an `_in(env)` variant so tests inject a `MapEnv` and never write to the
  process environment. Each crate declares the variables it reads in an `env_policy()` with
  their kinds, defaults and safety rules; binaries flatten `ap_support::env::EnvArgs` and call
  `apply_or_exit` so dangerous values are refused (`restricted`) or stop the process (`locked`).

Tests return `ap_support::testing::TestResult` and use `?`, `.required()`, `.err_or_fail()`,
`some!` and `err!`; fixtures return `TestResult<Fixture>`; mocks that implement a trait method
without an error path return the trait's own error type or a documented default. See
[docs/testing.md](docs/testing.md).

Logging goes through `tracing`. Libraries emit events and never install a subscriber. Binaries
flatten `ap_support::logging::LoggingArgs` into their `clap` parser and call `init()` once, which
gives every tool the same `--log-level`, `--log-format`, `--log-filter`, `-v`, `--quiet` and
`--log-target` flags plus the `AP_LOG_LEVEL`, `AP_LOG_FORMAT` and `RUST_LOG` variables.
Examples call `ap_support::logging::init_from_env()`. Command-line tools that produce output for
a user print that output with `println!`; everything diagnostic goes to `tracing`. See
[docs/logging.md](docs/logging.md).

## Lints

`cargo clippy --workspace --all-targets -- -D warnings` passes without any `#[allow]` or
`#![allow]` attribute in the workspace. A finding is fixed at its cause: a large enum variant is
boxed, a function with too many parameters takes a request struct, an unused field or function
is removed, and shared test helper modules are declared `pub mod common;` so every test binary
counts them as exported rather than dead. Do not add lint suppressions.

## Code style

- `cargo fmt` with the repository `rustfmt.toml`.
- `cargo clippy --all-targets -- -D warnings` must pass for every crate.
- Documentation comments are English. Sentences stay under 40 words. Do not use em dashes.
- Wire structs use `#[serde(rename_all = "camelCase")]` where the protocol is camelCase (MCP,
  A2A, A4P) and plain snake_case where the protocol is snake_case (A2X HTTP API).
- Optional wire fields are `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.

## Tests

- Port the intent of every original test file. Unit tests live next to the code, integration
  tests in `tests/`.
- Tests run offline. Network tests bind an in-process server on `127.0.0.1:0`.
- Tests avoid real sleeps longer than a few milliseconds. Time-based state machines take an
  explicit `now`.
- `cargo test -p <crate>` must pass.

## Examples

Original example programs are ported as cargo examples in the crate's `examples/` directory and listed in
the crate README with the command to run them.

## File headers

Every authored source file (Rust, scripts, examples, CI) starts with SPDX headers:

```rust
// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0
```

Files copied unchanged from the original SDKs, such as the A4P authorizer web assets, keep their
original headers.
