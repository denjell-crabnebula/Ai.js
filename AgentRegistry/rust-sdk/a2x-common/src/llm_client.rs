// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! LLM API client with retry logic, provider failover and batch support.
//!
//! Provides a unified async interface for OpenAI-compatible chat completion
//! APIs. Providers are tried in configuration order; within each provider the
//! API keys rotate on failure. [`LlmBackend`] is the trait the build and
//! search code depends on, so tests can substitute a fake.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::errors::A2xError;
use crate::paths::{LLM_APIKEY_EXAMPLE, llm_apikey_path};

/// Configuration for a single LLM provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_keys: Vec<String>,
}

#[derive(Debug)]
struct ProviderState {
    config: ProviderConfig,
    current_key_index: AtomicUsize,
}

/// A chat message in the OpenAI wire format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Response from an LLM call. Failures are reported in-band (`success =
/// false`) so callers can degrade gracefully, mirroring the Python client.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub tokens: u64,
    pub model: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl LlmResponse {
    pub fn failure(model: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            success: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }
}

/// Usage counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmStats {
    pub total_calls: u64,
    pub total_tokens: u64,
}

/// Abstraction over a chat-completion backend so search and build code can be
/// tested without network access.
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Call the model with a full message list.
    async fn call(&self, messages: &[ChatMessage], temperature: f32, max_tokens: Option<u32>) -> LlmResponse;

    /// Primary model name.
    fn model(&self) -> String;

    fn stats(&self) -> LlmStats;

    fn reset_stats(&self);

    /// Call the model for many prompts concurrently, preserving order.
    async fn call_batch(
        &self,
        prompts: &[String],
        system_prompt: &str,
        temperature: f32,
        max_workers: usize,
    ) -> Vec<LlmResponse> {
        let workers = max_workers.max(1);
        let jobs: Vec<(usize, Vec<ChatMessage>)> = prompts
            .iter()
            .enumerate()
            .map(|(idx, prompt)| {
                let mut messages = Vec::with_capacity(2);
                if !system_prompt.is_empty() {
                    messages.push(ChatMessage::system(system_prompt));
                }
                messages.push(ChatMessage::user(prompt.clone()));
                (idx, messages)
            })
            .collect();
        let results: Vec<(usize, LlmResponse)> = stream::iter(jobs)
            .map(|(idx, messages)| async move { (idx, self.call(&messages, temperature, None).await) })
            .buffer_unordered(workers)
            .collect()
            .await;
        let mut ordered: Vec<Option<LlmResponse>> = (0..prompts.len()).map(|_| None).collect();
        for (idx, r) in results {
            ordered[idx] = Some(r);
        }
        ordered.into_iter().map(|r| r.unwrap_or_default()).collect()
    }
}

/// Options for [`LlmClient`].
#[derive(Clone, Debug)]
pub struct LlmClientOptions {
    /// Maximum retry attempts per API key.
    pub max_retries: u32,
    /// Request timeout.
    pub timeout: Duration,
    /// Base for exponential backoff between retries (`base * 2^attempt`).
    /// Defaults to one second like the Python client; tests lower it.
    pub backoff_base: Duration,
}

impl Default for LlmClientOptions {
    fn default() -> Self {
        Self {
            max_retries: 3,
            timeout: Duration::from_secs(120),
            backoff_base: Duration::from_secs(1),
        }
    }
}

/// Client for OpenAI-compatible chat completion APIs with failover.
#[derive(Debug)]
pub struct LlmClient {
    providers: Vec<ProviderState>,
    options: LlmClientOptions,
    http: reqwest::Client,
    total_calls: AtomicU64,
    total_tokens: AtomicU64,
}

impl LlmClient {
    /// Load providers from `config_path`, or from the default
    /// `llm_apikey.json` location when `None`.
    pub fn new(config_path: Option<&Path>, options: LlmClientOptions) -> Result<Self, A2xError> {
        let path: PathBuf = match config_path {
            Some(p) => p.to_path_buf(),
            None => llm_apikey_path(),
        };
        let config = Self::load_config_or_raise(&path)?;
        let providers = Self::parse_providers(&config);
        if providers.is_empty() {
            return Err(A2xError::LlmNotConfigured(format!(
                "A2X build / search is unavailable: no valid LLM provider in {path:?}.\n\n\
                 The config needs a top-level \"providers\" array, and each entry needs \
                 \"base_url\", \"model\" and a non-empty \"api_keys\" list. Fill in a real API key \
                 and retry. A template is:\n{LLM_APIKEY_EXAMPLE}"
            )));
        }
        Self::from_providers(providers, options)
    }

