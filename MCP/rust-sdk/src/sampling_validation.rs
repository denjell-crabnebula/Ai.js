// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Validation of `tool_use` and `tool_result` sequences in sampling messages.
//!
//! Port of `src/shared/sampling_validation.cpp`. The rules come from the MCP
//! sampling specification:
//!
//! 1. A message containing `tool_result` blocks must contain only such blocks.
//! 2. A `tool_result` message must follow an assistant message with `tool_use` blocks.
//! 3. The `toolUseId` set must match the `tool_use.id` set of the previous message.

use std::collections::BTreeSet;

use crate::error::McpError;
use crate::types::{RoleType, SamplingMessage, SamplingMessageContentBlock};

const MIXED_CONTENT: &str = "Tool results mixed with other content";
const MISSING_RESULT: &str = "Tool result missing in request";

fn contains_tool_use(blocks: &[SamplingMessageContentBlock]) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, SamplingMessageContentBlock::ToolUse(_)))
}

fn contains_tool_result(blocks: &[SamplingMessageContentBlock]) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, SamplingMessageContentBlock::ToolResult(_)))
}

fn tool_use_ids(blocks: &[SamplingMessageContentBlock]) -> BTreeSet<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            SamplingMessageContentBlock::ToolUse(t) => Some(t.id.clone()),
            _ => None,
        })
        .collect()
}

fn tool_result_ids(blocks: &[SamplingMessageContentBlock]) -> BTreeSet<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            SamplingMessageContentBlock::ToolResult(t) => Some(t.tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn ensure_only_tool_results(blocks: &[SamplingMessageContentBlock]) -> Result<(), McpError> {
    if blocks
        .iter()
        .all(|b| matches!(b, SamplingMessageContentBlock::ToolResult(_)))
    {
        Ok(())
    } else {
        Err(McpError::argument(MIXED_CONTENT))
    }
}

/// Validate the `tool_use`/`tool_result` structure of a message list.
///
/// Errors carry the same messages as the C++ implementation.
pub fn validate_tool_use_result_messages(messages: &[SamplingMessage]) -> Result<(), McpError> {
    for (i, message) in messages.iter().enumerate() {
        let blocks = message.content.as_list();
        let has_tool_use = contains_tool_use(&blocks);
        let has_tool_result = contains_tool_result(&blocks);

        if has_tool_result {
            ensure_only_tool_results(&blocks)?;
            if message.role != RoleType::User {
                return Err(McpError::argument(MIXED_CONTENT));
            }
            if i == 0 {
                return Err(McpError::argument(MISSING_RESULT));
            }
            let prev = messages[i - 1].content.as_list();
            let use_ids = tool_use_ids(&prev);
            if use_ids.is_empty() {
                return Err(McpError::argument(MISSING_RESULT));
            }
            if use_ids != tool_result_ids(&blocks) {
                return Err(McpError::argument(MISSING_RESULT));
            }
        }

        if has_tool_use {
            if message.role != RoleType::Assistant {
                return Err(McpError::argument(MISSING_RESULT));
            }
            let Some(next) = messages.get(i + 1) else {
                return Err(McpError::argument(MISSING_RESULT));
            };
            let next_blocks = next.content.as_list();
            if !contains_tool_result(&next_blocks) {
                return Err(McpError::argument(MISSING_RESULT));
            }
            ensure_only_tool_results(&next_blocks)?;
            if tool_use_ids(&blocks) != tool_result_ids(&next_blocks) {
                return Err(McpError::argument(MISSING_RESULT));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SamplingContent, TextContent, ToolResultContent, ToolUseContent};
    use ap_support::testing::{ResultExt, TestResult};

    fn text(role: RoleType, t: &str) -> SamplingMessage {
        SamplingMessage {
            role,
            content: SamplingContent::Single(SamplingMessageContentBlock::Text(TextContent::new(t))),
            meta: None,
        }
    }

    fn tool_use(id: &str) -> SamplingMessage {
        SamplingMessage {
            role: RoleType::Assistant,
            content: SamplingContent::Single(SamplingMessageContentBlock::ToolUse(ToolUseContent {
                id: id.into(),
                name: "t".into(),
                ..Default::default()
            })),
            meta: None,
        }
    }

    fn tool_result(id: &str) -> SamplingMessage {
        SamplingMessage {
            role: RoleType::User,
            content: SamplingContent::Single(SamplingMessageContentBlock::ToolResult(ToolResultContent {
                tool_use_id: id.into(),
                ..Default::default()
            })),
            meta: None,
        }
    }

    #[test]
    fn plain_messages_are_valid() -> TestResult {
        assert!(validate_tool_use_result_messages(&[text(RoleType::User, "hi")]).is_ok());
        assert!(validate_tool_use_result_messages(&[]).is_ok());
        Ok(())
    }

    #[test]
    fn valid_tool_loop() -> TestResult {
        let msgs = [
            text(RoleType::User, "q"),
            tool_use("a"),
            tool_result("a"),
            text(RoleType::Assistant, "done"),
        ];
        assert!(validate_tool_use_result_messages(&msgs).is_ok());
        Ok(())
    }

    #[test]
    fn tool_result_without_tool_use_fails() -> TestResult {
        let err = validate_tool_use_result_messages(&[tool_result("x")]).err_or_fail()?;
        assert_eq!(err.to_string(), MISSING_RESULT);
        let err = validate_tool_use_result_messages(&[text(RoleType::Assistant, "n"), tool_result("x")])
            .err_or_fail()?;
        assert_eq!(err.to_string(), MISSING_RESULT);
        Ok(())
    }

    #[test]
    fn mixed_tool_result_content_fails() -> TestResult {
        let mixed = SamplingMessage {
            role: RoleType::User,
            content: SamplingContent::Multiple(vec![
                SamplingMessageContentBlock::ToolResult(ToolResultContent {
                    tool_use_id: "a".into(),
                    ..Default::default()
                }),
                SamplingMessageContentBlock::Text(TextContent::new("extra")),
            ]),
            meta: None,
        };
        let err = validate_tool_use_result_messages(&[tool_use("a"), mixed]).err_or_fail()?;
        assert_eq!(err.to_string(), MIXED_CONTENT);
        Ok(())
    }

    #[test]
    fn id_mismatch_and_missing_follow_up_fail() -> TestResult {
        let err = validate_tool_use_result_messages(&[tool_use("a"), tool_result("b")]).err_or_fail()?;
        assert_eq!(err.to_string(), MISSING_RESULT);
        let err = validate_tool_use_result_messages(&[tool_use("a")]).err_or_fail()?;
        assert_eq!(err.to_string(), MISSING_RESULT);
        let mut wrong_role = tool_use("a");
        wrong_role.role = RoleType::User;
        let err = validate_tool_use_result_messages(&[wrong_role, tool_result("a")]).err_or_fail()?;
        assert_eq!(err.to_string(), MISSING_RESULT);
        Ok(())
    }
}
