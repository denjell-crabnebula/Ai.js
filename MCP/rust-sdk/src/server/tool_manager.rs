// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Tool registry, port of `src/server/tool_manager.*`.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use serde_json::Value;

use super::{ServerContext, ToolHandler};
use crate::error::McpError;
use crate::types::{CallToolResult, DEFAULT_TOOLS_PAGE_SIZE, Icon, ListToolsResult, Tool, ToolAnnotations};

/// A registered tool.
#[derive(Clone)]
pub struct ServerTool {
    /// Unique name.
    pub name: String,
    /// Handler.
    pub handler: ToolHandler,
    /// Title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// JSON schema of the arguments, validated before the handler runs.
    pub input_schema: Option<Value>,
    /// JSON schema of `structuredContent`, validated after the handler runs.
    pub output_schema: Option<Value>,
    /// Unused flag kept for parity with `AddToolOptionalParams::structuredOutput`.
    pub structured_output: bool,
    /// Behaviour hints.
    pub annotations: Option<ToolAnnotations>,
    /// Icons.
    pub icons: Option<Vec<Icon>>,
}

impl ServerTool {
    /// A tool with only a name and a handler.
    pub fn new(name: impl Into<String>, handler: ToolHandler) -> Self {
        Self {
            name: name.into(),
            handler,
            title: None,
            description: None,
            input_schema: None,
            output_schema: None,
            structured_output: false,
            annotations: None,
            icons: None,
        }
    }

    /// The public tool description.
    pub fn to_tool(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
            annotations: self.annotations.clone(),
            icons: self.icons.clone(),
        }
    }
}

/// Decode a pagination cursor as a start index; invalid values fall back to 0.
pub(crate) fn cursor_start(cursor: Option<&str>, len: usize) -> usize {
    let start = cursor
        .and_then(|c| c.trim().parse::<i64>().ok())
        .map(|v| if v < 0 { 0 } else { v as usize })
        .unwrap_or(0);
    start.min(len)
}

/// Thread safe tool registry with cursor pagination.
pub struct ToolManager {
    overwrite: Mutex<bool>,
    tools: Mutex<BTreeMap<String, ServerTool>>,
    page_size: Mutex<usize>,
}

impl Default for ToolManager {
    fn default() -> Self {
        Self::new(true, DEFAULT_TOOLS_PAGE_SIZE)
    }
}

impl ToolManager {
    /// Create a registry. `overwrite` allows re-adding an existing tool.
    pub fn new(overwrite: bool, page_size: usize) -> Self {
        Self {
            overwrite: Mutex::new(overwrite),
            tools: Mutex::new(BTreeMap::new()),
            page_size: Mutex::new(page_size),
        }
    }

    /// Allow or forbid overwriting existing tools.
    pub fn set_overwrite(&self, overwrite: bool) {
        *self.overwrite.lock() = overwrite;
    }

    /// Current overwrite setting.
    pub fn overwrite(&self) -> bool {
        *self.overwrite.lock()
    }

    /// Set the page size of `list_tools`.
    pub fn set_page_size(&self, page_size: usize) {
        *self.page_size.lock() = page_size;
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.lock().len()
    }

    /// True when no tool is registered.
    pub fn is_empty(&self) -> bool {
        self.tools.lock().is_empty()
    }

