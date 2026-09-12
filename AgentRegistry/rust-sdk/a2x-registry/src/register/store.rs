// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `RegistryStore`: per dataset file I/O.
//!
//! Manages `user_config.json` (read only), `api_config.json` (read and
//! write), `service.json` (write only), the per dataset config files and
//! the `skills/` folders. The api_config entries are cached in memory so a
//! write never re-reads the file.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use a2x_common::atomic::atomic_write_json;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::errors::{RegistryError, Result};
use super::models::{AgentCard, DumpMode, GenericServiceData, RegistryEntry, ServiceType, SkillData, Source};
use super::validation::normalize_format_config;

pub const USER_CONFIG_FILE: &str = "user_config.json";
pub const API_CONFIG_FILE: &str = "api_config.json";
pub const REGISTER_CONFIG_FILE: &str = "register_config.json";
pub const VECTOR_CONFIG_FILE: &str = "vector_config.json";
pub const AUTH_CONFIG_FILE: &str = "auth_config.json";
pub const LEASE_CONFIG_FILE: &str = "lease_config.json";
pub const SERVICE_JSON_FILE: &str = "service.json";
pub const SKILLS_DIR: &str = "skills";
pub const REMOVED_SKILLS_DIR: &str = "removed_skills";

/// Per namespace auth flag persisted in `auth_config.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    pub required: bool,
    pub schema_version: i64,
}

impl Default for AuthConfig {
    /// Missing file: the namespace is anonymous (backward compatible).
    fn default() -> Self {
        Self {
            required: false,
            schema_version: 1,
        }
    }
}

/// Per namespace heartbeat policy persisted in `lease_config.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseConfig {
    pub enabled: bool,
    pub min_ttl: i64,
    pub max_ttl: i64,
    pub grace_period: i64,
    pub schema_version: i64,
}

impl Default for LeaseConfig {
    /// Missing file: heartbeat unsupported, every registration permanent.
    fn default() -> Self {
        Self {
            enabled: false,
            min_ttl: 10,
            max_ttl: 3600,
            grace_period: 300,
            schema_version: 1,
        }
    }
}

impl LeaseConfig {
    /// JSON object in the on-disk key order.
    pub fn to_json(&self) -> Map<String, Value> {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }
}

/// Thread safe file I/O for a single dataset directory.
pub struct RegistryStore {
    dir: PathBuf,
    api_entries: Mutex<IndexMap<String, RegistryEntry>>,
}

