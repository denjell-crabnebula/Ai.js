// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Error type for the A2X registry client.
//!
//! The Python SDK uses an exception hierarchy rooted at `A2XError`. This crate
//! folds that hierarchy into one [`ClientError`] enum. Each Python class maps to
//! one variant, and the predicate methods (`is_http`, `is_validation`, ...)
//! replace `isinstance` checks against the intermediate base classes.
//!
//! Status code mapping (identical to `transport._wrap_http_error`):
//!
//! | Status | Variant |
//! |--------|---------|
//! | 401 | [`ClientError::Authentication`] |
//! | 403 | [`ClientError::Authorization`] |
//! | 404 | [`ClientError::NotFound`] |
//! | 400 / 422 with `detail.code = heartbeat_not_supported` | [`ClientError::HeartbeatNotSupported`] |
//! | 400 / 422 with `detail.code = ttl_required` | [`ClientError::TtlRequired`] |
//! | 400 / 422 with `detail.code = ttl_out_of_range` | [`ClientError::TtlOutOfRange`] |
//! | 400 / 422 mentioning `user_config` | [`ClientError::UserConfigServiceImmutable`] |
//! | 400 / 422 otherwise | [`ClientError::Validation`] |
//! | 5xx | [`ClientError::Server`] |
//! | any other 4xx | [`ClientError::Http`] |

use serde_json::Value;

