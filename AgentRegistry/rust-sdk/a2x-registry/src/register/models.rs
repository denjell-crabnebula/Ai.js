// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Data models for the registration module.
//!
//! The Pydantic originals are used in three projections: a full dump, a
//! dump without `None` values (service.json metadata, filter matching) and a
//! dump without default values (api_config.json). The `to_json*` helpers
//! reproduce those projections and their key order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// State of the A2X taxonomy for a dataset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaxonomyState {
    /// No taxonomy built yet or `build_config.json` missing.
    Nonexistent,
    /// Taxonomy hash matches the current service list.
    Available,
    /// Hash mismatch: services changed since the last build.
    Unavailable,
    /// CRUD happened after the last check; re-evaluated on next search.
    Stale,
}

impl TaxonomyState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaxonomyState::Nonexistent => "nonexistent",
            TaxonomyState::Available => "available",
            TaxonomyState::Unavailable => "unavailable",
            TaxonomyState::Stale => "stale",
        }
    }
}

impl std::fmt::Display for TaxonomyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Service type of a registry entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Generic,
    A2a,
    Skill,
}

impl ServiceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceType::Generic => "generic",
            ServiceType::A2a => "a2a",
            ServiceType::Skill => "skill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "generic" => Some(ServiceType::Generic),
            "a2a" => Some(ServiceType::A2a),
            "skill" => Some(ServiceType::Skill),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an entry came from. Determines which mutations are allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    UserConfig,
    ApiConfig,
    Ephemeral,
    SkillFolder,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::UserConfig => "user_config",
            Source::ApiConfig => "api_config",
            Source::Ephemeral => "ephemeral",
            Source::SkillFolder => "skill_folder",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// --- Generic service ---

/// Payload of a generic service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenericServiceData {
    pub name: String,
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Map<String, Value>,
    #[serde(default)]
    pub url: Option<String>,
}

impl GenericServiceData {
    /// Full dump: `{name, description, inputSchema, url}` with `url` as null
    /// when absent.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("description".into(), Value::String(self.description.clone()));
        m.insert("inputSchema".into(), Value::Object(self.input_schema.clone()));
        m.insert(
            "url".into(),
            self.url.clone().map(Value::String).unwrap_or(Value::Null),
        );
        m
    }

    /// Dump without `None` values.
    pub fn to_json_exclude_none(&self) -> Map<String, Value> {
        let mut m = self.to_json();
        if self.url.is_none() {
            m.remove("url");
        }
        m
    }
}

/// Metadata parsed from a `SKILL.md` folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillData {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: String,
    /// Relative path such as `skills/{name}`.
    #[serde(default)]
    pub skill_path: String,
    /// Relative file paths inside the folder, sorted.
    #[serde(default)]
    pub files: Vec<String>,
}

impl SkillData {
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("description".into(), Value::String(self.description.clone()));
        m.insert("license".into(), Value::String(self.license.clone()));
        m.insert("skill_path".into(), Value::String(self.skill_path.clone()));
        m.insert(
            "files".into(),
            Value::Array(self.files.iter().cloned().map(Value::String).collect()),
        );
        m
    }
}

// --- A2A Agent Card (aligned with the A2A protocol spec) ---

/// One skill declared by an agent card. Unknown keys are dropped like the
/// Pydantic model (no `extra="allow"`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSkill {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default, rename = "inputModes")]
    pub input_modes: Vec<String>,
    #[serde(default, rename = "outputModes")]
    pub output_modes: Vec<String>,
}

