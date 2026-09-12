// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client factory, the port of `client_factory.cpp`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::a2a_log;
use crate::log::A2aLogLevel;
use crate::protocol::JSONRPC_TRANSPORT;
use crate::types::AgentCard;

use super::config::{ClientConfig, Consumer};
use super::default_client::{Client, DefaultClient};
use super::interceptor::{ClientCallInterceptor, ProtocolVersionInterceptor};
use super::jsonrpc_transport::JsonRpcTransport;
use super::transport::ClientTransport;

/// Factory for configured [`Client`] instances.
pub struct ClientFactory;

impl ClientFactory {
    /// Select the transport binding and URL like the C++ factory.
    ///
    /// Returns `None` when the card or config is invalid or no binding matches.
    pub fn select_transport(card: &AgentCard, config: &ClientConfig) -> Option<(String, String)> {
        if config.supported_transports.iter().any(String::is_empty) {
            return None;
        }
        if card.supported_interfaces.is_empty() {
            return None;
        }
        for itf in &card.supported_interfaces {
            if itf.protocol_binding.is_empty() || itf.url.is_empty() || itf.protocol_version.is_empty() {
                return None;
            }
        }
        let mut server_set: BTreeMap<String, String> = BTreeMap::new();
        for itf in &card.supported_interfaces {
            if server_set.contains_key(&itf.protocol_binding) {
                a2a_log!(
                    A2aLogLevel::Warn,
                    "Duplicate protocolBinding:{}",
                    itf.protocol_binding
                );
            } else {
                server_set.insert(itf.protocol_binding.clone(), itf.url.clone());
            }
        }
        let client_set: Vec<String> = if config.supported_transports.is_empty() {
            vec![JSONRPC_TRANSPORT.to_string()]
        } else {
            config.supported_transports.clone()
        };
        let chosen = if config.use_client_preference {
            client_set
                .iter()
                .find_map(|x| server_set.get(x).map(|url| (x.clone(), url.clone())))
        } else {
            server_set
                .iter()
                .find(|(binding, _)| client_set.contains(binding))
                .map(|(b, u)| (b.clone(), u.clone()))
        };
        match chosen {
            Some((protocol, url)) if !protocol.is_empty() && !url.is_empty() => Some((protocol, url)),
            _ => None,
        }
    }

    /// Create a client with the default JSON-RPC transport.
    ///
    /// The protocol version interceptor is appended to `interceptors`.
    /// Returns `None` when the card or config is invalid or no transport matches.
    pub fn create(
        card: &AgentCard,
        config: &ClientConfig,
        consumers: Vec<Consumer>,
        interceptors: Vec<Arc<dyn ClientCallInterceptor>>,
    ) -> Option<Arc<dyn Client>> {
        let (protocol, url) = Self::select_transport(card, config)?;
        let mut final_interceptors = interceptors;
        final_interceptors.push(Arc::new(ProtocolVersionInterceptor::new()));
        let transport: Arc<dyn ClientTransport> = if protocol == JSONRPC_TRANSPORT {
            Arc::new(JsonRpcTransport::new(url, card, config, final_interceptors))
        } else {
            return None;
        };
        Some(Arc::new(DefaultClient::new(
            card.clone(),
            config.clone(),
            transport,
            consumers,
        )))
    }

    /// Create a client with a custom transport.
    pub fn create_with_transport(
        card: &AgentCard,
        config: &ClientConfig,
        transport: Arc<dyn ClientTransport>,
        consumers: Vec<Consumer>,
    ) -> Option<Arc<dyn Client>> {
        Some(Arc::new(DefaultClient::new(
            card.clone(),
            config.clone(),
            transport,
            consumers,
        )))
    }
}
