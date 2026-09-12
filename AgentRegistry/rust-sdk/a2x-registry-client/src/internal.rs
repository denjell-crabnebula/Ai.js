// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared pure helpers: constants, URL paths, body builders and response
//! post-processing. Port of the Python `_internal.py` module.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::errors::ClientError;
use crate::models::{AgentDetail, JsonObject};
use crate::transport::Response;

/// SDK default `formats` for Agent Team use cases.
pub fn default_formats() -> JsonObject {
    let mut m = Map::new();
    m.insert("a2a".to_string(), Value::String("v0.0".to_string()));
    m
}

/// Default embedding model used by `create_dataset`. Kept in sync with the backend by hand.
pub const DEFAULT_EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

/// Service-state field on the agent card (`online` / `busy` / `offline`).
pub const STATUS_FIELD: &str = "status";
/// Status value: accepting traffic.
pub const STATUS_ONLINE: &str = "online";
/// Status value: registered but not accepting new work.
pub const STATUS_BUSY: &str = "busy";
/// Status value: drained.
pub const STATUS_OFFLINE: &str = "offline";
const VALID_STATUSES: [&str; 3] = [STATUS_BUSY, STATUS_OFFLINE, STATUS_ONLINE];

/// Default lease lifetime for `reserve_blank_agents`, in seconds.
pub const DEFAULT_RESERVATION_TTL: i64 = 30;

/// Name prefix used when constructing a blank card.
pub const BLANK_AGENT_NAME_PREFIX: &str = "_BlankAgent_";

/// Description sentinel identifying idle-pool agents. Changing it breaks interop.
pub const BLANK_DESCRIPTION_SENTINEL: &str = "__BLANK__";

/// Custom agent card field holding the agent's endpoint URL.
pub const ENDPOINT_FIELD: &str = "endpoint";

const CONTENT_TYPE_JSON: &str = "application/json";

/// Root path of the dataset routes (relative, so it joins under any mount point).
pub const DATASETS_ROOT: &str = "api/datasets";
/// Root path of the admin principal routes.
pub const AUTH_PRINCIPALS_ROOT: &str = "api/auth/principals";
/// Path of `GET /api/auth/whoami`.
pub const AUTH_WHOAMI_PATH: &str = "api/auth/whoami";
/// Root path of the self-service key routes.
pub const AUTH_KEYS_ROOT: &str = "api/auth/keys";

/// Percent-encode a path segment, keeping only unreserved characters (Python `quote(s, safe="")`).
pub fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `api/datasets/{dataset}`.
pub fn dataset_path(dataset: &str) -> String {
    format!("{DATASETS_ROOT}/{}", encode(dataset))
}

/// `api/datasets/{dataset}/services`.
pub fn services_path(dataset: &str) -> String {
    format!("{DATASETS_ROOT}/{}/services", encode(dataset))
}

/// `api/datasets/{dataset}/services/{service_id}`.
pub fn service_path(dataset: &str, service_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/services/{}",
        encode(dataset),
        encode(service_id)
    )
}

/// `api/datasets/{dataset}/services/a2a`.
pub fn a2a_register_path(dataset: &str) -> String {
    format!("{DATASETS_ROOT}/{}/services/a2a", encode(dataset))
}

/// `api/datasets/{dataset}/services/{service_id}/heartbeat` (POST extends, DELETE revokes).
pub fn heartbeat_path(dataset: &str, service_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/services/{}/heartbeat",
        encode(dataset),
        encode(service_id)
    )
}

/// `api/datasets/{dataset}/lease-config`.
pub fn lease_config_path(dataset: &str) -> String {
    format!("{DATASETS_ROOT}/{}/lease-config", encode(dataset))
}

/// `api/datasets/{dataset}/reservations`.
pub fn reservations_path(dataset: &str) -> String {
    format!("{DATASETS_ROOT}/{}/reservations", encode(dataset))
}

/// `api/datasets/{dataset}/reservations/{holder_id}`.
pub fn reservation_holder_path(dataset: &str, holder_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/reservations/{}",
        encode(dataset),
        encode(holder_id)
    )
}

/// `api/datasets/{dataset}/reservations/{holder_id}/{service_id}`.
pub fn reservation_holder_sid_path(dataset: &str, holder_id: &str, service_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/reservations/{}/{}",
        encode(dataset),
        encode(holder_id),
        encode(service_id)
    )
}

