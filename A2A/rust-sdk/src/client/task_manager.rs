// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client-side task bookkeeping for streams, the port of `client_task_manager.*`.

use crate::types::{Message, StreamEvent, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus};

/// Accumulates streaming events into a task snapshot.
#[derive(Clone, Debug, Default)]
pub struct ClientTaskManager {
    current_task: Option<Task>,
}

fn append_artifact_to_task(task: &mut Task, event: &TaskArtifactUpdateEvent) {
    let list = task.artifacts.get_or_insert_with(Vec::new);
    let new_artifact = &event.artifact;
    let append_parts = event.append.unwrap_or(false);
    let existing = list
        .iter_mut()
        .find(|a| a.artifact_id == new_artifact.artifact_id);
    match (append_parts, existing) {
        (true, Some(slot)) => slot.parts.extend(new_artifact.parts.iter().cloned()),
        (false, Some(slot)) => *slot = new_artifact.clone(),
        (_, None) => list.push(new_artifact.clone()),
    }
}

impl ClientTaskManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current task, or an error when no task was seen yet.
    pub fn get_task_or_raise(&self) -> Result<&Task, String> {
        self.current_task
            .as_ref()
            .ok_or_else(|| "no current Task".to_string())
    }

    /// The current task, if any.
    pub fn current_task(&self) -> Option<&Task> {
        self.current_task.as_ref()
    }

    /// Apply a task, status or artifact event. Message events are ignored.
    pub fn save_task_event(&mut self, ev: &StreamEvent) -> Result<(), String> {
        match ev {
            StreamEvent::Task(t) => {
                if self.current_task.is_some() {
                    return Err("Task is already set, create new manager for new tasks.".to_string());
                }
                self.save_task(t.clone());
                return Ok(());
            }
            StreamEvent::Message(_) => return Ok(()),
            _ => {}
        }
        if self.current_task.is_none() {
            let shell = match ev {
                StreamEvent::StatusUpdate(u) => Task {
                    id: u.task_id.clone(),
                    context_id: u.context_id.clone(),
                    status: u.status.clone(),
                    ..Default::default()
                },
                StreamEvent::ArtifactUpdate(u) => Task {
                    id: u.task_id.clone(),
                    context_id: u.context_id.clone(),
                    status: TaskStatus::new(TaskState::Unspecified),
                    ..Default::default()
                },
                StreamEvent::Task(_) | StreamEvent::Message(_) => return Ok(()),
            };
            self.current_task = Some(shell);
        }
        let Some(task) = self.current_task.as_mut() else {
            return Err("No current task to update.".to_string());
        };
        match ev {
            StreamEvent::StatusUpdate(u) => {
                if let Some(m) = &u.status.message {
                    task.history.get_or_insert_with(Vec::new).push(m.clone());
                }
                if u.metadata.is_some() && task.metadata.is_none() {
                    task.metadata = u.metadata.clone();
                }
                task.status = u.status.clone();
            }
            StreamEvent::ArtifactUpdate(u) => append_artifact_to_task(task, u),
            _ => {}
        }
        Ok(())
    }

    /// Move the status message into the history, append `msg` and adopt the task.
    pub fn update_with_message(&mut self, msg: &Message, mut task: Task) -> Task {
        if let Some(m) = task.status.message.clone() {
            match task.history.as_mut() {
                Some(h) => h.push(m),
                None => {
                    task.history = Some(vec![m]);
                    task.status.message = None;
                }
            }
        }
        task.history.get_or_insert_with(Vec::new).push(msg.clone());
        self.current_task = Some(task.clone());
        task
    }

    fn save_task(&mut self, task: Task) {
        self.current_task = Some(task);
    }
}