    /// Build a client from an explicit provider list.
    pub fn from_providers(
        providers: Vec<ProviderConfig>,
        options: LlmClientOptions,
    ) -> Result<Self, A2xError> {
        if providers.is_empty() {
            return Err(A2xError::LlmNotConfigured("no LLM providers configured".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(options.timeout)
            .pool_max_idle_per_host(20)
            .build()
            .map_err(|e| A2xError::Other(format!("cannot build HTTP client: {e}")))?;
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        tracing::info!(providers = ?names, "LLMClient initialized (priority order)");
        Ok(Self {
            providers: providers
                .into_iter()
                .map(|config| ProviderState {
                    config,
                    current_key_index: AtomicUsize::new(0),
                })
                .collect(),
            options,
            http,
            total_calls: AtomicU64::new(0),
            total_tokens: AtomicU64::new(0),
        })
    }

    /// Parse a config document (the `llm_apikey.json` shape) into providers.
    /// Entries without keys are skipped with a warning. `base_url` and
    /// `model` are required so a misconfigured entry never silently sends a
    /// key to another vendor's endpoint.
    pub fn parse_providers(config: &Value) -> Vec<ProviderConfig> {
        let mut providers = Vec::new();
        let Some(list) = config.get("providers").and_then(Value::as_array) else {
            return providers;
        };
        for (i, p) in list.iter().enumerate() {
            let mut api_keys: Vec<String> = p
                .get("api_keys")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            if api_keys.is_empty() {
                if let Some(k) = p.get("api_key").and_then(Value::as_str) {
                    api_keys.push(k.to_string());
                }
            }
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("provider_{i}"));
            if api_keys.is_empty() {
                tracing::warn!(provider = %name, "provider has no API keys, skipping");
                continue;
            }
            let (Some(base_url), Some(model)) = (
                p.get("base_url").and_then(Value::as_str),
                p.get("model").and_then(Value::as_str),
            ) else {
                tracing::warn!(provider = %name, "provider is missing base_url or model, skipping");
                continue;
            };
            providers.push(ProviderConfig {
                name,
                base_url: base_url.to_string(),
                model: model.to_string(),
                api_keys,
            });
        }
        providers
    }

    fn load_config(path: &Path) -> std::io::Result<Result<Value, serde_json::Error>> {
        let content = std::fs::read_to_string(path)?;
        // Tolerate trailing commas like the Python loader.
        let content = content.replace(",\n}", "\n}").replace(",}", "}");
        Ok(serde_json::from_str(&content))
    }

    fn load_config_or_raise(path: &Path) -> Result<Value, A2xError> {
        match Self::load_config(path) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(A2xError::LlmNotConfigured(format!(
                "A2X build / search is unavailable: the LLM config at {path:?} is not valid JSON.\n\
                 Parse error at line {}, column {}: {e}",
                e.line(),
                e.column()
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(A2xError::LlmNotConfigured(format!(
                "A2X build / search is unavailable: LLM API key file not found at {path:?}.\n\n\
                 To configure:\n  1. Create the file (default location ~/.a2x_registry/llm_apikey.json, \
                 or set A2X_REGISTRY_HOME to a directory containing your own llm_apikey.json)\n  \
                 2. Fill in your provider's api_keys.\n  3. Minimal example (OpenAI-compatible):\n{LLM_APIKEY_EXAMPLE}"
            ))),
            Err(e) => Err(A2xError::LlmNotConfigured(format!(
                "A2X build / search is unavailable: cannot read the LLM config at {path:?}: {e}"
            ))),
        }
    }

    pub fn providers(&self) -> Vec<ProviderConfig> {
        self.providers.iter().map(|p| p.config.clone()).collect()
    }

    pub fn options(&self) -> &LlmClientOptions {
        &self.options
    }

    async fn post_once(
        &self,
        provider: &ProviderState,
        api_key: &str,
        payload: &Value,
    ) -> Result<(String, u64), String> {
        let resp = self
            .http
            .post(&provider.config.base_url)
            .bearer_auth(api_key)
            .header("Content-Type", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "HTTP {status}: {}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let result: Value = resp.json().await.map_err(|e| e.to_string())?;
        let content = result
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| "response has no choices[0].message.content".to_string())?
            .to_string();
        let tokens = result
            .pointer("/usage/total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok((content, tokens))
    }
}

#[async_trait]
impl LlmBackend for LlmClient {
    async fn call(&self, messages: &[ChatMessage], temperature: f32, max_tokens: Option<u32>) -> LlmResponse {
        let mut last_error: Option<String> = None;
        for provider in &self.providers {
            let mut payload = json!({
                "model": provider.config.model,
                "messages": messages,
                "temperature": temperature,
            });
            if let Some(mt) = max_tokens {
                payload["max_tokens"] = json!(mt);
            }
            let num_keys = provider.config.api_keys.len();
            for key_offset in 0..num_keys {
                let key_idx = (provider.current_key_index.load(Ordering::Relaxed) + key_offset) % num_keys;
                let api_key = &provider.config.api_keys[key_idx];
                for attempt in 0..self.options.max_retries.max(1) {
                    match self.post_once(provider, api_key, &payload).await {
                        Ok((content, tokens)) => {
                            self.total_calls.fetch_add(1, Ordering::Relaxed);
                            self.total_tokens.fetch_add(tokens, Ordering::Relaxed);
                            if key_offset > 0 {
                                provider.current_key_index.store(key_idx, Ordering::Relaxed);
                            }
                            return LlmResponse {
                                content,
                                tokens,
                                model: provider.config.model.clone(),
                                success: true,
                                error: None,
                                provider: Some(provider.config.name.clone()),
                            };
                        }
                        Err(e) => {
                            last_error = Some(e);
                            if attempt + 1 < self.options.max_retries {
                                let wait = self.options.backoff_base * 2u32.pow(attempt);
                                tracing::warn!(
                                    provider = %provider.config.name,
                                    "request failed, retrying in {:?} ({}/{})",
                                    wait,
                                    attempt + 1,
                                    self.options.max_retries
                                );
                                tokio::time::sleep(wait).await;
                            }
                        }
                    }
                }
                if key_offset + 1 < num_keys {
                    tracing::warn!(provider = %provider.config.name, "switching to API key {}/{}", key_offset + 2, num_keys);
                }
            }
            tracing::warn!(provider = %provider.config.name, "all keys exhausted, falling back to next provider");
        }
        LlmResponse::failure(self.model(), last_error.unwrap_or_else(|| "no providers".into()))
    }

    fn model(&self) -> String {
        self.providers
            .first()
            .map(|p| p.config.model.clone())
            .unwrap_or_default()
    }

    fn stats(&self) -> LlmStats {
        LlmStats {
            total_calls: self.total_calls.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
        }
    }

    fn reset_stats(&self) {
        self.total_calls.store(0, Ordering::Relaxed);
        self.total_tokens.store(0, Ordering::Relaxed);
    }
}

#[async_trait]
impl<T: LlmBackend + ?Sized> LlmBackend for Arc<T> {
    async fn call(&self, messages: &[ChatMessage], temperature: f32, max_tokens: Option<u32>) -> LlmResponse {
        (**self).call(messages, temperature, max_tokens).await
    }
    fn model(&self) -> String {
        (**self).model()
    }
    fn stats(&self) -> LlmStats {
        (**self).stats()
    }
    fn reset_stats(&self) {
        (**self).reset_stats()
    }
}

/// Return the body of the first fenced markdown code block (` ``` ` or
/// ` ```json `), trimmed, or `None` when the text has no closed fence.
fn fenced_code_block(text: &str) -> Option<&str> {
    let open = text.find("```")?;
    let after_fence = &text[open + 3..];
    let body = after_fence.strip_prefix("json").unwrap_or(after_fence);
    let close = body.find("```")?;
    Some(body[..close].trim())
}

/// Parse JSON from an LLM response, handling markdown code blocks and
/// surrounding prose. Returns `None` when no JSON object can be found.
pub fn parse_json_response(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }
    if let Some(block) = fenced_code_block(text) {
        if let Ok(v) = serde_json::from_str::<Value>(block) {
            return Some(v);
        }
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        if let Ok(v) = serde_json::from_str::<Value>(&text[start..=end]) {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use axum::{Json, Router, routing::post};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn parse_json_variants() -> TestResult {
        assert_eq!(parse_json_response(r#"{"a":1}"#), Some(json!({"a": 1})));
        assert_eq!(
            parse_json_response("Sure:\n```json\n{\"a\": 2}\n```\nDone"),
            Some(json!({"a": 2}))
        );
        assert_eq!(
            parse_json_response("prefix {\"a\": 3} suffix"),
            Some(json!({"a": 3}))
        );
        assert_eq!(parse_json_response("nothing here"), None);
        Ok(())
    }

    #[test]
    fn provider_parsing_skips_incomplete_entries() -> TestResult {
        let cfg = json!({"providers": [
            {"name": "a", "base_url": "http://x", "model": "m", "api_keys": ["k1", "k2"]},
            {"name": "nokeys", "base_url": "http://x", "model": "m"},
            {"name": "legacy", "base_url": "http://y", "model": "m2", "api_key": "single"},
            {"name": "nourl", "model": "m", "api_keys": ["k"]}
        ]});
        let p = LlmClient::parse_providers(&cfg);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].api_keys, vec!["k1", "k2"]);
        assert_eq!(p[1].api_keys, vec!["single"]);
        Ok(())
    }

    #[test]
    fn missing_config_is_actionable() -> TestResult {
        let err = LlmClient::new(
            Some(Path::new("/nonexistent/llm_apikey.json")),
            LlmClientOptions::default(),
        )
        .err()
        .required()?;
        assert!(matches!(err, A2xError::LlmNotConfigured(_)));
        assert!(err.to_string().contains("not found"));
        Ok(())
    }

    #[test]
    fn trailing_comma_config_loads() -> TestResult {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("llm_apikey.json");
        std::fs::write(
            &p,
            "{\"providers\": [{\"name\":\"a\",\"base_url\":\"http://x\",\"model\":\"m\",\"api_keys\":[\"k\"],}],\n}",
        )?;
        let c = LlmClient::new(Some(&p), LlmClientOptions::default())?;
        assert_eq!(c.model(), "m");
        Ok(())
    }

    async fn spawn_mock(fail_first: usize) -> TestResult<String> {
        let counter = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/chat",
            post({
                let counter = counter.clone();
                move |Json(body): Json<Value>| {
                    let counter = counter.clone();
                    async move {
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        if n < fail_first {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "boom"})));
                        }
                        let user = body["messages"]
                            .as_array()
                            .and_then(|messages| messages.last())
                            .map(|message| message["content"].clone())
                            .unwrap_or(Value::Null);
                        (
                            axum::http::StatusCode::OK,
                            Json(json!({
                                "choices": [{"message": {"role": "assistant", "content": format!("echo:{}", user.as_str().unwrap_or(""))}}],
                                "usage": {"total_tokens": 7}
                            })),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(format!("http://{addr}/chat"))
    }

    fn opts() -> LlmClientOptions {
        LlmClientOptions {
            max_retries: 2,
            timeout: Duration::from_secs(5),
            backoff_base: Duration::from_millis(1),
        }
    }

    #[tokio::test]
    async fn call_and_batch_against_mock() -> TestResult {
        let url = spawn_mock(0).await?;
        let client = LlmClient::from_providers(
            vec![ProviderConfig {
                name: "mock".into(),
                base_url: url,
                model: "m".into(),
                api_keys: vec!["k".into()],
            }],
            opts(),
        )?;
        let r = client.call(&[ChatMessage::user("hi")], 0.0, Some(10)).await;
        assert!(r.success);
        assert_eq!(r.content, "echo:hi");
        assert_eq!(r.tokens, 7);
        assert_eq!(r.provider.as_deref(), Some("mock"));

        let batch = client
            .call_batch(&["a".into(), "b".into(), "c".into()], "sys", 0.0, 2)
            .await;
        let contents: Vec<_> = batch.iter().map(|r| r.content.as_str()).collect();
        assert_eq!(contents, vec!["echo:a", "echo:b", "echo:c"]);
        assert_eq!(client.stats().total_calls, 4);
        assert_eq!(client.stats().total_tokens, 28);
        client.reset_stats();
        assert_eq!(client.stats(), LlmStats::default());
        Ok(())
    }

    #[tokio::test]
    async fn failover_to_second_provider_and_key_rotation() -> TestResult {
        let bad = spawn_mock(usize::MAX).await?;
        let good = spawn_mock(0).await?;
        let client = LlmClient::from_providers(
            vec![
                ProviderConfig {
                    name: "bad".into(),
                    base_url: bad,
                    model: "m1".into(),
                    api_keys: vec!["k1".into(), "k2".into()],
                },
                ProviderConfig {
                    name: "good".into(),
                    base_url: good,
                    model: "m2".into(),
                    api_keys: vec!["k".into()],
                },
            ],
            opts(),
        )?;
        let r = client.call(&[ChatMessage::user("x")], 0.0, None).await;
        assert!(r.success);
        assert_eq!(r.provider.as_deref(), Some("good"));
        assert_eq!(r.model, "m2");
        Ok(())
    }

    #[tokio::test]
    async fn all_providers_failing_reports_error() -> TestResult {
        let bad = spawn_mock(usize::MAX).await?;
        let client = LlmClient::from_providers(
            vec![ProviderConfig {
                name: "bad".into(),
                base_url: bad,
                model: "m1".into(),
                api_keys: vec!["k1".into()],
            }],
            opts(),
        )?;
        let r = client.call(&[ChatMessage::user("x")], 0.0, None).await;
        assert!(!r.success);
        assert!(r.error.required()?.contains("HTTP 500"));
        assert_eq!(client.stats().total_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn retry_recovers_from_transient_failure() -> TestResult {
        let flaky = spawn_mock(1).await?;
        let client = LlmClient::from_providers(
            vec![ProviderConfig {
                name: "flaky".into(),
                base_url: flaky,
                model: "m".into(),
                api_keys: vec!["k".into()],
            }],
            opts(),
        )?;
        let r = client.call(&[ChatMessage::user("x")], 0.0, None).await;
        assert!(r.success);
        Ok(())
    }
}
