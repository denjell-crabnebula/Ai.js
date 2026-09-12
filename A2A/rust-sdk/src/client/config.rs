// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client configuration and event types, the port of `include/client/client.h`.

use std::sync::Arc;

use crate::types::{
    A2AError, AgentCard, Message, PushNotificationConfig, Task, TaskArtifactUpdateEvent,
    TaskStatusUpdateEvent,
};

/// Configuration for A2A client behaviour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// Whether the client supports streaming (`message/stream`).
    pub streaming: bool,
    /// Prefer polling over blocking for `message/send` (`returnImmediately`).
    pub polling: bool,
    /// Ordered transport labels, for example `JSONRPC`.
    pub supported_transports: Vec<String>,
    /// Prefer the client transport list over the server order.
    pub use_client_preference: bool,
    /// Accepted output modes for `message/send`.
    pub accepted_output_modes: Option<Vec<String>>,
    /// Default push notification configs; the first one is sent.
    pub push_notification_configs: Vec<PushNotificationConfig>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            streaming: true,
            polling: false,
            supported_transports: Vec::new(),
            use_client_preference: false,
            accepted_output_modes: None,
            push_notification_configs: Vec::new(),
        }
    }
}

/// Streaming update payload attached to a task snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateEvent {
    /// No update, the task itself was received.
    #[default]
    None,
    /// A status update.
    Status(TaskStatusUpdateEvent),
    /// An artifact update.
    Artifact(TaskArtifactUpdateEvent),
}

/// Client-side event: message, error, or task update.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    /// A message from the agent.
    Message(Message),
    /// A protocol or transport error.
    Error(A2AError),
    /// The current task snapshot with the update that produced it.
    TaskUpdate(Box<Task>, UpdateEvent),
}

/// Callback that receives every client event together with the agent card.
pub type Consumer = Arc<dyn Fn(&ClientEvent, &AgentCard) + Send + Sync>;

/// Per-request response handler, invoked per streaming event or once.
pub type ResponseHandler = Box<dyn FnMut(&ClientEvent, &AgentCard) + Send>;
