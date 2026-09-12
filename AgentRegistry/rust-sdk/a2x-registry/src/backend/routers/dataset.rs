// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Dataset management API: CRUD, services, reservations, skills, taxonomy
//! and per dataset configuration under `/api/datasets`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{parse_bool, run};
use crate::auth::{Authorize, RequireAdminOrAnon, RequireAdminStrict};
use crate::backend::default_queries::get_default_queries;
use crate::backend::errors::{ApiError, ApiResult};
use crate::backend::state::AppState;
use crate::heartbeat::HeartbeatError;
use crate::register::embedding::embedding_models_json;
use crate::register::service::{entry_filter_dict, filter_matches};
use crate::register::{
    RegisterA2ARequest, RegisterGenericRequest, RegisterResponse, RegistryEntry, RegistryError, ServiceType,
};
use crate::util::now_wall;

/// Router mounted at `/api/datasets`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_datasets).post(create_dataset))
        .route("/embedding-models", get(list_embedding_models))
        .route("/{dataset}", axum::routing::delete(delete_dataset))
        .route(
            "/{dataset}/auth-config",
            get(get_auth_config).post(set_auth_config),
        )
        .route(
            "/{dataset}/lease-config",
            get(get_lease_config).post(set_lease_config),
        )
        .route(
            "/{dataset}/register-config",
            get(get_register_config).post(set_register_config),
        )
        .route("/{dataset}/services", get(list_services))
        .route(
            "/{dataset}/services/{service_id}",
            get(get_single_service).put(update_service).delete(deregister),
        )
        .route("/{dataset}/services/generic", post(register_generic))
        .route("/{dataset}/services/a2a", post(register_a2a))
        .route(
            "/{dataset}/services/{service_id}/lease",
            axum::routing::delete(release_lease_self),
        )
        .route("/{dataset}/reservations", post(create_reservation))
        .route(
            "/{dataset}/reservations/{holder_id}",
            axum::routing::delete(release_reservation_bulk),
        )
        .route(
            "/{dataset}/reservations/{holder_id}/{service_id}",
            axum::routing::delete(release_reservation_one),
        )
        .route(
            "/{dataset}/reservations/{holder_id}/extend",
            post(extend_reservation),
        )
        .route("/{dataset}/skills", post(upload_skill))
        .route("/{dataset}/skills/{name}", axum::routing::delete(delete_skill))
        .route("/{dataset}/skills/{name}/download", get(download_skill))
        .route("/{dataset}/taxonomy", get(get_taxonomy))
        .route("/{dataset}/default-queries", get(default_queries))
        .route(
            "/{dataset}/vector-config",
            get(get_vector_config).post(set_vector_config),
        )
}

// ── Dataset CRUD ──────────────────────────────────────────────────────────────

async fn list_datasets(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    Json(state.registry.list_datasets_with_counts())
}

#[derive(Debug, Deserialize)]
struct InlineLeaseConfig {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "d10")]
    min_ttl: i64,
    #[serde(default = "d3600")]
    max_ttl: i64,
    #[serde(default = "d300")]
    grace_period: i64,
}

fn default_true() -> bool {
    true
}
fn d10() -> i64 {
    10
}
fn d3600() -> i64 {
    3600
}
fn d300() -> i64 {
    300
}

#[derive(Debug, Deserialize)]
struct CreateDatasetRequest {
    name: String,
    #[serde(default)]
    embedding_model: Option<String>,
    #[serde(default)]
    formats: Option<Map<String, Value>>,
    #[serde(default)]
    auth_required: bool,
    #[serde(default)]
    lease_config: Option<InlineLeaseConfig>,
}

