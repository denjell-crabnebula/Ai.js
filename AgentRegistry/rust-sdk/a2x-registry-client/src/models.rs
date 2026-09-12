// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Response types returned by the client.
//!
//! Each struct wraps one backend response shape. Unknown fields are ignored
//! for forward compatibility. [`AgentDetail::raw`] keeps the complete response
//! so callers can read fields the SDK has not declared yet.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::errors::ClientError;

/// A JSON object, used for agent cards and flattened list entries.
pub type JsonObject = Map<String, Value>;

/// Response from `POST /api/datasets`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetCreateResponse {
    /// Dataset name.
    pub dataset: String,
    /// Embedding model configured for the dataset.
    pub embedding_model: String,
    /// Accepted registration formats, `{type: min_version}`.
    pub formats: JsonObject,
    /// Always `"created"`.
    pub status: String,
}

/// Response from `DELETE /api/datasets/{dataset}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetDeleteResponse {
    /// Dataset name.
    pub dataset: String,
    /// Always `"deleted"`.
    pub status: String,
}

/// Response from admin `POST /api/auth/principals`.
///
/// `token` is the plaintext API key and is the only moment it appears on the
/// wire. Deliver it out of band; the server only keeps a sha256 hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrincipalCreateResponse {
    /// Stable principal id (`u_<hex>`).
    pub principal_id: String,
    /// Human readable handle.
    pub handle: String,
    /// `admin`, `provider` or `user`.
    pub role: String,
    /// Scoped namespaces; `None` for admin (global scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespaces: Option<Vec<String>>,
    /// Id of the first API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// First 12 characters of the plaintext token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
    /// Plaintext API key, shown once.
    pub token: String,
}

/// Response from `POST /api/datasets/{dataset}/services/a2a`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterResponse {
    /// Service id, server generated when the request omitted it.
    pub service_id: String,
    /// Dataset name.
    pub dataset: String,
    /// `"registered"` or `"updated"`.
    pub status: String,
    /// Granted lease TTL in seconds; `None` for permanent services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl: Option<i64>,
    /// Unix wall-clock expiry of the lease; `None` for permanent services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<f64>,
}

/// Response from `PUT /api/datasets/{dataset}/services/{service_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchResponse {
    /// Service id.
    pub service_id: String,
    /// Dataset name.
    pub dataset: String,
    /// Always `"updated"`.
    pub status: String,
    /// Top-level keys whose value actually changed.
    #[serde(default)]
    pub changed_fields: Vec<String>,
    /// True when `name` or `description` changed.
    #[serde(default)]
    pub taxonomy_affected: bool,
}

/// Successful deregister. `status` is always `"deregistered"`.
///
/// A missing service surfaces as [`ClientError::NotFound`], never as a 200
/// with `status = "not_found"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeregisterResponse {
    /// Service id.
    pub service_id: String,
    /// Always `"deregistered"`.
    pub status: String,
}

/// Full single-agent response. `metadata` is the complete agent card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDetail {
    /// Service id.
    pub id: String,
    /// Service type (`a2a`, `generic`, `skill`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Display name from the wrapper.
    pub name: String,
    /// Wrapper description (already transformed by the backend).
    pub description: String,
    /// The complete agent card as registered.
    pub metadata: JsonObject,
    /// The untouched response object.
    pub raw: JsonObject,
}

impl AgentDetail {
    /// Build from a response object, defaulting missing fields like Python's `from_dict`.
    pub fn from_object(data: JsonObject) -> Self {
        let text = |key: &str| data.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let metadata = match data.get("metadata") {
            Some(Value::Object(m)) => m.clone(),
            _ => JsonObject::new(),
        };
        AgentDetail {
            id: text("id"),
            r#type: text("type"),
            name: text("name"),
            description: text("description"),
            metadata,
            raw: data,
        }
    }
}

/// A successful reservation: the leader's handle to a leased agent set.
///
/// Python's `Reservation` is a context manager that releases the lease on
/// exit. Async Rust has no async drop, so call
/// [`crate::A2xRegistryClient::release_reservation`] explicitly. The blocking
/// module offers [`crate::blocking::ReservationGuard`] for scope-based release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    /// Holder id (server generated unless supplied).
    pub holder_id: String,
    /// Dataset the leases live in.
    pub dataset: String,
    /// Lease duration in seconds.
    pub ttl_seconds: i64,
    /// Unix wall-clock expiry of the leases.
    pub expires_at_unix: f64,
    /// Flat entries, same shape as `GET /services?fields=detail` elements.
    pub agents: Vec<JsonObject>,
    /// Set once the client released the leases (later releases are no-ops).
    #[serde(default)]
    pub released: bool,
}

impl Reservation {
    /// Build from a `POST /reservations` response body.
    pub fn from_value(data: &Value, dataset: &str) -> Result<Self, ClientError> {
        let holder_id = data
            .get("holder_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::decode("reservation response is missing holder_id"))?
            .to_string();
        let ttl_seconds = data.get("ttl_seconds").and_then(Value::as_i64).unwrap_or(30);
        let expires_at_unix = data.get("expires_at_unix").and_then(Value::as_f64).unwrap_or(0.0);
        let agents = match data.get("reservations") {
            Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_object().cloned()).collect(),
            _ => Vec::new(),
        };
        Ok(Reservation {
            holder_id,
            dataset: dataset.to_string(),
            ttl_seconds,
            expires_at_unix,
            agents,
            released: false,
        })
    }

    /// True when no leases are held any more.
    pub fn is_released(&self) -> bool {
        self.released
    }
}

/// Result of [`crate::A2xRegistryClient::shutdown`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShutdownReport {
    /// `(dataset, service_id)` pairs that were revoked or deregistered.
    pub revoked: Vec<(String, String)>,
    /// `(dataset, service_id, error)` triples for the failed targets.
    pub errors: Vec<(String, String, String)>,
}

/// Decode a JSON value into a response struct, mapping failures to [`ClientError::Decode`].
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ClientError> {
    serde_json::from_value(value).map_err(|e| ClientError::decode(format!("invalid response body: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use serde_json::json;

    #[test]
    fn register_response_ignores_unknown_and_defaults_lease() -> TestResult {
        let r: RegisterResponse =
            decode(json!({"service_id": "s", "dataset": "d", "status": "registered", "extra": 1}))?;
        assert_eq!(r.lease_ttl, None);
        assert_eq!(r.lease_expires_at, None);
        Ok(())
    }

    #[test]
    fn patch_response_defaults() -> TestResult {
        let r: PatchResponse = decode(json!({"service_id": "s", "dataset": "d", "status": "updated"}))?;
        assert!(r.changed_fields.is_empty());
        assert!(!r.taxonomy_affected);
        Ok(())
    }

    #[test]
    fn agent_detail_defaults_missing_fields() -> TestResult {
        let d = AgentDetail::from_object(json!({"id": "x"}).as_object().required()?.clone());
        assert_eq!(d.id, "x");
        assert_eq!(d.name, "");
        assert!(d.metadata.is_empty());
        assert_eq!(d.raw["id"], "x");
        Ok(())
    }

    #[test]
    fn reservation_from_value_defaults() -> TestResult {
        let r = Reservation::from_value(&json!({"holder_id": "h"}), "ds")?;
        assert_eq!(r.ttl_seconds, 30);
        assert_eq!(r.expires_at_unix, 0.0);
        assert!(r.agents.is_empty());
        assert!(!r.is_released());
        assert!(Reservation::from_value(&json!({}), "ds").is_err());
        Ok(())
    }
}
