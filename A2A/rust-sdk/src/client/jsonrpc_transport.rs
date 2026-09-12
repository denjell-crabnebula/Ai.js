// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC over HTTP client transport using reqwest, the port of
//! `jsonrpc_transport_impl.*` and `connection/libcurl_conn.*`.
//!
//! Non-streaming requests are HTTP POSTs with a JSON body. Streaming
//! requests (`message/stream`, `tasks/resubscribe`) read a `text/event-stream`
//! body where every SSE data event is a JSON-RPC response.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ap_jsonrpc::sse::SseParser;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::a2a_log;
use crate::error::{A2AErrorCode, A2aClientError};
use crate::log::A2aLogLevel;
use crate::protocol::http::{
    ACCEPT_ENCODING_HEADER, ACCEPT_ENCODING_VALUE, ACCEPT_HEADER, CACHE_CONTROL_HEADER,
    CACHE_CONTROL_NO_CACHE_NO_TRANSFORM, CONNECTION_HEADER, CONNECTION_KEEP_ALIVE, CONTENT_TYPE_HEADER,
    CONTENT_TYPE_JSON, CONTENT_TYPE_SSE, DEFAULT_CONNECTION_TIMEOUT_SECS, DEFAULT_REQUEST_TIMEOUT_SECS,
    HTTP_PARSE_ERROR, HTTP_STATUS_NOT_FOUND, HTTP_STATUS_OK,
};
use crate::protocol::{
    JSON_FIELD_ERROR, JSON_FIELD_ID, JSON_FIELD_JSONRPC, JSON_FIELD_METHOD, JSON_FIELD_PARAMS,
    JSON_FIELD_RESULT, JSONRPC_VERSION, METHOD_AGENT_CARD_GET, METHOD_MESSAGE_SEND, METHOD_MESSAGE_STREAM,
    METHOD_TASK_CANCEL, METHOD_TASK_GET, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET, METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST,
    METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET, METHOD_TASK_RESUBSCRIBE,
};
use crate::types::{
    A2AError, AgentCard, ClientCallContext, DeleteTaskPushNotificationConfigParams,
    GetTaskPushNotificationConfigParams, ListTaskPushNotificationConfigParams, MessageSendParams,
    SendMessageResult, StreamEvent, Task, TaskIdParams, TaskPushNotificationConfig, TaskQueryParams,
};
use crate::utils::is_final_or_interrupted;

use super::config::ClientConfig;
use super::interceptor::ClientCallInterceptor;
use super::transport::{ClientTransport, TransportError, TransportEvent, TransportEventCallback};

/// Book-keeping for one in-flight request.
#[derive(Clone, Debug)]
pub struct UserData {
    /// Request id.
    pub request_id: String,
    /// JSON-RPC method.
    pub method: String,
    /// Whether the response is a stream.
    pub is_stream: bool,
    /// Timeout in seconds, 0 for the default.
    pub timeout: u64,
}

struct Inner {
    url: String,
    agent_card: AgentCard,
    interceptors: Mutex<Vec<Arc<dyn ClientCallInterceptor>>>,
    callback: Mutex<Option<TransportEventCallback>>,
    request_data: Mutex<HashMap<String, UserData>>,
    http: reqwest::Client,
    extra_headers: BTreeMap<String, String>,
    cancel: CancellationToken,
    closed: AtomicBool,
}

/// JSON-RPC over HTTP implementation of [`ClientTransport`].
#[derive(Clone)]
pub struct JsonRpcTransport {
    inner: Arc<Inner>,
}

impl JsonRpcTransport {
    /// Create a transport for a JSON-RPC endpoint URL.
    pub fn new(
        url: impl Into<String>,
        agent_card: &AgentCard,
        config: &ClientConfig,
        interceptors: Vec<Arc<dyn ClientCallInterceptor>>,
    ) -> Self {
        Self::with_headers(url, agent_card, config, interceptors, BTreeMap::new())
    }

