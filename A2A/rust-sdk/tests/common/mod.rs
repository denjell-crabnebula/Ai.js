// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared fixtures for the integration tests.
use std::sync::Arc;

use a2a_sdk::A2aServerError;
use a2a_sdk::server::{AgentExecutor, RequestContext, TaskArtifactParam, TaskUpdater};
use a2a_sdk::types::*;
use async_trait::async_trait;

/// Agent card with one JSON-RPC interface.
pub fn make_agent_card(url: &str, streaming: bool) -> AgentCard {
    AgentCard {
        name: "TestAgent".into(),
        description: "A2A test agent".into(),
        version: "1.0.0".into(),
        default_input_modes: vec!["text".into()],
        default_output_modes: vec!["text".into()],
        capabilities: AgentCapabilities {
            streaming: Some(streaming),
            ..Default::default()
        },
        supported_interfaces: vec![AgentInterface::jsonrpc(url)],
        ..Default::default()
    }
}

/// A user text message.
pub fn user_message(id: &str, text: &str) -> Message {
    Message {
        message_id: id.into(),
        role: Role::User,
        parts: vec![Part::text(text).with_media_type("text/plain")],
        ..Default::default()
    }
}

/// Executor that answers with a processed message, like the hello world example.
pub struct HelloExecutor;

#[async_trait]
impl AgentExecutor for HelloExecutor {
    async fn execute(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.start_work(None);
        let input = context.get_user_input("\n");
        let response = task_updater.new_agent_message(vec![Part::text(format!("Processed: {input}"))], None);
        task_updater.send_response_message(&response);
        Ok(())
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.cancel(None);
        Ok(())
    }
}

/// Executor that streams two artifacts and completes.
pub struct StreamingExecutor;

#[async_trait]
impl AgentExecutor for StreamingExecutor {
    async fn execute(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.start_work(Some(
            task_updater.new_agent_message(vec![Part::text("working")], None),
        ));
        task_updater.add_artifact(&TaskArtifactParam {
            parts: vec![Part::text("chunk-1")],
            artifact_id: Some("art-1".into()),
            ..Default::default()
        });
        task_updater.add_artifact(&TaskArtifactParam {
            parts: vec![Part::text("chunk-2")],
            artifact_id: Some("art-1".into()),
            append: true,
            last_chunk: true,
            ..Default::default()
        });
        task_updater.complete(Some(
            task_updater.new_agent_message(vec![Part::text("done")], None),
        ));
        Ok(())
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.cancel(None);
        Ok(())
    }
}

/// Executor that only starts work and waits for input.
pub struct InputRequiredExecutor;

#[async_trait]
impl AgentExecutor for InputRequiredExecutor {
    async fn execute(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.start_work(None);
        task_updater.requires_input(Some(
            task_updater.new_agent_message(vec![Part::text("more?")], None),
        ));
        Ok(())
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        task_updater.cancel(None);
        Ok(())
    }
}

/// Executor that fails with a server error.
pub struct FailingExecutor;

#[async_trait]
impl AgentExecutor for FailingExecutor {
    async fn execute(
        &self,
        _context: Arc<RequestContext>,
        _task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        Err(A2aServerError::with_code("agent exploded", -32005))
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        _task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        Ok(())
    }
}
