// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Message helpers, the port of `src/utils/utils_message.h`.

use crate::types::{Message, Part, Role};

use super::generate_uuid;

/// Build an agent text message with a generated message id.
pub fn new_agent_text_message(text: &str, context_id: Option<String>, task_id: Option<String>) -> Message {
    let part = Part::text(text).with_media_type("text/plain");
    Message {
        role: Role::Agent,
        parts: vec![part],
        message_id: generate_uuid(),
        task_id,
        context_id,
        ..Default::default()
    }
}

/// Build an agent message from parts with a generated message id.
pub fn new_agent_parts_message(
    parts: Vec<Part>,
    context_id: Option<String>,
    task_id: Option<String>,
) -> Message {
    Message {
        role: Role::Agent,
        parts,
        message_id: generate_uuid(),
        task_id,
        context_id,
        ..Default::default()
    }
}

/// Collect the text of all text parts.
pub fn get_text_parts(parts: &[Part]) -> Vec<String> {
    parts.iter().filter_map(|p| p.text.clone()).collect()
}

/// Join the text parts of a message with a delimiter.
pub fn get_message_text(message: &Message, delimiter: &str) -> String {
    get_text_parts(&message.parts).join(delimiter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn text_message_basic() -> TestResult {
        let m = new_agent_text_message("hi", Some("ctx".into()), Some("task".into()));
        assert_eq!(m.role, Role::Agent);
        assert_eq!(m.parts.len(), 1);
        assert_eq!(m.parts[0].text.as_deref(), Some("hi"));
        assert_eq!(m.parts[0].media_type.as_deref(), Some("text/plain"));
        assert_eq!(m.context_id.as_deref(), Some("ctx"));
        assert_eq!(m.task_id.as_deref(), Some("task"));
        assert!(!m.message_id.is_empty());
        Ok(())
    }

    #[test]
    fn parts_message_and_text_extraction() -> TestResult {
        let parts = vec![
            Part::text("a"),
            Part::data(serde_json::json!({"k": 1})),
            Part::text("b"),
        ];
        let m = new_agent_parts_message(parts, None, None);
        assert_eq!(get_text_parts(&m.parts), vec!["a", "b"]);
        assert_eq!(get_message_text(&m, "\n"), "a\nb");
        assert_eq!(get_message_text(&m, ", "), "a, b");
        assert_eq!(get_message_text(&Message::default(), "\n"), "");
        Ok(())
    }
}
