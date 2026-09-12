// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Data models for A2X hierarchical taxonomy search.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use a2x_common::SearchResult;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Search mode. See the crate README for the recall / precision trade-off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Select all potentially relevant categories and services (high recall).
    #[default]
    GetAll,
    /// Select only clearly needed categories and services (balanced).
    GetImportant,
    /// Select a single best match; falls back to `get_important` when empty.
    GetOne,
}

impl SearchMode {
    pub const ALL: [SearchMode; 3] = [SearchMode::GetAll, SearchMode::GetOne, SearchMode::GetImportant];

    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::GetAll => "get_all",
            SearchMode::GetImportant => "get_important",
            SearchMode::GetOne => "get_one",
        }
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SearchMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "get_all" => Ok(SearchMode::GetAll),
            "get_important" => Ok(SearchMode::GetImportant),
            "get_one" => Ok(SearchMode::GetOne),
            other => Err(Error::Invalid(format!(
                "Invalid mode: {other}. Must be one of ('get_all', 'get_one', 'get_important')."
            ))),
        }
    }
}

/// Statistics accumulated during one search.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchStats {
    pub llm_calls: u64,
    pub total_tokens: u64,
    /// Human readable paths of selected categories (`Root/Travel/Flights`).
    pub visited_categories: Vec<String>,
    /// Human readable paths of pruned categories.
    pub pruned_categories: Vec<String>,
    /// Ids of every category the navigator entered.
    pub visited_category_ids: Vec<String>,
}

impl SearchStats {
    /// Compact form used by the streaming result and the HTTP API.
    pub fn summary(&self) -> StreamStats {
        StreamStats {
            llm_calls: self.llm_calls,
            total_tokens: self.total_tokens,
            visited_categories: self.visited_categories.len(),
            pruned_categories: self.pruned_categories.len(),
        }
    }
}

/// Thread-safe accumulator behind [`SearchStats`].
#[derive(Debug, Default)]
pub struct StatsCollector {
    inner: Mutex<SearchStats>,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_call(&self, tokens: u64) {
        let mut s = self.inner.lock();
        s.llm_calls += 1;
        s.total_tokens += tokens;
    }

    pub fn add_counts(&self, llm_calls: u64, tokens: u64) {
        let mut s = self.inner.lock();
        s.llm_calls += llm_calls;
        s.total_tokens += tokens;
    }

    pub fn add_visited_id(&self, id: &str) {
        self.inner.lock().visited_category_ids.push(id.to_string());
    }

    pub fn add_paths(&self, visited: Vec<String>, pruned: Vec<String>) {
        let mut s = self.inner.lock();
        s.visited_categories.extend(visited);
        s.pruned_categories.extend(pruned);
    }

    pub fn snapshot(&self) -> SearchStats {
        self.inner.lock().clone()
    }

    pub fn into_inner(self) -> SearchStats {
        self.inner.into_inner()
    }
}

/// One step of category navigation, for UI animation.
///
/// Two synthetic steps mark phase changes: `parent_id == "__phase2__"`
/// when service selection starts and `"__fallback__"` when `get_one`
/// re-runs as `get_important`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigationStep {
    pub parent_id: String,
    pub selected: Vec<String>,
    pub pruned: Vec<String>,
}

impl NavigationStep {
    pub const PHASE2: &'static str = "__phase2__";
    pub const FALLBACK: &'static str = "__fallback__";

    pub fn marker(parent_id: &str) -> Self {
        Self {
            parent_id: parent_id.to_string(),
            ..Default::default()
        }
    }
}

/// A leaf (or a node with direct services) reached during Phase 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNode {
    pub category_id: String,
    pub service_ids: Vec<String>,
}

/// A group of services submitted to one Phase 2 LLM call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceGroup {
    pub leaf_ids: HashSet<String>,
    pub service_ids: Vec<String>,
}

/// Compact stats attached to the streaming result message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStats {
    pub llm_calls: u64,
    pub total_tokens: u64,
    pub visited_categories: usize,
    pub pruned_categories: usize,
}

/// Message yielded by [`super::A2xSearch::search_streaming`]. Serializes to
/// the same JSON as the Python generator (`{"type": "step", ...}` and
/// `{"type": "result", ...}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMessage {
    Step {
        parent_id: String,
        selected: Vec<String>,
        pruned: Vec<String>,
    },
    Result {
        results: Vec<SearchResult>,
        stats: StreamStats,
    },
}

impl From<NavigationStep> for StreamMessage {
    fn from(step: NavigationStep) -> Self {
        StreamMessage::Step {
            parent_id: step.parent_id,
            selected: step.selected,
            pruned: step.pruned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn mode_parsing() -> TestResult {
        assert_eq!("get_one".parse::<SearchMode>()?, SearchMode::GetOne);
        assert!("bogus".parse::<SearchMode>().is_err());
        assert_eq!(SearchMode::GetImportant.to_string(), "get_important");
        Ok(())
    }

    #[test]
    fn stream_message_json_shape() -> TestResult {
        let m: StreamMessage = NavigationStep {
            parent_id: "root".into(),
            selected: vec!["a".into()],
            pruned: vec![],
        }
        .into();
        let v = serde_json::to_value(&m)?;
        assert_eq!(v["type"], "step");
        assert_eq!(v["parent_id"], "root");
        let r = StreamMessage::Result {
            results: vec![],
            stats: StreamStats::default(),
        };
        let v = serde_json::to_value(&r)?;
        assert_eq!(v["type"], "result");
        assert_eq!(v["stats"]["visited_categories"], 0);
        Ok(())
    }
}