impl RegistryStore {
    /// Open (and create) the dataset directory.
    pub fn new(dataset_dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dataset_dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            api_entries: Mutex::new(IndexMap::new()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // --- Read ---

    /// Read `user_config.json` (user maintained, system read only).
    pub fn load_user_config(&self) -> Vec<RegistryEntry> {
        let path = self.dir.join(USER_CONFIG_FILE);
        if !path.exists() {
            return Vec::new();
        }
        let entries = load_config_file(&path, Source::UserConfig);
        if entries.is_empty() && std::fs::metadata(&path).map(|m| m.len() > 10).unwrap_or(false) {
            tracing::error!(
                "user_config.json exists but produced 0 entries: {}",
                path.display()
            );
        }
        entries
    }

    /// Read `api_config.json` and cache it in memory.
    pub fn load_api_config(&self) -> Vec<RegistryEntry> {
        let entries = load_config_file(&self.dir.join(API_CONFIG_FILE), Source::ApiConfig);
        let mut guard = self.api_entries.lock();
        guard.clear();
        for e in &entries {
            guard.insert(e.service_id.clone(), e.clone());
        }
        entries
    }

    // --- Write api_config ---

    /// Upsert one entry into `api_config.json` (memory and disk).
    pub fn save_api_entry(&self, entry: &RegistryEntry) -> Result<()> {
        let mut guard = self.api_entries.lock();
        guard.insert(entry.service_id.clone(), entry.clone());
        self.flush_api_config(&guard)
    }

    /// Remove one entry from `api_config.json`. Returns true if found.
    pub fn remove_api_entry(&self, service_id: &str) -> Result<bool> {
        let mut guard = self.api_entries.lock();
        if guard.shift_remove(service_id).is_none() {
            return Ok(false);
        }
        self.flush_api_config(&guard)?;
        Ok(true)
    }

    /// Replace all api_config entries and write to disk.
    pub fn save_api_batch(&self, entries: &[RegistryEntry]) -> Result<()> {
        let mut guard = self.api_entries.lock();
        guard.clear();
        for e in entries {
            guard.insert(e.service_id.clone(), e.clone());
        }
        self.flush_api_config(&guard)
    }

    fn flush_api_config(&self, entries: &IndexMap<String, RegistryEntry>) -> Result<()> {
        let services: Vec<Value> = entries
            .values()
            .map(|e| Value::Object(entry_to_config_dict(e)))
            .collect();
        atomic_write_json(&self.dir.join(API_CONFIG_FILE), &json!({ "services": services }))?;
        Ok(())
    }

    // --- Skill folder I/O ---

    /// Scan `skills/*/SKILL.md` and return an entry for each valid skill.
    pub fn load_skills(&self) -> Vec<RegistryEntry> {
        let skills_dir = self.dir.join(SKILLS_DIR);
        if !skills_dir.is_dir() {
            return Vec::new();
        }
        let mut children: Vec<PathBuf> = std::fs::read_dir(&skills_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        children.sort();
        let mut entries = Vec::new();
        for child in children {
            let skill_md = child.join("SKILL.md");
            if !child.is_dir() || !skill_md.exists() {
                continue;
            }
            match parse_skill_md(&skill_md).map(|meta| {
                let name = meta["name"].clone();
                SkillData {
                    name: name.clone(),
                    description: meta["description"].clone(),
                    license: meta.get("license").cloned().unwrap_or_default(),
                    skill_path: format!("{SKILLS_DIR}/{name}"),
                    files: list_skill_files(&child),
                }
            }) {
                Ok(skill_data) => entries.push(RegistryEntry {
                    service_id: generate_service_id("skill", &skill_data.name),
                    r#type: ServiceType::Skill,
                    source: Source::SkillFolder,
                    service_data: None,
                    agent_card: None,
                    agent_card_url: None,
                    skill_data: Some(skill_data),
                    owner_id: None,
                    lease_ttl: None,
                }),
                Err(e) => tracing::warn!(
                    "Skipping invalid skill '{}': {}",
                    child.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    e
                ),
            }
        }
        entries
    }

    /// Extract a skill ZIP, validate `SKILL.md` and save to `skills/{name}/`.
    pub fn save_skill_zip(&self, zip_bytes: &[u8]) -> Result<SkillData> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
            .map_err(|e| RegistryError::Invalid(format!("Invalid ZIP file: {e}")))?;
        let names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .collect();
        for name in &names {
            if name.starts_with('/') || name.contains("..") {
                return Err(RegistryError::Invalid(format!("Unsafe path in ZIP: {name}")));
            }
        }
        let mut skill_md_path: Option<String> = None;
        let mut strip_prefix = String::new();
        if names.iter().any(|n| n == "SKILL.md") {
            skill_md_path = Some("SKILL.md".to_string());
        } else {
            let mut top_dirs: Vec<String> = names
                .iter()
                .filter(|n| n.contains('/'))
                .map(|n| n.split('/').next().unwrap_or("").to_string())
                .collect();
            top_dirs.sort();
            top_dirs.dedup();
            for td in top_dirs {
                let candidate = format!("{td}/SKILL.md");
                if names.contains(&candidate) {
                    skill_md_path = Some(candidate);
                    strip_prefix = format!("{td}/");
                    break;
                }
            }
        }
        let Some(skill_md_path) = skill_md_path else {
            return Err(RegistryError::Invalid(
                "ZIP must contain SKILL.md (at root or in a single top-level directory)".into(),
            ));
        };
        let mut content = String::new();
        archive
            .by_name(&skill_md_path)
            .map_err(|e| RegistryError::Invalid(format!("Invalid ZIP file: {e}")))?
            .read_to_string(&mut content)
            .map_err(|e| RegistryError::Invalid(format!("SKILL.md is not UTF-8: {e}")))?;
        let meta = parse_skill_md_content(&content)?;
        let name = meta["name"].clone();

        let target_dir = self.dir.join(SKILLS_DIR).join(&name);
        if target_dir.exists() {
            std::fs::remove_dir_all(&target_dir)?;
        }
        std::fs::create_dir_all(&target_dir)?;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| RegistryError::Invalid(format!("Invalid ZIP file: {e}")))?;
            if file.is_dir() {
                continue;
            }
            let mut rel = file.name().to_string();
            if !strip_prefix.is_empty() && rel.starts_with(&strip_prefix) {
                rel = rel[strip_prefix.len()..].to_string();
            }
            if rel.is_empty() {
                continue;
            }
            let dest = target_dir.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            std::fs::write(&dest, &buf)?;
        }

        Ok(SkillData {
            name: name.clone(),
            description: meta["description"].clone(),
            license: meta.get("license").cloned().unwrap_or_default(),
            skill_path: format!("{SKILLS_DIR}/{name}"),
            files: list_skill_files(&target_dir),
        })
    }

