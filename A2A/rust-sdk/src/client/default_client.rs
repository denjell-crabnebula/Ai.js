// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! The client interface and its default implementation, the port of
//! `include/client/client.h` and `default_client.*`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use futures::Stream;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aClientError};
use crate::log::A2aLogLevel;
use crate::protocol::{
    METHOD_AGENT_CARD_GET, METHOD_MESSAGE_SEND, METHOD_MESSAGE_STREAM, METHOD_TASK_CANCEL, METHOD_TASK_GET,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET,
    METHOD_TASK_RESUBSCRIBE,
};
use crate::types::{
    A2AError, AgentCard, ClientCallContext, DeleteTaskPushNotificationConfigParams,
    GetTaskPushNotificationConfigParams, ListTaskPushNotificationConfigParams, Message,
    MessageSendConfiguration, MessageSendParams, StreamEvent, Task, TaskIdParams, TaskPushNotificationConfig,
    TaskQueryParams,
};
use crate::utils::{generate_uuid, is_final_or_interrupted};

use super::config::{ClientConfig, ClientEvent, Consumer, ResponseHandler, UpdateEvent};
use super::interceptor::ClientCallInterceptor;
use super::task_manager::ClientTaskManager;
use super::transport::{ClientTransport, TransportError, TransportEvent};

