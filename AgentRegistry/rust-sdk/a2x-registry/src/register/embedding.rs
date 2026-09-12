// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Embedding model constants (port of `vector/utils/embedding_constants.py`).
//!
//! The vector backend lives in the `a2x-search` crate. These literals are
//! duplicated here so the registry can validate `vector_config.json` and
//! answer `GET /api/datasets/embedding-models` without that crate.

use serde_json::{Map, Value, json};

/// Default embedding model for new datasets.
pub const DEFAULT_EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

/// `(name, dim, language, description)` in the original declaration order.
pub const EMBEDDING_MODELS: &[(&str, u32, &str, &str)] = &[
    ("all-MiniLM-L6-v2", 384, "en", "English general-purpose (default)"),
    (
        "shibing624/text2vec-base-chinese",
        768,
        "zh",
        "Chinese text embedding",
    ),
    (
        "paraphrase-multilingual-MiniLM-L12-v2",
        384,
        "multilingual",
        "Multilingual 50+ languages",
    ),
];

/// Embedding dimension for a known model name.
pub fn embedding_dim(model: &str) -> Option<u32> {
    EMBEDDING_MODELS
        .iter()
        .find(|(n, _, _, _)| *n == model)
        .map(|(_, d, _, _)| *d)
}

/// The `EMBEDDING_MODELS` table as JSON, matching the Python dict layout.
pub fn embedding_models_json() -> Value {
    let mut map = Map::new();
    for (name, dim, lang, desc) in EMBEDDING_MODELS {
        map.insert(
            (*name).to_string(),
            json!({"dim": dim, "language": lang, "description": desc}),
        );
    }
    Value::Object(map)
}