    /// Move a skill folder to `removed_skills/`. Returns true if it existed.
    pub fn remove_skill(&self, name: &str) -> Result<bool> {
        let skill_dir = self.dir.join(SKILLS_DIR).join(name);
        if !skill_dir.exists() {
            return Ok(false);
        }
        let removed_dir = self.dir.join(REMOVED_SKILLS_DIR);
        std::fs::create_dir_all(&removed_dir)?;
        let dest = removed_dir.join(name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&skill_dir, &dest)?;
        Ok(true)
    }

    /// Rename `skills/{old_name}/` to `skills/{new_name}/`.
    pub fn rename_skill(&self, old_name: &str, new_name: &str) -> Result<()> {
        if new_name.is_empty() || new_name == old_name {
            return Ok(());
        }
        let old_dir = self.dir.join(SKILLS_DIR).join(old_name);
        let new_dir = self.dir.join(SKILLS_DIR).join(new_name);
        if !old_dir.exists() {
            return Err(RegistryError::FileNotFound(format!(
                "Skill folder not found: {old_name}"
            )));
        }
        if new_dir.exists() {
            return Err(RegistryError::Invalid(format!(
                "Skill folder already exists: {new_name}"
            )));
        }
        std::fs::rename(&old_dir, &new_dir)?;
        Ok(())
    }

