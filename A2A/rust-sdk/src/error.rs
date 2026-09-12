// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Error codes and error types.
//!
//! This module ports `include/error.h`, the `A2AErrorCode` enum of
//! `include/types.h` and `src/shared/a2a_error.cpp`.

use serde_json::Value;

use crate::protocol::http::HTTP_PARSE_ERROR;
use crate::types::A2AError;

/// A2A and JSON-RPC error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i64)]
pub enum A2AErrorCode {
    /// Success marker.
    A2aSuccess = 0,

    /// JSON-RPC parse error.
    JsonrpcParseError = -32700,
    /// JSON-RPC invalid request.
    JsonrpcInvalidRequest = -32600,
    /// JSON-RPC method not found.
    JsonrpcMethodNotFound = -32601,
    /// JSON-RPC invalid params.
    JsonrpcInvalidParams = -32602,
    /// JSON-RPC internal error.
    JsonrpcInternalError = -32603,

    /// Task not found.
    TaskNotFound = -32001,
    /// Task cannot be canceled.
    TaskNotCancelable = -32002,
    /// Push notifications are not supported.
    PushNotificationNotSupported = -32003,
    /// Operation is not supported.
    UnsupportedOperation = -32004,
    /// Incompatible content types.
    ContentTypeNotSupported = -32005,
    /// Invalid agent response.
    InvalidAgentResponse = -32006,
    /// Authenticated extended card is not configured.
    AuthenticatedExtendedCardNotConfigured = -32007,
    /// Extension support required.
    ExtensionSupportRequiredError = -32008,
    /// Version not supported.
    VersionNotSupportedError = -32009,

    /// Client request timeout.
    A2aRequestTimeout = -32101,
    /// Client transport exception.
    A2aTransportException = -32102,
    /// Client has no valid transport.
    A2aInvalidTransport = -32103,
    /// Client received an invalid format.
    A2aInvalidFormat = -32106,
    /// Client is unauthorized.
    A2aUnauthorized = -32107,
    /// Client status error (for example, the transport is closed).
    A2aStatusError = -32108,
    /// Client received invalid input.
    A2aInvalidInput = -32109,
    /// Client ran out of memory.
    A2aBadAlloc = -32110,
    /// Client concurrency limit reached.
    A2aConcurrentLimit = -32111,
}

impl A2AErrorCode {
    /// Numeric wire value of the code.
    pub const fn code(self) -> i64 {
        self as i64
    }

    /// Look up a known code by its numeric value.
    pub fn from_code(code: i64) -> Option<A2AErrorCode> {
        use A2AErrorCode::*;
        const ALL: [A2AErrorCode; 24] = [
            A2aSuccess,
            JsonrpcParseError,
            JsonrpcInvalidRequest,
            JsonrpcMethodNotFound,
            JsonrpcInvalidParams,
            JsonrpcInternalError,
            TaskNotFound,
            TaskNotCancelable,
            PushNotificationNotSupported,
            UnsupportedOperation,
            ContentTypeNotSupported,
            InvalidAgentResponse,
            AuthenticatedExtendedCardNotConfigured,
            ExtensionSupportRequiredError,
            VersionNotSupportedError,
            A2aRequestTimeout,
            A2aTransportException,
            A2aInvalidTransport,
            A2aInvalidFormat,
            A2aUnauthorized,
            A2aStatusError,
            A2aInvalidInput,
            A2aBadAlloc,
            A2aConcurrentLimit,
        ];
        ALL.iter().copied().find(|c| c.code() == code)
    }
}

impl From<A2AErrorCode> for i64 {
    fn from(code: A2AErrorCode) -> i64 {
        code.code()
    }
}

fn is_http_status_code(code: i64) -> bool {
    (100..=599).contains(&code)
}

fn is_json_related_error_code(code: i64) -> bool {
    code == A2AErrorCode::JsonrpcParseError.code()
        || code == A2AErrorCode::A2aInvalidFormat.code()
        || code == HTTP_PARSE_ERROR
}