    /// Create a transport that adds static headers to every request.
    pub fn with_headers(
        url: impl Into<String>,
        agent_card: &AgentCard,
        _config: &ClientConfig,
        interceptors: Vec<Arc<dyn ClientCallInterceptor>>,
        extra_headers: BTreeMap<String, String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(DEFAULT_CONNECTION_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();
        JsonRpcTransport {
            inner: Arc::new(Inner {
                url: url.into(),
                agent_card: agent_card.clone(),
                interceptors: Mutex::new(interceptors),
                callback: Mutex::new(None),
                request_data: Mutex::new(HashMap::new()),
                http,
                extra_headers,
                cancel: CancellationToken::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Endpoint URL.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// Number of in-flight requests.
    pub fn pending_requests(&self) -> usize {
        self.inner.request_data.lock().len()
    }

    /// Whether `close` was called.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    fn send(
        &self,
        request_id: &str,
        method: &str,
        payload: Value,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let rpc = serde_json::json!({
            JSON_FIELD_JSONRPC: JSONRPC_VERSION,
            JSON_FIELD_ID: request_id,
            JSON_FIELD_METHOD: method,
            JSON_FIELD_PARAMS: payload,
        });
        let mut data = rpc.to_string();
        let mut headers = BTreeMap::new();
        self.inner
            .apply_interceptors(method, &mut data, &mut headers, context);
        let user = UserData {
            request_id: request_id.to_string(),
            method: method.to_string(),
            is_stream: method == METHOD_MESSAGE_STREAM || method == METHOD_TASK_RESUBSCRIBE,
            timeout,
        };
        self.inner.submit(user, data, headers)
    }
}

impl Inner {
    fn apply_interceptors(
        &self,
        method: &str,
        payload: &mut String,
        headers: &mut BTreeMap<String, String>,
        context: Option<&ClientCallContext>,
    ) {
        let interceptors = self.interceptors.lock().clone();
        for i in &interceptors {
            i.intercept(method, payload, headers, Some(&self.agent_card), context);
        }
    }

    fn submit(
        self: &Arc<Self>,
        user: UserData,
        body: String,
        headers: BTreeMap<String, String>,
    ) -> Result<(), A2aClientError> {
        if self.closed.load(Ordering::SeqCst) {
            a2a_log!(A2aLogLevel::Error, "transport is not running");
            return Err(A2aClientError::from_code(
                A2AErrorCode::A2aTransportException,
                "transport exception",
            ));
        }
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            A2aClientError::from_code(
                A2AErrorCode::A2aTransportException,
                "transport exception: no tokio runtime",
            )
        })?;
        self.request_data
            .lock()
            .insert(user.request_id.clone(), user.clone());
        let inner = self.clone();
        handle.spawn(async move {
            let cancel = inner.cancel.clone();
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = inner.run_request(user, body, headers) => {}
            }
        });
        Ok(())
    }

    fn emit(&self, request_id: &str, event: TransportEvent) {
        let cb = self.callback.lock().clone();
        if let Some(cb) = cb {
            cb(request_id, event);
        }
    }

    /// Remove the request; returns false when it was already finished.
    fn finish(&self, request_id: &str) -> bool {
        self.request_data.lock().remove(request_id).is_some()
    }

    fn fail(&self, user: &UserData, code: i64, message: impl Into<String>) {
        let message = message.into();
        a2a_log!(A2aLogLevel::Error, "Error msg: {message}, error code: {code}");
        if !self.finish(&user.request_id) {
            return;
        }
        self.emit(
            &user.request_id,
            TransportEvent::Error(TransportError::new(code, message)),
        );
    }

    async fn run_request(self: Arc<Self>, user: UserData, body: String, headers: BTreeMap<String, String>) {
        let timeout = if user.timeout > 0 {
            user.timeout
        } else {
            DEFAULT_REQUEST_TIMEOUT_SECS
        };
        let mut req = if user.method == METHOD_AGENT_CARD_GET {
            self.http
                .get(&self.url)
                .header(ACCEPT_ENCODING_HEADER, ACCEPT_ENCODING_VALUE)
                .header(ACCEPT_HEADER, "*/*")
                .header(CONNECTION_HEADER, CONNECTION_KEEP_ALIVE)
        } else {
            let mut r = self
                .http
                .post(&self.url)
                .header(CONTENT_TYPE_HEADER, CONTENT_TYPE_JSON);
            if user.is_stream {
                r = r
                    .header(ACCEPT_HEADER, CONTENT_TYPE_SSE)
                    .header(CONNECTION_HEADER, CONNECTION_KEEP_ALIVE)
                    .header(CACHE_CONTROL_HEADER, CACHE_CONTROL_NO_CACHE_NO_TRANSFORM);
            } else {
                r = r.header(ACCEPT_HEADER, CONTENT_TYPE_JSON);
            }
            r.body(body)
        };
        for (k, v) in self.extra_headers.iter().chain(headers.iter()) {
            req = req.header(k, v);
        }
        if !user.is_stream {
            req = req.timeout(Duration::from_secs(timeout));
        }
        a2a_log!(
            A2aLogLevel::Debug,
            "Send request {}: {}",
            user.request_id,
            self.url
        );

        let response = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                self.fail(
                    &user,
                    A2AErrorCode::A2aRequestTimeout.code(),
                    format!("HTTP request failed: {e}"),
                );
                return;
            }
            Err(e) => {
                self.fail(
                    &user,
                    A2AErrorCode::A2aTransportException.code(),
                    format!("HTTP request failed: {e}"),
                );
                return;
            }
        };
        let status = i64::from(response.status().as_u16());
        if status == HTTP_STATUS_NOT_FOUND {
            self.fail(&user, status, "Session not found or expired");
            return;
        }
        if status != HTTP_STATUS_OK {
            self.fail(&user, status, "Http status not ok");
            return;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let Some(content_type) = content_type else {
            a2a_log!(A2aLogLevel::Error, "receive unexpected content type: <missing>");
            self.fail(&user, HTTP_PARSE_ERROR, "Content-Type not found in header");
            return;
        };
        if content_type.contains(CONTENT_TYPE_JSON) {
            match tokio::time::timeout(Duration::from_secs(timeout), response.text()).await {
                Ok(Ok(text)) => self.handle_json_body(&user, &text),
                Ok(Err(e)) => self.fail(
                    &user,
                    A2AErrorCode::A2aTransportException.code(),
                    format!("HTTP request failed: {e}"),
                ),
                Err(_) => self.fail(
                    &user,
                    A2AErrorCode::A2aRequestTimeout.code(),
                    "HTTP request failed: timeout",
                ),
            }
        } else if content_type.contains(CONTENT_TYPE_SSE) {
            self.handle_sse_body(&user, response, timeout).await;
        } else {
            a2a_log!(
                A2aLogLevel::Error,
                "receive unexpected content type: {content_type}"
            );
            self.fail(
                &user,
                HTTP_PARSE_ERROR,
                "application/json or text/event-stream not found in header",
            );
        }
    }

