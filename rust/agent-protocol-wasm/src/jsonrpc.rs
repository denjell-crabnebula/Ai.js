// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC 2.0 and SSE helpers exposed to JavaScript.
//!
//! The functions build wire text a JavaScript client sends with `fetch`, and
//! classify the text it receives. [`SseParser`] turns a streamed
//! `text/event-stream` body into event objects, which is how MCP Streamable
//! HTTP and A2A `message/stream` deliver server messages.

use ap_jsonrpc::{Incoming, Message, Notification, Request, RequestId, Response, RpcError, sse};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::convert::{from_js, to_js};

fn request_id(id: JsValue) -> Result<RequestId, JsError> {
    match from_js(id)? {
        Value::Number(n) => n
            .as_i64()
            .map(RequestId::Number)
            .ok_or_else(|| JsError::new("request id must be an integer or a string")),
        Value::String(s) => Ok(RequestId::String(s)),
        _ => Err(JsError::new("request id must be an integer or a string")),
    }
}

fn optional_params(params: JsValue) -> Result<Option<Value>, JsError> {
    if params.is_undefined() || params.is_null() {
        Ok(None)
    } else {
        from_js(params).map(Some)
    }
}

/// Serialize a JSON-RPC request. `id` is an integer or string, `params` any JSON value or `undefined`.
#[wasm_bindgen(js_name = buildRequest)]
pub fn build_request(id: JsValue, method: &str, params: JsValue) -> Result<String, JsError> {
    let req = Request::new(request_id(id)?, method, optional_params(params)?);
    req.to_json().map_err(|e| JsError::new(&e.to_string()))
}

/// Serialize a JSON-RPC notification.
#[wasm_bindgen(js_name = buildNotification)]
pub fn build_notification(method: &str, params: JsValue) -> Result<String, JsError> {
    let n = Notification::new(method, optional_params(params)?);
    n.to_json().map_err(|e| JsError::new(&e.to_string()))
}

/// Serialize a successful JSON-RPC response.
#[wasm_bindgen(js_name = buildResponse)]
pub fn build_response(id: JsValue, result: JsValue) -> Result<String, JsError> {
    let r = Response::success(request_id(id)?, from_js(result)?);
    r.to_json().map_err(|e| JsError::new(&e.to_string()))
}

/// Serialize a JSON-RPC error response. Pass `null` as `id` when the request id is unknown.
#[wasm_bindgen(js_name = buildErrorResponse)]
pub fn build_error_response(id: JsValue, code: i64, message: &str, data: JsValue) -> Result<String, JsError> {
    let id = if id.is_undefined() || id.is_null() {
        None
    } else {
        Some(request_id(id)?)
    };
    let mut err = RpcError::new(code, message);
    if let Some(d) = optional_params(data)? {
        err = err.with_data(d);
    }
    Response::error(id, err)
        .to_json()
        .map_err(|e| JsError::new(&e.to_string()))
}

fn message_to_value(m: &Message) -> Value {
    match m {
        Message::Request(r) => json!({
            "kind": "request",
            "id": r.id,
            "method": r.method,
            "params": r.params,
        }),
        Message::Notification(n) => json!({
            "kind": "notification",
            "method": n.method,
            "params": n.params,
        }),
        Message::Response(r) => json!({
            "kind": "response",
            "id": r.id,
            "result": r.result,
            "error": r.error,
        }),
    }
}

/// Parse one JSON-RPC message or a batch.
///
/// Returns `{kind: "request" | "notification" | "response", ...}` for a single
/// message, or an array of such objects for a batch. Throws on invalid input.
#[wasm_bindgen(js_name = parseMessage)]
pub fn parse_message(text: &str) -> Result<JsValue, JsError> {
    let value = match Message::parse_incoming(text).map_err(|e| JsError::new(&e.to_string()))? {
        Incoming::Single(m) => message_to_value(&m),
        Incoming::Batch(list) => Value::Array(list.iter().map(message_to_value).collect()),
    };
    to_js(&value)
}

/// Standard JSON-RPC error codes as an object.
#[wasm_bindgen(js_name = errorCodes)]
pub fn error_codes() -> Result<JsValue, JsError> {
    use ap_jsonrpc::error_codes as c;
    to_js(&json!({
        "PARSE_ERROR": c::PARSE_ERROR,
        "INVALID_REQUEST": c::INVALID_REQUEST,
        "METHOD_NOT_FOUND": c::METHOD_NOT_FOUND,
        "INVALID_PARAMS": c::INVALID_PARAMS,
        "INTERNAL_ERROR": c::INTERNAL_ERROR,
        "SERVER_ERROR": c::SERVER_ERROR,
    }))
}

/// Incremental Server-Sent Events parser.
///
/// Feed body chunks from a streaming `fetch` response; each call returns the
/// events completed by that chunk as `{event, data, id, retry}` objects.
#[wasm_bindgen]
pub struct SseParser {
    inner: sse::SseParser,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl SseParser {
    /// Create an empty parser.
    #[wasm_bindgen(constructor)]
    pub fn new() -> SseParser {
        SseParser {
            inner: sse::SseParser::new(),
        }
    }

    /// Feed raw bytes. Returns an array of completed events.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<JsValue, JsError> {
        let events: Vec<Value> = self.inner.feed(chunk).iter().map(event_to_value).collect();
        to_js(&events)
    }

    /// Feed text (a convenience over `feed` for already decoded chunks).
    #[wasm_bindgen(js_name = feedText)]
    pub fn feed_text(&mut self, chunk: &str) -> Result<JsValue, JsError> {
        self.feed(chunk.as_bytes())
    }

    /// Flush a trailing event at end of stream, if any.
    pub fn finish(&mut self) -> Result<JsValue, JsError> {
        match self.inner.finish() {
            Some(ev) => to_js(&event_to_value(&ev)),
            None => Ok(JsValue::NULL),
        }
    }
}

fn event_to_value(ev: &sse::SseEvent) -> Value {
    json!({
        "event": ev.event,
        "data": ev.data,
        "id": ev.id,
        "retry": ev.retry,
    })
}

/// Format an SSE event (`{event?, data, id?}`) as wire text, including the blank line.
#[wasm_bindgen(js_name = formatSseEvent)]
pub fn format_sse_event(event: JsValue) -> Result<String, JsError> {
    let v = from_js(event)?;
    let ev = sse::SseEvent {
        event: v.get("event").and_then(Value::as_str).map(str::to_string),
        data: v
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        id: v.get("id").and_then(Value::as_str).map(str::to_string),
        retry: v.get("retry").and_then(Value::as_u64),
    };
    Ok(ev.to_wire())
}
