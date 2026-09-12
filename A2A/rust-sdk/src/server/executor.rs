// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Agent executor interface, the port of `include/server/agent_executor.h`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::A2aServerError;

use super::request_context::RequestContext;
use super::task_updater::TaskUpdater;

/// Application hook that runs the agent logic for a request.
///
/// Progress is published through the [`TaskUpdater`]. An `Err` is mapped
/// to a JSON-RPC error response.
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    /// Execute the agent for a `message/send` or `message/stream` request.
    async fn execute(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError>;

    /// Execute with the JSON-RPC method name. Defaults to [`Self::execute`].
    async fn execute_with_method(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
        _method: &str,
    ) -> Result<(), A2aServerError> {
        self.execute(context, task_updater).await
    }

    /// Cancel a running task.
    async fn cancel(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError>;
}