async fn create_dataset(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateDatasetRequest>,
) -> ApiResult<Json<Value>> {
    if req.auth_required {
        if state.auth_store().is_none() {
            return Err(ApiError::conflict(
                "auth_not_initialized: cannot create an auth-required namespace before bootstrapping. Run 'a2x-registry auth init' first.",
            ));
        }
        let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
        RequireAdminStrict::check(
            &state,
            auth,
            "Admin token required to create auth-required namespace",
            "Admin role required to create auth-required namespace",
        )?;
    }
    let registry = state.registry.clone();
    let name = req.name.clone();
    let model = req.embedding_model.clone();
    let formats = req.formats.clone().map(Value::Object);
    let auth_required = req.auth_required;
    run(&state, move || {
        registry
            .create_dataset(&name, model.as_deref(), formats.as_ref(), auth_required)
            .map(|_| ())
    })
    .await?;
    if let Some(lc) = &req.lease_config {
        let registry = state.registry.clone();
        let name = req.name.clone();
        let (e, mn, mx, g) = (lc.enabled, lc.min_ttl, lc.max_ttl, lc.grace_period);
        run(&state, move || {
            registry.set_lease_config(&name, e, mn, mx, g).map(|_| ())
        })
        .await?;
    }
    let persisted = state.registry.get_vector_config(&req.name)?;
    let mut response = Map::new();
    response.insert("dataset".into(), Value::String(req.name.clone()));
    response.insert(
        "embedding_model".into(),
        persisted.get("embedding_model").cloned().unwrap_or(Value::Null),
    );
    response.insert(
        "formats".into(),
        formats_json(&state.registry.get_register_config(&req.name)?),
    );
    response.insert(
        "auth_required".into(),
        Value::Bool(state.registry.is_auth_required(&req.name)),
    );
    response.insert("status".into(), Value::String("created".into()));
    if req.lease_config.is_some() {
        response.insert(
            "lease_config".into(),
            Value::Object(state.registry.get_lease_config(&req.name).to_json()),
        );
    }
    Ok(Json(Value::Object(response)))
}

fn formats_json(cfg: &std::collections::HashMap<String, String>) -> Value {
    let mut m = Map::new();
    for t in crate::register::SUPPORTED_SERVICE_TYPES {
        if let Some(v) = cfg.get(*t) {
            m.insert((*t).to_string(), Value::String(v.clone()));
        }
    }
    Value::Object(m)
}

// ── Auth config toggle ────────────────────────────────────────────────────────

async fn get_auth_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
) -> ApiResult<Json<Value>> {
    if !state.database_dir.join(&dataset).exists() {
        return Err(ApiError::not_found(format!("Dataset '{dataset}' not found")));
    }
    Ok(Json(
        json!({"dataset": dataset, "required": state.registry.is_auth_required(&dataset)}),
    ))
}

#[derive(Debug, Deserialize)]
struct AuthConfigRequest {
    required: bool,
}

async fn set_auth_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminStrict(_admin): RequireAdminStrict,
    Json(req): Json<AuthConfigRequest>,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let ds = dataset.clone();
    let cfg = run(&state, move || registry.set_auth_config(&ds, req.required)).await?;
    let mut m = Map::new();
    m.insert("dataset".into(), Value::String(dataset));
    m.extend(cfg);
    Ok(Json(Value::Object(m)))
}

// ── Lease (heartbeat) config ──────────────────────────────────────────────────

async fn get_lease_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
) -> ApiResult<Json<Value>> {
    if !state.database_dir.join(&dataset).exists() {
        return Err(ApiError::not_found(format!("Dataset '{dataset}' not found")));
    }
    let mut m = Map::new();
    m.insert("dataset".into(), Value::String(dataset.clone()));
    m.extend(state.registry.get_lease_config(&dataset).to_json());
    Ok(Json(Value::Object(m)))
}

#[derive(Debug, Deserialize)]
struct LeaseConfigRequest {
    enabled: bool,
    #[serde(default = "d10")]
    min_ttl: i64,
    #[serde(default = "d3600")]
    max_ttl: i64,
    #[serde(default = "d300")]
    grace_period: i64,
}

async fn set_lease_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
    Json(req): Json<LeaseConfigRequest>,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let ds = dataset.clone();
    let cfg = run(&state, move || {
        registry.set_lease_config(&ds, req.enabled, req.min_ttl, req.max_ttl, req.grace_period)
    })
    .await?;
    let mut m = Map::new();
    m.insert("dataset".into(), Value::String(dataset));
    m.extend(cfg.to_json());
    Ok(Json(Value::Object(m)))
}

// ── Registration format config ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RegisterConfigRequest {
    formats: Map<String, Value>,
}

async fn get_register_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let cfg = state.registry.get_register_config(&dataset)?;
    Ok(Json(json!({"dataset": dataset, "formats": formats_json(&cfg)})))
}

