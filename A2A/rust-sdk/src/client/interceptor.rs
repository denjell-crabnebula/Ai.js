// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client call interceptors, the port of `include/client/client_call_interceptor.h`
//! and the protocol version interceptor.

use std::collections::BTreeMap;

use crate::protocol::DEFAULT_PROTOCOL_VERSION;
use crate::protocol::http::K_PROTOCOL_VERSION_HEADER;
use crate::types::{AgentCard, ClientCallContext};

/// Client-side request interceptor for header and payload mutation.
pub trait ClientCallInterceptor: Send + Sync {
    /// Intercept an outbound RPC call; may mutate payload and headers.
    fn intercept(
        &self,
        method_name: &str,
        payload: &mut String,
        headers: &mut BTreeMap<String, String>,
        agent_card: Option<&AgentCard>,
        context: Option<&ClientCallContext>,
    );
}

/// Adds the `A2A-Version` header to every request.
#[derive(Clone, Debug)]
pub struct ProtocolVersionInterceptor {
    version: String,
}

impl Default for ProtocolVersionInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolVersionInterceptor {
    /// Interceptor sending the default protocol version.
    pub fn new() -> Self {
        ProtocolVersionInterceptor {
            version: DEFAULT_PROTOCOL_VERSION.to_string(),
        }
    }

    /// Interceptor sending a custom version. An empty version sends nothing.
    pub fn with_version(version: impl Into<String>) -> Self {
        ProtocolVersionInterceptor {
            version: version.into(),
        }
    }
}

impl ClientCallInterceptor for ProtocolVersionInterceptor {
    fn intercept(
        &self,
        _method_name: &str,
        _payload: &mut String,
        headers: &mut BTreeMap<String, String>,
        _agent_card: Option<&AgentCard>,
        _context: Option<&ClientCallContext>,
    ) {
        if !self.version.is_empty() {
            headers.insert(K_PROTOCOL_VERSION_HEADER.to_string(), self.version.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn adds_version_header_and_keeps_payload() -> TestResult {
        let i = ProtocolVersionInterceptor::new();
        let mut payload = "{\"a\":1}".to_string();
        let mut headers = BTreeMap::new();
        headers.insert("X-Other".to_string(), "keep".to_string());
        headers.insert(K_PROTOCOL_VERSION_HEADER.to_string(), "0.1".to_string());
        i.intercept("SendMessage", &mut payload, &mut headers, None, None);
        assert_eq!(payload, "{\"a\":1}");
        assert_eq!(
            headers.get(K_PROTOCOL_VERSION_HEADER).map(String::as_str),
            Some("1.0")
        );
        assert_eq!(headers.get("X-Other").map(String::as_str), Some("keep"));
        i.intercept("GetTask", &mut payload, &mut headers, None, None);
        assert_eq!(headers.len(), 2);
        let empty = ProtocolVersionInterceptor::with_version("");
        let mut h2 = BTreeMap::new();
        empty.intercept("m", &mut payload, &mut h2, None, None);
        assert!(h2.is_empty());
        Ok(())
    }
}