    fn handle_json_body(&self, user: &UserData, text: &str) {
        if text.is_empty() {
            if !user.is_stream {
                self.fail(user, HTTP_PARSE_ERROR, "Empty non-streaming response");
            }
            return;
        }
        let json: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                a2a_log!(A2aLogLevel::Warn, "invalid data from transport layer");
                self.fail(user, A2AErrorCode::JsonrpcParseError.code(), e.to_string());
                return;
            }
        };
        if user.is_stream {
            self.on_stream_resp(user, &json);
            return;
        }
        if !self.finish(&user.request_id) {
            return;
        }
        self.on_non_stream_resp(user, &json);
    }

    async fn handle_sse_body(&self, user: &UserData, response: reqwest::Response, timeout: u64) {
        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(timeout), stream.next()).await;
            let chunk = match next {
                Ok(Some(Ok(bytes))) => bytes,
                Ok(Some(Err(e))) => {
                    self.fail(
                        user,
                        A2AErrorCode::A2aTransportException.code(),
                        format!("HTTP request failed: {e}"),
                    );
                    return;
                }
                Ok(None) => break,
                Err(_) => {
                    self.fail(user, A2AErrorCode::A2aRequestTimeout.code(), "SSE read timeout");
                    return;
                }
            };
            for event in parser.feed(&chunk) {
                if self.deliver_sse_event(user, &event.data) {
                    return;
                }
            }
        }
        if let Some(last) = parser.finish() {
            if self.deliver_sse_event(user, &last.data) {
                return;
            }
        }
        self.on_stream_fin(user);
    }

    /// Deliver one SSE data payload. Returns true when the stream is finished.
    fn deliver_sse_event(&self, user: &UserData, data: &str) -> bool {
        if data.is_empty() {
            self.on_stream_fin(user);
            return true;
        }
        a2a_log!(A2aLogLevel::Debug, "SSE event end, event data: {data}");
        let json: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                a2a_log!(A2aLogLevel::Warn, "invalid data from transport layer");
                self.fail(user, A2AErrorCode::JsonrpcParseError.code(), e.to_string());
                return true;
            }
        };
        !self.on_stream_resp(user, &json)
    }

    fn on_stream_fin(&self, user: &UserData) {
        if !self.finish(&user.request_id) {
            return;
        }
        self.emit(
            &user.request_id,
            TransportEvent::Error(TransportError::stream_end()),
        );
    }

    /// Handle one streaming response. Returns true when the stream stays open.
    fn on_stream_resp(&self, user: &UserData, data: &Value) -> bool {
        if let Some(err) = data.get(JSON_FIELD_ERROR) {
            let err: A2AError = serde_json::from_value(err.clone()).unwrap_or_default();
            if !self.finish(&user.request_id) {
                return false;
            }
            self.emit(
                &user.request_id,
                TransportEvent::Error(TransportError::new(err.code, err.message.unwrap_or_default())),
            );
            return false;
        }
        if !Self::id_matches(data, &user.request_id) {
            self.fail(
                user,
                A2AErrorCode::InvalidAgentResponse.code(),
                "RequestId in response data does not match with requestId in request",
            );
            return false;
        }
        let result = data.get(JSON_FIELD_RESULT).cloned().unwrap_or(Value::Null);
        let event: StreamEvent = match serde_json::from_value(result) {
            Ok(e) => e,
            Err(e) => {
                self.fail(user, A2AErrorCode::JsonrpcParseError.code(), e.to_string());
                return false;
            }
        };
        let mut open = true;
        if let StreamEvent::StatusUpdate(u) = &event {
            if is_final_or_interrupted(u.status.state) {
                if !self.finish(&user.request_id) {
                    return false;
                }
                open = false;
            }
        }
        let ev = match event {
            StreamEvent::Task(t) => TransportEvent::Task(t),
            StreamEvent::Message(m) => TransportEvent::Message(m),
            StreamEvent::StatusUpdate(u) => TransportEvent::StatusUpdate(u),
            StreamEvent::ArtifactUpdate(a) => TransportEvent::ArtifactUpdate(a),
        };
        self.emit(&user.request_id, ev);
        open
    }

    fn id_matches(data: &Value, request_id: &str) -> bool {
        match data.get(JSON_FIELD_ID) {
            None => true,
            Some(Value::String(s)) => s == request_id,
            Some(Value::Number(n)) => n.to_string() == request_id,
            Some(_) => false,
        }
    }

    fn on_non_stream_resp(&self, user: &UserData, data: &Value) {
        let ev = if !Self::id_matches(data, &user.request_id) {
            TransportEvent::Error(TransportError::new(
                A2AErrorCode::InvalidAgentResponse.code(),
                "RequestId in response data does not match with requestId in request",
            ))
        } else if let Some(err) = data.get(JSON_FIELD_ERROR) {
            let err: A2AError = serde_json::from_value(err.clone()).unwrap_or_default();
            TransportEvent::Error(TransportError::new(err.code, err.message.unwrap_or_default()))
        } else {
            match Self::parse_result(&user.method, data) {
                Ok(ev) => ev,
                Err(e) => {
                    TransportEvent::Error(TransportError::new(A2AErrorCode::A2aInvalidFormat.code(), e))
                }
            }
        };
        self.emit(&user.request_id, ev);
    }

    fn parse_result(method: &str, data: &Value) -> Result<TransportEvent, String> {
        fn result<T: serde::de::DeserializeOwned>(data: &Value) -> Result<T, String> {
            let r = data
                .get(JSON_FIELD_RESULT)
                .ok_or_else(|| "Response missing 'result' field".to_string())?;
            serde_json::from_value(r.clone()).map_err(|e| e.to_string())
        }
        Ok(match method {
            METHOD_MESSAGE_SEND => match result::<SendMessageResult>(data)? {
                SendMessageResult::Task(t) => TransportEvent::Task(*t),
                SendMessageResult::Message(m) => TransportEvent::Message(*m),
            },
            METHOD_TASK_GET | METHOD_TASK_CANCEL => TransportEvent::Task(result::<Task>(data)?),
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET | METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET => {
                TransportEvent::PushNotificationConfig(result::<TaskPushNotificationConfig>(data)?)
            }
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST => {
                TransportEvent::PushNotificationConfigs(result::<Vec<TaskPushNotificationConfig>>(data)?)
            }
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE => TransportEvent::None,
            METHOD_AGENT_CARD_GET => {
                if data.get(JSON_FIELD_RESULT).is_some() {
                    TransportEvent::AgentCard(result::<AgentCard>(data)?)
                } else {
                    TransportEvent::AgentCard(
                        serde_json::from_value(data.clone()).map_err(|e| e.to_string())?,
                    )
                }
            }
            _ => TransportEvent::Error(TransportError::new(
                A2AErrorCode::JsonrpcMethodNotFound.code(),
                "Method not found in response data",
            )),
        })
    }
}

