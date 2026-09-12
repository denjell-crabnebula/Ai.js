// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared application state and configuration.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value, json};

use super::build_jobs::BuildJobs;
use super::cluster_hooks::ClusterHooks;
use super::engines::{BuildEngine, SearchEngine, UnavailableEngine};
use super::services::{SearchService, TaxonomyService};
use super::workers::Workers;
use crate::auth::AuthStore;
use crate::heartbeat::{HeartbeatStore, HeartbeatSweeper};
use crate::register::RegistryService;

/// Where the backend keeps its data and which engines it uses.
pub struct AppConfig {
    /// Data root (`A2X_REGISTRY_HOME` resolution).
    pub home: PathBuf,
    /// `database/` directory holding the datasets.
    pub database_dir: PathBuf,
    /// Optional global `user_config.json` distributed on startup.
    pub global_config_path: Option<PathBuf>,
    /// Auth data directory (`principals.json`, `api_keys.json`, `audit.log`).
    pub auth_data_dir: PathBuf,
    /// `llm_apikey.json` location.
    pub llm_apikey_path: PathBuf,
    /// Front end build to serve at `/` (`A2X_FRONTEND_DIST_DIR`).
    pub frontend_dist: Option<PathBuf>,
    /// Heartbeat sweeper period.
    pub sweeper_period: std::time::Duration,
    pub workers: Workers,
    pub search_engine: Arc<dyn SearchEngine>,
    pub build_engine: Arc<dyn BuildEngine>,
    pub cluster: Option<Arc<dyn ClusterHooks>>,
}

impl AppConfig {
    /// Configuration rooted at `home` with the unavailable engines.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            database_dir: home.join("database"),
            global_config_path: None,
            auth_data_dir: home.join("auth_data"),
            llm_apikey_path: home.join("llm_apikey.json"),
            frontend_dist: None,
            sweeper_period: std::time::Duration::from_secs(5),
            workers: Workers::default(),
            search_engine: Arc::new(UnavailableEngine),
            build_engine: Arc::new(UnavailableEngine),
            cluster: None,
            home,
        }
    }

    /// Configuration from the environment (`A2X_REGISTRY_HOME`,
    /// `A2X_REGISTRY_AUTH_DATA`, `A2X_FRONTEND_DIST_DIR`, worker limits).
    pub fn from_env() -> Self {
        let mut cfg = Self::with_home(a2x_common::paths::get_home());
        cfg.database_dir = a2x_common::paths::database_dir();
        cfg.auth_data_dir = crate::auth::default_data_dir();
        cfg.llm_apikey_path = a2x_common::paths::llm_apikey_path();
        cfg.frontend_dist = ap_support::env::current()
            .get_non_blank("A2X_FRONTEND_DIST_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir());
        cfg.workers = Workers::from_env();
        cfg
    }
}

/// Warmup progress polled by the front end loading screen.
pub struct WarmupState {
    inner: Mutex<Map<String, Value>>,
}

impl Default for WarmupState {
    fn default() -> Self {
        let mut m = Map::new();
        m.insert("ready".into(), Value::Bool(false));
        m.insert("stage".into(), Value::String("starting".into()));
        m.insert("progress".into(), json!(0));
        m.insert("error".into(), Value::Null);
        Self { inner: Mutex::new(m) }
    }
}

impl WarmupState {
    pub fn set(&self, key: &str, value: Value) {
        self.inner.lock().insert(key.to_string(), value);
    }

    pub fn stage(&self, msg: &str, pct: i64) {
        let mut m = self.inner.lock();
        m.insert("stage".into(), Value::String(msg.into()));
        m.insert("progress".into(), json!(pct));
    }

