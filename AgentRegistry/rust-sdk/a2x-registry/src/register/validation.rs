// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Unified format validation for registered services.
//!
//! Each service type has a [`FormatValidator`] with an ordered list of
//! supported protocol versions (oldest first). A dataset declares, per
//! type, the oldest version it still accepts (`min_version`). Validation
//! runs from that version upwards and succeeds on the first passing
//! version. All `v0.0` checks only require `name` and `description`.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use serde_json::{Map, Value};

use super::models::{AgentCard, DumpMode};

/// Outcome of a format validation run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationResult {
    pub valid: bool,
    pub service_type: Option<String>,
    /// First (oldest) version that passed, or `None` when nothing passed.
    pub matched_version: Option<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    fn failure(service_type: &str, errors: Vec<String>) -> Self {
        Self {
            valid: false,
            service_type: Some(service_type.to_string()),
            matched_version: None,
            errors,
            warnings: Vec::new(),
        }
    }
}

fn has_text(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::String(s)) if !s.trim().is_empty())
}

/// Turn `v0.0` or `v1.2.3` into a sortable list of integers.
pub fn version_key(version: &str) -> Result<Vec<u64>, String> {
    if version.is_empty() {
        return Err(format!("Invalid version: {version:?}"));
    }
    let v = version.strip_prefix('v').unwrap_or(version);
    v.split('.')
        .map(|p| {
            p.parse::<u64>()
                .map_err(|_| format!("Invalid version string: {version:?}"))
        })
        .collect()
}

/// Normalize a card, object or nothing into a JSON object.
fn to_map(payload: &Value) -> Map<String, Value> {
    match payload {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    v.map(crate::util::py_truthy).unwrap_or(false)
}

/// Validator for one service type.
pub trait FormatValidator: Send + Sync {
    /// Unique type key: `generic`, `a2a` or `skill`.
    fn service_type(&self) -> &'static str;

    /// Supported versions ordered oldest to newest.
    fn supported_versions(&self) -> &'static [&'static str];

    /// Return `(errors, warnings)` for the given version. Empty errors
    /// means the version passed.
    fn check_version(&self, payload: &Map<String, Value>, version: &str) -> (Vec<String>, Vec<String>);

    /// Try each supported version at or above `min_version`, oldest first.
    fn validate(&self, payload: &Value, min_version: &str) -> ValidationResult {
        let threshold = match version_key(min_version) {
            Ok(k) => k,
            Err(e) => return ValidationResult::failure(self.service_type(), vec![e]),
        };
        let versions: Vec<&str> = self
            .supported_versions()
            .iter()
            .copied()
            .filter(|v| version_key(v).map(|k| k >= threshold).unwrap_or(false))
            .collect();
        if versions.is_empty() {
            return ValidationResult::failure(
                self.service_type(),
                vec![format!(
                    "No {} version >= {}. Supported: {}",
                    self.service_type(),
                    min_version,
                    py_list(self.supported_versions())
                )],
            );
        }
        let data = to_map(payload);
        let mut last_errors = Vec::new();
        let mut last_warnings = Vec::new();
        for version in &versions {
            let (errs, warns) = self.check_version(&data, version);
            if errs.is_empty() {
                return ValidationResult {
                    valid: true,
                    service_type: Some(self.service_type().to_string()),
                    matched_version: Some((*version).to_string()),
                    errors: Vec::new(),
                    warnings: warns,
                };
            }
            last_errors = errs;
            last_warnings = warns;
        }
        ValidationResult {
            valid: false,
            service_type: Some(self.service_type().to_string()),
            matched_version: None,
            errors: vec![format!(
                "No allowed {} version matched {}. Latest errors: {}",
                self.service_type(),
                py_list(&versions),
                last_errors.join("; ")
            )],
            warnings: last_warnings,
        }
    }
}

fn py_list(items: &[&str]) -> String {
    let parts: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", parts.join(", "))
}

/// Shared v0.0 baseline: requires `name` and `description` only.
fn check_name_description(payload: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
    let mut errs = Vec::new();
    if !has_text(payload.get("name")) {
        errs.push("name is required".to_string());
    }
    if !has_text(payload.get("description")) {
        errs.push("description is required".to_string());
    }
    (errs, Vec::new())
}

/// Validator for generic services (`v0.0`).
pub struct GenericValidator;

impl FormatValidator for GenericValidator {
    fn service_type(&self) -> &'static str {
        "generic"
    }
    fn supported_versions(&self) -> &'static [&'static str] {
        &["v0.0"]
    }
    fn check_version(&self, payload: &Map<String, Value>, version: &str) -> (Vec<String>, Vec<String>) {
        if version == "v0.0" {
            return check_name_description(payload);
        }
        (vec![format!("Unknown generic version {version}")], Vec::new())
    }
}

/// Validator for skills (`v0.0`).
pub struct SkillValidator;

