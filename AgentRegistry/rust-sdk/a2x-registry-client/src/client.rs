// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Asynchronous client entry point.
//!
//! [`A2xRegistryClient`] composes a [`Transport`] and an [`OwnershipStore`]
//! and turns each public method into one HTTP call plus, for mutating
//! methods, an ownership check or update. Business rules live here; network
//! and persistence concerns stay in their own modules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::auth::resolve_credentials;
use crate::errors::ClientError;
use crate::heartbeat::{HeartbeatFn, HeartbeatRegistry, HeartbeatRenewer};
use crate::internal as i;
use crate::internal::Formats;
use crate::models::{
    AgentDetail, DatasetCreateResponse, DatasetDeleteResponse, DeregisterResponse, JsonObject, PatchResponse,
    PrincipalCreateResponse, RegisterResponse, Reservation, ShutdownReport, decode,
};
use crate::ownership::OwnershipStore;
use crate::transport::{HttpMethod, Transport};

/// Default HTTP timeout, matching the Python `timeout=30.0`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the client persists the set of services it registered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OwnershipFile {
    /// `~/.a2x_registry_client/owned.json` (Python `ownership_file=None`).
    #[default]
    Default,
    /// Memory-only, nothing on disk (Python `ownership_file=False`).
    Disabled,
    /// Explicit file path.
    Path(PathBuf),
}

/// Constructor arguments for [`A2xRegistryClient`].
///
/// Mirrors `A2XRegistryClient.__init__(base_url, timeout, api_key, ownership_file)`.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Registry URL. `None` reads `cli_token.json`, then falls back to `http://127.0.0.1:8000`.
    pub base_url: Option<String>,
    /// HTTP timeout. Default 30 seconds.
    pub timeout: Duration,
    /// API key. `None` reads `cli_token.json`; when still absent no `Authorization` header is sent.
    pub api_key: Option<String>,
    /// Ownership persistence mode.
    pub ownership_file: OwnershipFile,
    /// Override of the `cli_token.json` location (mainly for tests).
    pub config_path: Option<PathBuf>,
    /// Override of the heartbeat renewal period; `None` uses `max(1s, ttl / 3)`. Test hook.
    pub heartbeat_period: Option<Duration>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            base_url: None,
            timeout: DEFAULT_TIMEOUT,
            api_key: None,
            ownership_file: OwnershipFile::Default,
            config_path: None,
            heartbeat_period: None,
        }
    }
}

impl ClientConfig {
    /// Defaults: everything resolved from `cli_token.json` and the SDK fallbacks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the registry URL.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set the HTTP timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the API key.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Set the ownership persistence mode.
    pub fn ownership_file(mut self, mode: OwnershipFile) -> Self {
        self.ownership_file = mode;
        self
    }

    /// Set the `cli_token.json` location.
    pub fn config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    /// Override the heartbeat renewal period.
    pub fn heartbeat_period(mut self, period: Duration) -> Self {
        self.heartbeat_period = Some(period);
        self
    }
}

/// Arguments for [`A2xRegistryClient::create_dataset`].
#[derive(Debug, Clone)]
pub struct CreateDatasetOptions {
    /// Embedding model, default `all-MiniLM-L6-v2`.
    pub embedding_model: String,
    /// Accepted registration formats.
    pub formats: Formats,
    /// Require API keys inside this namespace.
    pub auth_required: bool,
    /// Per-namespace heartbeat policy, e.g. `{"enabled": true, "min_ttl": 10, ...}`.
    pub lease_config: Option<JsonObject>,
}

impl Default for CreateDatasetOptions {
    fn default() -> Self {
        CreateDatasetOptions {
            embedding_model: i::DEFAULT_EMBEDDING_MODEL.to_string(),
            formats: Formats::Unset,
            auth_required: false,
            lease_config: None,
        }
    }
}

/// Arguments for [`A2xRegistryClient::register_agent`].
#[derive(Debug, Clone)]
pub struct RegisterOptions {
    /// Explicit service id; the server derives one from `name` when omitted.
    pub service_id: Option<String>,
    /// Persist the entry on the server and record ownership locally. Default true.
    pub persistent: bool,
    /// Heartbeat lease TTL in seconds; `None` registers a permanent service.
    pub lease_ttl: Option<i64>,
    /// Spawn a background renewer when the server grants a lease. Default false.
    pub auto_renew: bool,
}

