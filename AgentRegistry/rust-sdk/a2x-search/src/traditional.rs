// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Traditional (MCP style) search: pass every service name and description
//! to the LLM in one prompt and let it pick.
//!
//! No taxonomy, no pre-filtering. The interface matches [`crate::A2xSearch`]:
//! `search(query) -> (Vec<SearchResult>, TraditionalStats)`.

use std::path::Path;
use std::sync::Arc;

use a2x_common::{ChatMessage, LlmBackend, SearchResult, parse_json_response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::taxonomy::{ServiceRecord, ServicesIndex, index_services, load_services};

/// Search statistics, matching the A2X `SearchStats` fields that apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraditionalStats {
    pub llm_calls: u64,
    pub total_tokens: u64,
}

impl Default for TraditionalStats {
    fn default() -> Self {
        Self {
            llm_calls: 1,
            total_tokens: 0,
        }
    }
}

/// Quoted tokens of the form `name_123` (letters or underscores, an
/// underscore, then digits) found anywhere in `text`, in order. Used when an
/// LLM reply is not valid JSON.
fn quoted_service_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut ids = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' && bytes[i] != b'\'' {
            i += 1;
            continue;
        }
        let rest = &text[i + 1..];
        let Some(end) = rest.find(['"', '\'']) else {
            break;
        };
        let candidate = &rest[..end];
        if looks_like_service_id(candidate) {
            ids.push(candidate.to_string());
            i += 1 + end + 1;
        } else {
            i += 1;
        }
    }
    ids
}

fn looks_like_service_id(candidate: &str) -> bool {
    let Some((prefix, digits)) = candidate.rsplit_once('_') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
}

pub const SYSTEM_PROMPT: &str = "You are a service discovery assistant. Given a user query and a catalog of available services, identify ALL services that could fulfill the user's request.\n\nIMPORTANT:\n- Return service IDs that are relevant to the query\n- Include all plausible matches, not just the single best one\n- Consider both exact matches and closely related services\n- Return ONLY a JSON object with a \"service_ids\" array\n";

/// Full-context LLM search.
pub struct TraditionalSearch {
    llm: Arc<dyn LlmBackend>,
    services: Vec<ServiceRecord>,
    service_map: ServicesIndex,
    catalog_text: String,
}

impl TraditionalSearch {
    /// Load `service.json` from `service_path`.
    pub fn new(service_path: &Path, llm: Arc<dyn LlmBackend>) -> Result<Self> {
        Ok(Self::from_services(load_services(service_path)?, llm))
    }

    pub fn from_services(services: Vec<ServiceRecord>, llm: Arc<dyn LlmBackend>) -> Self {
        let catalog_text = services
            .iter()
            .map(|s| format!("- [{}] {}: {}", s.id, s.name, s.description_text()))
            .collect::<Vec<_>>()
            .join("\n");
        let service_map = index_services(services.clone());
        Self {
            llm,
            services,
            service_map,
            catalog_text,
        }
    }

    pub fn services(&self) -> &[ServiceRecord] {
        &self.services
    }

    pub fn service_count(&self) -> usize {
        self.services.len()
    }

    /// The user prompt for `query`.
    pub fn build_user_prompt(&self, query: &str) -> String {
        format!(
            "## Service Catalog ({} services)\n\n{}\n\n## User Query\n\n{query}\n\n## Task\n\nReturn a JSON object with a \"service_ids\" array containing the IDs of ALL services that could help fulfill this query.\n\n```json\n{{\"service_ids\": [\"id1\", \"id2\", ...]}}\n```",
            self.services.len(),
            self.catalog_text
        )
    }

    /// Search for services matching `query`.
    pub async fn search(&self, query: &str) -> (Vec<SearchResult>, TraditionalStats) {
        let messages = [
            ChatMessage::system(SYSTEM_PROMPT),
            ChatMessage::user(self.build_user_prompt(query)),
        ];
        let response = self.llm.call(&messages, 0.0, None).await;
        let stats = TraditionalStats {
            llm_calls: 1,
            total_tokens: response.tokens,
        };
        if !response.success {
            return (Vec::new(), stats);
        }
        let parsed = parse_json_response(&response.content);
        let service_ids: Vec<String> = match parsed.as_ref().and_then(|p| p.get("service_ids")) {
            Some(Value::Array(list)) => list
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(_) => Vec::new(),
            None => quoted_service_ids(&response.content),
        };
        let results = service_ids
            .iter()
            .filter_map(|sid| {
                self.service_map
                    .get(sid)
                    .map(|s| SearchResult::new(sid.clone(), s.name.clone(), s.description_text()))
            })
            .collect();
        (results, stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeLlm;
    use ap_support::testing::TestResult;

    fn services() -> Vec<ServiceRecord> {
        vec![
            ServiceRecord::new("tool_1", "Flights", "Book flights"),
            ServiceRecord::new("tool_2", "Hotels", "Book hotels"),
        ]
    }

    #[tokio::test]
    async fn json_and_regex_fallback() -> TestResult {
        let llm = FakeLlm::new()
            .on(
                "User Query\n\njson",
                "```json\n{\"service_ids\": [\"tool_2\", \"nope\"]}\n```",
            )
            .on(
                "User Query\n\nprose",
                "I would pick 'tool_1' and \"tool_2\" for this.",
            )
            .fail_on("User Query\n\nfail", "down");
        let s = TraditionalSearch::from_services(services(), Arc::new(llm));
        let (r, stats) = s.search("json").await;
        assert_eq!(
            r.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["tool_2"]
        );
        assert_eq!(stats.llm_calls, 1);
        let (r, _) = s.search("prose").await;
        assert_eq!(
            r.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["tool_1", "tool_2"]
        );
        let (r, stats) = s.search("fail").await;
        assert!(r.is_empty());
        assert_eq!(stats.total_tokens, 0);
        let prompt = s.build_user_prompt("q");
        assert!(prompt.starts_with("## Service Catalog (2 services)\n\n- [tool_1] Flights: Book flights\n- [tool_2] Hotels: Book hotels\n\n## User Query\n\nq\n\n## Task"));
        Ok(())
    }
}