/// `api/datasets/{dataset}/reservations/{holder_id}/extend`.
pub fn reservation_extend_path(dataset: &str, holder_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/reservations/{}/extend",
        encode(dataset),
        encode(holder_id)
    )
}

/// `api/datasets/{dataset}/services/{service_id}/lease` (teammate-self release).
pub fn service_lease_path(dataset: &str, service_id: &str) -> String {
    format!(
        "{DATASETS_ROOT}/{}/services/{}/lease",
        encode(dataset),
        encode(service_id)
    )
}

/// Ensure a trailing `/` so relative paths append under the mount point.
pub fn normalize_base_url(base_url: &str) -> String {
    if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    }
}

/// How `create_dataset` fills the `formats` field.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Formats {
    /// Argument omitted: send the SDK default `{"a2a": "v0.0"}`.
    #[default]
    Unset,
    /// Explicit `None`: omit the field so the backend enables every type.
    Omit,
    /// Explicit mapping.
    Explicit(JsonObject),
}

/// Body for `POST /api/datasets`.
///
/// `auth_required` is omitted when false so legacy callers produce byte-equal bodies.
pub fn build_create_dataset_body(
    name: &str,
    embedding_model: &str,
    formats: &Formats,
    auth_required: bool,
    lease_config: Option<&JsonObject>,
) -> JsonObject {
    let mut body = Map::new();
    body.insert("name".into(), Value::String(name.to_string()));
    body.insert(
        "embedding_model".into(),
        Value::String(embedding_model.to_string()),
    );
    match formats {
        Formats::Unset => {
            body.insert("formats".into(), Value::Object(default_formats()));
        }
        Formats::Omit => {}
        Formats::Explicit(map) => {
            body.insert("formats".into(), Value::Object(map.clone()));
        }
    }
    if auth_required {
        body.insert("auth_required".into(), Value::Bool(true));
    }
    if let Some(cfg) = lease_config {
        body.insert("lease_config".into(), Value::Object(cfg.clone()));
    }
    body
}

/// Body for `POST /api/auth/principals`.
pub fn build_create_principal_body(
    handle: &str,
    role: &str,
    namespaces: Option<&[String]>,
    note: Option<&str>,
) -> JsonObject {
    let mut body = Map::new();
    body.insert("handle".into(), Value::String(handle.to_string()));
    body.insert("role".into(), Value::String(role.to_string()));
    if let Some(ns) = namespaces {
        body.insert(
            "namespaces".into(),
            Value::Array(ns.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    if let Some(n) = note {
        body.insert("note".into(), Value::String(n.to_string()));
    }
    body
}

/// Body for `POST /services/a2a`.
pub fn build_register_agent_body(
    agent_card: &JsonObject,
    service_id: Option<&str>,
    persistent: bool,
) -> JsonObject {
    let mut body = Map::new();
    body.insert("agent_card".into(), Value::Object(agent_card.clone()));
    body.insert("persistent".into(), Value::Bool(persistent));
    if let Some(sid) = service_id {
        body.insert("service_id".into(), Value::String(sid.to_string()));
    }
    body
}

/// Body for `set_status`; validates the enum locally.
pub fn build_status_body(status: &str) -> Result<JsonObject, ClientError> {
    if !VALID_STATUSES.contains(&status) {
        return Err(ClientError::invalid(format!(
            "status must be one of {VALID_STATUSES:?}, got {status:?}"
        )));
    }
    let mut body = Map::new();
    body.insert(STATUS_FIELD.into(), Value::String(status.to_string()));
    Ok(body)
}

/// Blank-agent card template.
///
/// `name` encodes the endpoint so the server's deterministic service id stays
/// stable across re-registrations. `description` carries the blank sentinel and
/// `status` is `online` so the agent is immediately visible to online filters.
pub fn build_blank_agent_card(endpoint: &str) -> Result<JsonObject, ClientError> {
    if endpoint.trim().is_empty() {
        return Err(ClientError::invalid(format!(
            "endpoint must be a non-empty string, got {endpoint:?}"
        )));
    }
    let mut card = Map::new();
    card.insert(
        "name".into(),
        Value::String(format!("{BLANK_AGENT_NAME_PREFIX}{endpoint}")),
    );
    card.insert(
        "description".into(),
        Value::String(BLANK_DESCRIPTION_SENTINEL.to_string()),
    );
    card.insert(ENDPOINT_FIELD.into(), Value::String(endpoint.to_string()));
    card.insert(STATUS_FIELD.into(), Value::String(STATUS_ONLINE.to_string()));
    Ok(card)
}

/// Render a filter value the way Python's `str(v)` does for JSON scalars.
pub fn filter_value_to_string(value: &Value) -> Result<String, ClientError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(true) => Ok("True".to_string()),
        Value::Bool(false) => Ok("False".to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Err(ClientError::invalid("filter value must not be None")),
        other => Ok(other.to_string()),
    }
}

/// Build query params for `GET .../services?<filters>`.
///
/// Reserved keys (`fields`, `page`, `size`) and empty keys are rejected locally.
pub fn build_filter_params(filters: &[(String, String)]) -> Result<Vec<(String, String)>, ClientError> {
    const RESERVED: [&str; 3] = ["fields", "page", "size"];
    let mut params = Vec::with_capacity(filters.len());
    for (k, v) in filters {
        if k.is_empty() {
            return Err(ClientError::invalid(format!(
                "filter keys must be non-empty strings, got {k:?}"
            )));
        }
        if RESERVED.contains(&k.as_str()) {
            return Err(ClientError::invalid(format!(
                "filter key {k:?} collides with a reserved query param ({RESERVED:?}); backend would drop it before filtering"
            )));
        }
        params.push((k.clone(), v.clone()));
    }
    Ok(params)
}

/// Merge `page` / `size` into the query params with local validation.
///
/// `size == -1` leaves the params untouched (backend returns everything).
pub fn apply_pagination(
    mut params: Vec<(String, String)>,
    page: i64,
    size: i64,
) -> Result<Vec<(String, String)>, ClientError> {
    if page < 1 {
        return Err(ClientError::invalid(format!("page must be int >= 1, got {page}")));
    }
    if size < -1 {
        return Err(ClientError::invalid(format!(
            "size must be int >= -1 (-1 = all in one page), got {size}"
        )));
    }
    if size != -1 {
        params.push(("page".into(), page.to_string()));
        params.push(("size".into(), size.to_string()));
    }
    Ok(params)
}

/// Non-empty `endpoint` string from a card, if present.
pub fn extract_endpoint(card: &JsonObject) -> Option<String> {
    match card.get(ENDPOINT_FIELD) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Default ownership file: `~/.a2x_registry_client/owned.json`.
pub fn default_ownership_file() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".a2x_registry_client").join("owned.json"))
}

