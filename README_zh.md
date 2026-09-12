# Agent Protocol

[English](./README.md) | 简体中文

## 简介

**Agent Protocol** 是一套智能体互操作与授权协议的软件开发工具包（SDK）集合，帮助开发者在不同运行环境中快速集成智能体与工具/智能体之间的互操作能力，并建立可验证的授权边界。

当前，仓库包含四个软件开发工具包：

- Model Context Protocol (MCP) CPP SDK
- Agent2Agent Protocol (A2A) CPP SDK
- A2X agent registry
- A4P (Agentic Authentication, Authorization, and Audit Protocol) Python SDK

每个 SDK 还提供 Rust 实现，线路格式与原实现一致，Rust 客户端与服务端可与 C++ 和 Python 实现互通：

- [MCP Rust SDK](./MCP/rust-sdk)
- [A2A Rust SDK](./A2A/rust-sdk)
- [A2X 注册中心 Rust 实现](./AgentRegistry/rust-sdk)（注册中心后端、分类树构建与搜索、集群同步、客户端 SDK）
- [A4P Rust SDK](./A4P/rust-sdk)
- [WebAssembly 绑定](./rust/agent-protocol-wasm)，用于浏览器和 Node（JSON-RPC 与 SSE 工具、A4P User Authorizer）

Rust crate 组成一个以本目录为根的 Cargo workspace，概览见 [rust/README.md](./rust/README.md)，使用手册见 [rust/docs](./rust/docs/README.md)。

## 参与贡献

我们欢迎所有形式的贡献，包括但不限于:

- 提交问题和功能建议
- 改进文档
- 提交代码
- 分享使用经验

## 开源许可证

本项目依据Apache-2.0许可证授权。

本产品仅作为流程编排工具，不包含 AI 模型能力；用户在连接 AI 模型用于特定业务场景时，需自行承担欧盟 AI 法案等相关合规义务。