    /// Upsert frontmatter keys in `skills/{name}/SKILL.md`, preserving the
    /// body and any keys not in `updates`.
    pub fn update_skill_md(&self, name: &str, updates: &IndexMap<String, String>) -> Result<()> {
        let skill_md = self.dir.join(SKILLS_DIR).join(name).join("SKILL.md");
        if !skill_md.exists() {
            return Err(RegistryError::FileNotFound(format!(
                "SKILL.md not found for skill '{name}'"
            )));
        }
        let content = std::fs::read_to_string(&skill_md)?;
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Err(RegistryError::Invalid(
                "SKILL.md must have YAML frontmatter delimited by ---".into(),
            ));
        }
        let fm_body = parts[1].trim_matches('\n');
        let body = parts[2];
        let mut new_lines: Vec<String> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for line in fm_body.split('\n') {
            let update = split_frontmatter_line(line).and_then(|(indent, key, sep, _)| {
                updates.get_key_value(key).map(|(k, v)| (indent, k, sep, v))
            });
            match update {
                Some((indent, key, sep, value)) => {
                    new_lines.push(format!("{indent}{key}{sep}{value}"));
                    seen.push(key.as_str());
                }
                None => new_lines.push(line.to_string()),
            }
        }
        for (k, v) in updates {
            if !seen.contains(&k.as_str()) {
                new_lines.push(format!("{k}: {v}"));
            }
        }
        let new_content = format!("---\n{}\n---{}", new_lines.join("\n"), body);
        std::fs::write(&skill_md, new_content)?;
        Ok(())
    }

    /// Pack a skill folder into an in-memory ZIP.
    pub fn get_skill_zip(&self, name: &str) -> Result<Vec<u8>> {
        let skill_dir = self.dir.join(SKILLS_DIR).join(name);
        if !skill_dir.is_dir() {
            return Err(RegistryError::FileNotFound(format!(
                "Skill folder not found: {name}"
            )));
        }
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zf = zip::ZipWriter::new(&mut buf);
            let options =
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for rel in list_skill_files(&skill_dir) {
                zf.start_file(&rel, options)
                    .map_err(|e| RegistryError::Io(format!("zip write failed: {e}")))?;
                let data = std::fs::read(skill_dir.join(&rel))?;
                zf.write_all(&data)?;
            }
            zf.finish()
                .map_err(|e| RegistryError::Io(format!("zip finish failed: {e}")))?;
        }
        Ok(buf.into_inner())
    }

    // --- Write service.json ---

    /// Atomically write the output `service.json`.
    pub fn write_service_json(&self, services: &[Value]) -> Result<()> {
        atomic_write_json(&self.dir.join(SERVICE_JSON_FILE), &services)?;
        Ok(())
    }

    // --- Register format config ---

    /// Read `register_config.json`.
    ///
    /// `None` when the file is missing (caller substitutes defaults). An
    /// empty map when the file exists but is malformed or declares no valid
    /// format (a hard ban on every type).
    pub fn load_register_config(&self) -> Option<HashMap<String, String>> {
        let path = self.dir.join(REGISTER_CONFIG_FILE);
        if !path.exists() {
            return None;
        }
        match read_json(&path) {
            Ok(data) => Some(normalize_format_config(data.get("formats"))),
            Err(e) => {
                tracing::warn!("Failed to load {}: {}", path.display(), e);
                Some(HashMap::new())
            }
        }
    }

    /// Persist `register_config.json` with a normalized formats map.
    pub fn write_register_config(&self, formats: &HashMap<String, String>) -> Result<()> {
        let mut ordered = Map::new();
        for t in super::validation::SUPPORTED_SERVICE_TYPES {
            if let Some(v) = formats.get(*t) {
                ordered.insert((*t).to_string(), Value::String(v.clone()));
            }
        }
        for (t, v) in formats {
            ordered
                .entry(t.clone())
                .or_insert_with(|| Value::String(v.clone()));
        }
        atomic_write_json(
            &self.dir.join(REGISTER_CONFIG_FILE),
            &json!({ "formats": ordered }),
        )?;
        Ok(())
    }

    /// Read `vector_config.json`. `None` when missing or malformed.
    pub fn load_vector_config(&self) -> Option<Map<String, Value>> {
        let path = self.dir.join(VECTOR_CONFIG_FILE);
        if !path.exists() {
            return None;
        }
        match read_json(&path) {
            Ok(Value::Object(m)) => Some(m),
            _ => None,
        }
    }

    /// Persist `vector_config.json`.
    pub fn write_vector_config(&self, embedding_model: &str, embedding_dim: i64) -> Result<()> {
        atomic_write_json(
            &self.dir.join(VECTOR_CONFIG_FILE),
            &json!({"embedding_model": embedding_model, "embedding_dim": embedding_dim}),
        )?;
        Ok(())
    }

    // --- Auth config ---

    /// Read `auth_config.json`; a missing or malformed file yields the
    /// anonymous default and never fails.
    pub fn load_auth_config(&self) -> AuthConfig {
        let path = self.dir.join(AUTH_CONFIG_FILE);
        if !path.exists() {
            return AuthConfig::default();
        }
        let data = match read_json(&path) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                tracing::warn!(
                    "Malformed {} (not a dict); falling back to default",
                    path.display()
                );
                return AuthConfig::default();
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load {}: {}; falling back to default",
                    path.display(),
                    e
                );
                return AuthConfig::default();
            }
        };
        let required = data.get("required").map(crate::util::py_truthy).unwrap_or(false);
        let schema_version = match data.get("schema_version") {
            None => 1,
            Some(v) => match crate::util::py_int(v) {
                Some(i) => i,
                None => {
                    tracing::warn!(
                        "Failed to load {}: bad schema_version; falling back",
                        path.display()
                    );
                    return AuthConfig::default();
                }
            },
        };
        AuthConfig {
            required,
            schema_version,
        }
    }

    /// Persist `auth_config.json`.
    pub fn write_auth_config(&self, required: bool) -> Result<()> {
        atomic_write_json(
            &self.dir.join(AUTH_CONFIG_FILE),
            &json!({"required": required, "schema_version": 1}),
        )?;
        Ok(())
    }

    // --- Lease (heartbeat) config ---

    /// Read `lease_config.json`; a missing or malformed file yields the
    /// disabled default. Loaded values are coerced and clamped.
    pub fn load_lease_config(&self) -> LeaseConfig {
        let path = self.dir.join(LEASE_CONFIG_FILE);
        if !path.exists() {
            return LeaseConfig::default();
        }
        let data = match read_json(&path) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                tracing::warn!(
                    "Malformed {} (not a dict); falling back to default",
                    path.display()
                );
                return LeaseConfig::default();
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load {}: {}; falling back to default",
                    path.display(),
                    e
                );
                return LeaseConfig::default();
            }
        };
        let d = LeaseConfig::default();
        let int_or = |key: &str, default: i64| -> Option<i64> {
            match data.get(key) {
                None => Some(default),
                Some(v) => crate::util::py_int(v),
            }
        };
        let parsed = (|| {
            let enabled = data.get("enabled").map(crate::util::py_truthy).unwrap_or(false);
            let min_ttl = int_or("min_ttl", d.min_ttl)?.max(1);
            let max_ttl = int_or("max_ttl", d.max_ttl)?.max(min_ttl);
            let grace = int_or("grace_period", d.grace_period)?.max(0);
            let schema_version = int_or("schema_version", 1)?;
            Some(LeaseConfig {
                enabled,
                min_ttl,
                max_ttl,
                grace_period: grace,
                schema_version,
            })
        })();
        parsed.unwrap_or_else(|| {
            tracing::warn!(
                "Failed to load {}: bad integer field; falling back",
                path.display()
            );
            LeaseConfig::default()
        })
    }

    /// Persist `lease_config.json`. Bounds are validated here.
    pub fn write_lease_config(
        &self,
        enabled: bool,
        min_ttl: i64,
        max_ttl: i64,
        grace_period: i64,
    ) -> Result<()> {
        if min_ttl < 1 {
            return Err(RegistryError::Invalid(format!(
                "min_ttl must be >= 1, got {min_ttl}"
            )));
        }
        if max_ttl < min_ttl {
            return Err(RegistryError::Invalid(format!(
                "max_ttl ({max_ttl}) must be >= min_ttl ({min_ttl})"
            )));
        }
        if grace_period < 0 {
            return Err(RegistryError::Invalid(format!(
                "grace_period must be >= 0, got {grace_period}"
            )));
        }
        atomic_write_json(
            &self.dir.join(LEASE_CONFIG_FILE),
            &LeaseConfig {
                enabled,
                min_ttl,
                max_ttl,
                grace_period,
                schema_version: 1,
            },
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Module level utilities
// ---------------------------------------------------------------------------

fn read_json(path: &Path) -> std::result::Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Deterministic service id: `{prefix}_{sha256(name)[:16]}`.
pub fn generate_service_id(type_prefix: &str, name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    format!("{type_prefix}_{}", &hex::encode(digest)[..16])
}

/// Parse a config file (`user_config` or `api_config` layout) into entries.
fn load_config_file(path: &Path, source: Source) -> Vec<RegistryEntry> {
    if !path.exists() {
        return Vec::new();
    }
    let data = match read_json(path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to load {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    let services = data
        .get("services")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for svc in services {
        let sid = svc
            .get("service_id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        match parse_service_entry(&svc, source) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(e) => tracing::warn!("Skipping invalid entry in {}: {} - {}", path.display(), sid, e),
        }
    }
    entries
}

/// Parse a single service object from a config file.
fn parse_service_entry(svc: &Value, source: Source) -> std::result::Result<Option<RegistryEntry>, String> {
    let Value::Object(map) = svc else {
        return Err("entry is not an object".into());
    };
    let svc_type = map.get("type").and_then(Value::as_str).unwrap_or("generic");
    let mut service_id = map
        .get("service_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let owner_id = map
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let lease_ttl = map
        .get("lease_ttl")
        .and_then(|v| match v {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
            _ => None,
        })
        .filter(|t| *t > 0);

    match svc_type {
        "generic" => {
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let desc = map
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() || desc.is_empty() {
                return Ok(None);
            }
            if service_id.is_empty() {
                service_id = generate_service_id("generic", &name);
            }
            let input_schema = match map.get("inputSchema") {
                None => Map::new(),
                Some(Value::Object(o)) => o.clone(),
                Some(_) => return Err("inputSchema must be an object".into()),
            };
            let url = match map.get("url") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err("url must be a string".into()),
            };
            Ok(Some(RegistryEntry {
                service_id,
                r#type: ServiceType::Generic,
                source,
                service_data: Some(GenericServiceData {
                    name,
                    description: desc,
                    input_schema,
                    url,
                }),
                agent_card: None,
                agent_card_url: None,
                skill_data: None,
                owner_id,
                lease_ttl,
            }))
        }
        "a2a" => {
            let card_url = map
                .get("agent_card_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let agent_card = match map.get("agent_card") {
                Some(v) if crate::util::py_truthy(v) => {
                    Some(AgentCard::from_value(v.clone()).map_err(|e| e.to_string())?)
                }
                _ => None,
            };
            if service_id.is_empty() {
                let name = agent_card
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| card_url.clone().unwrap_or_else(|| "unknown".into()));
                service_id = generate_service_id("agent", &name);
            }
            Ok(Some(RegistryEntry {
                service_id,
                r#type: ServiceType::A2a,
                source,
                service_data: None,
                agent_card,
                agent_card_url: card_url,
                skill_data: None,
                owner_id,
                lease_ttl,
            }))
        }
        _ => Ok(None),
    }
}

/// Convert an entry back to the config file layout. `owner_id` and
/// `lease_ttl` are omitted when unset so pre-auth files round trip
/// byte for byte.
pub fn entry_to_config_dict(entry: &RegistryEntry) -> Map<String, Value> {
    let mut d = Map::new();
    d.insert("type".into(), Value::String(entry.r#type.as_str().into()));
    d.insert("service_id".into(), Value::String(entry.service_id.clone()));
    match entry.r#type {
        ServiceType::Generic => {
            if let Some(sd) = &entry.service_data {
                d.insert("name".into(), Value::String(sd.name.clone()));
                d.insert("description".into(), Value::String(sd.description.clone()));
                if !sd.input_schema.is_empty() {
                    d.insert("inputSchema".into(), Value::Object(sd.input_schema.clone()));
                }
                if let Some(url) = sd.url.as_ref().filter(|u| !u.is_empty()) {
                    d.insert("url".into(), Value::String(url.clone()));
                }
            }
        }
        ServiceType::A2a => {
            if let Some(url) = entry.agent_card_url.as_ref().filter(|u| !u.is_empty()) {
                d.insert("agent_card_url".into(), Value::String(url.clone()));
            }
            if let Some(card) = &entry.agent_card {
                d.insert(
                    "agent_card".into(),
                    Value::Object(card.to_json(DumpMode::ExcludeDefaults)),
                );
            }
        }
        ServiceType::Skill => {}
    }
    if let Some(o) = entry.owner_id.as_ref().filter(|o| !o.is_empty()) {
        d.insert("owner_id".into(), Value::String(o.clone()));
    }
    if let Some(t) = entry.lease_ttl.filter(|t| *t != 0) {
        d.insert("lease_ttl".into(), Value::from(t));
    }
    d
}

/// Split a YAML frontmatter line into indent, key, separator and value.
/// The key starts with a letter or underscore and continues with word
/// characters or dashes; the separator is the colon with surrounding
/// whitespace. Lines of any other shape yield `None`.
fn split_frontmatter_line(line: &str) -> Option<(&str, &str, &str, &str)> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let key_len = rest
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '-'))
        .map_or(rest.len(), |(i, _)| i);
    let (key, after_key) = rest.split_at(key_len);
    let colon = after_key.find(':')?;
    if !after_key[..colon].chars().all(char::is_whitespace) {
        return None;
    }
    let after_colon = &after_key[colon + 1..];
    let value_start = after_colon.len() - after_colon.trim_start().len();
    let (sep, value) = after_key.split_at(colon + 1 + value_start);
    Some((indent, key, sep, value))
}

/// Parse a `SKILL.md` file from disk.
pub fn parse_skill_md(path: &Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)?;
    parse_skill_md_content(&content)
}

