// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server call context, the port of `include/server/server_call_context.h`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::types::{MessageSendParams, Task};

use super::task_store::TaskStore;

/// Per-call server context shared between the handler, the store and the executor.
#[derive(Debug, Default)]
pub struct ServerCallContext {
    /// Arbitrary key-value state.
    pub state: RwLock<HashMap<String, String>>,
    /// Extensions requested by the client.
    pub requested_extensions: RwLock<HashSet<String>>,
    /// Extensions activated by the agent.
    pub activated_extensions: RwLock<HashSet<String>>,
}

impl ServerCallContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Parameters used to build a [`super::RequestContext`].
#[derive(Clone, Default)]
pub struct RequestContextParam {
    /// The incoming send request, if any.
    pub request: Option<MessageSendParams>,
    /// Task identifier.
    pub task_id: Option<String>,
    /// Context identifier.
    pub context_id: Option<String>,
    /// Task store used to look up the current task.
    pub task_store: Option<Arc<dyn TaskStore>>,
    /// Tasks referenced by the request.
    pub related_tasks: Vec<Task>,
    /// Call context.
    pub call_context: Option<Arc<ServerCallContext>>,
}
