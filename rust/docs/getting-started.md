# Getting started

## Prerequisites

- Rust 1.98.1. The toolchain is pinned in `rust-toolchain.toml` at the repository root, so
  `rustup` installs the right version, `rustfmt`, `clippy` and the `wasm32-unknown-unknown`
  target on first use.
- A C compiler, for the SQLite library bundled by `a4p`.
- For the WebAssembly examples only: Node 18 or newer and `wasm-bindgen-cli` 0.2.128.

Every crate uses Rust edition 2024.

## Layout

The Cargo workspace is rooted at the repository root and follows the repository's
`<Protocol>/<language>-sdk` convention:

| Path | Crate | Purpose |
|------|-------|---------|
| `MCP/rust-sdk` | `mcp-sdk` | Model Context Protocol client and server |
| `A2A/rust-sdk` | `a2a-sdk` | Agent-to-Agent protocol client and server |
| `A4P/rust-sdk` | `a4p` | Agentic authentication, authorization and audit |
| `AgentRegistry/rust-sdk/a2x-common` | `a2x-common` | Shared registry models, paths, lease table, LLM client |
| `AgentRegistry/rust-sdk/a2x-registry` | `a2x-registry` | Registry backend, HTTP API and CLIs |
| `AgentRegistry/rust-sdk/a2x-search` | `a2x-search` | Taxonomy build, hierarchical, vector and traditional search |
| `AgentRegistry/rust-sdk/a2x-cluster` | `a2x-cluster` | Registry replication between nodes |
| `AgentRegistry/rust-sdk/a2x-registry-client` | `a2x-registry-client` | Client SDK and CLI for the registry |
| `rust/ap-support` | `ap-support` | Error macros, environment sources, test helpers, logging |
| `rust/ap-jsonrpc` | `ap-jsonrpc` | JSON-RPC 2.0 messages and SSE parsing shared by MCP and A2A |
| `rust/agent-protocol-wasm` | `agent-protocol-wasm` | WebAssembly bindings |

## Building and checking

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

The workspace denies `clippy::unwrap_used`, `clippy::expect_used` and `clippy::panic`, and forbids
`unsafe` code, in every crate including tests. A new `unwrap()` fails `cargo clippy`; see
[Error handling](error-handling.md) for what to write instead.

The tests run offline: they use in-process servers on ephemeral ports, fake LLM backends and
in-memory stores. The test build is large; on machines with little disk, point `CARGO_TARGET_DIR`
at a volume with 20 GB free or test crate by crate (`cargo test -p a4p`).

## Using a crate from another project

Add the crate by path (or by git once published):

```toml
[dependencies]
mcp-sdk = { path = "../agent-protocol/MCP/rust-sdk" }
a4p = { path = "../agent-protocol/A4P/rust-sdk" }
```

Every crate re-exports what a program needs at its root or under a `prelude` module; the crate
README lists the entry points. All network APIs are `async` and expect a `tokio` runtime.

## Feature flags

| Crate | Feature | Default | Effect |
|-------|---------|---------|--------|
| `a4p` | `http-server` | on | Axum HTTP facade (`A4PHTTPServer`) |
| `a4p` | `client` | on | `A4PClient` over `reqwest` |
| `a4p` | `sqlite` | on | SQLite intent-token usage store |
| `a2x-registry` | `search` | on | Real search and build engines from `a2x-search` |
| `a2x-registry` | `cluster` | on | Cluster replication and `cluster` CLI subcommands |
| `ap-support` | `logging` | off | `ap_support::logging` (subscriber setup) |
| `ap-support` | `cli` | off | `ap_support::logging::LoggingArgs` for `clap` |

With `--no-default-features`, the `a4p` core (mandates, tokens, scope matching, user signatures)
has no native dependency and compiles for `wasm32-unknown-unknown`.

## Running the examples

Each crate keeps runnable examples under `examples/`, ported from the original SDKs. They are
listed with their commands in the crate READMEs and in the SDK pages of this manual. Examples
that need a server print the command for the matching server example.