fn strings(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

impl AgentSkill {
    fn to_json(&self, exclude_defaults: bool) -> Map<String, Value> {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value, is_default: bool| {
            if !(exclude_defaults && is_default) {
                m.insert(k.to_string(), v);
            }
        };
        put("id", Value::String(self.id.clone()), self.id.is_empty());
        put("name", Value::String(self.name.clone()), false);
        put(
            "description",
            Value::String(self.description.clone()),
            self.description.is_empty(),
        );
        put("tags", strings(&self.tags), self.tags.is_empty());
        put("examples", strings(&self.examples), self.examples.is_empty());
        put(
            "inputModes",
            strings(&self.input_modes),
            self.input_modes.is_empty(),
        );
        put(
            "outputModes",
            strings(&self.output_modes),
            self.output_modes.is_empty(),
        );
        m
    }
}

/// An A2A agent card. Non standard top level fields are preserved in
/// `extra` (Pydantic `extra="allow"`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, rename = "preferredTransport")]
    pub preferred_transport: String,
    #[serde(default)]
    pub provider: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Value>,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    #[serde(default, rename = "defaultInputModes")]
    pub default_input_modes: Vec<String>,
    #[serde(default, rename = "defaultOutputModes")]
    pub default_output_modes: Vec<String>,
    #[serde(default, rename = "documentationUrl")]
    pub documentation_url: String,
    #[serde(default, rename = "iconUrl")]
    pub icon_url: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Which Pydantic dump projection to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpMode {
    /// `model_dump()`.
    Full,
    /// `model_dump(exclude_none=True)`.
    ExcludeNone,
    /// `model_dump(exclude_defaults=True)`.
    ExcludeDefaults,
}

impl AgentCard {
    /// Build a card from a JSON object, rejecting missing `name` or
    /// `description` like the Pydantic constructor.
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(v)
    }

    /// Dump the card as a JSON object in the requested projection.
    pub fn to_json(&self, mode: DumpMode) -> Map<String, Value> {
        let ex_def = mode == DumpMode::ExcludeDefaults;
        let ex_none = mode == DumpMode::ExcludeNone;
        let mut m = Map::new();
        let put_str = |m: &mut Map<String, Value>, k: &str, v: &str| {
            if !(ex_def && v.is_empty()) {
                m.insert(k.to_string(), Value::String(v.to_string()));
            }
        };
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("description".into(), Value::String(self.description.clone()));
        put_str(&mut m, "version", &self.version);
        put_str(&mut m, "protocolVersion", &self.protocol_version);
        put_str(&mut m, "url", &self.url);
        put_str(&mut m, "preferredTransport", &self.preferred_transport);
        for (k, v) in [("provider", &self.provider), ("capabilities", &self.capabilities)] {
            match v {
                Some(val) => {
                    m.insert(k.to_string(), val.clone());
                }
                None => {
                    if !(ex_def || ex_none) {
                        m.insert(k.to_string(), Value::Null);
                    }
                }
            }
        }
        if !(ex_def && self.skills.is_empty()) {
            m.insert(
                "skills".into(),
                Value::Array(
                    self.skills
                        .iter()
                        .map(|s| Value::Object(s.to_json(ex_def)))
                        .collect(),
                ),
            );
        }
        if !(ex_def && self.default_input_modes.is_empty()) {
            m.insert("defaultInputModes".into(), strings(&self.default_input_modes));
        }
        if !(ex_def && self.default_output_modes.is_empty()) {
            m.insert("defaultOutputModes".into(), strings(&self.default_output_modes));
        }
        put_str(&mut m, "documentationUrl", &self.documentation_url);
        put_str(&mut m, "iconUrl", &self.icon_url);
        for (k, v) in &self.extra {
            if ex_none && v.is_null() {
                continue;
            }
            m.insert(k.clone(), v.clone());
        }
        m
    }
}

// --- Registry entry (internal unified representation) ---

/// Internal unified representation of a registered service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub service_id: String,
    #[serde(rename = "type")]
    pub r#type: ServiceType,
    pub source: Source,
    #[serde(default)]
    pub service_data: Option<GenericServiceData>,
    #[serde(default)]
    pub agent_card: Option<AgentCard>,
    #[serde(default)]
    pub agent_card_url: Option<String>,
    #[serde(default)]
    pub skill_data: Option<SkillData>,
    /// Owning principal id on auth-required namespaces. `None` for legacy,
    /// `user_config` and anonymous entries.
    #[serde(default)]
    pub owner_id: Option<String>,
    /// Heartbeat lease TTL requested at registration. `None` means permanent.
    #[serde(default)]
    pub lease_ttl: Option<i64>,
}

