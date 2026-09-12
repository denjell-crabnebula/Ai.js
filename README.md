# Agent Protocol

English | [简体中文](./README_zh.md)

## Introduction

**Agent Protocol** is a collection of software development kits (SDKs) for agent interoperability and authorization protocols. It helps developers quickly integrate agents, enable agent-to-tool / agent-to-agent interoperability, and establish verifiable authorization boundaries across different runtime environments.

This repository currently includes four SDKs:

- Model Context Protocol (MCP) CPP SDK
- Agent2Agent Protocol (A2A) CPP SDK
- A2X agent registry
- A4P (Agentic Authentication, Authorization, and Audit Protocol) Python SDK

Each SDK also has a Rust implementation with the same wire format, so Rust clients and servers interoperate with the C++ and Python ones:

- [MCP Rust SDK](./MCP/rust-sdk)
- [A2A Rust SDK](./A2A/rust-sdk)
- [A2X agent registry in Rust](./AgentRegistry/rust-sdk) (registry backend, taxonomy build and search, cluster replication, client SDK)
- [A4P Rust SDK](./A4P/rust-sdk)
- [WebAssembly bindings](./rust/agent-protocol-wasm) for browsers and Node (JSON-RPC and SSE helpers, A4P User Authorizer)

The Rust crates form one Cargo workspace rooted at this directory. See [rust/README.md](./rust/README.md) for an overview and [rust/docs](./rust/docs/README.md) for the usage manual.

## Contributing

We welcome all forms of contribution, including but not limited to:

- Reporting issues and suggesting features
- Improving documentation
- Submitting code
- Sharing usage experience

## License

This project is licensed under the Apache-2.0 License.

This product serves solely as a workflow orchestration tool and does not embed any AI model capabilities. When users integrate AI models for specific business scenarios, they shall bear full responsibility for compliance obligations under the EU AI Act and other relevant regulatory frameworks.
