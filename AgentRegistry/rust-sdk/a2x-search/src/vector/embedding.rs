// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Text embedding backends and the embedding model table.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use a2x_common::A2xError;
use a2x_common::paths::llm_apikey_path;
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// Default embedding model name.
pub const DEFAULT_EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

/// Metadata of one entry of the embedding model table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModelInfo {
    pub dim: usize,
    pub language: String,
    pub description: String,
}

/// The `EMBEDDING_MODELS` table: model name to dimension, language and
/// description. Serialized shape matches the Python dict.
pub fn embedding_models() -> IndexMap<String, EmbeddingModelInfo> {
    let mut m = IndexMap::new();
    m.insert(
        "all-MiniLM-L6-v2".to_string(),
        EmbeddingModelInfo {
            dim: 384,
            language: "en".into(),
            description: "English general-purpose (default)".into(),
        },
    );
    m.insert(
        "shibing624/text2vec-base-chinese".to_string(),
        EmbeddingModelInfo {
            dim: 768,
            language: "zh".into(),
            description: "Chinese text embedding".into(),
        },
    );
    m.insert(
        "paraphrase-multilingual-MiniLM-L12-v2".to_string(),
        EmbeddingModelInfo {
            dim: 384,
            language: "multilingual".into(),
            description: "Multilingual 50+ languages".into(),
        },
    );
    m
}

/// Dimension of a model in the table.
pub fn embedding_dim(model_name: &str) -> Option<usize> {
    embedding_models().get(model_name).map(|m| m.dim)
}

/// A text embedding backend. Vectors are L2 normalized so cosine
/// similarity is a dot product.
#[async_trait]
pub trait EmbeddingModel: Send + Sync {
    /// Embed each text into a vector of [`EmbeddingModel::dim`] floats.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Vector dimension.
    fn dim(&self) -> usize;

    /// Model name recorded in stores and configs.
    fn name(&self) -> String;

    /// Embed one text.
    async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[text.to_string()]).await?;
        v.pop()
            .ok_or_else(|| Error::Other("embedding backend returned no vector".into()))
    }
}

/// L2 normalize in place (no-op for the zero vector).
pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// ---------------------------------------------------------------------------
// Hashing embedding (offline, deterministic)
// ---------------------------------------------------------------------------

/// Deterministic bag-of-tokens embedding using feature hashing.
///
/// Tokens are lowercased alphanumeric words (each CJK character is its own
/// token) plus adjacent-token bigrams. No network, no model files. Meant
/// for tests, demos and CI; quality is far below a real model.
#[derive(Clone, Debug)]
pub struct HashingEmbedding {
    dim: usize,
    name: String,
}

impl HashingEmbedding {
    pub fn new(dim: usize) -> Self {
        Self {
            dim: dim.max(1),
            name: "hashing".into(),
        }
    }

    /// Report `name` (for example a table entry) while hashing offline.
    pub fn named(name: impl Into<String>, dim: usize) -> Self {
        Self {
            dim: dim.max(1),
            name: name.into(),
        }
    }

    fn tokens(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut word = String::new();
        for c in text.chars() {
            if c.is_ascii_alphanumeric() {
                word.push(c.to_ascii_lowercase());
            } else {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                if c.is_alphanumeric() {
                    tokens.push(c.to_lowercase().collect());
                }
            }
        }
        if !word.is_empty() {
            tokens.push(word);
        }
        tokens
    }