/// Parse `SKILL.md` YAML frontmatter. Returns `name`, `description` and
/// optionally `license`. Fails when `name` or `description` is missing.
/// The `name`, `description` or `license` field of a trimmed frontmatter
/// line, with its raw value text; `None` for any other line or an empty value.
fn frontmatter_field(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(':')?;
    if !matches!(key, "name" | "description" | "license") {
        return None;
    }
    let raw = rest.trim_start();
    if raw.is_empty() {
        return None;
    }
    Some((key, raw))
}

pub fn parse_skill_md_content(content: &str) -> Result<HashMap<String, String>> {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return Err(RegistryError::Invalid(
            "SKILL.md must have YAML frontmatter delimited by ---".into(),
        ));
    }
    let mut result = HashMap::new();
    for line in parts[1].trim().lines() {
        if let Some((key, raw)) = frontmatter_field(line.trim()) {
            let mut val = raw.trim().to_string();
            let bytes = val.as_bytes();
            if val.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') && bytes[bytes.len() - 1] == bytes[0]
            {
                val = val[1..val.len() - 1].to_string();
            }
            result.insert(key.to_string(), val);
        }
    }
    if result.get("name").map(|s| s.is_empty()).unwrap_or(true) {
        return Err(RegistryError::Invalid(
            "SKILL.md frontmatter must include 'name'".into(),
        ));
    }
    if result.get("description").map(|s| s.is_empty()).unwrap_or(true) {
        return Err(RegistryError::Invalid(
            "SKILL.md frontmatter must include 'description'".into(),
        ));
    }
    Ok(result)
}