    /// Public fields only: keys starting with `_` are internal handles.
    pub fn public(&self) -> Map<String, Value> {
        self.inner
            .lock()
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn is_ready(&self) -> bool {
        self.inner
            .lock()
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// Everything the routers need, behind an `Arc`.
pub struct AppState {
    pub config_home: PathBuf,
    pub database_dir: PathBuf,
    pub auth_data_dir: PathBuf,
    pub llm_apikey_path: PathBuf,
    pub frontend_dist: Option<PathBuf>,
    pub sweeper_period: std::time::Duration,
    pub registry: Arc<RegistryService>,
    pub search: Arc<SearchService>,
    pub taxonomy: Arc<TaxonomyService>,
    pub build_engine: Arc<dyn BuildEngine>,
    pub build_jobs: Arc<BuildJobs>,
    pub warmup: Arc<WarmupState>,
    pub workers: Arc<Workers>,
    auth: RwLock<Option<Arc<AuthStore>>>,
    heartbeat: RwLock<Option<Arc<HeartbeatStore>>>,
    sweeper: RwLock<Option<Arc<HeartbeatSweeper>>>,
    cluster: RwLock<Option<Arc<dyn ClusterHooks>>>,
    cluster_router: RwLock<Option<axum::Router>>,
    shutdown_hooks: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Arc<Self> {
        let registry = Arc::new(
            RegistryService::new(config.database_dir.clone(), config.global_config_path.clone())
                .with_agent_card_workers(config.workers.agent_card),
        );
        let search = Arc::new(SearchService::new(
            config.search_engine.clone(),
            registry.clone(),
            config.llm_apikey_path.clone(),
            config.workers.llm,
        ));
        Arc::new(Self {
            config_home: config.home,
            database_dir: config.database_dir.clone(),
            auth_data_dir: config.auth_data_dir,
            llm_apikey_path: config.llm_apikey_path,
            frontend_dist: config.frontend_dist,
            sweeper_period: config.sweeper_period,
            registry,
            search,
            taxonomy: Arc::new(TaxonomyService::new(config.database_dir)),
            build_engine: config.build_engine,
            build_jobs: Arc::new(BuildJobs::new()),
            warmup: Arc::new(WarmupState::default()),
            workers: Arc::new(config.workers),
            auth: RwLock::new(None),
            heartbeat: RwLock::new(None),
            sweeper: RwLock::new(None),
            cluster: RwLock::new(config.cluster),
            cluster_router: RwLock::new(None),
            shutdown_hooks: Mutex::new(Vec::new()),
        })
    }

    /// The active auth store, or `None` when auth is not initialized.
    pub fn auth_store(&self) -> Option<Arc<AuthStore>> {
        self.auth.read().clone()
    }

    /// Inject or clear the auth store.
    pub fn set_auth_store(&self, store: Option<Arc<AuthStore>>) {
        *self.auth.write() = store;
    }

    pub fn heartbeat_store(&self) -> Option<Arc<HeartbeatStore>> {
        self.heartbeat.read().clone()
    }

    pub fn set_heartbeat_store(&self, store: Option<Arc<HeartbeatStore>>) {
        *self.heartbeat.write() = store;
    }

    pub fn sweeper(&self) -> Option<Arc<HeartbeatSweeper>> {
        self.sweeper.read().clone()
    }

    pub fn set_sweeper(&self, sweeper: Option<Arc<HeartbeatSweeper>>) {
        *self.sweeper.write() = sweeper;
    }

    pub fn cluster(&self) -> Option<Arc<dyn ClusterHooks>> {
        self.cluster.read().clone()
    }

    pub fn set_cluster(&self, hooks: Option<Arc<dyn ClusterHooks>>) {
        *self.cluster.write() = hooks;
    }

    /// Router serving `/api/cluster/*`, or `None` while dormant.
    pub fn cluster_router(&self) -> Option<axum::Router> {
        self.cluster_router.read().clone()
    }

    pub fn set_cluster_router(&self, router: Option<axum::Router>) {
        *self.cluster_router.write() = router;
    }

    /// Register a callback run once on server shutdown.
    pub fn register_shutdown(&self, hook: Box<dyn Fn() + Send + Sync>) {
        self.shutdown_hooks.lock().push(hook);
    }

    /// Stop background daemons (server shutdown).
    pub fn shutdown(&self) {
        if let Some(s) = self.sweeper.write().take() {
            s.stop();
        }
        for hook in self.shutdown_hooks.lock().drain(..) {
            hook();
        }
    }
}
