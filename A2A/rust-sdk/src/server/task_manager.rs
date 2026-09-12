// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server task manager, the port of `src/server/tasks/task_manager.*`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::Value;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aServerError};
use crate::log::A2aLogLevel;
use crate::types::{Message, StreamEvent, Task, TaskState, TaskStatus, TaskStatusUpdateEvent};
use crate::utils::{append_artifact_to_task, is_final, is_final_event};

use super::call_context::ServerCallContext;
use super::task_store::TaskStore;

/// Callback invoked for every processed event of a task.
pub type EventCb = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// Per-task execution state registered with the [`TaskManager`].
#[derive(Default)]
pub struct TaskExecuteInfo {
    /// Serializes event processing for the task.
    callback_mutex: Mutex<()>,
    /// Non-streaming only: whether the response message is already sent.
    pub message_sent: AtomicBool,
    /// Callbacks invoked on event processing.
    event_cb: Mutex<Vec<EventCb>>,
    /// Call context bound to the task.
    pub call_context: Option<Arc<ServerCallContext>>,
}

impl TaskExecuteInfo {
    /// Create an info bound to an optional call context.
    pub fn new(call_context: Option<Arc<ServerCallContext>>) -> Self {
        TaskExecuteInfo {
            call_context,
            ..Default::default()
        }
    }

    /// Number of registered callbacks.
    pub fn callback_count(&self) -> usize {
        self.event_cb.lock().len()
    }

    fn callbacks(&self) -> Vec<EventCb> {
        self.event_cb.lock().clone()
    }
}

/// Applies task events to the store and fans them out to registered callbacks.
pub struct TaskManager {
    task_store: Arc<dyn TaskStore>,
    task_execute_map: Mutex<HashMap<String, Arc<TaskExecuteInfo>>>,
}

impl TaskManager {
    /// Create a manager on top of a task store.
    pub fn new(task_store: Arc<dyn TaskStore>) -> Self {
        TaskManager {
            task_store,
            task_execute_map: Mutex::new(HashMap::new()),
        }
    }

    /// The underlying task store.
    pub fn task_store(&self) -> &Arc<dyn TaskStore> {
        &self.task_store
    }

    /// Register execution info for a task. The id must be non-empty.
    pub fn register_task(&self, task_id: &str, info: Arc<TaskExecuteInfo>) -> Result<(), A2aServerError> {
        if task_id.is_empty() {
            return Err(A2aServerError::new("Task ID must be a non-empty string"));
        }
        self.task_execute_map.lock().insert(task_id.to_string(), info);
        Ok(())
    }

    /// Whether a task is registered.
    pub fn is_registered(&self, task_id: &str) -> bool {
        self.task_execute_map.lock().contains_key(task_id)
    }

    fn info(&self, task_id: &str) -> Option<Arc<TaskExecuteInfo>> {
        self.task_execute_map.lock().get(task_id).cloned()
    }

    /// Load a task from the store using the registered call context.
    pub fn get_task(&self, task_id: &str) -> Option<Task> {
        match self.info(task_id) {
            Some(info) => self.task_store.get(task_id, info.call_context.as_deref()),
            None => self.task_store.get(task_id, None),
        }
    }

    /// Context id of a stored task, or an empty string.
    pub fn get_context_id(&self, task_id: &str) -> String {
        self.task_store
            .get(task_id, None)
            .map(|t| t.context_id)
            .unwrap_or_default()
    }

    fn save_task_context_id(&self, event: &StreamEvent) {
        let (eid, ecid) = match event {
            StreamEvent::Task(t) => (t.id.clone(), t.context_id.clone()),
            StreamEvent::StatusUpdate(e) => (e.task_id.clone(), e.context_id.clone()),
            StreamEvent::ArtifactUpdate(e) => (e.task_id.clone(), e.context_id.clone()),
            StreamEvent::Message(_) => return,
        };
        if self.info(&eid).is_none() {
            self.handle_error(
                &eid,
                "Task in event has not been registered to task manager",
                None,
            );
            return;
        }
        if let Some(task) = self.get_task(&eid) {
            if !task.context_id.is_empty() && task.context_id != ecid {
                self.handle_error(
                    &eid,
                    &format!(
                        "Context in event doesn't match TaskManager {} : {}",
                        task.context_id, ecid
                    ),
                    None,
                );
            }
        }
    }