impl RegistryEntry {
    /// Display name used by the CLI.
    pub fn display_name(&self) -> String {
        match (
            self.r#type,
            &self.service_data,
            &self.skill_data,
            &self.agent_card,
        ) {
            (ServiceType::Generic, Some(sd), _, _) => sd.name.clone(),
            (ServiceType::Skill, _, Some(sk), _) => sk.name.clone(),
            (_, _, _, Some(card)) => card.name.clone(),
            _ => self.service_id.clone(),
        }
    }

    /// Description used by the CLI.
    pub fn display_description(&self) -> String {
        match (
            self.r#type,
            &self.service_data,
            &self.skill_data,
            &self.agent_card,
        ) {
            (ServiceType::Generic, Some(sd), _, _) => sd.description.clone(),
            (ServiceType::Skill, _, Some(sk), _) => sk.description.clone(),
            (_, _, _, Some(card)) => card.description.clone(),
            _ => String::new(),
        }
    }

    /// `model_dump(exclude_none=True)` used by the CLI JSON output.
    pub fn to_json_exclude_none(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("service_id".into(), Value::String(self.service_id.clone()));
        m.insert("type".into(), Value::String(self.r#type.as_str().into()));
        m.insert("source".into(), Value::String(self.source.as_str().into()));
        if let Some(sd) = &self.service_data {
            m.insert("service_data".into(), Value::Object(sd.to_json_exclude_none()));
        }
        if let Some(card) = &self.agent_card {
            m.insert(
                "agent_card".into(),
                Value::Object(card.to_json(DumpMode::ExcludeNone)),
            );
        }
        if let Some(url) = &self.agent_card_url {
            m.insert("agent_card_url".into(), Value::String(url.clone()));
        }
        if let Some(sk) = &self.skill_data {
            m.insert("skill_data".into(), Value::Object(sk.to_json()));
        }
        if let Some(o) = &self.owner_id {
            m.insert("owner_id".into(), Value::String(o.clone()));
        }
        if let Some(t) = self.lease_ttl {
            m.insert("lease_ttl".into(), Value::from(t));
        }
        m
    }
}

// --- HTTP request models ---

fn default_dataset() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

/// Body of `POST /api/datasets/{dataset}/services/generic`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisterGenericRequest {
    #[serde(default)]
    pub service_id: Option<String>,
    #[serde(default = "default_dataset")]
    pub dataset: String,
    pub name: String,
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Map<String, Value>,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_true")]
    pub persistent: bool,
    /// Optional heartbeat lease request, validated by the router.
    #[serde(default)]
    pub lease_ttl: Option<i64>,
}

impl RegisterGenericRequest {
    pub fn new(dataset: impl Into<String>, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            service_id: None,
            dataset: dataset.into(),
            name: name.into(),
            description: description.into(),
            input_schema: Map::new(),
            url: String::new(),
            persistent: true,
            lease_ttl: None,
        }
    }
}

/// Body of `POST /api/datasets/{dataset}/services/a2a`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisterA2ARequest {
    #[serde(default)]
    pub service_id: Option<String>,
    #[serde(default = "default_dataset")]
    pub dataset: String,
    #[serde(default)]
    pub agent_card: Option<AgentCard>,
    #[serde(default)]
    pub agent_card_url: Option<String>,
    #[serde(default = "default_true")]
    pub persistent: bool,
    #[serde(default)]
    pub lease_ttl: Option<i64>,
}

impl RegisterA2ARequest {
    pub fn with_card(dataset: impl Into<String>, card: AgentCard) -> Self {
        Self {
            service_id: None,
            dataset: dataset.into(),
            agent_card: Some(card),
            agent_card_url: None,
            persistent: true,
            lease_ttl: None,
        }
    }