/// Default `Authorization` header value for an API key, if any.
pub fn build_default_headers(api_key: Option<&str>) -> Option<(&'static str, String)> {
    match api_key {
        Some(key) if !key.is_empty() => Some(("Authorization", format!("Bearer {key}"))),
        _ => None,
    }
}

/// Decode a `GET /services/{sid}` response or fail with `UnexpectedServiceType`.
pub fn parse_agent_detail(resp: &Response) -> Result<AgentDetail, ClientError> {
    let content_type = resp.content_type.clone().unwrap_or_default();
    if !content_type.to_ascii_lowercase().contains(CONTENT_TYPE_JSON) {
        let shown = if content_type.is_empty() {
            "<unknown>".to_string()
        } else {
            content_type
        };
        return Err(ClientError::UnexpectedServiceType {
            status: resp.status,
            message: format!("expected application/json, got {shown}"),
        });
    }
    let data = resp.json()?;
    match data {
        Value::Object(obj) => Ok(AgentDetail::from_object(obj)),
        other => Err(ClientError::UnexpectedServiceType {
            status: resp.status,
            message: format!(
                "expected JSON object for agent detail, got {}",
                json_type_name(&other)
            ),
        }),
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Flatten a `GET /services` response into `{id, type, name, description, ...card}` objects.
///
/// `metadata` keys win on conflict, so for a2a the top-level `description`
/// is the raw card value rather than the backend's transformed one.
pub fn parse_agent_list(resp: &Response) -> Result<Vec<JsonObject>, ClientError> {
    let data = resp.json()?;
    Ok(flatten_agent_list(&data))
}

/// Flatten an already decoded list body. Non-arrays yield an empty list.
pub fn flatten_agent_list(data: &Value) -> Vec<JsonObject> {
    let Value::Array(items) = data else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|wrapped| {
            let mut flat = wrapped.as_object()?.clone();
            if let Some(Value::Object(metadata)) = flat.remove("metadata") {
                for (k, v) in metadata {
                    flat.insert(k, v);
                }
            }
            Some(flat)
        })
        .collect()
}

/// `{"status": <status>}` heartbeat body helper.
pub fn build_heartbeat_body(status: Option<&str>) -> Value {
    match status {
        Some(s) => json!({ "status": s }),
        None => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn encode_matches_python_quote_safe_empty() -> TestResult {
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
        assert_eq!(encode("ok_.-~"), "ok_.-~");
        assert_eq!(encode("ü"), "%C3%BC");
        Ok(())
    }

    #[test]
    fn paths() -> TestResult {
        assert_eq!(service_path("ds/1", "sid"), "api/datasets/ds%2F1/services/sid");
        assert_eq!(heartbeat_path("d", "s"), "api/datasets/d/services/s/heartbeat");
        assert_eq!(
            reservation_extend_path("d", "h"),
            "api/datasets/d/reservations/h/extend"
        );
        assert_eq!(service_lease_path("d", "s"), "api/datasets/d/services/s/lease");
        assert_eq!(lease_config_path("d"), "api/datasets/d/lease-config");
        assert_eq!(
            reservation_holder_sid_path("d", "h", "s"),
            "api/datasets/d/reservations/h/s"
        );
        Ok(())
    }

    #[test]
    fn normalize_adds_trailing_slash_once() -> TestResult {
        assert_eq!(normalize_base_url("http://x"), "http://x/");
        assert_eq!(normalize_base_url("http://x/"), "http://x/");
        Ok(())
    }

    #[test]
    fn status_body_validation() -> TestResult {
        assert!(build_status_body("online").is_ok());
        assert!(matches!(
            build_status_body("nope"),
            Err(ClientError::InvalidArgument { .. })
        ));
        Ok(())
    }

    #[test]
    fn blank_card_shape() -> TestResult {
        let card = build_blank_agent_card("http://a")?;
        assert_eq!(card["name"], "_BlankAgent_http://a");
        assert_eq!(card["description"], "__BLANK__");
        assert_eq!(card["endpoint"], "http://a");
        assert_eq!(card["status"], "online");
        assert!(build_blank_agent_card("  ").is_err());
        Ok(())
    }

    #[test]
    fn filter_params_reject_reserved_and_empty() -> TestResult {
        assert!(build_filter_params(&[("fields".into(), "x".into())]).is_err());
        assert!(build_filter_params(&[("".into(), "x".into())]).is_err());
        assert_eq!(
            build_filter_params(&[("k".into(), "v".into())])?,
            vec![("k".to_string(), "v".to_string())]
        );
        Ok(())
    }

    #[test]
    fn pagination_rules() -> TestResult {
        assert!(apply_pagination(vec![], 0, -1).is_err());
        assert!(apply_pagination(vec![], 1, -2).is_err());
        assert!(apply_pagination(vec![], 1, -1)?.is_empty());
        let p = apply_pagination(vec![], 3, 50)?;
        assert_eq!(
            p,
            vec![
                ("page".to_string(), "3".to_string()),
                ("size".to_string(), "50".to_string())
            ]
        );
        Ok(())
    }

    #[test]
    fn flatten_metadata_wins() -> TestResult {
        let data = json!([
            {"id": "a", "type": "a2a", "description": "x.", "metadata": {"description": "x", "endpoint": "e"}},
            "junk",
            {"id": "b", "type": "generic"}
        ]);
        let flat = flatten_agent_list(&data);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0]["description"], "x");
        assert_eq!(flat[0]["endpoint"], "e");
        assert!(flat[0].get("metadata").is_none());
        assert_eq!(flat[1]["type"], "generic");
        assert!(flatten_agent_list(&json!({"not": "list"})).is_empty());
        Ok(())
    }

    #[test]
    fn filter_value_strings() -> TestResult {
        assert_eq!(filter_value_to_string(&json!(true))?, "True");
        assert_eq!(filter_value_to_string(&json!(3))?, "3");
        assert!(filter_value_to_string(&Value::Null).is_err());
        Ok(())
    }

    #[test]
    fn create_dataset_body_variants() -> TestResult {
        let b = build_create_dataset_body("d", DEFAULT_EMBEDDING_MODEL, &Formats::Unset, false, None);
        assert_eq!(b["formats"], json!({"a2a": "v0.0"}));
        assert!(b.get("auth_required").is_none());
        let b = build_create_dataset_body("d", DEFAULT_EMBEDDING_MODEL, &Formats::Omit, true, None);
        assert!(b.get("formats").is_none());
        assert_eq!(b["auth_required"], true);
        Ok(())
    }
}