    /// Apply a task, status or artifact event to the stored task.
    pub fn save_task_event(&self, event: &StreamEvent) {
        self.save_task_context_id(event);
        let mut task = match event {
            StreamEvent::Task(t) => {
                self.save_task(t);
                return;
            }
            StreamEvent::Message(_) => return,
            other => self.ensure_task_for_event(other),
        };
        match event {
            StreamEvent::StatusUpdate(e) => {
                if let Some(m) = task.status.message.take() {
                    task.history.get_or_insert_with(Vec::new).push(m);
                }
                match (&mut task.metadata, &e.metadata) {
                    (None, m) => task.metadata = m.clone(),
                    (Some(Value::Object(existing)), Some(Value::Object(incoming))) => {
                        for (k, v) in incoming {
                            existing.insert(k.clone(), v.clone());
                        }
                    }
                    (Some(_), Some(incoming)) => task.metadata = Some(incoming.clone()),
                    (Some(_), None) => {}
                }
                task.status = e.status.clone();
            }
            StreamEvent::ArtifactUpdate(e) => append_artifact_to_task(&mut task, e),
            _ => {}
        }
        self.save_task(&task);
    }

    /// Process an event: persist it, notify callbacks and drop final tasks.
    pub fn process(&self, task_id: &str, event: &StreamEvent) {
        let Some(info) = self.info(task_id) else {
            self.handle_error(
                task_id,
                "Process task failed, task id has expired or is not registered to task manager",
                None,
            );
            return;
        };
        let _guard = info.callback_mutex.lock();
        self.save_task_event(event);
        for callback in info.callbacks() {
            callback(event);
        }
        if is_final_event(event) {
            self.task_execute_map.lock().remove(task_id);
        }
    }

    /// Load the task of an event, or create and register a submitted task for it.
    pub fn ensure_task_for_event(&self, event: &StreamEvent) -> Task {
        let (eid, ecid) = match event {
            StreamEvent::StatusUpdate(e) => (e.task_id.clone(), e.context_id.clone()),
            StreamEvent::ArtifactUpdate(e) => (e.task_id.clone(), e.context_id.clone()),
            StreamEvent::Task(t) => return t.clone(),
            StreamEvent::Message(m) => (
                m.task_id.clone().unwrap_or_default(),
                m.context_id.clone().unwrap_or_default(),
            ),
        };
        if let Some(task) = self.task_store.get(&eid, None) {
            return task;
        }
        let task = Task {
            id: eid.clone(),
            context_id: ecid,
            status: TaskStatus::new(TaskState::Submitted),
            ..Default::default()
        };
        let _ = self.register_task(&eid, Arc::new(TaskExecuteInfo::default()));
        self.save_task(&task);
        task
    }

    /// Save a task using the registered call context.
    pub fn save_task(&self, task: &Task) {
        match self.info(&task.id) {
            Some(info) => self.task_store.save(task, info.call_context.as_deref()),
            None => self.task_store.save(task, None),
        }
    }

    /// Move the status message into the history, append a message and save.
    pub fn update_with_message(&self, message: &Message, mut task: Task) -> Task {
        if let Some(m) = task.status.message.take() {
            task.history.get_or_insert_with(Vec::new).push(m);
        }
        task.history.get_or_insert_with(Vec::new).push(message.clone());
        self.save_task(&task);
        task
    }

    /// Swap the `message_sent` flag and return the previous value.
    pub fn exchange_message_sent(&self, task_id: &str, value: bool) -> bool {
        match self.info(task_id) {
            Some(info) => info.message_sent.swap(value, Ordering::SeqCst),
            None => false,
        }
    }

    /// Add an event callback to a registered task.
    pub fn add_event_callback(&self, task_id: &str, callback: EventCb) {
        if let Some(info) = self.info(task_id) {
            info.event_cb.lock().push(callback);
        }
    }

    /// Cancel a task: notify streaming callbacks and mark it canceled.
    pub fn cancel_task(&self, task: &mut Task) {
        if is_final(task.status.state) {
            a2a_log!(
                A2aLogLevel::Debug,
                "Task already canceled by agent, task id: {}",
                task.id
            );
            return;
        }
        let Some(info) = self.info(&task.id) else {
            task.status.state = TaskState::Canceled;
            self.save_task(task);
            return;
        };
        let _guard = info.callback_mutex.lock();
        let event = StreamEvent::StatusUpdate(TaskStatusUpdateEvent {
            context_id: task.context_id.clone(),
            metadata: None,
            status: TaskStatus::new(TaskState::Canceled),
            task_id: task.id.clone(),
        });
        task.status.state = TaskState::Canceled;
        self.save_task(task);
        for callback in info.callbacks() {
            callback(&event);
        }
        self.task_execute_map.lock().remove(&task.id);
    }

    fn handle_error(&self, task_id: &str, message: &str, code: Option<i64>) {
        let code = code.unwrap_or(A2AErrorCode::JsonrpcInternalError.code());
        a2a_log!(
            A2aLogLevel::Error,
            "Process task failed, task id: {task_id}, error msg: {message}, error code: {code}"
        );
    }
}
