// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC 2.0 message model and Server-Sent Events (SSE) parsing shared by
//! the MCP and A2A SDKs.
//!
//! The types here are deliberately transport-agnostic. They model the wire
//! format only: request, notification, response (success or error) and the
//! JSON-RPC error object. Method-specific parameter and result types live in
//! the protocol crates and are carried here as [`serde_json::Value`].
//!
//! # Example
//!
//! ```
//! use ap_jsonrpc::{Message, Request, RequestId, Response, RpcError};
//! use serde_json::json;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let req = Request::new(RequestId::from(1), "ping", None);
//! let text = req.to_json()?;
//! let parsed = Message::parse(&text)?;
//! assert!(matches!(parsed, Message::Request(_)));
//!
//! let ok = Response::success(RequestId::from(1), json!({}));
//! assert!(ok.error.is_none());
//! let err = Response::error(Some(RequestId::from(1)), RpcError::method_not_found("nope"));
//! assert_eq!(err.error.map(|e| e.code), Some(ap_jsonrpc::error_codes::METHOD_NOT_FOUND));
//! # Ok(())
//! # }
//! ```

pub mod sse;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The only JSON-RPC version this crate speaks.
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard JSON-RPC 2.0 error codes plus the reserved server-error base.
pub mod error_codes {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist or is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Start of the implementation-defined server error range (-32000..=-32099).
    pub const SERVER_ERROR: i64 = -32000;
}

/// A JSON-RPC request identifier. The specification allows numbers and
/// strings; both SDKs use 64-bit integers and strings.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl From<&RequestId> for Value {
    fn from(id: &RequestId) -> Value {
        match id {
            RequestId::Number(n) => Value::from(*n),
            RequestId::String(s) => Value::String(s.clone()),
        }
    }
}

impl RequestId {
    /// Render the id as text. Numbers render without quotes.
    pub fn as_key(&self) -> String {
        match self {
            RequestId::Number(n) => n.to_string(),
            RequestId::String(s) => s.clone(),
        }
    }

    /// Parse an id out of a JSON value. `null` and other types yield `None`.
    pub fn from_value(value: &Value) -> Option<RequestId> {
        match value {
            Value::Number(n) => n.as_i64().map(RequestId::Number),
            Value::String(s) => Some(RequestId::String(s.clone())),
            _ => None,
        }
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s}"),
        }
    }
}

impl From<i64> for RequestId {
    fn from(v: i64) -> Self {
        RequestId::Number(v)
    }
}

impl From<i32> for RequestId {
    fn from(v: i32) -> Self {
        RequestId::Number(v as i64)
    }
}

impl From<u32> for RequestId {
    fn from(v: u32) -> Self {
        RequestId::Number(v as i64)
    }
}

impl From<&str> for RequestId {
    fn from(v: &str) -> Self {
        RequestId::String(v.to_string())
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        RequestId::String(v)
    }
}

/// The JSON-RPC error object: `{ "code": int, "message": string, "data"?: any }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// The error as a JSON value.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("code".into(), Value::from(self.code));
        map.insert("message".into(), Value::String(self.message.clone()));
        if let Some(data) = &self.data {
            map.insert("data".into(), data.clone());
        }
        Value::Object(map)
    }

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(error_codes::PARSE_ERROR, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: impl AsRef<str>) -> Self {
        Self::new(
            error_codes::METHOD_NOT_FOUND,
            format!("Method not found: {}", method.as_ref()),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL_ERROR, message)
    }

    pub fn server_error(message: impl Into<String>) -> Self {
        Self::new(error_codes::SERVER_ERROR, message)
    }

    /// True when the code is one of the five standard JSON-RPC codes.
    pub fn is_standard_code(&self) -> bool {
        matches!(
            self.code,
            error_codes::PARSE_ERROR
                | error_codes::INVALID_REQUEST
                | error_codes::METHOD_NOT_FOUND
                | error_codes::INVALID_PARAMS
                | error_codes::INTERNAL_ERROR
        )
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// A JSON-RPC request (carries an id and expects a response).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }

    /// Deserialize `params` into a typed value. Missing params deserialize
    /// from `null`, so optional parameter structs can use `#[serde(default)]`.
    pub fn params_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, RpcError> {
        let value = self.params.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("jsonrpc".into(), Value::String(self.jsonrpc.clone()));
        map.insert("id".into(), Value::from(&self.id));
        map.insert("method".into(), Value::String(self.method.clone()));
        if let Some(params) = &self.params {
            map.insert("params".into(), params.clone());
        }
        Value::Object(map)
    }
}

/// A JSON-RPC notification (no id, no response).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }

    pub fn params_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, RpcError> {
        let value = self.params.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("jsonrpc".into(), Value::String(self.jsonrpc.clone()));
        map.insert("method".into(), Value::String(self.method.clone()));
        if let Some(params) = &self.params {
            map.insert("params".into(), params.clone());
        }
        Value::Object(map)
    }
}

