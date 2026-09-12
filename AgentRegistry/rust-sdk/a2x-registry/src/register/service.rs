// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `RegistryService`: multi dataset business logic orchestrator.
//!
//! Keeps an in-memory merged view per dataset. `service.json` is a pure
//! output that is never read back. One lock protects the in-memory state
//! (`entries`, `output_cache`, `taxonomy_states`, reservation leases); file
//! I/O always happens outside that lock.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use a2x_common::AuthContext;
use a2x_common::lease::monotonic_now;
use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::agent_card::{build_description, fetch_agent_card};
use super::embedding::{DEFAULT_EMBEDDING_MODEL, embedding_dim as known_embedding_dim};
use super::errors::{RegistryError, Result};
use super::models::{
    AgentCard, DeregisterResponse, DumpMode, GenericServiceData, RegisterA2ARequest, RegisterGenericRequest,
    RegisterResponse, RegistryEntry, RegistryStatus, ServiceType, SkillData, SkillResponse, Source,
    TaxonomyState, UpdateResponse,
};
use super::store::{
    API_CONFIG_FILE, LeaseConfig, REGISTER_CONFIG_FILE, RegistryStore, USER_CONFIG_FILE, generate_service_id,
};
use super::validation::{
    DEFAULT_FORMAT_CONFIG, SUPPORTED_SERVICE_TYPES, ValidationResult, normalize_format_config,
    validate_agent_card, validate_service,
};
use crate::util::{now_wall, py_str};

pub const BUILD_CONFIG_FILE: &str = "build_config.json";
pub const TAXONOMY_FILE: &str = "taxonomy.json";

/// Default reservation TTL in seconds.
pub const DEFAULT_RESERVATION_TTL: i64 = 30;

const DEFAULT_AGENT_CARD_WORKERS: usize = 10;

/// Resolve `A2X_REGISTRY_AGENT_CARD_WORKERS` with warning and fallback.
pub fn agent_card_workers_from_env() -> usize {
    crate::backend::workers::env_workers("A2X_REGISTRY_AGENT_CARD_WORKERS", DEFAULT_AGENT_CARD_WORKERS)
}

/// Callback fired when `service.json` content changed (vector index sync).
pub trait ServiceChangeListener: Send + Sync {
    fn on_service_changed(&self, dataset: &str);
}

impl<F: Fn(&str) + Send + Sync> ServiceChangeListener for F {
    fn on_service_changed(&self, dataset: &str) {
        self(dataset)
    }
}

/// Kind of local CRUD reported to [`MutationHook`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOp {
    Register,
    Update,
    Deregister,
}

impl MutationOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            MutationOp::Register => "register",
            MutationOp::Update => "update",
            MutationOp::Deregister => "deregister",
        }
    }
}

/// Hook fired after every successful local CRUD (cluster replication).
pub trait MutationHook: Send + Sync {
    fn on_mutation(&self, dataset: &str, service_id: &str, op: MutationOp, entry: Option<&RegistryEntry>);
}

/// Predicate installed by the heartbeat module.
pub trait UnhealthyCheck: Send + Sync {
    fn is_unhealthy(&self, dataset: &str, service_id: &str) -> bool;
}

impl<F: Fn(&str, &str) -> bool + Send + Sync> UnhealthyCheck for F {
    fn is_unhealthy(&self, dataset: &str, service_id: &str) -> bool {
        self(dataset, service_id)
    }
}

/// What [`RegistryService::reserve_services`] claims.
#[derive(Clone, Debug)]
pub struct ReserveRequest<'a> {
    /// Dataset to reserve from.
    pub dataset: &'a str,
    /// Filters the reserved services must match.
    pub filters: &'a Map<String, Value>,
    /// Maximum number of services to reserve.
    pub n: i64,
    /// Lease duration in seconds.
    pub ttl_seconds: i64,
    /// Holder id; forced to the caller's principal id when a caller is given.
    pub holder_id: Option<String>,
}

/// In-memory reservation lease (SQS visibility timeout style).
#[derive(Clone, Debug)]
struct Lease {
    holder_id: String,
    expires_at: f64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, IndexMap<String, RegistryEntry>>,
    output_cache: HashMap<String, Vec<Value>>,
    taxonomy_states: HashMap<String, TaxonomyState>,
    format_configs: HashMap<String, HashMap<String, String>>,
    leases: HashMap<(String, String), Lease>,
}

/// Multi dataset registry service.
pub struct RegistryService {
    database_dir: PathBuf,
    global_config_path: Option<PathBuf>,
    allowed_a2a_versions: Option<HashSet<String>>,
    stores: Mutex<HashMap<String, Arc<RegistryStore>>>,
    inner: Mutex<Inner>,
    auth_required_cache: Mutex<HashMap<String, bool>>,
    lease_config_cache: Mutex<HashMap<String, LeaseConfig>>,
    on_service_changed: RwLock<Option<Arc<dyn ServiceChangeListener>>>,
    on_mutation: RwLock<Option<Arc<dyn MutationHook>>>,
    unhealthy_check: RwLock<Option<Arc<dyn UnhealthyCheck>>>,
    agent_card_workers: usize,
}

impl RegistryService {
    /// Create a service over `database_dir`. `global_config_path` is an
    /// optional global `user_config.json` distributed on startup.
    pub fn new(database_dir: impl Into<PathBuf>, global_config_path: Option<PathBuf>) -> Self {
        Self {
            database_dir: database_dir.into(),
            global_config_path,
            allowed_a2a_versions: None,
            stores: Mutex::new(HashMap::new()),
            inner: Mutex::new(Inner::default()),
            auth_required_cache: Mutex::new(HashMap::new()),
            lease_config_cache: Mutex::new(HashMap::new()),
            on_service_changed: RwLock::new(None),
            on_mutation: RwLock::new(None),
            unhealthy_check: RwLock::new(None),
            agent_card_workers: agent_card_workers_from_env(),
        }
    }

    /// Legacy knob: a fixed A2A version allow list overriding the per
    /// dataset `register_config.json` for a2a entries.
    pub fn with_allowed_a2a_versions(mut self, versions: Option<HashSet<String>>) -> Self {
        self.allowed_a2a_versions = versions;
        self
    }

    /// Number of concurrent agent card fetches during startup.
    pub fn with_agent_card_workers(mut self, workers: usize) -> Self {
        self.agent_card_workers = workers.max(1);
        self
    }

    pub fn database_dir(&self) -> &Path {
        &self.database_dir
    }

    // -----------------------------------------------------------------------
    // Startup
    // -----------------------------------------------------------------------

