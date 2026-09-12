// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Error type of the SDK.
//!
//! The C++ SDK reports failures with three mechanisms: `Mcp::MCPError` for
//! JSON-RPC error objects received from the peer, `std::runtime_error` for
//! state and transport problems and `std::invalid_argument` for bad local
//! arguments. All three map onto [`McpError`].

use ap_jsonrpc::RpcError;
use serde_json::Value;

/// Standard JSON-RPC 2.0 error codes, see `Mcp::JsonRpcErrorCode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JsonRpcErrorCode {
    /// Invalid JSON was received.
    ParseError,
    /// The JSON sent is not a valid request object.
    InvalidRequest,
    /// The method does not exist.
    MethodNotFound,
    /// Invalid method parameters.
    InvalidParams,
    /// Internal JSON-RPC error.
    InternalError,
    /// Start of the implementation defined server error range.
    ServerError,
}

impl JsonRpcErrorCode {
    /// The numeric code on the wire.
    pub const fn code(self) -> i64 {
        match self {
            JsonRpcErrorCode::ParseError => -32700,
            JsonRpcErrorCode::InvalidRequest => -32600,
            JsonRpcErrorCode::MethodNotFound => -32601,
            JsonRpcErrorCode::InvalidParams => -32602,
            JsonRpcErrorCode::InternalError => -32603,
            JsonRpcErrorCode::ServerError => -32000,
        }
    }

    /// Map a numeric code to a known enum value, like `MCPError::codeEnum`.
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            -32700 => Some(JsonRpcErrorCode::ParseError),
            -32600 => Some(JsonRpcErrorCode::InvalidRequest),
            -32601 => Some(JsonRpcErrorCode::MethodNotFound),
            -32602 => Some(JsonRpcErrorCode::InvalidParams),
            -32603 => Some(JsonRpcErrorCode::InternalError),
            -32000 => Some(JsonRpcErrorCode::ServerError),
            _ => None,
        }
    }
}

/// Result-shaped JSON-RPC error, the counterpart of `Mcp::ErrorResult`.
pub type ErrorResult = RpcError;

/// Every failure the SDK can report.
#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    /// A JSON-RPC error object arrived from the peer (`Mcp::MCPError`).
    #[error("{}", .0.message)]
    Rpc(RpcError),
    /// The transport could not be created, connected or used.
    #[error("transport error: {0}")]
    Transport(String),
    /// No response arrived within the request timeout.
    #[error("request timed out after {0} ms")]
    Timeout(u64),
    /// A client method was called before `initialize`.
    #[error("client is not initialized.")]
    NotInitialized,
    /// `initialize` was called twice.
    #[error("client is initialized.")]
    AlreadyInitialized,
    /// The session or transport was closed while a request was pending.
    #[error("connection closed")]
    Closed,
    /// An operation is not allowed in the current state (`std::runtime_error`).
    #[error("{0}")]
    InvalidState(String),
    /// A local argument was rejected (`std::invalid_argument`).
    #[error("{0}")]
    InvalidArgument(String),
    /// Schema validation of tool input or output failed.
    #[error("{0}")]
    Validation(String),
    /// JSON could not be produced or parsed.
    #[error("json error: {0}")]
    Json(String),
}

impl McpError {
    /// Build a JSON-RPC error with an explicit code.
    pub fn rpc(code: i64, message: impl Into<String>) -> Self {
        McpError::Rpc(RpcError::new(code, message))
    }

    /// Build a JSON-RPC error from a known code.
    pub fn rpc_code(code: JsonRpcErrorCode, message: impl Into<String>) -> Self {
        McpError::Rpc(RpcError::new(code.code(), message))
    }

    /// Convenience constructor for state errors.
    pub fn state(message: impl Into<String>) -> Self {
        McpError::InvalidState(message.into())
    }

    /// Convenience constructor for argument errors.
    pub fn argument(message: impl Into<String>) -> Self {
        McpError::InvalidArgument(message.into())
    }

    /// The JSON-RPC error code when this is an [`McpError::Rpc`].
    pub fn code(&self) -> Option<i64> {
        match self {
            McpError::Rpc(e) => Some(e.code),
            _ => None,
        }
    }

    /// The known enum value of the JSON-RPC error code, if any.
    pub fn code_enum(&self) -> Option<JsonRpcErrorCode> {
        self.code().and_then(JsonRpcErrorCode::from_code)
    }

    /// The error message. For JSON-RPC errors this is the `message` field.
    pub fn message(&self) -> String {
        match self {
            McpError::Rpc(e) => e.message.clone(),
            other => other.to_string(),
        }
    }

    /// The optional `data` field of a JSON-RPC error.
    pub fn data(&self) -> Option<&Value> {
        match self {
            McpError::Rpc(e) => e.data.as_ref(),
            _ => None,
        }
    }

    /// True when the error is a JSON-RPC error received from the peer.
    pub fn is_rpc(&self) -> bool {
        matches!(self, McpError::Rpc(_))
    }

    /// Convert the error into the JSON-RPC error object a peer should receive.
    pub fn to_rpc_error(&self) -> RpcError {
        match self {
            McpError::Rpc(e) => e.clone(),
            McpError::InvalidArgument(m) | McpError::Validation(m) => RpcError::invalid_params(m.clone()),
            other => RpcError::internal_error(other.to_string()),
        }
    }
}

impl From<RpcError> for McpError {
    fn from(e: RpcError) -> Self {
        McpError::Rpc(e)
    }
}

impl From<serde_json::Error> for McpError {
    fn from(e: serde_json::Error) -> Self {
        McpError::Json(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn code_enum_maps_known_codes() -> TestResult {
        let err = McpError::rpc(-32601, "nope");
        assert_eq!(err.code(), Some(-32601));
        assert_eq!(err.code_enum(), Some(JsonRpcErrorCode::MethodNotFound));
        assert_eq!(err.message(), "nope");
        assert!(err.data().is_none());

        let custom = McpError::rpc(-32001, "custom");
        assert_eq!(custom.code_enum(), None);
        assert_eq!(JsonRpcErrorCode::ServerError.code(), -32000);
        assert_eq!(
            JsonRpcErrorCode::from_code(-32700),
            Some(JsonRpcErrorCode::ParseError)
        );
        Ok(())
    }

    #[test]
    fn non_rpc_errors_have_no_code() -> TestResult {
        let err = McpError::NotInitialized;
        assert_eq!(err.code(), None);
        assert_eq!(err.to_string(), "client is not initialized.");
        assert_eq!(McpError::AlreadyInitialized.to_string(), "client is initialized.");
        assert_eq!(McpError::argument("bad").to_rpc_error().code, -32602);
        Ok(())
    }
}
