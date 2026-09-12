// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Prompt registry, port of `src/server/prompt_manager.*`.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use serde_json::Value;

use super::{PromptHandler, ServerContext};
use crate::error::McpError;
use crate::types::{GetPromptResult, ListPromptsResult, PromptInfo};

#[derive(Clone)]
struct PromptEntry {
    info: PromptInfo,
    handler: PromptHandler,
}

/// Thread safe prompt registry.
pub struct PromptManager {
    overwrite: bool,
    prompts: Mutex<BTreeMap<String, PromptEntry>>,
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new(true)
    }
}

impl PromptManager {
    /// Create a registry. `overwrite` allows re-adding an existing prompt.
    pub fn new(overwrite: bool) -> Self {
        Self {
            overwrite,
            prompts: Mutex::new(BTreeMap::new()),
        }
    }

    /// Number of registered prompts.
    pub fn len(&self) -> usize {
        self.prompts.lock().len()
    }

    /// True when no prompt is registered.
    pub fn is_empty(&self) -> bool {
        self.prompts.lock().is_empty()
    }

    /// Register a prompt. The name must not be empty.
    pub fn add_prompt(&self, prompt: PromptInfo, handler: PromptHandler) -> Result<(), McpError> {
        if prompt.name.is_empty() {
            return Err(McpError::argument("Prompt name cannot be empty"));
        }
        let mut prompts = self.prompts.lock();
        if prompts.contains_key(&prompt.name) {
            if !self.overwrite {
                return Err(McpError::state(format!("Prompt already exists: {}", prompt.name)));
            }
            tracing::warn!("Prompt '{}' already exists, overwriting", prompt.name);
        }
        prompts.insert(
            prompt.name.clone(),
            PromptEntry {
                info: prompt,
                handler,
            },
        );
        Ok(())
    }

    /// Remove a prompt. Removing an unknown prompt is not an error.
    pub fn remove_prompt(&self, name: &str) {
        self.prompts.lock().remove(name);
    }

    /// All prompts, sorted by name.
    pub fn list_prompts(&self) -> ListPromptsResult {
        ListPromptsResult {
            prompts: self.prompts.lock().values().map(|e| e.info.clone()).collect(),
            next_cursor: None,
            meta: None,
        }
    }

    /// Render a prompt.
    pub async fn get_prompt(
        &self,
        ctx: ServerContext,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<GetPromptResult, McpError> {
        let handler = self
            .prompts
            .lock()
            .get(name)
            .map(|e| e.handler.clone())
            .ok_or_else(|| McpError::state(format!("Prompt not found: {name}")))?;
        handler(ctx, name.to_string(), arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{prompt_handler, session::ServerSession};
    use crate::types::{ContentBlock, PromptArgument, PromptMessage, RoleType};
    use ap_support::testing::{ResultExt, TestResult};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn info(name: &str) -> PromptInfo {
        PromptInfo {
            name: name.into(),
            description: Some("A test prompt".into()),
            ..Default::default()
        }
    }

    fn handler(description: &'static str) -> PromptHandler {
        prompt_handler(move |_ctx, _name, args| async move {
            let who = args
                .as_ref()
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("world")
                .to_string();
            Ok(GetPromptResult {
                description: Some(description.to_string()),
                messages: vec![PromptMessage {
                    role: RoleType::User,
                    content: ContentBlock::text(format!("Hello, {who}!")),
                }],
                meta: None,
            })
        })
    }

    fn ctx() -> ServerContext {
        ServerContext::new(ServerSession::detached(), None)
    }

    #[tokio::test]
    async fn add_get_remove() -> TestResult {
        let m = PromptManager::default();
        assert!(m.is_empty());
        assert_eq!(
            m.add_prompt(info(""), handler("x")).err_or_fail()?.to_string(),
            "Prompt name cannot be empty"
        );
        m.add_prompt(info("prompt1"), handler("Test description"))?;
        let r = m.get_prompt(ctx(), "prompt1", None).await?;
        assert_eq!(r.description.as_deref(), Some("Test description"));
        assert_eq!(r.messages[0].content.as_text(), Some("Hello, world!"));
        let r = m
            .get_prompt(ctx(), "prompt1", Some(json!({"name": "Alice"})))
            .await?;
        assert_eq!(r.messages[0].content.as_text(), Some("Hello, Alice!"));
        assert_eq!(
            m.get_prompt(ctx(), "nonexistent", None)
                .await
                .err_or_fail()?
                .to_string(),
            "Prompt not found: nonexistent"
        );

        m.add_prompt(info("prompt1"), handler("Second handler"))?;
        assert_eq!(
            m.get_prompt(ctx(), "prompt1", None).await?.description.as_deref(),
            Some("Second handler")
        );
        m.add_prompt(info("prompt2"), handler("2"))?;
        assert_eq!(m.len(), 2);
        m.remove_prompt("prompt1");
        m.remove_prompt("missing");
        let list = m.list_prompts();
        assert_eq!(list.prompts.len(), 1);
        assert_eq!(list.prompts[0].name, "prompt2");
        Ok(())
    }

    #[tokio::test]
    async fn no_overwrite_and_handler_errors() -> TestResult {
        let m = PromptManager::new(false);
        m.add_prompt(info("p"), handler("1"))?;
        assert_eq!(
            m.add_prompt(info("p"), handler("2")).err_or_fail()?.to_string(),
            "Prompt already exists: p"
        );
        let failing = prompt_handler(|_c, _n, _a| async { Err(McpError::state("Handler exception")) });
        let m = PromptManager::default();
        m.add_prompt(info("p"), failing)?;
        assert_eq!(
            m.get_prompt(ctx(), "p", None).await.err_or_fail()?.to_string(),
            "Handler exception"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fields_preserved_and_concurrent_access() -> TestResult {
        let m = Arc::new(PromptManager::default());
        let detailed = PromptInfo {
            name: "detailed_prompt".into(),
            description: Some("A detailed prompt description".into()),
            title: Some("Detailed Prompt Title".into()),
            icons: Some(vec![crate::types::Icon {
                src: "icon.png".into(),
                ..Default::default()
            }]),
            arguments: Some(vec![PromptArgument::new("param1", "d", true)]),
        };
        m.add_prompt(detailed.clone(), handler("x"))?;
        assert_eq!(m.list_prompts().prompts[0], detailed);

        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for i in 0..8 {
            let m = m.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                m.add_prompt(info(&format!("p{i}")), handler("h"))?;
                m.get_prompt(ctx(), "detailed_prompt", None).await?;
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<(), ap_support::testing::TestError>(())
            }));
        }
        for t in tasks {
            t.await??;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 8);
        assert_eq!(m.len(), 9);
        let names: Vec<String> = m.list_prompts().prompts.iter().map(|p| p.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        Ok(())
    }
}
