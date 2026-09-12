// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Push notification config store and sender, the port of `src/server/tasks/*push_notification*`.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::types::{PushNotificationConfig, Task};

/// Storage of push notification configs per task.
pub trait PushNotificationConfigStore: Send + Sync {
    /// Store a config for a task. A missing config id defaults to the task id.
    fn set_info(&self, task_id: &str, notification_config: PushNotificationConfig);
    /// All configs of a task.
    fn get_info(&self, task_id: &str) -> Vec<PushNotificationConfig>;
    /// Delete one config; a missing config id defaults to the task id.
    fn delete_info(&self, task_id: &str, config_id: Option<&str>);
}

/// In-memory push notification config store.
#[derive(Default)]
pub struct InMemoryPushNotificationConfigStore {
    data: Mutex<HashMap<String, Vec<PushNotificationConfig>>>,
}

impl InMemoryPushNotificationConfigStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PushNotificationConfigStore for InMemoryPushNotificationConfigStore {
    fn set_info(&self, task_id: &str, mut notification_config: PushNotificationConfig) {
        let mut data = self.data.lock();
        let vec = data.entry(task_id.to_string()).or_default();
        if notification_config.id.is_none() {
            notification_config.id = Some(task_id.to_string());
        }
        if let Some(pos) = vec.iter().position(|c| c.id == notification_config.id) {
            vec.remove(pos);
        }
        vec.push(notification_config);
    }

    fn get_info(&self, task_id: &str) -> Vec<PushNotificationConfig> {
        self.data.lock().get(task_id).cloned().unwrap_or_default()
    }

    fn delete_info(&self, task_id: &str, config_id: Option<&str>) {
        let mut data = self.data.lock();
        let Some(vec) = data.get_mut(task_id) else {
            return;
        };
        let cid = config_id.unwrap_or(task_id);
        if let Some(pos) = vec.iter().position(|c| c.id.as_deref() == Some(cid)) {
            vec.remove(pos);
        }
        if vec.is_empty() {
            data.remove(task_id);
        }
    }
}

/// Sends push notifications for task updates.
pub trait PushNotificationSender: Send + Sync {
    /// Send a notification for the current state of a task.
    fn send_notification(&self, task: &Task);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    fn cfg(id: Option<&str>, url: &str) -> PushNotificationConfig {
        PushNotificationConfig {
            id: id.map(str::to_string),
            url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn set_get_delete_semantics() -> TestResult {
        let store = InMemoryPushNotificationConfigStore::new();
        assert!(store.get_info("t").is_empty());
        store.set_info("t", cfg(None, "http://a"));
        let got = store.get_info("t");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id.as_deref(), Some("t"));
        store.set_info("t", cfg(Some("c1"), "http://b"));
        assert_eq!(store.get_info("t").len(), 2);
        store.set_info("t", cfg(Some("c1"), "http://c"));
        let got = store.get_info("t");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].url, "http://c");
        store.delete_info("t", Some("zzz"));
        assert_eq!(store.get_info("t").len(), 2);
        store.delete_info("t", None);
        let got = store.get_info("t");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id.as_deref(), Some("c1"));
        store.delete_info("t", Some("c1"));
        assert!(store.get_info("t").is_empty());
        store.delete_info("missing", None);
        Ok(())
    }
}
