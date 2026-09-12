// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client transport interface, the port of `include/client/client_transport.h`.

use std::sync::Arc;

use crate::error::A2aClientError;
use crate::types::{
    AgentCard, ClientCallContext, DeleteTaskPushNotificationConfigParams,
    GetTaskPushNotificationConfigParams, ListTaskPushNotificationConfigParams, Message, MessageSendParams,
    Task, TaskArtifactUpdateEvent, TaskIdParams, TaskPushNotificationConfig, TaskQueryParams,
    TaskStatusUpdateEvent,
};

use super::interceptor::ClientCallInterceptor;

/// Transport-layer error delivered through the transport callback.
///
/// An `error_code` of 0 marks the end of a stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransportError {
    /// A2AErrorCode value, HTTP status, or 0 for end of stream.
    pub error_code: i64,
    /// Human readable description.
    pub err_info: String,
}

impl TransportError {
    /// Build an error.
    pub fn new(error_code: i64, err_info: impl Into<String>) -> Self {
        TransportError {
            error_code,
            err_info: err_info.into(),
        }
    }

    /// End-of-stream marker.
    pub fn stream_end() -> Self {
        TransportError::default()
    }
}

/// Union of all event types a transport may deliver.
#[derive(Clone, Debug, PartialEq)]
pub enum TransportEvent {
    /// A transport or protocol error.
    Error(TransportError),
    /// A message result.
    Message(Message),
    /// A task result.
    Task(Task),
    /// A status update from a stream.
    StatusUpdate(TaskStatusUpdateEvent),
    /// An artifact update from a stream.
    ArtifactUpdate(TaskArtifactUpdateEvent),
    /// A push notification config result.
    PushNotificationConfig(TaskPushNotificationConfig),
    /// A list of push notification configs.
    PushNotificationConfigs(Vec<TaskPushNotificationConfig>),
    /// An empty result (delete).
    None,
    /// An agent card.
    AgentCard(AgentCard),
}

/// Callback invoked with the request id and the parsed response event.
pub type TransportEventCallback = Arc<dyn Fn(&str, TransportEvent) + Send + Sync>;

/// Low-level A2A client transport with an asynchronous callback model.
///
/// Every request method returns as soon as the request is queued. The
/// results arrive through the callback set with
/// [`ClientTransport::set_transport_callback`], correlated by request id.
/// Timeouts are in seconds; 0 selects the default.
pub trait ClientTransport: Send + Sync {
    /// Send a non-streaming `message/send` request.
    fn send_message(
        &self,
        request_id: &str,
        request: &MessageSendParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a streaming `message/stream` request.
    fn send_message_streaming(
        &self,
        request_id: &str,
        request: &MessageSendParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a `tasks/get` request.
    fn get_task(
        &self,
        request_id: &str,
        params: &TaskQueryParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a `tasks/cancel` request.
    fn cancel_task(
        &self,
        request_id: &str,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a push notification config set request.
    fn set_task_push_notification_config(
        &self,
        request_id: &str,
        config: &TaskPushNotificationConfig,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a push notification config get request.
    fn get_task_push_notification_config(
        &self,
        request_id: &str,
        params: &GetTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a push notification config list request.
    fn list_task_push_notification_configs(
        &self,
        request_id: &str,
        params: &ListTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Send a push notification config delete request.
    fn delete_task_push_notification_config(
        &self,
        request_id: &str,
        params: &DeleteTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Resubscribe to task streaming events.
    fn resubscribe(
        &self,
        request_id: &str,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Request the agent card.
    fn get_card(
        &self,
        request_id: &str,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError>;

    /// Register the callback for inbound transport events.
    fn set_transport_callback(&self, callback: TransportEventCallback);

    /// Close the transport. Pending requests fail with `A2A_STATUS_ERROR`.
    fn close(&self);

    /// Add a request interceptor.
    fn add_request_middleware(&self, middleware: Arc<dyn ClientCallInterceptor>);
}