impl ClientTransport for JsonRpcTransport {
    fn send_message(
        &self,
        request_id: &str,
        request: &MessageSendParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(request)?;
        self.send(request_id, METHOD_MESSAGE_SEND, payload, context, timeout)
    }

    fn send_message_streaming(
        &self,
        request_id: &str,
        request: &MessageSendParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(request)?;
        self.send(request_id, METHOD_MESSAGE_STREAM, payload, context, timeout)
    }

    fn get_task(
        &self,
        request_id: &str,
        params: &TaskQueryParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(request_id, METHOD_TASK_GET, payload, context, timeout)
    }

    fn cancel_task(
        &self,
        request_id: &str,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(request_id, METHOD_TASK_CANCEL, payload, context, timeout)
    }

    fn set_task_push_notification_config(
        &self,
        request_id: &str,
        config: &TaskPushNotificationConfig,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(config)?;
        self.send(
            request_id,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_SET,
            payload,
            context,
            timeout,
        )
    }

    fn get_task_push_notification_config(
        &self,
        request_id: &str,
        params: &GetTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(
            request_id,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_GET,
            payload,
            context,
            timeout,
        )
    }

    fn list_task_push_notification_configs(
        &self,
        request_id: &str,
        params: &ListTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(
            request_id,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_LIST,
            payload,
            context,
            timeout,
        )
    }