/// High-level A2A client API.
///
/// Timeouts are in seconds; 0 selects the transport default.
#[async_trait]
pub trait Client: Send + Sync {
    /// Send a message. The handler is called per streaming event, or once.
    ///
    /// Streaming is used when both the config and the agent card allow it.
    async fn send_message(
        &self,
        msg: &Message,
        context: Option<&ClientCallContext>,
        handler: ResponseHandler,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Retrieve a task (`tasks/get`).
    async fn get_task(
        &self,
        params: &TaskQueryParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Task, A2aClientError>;

    /// Cancel a task (`tasks/cancel`).
    async fn cancel_task(
        &self,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Task, A2aClientError>;

    /// Create or update a push notification config.
    async fn set_task_push_notification_config(
        &self,
        cfg: &TaskPushNotificationConfig,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<TaskPushNotificationConfig, A2aClientError>;

    /// Get a push notification config.
    async fn get_task_push_notification_config(
        &self,
        params: &GetTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<TaskPushNotificationConfig, A2aClientError>;

    /// List push notification configs.
    async fn list_task_push_notification_configs(
        &self,
        params: &ListTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aClientError>;

    /// Delete a push notification config.
    async fn delete_task_push_notification_config(
        &self,
        params: &DeleteTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Resubscribe to task events. Completes when the stream ends.
    async fn resubscribe(
        &self,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        handler: ResponseHandler,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Retrieve the agent card through the transport.
    async fn get_card(
        &self,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<AgentCard, A2aClientError>;

    /// Register a global event consumer.
    fn add_event_consumer(&self, consumer: Consumer);

    /// Add a request interceptor.
    fn add_request_middleware(&self, middleware: Arc<dyn ClientCallInterceptor>);

    /// Close the client and its transport.
    fn close(&self);
}

enum Completion {
    Unit(oneshot::Sender<Result<(), A2aClientError>>),
    Task(oneshot::Sender<Result<Task, A2aClientError>>),
    PushConfig(oneshot::Sender<Result<TaskPushNotificationConfig, A2aClientError>>),
    PushConfigs(oneshot::Sender<Result<Vec<TaskPushNotificationConfig>, A2aClientError>>),
    Card(oneshot::Sender<Result<AgentCard, A2aClientError>>),
}

impl Completion {
    fn fail(self, err: A2aClientError) {
        match self {
            Completion::Unit(tx) => {
                let _ = tx.send(Err(err));
            }
            Completion::Task(tx) => {
                let _ = tx.send(Err(err));
            }
            Completion::PushConfig(tx) => {
                let _ = tx.send(Err(err));
            }
            Completion::PushConfigs(tx) => {
                let _ = tx.send(Err(err));
            }
            Completion::Card(tx) => {
                let _ = tx.send(Err(err));
            }
        }
    }
}

struct CallbackInfo {
    request_id: String,
    method: String,
    completion: Option<Completion>,
    handler: Option<ResponseHandler>,
    mgr: Option<ClientTaskManager>,
}

struct Inner {
    card: AgentCard,
    config: ClientConfig,
    transport: Arc<dyn ClientTransport>,
    consumers: Mutex<Vec<Consumer>>,
    callback_info: Mutex<HashMap<String, Arc<Mutex<CallbackInfo>>>>,
}

/// Default [`Client`] on top of a [`ClientTransport`].
#[derive(Clone)]
pub struct DefaultClient {
    inner: Arc<Inner>,
}

fn abandoned() -> A2aClientError {
    A2aClientError::from_code(A2AErrorCode::A2aStatusError, "Request abandoned")
}

fn invalid_input() -> A2aClientError {
    A2aClientError::from_code(A2AErrorCode::A2aInvalidInput, "Invalid parameter")
}

fn transport_exception(e: &A2aClientError) -> A2aClientError {
    A2aClientError::from_code(A2AErrorCode::A2aTransportException, e.message())
}

impl DefaultClient {
    /// Create a client and bind the transport callback to it.
    pub fn new(
        card: AgentCard,
        config: ClientConfig,
        transport: Arc<dyn ClientTransport>,
        consumers: Vec<Consumer>,
    ) -> Self {
        let inner = Arc::new(Inner {
            card,
            config,
            transport: transport.clone(),
            consumers: Mutex::new(consumers),
            callback_info: Mutex::new(HashMap::new()),
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        transport.set_transport_callback(Arc::new(move |request_id, event| {
            if let Some(inner) = weak.upgrade() {
                inner.transport_event_cb(request_id, event);
            }
        }));
        DefaultClient { inner }
    }

    /// The agent card of this client.
    pub fn card(&self) -> &AgentCard {
        &self.inner.card
    }

    /// The client configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// Number of requests waiting for a response.
    pub fn pending_requests(&self) -> usize {
        self.inner.callback_info.lock().len()
    }

    /// Whether streaming is enabled by both the config and the agent card.
    pub fn can_stream(&self) -> bool {
        self.inner.can_stream()
    }

    async fn process_message_request(
        &self,
        params: MessageSendParams,
        context: Option<&ClientCallContext>,
        handler: ResponseHandler,
        timeout: u64,
        method: &str,
    ) -> Result<(), A2aClientError> {
        if params.message.message_id.is_empty() || params.message.parts.is_empty() {
            a2a_log!(A2aLogLevel::Error, "SendMessage invalid parameter");
            return Err(invalid_input());
        }
        let request_id = generate_uuid();
        let (tx, rx) = oneshot::channel();
        let streaming = method != METHOD_MESSAGE_SEND;
        self.inner.register(CallbackInfo {
            request_id: request_id.clone(),
            method: method.to_string(),
            completion: Some(Completion::Unit(tx)),
            handler: Some(handler),
            mgr: streaming.then(ClientTaskManager::new),
        });
        let sent = if streaming {
            self.inner
                .transport
                .send_message_streaming(&request_id, &params, context, timeout)
        } else {
            self.inner
                .transport
                .send_message(&request_id, &params, context, timeout)
        };
        if let Err(e) = sent {
            self.inner.unregister(&request_id);
            return Err(transport_exception(&e));
        }
        rx.await.unwrap_or_else(|_| Err(abandoned()))
    }
}

impl Inner {
    fn can_stream(&self) -> bool {
        self.config.streaming && self.card.capabilities.streaming.unwrap_or(false)
    }

    fn register(&self, info: CallbackInfo) {
        self.callback_info
            .lock()
            .insert(info.request_id.clone(), Arc::new(Mutex::new(info)));
    }

    fn unregister(&self, request_id: &str) {
        self.callback_info.lock().remove(request_id);
    }

    fn consume(&self, ev: &ClientEvent) {
        let consumers = self.consumers.lock().clone();
        for c in consumers {
            c(ev, &self.card);
        }
    }

    fn transport_event_cb(&self, request_id: &str, event: TransportEvent) {
        let cb = {
            let mut map = self.callback_info.lock();
            let Some(cb) = map.get(request_id).cloned() else {
                a2a_log!(A2aLogLevel::Warn, "The eventId not found: {request_id}");
                return;
            };
            let method = cb.lock().method.clone();
            if matches!(event, TransportEvent::Error(_))
                || (method != METHOD_MESSAGE_STREAM && method != METHOD_TASK_RESUBSCRIBE)
            {
                map.remove(request_id);
            }
            cb
        };
        let mut info = cb.lock();
        if let TransportEvent::Error(e) = event {
            if e.error_code == 0 {
                // Clean end of stream: complete without an error.
                a2a_log!(A2aLogLevel::Debug, "requestId: {request_id}, stream finished");
                match info.completion.take() {
                    Some(Completion::Unit(tx)) => {
                        let _ = tx.send(Ok(()));
                    }
                    Some(c) => c.fail(A2aClientError::make(0, String::new())),
                    None => {}
                }
                return;
            }
            a2a_log!(
                A2aLogLevel::Error,
                "requestId: {request_id}, error code: {}, err msg: {}",
                e.error_code,
                e.err_info
            );
            self.handler_error_resp(&mut info, &e);
            return;
        }
        if let Err(e) = self.handler_success_resp(&mut info, event) {
            a2a_log!(
                A2aLogLevel::Error,
                "exception occured: {e}, abandon response and release request: {request_id}"
            );
            self.unregister(request_id);
            info.completion.take();
        }
    }

    fn handler_error_resp(&self, cb: &mut CallbackInfo, e: &TransportError) {
        let err = A2aClientError::make(e.error_code, e.err_info.clone());
        if cb.method == METHOD_MESSAGE_SEND
            || cb.method == METHOD_MESSAGE_STREAM
            || cb.method == METHOD_TASK_RESUBSCRIBE
        {
            a2a_log!(
                A2aLogLevel::Error,
                "method: {}, error code: {}, err msg: {}",
                cb.method,
                e.error_code,
                e.err_info
            );
            let ev = ClientEvent::Error(A2AError::new(e.error_code, e.err_info.clone()));
            if let Some(h) = cb.handler.as_mut() {
                h(&ev, &self.card);
            }
        }
        if let Some(c) = cb.completion.take() {
            c.fail(err);
        }
    }

    fn handler_success_resp(&self, cb: &mut CallbackInfo, event: TransportEvent) -> Result<(), String> {
        match cb.method.as_str() {
            METHOD_MESSAGE_SEND => {
                let ev = match event {
                    TransportEvent::Message(m) => ClientEvent::Message(m),
                    TransportEvent::Task(t) => ClientEvent::TaskUpdate(Box::new(t), UpdateEvent::None),
                    other => return Err(format!("unexpected transport event for SendMessage: {other:?}")),
                };
                if let Some(h) = cb.handler.as_mut() {
                    h(&ev, &self.card);
                }
                self.consume(&ev);
                if let Some(Completion::Unit(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(()));
                }
            }
            METHOD_MESSAGE_STREAM | METHOD_TASK_RESUBSCRIBE => {
                let mgr = cb.mgr.get_or_insert_with(ClientTaskManager::new);
                let mut end = false;
                let ev = match event {
                    TransportEvent::Message(m) => ClientEvent::Message(m),
                    TransportEvent::Task(t) => {
                        mgr.save_task_event(&StreamEvent::Task(t.clone()))?;
                        ClientEvent::TaskUpdate(Box::new(t), UpdateEvent::None)
                    }
                    TransportEvent::StatusUpdate(u) => {
                        mgr.save_task_event(&StreamEvent::StatusUpdate(u.clone()))?;
                        let task = mgr.get_task_or_raise()?.clone();
                        if is_final_or_interrupted(u.status.state) {
                            end = true;
                        }
                        ClientEvent::TaskUpdate(Box::new(task), UpdateEvent::Status(u))
                    }
                    TransportEvent::ArtifactUpdate(a) => {
                        mgr.save_task_event(&StreamEvent::ArtifactUpdate(a.clone()))?;
                        let task = mgr.get_task_or_raise()?.clone();
                        ClientEvent::TaskUpdate(Box::new(task), UpdateEvent::Artifact(a))
                    }
                    other => return Err(format!("unexpected transport event for stream: {other:?}")),
                };
                if let Some(h) = cb.handler.as_mut() {
                    h(&ev, &self.card);
                }
                self.consume(&ev);
                if end {
                    if let Some(Completion::Unit(tx)) = cb.completion.take() {
                        let _ = tx.send(Ok(()));
                    }
                    self.unregister(&cb.request_id);
                }
            }
            METHOD_TASK_GET | METHOD_TASK_CANCEL => {
                let TransportEvent::Task(t) = event else {
                    return Err("expected Task result".to_string());
                };
                if let Some(Completion::Task(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(t));
                }
            }
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET | METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET => {
                let TransportEvent::PushNotificationConfig(c) = event else {
                    return Err("expected TaskPushNotificationConfig result".to_string());
                };
                if let Some(Completion::PushConfig(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(c));
                }
            }
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST => {
                let TransportEvent::PushNotificationConfigs(c) = event else {
                    return Err("expected list result".to_string());
                };
                if let Some(Completion::PushConfigs(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(c));
                }
            }
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE => {
                if let Some(Completion::Unit(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(()));
                }
            }
            METHOD_AGENT_CARD_GET => {
                let TransportEvent::AgentCard(c) = event else {
                    return Err("expected AgentCard result".to_string());
                };
                if let Some(Completion::Card(tx)) = cb.completion.take() {
                    let _ = tx.send(Ok(c));
                }
            }
            other => a2a_log!(
                A2aLogLevel::Warn,
                "method: {other} is ignored by HandlerSuccessResp"
            ),
        }
        Ok(())
    }
}

macro_rules! simple_request {
    ($self:ident, $method:expr_2021, $completion:path, $send:expr_2021) => {{
        let request_id = generate_uuid();
        let (tx, rx) = oneshot::channel();
        $self.inner.register(CallbackInfo {
            request_id: request_id.clone(),
            method: $method.to_string(),
            completion: Some($completion(tx)),
            handler: None,
            mgr: None,
        });
        let sent = $send(&request_id);
        if let Err(e) = sent {
            $self.inner.unregister(&request_id);
            return Err(transport_exception(&e));
        }
        rx.await.unwrap_or_else(|_| Err(abandoned()))
    }};
}

#[async_trait]
impl Client for DefaultClient {
    async fn send_message(
        &self,
        msg: &Message,
        context: Option<&ClientCallContext>,
        handler: ResponseHandler,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let config = &self.inner.config;
        let cfg = MessageSendConfiguration {
            accepted_output_modes: config.accepted_output_modes.clone(),
            history_length: None,
            push_notification_config: config.push_notification_configs.first().cloned(),
            return_immediately: Some(config.polling),
        };
        let params = MessageSendParams {
            configuration: Some(cfg),
            message: msg.clone(),
            metadata: None,
        };
        let method = if self.inner.can_stream() {
            METHOD_MESSAGE_STREAM
        } else {
            METHOD_MESSAGE_SEND
        };
        self.process_message_request(params, context, handler, timeout, method)
            .await
    }

    async fn get_task(
        &self,
        params: &TaskQueryParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Task, A2aClientError> {
        if params.id.is_empty() {
            a2a_log!(A2aLogLevel::Error, "GetTask invalid parameter.");
            return Err(invalid_input());
        }
        simple_request!(self, METHOD_TASK_GET, Completion::Task, |id: &str| self
            .inner
            .transport
            .get_task(id, params, context, timeout))
    }

    async fn cancel_task(
        &self,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Task, A2aClientError> {
        if params.id.is_empty() {
            a2a_log!(A2aLogLevel::Error, "CancelTask invalid parameter.");
            return Err(invalid_input());
        }
        simple_request!(self, METHOD_TASK_CANCEL, Completion::Task, |id: &str| self
            .inner
            .transport
            .cancel_task(id, params, context, timeout))
    }

    async fn set_task_push_notification_config(
        &self,
        cfg: &TaskPushNotificationConfig,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<TaskPushNotificationConfig, A2aClientError> {
        if cfg.task_id.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "SetTaskPushNotificationConfig invalid parameter."
            );
            return Err(invalid_input());
        }
        simple_request!(
            self,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET,
            Completion::PushConfig,
            |id: &str| self
                .inner
                .transport
                .set_task_push_notification_config(id, cfg, context, timeout)
        )
    }

    async fn get_task_push_notification_config(
        &self,
        params: &GetTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<TaskPushNotificationConfig, A2aClientError> {
        if params.id.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "GetTaskPushNotificationConfig invalid parameter."
            );
            return Err(invalid_input());
        }
        simple_request!(
            self,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET,
            Completion::PushConfig,
            |id: &str| self
                .inner
                .transport
                .get_task_push_notification_config(id, params, context, timeout)
        )
    }

    async fn list_task_push_notification_configs(
        &self,
        params: &ListTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aClientError> {
        if params.id.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "ListTaskPushNotificationConfigs invalid parameter."
            );
            return Err(invalid_input());
        }
        simple_request!(
            self,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST,
            Completion::PushConfigs,
            |id: &str| self
                .inner
                .transport
                .list_task_push_notification_configs(id, params, context, timeout)
        )
    }

    async fn delete_task_push_notification_config(
        &self,
        params: &DeleteTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        if params.id.is_empty() {
            a2a_log!(
                A2aLogLevel::Error,
                "DeleteTaskPushNotificationConfig invalid parameter."
            );
            return Err(invalid_input());
        }
        simple_request!(
            self,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE,
            Completion::Unit,
            |id: &str| self
                .inner
                .transport
                .delete_task_push_notification_config(id, params, context, timeout)
        )
    }

    async fn resubscribe(
        &self,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        handler: ResponseHandler,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        if !self.inner.can_stream() {
            return Err(A2aClientError::from_code(
                A2AErrorCode::UnsupportedOperation,
                "client and/or server do not support resubscription.",
            ));
        }
        let request_id = generate_uuid();
        let (tx, rx) = oneshot::channel();
        self.inner.register(CallbackInfo {
            request_id: request_id.clone(),
            method: METHOD_TASK_RESUBSCRIBE.to_string(),
            completion: Some(Completion::Unit(tx)),
            handler: Some(handler),
            mgr: Some(ClientTaskManager::new()),
        });
        if let Err(e) = self
            .inner
            .transport
            .resubscribe(&request_id, params, context, timeout)
        {
            self.inner.unregister(&request_id);
            return Err(transport_exception(&e));
        }
        rx.await.unwrap_or_else(|_| Err(abandoned()))
    }

    async fn get_card(
        &self,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<AgentCard, A2aClientError> {
        simple_request!(self, METHOD_AGENT_CARD_GET, Completion::Card, |id: &str| self
            .inner
            .transport
            .get_card(id, context, timeout))
    }

    fn add_event_consumer(&self, consumer: Consumer) {
        self.inner.consumers.lock().push(consumer);
    }

    fn add_request_middleware(&self, middleware: Arc<dyn ClientCallInterceptor>) {
        self.inner.transport.add_request_middleware(middleware);
    }

    fn close(&self) {
        self.inner.transport.close();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.transport.close();
    }
}

/// A stream of client events produced by [`send_message_stream`].
pub type ClientEventStream = Pin<Box<dyn Stream<Item = ClientEvent> + Send>>;

/// Send a message and receive the events as a [`Stream`].
///
/// The stream ends when the request completes. Errors that stop the
/// request before any handler call are delivered as [`ClientEvent::Error`].
pub fn send_message_stream(
    client: Arc<dyn Client>,
    msg: Message,
    context: Option<ClientCallContext>,
    timeout: u64,
) -> ClientEventStream {
    let (tx, rx) = mpsc::unbounded_channel::<ClientEvent>();
    tokio::spawn(async move {
        let saw_error = Arc::new(AtomicBool::new(false));
        let flag = saw_error.clone();
        let sender = tx.clone();
        let handler: ResponseHandler = Box::new(move |ev: &ClientEvent, _card: &AgentCard| {
            if matches!(ev, ClientEvent::Error(_)) {
                flag.store(true, Ordering::SeqCst);
            }
            let _ = sender.send(ev.clone());
        });
        let result = client
            .send_message(&msg, context.as_ref(), handler, timeout)
            .await;
        if let Err(e) = result {
            if !saw_error.load(Ordering::SeqCst) {
                let _ = tx.send(ClientEvent::Error(e.to_a2a_error()));
            }
        }
    });
    Box::pin(UnboundedReceiverStream::new(rx))
}
