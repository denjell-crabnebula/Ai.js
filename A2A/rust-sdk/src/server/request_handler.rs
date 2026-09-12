// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Request handler interface and default implementation.
//!
//! Ports `src/server/request_handler.h` and `default_request_handler.cpp`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aServerError};
use crate::log::A2aLogLevel;
use crate::types::{
    AgentCard, DeleteTaskPushNotificationConfigParams, GetTaskPushNotificationConfigParams,
    ListTaskPushNotificationConfigParams, MessageSendParams, StreamEvent, Task, TaskIdParams,
    TaskPushNotificationConfig, TaskQueryParams, TaskState, TaskStatus,
};
use crate::utils::{generate_uuid, is_final, is_final_event, is_final_or_interrupted};

use super::call_context::{RequestContextParam, ServerCallContext};
use super::executor::AgentExecutor;
use super::push_notification::{
    InMemoryPushNotificationConfigStore, PushNotificationConfigStore, PushNotificationSender,
};
use super::request_context::RequestContext;
use super::task_manager::{TaskExecuteInfo, TaskManager};
use super::task_store::{InMemoryTaskStore, TaskStore};
use super::task_updater::TaskUpdaterImpl;

/// Callback that delivers a stream event to the transport.
pub type StreamEmitter = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// Minimal in-process server handler interface.
#[async_trait]
pub trait RequestHandler: Send + Sync {
    /// Handle `message/send`; the response is delivered through `emit`.
    async fn on_send_message(
        &self,
        params: MessageSendParams,
        context: Option<Arc<ServerCallContext>>,
        emit: StreamEmitter,
        method: &str,
    ) -> Result<(), A2aServerError>;