/// A JSON-RPC response. Exactly one of `result` and `error` is present on a
/// well-formed message. `id` is `None` (serialized as `null`) only for
/// errors raised before the request id could be read, such as parse errors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn success(id: impl Into<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.into()),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<RequestId>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Convert into `Ok(result)` or `Err(error)`. A response with neither
    /// field is reported as an internal error.
    pub fn into_result(self) -> Result<Value, RpcError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        match self.result {
            Some(v) => Ok(v),
            None => Err(RpcError::internal_error(
                "response carries neither result nor error",
            )),
        }
    }

    /// Deserialize the success payload into a typed result.
    pub fn result_as<T: serde::de::DeserializeOwned>(self) -> Result<T, RpcError> {
        let value = self.into_result()?;
        serde_json::from_value(value).map_err(|e| RpcError::internal_error(format!("invalid result: {e}")))
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("jsonrpc".into(), Value::String(self.jsonrpc.clone()));
        map.insert("id".into(), self.id.as_ref().map_or(Value::Null, Value::from));
        if let Some(result) = &self.result {
            map.insert("result".into(), result.clone());
        }
        if let Some(error) = &self.error {
            map.insert("error".into(), error.to_value());
        }
        Value::Object(map)
    }
}

/// Any single JSON-RPC message.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

/// A parsed JSON-RPC payload: either a single message or a batch array.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Single(Message),
    Batch(Vec<Message>),
}

/// Why a payload could not be understood as JSON-RPC.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid JSON-RPC message: {0}")]
    Invalid(String),
    #[error("empty batch")]
    EmptyBatch,
}

impl ParseError {
    /// Map to the JSON-RPC error a server should return for this failure.
    pub fn to_rpc_error(&self) -> RpcError {
        match self {
            ParseError::Json(e) => RpcError::parse_error(e.to_string()),
            ParseError::Invalid(m) => RpcError::invalid_request(m.clone()),
            ParseError::EmptyBatch => RpcError::invalid_request("empty batch"),
        }
    }
}

impl Message {
    /// Parse a single JSON-RPC message from text. Batches are rejected here;
    /// use [`Message::parse_incoming`] to accept both.
    pub fn parse(text: &str) -> Result<Message, ParseError> {
        let value: Value = serde_json::from_str(text)?;
        Message::from_value(value)
    }

    /// Parse either one message or a batch.
    pub fn parse_incoming(text: &str) -> Result<Incoming, ParseError> {
        let value: Value = serde_json::from_str(text)?;
        match value {
            Value::Array(items) => {
                if items.is_empty() {
                    return Err(ParseError::EmptyBatch);
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(Message::from_value(item)?);
                }
                Ok(Incoming::Batch(out))
            }
            other => Ok(Incoming::Single(Message::from_value(other)?)),
        }
    }

    /// Classify a JSON object as request, notification or response.
    pub fn from_value(value: Value) -> Result<Message, ParseError> {
        let obj = match &value {
            Value::Object(o) => o,
            _ => return Err(ParseError::Invalid("message must be a JSON object".into())),
        };
        if let Some(v) = obj.get("jsonrpc") {
            if v.as_str() != Some(JSONRPC_VERSION) {
                return Err(ParseError::Invalid(format!("unsupported jsonrpc version: {v}")));
            }
        }
        let has_id = obj.get("id").map(|v| !v.is_null()).unwrap_or(false);
        if obj.contains_key("method") {
            if !obj.get("method").map(Value::is_string).unwrap_or(false) {
                return Err(ParseError::Invalid("method must be a string".into()));
            }
            if has_id {
                let req: Request = serde_json::from_value(value)
                    .map_err(|e| ParseError::Invalid(format!("bad request: {e}")))?;
                Ok(Message::Request(req))
            } else {
                let n: Notification = serde_json::from_value(value)
                    .map_err(|e| ParseError::Invalid(format!("bad notification: {e}")))?;
                Ok(Message::Notification(n))
            }
        } else if obj.contains_key("result") || obj.contains_key("error") {
            let r: Response = serde_json::from_value(value)
                .map_err(|e| ParseError::Invalid(format!("bad response: {e}")))?;
            if r.result.is_some() && r.error.is_some() {
                return Err(ParseError::Invalid("response has both result and error".into()));
            }
            Ok(Message::Response(r))
        } else {
            Err(ParseError::Invalid(
                "object is neither a request, notification nor response".into(),
            ))
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Message::Request(r) => r.to_value(),
            Message::Notification(n) => n.to_value(),
            Message::Response(r) => r.to_value(),
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.to_value())
    }

    /// The method name for requests and notifications.
    pub fn method(&self) -> Option<&str> {
        match self {
            Message::Request(r) => Some(&r.method),
            Message::Notification(n) => Some(&n.method),
            Message::Response(_) => None,
        }
    }

    /// The id for requests and responses that carry one.
    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Message::Request(r) => Some(&r.id),
            Message::Notification(_) => None,
            Message::Response(r) => r.id.as_ref(),
        }
    }

    pub fn is_request(&self) -> bool {
        matches!(self, Message::Request(_))
    }

    pub fn is_notification(&self) -> bool {
        matches!(self, Message::Notification(_))
    }

    pub fn is_response(&self) -> bool {
        matches!(self, Message::Response(_))
    }
}