/// Every error the SDK can return. Mirrors the Python `A2XError` hierarchy.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Network failure (DNS, connect, reset). Python: `A2XConnectionError`.
    #[error("{message}")]
    Connection {
        /// Human readable description of the transport failure.
        message: String,
    },

    /// The request exceeded the configured timeout. Python folds this into
    /// `A2XConnectionError`; it is split out here so callers can tell it apart.
    #[error("{message}")]
    Timeout {
        /// Human readable description of the timeout.
        message: String,
    },

    /// Any 4xx status that has no dedicated variant. Python: `A2XHTTPError`.
    #[error("{message}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 401: missing, invalid, revoked key or disabled principal.
    #[error("{message}")]
    Authentication {
        /// HTTP status code, always 401.
        status: u16,
        /// `HTTP 401: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 403: authenticated but the principal lacks permission.
    #[error("{message}")]
    Authorization {
        /// HTTP status code, always 403.
        status: u16,
        /// `HTTP 403: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 404: resource not found.
    #[error("{message}")]
    NotFound {
        /// HTTP status code, always 404.
        status: u16,
        /// `HTTP 404: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 400 or 422: request rejected by the backend. Python: `ValidationError`.
    #[error("{message}")]
    Validation {
        /// HTTP status code, 400 or 422.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 400 or 422 whose detail mentions `user_config`. The service comes from
    /// `user_config.json` and cannot be updated or deleted over HTTP.
    #[error("{message}")]
    UserConfigServiceImmutable {
        /// HTTP status code, 400 or 422.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// 400 with `code = heartbeat_not_supported`: the namespace has no lease config.
    #[error("{message}")]
    HeartbeatNotSupported {
        /// HTTP status code.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// The inner `detail` object of the response.
        payload: Option<Box<Value>>,
    },

    /// 400 with `code = ttl_required`: the namespace needs `lease_ttl`.
    #[error("{message}")]
    TtlRequired {
        /// HTTP status code.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// The inner `detail` object of the response.
        payload: Option<Box<Value>>,
        /// Namespace lower bound, when the server sent it.
        min_ttl: Option<i64>,
        /// Namespace upper bound, when the server sent it.
        max_ttl: Option<i64>,
    },

    /// 400 with `code = ttl_out_of_range`: `lease_ttl` is outside `[min_ttl, max_ttl]`.
    #[error("{message}")]
    TtlOutOfRange {
        /// HTTP status code.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// The inner `detail` object of the response.
        payload: Option<Box<Value>>,
        /// Namespace lower bound, when the server sent it.
        min_ttl: Option<i64>,
        /// Namespace upper bound, when the server sent it.
        max_ttl: Option<i64>,
    },

    /// `get_agent` received a non-JSON payload, for example a skill ZIP.
    #[error("{message}")]
    UnexpectedServiceType {
        /// HTTP status code of the response.
        status: u16,
        /// Description of the unexpected content.
        message: String,
    },

    /// 5xx: backend internal error. Python: `ServerError`.
    #[error("{message}")]
    Server {
        /// HTTP status code.
        status: u16,
        /// `HTTP <status>: <detail>` message.
        message: String,
        /// Parsed JSON body, when the body was JSON.
        payload: Option<Box<Value>>,
    },

    /// Local ownership check failed; no HTTP request was sent.
    #[error("service {service_id:?} in dataset {dataset:?} was not registered by this client")]
    NotOwned {
        /// Dataset that was targeted.
        dataset: String,
        /// Service id that is not owned by this client.
        service_id: String,
    },

    /// Local argument validation failed; no HTTP request was sent. Python: `ValueError`.
    #[error("{message}")]
    InvalidArgument {
        /// Why the argument was rejected.
        message: String,
    },

    /// A 2xx body could not be decoded into the expected shape.
    #[error("{message}")]
    Decode {
        /// Decoding failure description.
        message: String,
    },

    /// Local file system failure (credential or ownership files).
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl ClientError {
    /// Build an [`ClientError::InvalidArgument`] from any message.
    pub fn invalid(message: impl Into<String>) -> Self {
        ClientError::InvalidArgument {
            message: message.into(),
        }
    }

    /// Build a [`ClientError::Decode`] from any message.
    pub fn decode(message: impl Into<String>) -> Self {
        ClientError::Decode {
            message: message.into(),
        }
    }

    /// HTTP status code for HTTP-origin errors, `None` otherwise.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            ClientError::Http { status, .. }
            | ClientError::Authentication { status, .. }
            | ClientError::Authorization { status, .. }
            | ClientError::NotFound { status, .. }
            | ClientError::Validation { status, .. }
            | ClientError::UserConfigServiceImmutable { status, .. }
            | ClientError::HeartbeatNotSupported { status, .. }
            | ClientError::TtlRequired { status, .. }
            | ClientError::TtlOutOfRange { status, .. }
            | ClientError::UnexpectedServiceType { status, .. }
            | ClientError::Server { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Parsed response body for HTTP-origin errors, when it was JSON.
    pub fn payload(&self) -> Option<&Value> {
        match self {
            ClientError::Http { payload, .. }
            | ClientError::Authentication { payload, .. }
            | ClientError::Authorization { payload, .. }
            | ClientError::NotFound { payload, .. }
            | ClientError::Validation { payload, .. }
            | ClientError::UserConfigServiceImmutable { payload, .. }
            | ClientError::HeartbeatNotSupported { payload, .. }
            | ClientError::TtlRequired { payload, .. }
            | ClientError::TtlOutOfRange { payload, .. }
            | ClientError::Server { payload, .. } => payload.as_deref(),
            _ => None,
        }
    }

    /// True for the Python `A2XConnectionError` family (network and timeout).
    pub fn is_connection(&self) -> bool {
        matches!(self, ClientError::Connection { .. } | ClientError::Timeout { .. })
    }

    /// True for the Python `A2XHTTPError` family (any 4xx or 5xx origin).
    pub fn is_http(&self) -> bool {
        self.status_code().is_some()
    }

    /// True for the Python `ValidationError` family (400 / 422 and its subclasses).
    pub fn is_validation(&self) -> bool {
        matches!(
            self,
            ClientError::Validation { .. }
                | ClientError::UserConfigServiceImmutable { .. }
                | ClientError::HeartbeatNotSupported { .. }
                | ClientError::TtlRequired { .. }
                | ClientError::TtlOutOfRange { .. }
        )
    }

    /// True when the error is [`ClientError::NotFound`].
    pub fn is_not_found(&self) -> bool {
        matches!(self, ClientError::NotFound { .. })
    }

    /// True when the error is [`ClientError::NotOwned`].
    pub fn is_not_owned(&self) -> bool {
        matches!(self, ClientError::NotOwned { .. })
    }

    /// `min_ttl` carried by the TTL error variants.
    pub fn min_ttl(&self) -> Option<i64> {
        match self {
            ClientError::TtlRequired { min_ttl, .. } | ClientError::TtlOutOfRange { min_ttl, .. } => *min_ttl,
            _ => None,
        }
    }

    /// `max_ttl` carried by the TTL error variants.
    pub fn max_ttl(&self) -> Option<i64> {
        match self {
            ClientError::TtlRequired { max_ttl, .. } | ClientError::TtlOutOfRange { max_ttl, .. } => *max_ttl,
            _ => None,
        }
    }
}

/// Parse a response body the way `transport._parse_payload` does.
///
/// Non-JSON bodies yield `None`; JSON that is not an object is wrapped as
/// `{"detail": <value>}`.
pub(crate) fn parse_payload(body: &[u8]) -> Option<Value> {
    let data: Value = serde_json::from_slice(body).ok()?;
    if data.is_object() {
        Some(data)
    } else {
        Some(serde_json::json!({ "detail": data }))
    }
}

/// Map a non-2xx response to the matching [`ClientError`] variant.
///
/// `reason` is the HTTP reason phrase used when the body carries no detail.
pub fn wrap_http_error(status: u16, reason: Option<&str>, body: &[u8]) -> ClientError {
    let payload = parse_payload(body);
    let raw_detail = payload.as_ref().and_then(|p| p.get("detail")).cloned();
    let mut detail_str = String::new();
    let mut detail_obj: Option<Value> = None;
    match raw_detail {
        Some(Value::String(s)) => detail_str = s,
        Some(obj @ Value::Object(_)) => {
            let inner = obj
                .get("detail")
                .filter(|v| !v.is_null())
                .or_else(|| obj.get("code").filter(|v| !v.is_null()));
            detail_str = match inner {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            detail_obj = Some(obj);
        }
        _ => {}
    }
    let fallback = if detail_str.is_empty() {
        reason.filter(|r| !r.is_empty()).unwrap_or("request failed")
    } else {
        detail_str.as_str()
    };
    let message = format!("HTTP {status}: {fallback}");

    match status {
        401 => ClientError::Authentication {
            status,
            message,
            payload: payload.map(Box::new),
        },
        403 => ClientError::Authorization {
            status,
            message,
            payload: payload.map(Box::new),
        },
        404 => ClientError::NotFound {
            status,
            message,
            payload: payload.map(Box::new),
        },
        400 | 422 => {
            if let Some(obj) = detail_obj {
                let code = obj.get("code").and_then(Value::as_str).unwrap_or("");
                let min_ttl = obj.get("min_ttl").and_then(Value::as_i64);
                let max_ttl = obj.get("max_ttl").and_then(Value::as_i64);
                match code {
                    "heartbeat_not_supported" => {
                        return ClientError::HeartbeatNotSupported {
                            status,
                            message,
                            payload: Some(Box::new(obj)),
                        };
                    }
                    "ttl_required" => {
                        return ClientError::TtlRequired {
                            status,
                            message,
                            payload: Some(Box::new(obj)),
                            min_ttl,
                            max_ttl,
                        };
                    }
                    "ttl_out_of_range" => {
                        return ClientError::TtlOutOfRange {
                            status,
                            message,
                            payload: Some(Box::new(obj)),
                            min_ttl,
                            max_ttl,
                        };
                    }
                    _ => {}
                }
            }
            if detail_str.contains("user_config") {
                ClientError::UserConfigServiceImmutable {
                    status,
                    message,
                    payload: payload.map(Box::new),
                }
            } else {
                ClientError::Validation {
                    status,
                    message,
                    payload: payload.map(Box::new),
                }
            }
        }
        500..=599 => ClientError::Server {
            status,
            message,
            payload: payload.map(Box::new),
        },
        _ => ClientError::Http {
            status,
            message,
            payload: payload.map(Box::new),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn maps_status_codes() -> TestResult {
        assert!(matches!(
            wrap_http_error(401, None, b"{}"),
            ClientError::Authentication { .. }
        ));
        assert!(matches!(
            wrap_http_error(403, None, b"{}"),
            ClientError::Authorization { .. }
        ));
        assert!(matches!(
            wrap_http_error(404, None, b"{}"),
            ClientError::NotFound { .. }
        ));
        assert!(matches!(
            wrap_http_error(400, None, b"{}"),
            ClientError::Validation { .. }
        ));
        assert!(matches!(
            wrap_http_error(422, None, b"{}"),
            ClientError::Validation { .. }
        ));
        assert!(matches!(
            wrap_http_error(409, None, b"{}"),
            ClientError::Http { .. }
        ));
        assert!(matches!(
            wrap_http_error(500, None, b"{}"),
            ClientError::Server { .. }
        ));
        assert!(matches!(
            wrap_http_error(503, None, b"{}"),
            ClientError::Server { .. }
        ));
        Ok(())
    }

    #[test]
    fn message_uses_detail_then_reason_then_default() -> TestResult {
        let e = wrap_http_error(404, Some("Not Found"), br#"{"detail": "gone"}"#);
        assert_eq!(e.to_string(), "HTTP 404: gone");
        let e = wrap_http_error(404, Some("Not Found"), b"not json");
        assert_eq!(e.to_string(), "HTTP 404: Not Found");
        let e = wrap_http_error(404, None, b"");
        assert_eq!(e.to_string(), "HTTP 404: request failed");
        Ok(())
    }

    #[test]
    fn non_object_json_is_wrapped_in_detail() -> TestResult {
        let e = wrap_http_error(400, None, br#"["a", "b"]"#);
        assert_eq!(e.payload().required()?["detail"], serde_json::json!(["a", "b"]));
        Ok(())
    }

    #[test]
    fn user_config_detail_selects_immutable_variant() -> TestResult {
        let e = wrap_http_error(400, None, br#"{"detail": "service comes from user_config"}"#);
        assert!(matches!(e, ClientError::UserConfigServiceImmutable { .. }));
        assert!(e.is_validation());
        Ok(())
    }

    #[test]
    fn structured_detail_message_prefers_inner_detail_then_code() -> TestResult {
        let e = wrap_http_error(
            400,
            None,
            br#"{"detail": {"code": "ttl_required", "min_ttl": 1}}"#,
        );
        assert_eq!(e.to_string(), "HTTP 400: ttl_required");
        assert_eq!(e.min_ttl(), Some(1));
        assert_eq!(e.max_ttl(), None);
        Ok(())
    }

    #[test]
    fn not_owned_message_matches_python() -> TestResult {
        let e = ClientError::NotOwned {
            dataset: "ds".into(),
            service_id: "sid".into(),
        };
        assert_eq!(
            e.to_string(),
            "service \"sid\" in dataset \"ds\" was not registered by this client"
        );
        assert!(e.is_not_owned());
        assert!(!e.is_http());
        Ok(())
    }
}
