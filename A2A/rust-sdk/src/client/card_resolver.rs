// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Agent card resolvers, the port of `a2a_card_resolver.h`, `http_card_resolver.*`
//! and `http_card_resolver_builder.*`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aClientError};
use crate::log::A2aLogLevel;
use crate::types::AgentCard;
use crate::utils::generate_uuid;

use super::config::ClientConfig;
use super::jsonrpc_transport::JsonRpcTransport;
use super::transport::{ClientTransport, TransportEvent};

/// Resolves agent cards from a remote or local source.
#[async_trait]
pub trait A2ACardResolver: Send + Sync {
    /// Fetch the agent card. The HTTP resolver ignores `relative_card_path`
    /// and uses the path given at build time, like the original SDK.
    async fn get_agent_card(&self, relative_card_path: Option<&str>) -> Result<AgentCard, A2aClientError>;

    /// Fetch all agent cards from the source.
    async fn get_all_agent_cards(&self) -> Result<Vec<AgentCard>, A2aClientError>;
}

type Pending = oneshot::Sender<Result<AgentCard, A2aClientError>>;

/// Resolver fetching the card with an HTTP GET.
pub struct HttpCardResolver {
    base_url: String,
    relative_card_path: Option<String>,
    http_kwargs: BTreeMap<String, String>,
    transport: Arc<JsonRpcTransport>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

/// Join a base URL and a relative path without producing `//`.
pub fn join_card_url(base_url: &str, path: &str) -> String {
    let mut full = base_url.trim_end_matches('/').to_string();
    if !path.is_empty() {
        if !path.starts_with('/') {
            full.push('/');
        }
        full.push_str(path);
    }
    full
}

impl HttpCardResolver {
    /// Create a resolver. `http_kwargs` are sent as extra request headers.
    pub fn new(
        base_url: impl Into<String>,
        relative_card_path: Option<String>,
        http_kwargs: BTreeMap<String, String>,
    ) -> Self {
        let base_url = base_url.into();
        let full = join_card_url(&base_url, relative_card_path.as_deref().unwrap_or(""));
        let transport = Arc::new(JsonRpcTransport::with_headers(
            full,
            &AgentCard::default(),
            &ClientConfig::default(),
            Vec::new(),
            http_kwargs.clone(),
        ));
        let pending: Arc<Mutex<HashMap<String, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_cb = pending.clone();
        transport.set_transport_callback(Arc::new(move |id, ev| {
            Self::on_transport_event(&pending_cb, id, ev);
        }));
        HttpCardResolver {
            base_url,
            relative_card_path,
            http_kwargs,
            transport,
            pending,
        }
    }

    /// Base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Card path.
    pub fn relative_card_path(&self) -> Option<&str> {
        self.relative_card_path.as_deref()
    }

    /// Extra headers.
    pub fn http_kwargs(&self) -> &BTreeMap<String, String> {
        &self.http_kwargs
    }

    /// Full URL of the card.
    pub fn card_url(&self) -> &str {
        self.transport.url()
    }

    fn on_transport_event(
        pending: &Mutex<HashMap<String, Pending>>,
        request_id: &str,
        event: TransportEvent,
    ) {
        let Some(tx) = pending.lock().remove(request_id) else {
            return;
        };
        let result = match event {
            TransportEvent::Error(e) => Err(A2aClientError::make(e.error_code, e.err_info)),
            TransportEvent::AgentCard(card) => Ok(card),
            _ => Err(A2aClientError::from_code(
                A2AErrorCode::A2aInvalidFormat,
                "invalid response format for GetCard",
            )),
        };
        let _ = tx.send(result);
    }
}

#[async_trait]
impl A2ACardResolver for HttpCardResolver {
    async fn get_agent_card(&self, _relative_card_path: Option<&str>) -> Result<AgentCard, A2aClientError> {
        let request_id = generate_uuid();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(request_id.clone(), tx);
        if let Err(e) = self.transport.get_card(&request_id, None, 0) {
            self.pending.lock().remove(&request_id);
            return Err(A2aClientError::from_code(
                A2AErrorCode::A2aTransportException,
                e.message(),
            ));
        }
        rx.await.unwrap_or_else(|_| {
            Err(A2aClientError::from_code(
                A2AErrorCode::A2aStatusError,
                "Request abandoned",
            ))
        })
    }

    async fn get_all_agent_cards(&self) -> Result<Vec<AgentCard>, A2aClientError> {
        let card = self.get_agent_card(None).await?;
        Ok(vec![card])
    }
}

/// Factory for HTTP-based [`A2ACardResolver`] instances.
pub struct HttpCardResolverBuilder;

impl HttpCardResolverBuilder {
    /// Build an HTTP card resolver.
    ///
    /// Returns `None` when `base_url` or `agent_card_path` is empty.
    pub fn build(
        base_url: &str,
        agent_card_path: &str,
        http_kwargs: &BTreeMap<String, String>,
    ) -> Option<Arc<dyn A2ACardResolver>> {
        if base_url.is_empty() || agent_card_path.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "HttpCardResolverBuilder::Build invalid parameter."
            );
            return None;
        }
        Some(Arc::new(HttpCardResolver::new(
            base_url,
            Some(agent_card_path.to_string()),
            http_kwargs.clone(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn url_join() -> TestResult {
        assert_eq!(join_card_url("http://h:1/", "/card.json"), "http://h:1/card.json");
        assert_eq!(join_card_url("http://h:1", "card.json"), "http://h:1/card.json");
        assert_eq!(join_card_url("http://h:1/", ""), "http://h:1");
        Ok(())
    }

    #[test]
    fn builder_validates_input() -> TestResult {
        let kw = BTreeMap::new();
        assert!(HttpCardResolverBuilder::build("", "/x", &kw).is_none());
        assert!(HttpCardResolverBuilder::build("http://h", "", &kw).is_none());
        assert!(HttpCardResolverBuilder::build("", "", &kw).is_none());
        assert!(HttpCardResolverBuilder::build("http://h", "/x", &kw).is_some());
        Ok(())
    }
}