impl Default for RegisterOptions {
    fn default() -> Self {
        RegisterOptions {
            service_id: None,
            persistent: true,
            lease_ttl: None,
            auto_renew: false,
        }
    }
}

/// Arguments for [`A2xRegistryClient::list_agents`].
#[derive(Debug, Clone)]
pub struct ListOptions {
    /// 1-indexed page, used only when `size > 0`.
    pub page: i64,
    /// Page size; `-1` (default) returns everything in one response.
    pub size: i64,
    /// Equality filters with AND semantics, applied as query parameters.
    pub filters: Vec<(String, String)>,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            page: 1,
            size: -1,
            filters: Vec::new(),
        }
    }
}

impl ListOptions {
    /// No filters, no pagination.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a string filter.
    pub fn filter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.push((key.into(), value.into()));
        self
    }

    /// Add a filter from a JSON scalar, rendered like Python's `str(v)`.
    pub fn filter_value(mut self, key: impl Into<String>, value: &Value) -> Result<Self, ClientError> {
        self.filters.push((key.into(), i::filter_value_to_string(value)?));
        Ok(self)
    }

    /// Set the page number.
    pub fn page(mut self, page: i64) -> Self {
        self.page = page;
        self
    }

    /// Set the page size.
    pub fn size(mut self, size: i64) -> Self {
        self.size = size;
        self
    }
}

/// Arguments for [`A2xRegistryClient::reserve_blank_agents`].
#[derive(Debug, Clone)]
pub struct ReserveOptions {
    /// Maximum number of agents to lock. Default 1.
    pub n: usize,
    /// Lease duration in seconds. Default 30.
    pub ttl_seconds: i64,
    /// Stable holder id; the server generates `holder_<uuid>` when omitted.
    pub holder_id: Option<String>,
    /// Extra filters merged over the blank-agent defaults (same keys override).
    pub extra_filters: JsonObject,
}

impl Default for ReserveOptions {
    fn default() -> Self {
        ReserveOptions {
            n: 1,
            ttl_seconds: i::DEFAULT_RESERVATION_TTL,
            holder_id: None,
            extra_filters: Map::new(),
        }
    }
}

/// Arguments for [`A2xRegistryClient::shutdown`].
#[derive(Debug, Clone)]
pub struct ShutdownOptions {
    /// Explicit `(dataset, service_id)` targets. Takes precedence over `dataset`.
    pub sids: Option<Vec<(String, String)>>,
    /// Every owned service in this dataset.
    pub dataset: Option<String>,
    /// Deregister instead of revoking the lease. Default false.
    pub permanent: bool,
    /// Reserved for audit logs; unused by the server today.
    pub reason: String,
    /// Per-call timeout. Default 2 seconds.
    pub timeout: Duration,
    /// Return the first error instead of collecting it. Default false.
    pub raise_on_error: bool,
}

impl Default for ShutdownOptions {
    fn default() -> Self {
        ShutdownOptions {
            sids: None,
            dataset: None,
            permanent: false,
            reason: "explicit".to_string(),
            timeout: Duration::from_secs(2),
            raise_on_error: false,
        }
    }
}

struct ClientInner {
    transport: Transport,
    timeout: Duration,
    api_key: Option<String>,
    owned: Arc<OwnershipStore>,
    /// L1 cache for `restore_to_blank`: `(dataset, service_id) -> endpoint`. Memory only.
    blank_endpoints: Mutex<HashMap<(String, String), String>>,
    renewers: HeartbeatRegistry,
    heartbeat_period: Option<Duration>,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        self.renewers.signal_stop_all();
    }
}

/// Async client for the A2X registry. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct A2xRegistryClient {
    inner: Arc<ClientInner>,
}

