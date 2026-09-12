// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Task updater, the port of `include/server/task_updater.h` and `task_updater_impl.cpp`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::a2a_log;
use crate::log::A2aLogLevel;
use crate::types::{
    Artifact, Message, Part, Role, StreamEvent, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};
use crate::utils::{IdGenerator, IdGeneratorContext, UuidGenerator, is_final};

use super::task_manager::TaskManager;

/// Parameters for [`TaskUpdater::add_artifact`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskArtifactParam {
    /// Artifact parts.
    pub parts: Vec<Part>,
    /// Explicit artifact id; generated when absent.
    pub artifact_id: Option<String>,
    /// Artifact name.
    pub name: Option<String>,
    /// Metadata of the artifact and the event.
    pub metadata: Option<Value>,
    /// Append parts to an existing artifact.
    pub append: bool,
    /// Whether this is the last chunk.
    pub last_chunk: bool,
    /// Extension URIs.
    pub extensions: Vec<String>,
}

/// Interface used by an agent executor to publish task progress.
pub trait TaskUpdater: Send + Sync {
    /// Publish a status update. Ignored after a terminal state.
    fn update_status(
        &self,
        state: TaskState,
        message: Option<Message>,
        timestamp: Option<String>,
        metadata: Option<Value>,
    );

    /// Publish an artifact update. Ignored after a terminal state.
    fn add_artifact(&self, artifact_param: &TaskArtifactParam);

    /// Mark the task completed.
    fn complete(&self, message: Option<Message>) {
        self.update_status(TaskState::Completed, message, None, None);
    }

    /// Mark the task failed.
    fn failed(&self, message: Option<Message>) {
        self.update_status(TaskState::Failed, message, None, None);
    }

    /// Mark the task rejected.
    fn reject(&self, message: Option<Message>) {
        self.update_status(TaskState::Rejected, message, None, None);
    }

    /// Mark the task submitted.
    fn submit(&self, message: Option<Message>) {
        self.update_status(TaskState::Submitted, message, None, None);
    }

    /// Mark the task working.
    fn start_work(&self, message: Option<Message>) {
        self.update_status(TaskState::Working, message, None, None);
    }

    /// Mark the task canceled.
    fn cancel(&self, message: Option<Message>) {
        self.update_status(TaskState::Canceled, message, None, None);
    }

    /// Mark the task as waiting for input.
    fn requires_input(&self, message: Option<Message>) {
        self.update_status(TaskState::InputRequired, message, None, None);
    }

    /// Mark the task as waiting for authentication.
    fn requires_auth(&self, message: Option<Message>) {
        self.update_status(TaskState::AuthRequired, message, None, None);
    }

    /// Build an agent message bound to this task.
    fn new_agent_message(&self, parts: Vec<Part>, metadata: Option<Value>) -> Message;

    /// Send a response message and end the exchange.
    fn send_response_message(&self, message: &Message);
}

/// Default [`TaskUpdater`] backed by a [`TaskManager`].
pub struct TaskUpdaterImpl {
    task_id: String,
    context_id: String,
    task_manager: Arc<TaskManager>,
    terminal_state_reached: AtomicBool,
    artifact_id_generator: Arc<dyn IdGenerator>,
    message_id_generator: Arc<dyn IdGenerator>,
}

impl TaskUpdaterImpl {
    /// Create an updater for a task.
    pub fn new(
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        task_manager: Arc<TaskManager>,
    ) -> Self {
        TaskUpdaterImpl {
            task_id: task_id.into(),
            context_id: context_id.into(),
            task_manager,
            terminal_state_reached: AtomicBool::new(false),
            artifact_id_generator: Arc::new(UuidGenerator),
            message_id_generator: Arc::new(UuidGenerator),
        }
    }

    /// Task identifier.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Context identifier.
    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    /// Whether a terminal state was already published.
    pub fn terminal_state_reached(&self) -> bool {
        self.terminal_state_reached.load(Ordering::SeqCst)
    }

    /// Current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    pub fn get_current_timestamp() -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    fn id_context(&self) -> IdGeneratorContext {
        IdGeneratorContext {
            task_id: Some(self.task_id.clone()),
            context_id: Some(self.context_id.clone()),
        }
    }
}

impl TaskUpdater for TaskUpdaterImpl {
    fn update_status(
        &self,
        state: TaskState,
        message: Option<Message>,
        timestamp: Option<String>,
        metadata: Option<Value>,
    ) {
        if self.terminal_state_reached() {
            a2a_log!(
                A2aLogLevel::Warn,
                "Task {} is already in a terminal state!",
                self.task_id
            );
            return;
        }
        if is_final(state) {
            self.terminal_state_reached.store(true, Ordering::SeqCst);
        }
        let ts = timestamp.unwrap_or_else(Self::get_current_timestamp);
        let event = TaskStatusUpdateEvent {
            context_id: self.context_id.clone(),
            metadata,
            status: TaskStatus {
                message,
                state,
                timestamp: Some(ts),
            },
            task_id: self.task_id.clone(),
        };
        self.task_manager
            .process(&self.task_id, &StreamEvent::StatusUpdate(event));
    }

    fn add_artifact(&self, artifact_param: &TaskArtifactParam) {
        if self.terminal_state_reached() {
            a2a_log!(
                A2aLogLevel::Warn,
                "Task {} is already in a terminal state!",
                self.task_id
            );
            return;
        }
        let aid = artifact_param
            .artifact_id
            .clone()
            .unwrap_or_else(|| self.artifact_id_generator.generate(&self.id_context()));
        let artifact = Artifact {
            artifact_id: aid,
            parts: artifact_param.parts.clone(),
            name: artifact_param.name.clone(),
            metadata: artifact_param.metadata.clone(),
            extensions: Some(artifact_param.extensions.clone()),
            description: None,
        };
        let event = TaskArtifactUpdateEvent {
            artifact,
            context_id: self.context_id.clone(),
            task_id: self.task_id.clone(),
            append: Some(artifact_param.append),
            last_chunk: Some(artifact_param.last_chunk),
            metadata: artifact_param.metadata.clone(),
            index: None,
        };
        self.task_manager
            .process(&self.task_id, &StreamEvent::ArtifactUpdate(event));
    }

    fn new_agent_message(&self, parts: Vec<Part>, metadata: Option<Value>) -> Message {
        Message {
            parts,
            role: Role::Agent,
            message_id: self.message_id_generator.generate(&self.id_context()),
            task_id: Some(self.task_id.clone()),
            context_id: Some(self.context_id.clone()),
            metadata,
            ..Default::default()
        }
    }

    fn send_response_message(&self, message: &Message) {
        self.task_manager
            .process(&self.task_id, &StreamEvent::Message(message.clone()));
        self.terminal_state_reached.store(true, Ordering::SeqCst);
    }
}
