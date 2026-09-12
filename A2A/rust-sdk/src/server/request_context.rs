// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Request context handed to the agent executor, the port of `request_context.cpp`.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use crate::types::{Message, MessageSendConfiguration, MessageSendParams, Task};
use crate::utils::get_message_text;

use super::call_context::{RequestContextParam, ServerCallContext};
use super::task_store::TaskStore;

/// Read-mostly view of an incoming request for the agent executor.
pub struct RequestContext {
    params: Option<MessageSendParams>,
    task_id: Option<String>,
    context_id: Option<String>,
    task_store: Option<Arc<dyn TaskStore>>,
    related_tasks: Mutex<Vec<Task>>,
    call_context: Option<Arc<ServerCallContext>>,
}

impl RequestContext {
    /// Build a context. The message gets the task and context ids of the param.
    pub fn new(param: RequestContextParam) -> Self {
        let mut params = param.request;
        if let Some(p) = params.as_mut() {
            p.message.task_id = param.task_id.clone();
            p.message.context_id = param.context_id.clone();
        }
        RequestContext {
            params,
            task_id: param.task_id,
            context_id: param.context_id,
            task_store: param.task_store,
            related_tasks: Mutex::new(param.related_tasks),
            call_context: param.call_context,
        }
    }

    /// Text of the request message joined with a delimiter.
    pub fn get_user_input(&self, delimiter: &str) -> String {
        match &self.params {
            Some(p) => get_message_text(&p.message, delimiter),
            None => String::new(),
        }
    }

    /// Append a related task.
    pub fn attach_related_task(&self, task: Task) {
        self.related_tasks.lock().push(task);
    }

    /// The request message, if any.
    pub fn get_message(&self) -> Option<&Message> {
        self.params.as_ref().map(|p| &p.message)
    }

    /// Snapshot of the related tasks.
    pub fn get_related_tasks(&self) -> Vec<Task> {
        self.related_tasks.lock().clone()
    }

    /// The current task loaded from the store.
    pub fn get_current_task(&self) -> Option<Task> {
        match (&self.task_id, &self.task_store) {
            (Some(id), Some(store)) => store.get(id, None),
            _ => None,
        }
    }

    /// Task identifier.
    pub fn get_task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    /// Context identifier.
    pub fn get_context_id(&self) -> Option<&str> {
        self.context_id.as_deref()
    }

    /// Copy of the send configuration, if present.
    pub fn get_configuration(&self) -> Option<MessageSendConfiguration> {
        self.params.as_ref().and_then(|p| p.configuration.clone())
    }

    /// The call context.
    pub fn get_call_context(&self) -> Option<Arc<ServerCallContext>> {
        self.call_context.clone()
    }

    /// Request metadata, if present.
    pub fn get_metadata(&self) -> Option<&Value> {
        self.params.as_ref().and_then(|p| p.metadata.as_ref())
    }

    /// Record an activated extension in the call context.
    pub fn add_activated_extension(&self, uri: &str) {
        if let Some(ctx) = &self.call_context {
            ctx.activated_extensions.write().insert(uri.to_string());
        }
    }

    /// Extensions requested by the client.
    pub fn get_requested_extensions(&self) -> HashSet<String> {
        match &self.call_context {
            Some(ctx) => ctx.requested_extensions.read().clone(),
            None => HashSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::task_store::InMemoryTaskStore;
    use crate::types::Part;
    use ap_support::testing::{OptionExt, TestResult};

    fn params(texts: &[&str]) -> MessageSendParams {
        MessageSendParams {
            message: Message {
                message_id: "m1".into(),
                parts: texts.iter().map(|t| Part::text(*t)).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn empty_param_returns_defaults() -> TestResult {
        let ctx = RequestContext::new(RequestContextParam::default());
        assert_eq!(ctx.get_user_input("\n"), "");
        assert!(ctx.get_message().is_none());
        assert!(ctx.get_related_tasks().is_empty());
        assert!(ctx.get_current_task().is_none());
        assert!(ctx.get_task_id().is_none());
        assert!(ctx.get_context_id().is_none());
        assert!(ctx.get_configuration().is_none());
        assert!(ctx.get_call_context().is_none());
        assert!(ctx.get_metadata().is_none());
        assert!(ctx.get_requested_extensions().is_empty());
        ctx.add_activated_extension("x");
        Ok(())
    }

    #[test]
    fn ids_are_applied_to_message() -> TestResult {
        let store = Arc::new(InMemoryTaskStore::new());
        let task = Task {
            id: "t1".into(),
            context_id: "c1".into(),
            ..Default::default()
        };
        store.save(&task, None);
        let ctx = RequestContext::new(RequestContextParam {
            request: Some(params(&["a", "b"])),
            task_id: Some("t1".into()),
            context_id: Some("c1".into()),
            task_store: Some(store),
            ..Default::default()
        });
        assert_eq!(ctx.get_task_id(), Some("t1"));
        assert_eq!(ctx.get_message().required()?.task_id.as_deref(), Some("t1"));
        assert_eq!(ctx.get_message().required()?.context_id.as_deref(), Some("c1"));
        assert_eq!(ctx.get_user_input("\n"), "a\nb");
        assert_eq!(ctx.get_user_input(", "), "a, b");
        assert_eq!(ctx.get_current_task().required()?.id, "t1");
        ctx.attach_related_task(task);
        assert_eq!(ctx.get_related_tasks().len(), 1);
        Ok(())
    }

    #[test]
    fn call_context_extensions() -> TestResult {
        let call = Arc::new(ServerCallContext::new());
        call.requested_extensions.write().insert("req".into());
        let ctx = RequestContext::new(RequestContextParam {
            call_context: Some(call.clone()),
            ..Default::default()
        });
        ctx.add_activated_extension("act");
        assert!(call.activated_extensions.read().contains("act"));
        assert!(ctx.get_requested_extensions().contains("req"));
        assert!(Arc::ptr_eq(&ctx.get_call_context().required()?, &call));
        Ok(())
    }
}