    /// Initialize all datasets. Returns `{dataset: TaxonomyState}`.
    pub async fn startup(&self) -> Result<BTreeMap<String, TaxonomyState>> {
        if let Some(path) = &self.global_config_path {
            if path.exists() {
                self.distribute_global_config(path);
            }
        }
        let datasets = self.discover_datasets();
        tracing::info!(
            "Discovered {} datasets with registration config: {:?}",
            datasets.len(),
            datasets
        );

        // Phase 1: load config files and collect URL entries to fetch.
        let mut url_entries: Vec<(String, String, String)> = Vec::new();
        for dataset in &datasets {
            let store = self.get_store(dataset)?;
            let user_entries = store.load_user_config();
            let api_entries = store.load_api_config();
            let mut merged: IndexMap<String, RegistryEntry> = IndexMap::new();
            for e in user_entries.into_iter().chain(api_entries) {
                if let Some(vr) = self.validate_entry(dataset, &e, true)? {
                    if !vr.valid {
                        tracing::warn!(
                            "Skipping invalid {} '{}' in {}: {}",
                            e.r#type,
                            e.service_id,
                            dataset,
                            vr.errors.join("; ")
                        );
                        continue;
                    }
                }
                if let Some(url) = &e.agent_card_url {
                    url_entries.push((dataset.clone(), e.service_id.clone(), url.clone()));
                }
                merged.insert(e.service_id.clone(), e);
            }
            for e in store.load_skills() {
                if merged.contains_key(&e.service_id) {
                    continue;
                }
                if let Some(vr) = self.validate_entry(dataset, &e, false)? {
                    if !vr.valid {
                        tracing::warn!(
                            "Skipping invalid skill '{}' in {}: {}",
                            e.service_id,
                            dataset,
                            vr.errors.join("; ")
                        );
                        continue;
                    }
                }
                merged.insert(e.service_id.clone(), e);
            }
            self.inner.lock().entries.insert(dataset.clone(), merged);
        }

        // Phase 2: parallel fetch of agent_card_urls, then re-validate.
        if !url_entries.is_empty() {
            self.fetch_agent_cards_parallel(&url_entries).await;
            for (dataset, sid, _) in &url_entries {
                let entry = self.get_entry(dataset, sid);
                let Some(entry) = entry else { continue };
                if entry.agent_card.is_none() {
                    continue;
                }
                if let Some(vr) = self.validate_entry(dataset, &entry, false)? {
                    if !vr.valid {
                        tracing::warn!(
                            "Dropping invalid A2A '{}' in {} after fetch: {}",
                            sid,
                            dataset,
                            vr.errors.join("; ")
                        );
                        if let Some(ds) = self.inner.lock().entries.get_mut(dataset) {
                            ds.shift_remove(sid);
                        }
                    }
                }
            }
        }

        // Phase 3: generate output and compute taxonomy states.
        let mut result = BTreeMap::new();
        for dataset in &datasets {
            let store = self.get_store(dataset)?;
            let api_entries: Vec<RegistryEntry> = self
                .inner
                .lock()
                .entries
                .get(dataset)
                .map(|ds| {
                    ds.values()
                        .filter(|e| e.source == Source::ApiConfig)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if !api_entries.is_empty() {
                store.save_api_batch(&api_entries)?;
            }
            self.regenerate_output(dataset)?;
            let state = self.init_taxonomy_state(dataset);
            let count = self
                .inner
                .lock()
                .output_cache
                .get(dataset)
                .map(|o| o.len())
                .unwrap_or(0);
            tracing::info!("Dataset '{}': {} services, taxonomy={}", dataset, count, state);
            result.insert(dataset.clone(), state);
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Register
    // -----------------------------------------------------------------------

    /// Initialize a dataset with default embedding model and formats when
    /// it has no `vector_config.json` yet. Idempotent.
    fn ensure_dataset_initialized(&self, dataset: &str) -> Result<()> {
        if self
            .database_dir
            .join(dataset)
            .join("vector_config.json")
            .exists()
        {
            return Ok(());
        }
        self.init_dataset_files(dataset, DEFAULT_EMBEDDING_MODEL, &DEFAULT_FORMAT_CONFIG, false)?;
        tracing::info!("Auto-initialized dataset '{}' with defaults", dataset);
        Ok(())
    }

    /// Register a generic service. `caller` is `None` on the anonymous path.
    pub fn register_generic(
        &self,
        req: &RegisterGenericRequest,
        caller: Option<&AuthContext>,
    ) -> Result<RegisterResponse> {
        Self::assert_can_register(caller)?;
        let dataset = &req.dataset;
        let payload = serde_json::json!({
            "name": req.name, "description": req.description,
            "url": req.url, "inputSchema": req.input_schema,
        });
        self.require_valid(dataset, "generic", &payload)?;
        let service_id = req
            .service_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| generate_service_id("generic", &req.name));
        let entry = RegistryEntry {
            service_id,
            r#type: ServiceType::Generic,
            source: if req.persistent {
                Source::ApiConfig
            } else {
                Source::Ephemeral
            },
            service_data: Some(GenericServiceData {
                name: req.name.clone(),
                description: req.description.clone(),
                input_schema: req.input_schema.clone(),
                url: Some(req.url.clone()),
            }),
            agent_card: None,
            agent_card_url: None,
            skill_data: None,
            owner_id: caller.map(|c| c.principal_id.clone()),
            lease_ttl: req.lease_ttl,
        };
        self.do_register(dataset, entry, req.persistent)
    }

    /// Register an A2A agent from a full card or a card URL (fetched).
    pub async fn register_a2a(
        &self,
        req: &RegisterA2ARequest,
        caller: Option<&AuthContext>,
    ) -> Result<RegisterResponse> {
        Self::assert_can_register(caller)?;
        let (card, url) = if let Some(card) = &req.agent_card {
            (card.clone(), None)
        } else if let Some(url) = &req.agent_card_url {
            (fetch_agent_card(url).await?, Some(url.clone()))
        } else {
            return Err(RegistryError::Invalid(
                "Either agent_card or agent_card_url must be provided".into(),
            ));
        };
        self.register_a2a_resolved(req, card, url, caller)
    }

    /// Register an A2A agent whose card is already resolved. `agent_card_url`
    /// records the origin when the card was fetched.
    pub fn register_a2a_resolved(
        &self,
        req: &RegisterA2ARequest,
        agent_card: AgentCard,
        agent_card_url: Option<String>,
        caller: Option<&AuthContext>,
    ) -> Result<RegisterResponse> {
        Self::assert_can_register(caller)?;
        let dataset = &req.dataset;
        self.require_valid(dataset, "a2a", &Value::Object(agent_card.to_json(DumpMode::Full)))?;
        let service_id = req
            .service_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| generate_service_id("agent", &agent_card.name));
        let entry = RegistryEntry {
            service_id,
            r#type: ServiceType::A2a,
            source: if req.persistent {
                Source::ApiConfig
            } else {
                Source::Ephemeral
            },
            service_data: None,
            agent_card: Some(agent_card),
            agent_card_url,
            skill_data: None,
            owner_id: caller.map(|c| c.principal_id.clone()),
            lease_ttl: req.lease_ttl,
        };
        self.do_register(dataset, entry, req.persistent)
    }

    /// Register multiple entries at once (single file write).
    pub fn register_batch(&self, entries: &[RegistryEntry], dataset: &str, persistent: bool) -> Result<()> {
        let source = if persistent {
            Source::ApiConfig
        } else {
            Source::Ephemeral
        };
        let all_api: Vec<RegistryEntry> = {
            let mut inner = self.inner.lock();
            let ds = inner.entries.entry(dataset.to_string()).or_default();
            for e in entries {
                let mut copy = e.clone();
                copy.source = source;
                ds.insert(copy.service_id.clone(), copy);
            }
            if persistent {
                ds.values()
                    .filter(|e| e.source == Source::ApiConfig)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        };
        if persistent && !all_api.is_empty() {
            self.get_store(dataset)?.save_api_batch(&all_api)?;
        }
        self.regenerate_output(dataset)?;
        self.mark_taxonomy_stale(dataset);
        Ok(())
    }

    /// Upload a skill ZIP, extract to `skills/{name}/` and register it.
    pub fn register_skill(
        &self,
        dataset: &str,
        zip_bytes: &[u8],
        caller: Option<&AuthContext>,
    ) -> Result<SkillResponse> {
        Self::assert_can_register(caller)?;
        self.ensure_dataset_initialized(dataset)?;
        self.require_type_allowed(dataset, "skill")?;
        let store = self.get_store(dataset)?;
        let skill_data = store.save_skill_zip(zip_bytes)?;
        self.require_valid(dataset, "skill", &Value::Object(skill_data.to_json()))?;
        let service_id = generate_service_id("skill", &skill_data.name);
        let entry = RegistryEntry {
            service_id: service_id.clone(),
            r#type: ServiceType::Skill,
            source: Source::SkillFolder,
            service_data: None,
            agent_card: None,
            agent_card_url: None,
            skill_data: Some(skill_data.clone()),
            owner_id: caller.map(|c| c.principal_id.clone()),
            lease_ttl: None,
        };
        let status = {
            let mut inner = self.inner.lock();
            let ds = inner.entries.entry(dataset.to_string()).or_default();
            if let (Some(existing), Some(c)) = (ds.get(&service_id), caller) {
                Self::assert_owner(existing, c)?;
            }
            let status = if ds.contains_key(&service_id) {
                "updated"
            } else {
                "registered"
            };
            ds.insert(service_id.clone(), entry.clone());
            status
        };
        self.regenerate_output(dataset)?;
        self.mark_taxonomy_stale(dataset);
        self.emit_mutation(
            dataset,
            &service_id,
            if status == "updated" {
                MutationOp::Update
            } else {
                MutationOp::Register
            },
            Some(&entry),
        );
        Ok(SkillResponse {
            name: skill_data.name,
            dataset: dataset.to_string(),
            service_id,
            status: status.to_string(),
        })
    }

    /// Remove a skill folder (moved to `removed_skills/`) and its entry.
    pub fn deregister_skill(
        &self,
        dataset: &str,
        name: &str,
        caller: Option<&AuthContext>,
    ) -> Result<SkillResponse> {
        let service_id = generate_service_id("skill", name);
        {
            let mut inner = self.inner.lock();
            let Some(ds) = inner.entries.get_mut(dataset) else {
                return Ok(SkillResponse {
                    name: name.into(),
                    dataset: dataset.into(),
                    service_id: String::new(),
                    status: "not_found".into(),
                });
            };
            let Some(existing) = ds.get(&service_id) else {
                return Ok(SkillResponse {
                    name: name.into(),
                    dataset: dataset.into(),
                    service_id: String::new(),
                    status: "not_found".into(),
                });
            };
            if let Some(c) = caller {
                Self::assert_owner(existing, c)?;
            }
            ds.shift_remove(&service_id);
        }
        self.get_store(dataset)?.remove_skill(name)?;
        self.regenerate_output(dataset)?;
        self.mark_taxonomy_stale(dataset);
        self.emit_mutation(dataset, &service_id, MutationOp::Deregister, None);
        Ok(SkillResponse {
            name: name.into(),
            dataset: dataset.into(),
            service_id,
            status: "deleted".into(),
        })
    }

    /// Pack a skill folder into ZIP bytes.
    pub fn get_skill_zip(&self, dataset: &str, name: &str) -> Result<Vec<u8>> {
        self.get_store(dataset)?.get_skill_zip(name)
    }

    // -----------------------------------------------------------------------
    // Update (partial field merge)
    // -----------------------------------------------------------------------

    const GENERIC_UPDATE_FIELDS: &'static [&'static str] = &["name", "description", "inputSchema", "url"];
    const SKILL_UPDATE_FIELDS: &'static [&'static str] = &["name", "description", "license"];

    /// Partially update a service by top level field upsert.
    ///
    /// No format validation runs. `name` or `description` changes mark the
    /// taxonomy stale. `user_config` entries are rejected. Skill updates
    /// rewrite `SKILL.md` and rename the folder on a name change.
    pub fn update_service(
        &self,
        dataset: &str,
        service_id: &str,
        updates: &Map<String, Value>,
        caller: Option<&AuthContext>,
    ) -> Result<UpdateResponse> {
        let entry = {
            let inner = self.inner.lock();
            let entry = inner
                .entries
                .get(dataset)
                .and_then(|ds| ds.get(service_id))
                .cloned()
                .ok_or_else(|| {
                    RegistryError::NotFound(format!(
                        "Service '{service_id}' not found in dataset '{dataset}'"
                    ))
                })?;
            if entry.source == Source::UserConfig {
                return Err(RegistryError::Invalid(
                    "Cannot update user_config entries via API. Edit user_config.json directly and restart."
                        .into(),
                ));
            }
            if let Some(c) = caller {
                Self::assert_owner(&entry, c)?;
            }
            entry
        };

        let (new_entry, changed) = match entry.r#type {
            ServiceType::Generic => Self::apply_generic_updates(&entry, updates)?,
            ServiceType::A2a => Self::apply_a2a_updates(&entry, updates)?,
            ServiceType::Skill => self.apply_skill_updates(dataset, &entry, updates)?,
        };

        {
            let mut inner = self.inner.lock();
            let ds = inner.entries.entry(dataset.to_string()).or_default();
            if !ds.contains_key(service_id) {
                return Err(RegistryError::NotFound(format!(
                    "Service '{service_id}' was removed during update in dataset '{dataset}'"
                )));
            }
            ds.insert(service_id.to_string(), new_entry.clone());
        }

        if new_entry.source == Source::ApiConfig {
            self.get_store(dataset)?.save_api_entry(&new_entry)?;
        }
        self.regenerate_output(dataset)?;
        let taxonomy_affected = changed.contains("name") || changed.contains("description");
        if taxonomy_affected {
            self.mark_taxonomy_stale(dataset);
        }
        self.emit_mutation(dataset, service_id, MutationOp::Update, Some(&new_entry));
        let mut changed_fields: Vec<String> = changed.into_iter().collect();
        changed_fields.sort();
        Ok(UpdateResponse {
            service_id: service_id.to_string(),
            dataset: dataset.to_string(),
            status: "updated".into(),
            changed_fields,
            taxonomy_affected,
        })
    }

