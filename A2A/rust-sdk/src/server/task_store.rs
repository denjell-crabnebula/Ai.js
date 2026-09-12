// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Task persistence, the port of `include/server/task_store.h` and the in-memory store.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::types::Task;

use super::call_context::ServerCallContext;

/// Storage for tasks. `get` returns an owned copy of the stored task.
pub trait TaskStore: Send + Sync {
    /// Save or replace a task.
    fn save(&self, task: &Task, context: Option<&ServerCallContext>);
    /// Load a task by id.
    fn get(&self, task_id: &str, context: Option<&ServerCallContext>) -> Option<Task>;
    /// Delete a task by id.
    fn delete(&self, task_id: &str, context: Option<&ServerCallContext>);
}

/// In-memory task store keyed by task id.
#[derive(Default)]
pub struct InMemoryTaskStore {
    tasks: Mutex<HashMap<String, Task>>,
}

impl InMemoryTaskStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TaskStore for InMemoryTaskStore {
    fn save(&self, task: &Task, _context: Option<&ServerCallContext>) {
        self.tasks.lock().insert(task.id.clone(), task.clone());
    }

    fn get(&self, task_id: &str, _context: Option<&ServerCallContext>) -> Option<Task> {
        self.tasks.lock().get(task_id).cloned()
    }

    fn delete(&self, task_id: &str, _context: Option<&ServerCallContext>) {
        self.tasks.lock().remove(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, TaskState};
    use ap_support::testing::{OptionExt, TestResult};

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            context_id: "ctx".into(),
            ..Default::default()
        }
    }

    #[test]
    fn save_get_replace_delete() -> TestResult {
        let store = InMemoryTaskStore::new();
        assert!(store.get("missing", None).is_none());
        let mut t = task("t1");
        t.history = Some(vec![Message {
            message_id: "m".into(),
            ..Default::default()
        }]);
        store.save(&t, None);
        assert_eq!(store.get("t1", None).required()?.history.required()?.len(), 1);
        t.status.state = TaskState::Completed;
        store.save(&t, None);
        assert_eq!(
            store.get("t1", None).required()?.status.state,
            TaskState::Completed
        );
        store.save(&task("t2"), None);
        assert_eq!(store.get("t2", None).required()?.id, "t2");
        store.delete("t1", None);
        assert!(store.get("t1", None).is_none());
        store.delete("nope", None);
        Ok(())
    }
}