    pub fn with_url(dataset: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            service_id: None,
            dataset: dataset.into(),
            agent_card: None,
            agent_card_url: Some(url.into()),
            persistent: true,
            lease_ttl: None,
        }
    }
}

// --- HTTP response models ---

/// Response of the register endpoints. `status` is `registered` or `updated`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub service_id: String,
    pub dataset: String,
    pub status: String,
    #[serde(default)]
    pub lease_ttl: Option<i64>,
    #[serde(default)]
    pub lease_expires_at: Option<f64>,
}

/// Response of `DELETE /services/{service_id}`. `status` is `deregistered`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeregisterResponse {
    pub service_id: String,
    pub status: String,
}

/// Response of `PUT /services/{service_id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateResponse {
    pub service_id: String,
    pub dataset: String,
    pub status: String,
    #[serde(default)]
    pub changed_fields: Vec<String>,
    #[serde(default)]
    pub taxonomy_affected: bool,
}

/// Response of the skill endpoints. `status` is `registered`, `updated`,
/// `deleted` or `not_found`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillResponse {
    pub name: String,
    pub dataset: String,
    #[serde(default)]
    pub service_id: String,
    pub status: String,
}

/// Summary returned by `RegistryService::get_status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegistryStatus {
    pub total_services: usize,
    pub by_source: BTreeMap<String, usize>,
    pub datasets: Vec<String>,
}

/// Body of `POST /api/datasets/{dataset}/build`. Every field is optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildRequest {
    /// `no`, `yes` or `keyword`.
    #[serde(default = "default_resume")]
    pub resume: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_threshold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_service_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_categories_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_leaf_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_batch_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_keywords_per_service: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_threshold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification_retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_refine_iterations: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_keywords: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_design: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_classify: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_design: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_design_small: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_classify: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_validate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_keywords: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_cross_domain: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<i64>,
    /// `DEBUG`, `INFO`, `WARNING` or `ERROR`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
}

fn default_resume() -> String {
    "no".to_string()
}

impl BuildRequest {
    /// Extra build parameters: every set field except `resume`, as the
    /// Python router passed them to the builder.
    pub fn extra_params(&self) -> Map<String, Value> {
        let mut m = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        m.remove("resume");
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    fn card() -> TestResult<AgentCard> {
        Ok(AgentCard::from_value(json!({
            "name": "n", "description": "d", "capabilities": {},
            "skills": [{"id": "s", "name": "s", "description": "s", "tags": ["t"]}],
            "status": "online", "custom": null
        }))?)
    }

    #[test]
    fn dump_projections_follow_pydantic() -> TestResult {
        let c = card()?;
        let full = c.to_json(DumpMode::Full);
        assert_eq!(full["provider"], Value::Null);
        assert_eq!(full["version"], "");
        assert_eq!(full["skills"][0]["examples"], json!([]));
        assert_eq!(full["status"], "online");

        let none = c.to_json(DumpMode::ExcludeNone);
        assert!(!none.contains_key("provider"));
        assert!(none.contains_key("capabilities"));
        assert!(!none.contains_key("custom"));
        assert_eq!(none["skills"][0]["inputModes"], json!([]));

        let def = c.to_json(DumpMode::ExcludeDefaults);
        assert_eq!(
            serde_json::to_string(&def)?,
            r#"{"name":"n","description":"d","capabilities":{},"skills":[{"id":"s","name":"s","description":"s","tags":["t"]}],"status":"online","custom":null}"#
        );
        Ok(())
    }

    #[test]
    fn card_requires_name_and_description() -> TestResult {
        assert!(AgentCard::from_value(json!({"name": "x"})).is_err());
        assert!(AgentCard::from_value(json!({"name": "x", "description": "y", "version": null})).is_err());
        Ok(())
    }

    #[test]
    fn build_request_extra_params() -> TestResult {
        let r: BuildRequest = serde_json::from_value(json!({"resume": "yes", "workers": 3}))?;
        let extra = r.extra_params();
        assert_eq!(extra.len(), 1);
        assert_eq!(extra["workers"], 3);
        Ok(())
    }
}
