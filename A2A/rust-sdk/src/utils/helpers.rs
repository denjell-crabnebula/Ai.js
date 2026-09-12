// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Task helpers, the port of `src/server/utils_helpers.h`.

use ap_jsonrpc::{RequestId, Response, RpcError};

use crate::error::A2aServerError;
use crate::types::{
    Artifact, MessageSendParams, Part, StreamEvent, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
};

use super::generate_uuid;

/// Create a submitted task for a send request, generating a context id if needed.
pub fn create_task_obj(params: &mut MessageSendParams) -> Task {
    if params.message.context_id.is_none() {
        params.message.context_id = Some(generate_uuid());
    }
    Task {
        id: generate_uuid(),
        context_id: params.message.context_id.clone().unwrap_or_default(),
        status: TaskStatus::new(TaskState::Submitted),
        history: Some(vec![params.message.clone()]),
        ..Default::default()
    }
}

/// Apply an artifact update to a task (server semantics).
///
/// Without `append` the artifact replaces one with the same id or is added.
/// With `append` the parts are appended to an existing artifact; a chunk for
/// an unknown artifact is ignored.
pub fn append_artifact_to_task(task: &mut Task, event: &TaskArtifactUpdateEvent) {
    let list = task.artifacts.get_or_insert_with(Vec::new);
    let new_artifact = &event.artifact;
    let append_parts = event.append.unwrap_or(false);
    let existing = list
        .iter_mut()
        .find(|a| a.artifact_id == new_artifact.artifact_id);
    if !append_parts {
        match existing {
            Some(slot) => *slot = new_artifact.clone(),
            None => list.push(new_artifact.clone()),
        }
        return;
    }
    if let Some(slot) = existing {
        slot.parts.extend(new_artifact.parts.iter().cloned());
    }
}

/// Whether the client and server output modes overlap. Empty lists match everything.
pub fn are_modalities_compatible(
    server_output_modes: Option<&[String]>,
    client_output_modes: Option<&[String]>,
) -> bool {
    let client = match client_output_modes {
        Some(c) if !c.is_empty() => c,
        _ => return true,
    };
    let server = match server_output_modes {
        Some(s) if !s.is_empty() => s,
        _ => return true,
    };
    client.iter().any(|c| server.contains(c))
}

/// Create a simple text artifact with the provided id.
pub fn build_text_artifact(text: &str, artifact_id: &str) -> Artifact {
    Artifact {
        artifact_id: artifact_id.to_string(),
        parts: vec![Part::text(text).with_media_type("text/plain")],
        ..Default::default()
    }
}

/// Whether an event ends a streaming exchange.
pub fn is_final_event(ev: &StreamEvent) -> bool {
    match ev {
        StreamEvent::Task(t) => {
            let st = t.status.state;
            is_final_or_interrupted(st) || st == TaskState::Unspecified
        }
        StreamEvent::Message(_) => true,
        StreamEvent::StatusUpdate(e) => is_final_or_interrupted(e.status.state),
        StreamEvent::ArtifactUpdate(_) => false,
    }
}

/// Return an internal server error when the expression is false.
pub fn validate_or_throw(expr: bool, error_message: &str) -> Result<(), A2aServerError> {
    if expr {
        Ok(())
    } else {
        Err(A2aServerError::new(error_message))
    }
}

/// Whether the state is terminal.
pub fn is_final(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Canceled | TaskState::Failed | TaskState::Rejected
    )
}

/// Whether the state waits for the user.
pub fn is_interrupted(state: TaskState) -> bool {
    matches!(state, TaskState::InputRequired | TaskState::AuthRequired)
}

/// Whether the state is terminal or waits for the user.
pub fn is_final_or_interrupted(state: TaskState) -> bool {
    is_final(state) || is_interrupted(state)
}