impl FormatValidator for SkillValidator {
    fn service_type(&self) -> &'static str {
        "skill"
    }
    fn supported_versions(&self) -> &'static [&'static str] {
        &["v0.0"]
    }
    fn check_version(&self, payload: &Map<String, Value>, version: &str) -> (Vec<String>, Vec<String>) {
        if version == "v0.0" {
            return check_name_description(payload);
        }
        (vec![format!("Unknown skill version {version}")], Vec::new())
    }
}

/// Validator for A2A agent cards: `v0.0` (loose) and `v1.0` (full spec).
pub struct A2AValidator;

impl A2AValidator {
    fn v0_0(payload: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
        let (errs, mut warns) = check_name_description(payload);
        if !truthy(payload.get("version")) {
            warns.push("version is recommended".to_string());
        }
        if !truthy(payload.get("skills")) {
            warns.push("skills is recommended (agent has no declared capabilities)".to_string());
        }
        (errs, warns)
    }

    fn v1_0(payload: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
        let mut errs = Vec::new();
        let mut warns = Vec::new();
        if !has_text(payload.get("name")) {
            errs.push("name is required".into());
        }
        if !has_text(payload.get("description")) {
            errs.push("description is required".into());
        }
        if !has_text(payload.get("version")) {
            errs.push("version is required".into());
        }
        if !has_text(payload.get("url")) {
            errs.push("url (or supported_interfaces) is required".into());
        }
        if matches!(payload.get("capabilities"), None | Some(Value::Null)) {
            errs.push("capabilities is required (can be empty object)".into());
        }
        if !truthy(payload.get("defaultInputModes")) {
            errs.push("defaultInputModes is required (e.g. [\"text/plain\"])".into());
        }
        if !truthy(payload.get("defaultOutputModes")) {
            errs.push("defaultOutputModes is required (e.g. [\"text/plain\"])".into());
        }
        match payload.get("skills") {
            Some(Value::Array(skills)) if !skills.is_empty() => {
                for (i, sk) in skills.iter().enumerate() {
                    let s = to_map(sk);
                    let p = format!("skills[{i}]");
                    if !has_text(s.get("id")) {
                        errs.push(format!("{p}.id is required"));
                    }
                    if !has_text(s.get("name")) {
                        errs.push(format!("{p}.name is required"));
                    }
                    if !has_text(s.get("description")) {
                        errs.push(format!("{p}.description is required"));
                    }
                    if !truthy(s.get("tags")) {
                        errs.push(format!("{p}.tags is required (at least one tag)"));
                    }
                }
            }
            _ => errs.push("skills is required (at least one skill)".into()),
        }
        let prov = payload
            .get("provider")
            .filter(|v| crate::util::py_truthy(v))
            .map(to_map)
            .unwrap_or_default();
        if !prov.is_empty() {
            if !has_text(prov.get("organization")) {
                errs.push("provider.organization is required when provider is present".into());
            }
            if !has_text(prov.get("url")) {
                errs.push("provider.url is required when provider is present".into());
            }
        }
        if !has_text(payload.get("protocolVersion")) {
            warns.push("protocolVersion is recommended (e.g. \"1.0\")".into());
        }
        if prov.is_empty() {
            warns.push("provider is recommended".into());
        }
        if !has_text(payload.get("documentationUrl")) {
            warns.push("documentationUrl is recommended".into());
        }
        (errs, warns)
    }
}

impl FormatValidator for A2AValidator {
    fn service_type(&self) -> &'static str {
        "a2a"
    }
    fn supported_versions(&self) -> &'static [&'static str] {
        &["v0.0", "v1.0"]
    }
    fn check_version(&self, payload: &Map<String, Value>, version: &str) -> (Vec<String>, Vec<String>) {
        match version {
            "v0.0" => Self::v0_0(payload),
            "v1.0" => Self::v1_0(payload),
            _ => (vec![format!("Unknown a2a version {version}")], Vec::new()),
        }
    }
}

/// Supported service types in declaration order.
pub const SUPPORTED_SERVICE_TYPES: &[&str] = &["generic", "a2a", "skill"];

/// Default per dataset config: all three types allowed from `v0.0`.
pub static DEFAULT_FORMAT_CONFIG: Lazy<HashMap<String, String>> = Lazy::new(|| {
    SUPPORTED_SERVICE_TYPES
        .iter()
        .map(|t| ((*t).to_string(), "v0.0".to_string()))
        .collect()
});

static VALIDATORS: Lazy<Vec<Box<dyn FormatValidator>>> = Lazy::new(|| {
    vec![
        Box::new(GenericValidator),
        Box::new(A2AValidator),
        Box::new(SkillValidator),
    ]
});

/// Look up the validator for a service type.
pub fn validator_for(service_type: &str) -> Option<&'static dyn FormatValidator> {
    VALIDATORS
        .iter()
        .find(|v| v.service_type() == service_type)
        .map(|v| v.as_ref())
}