/// Client-side error, the port of `A2AClientException` and its subclasses.
///
/// The variant is chosen from the code exactly like `A2AClientException::Make`.
/// `Display` renders the JSON form `{"code":..,"message":..}` like `what()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A2aClientError {
    /// Non-2xx HTTP response (`A2AClientHTTPError`).
    Http {
        /// HTTP status code.
        status_code: i64,
        /// Error description.
        message: String,
    },
    /// JSON-RPC or A2A protocol error in a response body (`A2AClientJSONError`).
    Json {
        /// Wire error code.
        code: i64,
        /// Error description.
        message: String,
    },
    /// Request timeout (`A2AClientTimeoutError`).
    Timeout {
        /// Error description.
        message: String,
    },
    /// Any other client error (`A2AClientException`).
    Other {
        /// Error code.
        code: i64,
        /// Error description.
        message: String,
    },
}

impl A2aClientError {
    /// Build the error variant matching `A2AClientException::Make`.
    pub fn make(code: i64, message: impl Into<String>) -> Self {
        let message = message.into();
        if is_http_status_code(code) {
            return A2aClientError::Http {
                status_code: code,
                message,
            };
        }
        if is_json_related_error_code(code) {
            return A2aClientError::Json { code, message };
        }
        if code == A2AErrorCode::A2aRequestTimeout.code() {
            return A2aClientError::Timeout { message };
        }
        A2aClientError::Other { code, message }
    }

    /// Build an error from a known code.
    pub fn from_code(code: A2AErrorCode, message: impl Into<String>) -> Self {
        Self::make(code.code(), message)
    }

    /// Error code (A2AErrorCode value or HTTP status).
    pub fn code(&self) -> i64 {
        match self {
            A2aClientError::Http { status_code, .. } => *status_code,
            A2aClientError::Json { code, .. } => *code,
            A2aClientError::Timeout { .. } => A2AErrorCode::A2aRequestTimeout.code(),
            A2aClientError::Other { code, .. } => *code,
        }
    }

    /// Human readable message.
    pub fn message(&self) -> &str {
        match self {
            A2aClientError::Http { message, .. }
            | A2aClientError::Json { message, .. }
            | A2aClientError::Timeout { message }
            | A2aClientError::Other { message, .. } => message,
        }
    }

    /// HTTP status code when the error is an HTTP error.
    pub fn status_code(&self) -> Option<i64> {
        match self {
            A2aClientError::Http { status_code, .. } => Some(*status_code),
            _ => None,
        }
    }

    /// Convert to the wire error object.
    pub fn to_a2a_error(&self) -> A2AError {
        A2AError {
            code: self.code(),
            data: None,
            message: Some(self.message().to_string()),
        }
    }

    /// Parse an `A2AError` from the text of an error, like `TryParse`.
    ///
    /// Accepts the JSON form produced by `Display` of this type.
    pub fn try_parse(text: &str) -> Option<A2AError> {
        let value: Value = serde_json::from_str(text).ok()?;
        serde_json::from_value(value).ok()
    }
}

impl std::fmt::Display for A2aClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let json = serde_json::to_string(&self.to_a2a_error()).unwrap_or_default();
        f.write_str(&json)
    }
}

impl std::error::Error for A2aClientError {}

/// Server-side error mapped to a JSON-RPC error response.
///
/// This is the port of `A2AServerError` and `MethodNotImplementedError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A2aServerError {
    /// Error carrying an explicit JSON-RPC or A2A code.
    Server {
        /// JSON-RPC or A2A error code returned to the client.
        code: i64,
        /// Error description.
        message: String,
    },
    /// The server does not implement the requested method (-32601).
    MethodNotImplemented {
        /// Detail appended to the error text.
        message: String,
    },
}

impl A2aServerError {
    /// Internal error (-32603) with a message, like `A2AServerError(message)`.
    pub fn new(message: impl Into<String>) -> Self {
        A2aServerError::Server {
            code: A2AErrorCode::JsonrpcInternalError.code(),
            message: message.into(),
        }
    }

    /// Error with an explicit numeric code, like `A2AServerError(message, code)`.
    pub fn with_code(message: impl Into<String>, code: i64) -> Self {
        A2aServerError::Server {
            code,
            message: message.into(),
        }
    }