    /// Register a tool.
    pub fn add_tool(&self, tool: ServerTool) -> Result<(), McpError> {
        let mut tools = self.tools.lock();
        if tools.contains_key(&tool.name) {
            if !self.overwrite() {
                return Err(McpError::state(format!("Tool '{}' already exists", tool.name)));
            }
            tracing::warn!("Tool '{}' already exists, overwriting", tool.name);
        }
        tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    /// Remove a tool.
    pub fn remove_tool(&self, name: &str) -> Result<(), McpError> {
        if name.is_empty() {
            return Err(McpError::argument("Tool name cannot be empty"));
        }
        let mut tools = self.tools.lock();
        if tools.remove(name).is_none() {
            return Err(McpError::state(format!("Tool '{name}' not found")));
        }
        Ok(())
    }

    /// One page of tools. Names are sorted; the cursor is a start index.
    pub fn list_tools(&self, cursor: Option<&str>) -> ListToolsResult {
        let tools = self.tools.lock();
        let names: Vec<&String> = tools.keys().collect();
        let start = cursor_start(cursor, names.len());
        let end = (start + *self.page_size.lock()).min(names.len());
        let page = names[start..end]
            .iter()
            .filter_map(|n| tools.get(*n))
            .map(ServerTool::to_tool)
            .collect();
        ListToolsResult {
            tools: page,
            next_cursor: (end < names.len()).then(|| end.to_string()),
            meta: None,
        }
    }

    /// Call a tool: validate the input, run the handler, validate the output.
    ///
    /// Handler failures become `isError` results; validation failures and
    /// unknown tools are returned as errors, matching the C++ exceptions.
    pub async fn call_tool(
        &self,
        ctx: ServerContext,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let tool = self
            .tools
            .lock()
            .get(name)
            .cloned()
            .ok_or_else(|| McpError::state(format!("Tool not found: {name}")))?;
        if let Some(schema) = &tool.input_schema {
            crate::schema::validate(schema, &arguments).map_err(|e| {
                McpError::Validation(format!("Input validation failed for tool '{name}': {e}"))
            })?;
        }
        let result = match (tool.handler)(ctx, name.to_string(), arguments).await {
            Ok(r) => r,
            Err(e) => CallToolResult::error(e.message()),
        };
        if let Some(schema) = &tool.output_schema {
            if !result.is_error {
                if let Some(structured) = &result.structured_content {
                    if !structured.is_object() {
                        return Err(McpError::Validation(format!(
                            "Output validation failed for tool '{name}': structuredContent must be a JSON object for tool '{name}'"
                        )));
                    }
                    crate::schema::validate(schema, structured).map_err(|e| {
                        McpError::Validation(format!("Output validation failed for tool '{name}': {e}"))
                    })?;
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{ServerContext, tool_handler};
    use crate::types::ContentBlock;
    use ap_support::testing::{ResultExt, TestResult};
    use serde_json::json;
    use std::sync::Arc;

    fn echo() -> ToolHandler {
        tool_handler(|_ctx, _name, args| async move {
            let q = args
                .get("user_query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(CallToolResult::text(format!("Echo: {q}")))
        })
    }

    fn make(name: &str) -> ServerTool {
        let mut t = ServerTool::new(name, echo());
        t.description = Some("First tool".into());
        t
    }

    fn ctx() -> ServerContext {
        ServerContext::new(crate::server::session::ServerSession::detached(), None)
    }

    #[test]
    fn add_remove_and_overwrite() -> TestResult {
        let m = ToolManager::default();
        assert!(m.overwrite());
        m.add_tool(make("tool1"))?;
        m.add_tool(make("tool1"))?;
        m.set_overwrite(false);
        assert!(!m.overwrite());
        let err = m.add_tool(make("tool1")).err_or_fail()?;
        assert_eq!(err.to_string(), "Tool 'tool1' already exists");
        assert_eq!(
            m.remove_tool("").err_or_fail()?.to_string(),
            "Tool name cannot be empty"
        );
        assert_eq!(
            m.remove_tool("nope").err_or_fail()?.to_string(),
            "Tool 'nope' not found"
        );
        m.remove_tool("tool1")?;
        assert!(m.remove_tool("tool1").is_err());
        assert!(m.is_empty());
        let long = "a".repeat(1000);
        m.add_tool(make(&long))?;
        assert_eq!(m.list_tools(None).tools[0].name, long);
        Ok(())
    }

    #[test]
    fn list_tools_is_sorted_and_paginated() -> TestResult {
        let m = ToolManager::new(true, 2);
        for name in ["c", "a", "b", "d", "e"] {
            m.add_tool(make(name))?;
        }
        assert_eq!(m.len(), 5);
        let p1 = m.list_tools(None);
        assert_eq!(
            p1.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(p1.tools[0].description.as_deref(), Some("First tool"));
        assert_eq!(p1.next_cursor.as_deref(), Some("2"));
        let p2 = m.list_tools(p1.next_cursor.as_deref());
        assert_eq!(
            p2.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["c", "d"]
        );
        let p3 = m.list_tools(p2.next_cursor.as_deref());
        assert_eq!(p3.tools.len(), 1);
        assert!(p3.next_cursor.is_none());
        assert_eq!(m.list_tools(Some("garbage")).tools.len(), 2);
        assert_eq!(m.list_tools(Some("100")).tools.len(), 0);
        m.set_page_size(50);
        assert_eq!(m.list_tools(None).tools.len(), 5);
        let empty = ToolManager::default();
        assert!(empty.list_tools(None).tools.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn call_tool_paths() -> TestResult {
        let m = ToolManager::default();
        let mut t = make("echo");
        t.input_schema = Some(
            json!({"type": "object", "properties": {"user_query": {"type": "string"}},
            "required": ["user_query"]}),
        );
        m.add_tool(t)?;
        let r = m.call_tool(ctx(), "echo", json!({"user_query": "hi"})).await?;
        assert_eq!(r.content[0].as_text(), Some("Echo: hi"));
        assert!(!r.is_error);
        let err = m.call_tool(ctx(), "echo", json!({})).await.err_or_fail()?;
        assert!(
            err.to_string()
                .starts_with("Input validation failed for tool 'echo'")
        );
        assert_eq!(
            m.call_tool(ctx(), "nope", json!({}))
                .await
                .err_or_fail()?
                .to_string(),
            "Tool not found: nope"
        );
        assert!(m.call_tool(ctx(), "", json!({})).await.is_err());

        // A failing handler becomes an isError result.
        let failing = ServerTool::new(
            "fail",
            tool_handler(|_c, _n, _a| async { Err(McpError::state("Simulated tool error")) }),
        );
        m.add_tool(failing)?;
        let r = m.call_tool(ctx(), "fail", json!({})).await?;
        assert!(r.is_error);
        assert_eq!(r.content[0].as_text(), Some("Simulated tool error"));
        assert!(matches!(r.content[0], ContentBlock::Text(_)));
        m.remove_tool("fail")?;
        assert!(m.call_tool(ctx(), "fail", json!({})).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn output_schema_validation() -> TestResult {
        let structured = || {
            tool_handler(|_c, _n, args: Value| async move {
                if let Some(v) = args.get("returnData") {
                    return Ok(CallToolResult::from_structured(v.clone()));
                }
                Ok(CallToolResult::from_structured(
                    json!({"status": "success", "code": 200}),
                ))
            })
        };
        let m = ToolManager::default();
        let mut pass = ServerTool::new("test", structured());
        pass.output_schema = Some(
            json!({"type": "object", "properties": {"status": {"type": "string"},
            "code": {"type": "number"}}, "required": ["status", "code"]}),
        );
        m.add_tool(pass)?;
        assert!(m.call_tool(ctx(), "test", json!({})).await.is_ok());

        let mut fail = ServerTool::new("test", structured());
        fail.output_schema = Some(
            json!({"type": "object", "properties": {"required_field": {"type": "string"}},
            "required": ["required_field"]}),
        );
        m.add_tool(fail)?;
        let err = m.call_tool(ctx(), "test", json!({})).await.err_or_fail()?;
        assert!(
            err.to_string()
                .starts_with("Output validation failed for tool 'test'")
        );

        let mut not_object = ServerTool::new("test", structured());
        not_object.output_schema = Some(json!({"type": "object"}));
        m.add_tool(not_object)?;
        let err = m
            .call_tool(ctx(), "test", json!({"returnData": [1, 2, 3]}))
            .await
            .err_or_fail()?;
        assert!(
            err.to_string()
                .contains("structuredContent must be a JSON object")
        );

        let mut no_structured = ServerTool::new(
            "test",
            tool_handler(|_c, _n, _a| async { Ok(CallToolResult::text("no structured")) }),
        );
        no_structured.output_schema = Some(json!({"type": "object"}));
        m.add_tool(no_structured)?;
        assert!(m.call_tool(ctx(), "test", json!({})).await.is_ok());

        let mut error_result = ServerTool::new(
            "test",
            tool_handler(|_c, _n, _a| async {
                Ok(CallToolResult {
                    is_error: true,
                    structured_content: Some(json!({"invalid": "data"})),
                    ..Default::default()
                })
            }),
        );
        error_result.output_schema = Some(
            json!({"type": "object", "properties": {"required": {"type": "string"}},
            "required": ["required"]}),
        );
        m.add_tool(error_result)?;
        assert!(m.call_tool(ctx(), "test", json!({})).await.is_ok());
        let _ = Arc::new(0);
        Ok(())
    }
}