    fn check_unknown(updates: &Map<String, Value>, allowed: &[&str], kind: &str) -> Result<()> {
        let mut unknown: Vec<&String> = updates
            .keys()
            .filter(|k| !allowed.contains(&k.as_str()))
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort();
        let mut allowed_sorted: Vec<&str> = allowed.to_vec();
        allowed_sorted.sort();
        Err(RegistryError::Invalid(format!(
            "Unknown {kind} fields: {}. Allowed: {}",
            py_list(unknown.iter().map(|s| s.as_str())),
            py_list(allowed_sorted.iter().copied())
        )))
    }

    fn changed_keys(current: &Map<String, Value>, updates: &Map<String, Value>) -> HashSet<String> {
        updates
            .iter()
            .filter(|(k, v)| current.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn apply_generic_updates(
        entry: &RegistryEntry,
        updates: &Map<String, Value>,
    ) -> Result<(RegistryEntry, HashSet<String>)> {
        Self::check_unknown(updates, Self::GENERIC_UPDATE_FIELDS, "generic")?;
        let mut current = entry
            .service_data
            .as_ref()
            .map(|s| s.to_json())
            .unwrap_or_default();
        let changed = Self::changed_keys(&current, updates);
        for (k, v) in updates {
            current.insert(k.clone(), v.clone());
        }
        let new_data: GenericServiceData = serde_json::from_value(Value::Object(current))
            .map_err(|e| RegistryError::Invalid(format!("invalid generic update: {e}")))?;
        let mut new_entry = entry.clone();
        new_entry.service_data = Some(new_data);
        Ok((new_entry, changed))
    }

    fn apply_a2a_updates(
        entry: &RegistryEntry,
        updates: &Map<String, Value>,
    ) -> Result<(RegistryEntry, HashSet<String>)> {
        let Some(card) = &entry.agent_card else {
            return Err(RegistryError::Invalid(
                "Cannot update a2a entry with no resolved agent_card".into(),
            ));
        };
        let mut current = card.to_json(DumpMode::Full);
        let changed = Self::changed_keys(&current, updates);
        for (k, v) in updates {
            current.insert(k.clone(), v.clone());
        }
        let new_card = AgentCard::from_value(Value::Object(current))
            .map_err(|e| RegistryError::Invalid(format!("invalid agent card update: {e}")))?;
        let mut new_entry = entry.clone();
        new_entry.agent_card = Some(new_card);
        Ok((new_entry, changed))
    }

    fn apply_skill_updates(
        &self,
        dataset: &str,
        entry: &RegistryEntry,
        updates: &Map<String, Value>,
    ) -> Result<(RegistryEntry, HashSet<String>)> {
        Self::check_unknown(updates, Self::SKILL_UPDATE_FIELDS, "skill")?;
        let Some(skill) = &entry.skill_data else {
            return Err(RegistryError::Invalid("skill entry has no skill_data".into()));
        };
        let current = skill.to_json();
        let changed = Self::changed_keys(&current, updates);
        let mut merged = current.clone();
        for (k, v) in updates {
            merged.insert(k.clone(), v.clone());
        }
        let store = self.get_store(dataset)?;
        let old_name = skill.name.clone();
        let new_name = py_str(&merged["name"]);
        if new_name != old_name {
            store.rename_skill(&old_name, &new_name)?;
            merged.insert("skill_path".into(), Value::String(format!("skills/{new_name}")));
        }
        let mut md_updates: IndexMap<String, String> = IndexMap::new();
        for k in ["name", "description", "license"] {
            if changed.contains(k) {
                if let Some(v) = merged.get(k).filter(|v| !v.is_null()) {
                    md_updates.insert(k.to_string(), py_str(v));
                }
            }
        }
        if !md_updates.is_empty() {
            store.update_skill_md(&new_name, &md_updates)?;
        }
        let new_data: SkillData = serde_json::from_value(Value::Object(merged))
            .map_err(|e| RegistryError::Invalid(format!("invalid skill update: {e}")))?;
        let mut new_entry = entry.clone();
        new_entry.skill_data = Some(new_data);
        Ok((new_entry, changed))
    }

    fn do_register(&self, dataset: &str, entry: RegistryEntry, persistent: bool) -> Result<RegisterResponse> {
        self.ensure_dataset_initialized(dataset)?;
        let status = {
            let mut inner = self.inner.lock();
            let ds = inner.entries.entry(dataset.to_string()).or_default();
            let status = if ds.contains_key(&entry.service_id) {
                "updated"
            } else {
                "registered"
            };
            ds.insert(entry.service_id.clone(), entry.clone());
            status
        };
        if persistent {
            self.get_store(dataset)?.save_api_entry(&entry)?;
        }
        self.regenerate_output(dataset)?;
        self.mark_taxonomy_stale(dataset);
        self.emit_mutation(
            dataset,
            &entry.service_id,
            if status == "updated" {
                MutationOp::Update
            } else {
                MutationOp::Register
            },
            Some(&entry),
        );
        Ok(RegisterResponse {
            service_id: entry.service_id,
            dataset: dataset.to_string(),
            status: status.to_string(),
            lease_ttl: None,
            lease_expires_at: None,
        })
    }

    // -----------------------------------------------------------------------
    // Deregister
    // -----------------------------------------------------------------------

    /// Deregister a service. Rejects `user_config` and `skill_folder`
    /// sources; enforces owner or admin when `caller` is given.
    pub fn deregister(
        &self,
        dataset: &str,
        service_id: &str,
        caller: Option<&AuthContext>,
    ) -> Result<DeregisterResponse> {
        let source =
            {
                let mut inner = self.inner.lock();
                let Some(entry) = inner.entries.get(dataset).and_then(|ds| ds.get(service_id)) else {
                    return Err(RegistryError::NotFound(format!(
                        "Service '{service_id}' not found in dataset '{dataset}'"
                    )));
                };
                match entry.source {
                    Source::UserConfig => {
                        return Err(RegistryError::Invalid(
                            "Cannot deregister user_config entries via API. Edit user_config.json instead."
                                .into(),
                        ));
                    }
                    Source::SkillFolder => return Err(RegistryError::Invalid(
                        "Cannot deregister skill entries via generic API. Use DELETE /skills/{name} instead."
                            .into(),
                    )),
                    _ => {}
                }
                if let Some(c) = caller {
                    Self::assert_owner(entry, c)?;
                }
                let source = entry.source;
                if let Some(ds) = inner.entries.get_mut(dataset) {
                    ds.shift_remove(service_id);
                }
                source
            };
        if source == Source::ApiConfig {
            self.get_store(dataset)?.remove_api_entry(service_id)?;
        }
        self.regenerate_output(dataset)?;
        self.mark_taxonomy_stale(dataset);
        self.emit_mutation(dataset, service_id, MutationOp::Deregister, None);
        Ok(DeregisterResponse {
            service_id: service_id.to_string(),
            status: "deregistered".into(),
        })
    }

    // ----- Authorization helpers ------------------------------------------

    /// Reject registration by `user` role principals.
    fn assert_can_register(caller: Option<&AuthContext>) -> Result<()> {
        let Some(c) = caller else { return Ok(()) };
        if c.is_admin() || c.role == a2x_common::Role::Provider {
            return Ok(());
        }
        Err(RegistryError::Permission(format!(
            "Role '{}' cannot register services; need admin or provider",
            c.role
        )))
    }

    /// Admin short circuits. Unclaimed entries (`owner_id=None`) are admin
    /// only. Otherwise `owner_id` must equal the caller's principal id.
    fn assert_owner(entry: &RegistryEntry, caller: &AuthContext) -> Result<()> {
        if caller.is_admin() {
            return Ok(());
        }
        match &entry.owner_id {
            None => Err(RegistryError::Permission(
                "System / unclaimed service: admin role required to mutate".into(),
            )),
            Some(o) if *o != caller.principal_id => Err(RegistryError::Permission(format!(
                "Service '{}' is owned by another principal",
                entry.service_id
            ))),
            _ => Ok(()),
        }
    }

    // -----------------------------------------------------------------------
    // Taxonomy state
    // -----------------------------------------------------------------------

    /// Cached taxonomy state, or `None` if the dataset is not managed here.
    pub fn get_taxonomy_state(&self, dataset: &str) -> Option<TaxonomyState> {
        self.inner.lock().taxonomy_states.get(dataset).copied()
    }

    /// Taxonomy state, resolving `Stale` by re-checking the hash.
    pub fn check_taxonomy_state(&self, dataset: &str) -> Option<TaxonomyState> {
        let state = self.get_taxonomy_state(dataset)?;
        if state != TaxonomyState::Stale {
            return Some(state);
        }
        let new_state = self.compute_taxonomy_state(dataset);
        self.inner
            .lock()
            .taxonomy_states
            .insert(dataset.to_string(), new_state);
        tracing::info!("Dataset '{}': taxonomy re-checked, state={}", dataset, new_state);
        Some(new_state)
    }

    // -----------------------------------------------------------------------
    // Query (read only, snapshots)
    // -----------------------------------------------------------------------

    /// Cached `service.json` output for a dataset (no liveness filtering).
    pub fn list_services(&self, dataset: &str) -> Vec<Value> {
        self.inner
            .lock()
            .output_cache
            .get(dataset)
            .cloned()
            .unwrap_or_default()
    }

    /// All entries for a dataset in insertion order.
    pub fn list_entries(&self, dataset: &str) -> Vec<RegistryEntry> {
        self.inner
            .lock()
            .entries
            .get(dataset)
            .map(|ds| ds.values().cloned().collect())
            .unwrap_or_default()
    }

    /// True if the heartbeat module marked this service unhealthy.
    pub fn is_unhealthy(&self, dataset: &str, service_id: &str) -> bool {
        let guard = self.unhealthy_check.read();
        match guard.as_ref() {
            Some(cb) => cb.is_unhealthy(dataset, service_id),
            None => false,
        }
    }

    /// Install the heartbeat unhealthy predicate.
    pub fn set_unhealthy_check(&self, callback: Option<Arc<dyn UnhealthyCheck>>) {
        *self.unhealthy_check.write() = callback;
    }

    /// Install the cluster replication hook.
    pub fn set_on_mutation(&self, callback: Option<Arc<dyn MutationHook>>) {
        *self.on_mutation.write() = callback;
    }

    /// Register a callback invoked when `service.json` content changes.
    pub fn set_on_service_changed(&self, callback: Option<Arc<dyn ServiceChangeListener>>) {
        *self.on_service_changed.write() = callback;
    }

    fn emit_mutation(&self, dataset: &str, service_id: &str, op: MutationOp, entry: Option<&RegistryEntry>) {
        let hook = self.on_mutation.read().clone();
        if let Some(h) = hook {
            h.on_mutation(dataset, service_id, op, entry);
        }
    }

    /// A single entry.
    pub fn get_entry(&self, dataset: &str, service_id: &str) -> Option<RegistryEntry> {
        self.inner
            .lock()
            .entries
            .get(dataset)
            .and_then(|ds| ds.get(service_id))
            .cloned()
    }

    /// Status summary, optionally for one dataset.
    pub fn get_status(&self, dataset: Option<&str>) -> RegistryStatus {
        let inner = self.inner.lock();
        let to_check: Vec<String> = match dataset {
            Some(d) => vec![d.to_string()],
            None => inner.entries.keys().cloned().collect(),
        };
        let mut total = 0;
        let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
        for ds in &to_check {
            if let Some(entries) = inner.entries.get(ds) {
                for e in entries.values() {
                    total += 1;
                    *by_source.entry(e.source.as_str().to_string()).or_insert(0) += 1;
                }
            }
        }
        let mut datasets: Vec<String> = inner.entries.keys().cloned().collect();
        datasets.sort();
        RegistryStatus {
            total_services: total,
            by_source,
            datasets,
        }
    }

    pub fn dataset_dir(&self, dataset: &str) -> PathBuf {
        self.database_dir.join(dataset)
    }

    pub fn service_json_path(&self, dataset: &str) -> PathBuf {
        self.dataset_dir(dataset).join("service.json")
    }

    pub fn query_path(&self, dataset: &str) -> PathBuf {
        self.dataset_dir(dataset).join("query").join("query.json")
    }

    pub fn taxonomy_dir(&self, dataset: &str) -> PathBuf {
        self.dataset_dir(dataset).join("taxonomy")
    }

    pub fn taxonomy_path(&self, dataset: &str) -> PathBuf {
        self.taxonomy_dir(dataset).join("taxonomy.json")
    }

    pub fn class_path(&self, dataset: &str) -> PathBuf {
        self.taxonomy_dir(dataset).join("class.json")
    }

    /// Shared ChromaDB directory (one per database root).
    pub fn chroma_dir(&self) -> PathBuf {
        self.database_dir.join("chroma")
    }

    /// True if `<database_dir>/<name>` is a directory.
    pub fn dataset_exists(&self, name: &str) -> bool {
        self.database_dir.exists() && self.database_dir.join(name).is_dir()
    }

    /// All dataset directory names, alphabetical.
    pub fn list_datasets(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.database_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        names
    }

    /// Datasets on disk that have a `service.json`, with service and query
    /// counts.
    pub fn list_datasets_with_counts(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for name in self.list_datasets() {
            let d = self.database_dir.join(&name);
            let service_file = d.join("service.json");
            if !service_file.exists() {
                continue;
            }
            let svc_count = count_json_items(&service_file);
            let query_file = d.join("query").join("query.json");
            let q_count = if query_file.exists() {
                count_json_items(&query_file)
            } else {
                0
            };
            out.push(serde_json::json!({
                "name": name, "service_count": svc_count, "query_count": q_count,
            }));
        }
        out
    }

    // -----------------------------------------------------------------------
    // Reservation leases (in-memory)
    // -----------------------------------------------------------------------

    fn sweep_expired_leases_locked(inner: &mut Inner, now: f64) {
        inner.leases.retain(|_, l| l.expires_at > now);
    }

    /// True if there is an unexpired lease on `(dataset, service_id)`.
    pub fn is_leased(&self, dataset: &str, service_id: &str) -> bool {
        let mut inner = self.inner.lock();
        Self::sweep_expired_leases_locked(&mut inner, monotonic_now());
        inner
            .leases
            .contains_key(&(dataset.to_string(), service_id.to_string()))
    }

    /// Atomically filter and claim up to `n` unleased matching services.
    ///
    /// Returns `(holder_id, expires_at_unix, reservations)`. With a `caller`
    /// the holder id is forced to the caller's principal id.
    pub fn reserve_services(
        &self,
        dataset: &str,
        filters: &Map<String, Value>,
        n: i64,
        ttl_seconds: i64,
        holder_id: Option<String>,
        caller: Option<&AuthContext>,
    ) -> Result<(String, f64, Vec<Value>)> {
        let request = ReserveRequest {
            dataset,
            filters,
            n,
            ttl_seconds,
            holder_id,
        };
        self.reserve_services_at(request, caller, monotonic_now(), now_wall())
    }

    /// [`Self::reserve_services`] with explicit clocks for tests.
    pub fn reserve_services_at(
        &self,
        request: ReserveRequest<'_>,
        caller: Option<&AuthContext>,
        now_mono: f64,
        now_wall: f64,
    ) -> Result<(String, f64, Vec<Value>)> {
        let ReserveRequest {
            dataset,
            filters,
            n,
            ttl_seconds,
            holder_id,
        } = request;
        if n < 0 {
            return Err(RegistryError::Invalid(format!("n must be >= 0, got {n}")));
        }
        if ttl_seconds < 1 {
            return Err(RegistryError::Invalid(format!(
                "ttl_seconds must be >= 1, got {ttl_seconds}"
            )));
        }
        let holder_id = match (caller, holder_id) {
            (Some(c), _) => c.principal_id.clone(),
            (None, Some(h)) => h,
            (None, None) => format!("holder_{}", uuid::Uuid::new_v4().simple()),
        };
        let expires_at_mono = now_mono + ttl_seconds as f64;
        let expires_at_wall = now_wall + ttl_seconds as f64;

        let candidates: Vec<RegistryEntry> = {
            let inner = self.inner.lock();
            let mut v: Vec<RegistryEntry> = inner
                .entries
                .get(dataset)
                .map(|ds| ds.values().cloned().collect())
                .unwrap_or_default();
            v.sort_by(|a, b| a.service_id.cmp(&b.service_id));
            v
        };
        let unhealthy: HashSet<String> = candidates
            .iter()
            .filter(|e| self.is_unhealthy(dataset, &e.service_id))
            .map(|e| e.service_id.clone())
            .collect();

        let mut inner = self.inner.lock();
        Self::sweep_expired_leases_locked(&mut inner, now_mono);
        let wrapped_by_id: HashMap<String, Value> = inner
            .output_cache
            .get(dataset)
            .map(|o| {
                o.iter()
                    .filter_map(|s| {
                        s.get("id")
                            .and_then(Value::as_str)
                            .map(|id| (id.to_string(), s.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut claimed: Vec<Value> = Vec::new();
        for entry in &candidates {
            if claimed.len() as i64 >= n {
                break;
            }
            let key = (dataset.to_string(), entry.service_id.clone());
            if inner.leases.contains_key(&key) || unhealthy.contains(&entry.service_id) {
                continue;
            }
            if !entry_matches_filters(entry, filters) {
                continue;
            }
            let Some(w) = wrapped_by_id.get(&entry.service_id) else {
                continue;
            };
            claimed.push(w.clone());
        }
        for w in &claimed {
            let sid = w
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            inner.leases.insert(
                (dataset.to_string(), sid),
                Lease {
                    holder_id: holder_id.clone(),
                    expires_at: expires_at_mono,
                },
            );
        }
        Ok((holder_id, expires_at_wall, claimed))
    }

    /// Release leases held by `holder_id`: all of them when `service_ids`
    /// is `None`, otherwise only those ids. Returns the released ids.
    pub fn release_reservation(
        &self,
        dataset: &str,
        holder_id: &str,
        service_ids: Option<&[String]>,
        caller: Option<&AuthContext>,
    ) -> Result<Vec<String>> {
        if let Some(c) = caller {
            if !c.is_admin() && holder_id != c.principal_id {
                return Err(RegistryError::Permission(format!(
                    "Cannot release leases of another holder ('{holder_id}')"
                )));
            }
        }
        let mut inner = self.inner.lock();
        Self::sweep_expired_leases_locked(&mut inner, monotonic_now());
        let mut released = Vec::new();
        match service_ids {
            None => {
                let keys: Vec<(String, String)> = inner
                    .leases
                    .iter()
                    .filter(|((ds, _), l)| ds == dataset && l.holder_id == holder_id)
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in keys {
                    inner.leases.remove(&k);
                    released.push(k.1);
                }
            }
            Some(ids) => {
                for sid in ids {
                    let key = (dataset.to_string(), sid.clone());
                    let Some(lease) = inner.leases.get(&key) else {
                        continue;
                    };
                    if lease.holder_id != holder_id {
                        return Err(RegistryError::Permission(format!(
                            "Lease on '{sid}' is held by a different holder"
                        )));
                    }
                    inner.leases.remove(&key);
                    released.push(sid.clone());
                }
            }
        }
        Ok(released)
    }

    /// Release any lease on `(dataset, service_id)` regardless of holder.
    /// Returns `(released, prev_holder_id)`.
    pub fn release_lease_by_sid(
        &self,
        dataset: &str,
        service_id: &str,
        caller: Option<&AuthContext>,
    ) -> Result<(bool, Option<String>)> {
        if let Some(c) = caller {
            if let Some(entry) = self.get_entry(dataset, service_id) {
                Self::assert_owner(&entry, c)?;
            }
        }
        let mut inner = self.inner.lock();
        Self::sweep_expired_leases_locked(&mut inner, monotonic_now());
        match inner
            .leases
            .remove(&(dataset.to_string(), service_id.to_string()))
        {
            None => Ok((false, None)),
            Some(l) => Ok((true, Some(l.holder_id))),
        }
    }

    /// Extend all of `holder_id`'s leases in `dataset`. Returns the new
    /// wall clock expiry. Fails with `NotFound` when no live lease exists.
    pub fn extend_reservation(
        &self,
        dataset: &str,
        holder_id: &str,
        ttl_seconds: i64,
        caller: Option<&AuthContext>,
    ) -> Result<f64> {
        self.extend_reservation_at(
            dataset,
            holder_id,
            ttl_seconds,
            caller,
            monotonic_now(),
            now_wall(),
        )
    }

    /// [`Self::extend_reservation`] with explicit clocks for tests.
    pub fn extend_reservation_at(
        &self,
        dataset: &str,
        holder_id: &str,
        ttl_seconds: i64,
        caller: Option<&AuthContext>,
        now_mono: f64,
        now_wall: f64,
    ) -> Result<f64> {
        if ttl_seconds < 1 {
            return Err(RegistryError::Invalid(format!(
                "ttl_seconds must be >= 1, got {ttl_seconds}"
            )));
        }
        if let Some(c) = caller {
            if !c.is_admin() && holder_id != c.principal_id {
                return Err(RegistryError::Permission(format!(
                    "Cannot extend leases of another holder ('{holder_id}')"
                )));
            }
        }
        let new_mono = now_mono + ttl_seconds as f64;
        let new_wall = now_wall + ttl_seconds as f64;
        let mut inner = self.inner.lock();
        Self::sweep_expired_leases_locked(&mut inner, now_mono);
        let mut owned = 0;
        for ((ds, _), lease) in inner.leases.iter_mut() {
            if ds == dataset && lease.holder_id == holder_id {
                lease.expires_at = new_mono;
                owned += 1;
            }
        }
        if owned == 0 {
            return Err(RegistryError::NotFound(format!(
                "No live leases for holder '{holder_id}' in dataset '{dataset}'"
            )));
        }
        Ok(new_wall)
    }

    // -----------------------------------------------------------------------
    // Internal: output generation
    // -----------------------------------------------------------------------

    fn get_store(&self, dataset: &str) -> Result<Arc<RegistryStore>> {
        let mut stores = self.stores.lock();
        if let Some(s) = stores.get(dataset) {
            return Ok(s.clone());
        }
        let store = Arc::new(RegistryStore::new(self.database_dir.join(dataset))?);
        stores.insert(dataset.to_string(), store.clone());
        Ok(store)
    }

    /// Rebuild the output cache and write `service.json` when it changed.
    fn regenerate_output(&self, dataset: &str) -> Result<bool> {
        let (changed, output) = {
            let mut inner = self.inner.lock();
            let output: Vec<Value> = inner
                .entries
                .get(dataset)
                .map(|ds| ds.values().map(|e| Value::Object(entry_to_output(e))).collect())
                .unwrap_or_default();
            let changed = inner.output_cache.get(dataset) != Some(&output);
            inner.output_cache.insert(dataset.to_string(), output.clone());
            (changed, output)
        };
        if changed {
            self.get_store(dataset)?.write_service_json(&output)?;
            let cb = self.on_service_changed.read().clone();
            if let Some(cb) = cb {
                cb.on_service_changed(dataset);
            }
        }
        Ok(changed)
    }

    // -----------------------------------------------------------------------
    // Internal: taxonomy state
    // -----------------------------------------------------------------------

    fn init_taxonomy_state(&self, dataset: &str) -> TaxonomyState {
        let state = self.compute_taxonomy_state(dataset);
        self.inner
            .lock()
            .taxonomy_states
            .insert(dataset.to_string(), state);
        state
    }

    /// Compare the current service hash against `build_config.json`.
    fn compute_taxonomy_state(&self, dataset: &str) -> TaxonomyState {
        let build_config_path = self.taxonomy_dir(dataset).join(BUILD_CONFIG_FILE);
        let taxonomy_path = self.taxonomy_dir(dataset).join(TAXONOMY_FILE);
        if !build_config_path.exists() || !taxonomy_path.exists() {
            return TaxonomyState::Nonexistent;
        }
        let Some(stored) = read_build_hash(&build_config_path) else {
            return TaxonomyState::Nonexistent;
        };
        let current = self.list_services(dataset);
        match compute_build_hash(&current) {
            Ok(hash) if hash == stored => TaxonomyState::Available,
            Ok(_) => TaxonomyState::Unavailable,
            Err(e) => {
                tracing::warn!("could not hash the services of {dataset}: {e}");
                TaxonomyState::Unavailable
            }
        }
    }

    /// Mark the taxonomy stale after CRUD (only when currently available).
    fn mark_taxonomy_stale(&self, dataset: &str) {
        let mut inner = self.inner.lock();
        if inner.taxonomy_states.get(dataset) == Some(&TaxonomyState::Available) {
            inner
                .taxonomy_states
                .insert(dataset.to_string(), TaxonomyState::Stale);
        }
    }

    // -----------------------------------------------------------------------
    // Dataset lifecycle
    // -----------------------------------------------------------------------

    /// Create a new empty dataset with vector and register configs.
    pub fn create_dataset(
        &self,
        name: &str,
        embedding_model: Option<&str>,
        formats: Option<&Value>,
        auth_required: bool,
    ) -> Result<PathBuf> {
        let ds_dir = self.database_dir.join(name);
        if ds_dir.exists() {
            return Err(RegistryError::Invalid(format!("Dataset '{name}' already exists")));
        }
        let model = embedding_model.unwrap_or(DEFAULT_EMBEDDING_MODEL);
        let normalized = self.normalize_or_default_formats(formats)?;
        self.init_dataset_files(name, model, &normalized, auth_required)?;
        tracing::info!(
            "Created dataset '{}' (embedding: {}, formats: {:?}, auth_required: {})",
            name,
            model,
            normalized,
            auth_required
        );
        Ok(ds_dir)
    }

    fn normalize_or_default_formats(&self, formats: Option<&Value>) -> Result<HashMap<String, String>> {
        let Some(raw) = formats else {
            return Ok(DEFAULT_FORMAT_CONFIG.clone());
        };
        let normalized = normalize_format_config(Some(raw));
        if normalized.is_empty() {
            return Err(RegistryError::Invalid(format!(
                "formats must declare at least one valid type/version. Supported types: {}",
                py_list(SUPPORTED_SERVICE_TYPES.iter().copied())
            )));
        }
        Ok(normalized)
    }

    /// Idempotent: create the dataset directory and its config files
    /// without clobbering existing ones.
    fn init_dataset_files(
        &self,
        name: &str,
        embedding_model: &str,
        formats: &HashMap<String, String>,
        auth_required: bool,
    ) -> Result<()> {
        let ds_dir = self.database_dir.join(name);
        std::fs::create_dir_all(&ds_dir)?;
        std::fs::create_dir_all(ds_dir.join("query"))?;
        if !ds_dir.join("vector_config.json").exists() {
            self.set_vector_config(name, Some(embedding_model), None)?;
        }
        if !ds_dir.join("register_config.json").exists() {
            self.set_register_config(name, &Value::Object(formats_to_map(formats)))?;
        }
        if auth_required && !ds_dir.join("auth_config.json").exists() {
            self.get_store(name)?.write_auth_config(true)?;
            self.auth_required_cache.lock().insert(name.to_string(), true);
        }
        Ok(())
    }

    /// Delete a dataset directory and every in-memory cache for it.
    pub fn delete_dataset(&self, name: &str) -> Result<()> {
        let ds_dir = self.database_dir.join(name);
        if !ds_dir.exists() {
            return Err(RegistryError::Invalid(format!("Dataset '{name}' does not exist")));
        }
        {
            let mut inner = self.inner.lock();
            inner.entries.remove(name);
            inner.output_cache.remove(name);
            inner.taxonomy_states.remove(name);
            inner.format_configs.remove(name);
            inner.leases.retain(|(ds, _), _| ds != name);
        }
        self.stores.lock().remove(name);
        self.auth_required_cache.lock().remove(name);
        self.lease_config_cache.lock().remove(name);
        std::fs::remove_dir_all(&ds_dir)?;
        tracing::info!("Deleted dataset '{}'", name);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Per namespace auth gating
    // -----------------------------------------------------------------------

    /// Whether mutations on `dataset` require an authenticated caller.
    /// Missing file or missing dataset means `false`.
    pub fn is_auth_required(&self, dataset: &str) -> bool {
        if let Some(v) = self.auth_required_cache.lock().get(dataset) {
            return *v;
        }
        if !self.database_dir.join(dataset).exists() {
            return false;
        }
        let required = self
            .get_store(dataset)
            .map(|s| s.load_auth_config().required)
            .unwrap_or(false);
        self.auth_required_cache
            .lock()
            .insert(dataset.to_string(), required);
        required
    }

    /// Toggle the `auth_required` flag on an existing dataset.
    pub fn set_auth_config(&self, dataset: &str, required: bool) -> Result<Map<String, Value>> {
        if !self.database_dir.join(dataset).exists() {
            return Err(RegistryError::Invalid(format!(
                "Dataset '{dataset}' does not exist"
            )));
        }
        self.get_store(dataset)?.write_auth_config(required)?;
        self.auth_required_cache
            .lock()
            .insert(dataset.to_string(), required);
        let mut m = Map::new();
        m.insert("required".into(), Value::Bool(required));
        m.insert("schema_version".into(), Value::from(1));
        Ok(m)
    }

    // -----------------------------------------------------------------------
    // Per namespace heartbeat lease gating
    // -----------------------------------------------------------------------

    /// Heartbeat lease config for `dataset` (cached; disabled default).
    pub fn get_lease_config(&self, dataset: &str) -> LeaseConfig {
        if let Some(c) = self.lease_config_cache.lock().get(dataset) {
            return c.clone();
        }
        if !self.database_dir.join(dataset).exists() {
            return LeaseConfig::default();
        }
        let cfg = self
            .get_store(dataset)
            .map(|s| s.load_lease_config())
            .unwrap_or_default();
        self.lease_config_cache
            .lock()
            .insert(dataset.to_string(), cfg.clone());
        cfg
    }

    /// Configure the heartbeat lease policy on a dataset.
    pub fn set_lease_config(
        &self,
        dataset: &str,
        enabled: bool,
        min_ttl: i64,
        max_ttl: i64,
        grace_period: i64,
    ) -> Result<LeaseConfig> {
        if !self.database_dir.join(dataset).exists() {
            return Err(RegistryError::Invalid(format!(
                "Dataset '{dataset}' does not exist"
            )));
        }
        self.get_store(dataset)?
            .write_lease_config(enabled, min_ttl, max_ttl, grace_period)?;
        let cfg = LeaseConfig {
            enabled,
            min_ttl,
            max_ttl,
            grace_period,
            schema_version: 1,
        };
        self.lease_config_cache
            .lock()
            .insert(dataset.to_string(), cfg.clone());
        Ok(cfg)
    }

    // -----------------------------------------------------------------------
    // Internal: unified format validation
    // -----------------------------------------------------------------------

    /// Effective `{type: min_version}` map for a dataset (cached; file;
    /// then defaults).
    pub fn get_register_config(&self, dataset: &str) -> Result<HashMap<String, String>> {
        if let Some(c) = self.inner.lock().format_configs.get(dataset) {
            return Ok(c.clone());
        }
        let cfg = self
            .get_store(dataset)?
            .load_register_config()
            .unwrap_or_else(|| DEFAULT_FORMAT_CONFIG.clone());
        self.inner
            .lock()
            .format_configs
            .insert(dataset.to_string(), cfg.clone());
        Ok(cfg)
    }

    /// `{embedding_model, embedding_dim}` for a dataset, or the system
    /// default when the file is missing (not written).
    pub fn get_vector_config(&self, dataset: &str) -> Result<Map<String, Value>> {
        if let Some(cfg) = self.get_store(dataset)?.load_vector_config() {
            return Ok(cfg);
        }
        let mut m = Map::new();
        m.insert(
            "embedding_model".into(),
            Value::String(DEFAULT_EMBEDDING_MODEL.into()),
        );
        m.insert(
            "embedding_dim".into(),
            Value::from(known_embedding_dim(DEFAULT_EMBEDDING_MODEL).unwrap_or(0)),
        );
        Ok(m)
    }

    /// Persist a new embedding config. The dimension is resolved from the
    /// known model table unless given explicitly.
    pub fn set_vector_config(
        &self,
        dataset: &str,
        embedding_model: Option<&str>,
        embedding_dim: Option<i64>,
    ) -> Result<Map<String, Value>> {
        let model_name = embedding_model
            .filter(|m| !m.is_empty())
            .unwrap_or(DEFAULT_EMBEDDING_MODEL);
        let dim = match known_embedding_dim(model_name) {
            Some(d) => i64::from(d),
            None => embedding_dim.ok_or_else(|| {
                RegistryError::Invalid(format!(
                    "Unknown embedding model '{model_name}'; provide embedding_dim explicitly"
                ))
            })?,
        };
        self.get_store(dataset)?.write_vector_config(model_name, dim)?;
        let mut m = Map::new();
        m.insert("embedding_model".into(), Value::String(model_name.into()));
        m.insert("embedding_dim".into(), Value::from(dim));
        Ok(m)
    }

    /// Persist a new `formats` mapping. Unknown types and versions are
    /// dropped; an empty result is rejected.
    pub fn set_register_config(&self, dataset: &str, formats: &Value) -> Result<HashMap<String, String>> {
        let cfg = normalize_format_config(Some(formats));
        if cfg.is_empty() {
            return Err(RegistryError::Invalid(format!(
                "formats must declare at least one valid type with a known version; supported types: {}",
                py_list(SUPPORTED_SERVICE_TYPES.iter().copied())
            )));
        }
        self.get_store(dataset)?.write_register_config(&cfg)?;
        self.inner
            .lock()
            .format_configs
            .insert(dataset.to_string(), cfg.clone());
        Ok(cfg)
    }

    fn require_type_allowed(&self, dataset: &str, service_type: &str) -> Result<String> {
        let cfg = self.get_register_config(dataset)?;
        match cfg.get(service_type) {
            Some(v) => Ok(v.clone()),
            None => {
                let mut allowed: Vec<&String> = cfg.keys().collect();
                allowed.sort();
                Err(RegistryError::Invalid(format!(
                    "Service type '{service_type}' is not allowed for dataset '{dataset}'. Allowed: {}",
                    py_list(allowed.iter().map(|s| s.as_str()))
                )))
            }
        }
    }

    fn require_valid(&self, dataset: &str, service_type: &str, payload: &Value) -> Result<ValidationResult> {
        let min_version = self.require_type_allowed(dataset, service_type)?;
        let result = match (&self.allowed_a2a_versions, service_type) {
            (Some(allowed), "a2a") => {
                let card = AgentCard::from_value(payload.clone())
                    .map_err(|e| RegistryError::Invalid(format!("invalid agent card: {e}")))?;
                validate_agent_card(&card, Some(allowed))
            }
            _ => validate_service(service_type, payload, &min_version),
        };
        if !result.valid {
            return Err(RegistryError::Invalid(format!(
                "{service_type} payload failed validation for dataset '{dataset}': {}",
                result.errors.join("; ")
            )));
        }
        if !result.warnings.is_empty() {
            tracing::info!(
                "{} payload passed as {} ({}) with warnings: {}",
                service_type,
                result.matched_version.as_deref().unwrap_or("?"),
                dataset,
                result.warnings.join("; ")
            );
        }
        Ok(result)
    }

    /// Startup side validation. `None` when validation is skipped (an A2A
    /// entry whose card has not been fetched yet).
    fn validate_entry(
        &self,
        dataset: &str,
        entry: &RegistryEntry,
        skip_a2a_url_only: bool,
    ) -> Result<Option<ValidationResult>> {
        let cfg = self.get_register_config(dataset)?;
        let t = entry.r#type.as_str();
        let Some(min_version) = cfg.get(t) else {
            return Ok(Some(ValidationResult {
                valid: false,
                service_type: Some(t.into()),
                matched_version: None,
                errors: vec![format!("service type '{t}' not allowed in dataset '{dataset}'")],
                warnings: Vec::new(),
            }));
        };
        let fail = |errors: Vec<String>| ValidationResult {
            valid: false,
            service_type: Some(t.into()),
            matched_version: None,
            errors,
            warnings: Vec::new(),
        };
        match entry.r#type {
            ServiceType::A2a => match &entry.agent_card {
                None if skip_a2a_url_only => Ok(None),
                None => Ok(Some(fail(vec![
                    "agent_card not present (URL fetch not yet completed)".into(),
                ]))),
                Some(card) => {
                    if let Some(allowed) = &self.allowed_a2a_versions {
                        return Ok(Some(validate_agent_card(card, Some(allowed))));
                    }
                    Ok(Some(validate_service(
                        "a2a",
                        &Value::Object(card.to_json(DumpMode::Full)),
                        min_version,
                    )))
                }
            },
            ServiceType::Generic => match &entry.service_data {
                Some(sd) => Ok(Some(validate_service(
                    "generic",
                    &serde_json::json!({"name": sd.name, "description": sd.description}),
                    min_version,
                ))),
                None => Ok(Some(fail(vec!["entry has no payload to validate".into()]))),
            },
            ServiceType::Skill => match &entry.skill_data {
                Some(sk) => Ok(Some(validate_service(
                    "skill",
                    &serde_json::json!({"name": sk.name, "description": sk.description}),
                    min_version,
                ))),
                None => Ok(Some(fail(vec!["entry has no payload to validate".into()]))),
            },
        }
    }

    async fn fetch_agent_cards_parallel(&self, url_entries: &[(String, String, String)]) {
        let workers = self.agent_card_workers.max(1);
        let results: Vec<((String, String, String), Result<AgentCard>)> =
            stream::iter(url_entries.iter().cloned())
                .map(|item| async move {
                    let card = fetch_agent_card(&item.2).await;
                    (item, card)
                })
                .buffer_unordered(workers)
                .collect()
                .await;
        for ((dataset, sid, url), outcome) in results {
            match outcome {
                Ok(card) => {
                    let mut inner = self.inner.lock();
                    if let Some(entry) = inner.entries.get_mut(&dataset).and_then(|ds| ds.get_mut(&sid)) {
                        entry.agent_card = Some(card);
                    }
                    tracing::info!("Fetched agent card '{}' from {}", sid, url);
                }
                Err(e) => {
                    let has_cache = self
                        .get_entry(&dataset, &sid)
                        .map(|e| e.agent_card.is_some())
                        .unwrap_or(false);
                    if has_cache {
                        tracing::warn!("Failed to fetch {}, using cached snapshot: {}", url, e);
                    } else {
                        tracing::warn!("Failed to fetch {}, no cache: {}", url, e);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal: dataset discovery and global config
    // -----------------------------------------------------------------------

    fn discover_datasets(&self) -> Vec<String> {
        self.list_datasets()
            .into_iter()
            .filter(|name| {
                let d = self.database_dir.join(name);
                d.join(USER_CONFIG_FILE).exists()
                    || d.join(API_CONFIG_FILE).exists()
                    || d.join(REGISTER_CONFIG_FILE).exists()
                    || d.join("skills").is_dir()
            })
            .collect()
    }

    fn distribute_global_config(&self, path: &Path) {
        let data: Value = match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to load global config {}: {}", path.display(), e);
                return;
            }
        };
        let mut by_dataset: IndexMap<String, Vec<Value>> = IndexMap::new();
        for svc in data
            .get("services")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let mut svc = svc;
            let ds = match svc.as_object_mut().and_then(|m| m.remove("dataset")) {
                Some(Value::String(s)) => s,
                _ => "default".to_string(),
            };
            by_dataset.entry(ds).or_default().push(svc);
        }
        for (dataset, services) in by_dataset {
            let dataset_dir = self.database_dir.join(&dataset);
            if let Err(e) = std::fs::create_dir_all(&dataset_dir) {
                tracing::warn!("Cannot create {}: {}", dataset_dir.display(), e);
                continue;
            }
            let user_config_path = dataset_dir.join(USER_CONFIG_FILE);
            if !user_config_path.exists() {
                let content = serde_json::to_string_pretty(&serde_json::json!({ "services": services }))
                    .unwrap_or_default();
                if std::fs::write(&user_config_path, content).is_ok() {
                    tracing::info!(
                        "Created {} from global config ({} services)",
                        user_config_path.display(),
                        services.len()
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Module level utilities
// ---------------------------------------------------------------------------

fn py_list<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<String> = items.map(|s| format!("'{s}'")).collect();
    format!("[{}]", parts.join(", "))
}

fn formats_to_map(formats: &HashMap<String, String>) -> Map<String, Value> {
    formats
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect()
}

fn count_json_items(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|v| match v {
            Value::Array(a) => a.len(),
            Value::Object(o) => o.len(),
            Value::String(s) => s.chars().count(),
            _ => 0,
        })
        .unwrap_or(0)
}

/// The type specific raw dict used for filter matching.
pub fn entry_filter_dict(entry: &RegistryEntry) -> Option<Map<String, Value>> {
    match entry.r#type {
        ServiceType::A2a => entry
            .agent_card
            .as_ref()
            .map(|c| c.to_json(DumpMode::ExcludeNone)),
        ServiceType::Generic => entry.service_data.as_ref().map(|s| s.to_json()),
        ServiceType::Skill => entry.skill_data.as_ref().map(|s| s.to_json()),
    }
}

/// Filter AND match with the default-online carve out: `status=online`
/// also matches entries without a `status` field.
pub fn filter_matches(filters: &Map<String, Value>, raw: &Map<String, Value>) -> bool {
    for (k, v) in filters {
        let v = py_str(v);
        if k == "status" && v == "online" && !raw.contains_key("status") {
            continue;
        }
        match raw.get(k) {
            Some(actual) if py_str(actual) == v => {}
            _ => return false,
        }
    }
    true
}

fn entry_matches_filters(entry: &RegistryEntry, filters: &Map<String, Value>) -> bool {
    match entry_filter_dict(entry) {
        Some(raw) => filter_matches(filters, &raw),
        None => false,
    }
}

/// Convert an entry to the `service.json` output shape:
/// `{id, type, name, description, metadata, owner_id?}`.
pub fn entry_to_output(entry: &RegistryEntry) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(entry.service_id.clone()));
    match (
        entry.r#type,
        &entry.skill_data,
        &entry.service_data,
        &entry.agent_card,
    ) {
        (ServiceType::Skill, Some(sd), _, _) => {
            out.insert("type".into(), "skill".into());
            out.insert("name".into(), Value::String(sd.name.clone()));
            out.insert("description".into(), Value::String(sd.description.clone()));
            out.insert(
                "metadata".into(),
                serde_json::json!({
                    "skill_path": sd.skill_path, "license": sd.license, "files": sd.files,
                }),
            );
        }
        (ServiceType::Generic, _, Some(sd), _) => {
            let mut metadata = Map::new();
            if !sd.input_schema.is_empty() {
                metadata.insert("inputSchema".into(), Value::Object(sd.input_schema.clone()));
            }
            if let Some(url) = sd.url.as_ref().filter(|u| !u.is_empty()) {
                metadata.insert("url".into(), Value::String(url.clone()));
            }
            out.insert("type".into(), "generic".into());
            out.insert("name".into(), Value::String(sd.name.clone()));
            out.insert("description".into(), Value::String(sd.description.clone()));
            out.insert("metadata".into(), Value::Object(metadata));
        }
        (ServiceType::A2a, _, _, Some(card)) => {
            out.insert("type".into(), "a2a".into());
            out.insert("name".into(), Value::String(card.name.clone()));
            out.insert("description".into(), Value::String(build_description(card)));
            out.insert(
                "metadata".into(),
                Value::Object(card.to_json(DumpMode::ExcludeNone)),
            );
        }
        _ => {
            out.insert("type".into(), "a2a".into());
            out.insert("name".into(), Value::String(entry.service_id.clone()));
            out.insert(
                "description".into(),
                Value::String(format!(
                    "Unresolved agent card: {}",
                    entry.agent_card_url.as_deref().unwrap_or("unknown")
                )),
            );
            out.insert("metadata".into(), Value::Object(Map::new()));
        }
    }
    if let Some(o) = entry.owner_id.as_ref().filter(|o| !o.is_empty()) {
        out.insert("owner_id".into(), Value::String(o.clone()));
    }
    out
}

/// Hash of the sorted `(name, description)` pairs, order independent.
/// Byte compatible with the Python `json.dumps` based hash.
pub fn compute_build_hash(services: &[Value]) -> serde_json::Result<String> {
    let mut pairs: Vec<(String, String)> = services
        .iter()
        .map(|s| {
            (
                s.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                s.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    pairs.sort();
    let text = crate::util::pyjson::dumps(&pairs)?;
    Ok(hex::encode(Sha256::digest(text.as_bytes())))
}

fn read_build_hash(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("service_hash").and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserve<'a>(
        dataset: &'a str,
        filters: &'a Map<String, Value>,
        n: i64,
        ttl_seconds: i64,
    ) -> ReserveRequest<'a> {
        ReserveRequest {
            dataset,
            filters,
            n,
            ttl_seconds,
            holder_id: None,
        }
    }
    use a2x_common::Role;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use serde_json::json;

    fn card(name: &str) -> TestResult<AgentCard> {
        Ok(AgentCard::from_value(json!({
            "name": name, "description": "tester", "url": "http://example.invalid",
            "version": "1.0", "capabilities": {},
            "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
            "skills": [{"id": "s", "name": "s", "description": "s", "tags": ["t"]}]
        }))?)
    }

    fn svc() -> TestResult<(tempfile::TempDir, RegistryService)> {
        let tmp = tempfile::tempdir()?;
        let s = RegistryService::new(tmp.path().join("database"), None);
        Ok((tmp, s))
    }

    #[test]
    fn build_hash_matches_python_layout() -> TestResult {
        let services = vec![
            json!({"name": "b", "description": "y"}),
            json!({"name": "a", "description": "x"}),
        ];
        let expected = hex::encode(Sha256::digest(b"[[\"a\", \"x\"], [\"b\", \"y\"]]"));
        assert_eq!(compute_build_hash(&services)?, expected);
        Ok(())
    }

    #[tokio::test]
    async fn register_update_deregister_roundtrip() -> TestResult {
        let (tmp, s) = svc()?;
        s.create_dataset("ds", None, None, false)?;
        assert!(s.create_dataset("ds", None, None, false).is_err());
        let r = s.register_generic(&RegisterGenericRequest::new("ds", "Calc", "adds"), None)?;
        assert_eq!(r.status, "registered");
        let r2 = s.register_generic(&RegisterGenericRequest::new("ds", "Calc", "adds more"), None)?;
        assert_eq!(r2.status, "updated");
        let out = s.list_services("ds");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["description"], "adds more");
        assert_eq!(out[0]["metadata"], json!({}));
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(
            tmp.path().join("database/ds/service.json"),
        )?)?;
        assert_eq!(on_disk, Value::Array(out.clone()));

        let mut upd = Map::new();
        upd.insert("url".into(), json!("http://x"));
        upd.insert("description".into(), json!("adds more"));
        let u = s.update_service("ds", &r.service_id, &upd, None)?;
        assert_eq!(u.changed_fields, vec!["url"]);
        assert!(!u.taxonomy_affected);
        let mut bad = Map::new();
        bad.insert("bogus".into(), json!(1));
        let e = s.update_service("ds", &r.service_id, &bad, None).err_or_fail()?;
        assert!(e.to_string().starts_with("Unknown generic fields: ['bogus']"));
        assert!(matches!(
            s.update_service("ds", "nope", &upd, None),
            Err(RegistryError::NotFound(_))
        ));

        let a = s
            .register_a2a(&RegisterA2ARequest::with_card("ds", card("agent")?), None)
            .await?;
        let mut st = Map::new();
        st.insert("status".into(), json!("busy"));
        let u = s.update_service("ds", &a.service_id, &st, None)?;
        assert_eq!(u.changed_fields, vec!["status"]);
        assert_eq!(
            s.get_entry("ds", &a.service_id)
                .required()?
                .agent_card
                .required()?
                .extra["status"],
            "busy"
        );

        let d = s.deregister("ds", &a.service_id, None)?;
        assert_eq!(d.status, "deregistered");
        assert!(matches!(
            s.deregister("ds", &a.service_id, None),
            Err(RegistryError::NotFound(_))
        ));
        let api: Value = serde_json::from_str(&std::fs::read_to_string(
            tmp.path().join("database/ds/api_config.json"),
        )?)?;
        assert_eq!(api["services"].as_array().required()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn startup_merges_sources_and_priority() -> TestResult {
        let (tmp, s) = svc()?;
        let ds = tmp.path().join("database/ds");
        std::fs::create_dir_all(&ds)?;
        std::fs::write(
            ds.join("user_config.json"),
            json!({"services": [
                {"type": "generic", "service_id": "shared", "name": "user", "description": "from user"},
                {"type": "generic", "name": "only user", "description": "u"}
            ]})
            .to_string(),
        )?;
        std::fs::write(ds.join("api_config.json"), json!({"services": [
            {"type": "generic", "service_id": "shared", "name": "api", "description": "from api"},
            {"type": "a2a", "agent_card_url": "http://127.0.0.1:9/nope", "agent_card": {"name": "cached", "description": "snap"}}
        ]}).to_string())?;
        std::fs::create_dir_all(ds.join("skills/sk"))?;
        std::fs::write(
            ds.join("skills/sk/SKILL.md"),
            "---\nname: sk\ndescription: skill\n---\n",
        )?;
        let states = s.startup().await?;
        assert_eq!(states["ds"], TaxonomyState::Nonexistent);
        let entries = s.list_entries("ds");
        assert_eq!(entries.len(), 4);
        let shared = s.get_entry("ds", "shared").required()?;
        assert_eq!(shared.source, Source::ApiConfig);
        assert_eq!(shared.service_data.required()?.name, "api");
        let out = s.list_services("ds");
        assert_eq!(out[0]["id"], "shared");
        assert_eq!(out[3]["type"], "skill");
        let cached = out.iter().find(|o| o["name"] == "cached").required()?;
        assert_eq!(cached["type"], "a2a");
        assert!(matches!(
            s.deregister("ds", &entries[1].service_id, None),
            Err(RegistryError::Invalid(_))
        ));
        assert_eq!(s.get_status(None).total_services, 4);
        assert_eq!(s.get_status(Some("ds")).by_source["user_config"], 1);
        Ok(())
    }

    #[test]
    fn taxonomy_state_follows_hash() -> TestResult {
        let (tmp, s) = svc()?;
        s.create_dataset("ds", None, None, false)?;
        s.register_generic(&RegisterGenericRequest::new("ds", "n", "d"), None)?;
        let tax = tmp.path().join("database/ds/taxonomy");
        std::fs::create_dir_all(&tax)?;
        std::fs::write(tax.join("taxonomy.json"), "{}")?;
        let h = compute_build_hash(&s.list_services("ds"))?;
        std::fs::write(
            tax.join("build_config.json"),
            json!({"service_hash": h}).to_string(),
        )?;
        assert_eq!(s.init_taxonomy_state("ds"), TaxonomyState::Available);
        s.register_generic(&RegisterGenericRequest::new("ds", "m", "e"), None)?;
        assert_eq!(s.get_taxonomy_state("ds"), Some(TaxonomyState::Stale));
        assert_eq!(s.check_taxonomy_state("ds"), Some(TaxonomyState::Unavailable));
        assert_eq!(s.check_taxonomy_state("other"), None);
        Ok(())
    }

    #[test]
    fn register_config_gates_types() -> TestResult {
        let (_tmp, s) = svc()?;
        s.create_dataset("ds", None, Some(&json!({"a2a": "v1.0"})), false)?;
        let e = s
            .register_generic(&RegisterGenericRequest::new("ds", "n", "d"), None)
            .err_or_fail()?;
        assert!(e.to_string().contains("not allowed"));
        let loose = AgentCard::from_value(json!({"name": "a", "description": "b"}))?;
        let e = s
            .register_a2a_resolved(
                &RegisterA2ARequest::with_card("ds", loose),
                AgentCard::from_value(json!({"name": "a", "description": "b"}))?,
                None,
                None,
            )
            .err_or_fail()?;
        assert!(e.to_string().contains("failed validation"));
        assert!(
            s.create_dataset("empty", None, Some(&json!({"x": "v0.0"})), false)
                .is_err()
        );
        assert_eq!(s.get_register_config("ds")?.len(), 1);
        s.set_register_config("ds", &json!({"generic": "v0.0"}))?;
        assert!(
            s.register_generic(&RegisterGenericRequest::new("ds", "n", "d"), None)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn auth_checks_in_service_layer() -> TestResult {
        let (_tmp, s) = svc()?;
        let provider = AuthContext::new(
            "u_p",
            Role::Provider,
            Some(["ds".to_string()].into_iter().collect()),
        );
        let other = AuthContext::new(
            "u_o",
            Role::Provider,
            Some(["ds".to_string()].into_iter().collect()),
        );
        let user = AuthContext::new("u_u", Role::User, Some(["ds".to_string()].into_iter().collect()));
        let admin = AuthContext::admin("u_a");
        assert!(matches!(
            s.register_generic(&RegisterGenericRequest::new("ds", "n", "d"), Some(&user)),
            Err(RegistryError::Permission(_))
        ));
        let r = s.register_generic(&RegisterGenericRequest::new("ds", "n", "d"), Some(&provider))?;
        assert_eq!(
            s.get_entry("ds", &r.service_id).required()?.owner_id.as_deref(),
            Some("u_p")
        );
        assert_eq!(s.list_services("ds")[0]["owner_id"], "u_p");
        let mut upd = Map::new();
        upd.insert("description".into(), json!("x"));
        assert!(matches!(
            s.update_service("ds", &r.service_id, &upd, Some(&other)),
            Err(RegistryError::Permission(_))
        ));
        assert!(s.update_service("ds", &r.service_id, &upd, Some(&admin)).is_ok());
        let anon = s.register_generic(&RegisterGenericRequest::new("ds", "m", "d"), None)?;
        let e = s
            .update_service("ds", &anon.service_id, &upd, Some(&provider))
            .err_or_fail()?;
        assert!(e.to_string().contains("admin role required"));
        Ok(())
    }

    #[test]
    fn reservations_are_toctou_safe_and_scoped() -> TestResult {
        let (_tmp, s) = svc()?;
        for n in ["a", "b", "c"] {
            let mut card = json!({"name": n, "description": "__BLANK__"});
            if n == "c" {
                card["status"] = json!("busy");
            }
            s.register_a2a_resolved(
                &RegisterA2ARequest::with_card("ds", AgentCard::from_value(card.clone())?),
                AgentCard::from_value(card)?,
                None,
                None,
            )?;
        }
        let mut filters = Map::new();
        filters.insert("description".into(), json!("__BLANK__"));
        filters.insert("status".into(), json!("online"));
        let (holder, exp, claimed) =
            s.reserve_services_at(reserve("ds", &filters, 5, 30), None, 100.0, 1000.0)?;
        assert!(holder.starts_with("holder_"));
        assert_eq!(exp, 1030.0);
        assert_eq!(claimed.len(), 2);
        assert_eq!(claimed[0]["description"], "__BLANK__.");
        let (_, _, again) = s.reserve_services_at(reserve("ds", &filters, 5, 30), None, 101.0, 1001.0)?;
        assert!(again.is_empty());
        assert!(s.is_leased("ds", claimed[0]["id"].as_str().required()?));
        assert!(
            s.extend_reservation_at("ds", "nobody", 10, None, 102.0, 1002.0)
                .is_err()
        );
        assert_eq!(
            s.extend_reservation_at("ds", &holder, 10, None, 102.0, 1002.0)?,
            1012.0
        );
        let sid0 = claimed[0]["id"].as_str().required()?.to_string();
        assert!(matches!(
            s.release_reservation("ds", "other", Some(std::slice::from_ref(&sid0)), None),
            Err(RegistryError::Permission(_))
        ));
        assert_eq!(
            s.release_reservation("ds", &holder, Some(&[sid0.clone(), "ghost".into()]), None)?,
            vec![sid0.clone()]
        );
        let (released, prev) = s.release_lease_by_sid("ds", claimed[1]["id"].as_str().required()?, None)?;
        assert!(released);
        assert_eq!(prev.as_deref(), Some(holder.as_str()));
        assert_eq!(s.release_lease_by_sid("ds", &sid0, None)?, (false, None));
        assert!(s.release_reservation("ds", &holder, None, None)?.is_empty());
        assert!(s.reserve_services("ds", &filters, -1, 30, None, None).is_err());
        assert!(s.reserve_services("ds", &filters, 1, 0, None, None).is_err());
        let user = AuthContext::new("u_1", Role::User, Some(["ds".to_string()].into_iter().collect()));
        let (h, _, _) = s.reserve_services("ds", &filters, 1, 30, Some("forged".into()), Some(&user))?;
        assert_eq!(h, "u_1");
        Ok(())
    }

    #[test]
    fn skill_register_update_deregister() -> TestResult {
        let (tmp, s) = svc()?;
        let zip = {
            use std::io::Write;
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut zf = zip::ZipWriter::new(&mut buf);
            zf.start_file("SKILL.md", zip::write::SimpleFileOptions::default())?;
            zf.write_all(b"---\nname: art\ndescription: Make art\n---\nbody\n")?;
            zf.finish()?;
            buf.into_inner()
        };
        let r = s.register_skill("ds", &zip, None)?;
        assert_eq!(r.status, "registered");
        assert_eq!(s.register_skill("ds", &zip, None)?.status, "updated");
        let mut upd = Map::new();
        upd.insert("name".into(), json!("art2"));
        upd.insert("license".into(), json!("MIT"));
        let u = s.update_service("ds", &r.service_id, &upd, None)?;
        assert!(u.taxonomy_affected);
        assert!(tmp.path().join("database/ds/skills/art2/SKILL.md").exists());
        let entry = s.get_entry("ds", &r.service_id).required()?;
        assert_eq!(entry.skill_data.as_ref().required()?.skill_path, "skills/art2");
        assert!(matches!(
            s.deregister("ds", &r.service_id, None),
            Err(RegistryError::Invalid(_))
        ));
        assert!(s.get_skill_zip("ds", "art2").is_ok());
        // The entry keeps its original id (hash of the original name), so
        // deregistration addresses the original name, as in the Python port.
        assert_eq!(s.deregister_skill("ds", "art2", None)?.status, "not_found");
        assert_eq!(s.deregister_skill("ds", "art", None)?.status, "deleted");
        assert_eq!(s.deregister_skill("ds", "art", None)?.status, "not_found");
        Ok(())
    }

    #[test]
    fn dataset_lifecycle_and_configs() -> TestResult {
        let (tmp, s) = svc()?;
        s.create_dataset("ds", Some("shibing624/text2vec-base-chinese"), None, true)?;
        assert!(s.is_auth_required("ds"));
        assert!(!s.is_auth_required("missing"));
        assert_eq!(s.get_vector_config("ds")?["embedding_dim"], 768);
        assert!(s.set_vector_config("ds", Some("unknown-model"), None).is_err());
        assert_eq!(
            s.set_vector_config("ds", Some("unknown-model"), Some(12))?["embedding_dim"],
            12
        );
        assert!(!s.get_lease_config("ds").enabled);
        assert!(s.set_lease_config("missing", true, 1, 2, 3).is_err());
        let cfg = s.set_lease_config("ds", true, 5, 60, 10)?;
        assert_eq!(cfg.max_ttl, 60);
        assert!(s.get_lease_config("ds").enabled);
        assert_eq!(s.set_auth_config("ds", false)?["required"], false);
        assert!(!s.is_auth_required("ds"));
        assert_eq!(s.list_datasets(), vec!["ds".to_string()]);
        assert!(s.list_datasets_with_counts().is_empty());
        s.register_generic(&RegisterGenericRequest::new("ds", "n", "d"), None)?;
        assert_eq!(s.list_datasets_with_counts()[0]["service_count"], 1);
        s.delete_dataset("ds")?;
        assert!(!tmp.path().join("database/ds").exists());
        assert!(s.delete_dataset("ds").is_err());
        assert!(s.list_entries("ds").is_empty());
        Ok(())
    }
}
