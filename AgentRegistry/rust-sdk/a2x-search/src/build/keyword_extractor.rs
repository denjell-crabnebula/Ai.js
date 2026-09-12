// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Batched keyword extraction from services.
//!
//! Pure computation: extracts keywords through the LLM and returns them.
//! Caching (`keywords.json`) is handled by the caller.

use a2x_common::{ChatMessage, LlmBackend, parse_json_response};
use indexmap::IndexMap;
use serde_json::Value;

use super::config::AutoHierarchicalConfig;
use super::progress::BuildSink;
use super::prompts::{
    KEYWORD_EXTRACTION_TEMPLATE, NodeInfo, SYSTEM_KEYWORD_EXTRACTION, fill, format_keywords_for_prompt,
    format_node_context_for_keywords, format_services_batch,
};
use crate::taxonomy::ServiceRecord;

/// `keyword -> count`, sorted by count descending.
pub type KeywordCounts = IndexMap<String, u64>;

/// Extract functional keywords from services in sequential batches.
///
/// Each batch sees the accumulated keyword list so synonyms are reused.
/// For non-root nodes extraction is scoped to the node's domain.
pub struct KeywordExtractor<'a> {
    llm: &'a dyn LlmBackend,
    config: &'a AutoHierarchicalConfig,
    sink: &'a BuildSink,
}

impl<'a> KeywordExtractor<'a> {
    pub fn new(llm: &'a dyn LlmBackend, config: &'a AutoHierarchicalConfig, sink: &'a BuildSink) -> Self {
        Self { llm, config, sink }
    }

    /// Extract keywords from all services in batches.
    pub async fn extract(&self, services: &[ServiceRecord], node_info: Option<&NodeInfo>) -> KeywordCounts {
        let batch_size = self.config.keyword_batch_size.max(1);
        let n_batches = services.len().div_ceil(batch_size);
        let mut keywords = KeywordCounts::new();

        let node_label = node_info
            .map(|n| format!(" (within '{}')", n.name_or("Unknown")))
            .unwrap_or_default();
        self.sink.log(format!(
            "Keyword Extraction: {} services{node_label}, {n_batches} batches",
            services.len()
        ));

        let node_context_section = format_node_context_for_keywords(node_info);

        for (batch_idx, batch) in services.chunks(batch_size).enumerate() {
            let batch_keywords = self.extract_batch(batch, &keywords, &node_context_section).await;
            for (kw, count) in batch_keywords {
                *keywords.entry(kw).or_insert(0) += count;
            }
            self.sink.progress(
                batch_idx + 1,
                n_batches,
                "",
                &format!("{} keywords", keywords.len()),
            );
        }

        let keywords = sort_keywords(keywords);
        self.sink
            .log(format!("Extraction complete: {} unique keywords", keywords.len()));
        let top: Vec<&String> = keywords.keys().take(20).collect();
        self.sink.log(format!("Top 20: {top:?}"));
        keywords
    }

    async fn extract_batch(
        &self,
        services: &[ServiceRecord],
        existing_keywords: &KeywordCounts,
        node_context_section: &str,
    ) -> KeywordCounts {
        let services_text = format_services_batch(services, 150);
        let existing_text = format_keywords_for_prompt(existing_keywords, 300);
        let prompt = fill(
            KEYWORD_EXTRACTION_TEMPLATE,
            &[
                ("batch_size", &services.len().to_string()),
                ("max_keywords", &self.config.max_keywords_per_service.to_string()),
                ("existing_keywords_text", &existing_text),
                ("services_text", &services_text),
                ("node_context_section", node_context_section),
            ],
        );
        let response = self
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_KEYWORD_EXTRACTION),
                    ChatMessage::user(prompt),
                ],
                self.config.temperature_keywords,
                Some(self.config.max_tokens_keywords),
            )
            .await;

        if !response.success {
            self.sink.warn(format!(
                "Batch extraction failed: {}",
                response.error.as_deref().unwrap_or("unknown")
            ));
            return KeywordCounts::new();
        }
        let Some(result) = parse_json_response(&response.content) else {
            self.sink.warn("Failed to parse extraction response");
            return KeywordCounts::new();
        };
        let Some(extractions) = result.get("extractions").and_then(Value::as_array) else {
            self.sink.warn("Failed to parse extraction response");
            return KeywordCounts::new();
        };

        let mut batch_keywords = KeywordCounts::new();
        for extraction in extractions {
            let kws = extraction.get("keywords").and_then(Value::as_array);
            for kw in kws.into_iter().flatten() {
                let Some(kw) = kw.as_str() else { continue };
                let kw = kw.trim().to_lowercase().replace(' ', "_");
                if !kw.is_empty() && kw.chars().count() > 1 {
                    *batch_keywords.entry(kw).or_insert(0) += 1;
                }
            }
        }
        self.sink.log(format!(
            "Extracted {} unique keywords from {} services",
            batch_keywords.len(),
            services.len()
        ));
        batch_keywords
    }
}

/// Sort keywords by count descending (stable).
pub fn sort_keywords(keywords: KeywordCounts) -> KeywordCounts {
    let mut items: Vec<(String, u64)> = keywords.into_iter().collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.1));
    items.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeLlm;
    use ap_support::testing::TestResult;

    #[tokio::test]
    async fn extraction_accumulates_across_batches() -> TestResult {
        let llm = FakeLlm::new().on_fn(|p| {
            if p.contains("EXISTING KEYWORDS (reuse when applicable):\n(none yet") {
                Some(r#"```json
{"extractions": [{"service_id": "a", "keywords": ["Weather", "travel"]}, {"service_id": "b", "keywords": ["weather"]}]}
```"#.into())
            } else if p.contains("- weather (count: 2)") {
                Some(r#"{"extractions": [{"service_id": "c", "keywords": ["stock market", "x"]}]}"#.into())
            } else {
                None
            }
        });
        let mut config = AutoHierarchicalConfig::new("db/DS/service.json");
        config.keyword_batch_size = 2;
        let sink = BuildSink::null();
        let ex = KeywordExtractor::new(&llm, &config, &sink);
        let services = vec![
            ServiceRecord::new("a", "A", "x"),
            ServiceRecord::new("b", "B", "y"),
            ServiceRecord::new("c", "C", "z"),
        ];
        let kw = ex.extract(&services, None).await;
        let items: Vec<(&String, &u64)> = kw.iter().collect();
        assert_eq!(items[0], (&"weather".to_string(), &2));
        assert_eq!(kw.get("travel"), Some(&1));
        assert_eq!(kw.get("stock_market"), Some(&1));
        assert!(!kw.contains_key("x"));
        assert_eq!(llm.calls(), 2);
        Ok(())
    }
}
