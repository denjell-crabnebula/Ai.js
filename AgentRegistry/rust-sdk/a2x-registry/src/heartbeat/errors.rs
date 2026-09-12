// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Heartbeat domain errors. The backend renders them as HTTP 400 with a
//! structured body `{code, detail, min_ttl?, max_ttl?}`.

use serde_json::{Map, Value};

/// Machine readable rejection code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatErrorCode {
    /// Client sent `lease_ttl` on a namespace with heartbeat disabled.
    NotSupported,
    /// Namespace requires heartbeats but the client sent no `lease_ttl`.
    TtlRequired,
    /// `lease_ttl` violates the namespace bounds.
    TtlOutOfRange,
}

impl HeartbeatErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            HeartbeatErrorCode::NotSupported => "heartbeat_not_supported",
            HeartbeatErrorCode::TtlRequired => "ttl_required",
            HeartbeatErrorCode::TtlOutOfRange => "ttl_out_of_range",
        }
    }
}

/// A heartbeat rejection with optional bounds for the client.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HeartbeatError {
    pub code: HeartbeatErrorCode,
    pub message: String,
    pub min_ttl: Option<i64>,
    pub max_ttl: Option<i64>,
}

impl HeartbeatError {
    pub fn not_supported(message: impl Into<String>) -> Self {
        Self {
            code: HeartbeatErrorCode::NotSupported,
            message: message.into(),
            min_ttl: None,
            max_ttl: None,
        }
    }

    /// Structured body: `{code, detail, min_ttl?, max_ttl?}`.
    pub fn body(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("code".into(), Value::String(self.code.as_str().into()));
        m.insert("detail".into(), Value::String(self.message.clone()));
        if let Some(v) = self.min_ttl {
            m.insert("min_ttl".into(), Value::from(v));
        }
        if let Some(v) = self.max_ttl {
            m.insert("max_ttl".into(), Value::from(v));
        }
        m
    }
}
