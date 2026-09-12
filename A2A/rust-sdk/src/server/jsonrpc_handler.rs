// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC method handler, the port of `src/server/jsonrpc_handler.*`.

use std::sync::Arc;

use ap_jsonrpc::{RequestId, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{A2AErrorCode, A2aServerError};
use crate::protocol::{JSON_FIELD_ID, JSON_FIELD_PARAMS};
use crate::types::success_response;
use crate::utils::make_error;

use super::request_handler::{RequestHandler, StreamEmitter};

/// Extract the JSON-RPC request id (number or string) of a request object.
pub fn request_id_of(req: &Value) -> Option<RequestId> {
    req.get(JSON_FIELD_ID).and_then(RequestId::from_value)
}

fn params_of<T: DeserializeOwned>(req: &Value) -> Result<T, String> {
    let params = req
        .get(JSON_FIELD_PARAMS)
        .ok_or_else(|| "key 'params' not found".to_string())?;
    serde_json::from_value(params.clone()).map_err(|e| e.to_string())
}

fn internal(id: Option<RequestId>, message: &str) -> Response {
    make_error(id, A2AErrorCode::JsonrpcInternalError.code(), message)
}

fn server_error(id: Option<RequestId>, e: &A2aServerError) -> Response {
    make_error(id, e.status_code(), &e.message())
}

fn ok<T: serde::Serialize>(id: Option<RequestId>, result: &T) -> Response {
    match success_response(id.clone(), result) {
        Ok(r) => r,
        Err(e) => internal(id, &e),
    }
}

/// Light-weight JSON-RPC handler that maps requests to a [`RequestHandler`].
pub struct JsonRpcHandler {
    handler: Arc<dyn RequestHandler>,
}

impl JsonRpcHandler {
    /// Wrap a request handler.
    pub fn new(handler: Arc<dyn RequestHandler>) -> Self {
        JsonRpcHandler { handler }
    }

    /// `message/send`: returns `None` on success (the response goes through `emit`).
    pub async fn on_message_send(&self, req: &Value, emit: StreamEmitter, method: &str) -> Option<Response> {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return Some(internal(id, &e)),
        };
        match self.handler.on_send_message(params, None, emit, method).await {
            Ok(()) => None,
            Err(e) => Some(server_error(id, &e)),
        }
    }

    /// `tasks/get`.
    pub async fn on_get_task(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self.handler.on_get_task(params, None).await {
            Ok(t) => ok(id, &t),
            Err(e) => server_error(id, &e),
        }
    }

    /// `tasks/cancel`.
    pub async fn on_cancel_task(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self.handler.on_cancel_task(params, None).await {
            Ok(t) => ok(id, &t),
            Err(e) => server_error(id, &e),
        }
    }

    /// `tasks/pushNotificationConfig/set`: echoes the config on success.
    pub async fn on_set_push_notification_config(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let cfg: crate::types::TaskPushNotificationConfig = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self
            .handler
            .on_set_task_push_notification_config(cfg.clone(), None)
            .await
        {
            Ok(()) => ok(id, &cfg),
            Err(e) => server_error(id, &e),
        }
    }

    /// `tasks/pushNotificationConfig/get`.
    pub async fn on_get_push_notification_config(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self
            .handler
            .on_get_task_push_notification_config(params, None)
            .await
        {
            Ok(r) => ok(id, &r),
            Err(e) => server_error(id, &e),
        }
    }

    /// `tasks/pushNotificationConfig/list`.
    pub async fn on_list_push_notification_config(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self
            .handler
            .on_list_task_push_notification_configs(params, None)
            .await
        {
            Ok(r) => ok(id, &r),
            Err(e) => server_error(id, &e),
        }
    }

    /// `tasks/pushNotificationConfig/delete`: result is `null` on success.
    pub async fn on_delete_push_notification_config(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        let params = match params_of(req) {
            Ok(p) => p,
            Err(e) => return internal(id, &e),
        };
        match self
            .handler
            .on_delete_task_push_notification_config(params, None)
            .await
        {
            Ok(()) => Response {
                jsonrpc: ap_jsonrpc::JSONRPC_VERSION.to_string(),
                id,
                result: Some(Value::Null),
                error: None,
            },
            Err(e) => server_error(id, &e),
        }
    }

    /// Agent card request.
    pub fn on_get_agent_card(&self, req: &Value) -> Response {
        let id = request_id_of(req);
        match self.handler.on_get_card(None) {
            Ok(c) => ok(id, &c),
            Err(e) => server_error(id, &e),
        }
    }

    /// `message/stream`: parse errors become internal errors prefixed with `Streaming error: `.
    pub async fn on_message_send_streaming(
        &self,
        req: &Value,
        emit: StreamEmitter,
    ) -> Result<(), A2aServerError> {
        let params = params_of(req).map_err(|e| A2aServerError::new(format!("Streaming error: {e}")))?;
        self.handler.on_send_message_streaming(params, emit, None).await
    }

    /// `tasks/resubscribe`: parse errors become internal errors prefixed with `Streaming error: `.
    pub async fn on_resubscribe_to_task(
        &self,
        req: &Value,
        emit: StreamEmitter,
    ) -> Result<(), A2aServerError> {
        let params = params_of(req).map_err(|e| A2aServerError::new(format!("Streaming error: {e}")))?;
        self.handler.on_resubscribe_to_task(params, emit, None).await
    }
}