/// Validate a payload for `service_type` at or above `min_version`.
pub fn validate_service(service_type: &str, payload: &Value, min_version: &str) -> ValidationResult {
    match validator_for(service_type) {
        Some(v) => v.validate(payload, min_version),
        None => ValidationResult::failure(
            service_type,
            vec![format!(
                "Unknown service type {service_type:?}. Supported: {}",
                py_list(SUPPORTED_SERVICE_TYPES)
            )],
        ),
    }
}

/// Sanitize a user supplied `formats` value.
///
/// Unknown types and versions are dropped. Accepts `{type: "v0.0"}` or
/// `{type: {"min_version": "v0.0"}}`. Returns an empty map when nothing
/// valid remains; callers substitute defaults or reject.
pub fn normalize_format_config(raw: Option<&Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(Value::Object(map)) = raw else {
        return out;
    };
    for (t, v) in map {
        let Some(validator) = validator_for(t) else {
            continue;
        };
        let version = match v {
            Value::Object(o) => o
                .get("min_version")
                .cloned()
                .unwrap_or(Value::String("v0.0".into())),
            other => other.clone(),
        };
        let Value::String(ver) = version else {
            continue;
        };
        if !validator.supported_versions().contains(&ver.as_str()) {
            continue;
        }
        out.insert(t.clone(), ver);
    }
    out
}

/// Legacy shim: validate an agent card against a set of allowed A2A
/// versions. The oldest allowed version becomes `min_version`.
pub fn validate_agent_card(card: &AgentCard, allowed_versions: Option<&HashSet<String>>) -> ValidationResult {
    let all: HashSet<String> = A2AValidator
        .supported_versions()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let allowed = allowed_versions.unwrap_or(&all);
    let mut known: Vec<&String> = allowed.iter().filter(|v| all.contains(*v)).collect();
    if known.is_empty() {
        let mut listed: Vec<&String> = allowed.iter().collect();
        listed.sort();
        return ValidationResult::failure(
            "a2a",
            vec![format!("No known A2A versions in allowed set: {listed:?}")],
        );
    }
    known.sort_by_key(|v| version_key(v).unwrap_or_default());
    validate_service("a2a", &Value::Object(card.to_json(DumpMode::Full)), known[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn v0_only_needs_name_and_description() -> TestResult {
        let r = validate_service("generic", &json!({"name": "a", "description": "b"}), "v0.0");
        assert!(r.valid);
        assert_eq!(r.matched_version.as_deref(), Some("v0.0"));
        let r = validate_service("generic", &json!({"name": " ", "description": "b"}), "v0.0");
        assert!(!r.valid);
        assert!(r.errors[0].contains("name is required"));
        Ok(())
    }

    #[test]
    fn a2a_min_version_v1_requires_full_spec() -> TestResult {
        let loose = json!({"name": "a", "description": "b"});
        assert!(validate_service("a2a", &loose, "v0.0").valid);
        let r = validate_service("a2a", &loose, "v1.0");
        assert!(!r.valid);
        assert!(r.errors[0].contains("version is required"));
        let full = json!({
            "name": "a", "description": "b", "version": "1", "url": "u", "capabilities": {},
            "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
            "skills": [{"id": "s", "name": "s", "description": "s", "tags": ["t"]}]
        });
        let r = validate_service("a2a", &full, "v1.0");
        assert!(r.valid, "{:?}", r.errors);
        assert_eq!(r.matched_version.as_deref(), Some("v1.0"));
        assert!(r.warnings.iter().any(|w| w.contains("provider is recommended")));
        Ok(())
    }

    #[test]
    fn unknown_type_and_bad_version() -> TestResult {
        assert!(!validate_service("nope", &json!({}), "v0.0").valid);
        let r = validate_service("generic", &json!({}), "vx");
        assert!(!r.valid);
        assert!(r.errors[0].contains("Invalid version string"));
        let r = validate_service("generic", &json!({"name": "a", "description": "b"}), "v9.0");
        assert!(r.errors[0].starts_with("No generic version >= v9.0"));
        Ok(())
    }

    #[test]
    fn normalize_drops_unknown() -> TestResult {
        let cfg = normalize_format_config(Some(&json!({
            "generic": "v0.0", "a2a": {"min_version": "v1.0"}, "skill": "v7.0", "bogus": "v0.0", "x": 1
        })));
        assert_eq!(cfg.len(), 2);
        assert_eq!(cfg["a2a"], "v1.0");
        assert!(normalize_format_config(Some(&json!("str"))).is_empty());
        assert!(normalize_format_config(None).is_empty());
        Ok(())
    }

    #[test]
    fn legacy_agent_card_shim() -> TestResult {
        let card = AgentCard::from_value(json!({"name": "a", "description": "b"}))?;
        assert!(validate_agent_card(&card, None).valid);
        let only_v1: HashSet<String> = ["v1.0".to_string()].into_iter().collect();
        assert!(!validate_agent_card(&card, Some(&only_v1)).valid);
        let none: HashSet<String> = ["v9".to_string()].into_iter().collect();
        assert!(validate_agent_card(&card, Some(&none)).errors[0].contains("No known A2A versions"));
        Ok(())
    }
}
