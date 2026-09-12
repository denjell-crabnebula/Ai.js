# Agent Protocol Rust SDKs

Rust implementations of the four agent-protocol SDKs, living next to the C++ and Python
implementations they port: the MCP SDK, the A2A SDK, the A2X agent registry and the A4P
authorization SDK. All crates form one Cargo workspace rooted at the repository root. Each SDK
directory follows the repository's `<Protocol>/<language>-sdk` convention; shared crates, the
WebAssembly bindings and these documents live under `rust/`.

| Crate | Protocol | Original |
|-------|----------|----------|
| [`mcp-sdk`](../MCP/rust-sdk) | Model Context Protocol client and server, Streamable HTTP and stdio transports | `MCP/cpp-sdk` |
| [`a2a-sdk`](../A2A/rust-sdk) | Agent-to-Agent protocol v1.0 client and server over HTTP JSON-RPC with SSE streaming | `A2A/cpp-sdk` |
| [`a4p`](../A4P/rust-sdk) | Agentic Authentication, Authorization and Audit Protocol: mandates, intent tokens, user signatures | `A4P` |
| [`a2x-registry`](../AgentRegistry/rust-sdk/a2x-registry) | A2X agent service registry backend and CLI (registration, auth, heartbeat leases, HTTP API) | `AgentRegistry/a2x_registry` |
| [`a2x-search`](../AgentRegistry/rust-sdk/a2x-search) | A2X hierarchical taxonomy build and LLM-navigated search, vector and traditional baselines, evaluation CLIs | `AgentRegistry/a2x_registry/{a2x,vector,traditional}` |
| [`a2x-cluster`](../AgentRegistry/rust-sdk/a2x-cluster) | Registry cluster replication: full-mesh gossip, LWW envelopes, Merkle anti-entropy | `AgentRegistry/a2x_registry/cluster` |
| [`a2x-registry-client`](../AgentRegistry/rust-sdk/a2x-registry-client) | Client SDK and CLI for the registry | `AgentRegistry/client` |
| [`a2x-common`](../AgentRegistry/rust-sdk/a2x-common) | Shared registry models, path resolution, lease table, multi-provider LLM client | `AgentRegistry/a2x_registry/common` |
| [`ap-jsonrpc`](../rust/ap-jsonrpc) | JSON-RPC 2.0 message model and SSE parser shared by MCP and A2A | internal to both C++ SDKs |
| [`ap-support`](../rust/ap-support) | Error-handling macros, environment sources, test helpers and logging setup shared by every crate | new |
| [`agent-protocol-wasm`](../rust/agent-protocol-wasm) | WebAssembly bindings: JSON-RPC and SSE wire helpers plus the A4P User Authorizer for browsers and Node | new |

## Status

Every crate builds, passes `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
`cargo doc` with warnings denied, and its test suite. The workspace runs 788 tests offline.

The whole workspace, tests included, contains no `unwrap`, `expect`, `panic!` or `unsafe`:
`clippy::unwrap_used`, `clippy::expect_used` and `clippy::panic` are denied and unsafe code is
forbidden through the workspace lints. Tests return `TestResult` and propagate failures with `?`.
Configuration is read through a typed environment parser with a lockdown policy: every binary
refuses dangerous values such as public bind addresses, plain-http remote URLs, system paths,
out-of-range worker counts and development signing keys, and `--env-lockdown locked` turns any
violation into a refusal to start. See [docs/configuration.md](docs/configuration.md).

| Crate | Rust lines | Tests |
|-------|-----------:|------:|
| `mcp-sdk` | 16.2K | 137 |
| `a2a-sdk` | 13.0K | 102 |
| `a4p` | 13.3K | 120 |
| `a2x-registry` | 15.2K | 104 |
| `a2x-search` | 11.1K | 51 |
| `a2x-cluster` | 7.8K | 106 |
| `a2x-registry-client` | 6.5K | 110 |
| `a2x-common` | 1.6K | 20 |
| `ap-jsonrpc` | 1.0K | 21 |
| `ap-support` | 0.9K | 15 |
| `agent-protocol-wasm` | 0.7K | 2 native, end-to-end Node example |

## Building

The toolchain is pinned to Rust 1.98.1 by `rust-toolchain.toml` (rustup installs it on first use) and every crate
uses edition 2024. A C compiler is needed for the bundled SQLite used by `a4p`.

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The `agent-protocol-wasm` crate additionally builds for `wasm32-unknown-unknown`; see its README for
the JavaScript bindings and the Node and browser examples.

The usage manual lives in [docs/](docs/README.md): getting started, one page per SDK, the
command-line tools, logging, error handling, configuration and testing. Each crate README
documents its API, maps the original C++ or Python symbols to Rust, and lists the example
programs and how to run them.

## Design notes

- Wire compatibility with the original implementations is a hard requirement. JSON field names,
  JSON-RPC methods, error codes, HTTP paths and on-disk file formats are unchanged, so Rust clients
  and servers interoperate with the C++ and Python ones.
- Public APIs are idiomatic Rust: async functions returning `Result`, traits instead of abstract
  classes, config structs with defaults. Names stay close to the originals.
- Networking uses `tokio`, `axum` and `reqwest`; JSON uses `serde`; logging uses `tracing`.

See [docs/PORTING_CONVENTIONS.md](PORTING_CONVENTIONS.md) for the full set of rules.

## What is not ported

- The React web UI under `AgentRegistry/ui`. It is a TypeScript application that talks to the
  backend over HTTP and can be served unchanged by the Rust backend through `A2X_FRONTEND_DIST_DIR`.
- Local sentence-transformers embedding models and ChromaDB. The `a2x-search` crate replaces them
  with an embedding trait, an HTTP embedding backend and an in-memory vector store persisted as JSON.

## License

Apache-2.0, like the rest of the repository. See [LICENSE](../LICENSE). The Rust dependencies are
listed in the repository's [third-party notice](../Third_Party_Open_Source_Software_Notice.txt); regenerate
that section with `python3 rust/tools/third_party_notices.py` after changing dependencies.

The original notice applies here as well: this project serves solely as a workflow orchestration
tool and does not embed any AI model capabilities. Users who integrate AI models for specific
business scenarios bear full responsibility for compliance obligations under the EU AI Act and
other relevant regulatory frameworks.