async fn set_register_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
    Json(req): Json<RegisterConfigRequest>,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let ds = dataset.clone();
    let formats = Value::Object(req.formats);
    let cfg = run(&state, move || registry.set_register_config(&ds, &formats)).await?;
    Ok(Json(json!({"dataset": dataset, "formats": formats_json(&cfg)})))
}

async fn delete_dataset(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
) -> ApiResult<Json<Value>> {
    state.search.purge_dataset(&dataset);
    state.taxonomy.invalidate(&dataset);
    let registry = state.registry.clone();
    let ds = dataset.clone();
    run(&state, move || registry.delete_dataset(&ds)).await?;
    Ok(Json(json!({"dataset": dataset, "status": "deleted"})))
}

// ── Services ──────────────────────────────────────────────────────────────────

const RESERVED_QUERY_PARAMS: &[&str] = &["fields", "page", "size", "include_leased", "include_unhealthy"];

async fn list_services(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Response> {
    let mut fields = "detail".to_string();
    let mut size: i64 = -1;
    let mut page: i64 = 1;
    let mut include_leased = false;
    let mut include_unhealthy = false;
    let mut filters: IndexMap<String, String> = IndexMap::new();
    for (k, v) in params {
        match k.as_str() {
            "fields" => fields = v,
            "size" => {
                size = v
                    .trim()
                    .parse()
                    .ok()
                    .filter(|s| *s >= -1)
                    .ok_or_else(|| ApiError::unprocessable("size must be an integer >= -1"))?
            }
            "page" => {
                page = v
                    .trim()
                    .parse()
                    .ok()
                    .filter(|p| *p >= 1)
                    .ok_or_else(|| ApiError::unprocessable("page must be an integer >= 1"))?
            }
            "include_leased" => include_leased = parse_bool("include_leased", &v)?,
            "include_unhealthy" => include_unhealthy = parse_bool("include_unhealthy", &v)?,
            _ => {
                if !RESERVED_QUERY_PARAMS.contains(&k.as_str()) {
                    filters.insert(k, v);
                }
            }
        }
    }
    if fields != "brief" && fields != "detail" {
        return Err(ApiError::bad_request(format!(
            "fields must be one of ['brief', 'detail'], got '{fields}'"
        )));
    }
    let filter_map: Map<String, Value> = filters
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();

    let svc = &state.registry;
    let wrapped_by_id: std::collections::HashMap<String, Value> = svc
        .list_services(&dataset)
        .into_iter()
        .filter_map(|s| {
            s.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), s.clone()))
        })
        .collect();
    let mut entries = svc.list_entries(&dataset);
    entries.sort_by(|a, b| a.service_id.cmp(&b.service_id));
    let mut matched: Vec<Value> = Vec::new();
    for entry in &entries {
        if !include_leased && svc.is_leased(&dataset, &entry.service_id) {
            continue;
        }
        if !include_unhealthy && svc.is_unhealthy(&dataset, &entry.service_id) {
            continue;
        }
        let Some(raw) = entry_filter_dict(entry) else {
            continue;
        };
        if !filter_map.is_empty() && !filter_matches(&filter_map, &raw) {
            continue;
        }
        let Some(wrapped) = wrapped_by_id.get(&entry.service_id) else {
            continue;
        };
        let mut row = wrapped.as_object().cloned().unwrap_or_default();
        row.insert("source".into(), Value::String(entry.source.as_str().into()));
        matched.push(Value::Object(row));
    }
    if let Some(cluster) = state.cluster() {
        for fr in cluster.foreign_rows(&dataset) {
            let Some(raw) = entry_filter_dict(&fr.entry) else {
                continue;
            };
            if !filter_map.is_empty() && !filter_matches(&filter_map, &raw) {
                continue;
            }
            let mut row = fr.wrapped.as_object().cloned().unwrap_or_default();
            row.insert("source".into(), Value::String("cluster".into()));
            matched.push(Value::Object(row));
        }
    }

    let total = matched.len();
    let mut headers = HeaderMap::new();
    let page_slice: Vec<Value> = if size == -1 {
        matched
    } else {
        let size_u = size.max(0) as usize;
        let offset = (page as usize - 1) * size_u;
        let slice: Vec<Value> = matched.into_iter().skip(offset).take(size_u).collect();
        let total_pages = if size_u == 0 {
            1
        } else {
            std::cmp::max(1, total.div_ceil(size_u))
        };
        headers.insert(
            "X-Total-Count",
            HeaderValue::from_str(&total.to_string()).unwrap_or(HeaderValue::from_static("0")),
        );
        headers.insert(
            "X-Page",
            HeaderValue::from_str(&page.to_string()).unwrap_or(HeaderValue::from_static("1")),
        );
        headers.insert(
            "X-Total-Pages",
            HeaderValue::from_str(&total_pages.to_string()).unwrap_or(HeaderValue::from_static("1")),
        );
        headers.insert(
            "X-Page-Size",
            HeaderValue::from_str(&size.to_string()).unwrap_or(HeaderValue::from_static("0")),
        );
        slice
    };
    let body: Vec<Value> = if fields == "brief" {
        page_slice
            .iter()
            .map(|s| {
                json!({
                    "id": s.get("id").cloned().unwrap_or(Value::Null),
                    "name": s.get("name").cloned().unwrap_or(Value::Null),
                    "description": s.get("description").cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    } else {
        page_slice
    };
    Ok((headers, Json(body)).into_response())
}

fn zip_response(name: &str, bytes: Vec<u8>) -> Response {
    let disposition = format!("attachment; filename=\"{name}.zip\"");
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/zip")),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).unwrap_or(HeaderValue::from_static("attachment")),
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn get_single_service(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Response> {
    let svc = &state.registry;
    let Some(entry) = svc.get_entry(&dataset, &service_id) else {
        if let Some(cluster) = state.cluster() {
            if service_id.contains(':') {
                if let Some(foreign) = cluster.foreign_entry(&dataset, &service_id) {
                    return Ok(Json(foreign).into_response());
                }
            }
        }
        return Err(ApiError::not_found(format!(
            "Service '{service_id}' not found in dataset '{dataset}'"
        )));
    };
    if entry.r#type == ServiceType::Skill {
        if let Some(sd) = &entry.skill_data {
            let bytes = svc.get_skill_zip(&dataset, &sd.name).map_err(|e| match e {
                RegistryError::FileNotFound(_) => {
                    ApiError::not_found(format!("Skill folder not found: {}", sd.name))
                }
                other => ApiError::from(other),
            })?;
            return Ok(zip_response(&sd.name, bytes));
        }
    }
    let output = svc
        .list_services(&dataset)
        .into_iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(service_id.as_str()))
        .unwrap_or(Value::Null);
    Ok(Json(output).into_response())
}

/// Run the heartbeat four corner matrix before registering.
fn validate_lease_request(
    state: &AppState,
    dataset: &str,
    client_ttl: Option<i64>,
) -> ApiResult<Option<i64>> {
    match state.heartbeat_store() {
        None => match client_ttl {
            None => Ok(None),
            Some(_) => Err(ApiError::from(HeartbeatError::not_supported(
                "Heartbeat module not initialized on this registry; remove 'lease_ttl' from the request.",
            ))),
        },
        Some(store) => store.validate(dataset, client_ttl).map_err(ApiError::from),
    }
}

/// Install the validated lease after a successful registration.
fn install_lease(
    state: &AppState,
    mut response: RegisterResponse,
    dataset: &str,
    ttl: Option<i64>,
) -> RegisterResponse {
    let Some(ttl) = ttl else { return response };
    if let Some(store) = state.heartbeat_store() {
        let lease = store.install(dataset, &response.service_id, ttl);
        response.lease_ttl = Some(lease.ttl_seconds as i64);
        response.lease_expires_at = Some(now_wall() + lease.ttl_seconds as f64);
    }
    response
}

async fn register_generic(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(ctx): Authorize,
    Json(mut req): Json<RegisterGenericRequest>,
) -> ApiResult<Json<RegisterResponse>> {
    req.dataset = dataset.clone();
    let validated_ttl = validate_lease_request(&state, &dataset, req.lease_ttl)?;
    req.lease_ttl = validated_ttl;
    let registry = state.registry.clone();
    let response = run(&state, move || registry.register_generic(&req, ctx.as_ref())).await?;
    Ok(Json(install_lease(&state, response, &dataset, validated_ttl)))
}

async fn register_a2a(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(ctx): Authorize,
    Json(mut req): Json<RegisterA2ARequest>,
) -> ApiResult<Json<RegisterResponse>> {
    req.dataset = dataset.clone();
    let validated_ttl = validate_lease_request(&state, &dataset, req.lease_ttl)?;
    req.lease_ttl = validated_ttl;
    let (card, url) = if let Some(card) = req.agent_card.clone() {
        (card, None)
    } else if let Some(url) = req.agent_card_url.clone() {
        (
            crate::register::agent_card::fetch_agent_card(&url).await?,
            Some(url),
        )
    } else {
        return Err(ApiError::bad_request(
            "Either agent_card or agent_card_url must be provided",
        ));
    };
    let registry = state.registry.clone();
    let response = run(&state, move || {
        registry.register_a2a_resolved(&req, card, url, ctx.as_ref())
    })
    .await?;
    Ok(Json(install_lease(&state, response, &dataset, validated_ttl)))
}

/// Server identity fields stripped from every `PUT` body.
const FORBIDDEN_UPDATE_FIELDS: &[&str] = &["owner_id", "service_id", "type", "source"];

async fn update_service(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
    Json(updates): Json<Value>,
) -> ApiResult<Json<Value>> {
    let Value::Object(mut updates) = updates else {
        return Err(ApiError::unprocessable(
            "body must be a JSON object of {field: value}",
        ));
    };
    updates.retain(|k, _| !FORBIDDEN_UPDATE_FIELDS.contains(&k.as_str()));
    let registry = state.registry.clone();
    let resp = run(&state, move || {
        registry.update_service(&dataset, &service_id, &updates, ctx.as_ref())
    })
    .await?;
    Ok(Json(serde_json::to_value(resp).unwrap_or(Value::Null)))
}

async fn deregister(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let resp = run(&state, move || {
        registry.deregister(&dataset, &service_id, ctx.as_ref())
    })
    .await?;
    Ok(Json(serde_json::to_value(resp).unwrap_or(Value::Null)))
}

// ── Reservations ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ReservationRequest {
    #[serde(default)]
    filters: Map<String, Value>,
    #[serde(default = "d1")]
    n: i64,
    #[serde(default = "d30")]
    ttl_seconds: i64,
    #[serde(default)]
    holder_id: Option<String>,
}

fn d1() -> i64 {
    1
}
fn d30() -> i64 {
    30
}

async fn create_reservation(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(ctx): Authorize,
    Json(req): Json<ReservationRequest>,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let ttl = req.ttl_seconds;
    let (holder_id, expires_at_unix, claimed) = run(&state, move || {
        registry.reserve_services(
            &dataset,
            &req.filters,
            req.n,
            req.ttl_seconds,
            req.holder_id,
            ctx.as_ref(),
        )
    })
    .await?;
    Ok(Json(json!({
        "holder_id": holder_id,
        "ttl_seconds": ttl,
        "expires_at_unix": expires_at_unix,
        "reservations": claimed,
    })))
}

async fn release_reservation_bulk(
    State(state): State<Arc<AppState>>,
    Path((dataset, holder_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let released = run(&state, move || {
        registry.release_reservation(&dataset, &holder_id, None, ctx.as_ref())
    })
    .await?;
    Ok(Json(json!({ "released": released })))
}

async fn release_reservation_one(
    State(state): State<Arc<AppState>>,
    Path((dataset, holder_id, service_id)): Path<(String, String, String)>,
    Authorize(ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let released = run(&state, move || {
        registry.release_reservation(&dataset, &holder_id, Some(&[service_id]), ctx.as_ref())
    })
    .await?;
    Ok(Json(json!({ "released": released })))
}

async fn extend_reservation(
    State(state): State<Arc<AppState>>,
    Path((dataset, holder_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ttl = match body.get("ttl_seconds") {
        None => 30,
        Some(v) => {
            crate::util::py_int(v).ok_or_else(|| ApiError::unprocessable("ttl_seconds must be an integer"))?
        }
    };
    let registry = state.registry.clone();
    let new_expires = run(&state, move || {
        registry.extend_reservation(&dataset, &holder_id, ttl, ctx.as_ref())
    })
    .await?;
    Ok(Json(json!({ "expires_at_unix": new_expires })))
}

async fn release_lease_self(
    State(state): State<Arc<AppState>>,
    Path((dataset, service_id)): Path<(String, String)>,
    Authorize(ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let (released, prev) = run(&state, move || {
        registry.release_lease_by_sid(&dataset, &service_id, ctx.as_ref())
    })
    .await?;
    Ok(Json(json!({ "released": released, "prev_holder_id": prev })))
}

// ── Skills ────────────────────────────────────────────────────────────────────

async fn upload_skill(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(ctx): Authorize,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let mut zip_bytes: Option<Bytes> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::unprocessable(format!("invalid multipart body: {e}")))?
    {
        if field.name() == Some("file") {
            zip_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::unprocessable(format!("cannot read upload: {e}")))?,
            );
            break;
        }
    }
    let Some(zip_bytes) = zip_bytes else {
        return Err(ApiError::unprocessable("multipart field 'file' is required"));
    };
    let registry = state.registry.clone();
    let resp = run(&state, move || {
        registry.register_skill(&dataset, &zip_bytes, ctx.as_ref())
    })
    .await?;
    Ok(Json(serde_json::to_value(resp).unwrap_or(Value::Null)))
}

async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Path((dataset, name)): Path<(String, String)>,
    Authorize(ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let registry = state.registry.clone();
    let resp = run(&state, move || {
        registry.deregister_skill(&dataset, &name, ctx.as_ref())
    })
    .await?;
    Ok(Json(serde_json::to_value(resp).unwrap_or(Value::Null)))
}

async fn download_skill(
    State(state): State<Arc<AppState>>,
    Path((dataset, name)): Path<(String, String)>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Response> {
    let bytes = state
        .registry
        .get_skill_zip(&dataset, &name)
        .map_err(|e| match e {
            RegistryError::FileNotFound(_) => ApiError::not_found(format!("Skill '{name}' not found")),
            other => ApiError::from(other),
        })?;
    Ok(zip_response(&name, bytes))
}

// ── Taxonomy and default queries ──────────────────────────────────────────────

async fn get_taxonomy(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Json<Value>> {
    state
        .taxonomy
        .get_taxonomy_tree(&dataset)
        .map(Json)
        .map_err(|_| ApiError::not_found(format!("No taxonomy for dataset '{dataset}'")))
}

async fn default_queries(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let (queries, source) = get_default_queries(&state.config_home, &state.database_dir, &dataset);
    let mut out = Vec::new();
    for q in queries {
        let parsed: crate::backend::schemas::DefaultQuery = serde_json::from_value(q)
            .map_err(|e| ApiError::internal(format!("invalid default query: {e}")))?;
        out.push(serde_json::to_value(parsed).unwrap_or(Value::Null));
    }
    Ok(Json(json!({ "source": source, "queries": out })))
}

// ── Embedding / vector config ─────────────────────────────────────────────────

async fn list_embedding_models() -> Json<Value> {
    Json(json!({ "models": embedding_models_json() }))
}

async fn get_vector_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Authorize(_ctx): Authorize,
) -> ApiResult<Json<Value>> {
    let cfg = state.registry.get_vector_config(&dataset)?;
    let mut m = Map::new();
    m.insert("dataset".into(), Value::String(dataset));
    m.extend(cfg);
    Ok(Json(Value::Object(m)))
}

async fn set_vector_config(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    RequireAdminOrAnon(_ctx): RequireAdminOrAnon,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let model = body
        .get("embedding_model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let dim = body.get("embedding_dim").and_then(crate::util::py_int);
    let registry = state.registry.clone();
    let ds = dataset.clone();
    let cfg = run(&state, move || {
        registry.set_vector_config(&ds, model.as_deref(), dim)
    })
    .await?;
    state.search.schedule_vector_sync(&dataset);
    let mut m = Map::new();
    m.insert("dataset".into(), Value::String(dataset));
    m.extend(cfg);
    m.insert(
        "message".into(),
        Value::String("配置已保存，向量索引将在后台重建".into()),
    );
    Ok(Json(Value::Object(m)))
}

/// Sort helper reused by tests.
pub fn sorted_entries(mut entries: Vec<RegistryEntry>) -> Vec<RegistryEntry> {
    entries.sort_by(|a, b| a.service_id.cmp(&b.service_id));
    entries
}
