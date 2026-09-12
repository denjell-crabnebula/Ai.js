// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Deterministic LLM double for tests and offline demos.
//!
//! [`FakeLlm`] answers from scripted rules. Each rule matches a substring
//! of the prompt (system and user messages joined) or runs a closure. The
//! first matching rule wins; unmatched prompts get the default response.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use a2x_common::{ChatMessage, LlmBackend, LlmResponse, LlmStats};
use async_trait::async_trait;
use parking_lot::Mutex;

type RuleFn = dyn Fn(&str) -> Option<String> + Send + Sync;

enum Rule {
    Substring { needle: String, response: String },
    Fail { needle: String, error: String },
    Func(Box<RuleFn>),
}

type ObserverFn = dyn Fn(&str) + Send + Sync;

/// Scripted [`LlmBackend`].
pub struct FakeLlm {
    observers: Mutex<Vec<Box<ObserverFn>>>,
    rules: Mutex<Vec<Rule>>,
    default_response: Mutex<String>,
    prompts: Mutex<Vec<String>>,
    calls: AtomicU64,
    tokens: AtomicU64,
    tokens_per_call: u64,
}

impl Default for FakeLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeLlm {
    pub fn new() -> Self {
        Self {
            observers: Mutex::new(Vec::new()),
            rules: Mutex::new(Vec::new()),
            default_response: Mutex::new("NONE".to_string()),
            prompts: Mutex::new(Vec::new()),
            calls: AtomicU64::new(0),
            tokens: AtomicU64::new(0),
            tokens_per_call: 10,
        }
    }

    /// Wrap in an `Arc` for injection.
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Respond with `response` when the prompt contains `needle`.
    pub fn on(self, needle: impl Into<String>, response: impl Into<String>) -> Self {
        self.rules.lock().push(Rule::Substring {
            needle: needle.into(),
            response: response.into(),
        });
        self
    }

    /// Return a failed response when the prompt contains `needle`.
    pub fn fail_on(self, needle: impl Into<String>, error: impl Into<String>) -> Self {
        self.rules.lock().push(Rule::Fail {
            needle: needle.into(),
            error: error.into(),
        });
        self
    }

    /// Respond with the closure's output when it returns `Some`.
    pub fn on_fn<F>(self, f: F) -> Self
    where
        F: Fn(&str) -> Option<String> + Send + Sync + 'static,
    {
        self.rules.lock().push(Rule::Func(Box::new(f)));
        self
    }

    /// Run `f` on every prompt before rule matching (for side effects
    /// such as cancelling a build).
    pub fn observe<F>(self, f: F) -> Self
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        self.observers.lock().push(Box::new(f));
        self
    }

    /// Response for prompts no rule matches (default `"NONE"`).
    pub fn with_default(self, response: impl Into<String>) -> Self {
        *self.default_response.lock() = response.into();
        self
    }

    /// Every prompt seen so far, in call order.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().clone()
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    /// Number of prompts containing `needle`.
    pub fn count_prompts_containing(&self, needle: &str) -> usize {
        self.prompts.lock().iter().filter(|p| p.contains(needle)).count()
    }

    fn answer(&self, prompt: &str) -> LlmResponse {
        let rules = self.rules.lock();
        for rule in rules.iter() {
            match rule {
                Rule::Substring { needle, response } if prompt.contains(needle.as_str()) => {
                    return self.ok(response.clone());
                }
                Rule::Fail { needle, error } if prompt.contains(needle.as_str()) => {
                    return LlmResponse::failure("fake", error.clone());
                }
                Rule::Func(f) => {
                    if let Some(r) = f(prompt) {
                        return self.ok(r);
                    }
                }
                _ => {}
            }
        }
        let default = self.default_response.lock().clone();
        self.ok(default)
    }

    fn ok(&self, content: String) -> LlmResponse {
        LlmResponse {
            content,
            tokens: self.tokens_per_call,
            model: "fake".into(),
            success: true,
            error: None,
            provider: Some("fake".into()),
        }
    }
}

#[async_trait]
impl LlmBackend for FakeLlm {
    async fn call(
        &self,
        messages: &[ChatMessage],
        _temperature: f32,
        _max_tokens: Option<u32>,
    ) -> LlmResponse {
        let prompt = messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        self.prompts.lock().push(prompt.clone());
        for observer in self.observers.lock().iter() {
            observer(&prompt);
        }
        let response = self.answer(&prompt);
        self.calls.fetch_add(1, Ordering::Relaxed);
        if response.success {
            self.tokens.fetch_add(response.tokens, Ordering::Relaxed);
        }
        response
    }

    fn model(&self) -> String {
        "fake".into()
    }

    fn stats(&self) -> LlmStats {
        LlmStats {
            total_calls: self.calls.load(Ordering::Relaxed),
            total_tokens: self.tokens.load(Ordering::Relaxed),
        }
    }

    fn reset_stats(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.tokens.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[tokio::test]
    async fn rules_match_in_order() -> TestResult {
        let llm = FakeLlm::new()
            .on("hello", "1")
            .fail_on("boom", "down")
            .on_fn(|p| p.contains("dyn").then(|| "2".to_string()));
        let r = llm.call(&[ChatMessage::user("say hello")], 0.0, None).await;
        assert_eq!(r.content, "1");
        let r = llm.call(&[ChatMessage::user("boom")], 0.0, None).await;
        assert!(!r.success);
        let r = llm.call(&[ChatMessage::user("dyn")], 0.0, None).await;
        assert_eq!(r.content, "2");
        let r = llm.call(&[ChatMessage::user("nothing")], 0.0, None).await;
        assert_eq!(r.content, "NONE");
        assert_eq!(llm.calls(), 4);
        assert_eq!(llm.count_prompts_containing("boom"), 1);
        Ok(())
    }
}