    /// Error with a known code.
    pub fn from_code(message: impl Into<String>, code: A2AErrorCode) -> Self {
        Self::with_code(message, code.code())
    }

    /// `MethodNotImplementedError` with the default detail.
    pub fn method_not_implemented() -> Self {
        A2aServerError::MethodNotImplemented {
            message: "This method is not implemented by the server".to_string(),
        }
    }

    /// `MethodNotImplementedError` with a custom detail.
    pub fn method_not_implemented_with(message: impl Into<String>) -> Self {
        A2aServerError::MethodNotImplemented {
            message: message.into(),
        }
    }

    /// JSON-RPC or A2A error code returned to the client.
    pub fn status_code(&self) -> i64 {
        match self {
            A2aServerError::Server { code, .. } => *code,
            A2aServerError::MethodNotImplemented { .. } => A2AErrorCode::JsonrpcMethodNotFound.code(),
        }
    }

    /// Full error text, like `what()`.
    pub fn message(&self) -> String {
        match self {
            A2aServerError::Server { message, .. } => message.clone(),
            A2aServerError::MethodNotImplemented { message } => {
                format!("Not Implemented operation Error: {message}")
            }
        }
    }

    /// Convert to the wire error object.
    pub fn to_a2a_error(&self) -> A2AError {
        A2AError {
            code: self.status_code(),
            data: None,
            message: Some(self.message()),
        }
    }
}

impl std::fmt::Display for A2aServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for A2aServerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn server_error_default_status_code() -> TestResult {
        let e = A2aServerError::new("boom");
        assert_eq!(e.status_code(), -32603);
        assert_eq!(e.message(), "boom");
        Ok(())
    }

    #[test]
    fn server_error_custom_status_code() -> TestResult {
        let e = A2aServerError::with_code("nope", -32001);
        assert_eq!(e.status_code(), -32001);
        Ok(())
    }

    #[test]
    fn method_not_implemented_uses_method_not_found_code() -> TestResult {
        let e = A2aServerError::method_not_implemented();
        assert_eq!(e.status_code(), -32601);
        assert_eq!(
            e.message(),
            "Not Implemented operation Error: This method is not implemented by the server"
        );
        Ok(())
    }

    #[test]
    fn client_http_error_stores_status_code() -> TestResult {
        let e = A2aClientError::make(404, "not found");
        assert_eq!(e.status_code(), Some(404));
        assert_eq!(e.code(), 404);
        assert!(matches!(e, A2aClientError::Http { .. }));
        Ok(())
    }

    #[test]
    fn client_json_error_serializes_code() -> TestResult {
        let e = A2aClientError::make(-32700, "bad json");
        assert!(matches!(e, A2aClientError::Json { .. }));
        assert_eq!(e.to_string(), "{\"code\":-32700,\"message\":\"bad json\"}");
        Ok(())
    }

    #[test]
    fn client_timeout_error_uses_request_timeout_code() -> TestResult {
        let e = A2aClientError::make(-32101, "slow");
        assert!(matches!(e, A2aClientError::Timeout { .. }));
        assert_eq!(e.code(), -32101);
        Ok(())
    }

    #[test]
    fn make_and_try_parse() -> TestResult {
        let e = A2aClientError::make(-32001, "Task not found");
        assert!(matches!(e, A2aClientError::Other { .. }));
        let parsed = A2aClientError::try_parse(&e.to_string()).required()?;
        assert_eq!(parsed.code, -32001);
        assert_eq!(parsed.message.as_deref(), Some("Task not found"));
        assert!(A2aClientError::try_parse("not json").is_none());
        Ok(())
    }

    #[test]
    fn error_code_round_trip() -> TestResult {
        assert_eq!(A2AErrorCode::TaskNotFound.code(), -32001);
        assert_eq!(
            A2AErrorCode::from_code(-32009),
            Some(A2AErrorCode::VersionNotSupportedError)
        );
        assert_eq!(A2AErrorCode::from_code(42), None);
        Ok(())
    }
}