    fn delete_task_push_notification_config(
        &self,
        request_id: &str,
        params: &DeleteTaskPushNotificationConfigParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(
            request_id,
            METHOD_TASK_PUSH_NOTIFICATION_CONFIG_DELETE,
            payload,
            context,
            timeout,
        )
    }

    fn resubscribe(
        &self,
        request_id: &str,
        params: &TaskIdParams,
        context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let payload = to_value(params)?;
        self.send(request_id, METHOD_TASK_RESUBSCRIBE, payload, context, timeout)
    }

    fn get_card(
        &self,
        request_id: &str,
        _context: Option<&ClientCallContext>,
        timeout: u64,
    ) -> Result<(), A2aClientError> {
        let user = UserData {
            request_id: request_id.to_string(),
            method: METHOD_AGENT_CARD_GET.to_string(),
            is_stream: false,
            timeout,
        };
        self.inner.submit(user, String::new(), BTreeMap::new())
    }

    fn set_transport_callback(&self, callback: TransportEventCallback) {
        *self.inner.callback.lock() = Some(callback);
    }

    fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.cancel.cancel();
        let pending: Vec<UserData> = self.inner.request_data.lock().drain().map(|(_, v)| v).collect();
        for user in pending {
            self.inner.emit(
                &user.request_id,
                TransportEvent::Error(TransportError::new(
                    A2AErrorCode::A2aStatusError.code(),
                    "Transport is closed",
                )),
            );
        }
    }

    fn add_request_middleware(&self, middleware: Arc<dyn ClientCallInterceptor>) {
        self.inner.interceptors.lock().push(middleware);
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, A2aClientError> {
    serde_json::to_value(value)
        .map_err(|e| A2aClientError::from_code(A2AErrorCode::A2aInvalidInput, e.to_string()))
}
