# Agent Protocol Rust SDKs: usage documentation

This directory is the usage manual for the Rust implementations of the agent-protocol SDKs.
Each crate also has a README with its API mapping to the original C++ or Python implementation;
these pages explain how to use the crates together, how to configure and run the binaries, and the
conventions every crate follows.

| Page | Content |
|------|---------|
| [Getting started](getting-started.md) | Toolchain, building, testing, workspace layout, feature flags |
| [MCP SDK](mcp.md) | Model Context Protocol servers and clients over Streamable HTTP and stdio |
| [A2A SDK](a2a.md) | Agent-to-Agent servers and clients, streaming, agent cards |
| [A4P SDK](a4p.md) | Mandates, intent tokens, user signatures, the HTTP facade and client |
| [Registry, search and cluster](registry.md) | The A2X registry backend, taxonomy search, cluster replication and the client SDK |
| [WebAssembly bindings](wasm.md) | JSON-RPC, SSE and the A4P User Authorizer in browsers and Node |
| [Command-line tools](cli.md) | Every binary, its subcommands and shared flags |
| [Logging](logging.md) | Levels, formats, filters, environment variables and how libraries log |
| [Error handling](error-handling.md) | Error types, the no-panic policy and the `ap-support` macros |
| [Configuration](configuration.md) | The environment lockdown, every environment variable grouped by crate, and how to inject them in code |
| [Testing](testing.md) | The `TestResult` style, helpers, mock servers and running the suites |

Related documents outside this directory:

- [rust/README.md](../README.md): crate overview and status.
- [rust/PORTING_CONVENTIONS.md](../PORTING_CONVENTIONS.md): the rules the ports follow.
- Crate READMEs: [MCP](../../MCP/rust-sdk/README.md), [A2A](../../A2A/rust-sdk/README.md),
  [A4P](../../A4P/rust-sdk/README.md), [a2x-registry](../../AgentRegistry/rust-sdk/a2x-registry/README.md),
  [a2x-search](../../AgentRegistry/rust-sdk/a2x-search/README.md),
  [a2x-cluster](../../AgentRegistry/rust-sdk/a2x-cluster/README.md),
  [a2x-registry-client](../../AgentRegistry/rust-sdk/a2x-registry-client/README.md),
  [agent-protocol-wasm](../agent-protocol-wasm/README.md), [ap-support](../ap-support/src/lib.rs).