    fn fnv1a(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Embed one text synchronously.
    pub fn embed_sync(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        let tokens = Self::tokens(text);
        let mut features: Vec<String> = tokens.clone();
        for pair in tokens.windows(2) {
            features.push(format!("{} {}", pair[0], pair[1]));
        }
        for f in features {
            let h = Self::fnv1a(&f);
            let idx = (h % self.dim as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign;
        }
        normalize(&mut v);
        v
    }
}

#[async_trait]
impl EmbeddingModel for HashingEmbedding {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_sync(t)).collect())
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

// ---------------------------------------------------------------------------
// OpenAI compatible embeddings endpoint
// ---------------------------------------------------------------------------

/// Configuration of an OpenAI compatible `/embeddings` endpoint.
///
/// Read from the `"embedding"` object of `llm_apikey.json`:
///
/// ```json
/// {"providers": [...],
///  "embedding": {"base_url": "https://api.openai.com/v1/embeddings",
///                "model": "text-embedding-3-small", "api_keys": ["sk-..."], "dim": 1536}}
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingProviderConfig {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Vector dimension; looked up in the model table when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<usize>,
    /// Texts per HTTP request.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_batch_size() -> usize {
    100
}

impl EmbeddingProviderConfig {
    /// Parse the `"embedding"` object of an `llm_apikey.json` document.
    pub fn from_config_value(config: &Value) -> Option<Self> {
        let e = config.get("embedding")?;
        let base_url = e.get("base_url")?.as_str()?.to_string();
        let model = e.get("model")?.as_str()?.to_string();
        let mut api_keys: Vec<String> = e
            .get("api_keys")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        if api_keys.is_empty() {
            if let Some(k) = e.get("api_key").and_then(Value::as_str) {
                api_keys.push(k.to_string());
            }
        }
        Some(Self {
            base_url,
            model,
            api_keys,
            dim: e.get("dim").and_then(Value::as_u64).map(|d| d as usize),
            batch_size: e
                .get("batch_size")
                .and_then(Value::as_u64)
                .map(|b| b as usize)
                .unwrap_or(100),
        })
    }

    /// Load from `path`, or the default `llm_apikey.json` location.
    /// Returns `Ok(None)` when the file or the section is absent.
    pub fn load(path: Option<&Path>) -> Result<Option<Self>> {
        let path = path.map(Path::to_path_buf).unwrap_or_else(llm_apikey_path);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let text = text.replace(",\n}", "\n}").replace(",}", "}");
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::json(&path, e))?;
        Ok(Self::from_config_value(&v))
    }
}

/// Embeddings over an OpenAI compatible HTTP endpoint.
pub struct OpenAiCompatibleEmbedding {
    config: EmbeddingProviderConfig,
    http: reqwest::Client,
    dim: usize,
}

impl OpenAiCompatibleEmbedding {
    pub fn new(config: EmbeddingProviderConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| Error::Other(format!("cannot build HTTP client: {e}")))?;
        let dim = config.dim.or_else(|| embedding_dim(&config.model)).unwrap_or(0);
        Ok(Self { config, http, dim })
    }

    pub fn config(&self) -> &EmbeddingProviderConfig {
        &self.config
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut req = self
            .http
            .post(&self.config.base_url)
            .header("Content-Type", "application/json")
            .json(&json!({"model": self.config.model, "input": texts}));
        if let Some(key) = self.config.api_keys.first() {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Other(format!("embedding request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "embedding request failed: HTTP {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("embedding response is not JSON: {e}")))?;
        let data = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Other("embedding response has no data array".into()))?;
        let mut rows: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for (pos, item) in data.iter().enumerate() {
            let index = item
                .get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .unwrap_or(pos);
            let mut vector: Vec<f32> = item
                .get("embedding")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_f64).map(|f| f as f32).collect())
                .unwrap_or_default();
            normalize(&mut vector);
            rows.push((index, vector));
        }
        rows.sort_by_key(|(i, _)| *i);
        if rows.len() != texts.len() {
            return Err(Error::Other(format!(
                "embedding response returned {} vectors for {} inputs",
                rows.len(),
                texts.len()
            )));
        }
        Ok(rows.into_iter().map(|(_, v)| v).collect())
    }
}

#[async_trait]
impl EmbeddingModel for OpenAiCompatibleEmbedding {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.config.batch_size.max(1)) {
            out.extend(self.embed_batch(chunk).await?);
        }
        Ok(out)
    }

    fn dim(&self) -> usize {
        if self.dim > 0 { self.dim } else { 0 }
    }

    fn name(&self) -> String {
        self.config.model.clone()
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Which backend implements a model name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmbeddingBackend {
    /// OpenAI compatible endpoint when `llm_apikey.json` has an
    /// `embedding` section, otherwise an error.
    #[default]
    Auto,
    /// Offline feature hashing.
    Hashing,
    /// OpenAI compatible endpoint (required).
    OpenAi,
}

impl FromStr for EmbeddingBackend {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(EmbeddingBackend::Auto),
            "hashing" => Ok(EmbeddingBackend::Hashing),
            "openai" => Ok(EmbeddingBackend::OpenAi),
            other => Err(Error::Invalid(format!(
                "unknown embedding backend {other:?}; use auto, hashing or openai"
            ))),
        }
    }
}