/// All files in a skill folder as sorted relative POSIX paths.
pub fn list_skill_files(skill_dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = walkdir::WalkDir::new(skill_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            e.path().strip_prefix(skill_dir).ok().map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use serde_json::json;

    fn make_zip(files: &[(&str, &str)]) -> TestResult<Vec<u8>> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zf = zip::ZipWriter::new(&mut buf);
            for (name, content) in files {
                zf.start_file(*name, zip::write::SimpleFileOptions::default())?;
                zf.write_all(content.as_bytes())?;
            }
            zf.finish()?;
        }
        Ok(buf.into_inner())
    }

    #[test]
    fn service_id_is_deterministic() -> TestResult {
        assert_eq!(
            generate_service_id("generic", "Calculator").len(),
            "generic_".len() + 16
        );
        assert_eq!(
            generate_service_id("agent", "x"),
            generate_service_id("agent", "x")
        );
        Ok(())
    }

    #[test]
    fn skill_md_parsing() -> TestResult {
        let meta =
            parse_skill_md_content("---\nname: \"art\"\ndescription: 'Make art'\nlicense: MIT\n---\nbody")?;
        assert_eq!(meta["name"], "art");
        assert_eq!(meta["description"], "Make art");
        assert_eq!(meta["license"], "MIT");
        assert!(parse_skill_md_content("no frontmatter").is_err());
        assert!(parse_skill_md_content("---\nname: x\n---").is_err());
        Ok(())
    }

    #[test]
    fn api_config_round_trip_omits_none_fields() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RegistryStore::new(dir.path().join("ds"))?;
        let entry = RegistryEntry {
            service_id: "generic_1".into(),
            r#type: ServiceType::Generic,
            source: Source::ApiConfig,
            service_data: Some(GenericServiceData {
                name: "n".into(),
                description: "d".into(),
                input_schema: Map::new(),
                url: Some(String::new()),
            }),
            agent_card: None,
            agent_card_url: None,
            skill_data: None,
            owner_id: None,
            lease_ttl: None,
        };
        store.save_api_entry(&entry)?;
        let text = std::fs::read_to_string(dir.path().join("ds").join(API_CONFIG_FILE))?;
        assert_eq!(
            text,
            "{\n  \"services\": [\n    {\n      \"type\": \"generic\",\n      \"service_id\": \"generic_1\",\n      \"name\": \"n\",\n      \"description\": \"d\"\n    }\n  ]\n}\n"
        );
        let loaded = store.load_api_config();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].service_data.as_ref().required()?.url, None);
        assert!(store.remove_api_entry("generic_1")?);
        assert!(!store.remove_api_entry("generic_1")?);
        Ok(())
    }

    #[test]
    fn a2a_config_with_owner_and_lease() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RegistryStore::new(dir.path().join("ds"))?;
        std::fs::write(
            dir.path().join("ds").join(API_CONFIG_FILE),
            json!({"services": [
                {"type": "a2a", "agent_card": {"name": "a", "description": "b", "status": "online"}, "owner_id": "u_1", "lease_ttl": 30},
                {"type": "a2a", "agent_card_url": "http://x/card.json"},
                {"type": "a2a", "agent_card": {"description": "missing name"}},
                {"type": "bogus"}
            ]})
            .to_string(),
        )
        ?;
        let entries = store.load_api_config();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].owner_id.as_deref(), Some("u_1"));
        assert_eq!(entries[0].lease_ttl, Some(30));
        assert_eq!(entries[0].service_id, generate_service_id("agent", "a"));
        assert_eq!(
            entries[1].service_id,
            generate_service_id("agent", "http://x/card.json")
        );
        let d = entry_to_config_dict(&entries[0]);
        assert_eq!(d["agent_card"]["status"], "online");
        assert!(!d["agent_card"].as_object().required()?.contains_key("version"));
        assert_eq!(d["owner_id"], "u_1");
        assert_eq!(d["lease_ttl"], 30);
        Ok(())
    }

    #[test]
    fn skill_zip_lifecycle() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RegistryStore::new(dir.path().join("ds"))?;
        let zip_bytes = make_zip(&[
            (
                "my-skill/SKILL.md",
                "---\nname: my-skill\ndescription: Does things\n---\n# Body\n",
            ),
            ("my-skill/scripts/run.py", "print(1)\n"),
        ])?;
        let data = store.save_skill_zip(&zip_bytes)?;
        assert_eq!(data.name, "my-skill");
        assert_eq!(data.skill_path, "skills/my-skill");
        assert_eq!(
            data.files,
            vec!["SKILL.md".to_string(), "scripts/run.py".to_string()]
        );

        let entries = store.load_skills();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, Source::SkillFolder);

        let mut updates = IndexMap::new();
        updates.insert("description".to_string(), "Better".to_string());
        updates.insert("license".to_string(), "MIT".to_string());
        store.update_skill_md("my-skill", &updates)?;
        let md = std::fs::read_to_string(dir.path().join("ds/skills/my-skill/SKILL.md"))?;
        assert_eq!(
            md,
            "---\nname: my-skill\ndescription: Better\nlicense: MIT\n---\n# Body\n"
        );

        store.rename_skill("my-skill", "renamed")?;
        assert!(store.rename_skill("missing", "x").is_err());
        let packed = store.get_skill_zip("renamed")?;
        let mut archive = zip::ZipArchive::new(Cursor::new(packed))?;
        let names: Vec<String> = (0..archive.len())
            .map(|i| -> TestResult<_> { Ok(archive.by_index(i)?.name().to_string()) })
            .collect::<TestResult<Vec<_>>>()?;
        assert_eq!(names, vec!["SKILL.md".to_string(), "scripts/run.py".to_string()]);

        assert!(store.remove_skill("renamed")?);
        assert!(dir.path().join("ds/removed_skills/renamed/SKILL.md").exists());
        assert!(!store.remove_skill("renamed")?);

        let bad = make_zip(&[("../evil", "x")])?;
        assert!(matches!(
            store.save_skill_zip(&bad),
            Err(RegistryError::Invalid(_))
        ));
        let no_md = make_zip(&[("a/b.txt", "x")])?;
        assert!(
            store
                .save_skill_zip(&no_md)
                .err_or_fail()?
                .to_string()
                .contains("SKILL.md")
        );
        Ok(())
    }

    #[test]
    fn config_files_defaults_and_coercion() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RegistryStore::new(dir.path().join("ds"))?;
        assert_eq!(store.load_register_config(), None);
        assert_eq!(store.load_auth_config(), AuthConfig::default());
        assert_eq!(store.load_lease_config(), LeaseConfig::default());
        std::fs::write(
            dir.path().join("ds").join(LEASE_CONFIG_FILE),
            r#"{"enabled": 1, "min_ttl": "0", "max_ttl": 5.9, "grace_period": -3}"#,
        )?;
        let cfg = store.load_lease_config();
        assert_eq!(
            (cfg.enabled, cfg.min_ttl, cfg.max_ttl, cfg.grace_period),
            (true, 1, 5, 0)
        );
        std::fs::write(dir.path().join("ds").join(LEASE_CONFIG_FILE), "[]")?;
        assert_eq!(store.load_lease_config(), LeaseConfig::default());
        std::fs::write(dir.path().join("ds").join(REGISTER_CONFIG_FILE), "not json")?;
        assert_eq!(store.load_register_config(), Some(HashMap::new()));
        assert!(store.write_lease_config(true, 0, 10, 1).is_err());
        assert!(store.write_lease_config(true, 20, 10, 1).is_err());
        assert!(store.write_lease_config(true, 5, 10, -1).is_err());
        store.write_lease_config(true, 5, 10, 1)?;
        assert_eq!(store.load_lease_config().max_ttl, 10);
        store.write_auth_config(true)?;
        assert!(store.load_auth_config().required);
        let mut fm = HashMap::new();
        fm.insert("a2a".to_string(), "v1.0".to_string());
        store.write_register_config(&fm)?;
        assert_eq!(store.load_register_config().required()?["a2a"], "v1.0");
        Ok(())
    }
}