impl std::fmt::Debug for A2xRegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2xRegistryClient")
            .field("base_url", &self.base_url())
            .field("timeout", &self.inner.timeout)
            .field("api_key", &self.inner.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl A2xRegistryClient {
    /// Build a client. No HTTP is sent; the ownership file is loaded from disk.
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        let (api_key, base_url) = resolve_credentials(
            config.api_key.as_deref(),
            config.base_url.as_deref(),
            config.config_path.as_deref(),
        );
        let transport = Transport::new(&base_url, config.timeout, api_key.as_deref())?;
        let file_path = match config.ownership_file {
            OwnershipFile::Default => i::default_ownership_file(),
            OwnershipFile::Disabled => None,
            OwnershipFile::Path(p) => Some(p),
        };
        let owned = Arc::new(OwnershipStore::new(file_path, transport.base_url()));
        Ok(A2xRegistryClient {
            inner: Arc::new(ClientInner {
                transport,
                timeout: config.timeout,
                api_key,
                owned,
                blank_endpoints: Mutex::new(HashMap::new()),
                renewers: HeartbeatRegistry::new(),
                heartbeat_period: config.heartbeat_period,
            }),
        })
    }

    /// Convenience: explicit URL, memory-only ownership, no API key lookup.
    pub fn connect(base_url: &str) -> Result<Self, ClientError> {
        Self::new(
            ClientConfig::new()
                .base_url(base_url)
                .ownership_file(OwnershipFile::Disabled),
        )
    }

    /// Normalised base URL (always ends with `/`).
    pub fn base_url(&self) -> &str {
        self.inner.transport.base_url()
    }

    /// Configured HTTP timeout.
    pub fn timeout(&self) -> Duration {
        self.inner.timeout
    }

    /// Resolved API key, if any.
    pub fn api_key(&self) -> Option<&str> {
        self.inner.api_key.as_deref()
    }

    /// The ownership store backing the `_owned` checks.
    pub fn ownership(&self) -> &OwnershipStore {
        &self.inner.owned
    }

    /// The registry of background heartbeat renewers.
    pub fn heartbeat_renewers(&self) -> &HeartbeatRegistry {
        &self.inner.renewers
    }

    /// Stop background renewers (best-effort, 1 second each). Not a deregister.
    pub async fn close(&self) {
        self.inner.renewers.shutdown_all(Duration::from_secs(1)).await;
    }

    // ── Raw access (used by the CLI) ─────────────────────────────────────

    /// Send an arbitrary request under the base URL and decode the JSON body.
    pub async fn request_json(
        &self,
        method: HttpMethod,
        path: &str,
        json: Option<&Value>,
    ) -> Result<Value, ClientError> {
        self.inner
            .transport
            .request(method, path, json, None)
            .await?
            .json()
    }

    // ── Ownership helpers ────────────────────────────────────────────────

    fn assert_owned(&self, dataset: &str, service_id: &str) -> Result<(), ClientError> {
        if self.inner.owned.contains(dataset, service_id) {
            Ok(())
        } else {
            Err(ClientError::NotOwned {
                dataset: dataset.to_string(),
                service_id: service_id.to_string(),
            })
        }
    }

    async fn owned_blocking<F>(&self, f: F) -> Result<(), ClientError>
    where
        F: FnOnce(&OwnershipStore) + Send + 'static,
    {
        let owned = Arc::clone(&self.inner.owned);
        tokio::task::spawn_blocking(move || f(&owned))
            .await
            .map_err(|e| ClientError::Io(std::io::Error::other(e)))
    }

    async fn owned_add(&self, dataset: &str, service_id: &str) -> Result<(), ClientError> {
        let (ds, sid) = (dataset.to_string(), service_id.to_string());
        self.owned_blocking(move |o| o.add(&ds, &sid)).await
    }

    async fn owned_remove(&self, dataset: &str, service_id: &str) -> Result<(), ClientError> {
        let (ds, sid) = (dataset.to_string(), service_id.to_string());
        self.owned_blocking(move |o| o.remove(&ds, &sid)).await
    }

    async fn owned_remove_dataset(&self, dataset: &str) -> Result<(), ClientError> {
        let ds = dataset.to_string();
        self.owned_blocking(move |o| o.remove_dataset(&ds)).await
    }

    fn cache_endpoint(&self, dataset: &str, service_id: &str, endpoint: &str) {
        self.inner.blank_endpoints.lock().insert(
            (dataset.to_string(), service_id.to_string()),
            endpoint.to_string(),
        );
    }

    fn forget_endpoint(&self, dataset: &str, service_id: &str) {
        self.inner
            .blank_endpoints
            .lock()
            .remove(&(dataset.to_string(), service_id.to_string()));
    }

    // ── Datasets ─────────────────────────────────────────────────────────

    /// Create a dataset (namespace). `POST /api/datasets`.
    ///
    /// `auth_required` and `lease_config` are opt-in and omitted from the body
    /// when unset, so legacy callers keep byte-equal request bodies.
    pub async fn create_dataset(
        &self,
        name: &str,
        opts: &CreateDatasetOptions,
    ) -> Result<DatasetCreateResponse, ClientError> {
        let body = i::build_create_dataset_body(
            name,
            &opts.embedding_model,
            &opts.formats,
            opts.auth_required,
            opts.lease_config.as_ref(),
        );
        let resp = self
            .inner
            .transport
            .request(
                HttpMethod::Post,
                i::DATASETS_ROOT,
                Some(&Value::Object(body)),
                None,
            )
            .await?;
        decode(resp.json()?)
    }

    /// Create a principal and return its first API key in plaintext. Admin only.
    ///
    /// `role` is `admin`, `provider` or `user`; non-admin roles must pass `namespaces`.
    pub async fn create_principal(
        &self,
        handle: &str,
        role: &str,
        namespaces: Option<&[String]>,
        note: Option<&str>,
    ) -> Result<PrincipalCreateResponse, ClientError> {
        let body = i::build_create_principal_body(handle, role, namespaces, note);
        let resp = self
            .inner
            .transport
            .request(
                HttpMethod::Post,
                i::AUTH_PRINCIPALS_ROOT,
                Some(&Value::Object(body)),
                None,
            )
            .await?;
        decode(resp.json()?)
    }

    /// Delete a dataset. Local ownership for it is cleared on success and on a 400.
    pub async fn delete_dataset(&self, name: &str) -> Result<DatasetDeleteResponse, ClientError> {
        let resp = match self
            .inner
            .transport
            .request(HttpMethod::Delete, &i::dataset_path(name), None, None)
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_validation() => {
                self.owned_remove_dataset(name).await?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let result: DatasetDeleteResponse = decode(resp.json()?)?;
        self.owned_remove_dataset(name).await?;
        Ok(result)
    }

    // ── Agents ───────────────────────────────────────────────────────────

    /// Register an A2A agent. `POST /api/datasets/{dataset}/services/a2a`.
    ///
    /// With `persistent` the service id is recorded as owned. With `lease_ttl`
    /// the server grants a heartbeat lease; with `auto_renew` a background
    /// renewer is started for it. Lease rejections surface as the typed
    /// `HeartbeatNotSupported`, `TtlRequired` and `TtlOutOfRange` errors.
    pub async fn register_agent(
        &self,
        dataset: &str,
        agent_card: &JsonObject,
        opts: &RegisterOptions,
    ) -> Result<RegisterResponse, ClientError> {
        let mut body = i::build_register_agent_body(agent_card, opts.service_id.as_deref(), opts.persistent);
        if let Some(ttl) = opts.lease_ttl {
            body.insert("lease_ttl".into(), json!(ttl));
        }
        let resp = self
            .inner
            .transport
            .request(
                HttpMethod::Post,
                &i::a2a_register_path(dataset),
                Some(&Value::Object(body)),
                None,
            )
            .await?;
        let result: RegisterResponse = decode(resp.json()?)?;
        if opts.persistent {
            self.owned_add(dataset, &result.service_id).await?;
        }
        if opts.auto_renew {
            if let Some(ttl) = result.lease_ttl {
                self.install_renewer(dataset, &result.service_id, ttl)?;
            }
        }
        Ok(result)
    }

    fn install_renewer(&self, dataset: &str, service_id: &str, ttl: i64) -> Result<(), ClientError> {
        let weak = Arc::downgrade(&self.inner);
        let fn_: HeartbeatFn = Arc::new(move |ds: String, sid: String| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(inner) = weak.upgrade() else {
                    return Err(ClientError::Connection {
                        message: "client was dropped".to_string(),
                    });
                };
                A2xRegistryClient { inner }
                    .heartbeat(&ds, &sid, None)
                    .await
                    .map(|_| ())
            })
        });
        let renewer = HeartbeatRenewer::new(dataset, service_id, ttl, fn_, self.inner.heartbeat_period)?;
        self.inner.renewers.add(renewer);
        Ok(())
    }

    // ── Heartbeat / lifecycle ────────────────────────────────────────────

    /// Extend the heartbeat lease of an owned service. Returns the server's lease info.
    ///
    /// `status` piggybacks an `agent_card.status` update in the same call.
    pub async fn heartbeat(
        &self,
        dataset: &str,
        service_id: &str,
        status: Option<&str>,
    ) -> Result<Value, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let body = i::build_heartbeat_body(status);
        self.inner
            .transport
            .request(
                HttpMethod::Post,
                &i::heartbeat_path(dataset, service_id),
                Some(&body),
                None,
            )
            .await?
            .json()
    }

    /// Set `status = offline` but keep the registration (graceful shutdown step one).
    pub async fn drain(&self, dataset: &str, service_id: &str) -> Result<PatchResponse, ClientError> {
        let mut fields = Map::new();
        fields.insert(i::STATUS_FIELD.into(), Value::String(i::STATUS_OFFLINE.into()));
        self.update_agent(dataset, service_id, &fields).await
    }

    /// Revoke heartbeat leases of owned services, or deregister them with `permanent`.
    ///
    /// Selection: explicit `sids`, else every owned sid in `dataset`, else all
    /// owned sids. Renewers are stopped first. Each call is bounded by
    /// `opts.timeout`; failures are collected unless `raise_on_error` is set.
    pub async fn shutdown(&self, opts: &ShutdownOptions) -> Result<ShutdownReport, ClientError> {
        let targets: Vec<(String, String)> = if let Some(sids) = &opts.sids {
            sids.clone()
        } else if let Some(ds) = &opts.dataset {
            self.inner
                .owned
                .list(ds)
                .into_iter()
                .map(|s| (ds.clone(), s))
                .collect()
        } else {
            self.inner
                .owned
                .datasets()
                .into_iter()
                .flat_map(|ds| {
                    self.inner
                        .owned
                        .list(&ds)
                        .into_iter()
                        .map(move |s| (ds.clone(), s))
                })
                .collect()
        };

        for (ds, sid) in &targets {
            self.inner.renewers.remove(ds, sid).await;
        }

        let mut report = ShutdownReport::default();
        for (ds, sid) in targets {
            let call = async {
                if opts.permanent {
                    self.deregister_agent(&ds, &sid).await.map(|_| ())
                } else {
                    let body = json!({ "permanent": false });
                    self.inner
                        .transport
                        .request(
                            HttpMethod::Delete,
                            &i::heartbeat_path(&ds, &sid),
                            Some(&body),
                            None,
                        )
                        .await
                        .map(|_| ())
                }
            };
            let outcome = match tokio::time::timeout(opts.timeout, call).await {
                Ok(r) => r,
                Err(_) => Err(ClientError::Timeout {
                    message: format!("shutdown call for {ds}/{sid} exceeded {:?}", opts.timeout),
                }),
            };
            match outcome {
                Ok(()) => report.revoked.push((ds, sid)),
                Err(e) => {
                    if opts.raise_on_error {
                        return Err(e);
                    }
                    report.errors.push((ds, sid, e.to_string()));
                }
            }
        }
        if let Some(first) = report.errors.first() {
            tracing::warn!(
                "shutdown best-effort cleanup left {} errors; TTL will eventually clean up. First: {first:?}",
                report.errors.len()
            );
        }
        Ok(report)
    }

    /// Partial update of an owned service (`PUT`, top-level upsert).
    ///
    /// A 404 clears local ownership before the error is returned.
    pub async fn update_agent(
        &self,
        dataset: &str,
        service_id: &str,
        fields: &JsonObject,
    ) -> Result<PatchResponse, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let resp = match self
            .inner
            .transport
            .request(
                HttpMethod::Put,
                &i::service_path(dataset, service_id),
                Some(&Value::Object(fields.clone())),
                None,
            )
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_not_found() => {
                self.owned_remove(dataset, service_id).await?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        decode(resp.json()?)
    }

    /// Set the agent card `status` (`online` / `busy` / `offline`).
    ///
    /// The value is validated locally before the ownership check.
    pub async fn set_status(
        &self,
        dataset: &str,
        service_id: &str,
        status: &str,
    ) -> Result<PatchResponse, ClientError> {
        let body = i::build_status_body(status)?;
        self.assert_owned(dataset, service_id)?;
        let resp = match self
            .inner
            .transport
            .request(
                HttpMethod::Put,
                &i::service_path(dataset, service_id),
                Some(&Value::Object(body)),
                None,
            )
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_not_found() => {
                self.owned_remove(dataset, service_id).await?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        decode(resp.json()?)
    }

    /// List services with optional equality filters and pagination.
    ///
    /// Returns flat objects `{id, type, name, description, ...card_fields}`;
    /// card fields win on key conflicts.
    pub async fn list_agents(
        &self,
        dataset: &str,
        opts: &ListOptions,
    ) -> Result<Vec<JsonObject>, ClientError> {
        let params = i::build_filter_params(&opts.filters)?;
        let params = i::apply_pagination(params, opts.page, opts.size)?;
        let resp = self
            .inner
            .transport
            .request(HttpMethod::Get, &i::services_path(dataset), None, Some(&params))
            .await?;
        i::parse_agent_list(&resp)
    }

    /// Fetch one service. Fails with `UnexpectedServiceType` for skill ZIPs.
    pub async fn get_agent(&self, dataset: &str, service_id: &str) -> Result<AgentDetail, ClientError> {
        let resp = self
            .inner
            .transport
            .request(HttpMethod::Get, &i::service_path(dataset, service_id), None, None)
            .await?;
        i::parse_agent_detail(&resp)
    }

    /// Deregister an owned service. Ownership and the endpoint cache are cleared.
    pub async fn deregister_agent(
        &self,
        dataset: &str,
        service_id: &str,
    ) -> Result<DeregisterResponse, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let resp = match self
            .inner
            .transport
            .request(
                HttpMethod::Delete,
                &i::service_path(dataset, service_id),
                None,
                None,
            )
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_not_found() => {
                self.owned_remove(dataset, service_id).await?;
                self.forget_endpoint(dataset, service_id);
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let result: DeregisterResponse = decode(resp.json()?)?;
        self.owned_remove(dataset, service_id).await?;
        self.forget_endpoint(dataset, service_id);
        Ok(result)
    }

    // ── Team-agent helpers ───────────────────────────────────────────────

    /// Register a blank (idle) agent into the pool and seed the endpoint cache.
    pub async fn register_blank_agent(
        &self,
        dataset: &str,
        endpoint: &str,
        service_id: Option<&str>,
        persistent: bool,
    ) -> Result<RegisterResponse, ClientError> {
        let card = i::build_blank_agent_card(endpoint)?;
        let opts = RegisterOptions {
            service_id: service_id.map(str::to_string),
            persistent,
            ..Default::default()
        };
        let result = self.register_agent(dataset, &card, &opts).await?;
        self.cache_endpoint(dataset, &result.service_id, endpoint);
        Ok(result)
    }

    /// Up to `n` idle blank agents (`description=__BLANK__ AND status=online`).
    pub async fn list_idle_blank_agents(
        &self,
        dataset: &str,
        n: usize,
    ) -> Result<Vec<JsonObject>, ClientError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let opts = ListOptions::new()
            .filter("description", i::BLANK_DESCRIPTION_SENTINEL)
            .filter(i::STATUS_FIELD, i::STATUS_ONLINE);
        let mut agents = self.list_agents(dataset, &opts).await?;
        agents.truncate(n);
        Ok(agents)
    }

    /// Fully replace an owned agent's card (not a merge).
    ///
    /// A missing `endpoint` is auto-filled from the cache, then from
    /// `get_agent`, else the call fails locally. After success the endpoint
    /// cache is refreshed and, with `release_lease`, any reservation lease on
    /// the service is released best-effort through the teammate-self endpoint.
    pub async fn replace_agent_card(
        &self,
        dataset: &str,
        service_id: &str,
        agent_card: &JsonObject,
        release_lease: bool,
    ) -> Result<RegisterResponse, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let mut card = agent_card.clone();
        let endpoint = match i::extract_endpoint(&card) {
            Some(e) => e,
            None => {
                let e = self.resolve_endpoint(dataset, service_id).await?;
                card.insert(i::ENDPOINT_FIELD.into(), Value::String(e.clone()));
                e
            }
        };
        let body = i::build_register_agent_body(&card, Some(service_id), true);
        let resp = match self
            .inner
            .transport
            .request(
                HttpMethod::Post,
                &i::a2a_register_path(dataset),
                Some(&Value::Object(body)),
                None,
            )
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_not_found() => {
                self.owned_remove(dataset, service_id).await?;
                self.forget_endpoint(dataset, service_id);
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let result: RegisterResponse = decode(resp.json()?)?;
        self.owned_add(dataset, &result.service_id).await?;
        self.cache_endpoint(dataset, &result.service_id, &endpoint);

        if release_lease {
            match self.release_my_lease(dataset, &result.service_id).await {
                Ok(_) => {}
                Err(e)
                    if e.is_connection() || matches!(e, ClientError::Server { .. }) || e.is_not_found() =>
                {
                    tracing::warn!(
                        "replace_agent_card succeeded but release_my_lease failed for {dataset}/{}: {e}. Lease will TTL-expire.",
                        result.service_id
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Ok(result)
    }

    /// Overwrite an owned agent with the blank card template.
    pub async fn restore_to_blank(
        &self,
        dataset: &str,
        service_id: &str,
    ) -> Result<RegisterResponse, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let endpoint = self.resolve_endpoint(dataset, service_id).await?;
        let card = i::build_blank_agent_card(&endpoint)?;
        self.replace_agent_card(dataset, service_id, &card, true).await
    }

    /// Last-known endpoint: L1 cache, then `get_agent`, else `InvalidArgument`.
    async fn resolve_endpoint(&self, dataset: &str, service_id: &str) -> Result<String, ClientError> {
        let cached = self
            .inner
            .blank_endpoints
            .lock()
            .get(&(dataset.to_string(), service_id.to_string()))
            .cloned();
        if let Some(e) = cached.filter(|e| !e.is_empty()) {
            return Ok(e);
        }
        let detail = self.get_agent(dataset, service_id).await?;
        i::extract_endpoint(&detail.metadata).ok_or_else(|| {
            ClientError::invalid(format!(
                "No 'endpoint' available for service {service_id:?} in dataset {dataset:?}: not in local L1 cache and not in current Agent Card. Provide 'endpoint' explicitly, or call register_blank_agent first to seed the cache."
            ))
        })
    }

    // ── Reservations ─────────────────────────────────────────────────────

    /// Reserve up to `n` idle blank agents for `ttl_seconds`.
    ///
    /// The filter is `description=__BLANK__ AND status=online` merged with
    /// `extra_filters`. Release explicitly with [`Self::release_reservation`].
    pub async fn reserve_blank_agents(
        &self,
        dataset: &str,
        opts: &ReserveOptions,
    ) -> Result<Reservation, ClientError> {
        if opts.ttl_seconds < 1 {
            return Err(ClientError::invalid(format!(
                "ttl_seconds must be >= 1, got {}",
                opts.ttl_seconds
            )));
        }
        let mut filters = Map::new();
        filters.insert(
            "description".into(),
            Value::String(i::BLANK_DESCRIPTION_SENTINEL.into()),
        );
        filters.insert(i::STATUS_FIELD.into(), Value::String(i::STATUS_ONLINE.into()));
        for (k, v) in &opts.extra_filters {
            filters.insert(k.clone(), v.clone());
        }
        let mut body = Map::new();
        body.insert("filters".into(), Value::Object(filters));
        body.insert("n".into(), json!(opts.n));
        body.insert("ttl_seconds".into(), json!(opts.ttl_seconds));
        if let Some(h) = &opts.holder_id {
            body.insert("holder_id".into(), Value::String(h.clone()));
        }
        let resp = self
            .inner
            .transport
            .request(
                HttpMethod::Post,
                &i::reservations_path(dataset),
                Some(&Value::Object(body)),
                None,
            )
            .await?;
        Reservation::from_value(&resp.json()?, dataset)
    }

    /// Release leases held by `reservation`: all of them, or only `service_ids`.
    ///
    /// Idempotent; returns the sids actually released and marks the reservation released.
    pub async fn release_reservation(
        &self,
        reservation: &mut Reservation,
        service_ids: Option<&[String]>,
    ) -> Result<Vec<String>, ClientError> {
        let mut released = Vec::new();
        match service_ids {
            None => {
                let path = i::reservation_holder_path(&reservation.dataset, &reservation.holder_id);
                let body = self
                    .inner
                    .transport
                    .request(HttpMethod::Delete, &path, None, None)
                    .await?
                    .json()?;
                released.extend(released_list(&body));
            }
            Some(sids) => {
                for sid in sids {
                    let path =
                        i::reservation_holder_sid_path(&reservation.dataset, &reservation.holder_id, sid);
                    let body = self
                        .inner
                        .transport
                        .request(HttpMethod::Delete, &path, None, None)
                        .await?
                        .json()?;
                    released.extend(released_list(&body));
                }
            }
        }
        reservation.released = true;
        Ok(released)
    }

    /// Extend every lease under `reservation`. Returns the new `expires_at_unix`.
    ///
    /// A 404 means the leases already expired.
    pub async fn extend_reservation(
        &self,
        reservation: &mut Reservation,
        ttl_seconds: i64,
    ) -> Result<f64, ClientError> {
        if ttl_seconds < 1 {
            return Err(ClientError::invalid(format!(
                "ttl_seconds must be >= 1, got {ttl_seconds}"
            )));
        }
        let path = i::reservation_extend_path(&reservation.dataset, &reservation.holder_id);
        let body = json!({ "ttl_seconds": ttl_seconds });
        let resp = self
            .inner
            .transport
            .request(HttpMethod::Post, &path, Some(&body), None)
            .await?
            .json()?;
        let new_expires = resp
            .get("expires_at_unix")
            .and_then(Value::as_f64)
            .ok_or_else(|| ClientError::decode("extend response is missing expires_at_unix"))?;
        reservation.expires_at_unix = new_expires;
        reservation.ttl_seconds = ttl_seconds;
        Ok(new_expires)
    }

    /// Release any lease on an owned service regardless of holder (teammate-self path).
    ///
    /// Returns true when a lease was released, false when none was held.
    pub async fn release_my_lease(&self, dataset: &str, service_id: &str) -> Result<bool, ClientError> {
        self.assert_owned(dataset, service_id)?;
        let body = self
            .inner
            .transport
            .request(
                HttpMethod::Delete,
                &i::service_lease_path(dataset, service_id),
                None,
                None,
            )
            .await?
            .json()?;
        Ok(body.get("released").is_some_and(truthy))
    }

    // ── Auth self-service (used by the CLI) ──────────────────────────────

    /// `GET /api/auth/whoami`: the principal behind the configured key.
    pub async fn whoami(&self) -> Result<Value, ClientError> {
        self.request_json(HttpMethod::Get, i::AUTH_WHOAMI_PATH, None)
            .await
    }

    /// `GET /api/auth/keys`: keys of the current principal (admins see all).
    pub async fn list_keys(&self) -> Result<Value, ClientError> {
        self.request_json(HttpMethod::Get, i::AUTH_KEYS_ROOT, None).await
    }

    /// `POST /api/auth/keys`: issue a new key for the current principal. The body carries the plaintext token.
    pub async fn create_key(&self, name: &str) -> Result<Value, ClientError> {
        self.request_json(
            HttpMethod::Post,
            i::AUTH_KEYS_ROOT,
            Some(&json!({ "name": name })),
        )
        .await
    }

    /// `DELETE /api/auth/keys/{key_id}`: revoke a key.
    pub async fn revoke_key(&self, key_id: &str) -> Result<Value, ClientError> {
        let path = format!("{}/{}", i::AUTH_KEYS_ROOT, i::encode(key_id));
        self.request_json(HttpMethod::Delete, &path, None).await
    }
}

fn released_list(body: &Value) -> Vec<String> {
    match body.get("released") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Python `bool(value)` for JSON scalars.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