/// Build a JSON-RPC error response.
pub fn make_error(id: Option<RequestId>, code: i64, msg: &str) -> Response {
    Response::error(id, RpcError::new(code, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, TaskStatusUpdateEvent};
    use ap_support::testing::{OptionExt, TestResult};

    fn artifact(id: &str, texts: &[&str]) -> Artifact {
        Artifact {
            artifact_id: id.to_string(),
            parts: texts.iter().map(|t| Part::text(*t)).collect(),
            ..Default::default()
        }
    }

    fn event(id: &str, texts: &[&str], append: Option<bool>) -> TaskArtifactUpdateEvent {
        TaskArtifactUpdateEvent {
            artifact: artifact(id, texts),
            context_id: "ctx".into(),
            task_id: "task".into(),
            append,
            ..Default::default()
        }
    }

    #[test]
    fn create_task_obj_generates_context_id() -> TestResult {
        let mut params = MessageSendParams {
            message: Message {
                message_id: "m".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let t = create_task_obj(&mut params);
        assert!(!t.id.is_empty());
        assert!(!t.context_id.is_empty());
        assert_eq!(params.message.context_id.as_deref(), Some(t.context_id.as_str()));
        assert_eq!(t.status.state, TaskState::Submitted);
        assert_eq!(t.history.as_ref().map(Vec::len), Some(1));

        params.message.context_id = Some("keep".into());
        let t2 = create_task_obj(&mut params);
        assert_eq!(t2.context_id, "keep");
        Ok(())
    }

    #[test]
    fn append_artifact_replace_and_append() -> TestResult {
        let mut task = Task::default();
        append_artifact_to_task(&mut task, &event("a", &["1"], None));
        assert_eq!(task.artifacts.as_ref().required()?.len(), 1);
        append_artifact_to_task(&mut task, &event("a", &["2"], Some(false)));
        let arts = task.artifacts.as_ref().required()?;
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].parts[0].text.as_deref(), Some("2"));
        append_artifact_to_task(&mut task, &event("b", &["x"], Some(false)));
        assert_eq!(task.artifacts.as_ref().required()?.len(), 2);
        append_artifact_to_task(&mut task, &event("a", &["3"], Some(true)));
        let arts = task.artifacts.as_ref().required()?;
        assert_eq!(arts[0].parts.len(), 2);
        append_artifact_to_task(&mut task, &event("zzz", &["ignored"], Some(true)));
        assert_eq!(task.artifacts.as_ref().required()?.len(), 2);
        Ok(())
    }

    #[test]
    fn modalities() -> TestResult {
        let s = vec!["text".to_string()];
        let c = vec!["image".to_string()];
        assert!(are_modalities_compatible(Some(&s), None));
        assert!(are_modalities_compatible(Some(&s), Some(&[])));
        assert!(are_modalities_compatible(None, Some(&c)));
        assert!(are_modalities_compatible(Some(&[]), Some(&c)));
        assert!(are_modalities_compatible(Some(&s), Some(&s)));
        assert!(!are_modalities_compatible(Some(&s), Some(&c)));
        Ok(())
    }

    #[test]
    fn state_helpers_and_final_event() -> TestResult {
        assert!(is_final(TaskState::Completed));
        assert!(is_final(TaskState::Rejected));
        assert!(!is_final(TaskState::Working));
        assert!(is_interrupted(TaskState::InputRequired));
        assert!(is_final_or_interrupted(TaskState::AuthRequired));
        assert!(validate_or_throw(false, "bad").is_err());
        assert!(validate_or_throw(true, "bad").is_ok());
        assert!(is_final_event(&StreamEvent::Message(Message::default())));
        let t = Task {
            status: TaskStatus::new(TaskState::Completed),
            ..Default::default()
        };
        assert!(is_final_event(&StreamEvent::Task(t)));
        assert!(!is_final_event(&StreamEvent::ArtifactUpdate(event(
            "a",
            &[],
            None
        ))));
        let su = TaskStatusUpdateEvent {
            status: TaskStatus::new(TaskState::Working),
            ..Default::default()
        };
        assert!(!is_final_event(&StreamEvent::StatusUpdate(su)));
        let b = build_text_artifact("hi", "id-1");
        assert_eq!(b.artifact_id, "id-1");
        assert_eq!(b.parts[0].text.as_deref(), Some("hi"));
        Ok(())
    }

    #[test]
    fn make_error_builds_jsonrpc_error() -> TestResult {
        let r = make_error(Some(RequestId::from("1")), -32001, "nope");
        let v = r.to_value();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "1");
        assert_eq!(v["error"]["code"], -32001);
        assert_eq!(v["error"]["message"], "nope");
        Ok(())
    }
}