impl From<Request> for Message {
    fn from(r: Request) -> Self {
        Message::Request(r)
    }
}

impl From<Notification> for Message {
    fn from(n: Notification) -> Self {
        Message::Notification(n)
    }
}

impl From<Response> for Message {
    fn from(r: Response) -> Self {
        Message::Response(r)
    }
}

/// Best-effort extraction of the response id from raw JSON without a full
/// parse. Returns `None` for requests, notifications and malformed ids.
pub fn try_get_response_id(value: &Value) -> Option<RequestId> {
    let obj = value.as_object()?;
    if obj.contains_key("method") {
        return None;
    }
    if !(obj.contains_key("result") || obj.contains_key("error")) {
        return None;
    }
    obj.get("id").and_then(RequestId::from_value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};
    use serde_json::json;

    #[test]
    fn request_roundtrip() -> TestResult {
        let r = Request::new(7, "tools/list", Some(json!({"cursor": "abc"})));
        let text = r.to_json()?;
        assert!(text.contains("\"jsonrpc\":\"2.0\""));
        let back = Message::parse(&text)?;
        assert_eq!(back, Message::Request(r));
        Ok(())
    }

    #[test]
    fn string_and_numeric_ids() -> TestResult {
        let a = Message::parse(r#"{"jsonrpc":"2.0","id":"x-1","method":"ping"}"#)?;
        assert_eq!(a.id(), Some(&RequestId::from("x-1")));
        let b = Message::parse(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#)?;
        assert_eq!(b.id(), Some(&RequestId::from(42)));
        Ok(())
    }

    #[test]
    fn notification_has_no_id() -> TestResult {
        let m = Message::parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
        assert!(m.is_notification());
        let m = Message::parse(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#)?;
        assert!(m.is_notification());
        Ok(())
    }

    #[test]
    fn response_success_and_error() -> TestResult {
        let ok = Message::parse(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#)?;
        match ok {
            Message::Response(r) => assert_eq!(r.into_result()?, json!({"tools": []})),
            _ => return Err(ap_support::testing::TestFailure::new("unexpected value").into()),
        }
        let err = Message::parse(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#)?;
        match err {
            Message::Response(r) => {
                let e = r.into_result().err_or_fail()?;
                assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
                assert!(e.is_standard_code());
            }
            _ => return Err(ap_support::testing::TestFailure::new("unexpected value").into()),
        }
        Ok(())
    }

    #[test]
    fn error_response_with_null_id_serializes_null() -> TestResult {
        let r = Response::error(None, RpcError::parse_error("bad"));
        let text = r.to_json()?;
        assert!(text.contains("\"id\":null"));
        let back = Message::parse(&text)?;
        assert!(back.is_response());
        Ok(())
    }

    #[test]
    fn rejects_garbage() -> TestResult {
        assert!(matches!(Message::parse("not json"), Err(ParseError::Json(_))));
        assert!(matches!(Message::parse("[]"), Err(ParseError::Invalid(_))));
        assert!(matches!(
            Message::parse(r#"{"jsonrpc":"1.0","method":"x"}"#),
            Err(ParseError::Invalid(_))
        ));
        assert!(matches!(
            Message::parse(r#"{"jsonrpc":"2.0"}"#),
            Err(ParseError::Invalid(_))
        ));
        assert!(matches!(
            Message::parse(r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":"m"}}"#),
            Err(ParseError::Invalid(_))
        ));
        Ok(())
    }

    #[test]
    fn batch_parsing() -> TestResult {
        let inc = Message::parse_incoming(
            r#"[{"jsonrpc":"2.0","id":1,"method":"a"},{"jsonrpc":"2.0","method":"b"}]"#,
        )?;
        match inc {
            Incoming::Batch(v) => {
                assert_eq!(v.len(), 2);
                assert!(v[0].is_request());
                assert!(v[1].is_notification());
            }
            _ => return Err(ap_support::testing::TestFailure::new("unexpected value").into()),
        }
        assert!(matches!(
            Message::parse_incoming("[]"),
            Err(ParseError::EmptyBatch)
        ));
        Ok(())
    }

    #[test]
    fn typed_params() -> TestResult {
        #[derive(Deserialize, Debug, PartialEq)]
        struct P {
            name: String,
        }
        let r = Request::new(1, "tools/call", Some(json!({"name": "echo"})));
        assert_eq!(r.params_as::<P>()?, P { name: "echo".into() });
        let bad = Request::new(1, "tools/call", Some(json!({"nam": "echo"})));
        assert_eq!(
            bad.params_as::<P>().err_or_fail()?.code,
            error_codes::INVALID_PARAMS
        );
        Ok(())
    }

    #[test]
    fn response_id_extraction() -> TestResult {
        let v = json!({"jsonrpc":"2.0","id":"abc","result":{}});
        assert_eq!(try_get_response_id(&v), Some(RequestId::from("abc")));
        let v = json!({"jsonrpc":"2.0","id":1,"method":"x"});
        assert_eq!(try_get_response_id(&v), None);
        Ok(())
    }
}
