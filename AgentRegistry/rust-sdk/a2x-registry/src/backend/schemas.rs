// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Request and response schemas of the search and dataset routers.

use a2x_common::SearchResult;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

fn default_dataset() -> String {
    "ToolRet_clean".to_string()
}

fn default_top_k() -> Option<i64> {
    Some(10)
}

/// Body of `POST /api/search`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub method: String,
    #[serde(default = "default_dataset")]
    pub dataset: String,
    #[serde(default = "default_top_k")]
    pub top_k: Option<i64>,
}

/// Response of `POST /api/search`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub stats: Map<String, Value>,
    pub elapsed_time: f64,
}

/// One dataset in `GET /api/datasets`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub name: String,
    pub service_count: usize,
    pub query_count: usize,
}

/// Body of `POST /api/search/judge`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JudgeRequest {
    pub query: String,
    pub services: Vec<SearchResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JudgeResult {
    pub service_id: String,
    pub relevant: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JudgeResponse {
    pub results: Vec<JudgeResult>,
}

/// One entry of `GET /api/datasets/{dataset}/default-queries`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefaultQuery {
    pub query: String,
    #[serde(default)]
    pub query_en: String,
}
