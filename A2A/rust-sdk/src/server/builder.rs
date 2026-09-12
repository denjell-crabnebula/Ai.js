// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! HTTP server configuration and builder, the port of
//! `include/server/http_server_builder.h` and `http_server_builder.cpp`.

use std::sync::Arc;

use crate::a2a_log;
use crate::error::A2aServerError;
use crate::log::A2aLogLevel;
use crate::protocol::{DEFAULT_JSONRPC_ENDPOINT, JSONRPC_TRANSPORT};
use crate::types::AgentCard;

use super::executor::AgentExecutor;
use super::server_impl::{Server, ServerImpl};
use super::task_store::TaskStore;

/// HTTP server configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpConfig {
    /// IP address to bind to. Empty binds all interfaces.
    pub ip: String,
    /// Port to listen on. 0 picks a free port.
    pub port: u16,
    /// Number of I/O threads in the original SDK; informational here.
    pub io_thread_num: u32,
    /// JSON-RPC endpoint path.
    pub endpoint: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            ip: String::new(),
            port: 0,
            io_thread_num: 1,
            endpoint: DEFAULT_JSONRPC_ENDPOINT.to_string(),
        }
    }
}

impl HttpConfig {
    /// Configuration for an address and port with the default endpoint.
    pub fn new(ip: impl Into<String>, port: u16) -> Self {
        HttpConfig {
            ip: ip.into(),
            port,
            ..Default::default()
        }
    }
}

/// Parsed form of the primary interface URL.
#[derive(Debug, PartialEq, Eq)]
struct ParsedUrl {
    port: i64,
    path: Option<String>,
}

/// Parse `^(https?://)?([^:/\s]+):(\d+)(/.*)?$` without a regex engine.
fn parse_interface_url(url: &str) -> Option<ParsedUrl> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let colon = rest.find(':')?;
    let host = &rest[..colon];
    if host.is_empty() || host.contains('/') || host.chars().any(char::is_whitespace) {
        return None;
    }
    let after = &rest[colon + 1..];
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let tail = &after[digits.len()..];
    let path = if tail.is_empty() {
        None
    } else if tail.starts_with('/') {
        Some(tail.to_string())
    } else {
        return None;
    };
    let port = digits.parse::<i64>().unwrap_or(i64::MAX);
    Some(ParsedUrl { port, path })
}

/// Derive the JSON-RPC endpoint from the primary interface URL.
pub fn endpoint_from_url(url: &str) -> Result<String, A2aServerError> {
    match parse_interface_url(url) {
        Some(parsed) => {
            if !(1..=65535).contains(&parsed.port) {
                a2a_log!(A2aLogLevel::Error, "Invalid port in agentCard.url");
                return Err(A2aServerError::new("Invalid port in agentCard.url"));
            }
            Ok(parsed
                .path
                .unwrap_or_else(|| DEFAULT_JSONRPC_ENDPOINT.to_string()))
        }
        None => {
            let endpoint = match url.rfind('/') {
                Some(pos) => url[pos..].to_string(),
                None => DEFAULT_JSONRPC_ENDPOINT.to_string(),
            };
            a2a_log!(
                A2aLogLevel::Warn,
                "Using fallback endpoint extraction: {endpoint}"
            );
            Ok(endpoint)
        }
    }
}

fn set_protocol_binding(agent_card: &AgentCard, transport_type: &str) -> AgentCard {
    let mut modified = agent_card.clone();
    if let Some(first) = modified.supported_interfaces.first_mut() {
        if first.protocol_binding != transport_type {
            a2a_log!(
                A2aLogLevel::Warn,
                "Agent card's primaryInterface.protocolBinding is not {transport_type}, changing to default value"
            );
            first.protocol_binding = transport_type.to_string();
        }
    }
    modified
}

/// Builds HTTP servers.
pub struct HttpServerBuilder;

impl HttpServerBuilder {
    /// Build a server. The JSON-RPC endpoint is taken from the first interface URL.
    ///
    /// Fails when the card has no interface, the URL is empty or its port is
    /// out of range. A missing task store defaults to the in-memory store.
    pub fn build(
        config: &HttpConfig,
        agent_card: &AgentCard,
        extended_agent_card: &AgentCard,
        agent_executor: Arc<dyn AgentExecutor>,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Result<Arc<dyn Server>, A2aServerError> {
        let server = Self::build_impl(
            config,
            agent_card,
            extended_agent_card,
            Some(agent_executor),
            task_store,
        )?;
        Ok(Arc::new(server))
    }

    /// Build a [`ServerImpl`], optionally without an executor.
    pub fn build_impl(
        config: &HttpConfig,
        agent_card: &AgentCard,
        extended_agent_card: &AgentCard,
        agent_executor: Option<Arc<dyn AgentExecutor>>,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Result<ServerImpl, A2aServerError> {
        let Some(first) = agent_card.supported_interfaces.first() else {
            a2a_log!(A2aLogLevel::Error, "agentCard.supportedInterfaces is empty");
            return Err(A2aServerError::new("agentCard.supportedInterfaces is empty"));
        };
        if first.url.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "agentCard.supportedInterfaces[0].url is empty"
            );
            return Err(A2aServerError::new(
                "agentCard.supportedInterfaces[0].url is empty",
            ));
        }
        let endpoint = endpoint_from_url(&first.url)?;
        let server_config = HttpConfig {
            ip: config.ip.clone(),
            port: config.port,
            io_thread_num: config.io_thread_num,
            endpoint,
        };
        let modified = set_protocol_binding(agent_card, JSONRPC_TRANSPORT);
        Ok(ServerImpl::new(
            Arc::new(modified),
            Arc::new(extended_agent_card.clone()),
            agent_executor,
            server_config,
            task_store,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn url_parsing_matches_regex_semantics() -> TestResult {
        assert_eq!(
            parse_interface_url("http://127.0.0.1:8080/rpc"),
            Some(ParsedUrl {
                port: 8080,
                path: Some("/rpc".into())
            })
        );
        assert_eq!(
            parse_interface_url("localhost:9000"),
            Some(ParsedUrl {
                port: 9000,
                path: None
            })
        );
        assert_eq!(parse_interface_url("http://host/rpc"), None);
        assert_eq!(parse_interface_url("http://host:abc/rpc"), None);
        assert_eq!(parse_interface_url("http://host:80x/rpc"), None);
        Ok(())
    }

    #[test]
    fn endpoint_extraction() -> TestResult {
        assert_eq!(endpoint_from_url("http://h:80/a/b")?, "/a/b");
        assert_eq!(endpoint_from_url("http://h:80")?, "/jsonrpc");
        assert_eq!(endpoint_from_url("h:80/x")?, "/x");
        assert!(endpoint_from_url("http://h:0/x").is_err());
        assert!(endpoint_from_url("http://h:65536/x").is_err());
        assert_eq!(endpoint_from_url("http://h/custom")?, "/custom");
        assert_eq!(endpoint_from_url("nothing")?, "/jsonrpc");
        Ok(())
    }
}