/// Environment variable selecting the backend (`auto`, `hashing`, `openai`).
pub const EMBEDDING_BACKEND_ENV: &str = "A2X_REGISTRY_EMBEDDING_BACKEND";

/// Resolve `model_name` with the backend from `A2X_REGISTRY_EMBEDDING_BACKEND`
/// (default `auto`). Replaces `EmbeddingModel(model_name)` in Python.
pub fn default_embedding_model(model_name: &str) -> Result<Arc<dyn EmbeddingModel>> {
    let backend = ap_support::env::current()
        .get_non_blank(EMBEDDING_BACKEND_ENV)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or_default();
    resolve_embedding_model(model_name, backend)
}

/// Resolve `model_name` with an explicit backend.
pub fn resolve_embedding_model(
    model_name: &str,
    backend: EmbeddingBackend,
) -> Result<Arc<dyn EmbeddingModel>> {
    let dim = embedding_dim(model_name).unwrap_or(384);
    match backend {
        EmbeddingBackend::Hashing => Ok(Arc::new(HashingEmbedding::named(model_name, dim))),
        EmbeddingBackend::OpenAi | EmbeddingBackend::Auto => {
            let cfg = EmbeddingProviderConfig::load(None)?;
            match cfg {
                Some(mut cfg) => {
                    if cfg.dim.is_none() {
                        cfg.dim = embedding_dim(&cfg.model);
                    }
                    Ok(Arc::new(OpenAiCompatibleEmbedding::new(cfg)?))
                }
                None => Err(Error::Common(A2xError::VectorSearchUnavailable(format!(
                    "Vector search is unavailable: could not load embedding model {model_name:?}.\n\n\
                     The Rust port has no sentence-transformers. Configure an OpenAI-compatible \
                     embeddings endpoint by adding an \"embedding\" object to {} \
                     (base_url, model, api_keys, optional dim), or set {EMBEDDING_BACKEND_ENV}=hashing \
                     for the offline feature-hashing backend (tests and demos only).",
                    llm_apikey_path().display()
                )))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn table_matches_python() -> TestResult {
        let m = embedding_models();
        assert_eq!(m.len(), 3);
        assert!(m.contains_key(DEFAULT_EMBEDDING_MODEL));
        assert_eq!(embedding_dim("shibing624/text2vec-base-chinese"), Some(768));
        let v = serde_json::to_value(&m)?;
        assert_eq!(v["all-MiniLM-L6-v2"]["dim"], 384);
        assert_eq!(v["all-MiniLM-L6-v2"]["language"], "en");
        Ok(())
    }

    #[tokio::test]
    async fn hashing_is_deterministic_and_normalized() -> TestResult {
        let m = HashingEmbedding::new(64);
        let a = m.embed_one("Book a flight to Tokyo").await?;
        let b = m.embed_one("book a FLIGHT to tokyo").await?;
        assert_eq!(a, b);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
        let c = m.embed_one("stock market prices").await?;
        let sim_ab: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let sim_ac: f32 = a.iter().zip(&c).map(|(x, y)| x * y).sum();
        assert!(sim_ab > sim_ac);
        assert_eq!(
            HashingEmbedding::tokens("天气 查询 api"),
            vec!["天", "气", "查", "询", "api"]
        );
        assert_eq!(m.embed_one("").await?.len(), 64);
        Ok(())
    }

    #[test]
    fn provider_config_parsing() -> TestResult {
        let v = serde_json::json!({"providers": [], "embedding": {"base_url": "http://x", "model": "m", "api_key": "k"}});
        let c = EmbeddingProviderConfig::from_config_value(&v).required()?;
        assert_eq!(c.api_keys, vec!["k"]);
        assert_eq!(c.batch_size, 100);
        assert!(EmbeddingProviderConfig::from_config_value(&serde_json::json!({"providers": []})).is_none());
        assert!("bogus".parse::<EmbeddingBackend>().is_err());
        let h = resolve_embedding_model("all-MiniLM-L6-v2", EmbeddingBackend::Hashing)?;
        assert_eq!(h.dim(), 384);
        assert_eq!(h.name(), "all-MiniLM-L6-v2");
        Ok(())
    }
}