    /// Handle `tasks/get`.
    async fn on_get_task(
        &self,
        params: TaskQueryParams,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<Task, A2aServerError>;

    /// Handle `tasks/cancel`.
    async fn on_cancel_task(
        &self,
        params: TaskIdParams,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<Task, A2aServerError>;

    /// Handle `tasks/pushNotificationConfig/set`.
    async fn on_set_task_push_notification_config(
        &self,
        cfg: TaskPushNotificationConfig,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError>;

    /// Handle `tasks/pushNotificationConfig/get`.
    async fn on_get_task_push_notification_config(
        &self,
        params: GetTaskPushNotificationConfigParams,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<TaskPushNotificationConfig, A2aServerError>;

    /// Handle `tasks/pushNotificationConfig/list`.
    async fn on_list_task_push_notification_configs(
        &self,
        params: ListTaskPushNotificationConfigParams,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aServerError>;

    /// Handle `tasks/pushNotificationConfig/delete`.
    async fn on_delete_task_push_notification_config(
        &self,
        params: DeleteTaskPushNotificationConfigParams,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError>;

    /// Handle `message/stream`; events are delivered through `emit`.
    async fn on_send_message_streaming(
        &self,
        params: MessageSendParams,
        emit: StreamEmitter,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError>;

    /// Handle `tasks/resubscribe`; events are delivered through `emit`.
    async fn on_resubscribe_to_task(
        &self,
        params: TaskIdParams,
        emit: StreamEmitter,
        context: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError>;

    /// Return the agent card.
    fn on_get_card(&self, context: Option<Arc<ServerCallContext>>) -> Result<AgentCard, A2aServerError>;
}

/// Default request handler that drives an [`AgentExecutor`] through a [`TaskManager`].
pub struct DefaultRequestHandler {
    executor: Option<Arc<dyn AgentExecutor>>,
    agent_card: Arc<AgentCard>,
    task_store: Arc<dyn TaskStore>,
    task_manager: Arc<TaskManager>,
    push_config_store: Option<Arc<dyn PushNotificationConfigStore>>,
    push_sender: Option<Arc<dyn PushNotificationSender>>,
}

impl DefaultRequestHandler {
    /// Create a handler. A missing task store defaults to [`InMemoryTaskStore`].
    pub fn new(
        executor: Option<Arc<dyn AgentExecutor>>,
        agent_card: Arc<AgentCard>,
        task_store: Option<Arc<dyn TaskStore>>,
    ) -> Self {
        let task_store: Arc<dyn TaskStore> = task_store.unwrap_or_else(|| Arc::new(InMemoryTaskStore::new()));
        DefaultRequestHandler {
            executor,
            agent_card,
            task_store: task_store.clone(),
            task_manager: Arc::new(TaskManager::new(task_store)),
            push_config_store: Some(Arc::new(InMemoryPushNotificationConfigStore::new())),
            push_sender: None,
        }
    }

    /// Create a handler with explicit push notification store and sender.
    pub fn with_stores(
        executor: Option<Arc<dyn AgentExecutor>>,
        agent_card: Arc<AgentCard>,
        task_store: Arc<dyn TaskStore>,
        push_config_store: Option<Arc<dyn PushNotificationConfigStore>>,
        push_sender: Option<Arc<dyn PushNotificationSender>>,
    ) -> Self {
        DefaultRequestHandler {
            executor,
            agent_card,
            task_store: task_store.clone(),
            task_manager: Arc::new(TaskManager::new(task_store)),
            push_config_store,
            push_sender,
        }
    }

    /// The task manager used by this handler.
    pub fn task_manager(&self) -> &Arc<TaskManager> {
        &self.task_manager
    }

    /// The task store used by this handler.
    pub fn task_store(&self) -> &Arc<dyn TaskStore> {
        &self.task_store
    }

    /// Load the tasks referenced by `referenceTaskIds`, skipping unknown ids.
    pub fn get_related_tasks_from_reference_task_ids(
        &self,
        params: &MessageSendParams,
        ctx: Option<&ServerCallContext>,
    ) -> Vec<Task> {
        let mut related = Vec::new();
        let Some(ids) = &params.message.reference_task_ids else {
            return related;
        };
        for ref_task_id in ids {
            if ref_task_id.is_empty() {
                continue;
            }
            match self.task_store.get(ref_task_id, ctx) {
                Some(t) => related.push(t),
                None => a2a_log!(
                    A2aLogLevel::Warn,
                    "Reference task Id does not exist: {ref_task_id}"
                ),
            }
        }
        related
    }

    /// Resolve the task id of a request and load the existing task, if any.
    pub fn determine_task_id(
        &self,
        params: &MessageSendParams,
        ctx: Option<&ServerCallContext>,
    ) -> Result<(String, Option<Task>), A2aServerError> {
        let task_id = match &params.message.task_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => return Ok((Self::generate_task_id(), None)),
        };
        match self.task_store.get(&task_id, ctx) {
            Some(task) => {
                if let Some(cid) = &params.message.context_id {
                    if !cid.is_empty() && &task.context_id != cid {
                        return Err(A2aServerError::from_code(
                            "Existing task contextId does not match requested contextId",
                            A2AErrorCode::JsonrpcInvalidRequest,
                        ));
                    }
                }
                Ok((task_id, Some(task)))
            }
            None => Err(A2aServerError::from_code(
                "Task id not found",
                A2AErrorCode::TaskNotFound,
            )),
        }
    }

    fn generate_task_id() -> String {
        format!("task-{}", generate_uuid())
    }

    fn create_new_task(
        &self,
        params: &MessageSendParams,
        task_id: &str,
        context_id: &str,
        ctx: Option<&ServerCallContext>,
    ) {
        let new_task = Task {
            id: task_id.to_string(),
            context_id: context_id.to_string(),
            status: TaskStatus::new(TaskState::Submitted),
            history: Some(vec![params.message.clone()]),
            ..Default::default()
        };
        self.task_store.save(&new_task, ctx);
        self.update_push_notification_config(params, task_id);
    }

    fn update_push_notification_config(&self, params: &MessageSendParams, task_id: &str) {
        if let (Some(store), Some(cfg)) = (&self.push_config_store, &params.configuration) {
            if let Some(push) = &cfg.push_notification_config {
                store.set_info(task_id, push.clone());
            }
        }
    }

    fn executor(&self) -> Result<&Arc<dyn AgentExecutor>, A2aServerError> {
        self.executor
            .as_ref()
            .ok_or_else(|| A2aServerError::new("Agent executor not available"))
    }

    async fn execute_agent_and_get_result(
        &self,
        params: &MessageSendParams,
        task_id: &str,
        request_context: Arc<RequestContext>,
        emit: StreamEmitter,
        method: &str,
    ) -> Result<(), A2aServerError> {
        let task_updater = Arc::new(TaskUpdaterImpl::new(
            task_id,
            self.task_manager.get_context_id(task_id),
            self.task_manager.clone(),
        ));
        let blocking = params
            .configuration
            .as_ref()
            .map(|c| !c.return_immediately.unwrap_or(false))
            .unwrap_or(true);

        let manager = self.task_manager.clone();
        let tid = task_id.to_string();
        self.task_manager.add_event_callback(
            task_id,
            Arc::new(move |event: &StreamEvent| {
                let Some(mut task) = manager.get_task(&tid) else {
                    a2a_log!(
                        A2aLogLevel::Error,
                        "Error processing event: task id is invalid, task id: {tid}"
                    );
                    return;
                };
                if let StreamEvent::Message(_) = event {
                    task.status.state = TaskState::Completed;
                    manager.save_task(&task);
                    if !manager.exchange_message_sent(&tid, true) {
                        emit(event);
                    }
                    return;
                }
                if (is_final_event(event) || !blocking) && !manager.exchange_message_sent(&tid, true) {
                    emit(&StreamEvent::Task(task));
                }
            }),
        );
        let executor = self.executor()?;
        if method == crate::protocol::METHOD_MESSAGE_SEND {
            executor.execute(request_context, task_updater).await
        } else {
            executor
                .execute_with_method(request_context, task_updater, method)
                .await
        }
    }

    fn send_push_notification_if_needed(&self, task_id: &str, ctx: Option<&ServerCallContext>) {
        if let Some(sender) = &self.push_sender {
            if task_id.is_empty() {
                return;
            }
            if let Some(task) = self.task_store.get(task_id, ctx) {
                sender.send_notification(&task);
            }
        }
    }

    /// Prepare the task of a streaming request and return its pre-update snapshot.
    pub fn initialize_streaming_task(
        &self,
        params: &MessageSendParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<(Option<Task>, String), A2aServerError> {
        let (task_id, existing) = self.determine_task_id(params, ctx.as_deref())?;
        let context_id = match &existing {
            Some(t) => t.context_id.clone(),
            None => params.message.context_id.clone().unwrap_or_else(generate_uuid),
        };
        let info = Arc::new(TaskExecuteInfo::new(ctx.clone()));
        if let Some(existing) = existing {
            if is_final(existing.status.state) {
                return Err(A2aServerError::from_code(
                    "Cannot execute task in final state",
                    A2AErrorCode::UnsupportedOperation,
                ));
            }
            self.task_manager.register_task(&task_id, info)?;
            self.task_manager
                .update_with_message(&params.message, existing.clone());
            return Ok((Some(existing), task_id));
        }
        self.create_new_task(params, &task_id, &context_id, ctx.as_deref());
        let created = self.task_store.get(&task_id, ctx.as_deref());
        self.task_manager.register_task(&task_id, info)?;
        Ok((created, task_id))
    }

    async fn setup_and_execute_streaming_agent(
        &self,
        task_id: &str,
        request_context: Option<Arc<RequestContext>>,
        emit: StreamEmitter,
        resubscribe: bool,
    ) -> Result<(), A2aServerError> {
        let manager = self.task_manager.clone();
        let sender = self.push_sender.clone();
        let tid = task_id.to_string();
        self.task_manager.add_event_callback(
            task_id,
            Arc::new(move |ev: &StreamEvent| {
                emit(ev);
                let Some(sender) = &sender else {
                    return;
                };
                if resubscribe {
                    return;
                }
                if let Some(task) = manager.get_task(&tid) {
                    sender.send_notification(&task);
                }
            }),
        );
        if resubscribe {
            return Ok(());
        }
        let task_updater = Arc::new(TaskUpdaterImpl::new(
            task_id,
            self.task_manager.get_context_id(task_id),
            self.task_manager.clone(),
        ));
        let request_context =
            request_context.ok_or_else(|| A2aServerError::new("Request context not available"))?;
        self.executor()?.execute(request_context, task_updater).await
    }
}

#[async_trait]
impl RequestHandler for DefaultRequestHandler {
    async fn on_send_message(
        &self,
        params: MessageSendParams,
        ctx: Option<Arc<ServerCallContext>>,
        emit: StreamEmitter,
        method: &str,
    ) -> Result<(), A2aServerError> {
        let (task_id, existing) = self.determine_task_id(&params, ctx.as_deref())?;
        let context_id = match &existing {
            Some(t) => t.context_id.clone(),
            None => params.message.context_id.clone().unwrap_or_else(generate_uuid),
        };
        let related_tasks = self.get_related_tasks_from_reference_task_ids(&params, ctx.as_deref());
        let info = Arc::new(TaskExecuteInfo::new(ctx.clone()));
        match existing {
            Some(existing) => {
                if is_final(existing.status.state) {
                    return Err(A2aServerError::from_code(
                        "Cannot execute task in final state",
                        A2AErrorCode::UnsupportedOperation,
                    ));
                }
                self.task_manager.register_task(&task_id, info)?;
                self.task_manager.update_with_message(&params.message, existing);
            }
            None => {
                self.create_new_task(&params, &task_id, &context_id, ctx.as_deref());
                self.task_manager.register_task(&task_id, info)?;
            }
        }
        self.task_manager.exchange_message_sent(&task_id, false);

        let request_context = Arc::new(RequestContext::new(RequestContextParam {
            request: Some(params.clone()),
            task_id: Some(task_id.clone()),
            context_id: Some(context_id),
            task_store: Some(self.task_store.clone()),
            related_tasks,
            call_context: ctx.clone(),
        }));
        self.execute_agent_and_get_result(&params, &task_id, request_context, emit, method)
            .await?;
        self.send_push_notification_if_needed(&task_id, ctx.as_deref());
        Ok(())
    }

    async fn on_get_task(
        &self,
        params: TaskQueryParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<Task, A2aServerError> {
        let mut task = self
            .task_store
            .get(&params.id, ctx.as_deref())
            .ok_or_else(|| A2aServerError::from_code("Task id not found", A2AErrorCode::TaskNotFound))?;
        if let (Some(limit), Some(history)) = (params.history_length, task.history.as_mut()) {
            if limit > 0 && history.len() > limit as usize {
                let drop = history.len() - limit as usize;
                history.drain(..drop);
            }
        }
        Ok(task)
    }

    async fn on_cancel_task(
        &self,
        params: TaskIdParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<Task, A2aServerError> {
        let mut task = self
            .task_store
            .get(&params.id, ctx.as_deref())
            .ok_or_else(|| A2aServerError::from_code("Task id not found", A2AErrorCode::TaskNotFound))?;
        if is_final(task.status.state) {
            return Err(A2aServerError::from_code(
                "Cancel task failed",
                A2AErrorCode::TaskNotCancelable,
            ));
        }
        match &self.executor {
            Some(executor) => {
                let request_context = Arc::new(RequestContext::new(RequestContextParam {
                    request: None,
                    task_id: Some(task.id.clone()),
                    context_id: Some(task.context_id.clone()),
                    task_store: Some(self.task_store.clone()),
                    related_tasks: Vec::new(),
                    call_context: ctx.clone(),
                }));
                let task_updater = Arc::new(TaskUpdaterImpl::new(
                    task.id.clone(),
                    task.context_id.clone(),
                    self.task_manager.clone(),
                ));
                executor.cancel(request_context, task_updater).await?;
                if let Some(latest) = self.task_store.get(&params.id, ctx.as_deref()) {
                    task = latest;
                }
                self.task_manager.cancel_task(&mut task);
                a2a_log!(A2aLogLevel::Debug, "Task canceled, task id: {}", task.id);
            }
            None => {
                task.status.state = TaskState::Canceled;
                self.task_store.save(&task, ctx.as_deref());
                a2a_log!(
                    A2aLogLevel::Warn,
                    "Agent executor not available, task state set to canceled, task id: {}",
                    task.id
                );
            }
        }
        Ok(task)
    }

    async fn on_set_task_push_notification_config(
        &self,
        cfg: TaskPushNotificationConfig,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError> {
        let store = self.push_config_store_or_err()?;
        self.require_task(&cfg.task_id, ctx.as_deref())?;
        store.set_info(&cfg.task_id, cfg.push_notification_config);
        Ok(())
    }

    async fn on_get_task_push_notification_config(
        &self,
        params: GetTaskPushNotificationConfigParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<TaskPushNotificationConfig, A2aServerError> {
        let store = self.push_config_store_or_err()?;
        self.require_task(&params.id, ctx.as_deref())?;
        let configs = store.get_info(&params.id);
        Ok(TaskPushNotificationConfig {
            task_id: params.id,
            push_notification_config: configs.into_iter().next().unwrap_or_default(),
        })
    }

    async fn on_list_task_push_notification_configs(
        &self,
        params: ListTaskPushNotificationConfigParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aServerError> {
        let store = self.push_config_store_or_err()?;
        self.require_task(&params.id, ctx.as_deref())?;
        Ok(store
            .get_info(&params.id)
            .into_iter()
            .map(|config| TaskPushNotificationConfig {
                task_id: params.id.clone(),
                push_notification_config: config,
            })
            .collect())
    }

    async fn on_delete_task_push_notification_config(
        &self,
        params: DeleteTaskPushNotificationConfigParams,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError> {
        let store = self.push_config_store_or_err()?;
        self.require_task(&params.id, ctx.as_deref())?;
        store.delete_info(&params.id, Some(&params.push_notification_config_id));
        Ok(())
    }

    async fn on_send_message_streaming(
        &self,
        params: MessageSendParams,
        emit: StreamEmitter,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError> {
        let (existing, task_id) = self.initialize_streaming_task(&params, ctx.clone())?;
        match existing {
            Some(task) => emit(&StreamEvent::Task(task)),
            None => return Err(A2aServerError::new("find or create task failed")),
        }
        let related_tasks = self.get_related_tasks_from_reference_task_ids(&params, ctx.as_deref());
        let request_context = Arc::new(RequestContext::new(RequestContextParam {
            request: Some(params),
            task_id: Some(task_id.clone()),
            context_id: Some(self.task_manager.get_context_id(&task_id)),
            task_store: Some(self.task_store.clone()),
            related_tasks,
            call_context: ctx,
        }));
        self.setup_and_execute_streaming_agent(&task_id, Some(request_context), emit, false)
            .await
    }

    async fn on_resubscribe_to_task(
        &self,
        params: TaskIdParams,
        emit: StreamEmitter,
        ctx: Option<Arc<ServerCallContext>>,
    ) -> Result<(), A2aServerError> {
        let task = self
            .task_store
            .get(&params.id, ctx.as_deref())
            .ok_or_else(|| A2aServerError::from_code("Task id not found", A2AErrorCode::TaskNotFound))?;
        let state = task.status.state;
        let id = task.id.clone();
        emit(&StreamEvent::Task(task));
        if is_final_or_interrupted(state) {
            return Ok(());
        }
        self.setup_and_execute_streaming_agent(&id, None, emit, true)
            .await
    }

    fn on_get_card(&self, _ctx: Option<Arc<ServerCallContext>>) -> Result<AgentCard, A2aServerError> {
        Ok((*self.agent_card).clone())
    }
}

impl DefaultRequestHandler {
    fn push_config_store_or_err(&self) -> Result<&Arc<dyn PushNotificationConfigStore>, A2aServerError> {
        self.push_config_store.as_ref().ok_or_else(|| {
            A2aServerError::from_code(
                "Push notification config is not set",
                A2AErrorCode::PushNotificationNotSupported,
            )
        })
    }

    fn require_task(&self, task_id: &str, ctx: Option<&ServerCallContext>) -> Result<(), A2aServerError> {
        if self.task_store.get(task_id, ctx).is_none() {
            return Err(A2aServerError::from_code(
                "Task id not found",
                A2AErrorCode::TaskNotFound,
            ));
        }
        Ok(())
    }
}
