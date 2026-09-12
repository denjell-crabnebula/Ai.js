// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `AuthStore`: file backed credential store.
//!
//! Layout under the auth data directory:
//! `principals.json` (list of principals), `api_keys.json` (list of keys)
//! and `audit.log` (JSON lines, append only). The hot path
//! `authenticate(token)` is one sha256 lookup against an in-memory index.

use std::path::{Path, PathBuf};

use a2x_common::atomic::atomic_write_bytes;
use a2x_common::{AuthContext, Role};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use super::errors::AuthenticationError;
use super::models::{ApiKey, Principal};
use super::tokens::{TOKEN_PREFIX, constant_time_equals, generate_token, hash_token, token_prefix};
use crate::util::utcnow_iso;

pub const PRINCIPALS_FILE: &str = "principals.json";
pub const KEYS_FILE: &str = "api_keys.json";
pub const AUDIT_LOG_FILE: &str = "audit.log";

/// Resolve the auth data directory: `$A2X_REGISTRY_AUTH_DATA`, else
/// `<home>/auth_data` where `<home>` follows `A2X_REGISTRY_HOME`.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = ap_support::env::current().get_non_blank("A2X_REGISTRY_AUTH_DATA") {
        return PathBuf::from(dir);
    }
    a2x_common::paths::get_home().join("auth_data")
}

/// Errors from store mutations.
#[derive(Debug, thiserror::Error)]
pub enum AuthStoreError {
    /// Bootstrap refused because the store already exists.
    #[error("{0}")]
    AlreadyInitialized(String),
    /// Invalid input (`ValueError`).
    #[error("{0}")]
    Invalid(String),
    /// Principal or key not found (`KeyError`).
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for AuthStoreError {
    fn from(e: std::io::Error) -> Self {
        AuthStoreError::Io(e.to_string())
    }
}

/// How `update_principal` treats the `namespaces` field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamespacesUpdate {
    /// Keep the current value.
    Unset,
    /// Replace with the given value (`None` means all, admin only).
    Set(Option<Vec<String>>),
}

struct Tables {
    principals: IndexMap<String, Principal>,
    keys: IndexMap<String, ApiKey>,
    keys_by_hash: IndexMap<String, String>,
}

/// File backed credential store with in-memory indices.
pub struct AuthStore {
    dir: PathBuf,
    tables: Mutex<Tables>,
}

fn write_json_python_style(path: &Path, data: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    atomic_write_bytes(path, content.as_bytes())
}

fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    format!("{prefix}_{}", hex::encode(bytes))
}

fn parse_role(role: &str) -> Result<Role, AuthStoreError> {
    Role::parse(role).ok_or_else(|| {
        AuthStoreError::Invalid(format!(
            "role must be one of ('admin', 'provider', 'user'), got '{role}'"
        ))
    })
}

impl AuthStore {
    fn empty(dir: PathBuf) -> Self {
        Self {
            dir,
            tables: Mutex::new(Tables {
                principals: IndexMap::new(),
                keys: IndexMap::new(),
                keys_by_hash: IndexMap::new(),
            }),
        }
    }

    /// Return a loaded store if bootstrap has happened, else `None`.
    pub fn load_or_none(data_dir: Option<&Path>) -> Result<Option<AuthStore>, AuthStoreError> {
        let dir = data_dir.map(Path::to_path_buf).unwrap_or_else(default_data_dir);
        if !dir.join(PRINCIPALS_FILE).exists() {
            return Ok(None);
        }
        let store = Self::empty(dir);
        store.load_from_disk()?;
        Ok(Some(store))
    }

    /// Initialize a fresh store with one admin principal and key.
    ///
    /// Refuses when `principals.json` already exists. Returns the store and
    /// the plaintext admin token, which must be surfaced to the operator.
    pub fn bootstrap(
        data_dir: Option<&Path>,
        admin_token: Option<&str>,
        admin_handle: &str,
    ) -> Result<(AuthStore, String), AuthStoreError> {
        let dir = data_dir.map(Path::to_path_buf).unwrap_or_else(default_data_dir);
        if dir.join(PRINCIPALS_FILE).exists() {
            return Err(AuthStoreError::AlreadyInitialized(format!(
                "Auth already initialized at {}. Run 'a2x-registry auth reset-admin --confirm' to rotate the admin key.",
                dir.display()
            )));
        }
        let token = admin_token.map(str::to_string).unwrap_or_else(generate_token);
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(AuthStoreError::Invalid(format!(
                "admin_token must start with '{TOKEN_PREFIX}'"
            )));
        }
        std::fs::create_dir_all(&dir)?;
        let store = Self::empty(dir);
        let principal = Principal {
            id: new_id("u"),
            handle: admin_handle.to_string(),
            role: Role::Admin,
            namespaces: None,
            created_at: utcnow_iso(),
            disabled_at: None,
            note: "Bootstrap admin (root)".into(),
        };
        let key = ApiKey {
            key_id: new_id("k"),
            principal_id: principal.id.clone(),
            key_hash: hash_token(&token),
            key_prefix: token_prefix(&token),
            name: "bootstrap".into(),
            created_at: utcnow_iso(),
            expires_at: None,
            last_used_at: None,
            revoked_at: None,
        };
        {
            let mut t = store.tables.lock();
            t.principals.insert(principal.id.clone(), principal.clone());
            t.keys.insert(key.key_id.clone(), key.clone());
            t.keys_by_hash.insert(key.key_hash.clone(), key.key_id.clone());
            store.persist_principals(&t)?;
            store.persist_keys(&t)?;
        }
        store.audit(
            "principal.created",
            &[
                ("principal_id", principal.id.clone().into()),
                ("role", "admin".into()),
                ("by", "bootstrap".into()),
            ],
        );
        store.audit(
            "key.created",
            &[
                ("key_id", key.key_id.clone().into()),
                ("principal_id", principal.id.clone().into()),
                ("key_prefix", key.key_prefix.clone().into()),
                ("by", "bootstrap".into()),
            ],
        );
        Ok((store, token))
    }

    // ── load / persist ──────────────────────────────────────────────────

    fn load_from_disk(&self) -> Result<(), AuthStoreError> {
        let path_p = self.dir.join(PRINCIPALS_FILE);
        let path_k = self.dir.join(KEYS_FILE);
        let mut t = self.tables.lock();
        t.principals.clear();
        t.keys.clear();
        t.keys_by_hash.clear();
        let raw = std::fs::read_to_string(&path_p)?;
        let principals: Vec<Principal> = serde_json::from_str(&raw)
            .map_err(|e| AuthStoreError::Io(format!("Failed to load {}: {e}", path_p.display())))?;
        for p in principals {
            t.principals.insert(p.id.clone(), p);
        }
        if path_k.exists() {
            let raw = std::fs::read_to_string(&path_k)?;
            let keys: Vec<ApiKey> = serde_json::from_str(&raw)
                .map_err(|e| AuthStoreError::Io(format!("Failed to load {}: {e}", path_k.display())))?;
            for k in keys {
                t.keys_by_hash.insert(k.key_hash.clone(), k.key_id.clone());
                t.keys.insert(k.key_id.clone(), k);
            }
        }
        Ok(())
    }

    fn persist_principals(&self, t: &Tables) -> Result<(), AuthStoreError> {
        let data: Vec<Value> = t
            .principals
            .values()
            .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
            .collect();
        write_json_python_style(&self.dir.join(PRINCIPALS_FILE), &Value::Array(data))?;
        Ok(())
    }

    fn persist_keys(&self, t: &Tables) -> Result<(), AuthStoreError> {
        let data: Vec<Value> = t
            .keys
            .values()
            .map(|k| serde_json::to_value(k).unwrap_or(Value::Null))
            .collect();
        write_json_python_style(&self.dir.join(KEYS_FILE), &Value::Array(data))?;
        Ok(())
    }

    // ── audit log ───────────────────────────────────────────────────────

    /// Append a JSON line audit record. Best effort, never fails.
    pub fn audit(&self, event: &str, fields: &[(&str, Value)]) {
        let mut entry = Map::new();
        entry.insert("ts".into(), Value::String(utcnow_iso()));
        entry.insert("event".into(), Value::String(event.into()));
        for (k, v) in fields {
            entry.insert((*k).to_string(), v.clone());
        }
        let line = match crate::util::pyjson::dumps(&Value::Object(entry)) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!("audit record for {event} could not be serialized: {e}");
                return;
            }
        };
        let result = std::fs::create_dir_all(&self.dir).and_then(|_| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(AUDIT_LOG_FILE))?;
            writeln!(f, "{line}")
        });
        if let Err(e) = result {
            tracing::warn!("Audit log write failed ({}): {}", e, line);
        }
    }

    // ── authentication hot path ─────────────────────────────────────────

    /// Verify a plaintext bearer token and return its context.
    pub fn authenticate(&self, token: &str) -> Result<AuthContext, AuthenticationError> {
        if token.is_empty() {
            self.audit("auth.failed", &[("reason", "empty_token".into())]);
            return Err(AuthenticationError("Empty or non-string token".into()));
        }
        if !token.starts_with(TOKEN_PREFIX) {
            let shown: String = token.chars().take(12).collect();
            self.audit(
                "auth.failed",
                &[("reason", "wrong_prefix".into()), ("key_prefix", shown.into())],
            );
            return Err(AuthenticationError("Token has wrong prefix".into()));
        }
        let h = hash_token(token);
        let key_id = self.tables.lock().keys_by_hash.get(&h).cloned();
        let Some(key_id) = key_id else {
            self.audit(
                "auth.failed",
                &[
                    ("reason", "invalid_token".into()),
                    ("key_prefix", token_prefix(token).into()),
                ],
            );
            return Err(AuthenticationError("Invalid API key".into()));
        };
        let (key, principal) = {
            let t = self.tables.lock();
            let key = t.keys.get(&key_id).cloned();
            let principal = key
                .as_ref()
                .and_then(|k| t.principals.get(&k.principal_id).cloned());
            (key, principal)
        };
        let (Some(key), Some(principal)) = (key, principal) else {
            self.audit(
                "auth.failed",
                &[
                    ("reason", "dangling_key".into()),
                    ("key_id", key_id.clone().into()),
                    ("key_prefix", token_prefix(token).into()),
                ],
            );
            return Err(AuthenticationError("Key references a missing principal".into()));
        };
        if !constant_time_equals(&key.key_hash, &h) {
            self.audit(
                "auth.failed",
                &[
                    ("reason", "hash_mismatch".into()),
                    ("key_id", key.key_id.clone().into()),
                ],
            );
            return Err(AuthenticationError("Hash mismatch".into()));
        }
        if key.is_revoked() {
            self.audit(
                "auth.failed",
                &[
                    ("reason", "revoked".into()),
                    ("key_id", key.key_id.clone().into()),
                ],
            );
            return Err(AuthenticationError("API key has been revoked".into()));
        }
        if principal.is_disabled() {
            self.audit(
                "auth.failed",
                &[
                    ("reason", "principal_disabled".into()),
                    ("principal_id", principal.id.clone().into()),
                    ("key_id", key.key_id.clone().into()),
                ],
            );
            return Err(AuthenticationError(format!(
                "Principal '{}' is disabled",
                principal.handle
            )));
        }
        if let Some(k) = self.tables.lock().keys.get_mut(&key_id) {
            k.last_used_at = Some(utcnow_iso());
        }
        Ok(principal.to_context())
    }

    // ── principal CRUD ──────────────────────────────────────────────────

    /// Create a principal and its first key. Returns the plaintext token.
    pub fn create_principal(
        &self,
        handle: &str,
        role: &str,
        namespaces: Option<Vec<String>>,
        note: &str,
        by: Option<&str>,
    ) -> Result<(Principal, String), AuthStoreError> {
        let parsed = parse_role(role)?;
        if parsed == Role::Admin && namespaces.is_some() {
            return Err(AuthStoreError::Invalid(
                "admin principal must have namespaces=None (all)".into(),
            ));
        }
        if parsed != Role::Admin && namespaces.is_none() {
            return Err(AuthStoreError::Invalid(format!(
                "{role} principal requires namespaces=list (got NoneType)"
            )));
        }
        let (principal, key, token) = {
            let mut t = self.tables.lock();
            if t.principals.values().any(|p| p.handle == handle) {
                return Err(AuthStoreError::Invalid(format!(
                    "handle '{handle}' already in use"
                )));
            }
            let principal = Principal {
                id: new_id("u"),
                handle: handle.to_string(),
                role: parsed,
                namespaces,
                created_at: utcnow_iso(),
                disabled_at: None,
                note: note.to_string(),
            };
            t.principals.insert(principal.id.clone(), principal.clone());
            let token = generate_token();
            let key = ApiKey {
                key_id: new_id("k"),
                principal_id: principal.id.clone(),
                key_hash: hash_token(&token),
                key_prefix: token_prefix(&token),
                name: "initial".into(),
                created_at: utcnow_iso(),
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
            };
            t.keys.insert(key.key_id.clone(), key.clone());
            t.keys_by_hash.insert(key.key_hash.clone(), key.key_id.clone());
            self.persist_principals(&t)?;
            self.persist_keys(&t)?;
            (principal, key, token)
        };
        let by_v = by.map(|b| Value::String(b.into())).unwrap_or(Value::Null);
        self.audit(
            "principal.created",
            &[
                ("principal_id", principal.id.clone().into()),
                ("role", role.into()),
                ("by", by_v.clone()),
            ],
        );
        self.audit(
            "key.created",
            &[
                ("key_id", key.key_id.clone().into()),
                ("principal_id", principal.id.clone().into()),
                ("key_prefix", key.key_prefix.clone().into()),
                ("by", by_v),
            ],
        );
        Ok((principal, token))
    }

    pub fn list_principals(&self) -> Vec<Principal> {
        self.tables.lock().principals.values().cloned().collect()
    }

    pub fn get_principal(&self, principal_id: &str) -> Option<Principal> {
        self.tables.lock().principals.get(principal_id).cloned()
    }

    /// Partial update of a principal's mutable fields. Role transitions
    /// must keep the namespaces invariant (admin means `None`).
    pub fn update_principal(
        &self,
        principal_id: &str,
        namespaces: NamespacesUpdate,
        role: Option<&str>,
        disabled: Option<bool>,
        note: Option<&str>,
        by: Option<&str>,
    ) -> Result<Principal, AuthStoreError> {
        let updated = {
            let mut t = self.tables.lock();
            let principal =
                t.principals.get(principal_id).cloned().ok_or_else(|| {
                    AuthStoreError::NotFound(format!("Principal '{principal_id}' not found"))
                })?;
            let new_role = match role {
                Some(r) => parse_role(r)?,
                None => principal.role,
            };
            let new_ns = match namespaces {
                NamespacesUpdate::Unset => principal.namespaces.clone(),
                NamespacesUpdate::Set(v) => v,
            };
            if new_role == Role::Admin && new_ns.is_some() {
                return Err(AuthStoreError::Invalid(
                    "admin principal must have namespaces=None (all)".into(),
                ));
            }
            if new_role != Role::Admin && new_ns.is_none() {
                return Err(AuthStoreError::Invalid(format!(
                    "{new_role} principal requires namespaces=list"
                )));
            }
            let new_disabled_at = match disabled {
                Some(true) => principal.disabled_at.clone().or_else(|| Some(utcnow_iso())),
                Some(false) => None,
                None => principal.disabled_at.clone(),
            };
            let updated = Principal {
                id: principal.id.clone(),
                handle: principal.handle.clone(),
                role: new_role,
                namespaces: new_ns,
                created_at: principal.created_at.clone(),
                disabled_at: new_disabled_at,
                note: note.map(str::to_string).unwrap_or(principal.note.clone()),
            };
            t.principals.insert(principal_id.to_string(), updated.clone());
            self.persist_principals(&t)?;
            updated
        };
        self.audit(
            "principal.updated",
            &[
                ("principal_id", principal_id.into()),
                ("by", by.map(|b| Value::String(b.into())).unwrap_or(Value::Null)),
            ],
        );
        Ok(updated)
    }

    // ── key CRUD ────────────────────────────────────────────────────────

    /// Issue a new key for an existing principal. Returns `(key, plaintext)`.
    pub fn create_key(
        &self,
        principal_id: &str,
        name: &str,
        by: Option<&str>,
    ) -> Result<(ApiKey, String), AuthStoreError> {
        let (key, token) = {
            let mut t = self.tables.lock();
            if !t.principals.contains_key(principal_id) {
                return Err(AuthStoreError::NotFound(format!(
                    "Principal '{principal_id}' not found"
                )));
            }
            let token = generate_token();
            let key = ApiKey {
                key_id: new_id("k"),
                principal_id: principal_id.to_string(),
                key_hash: hash_token(&token),
                key_prefix: token_prefix(&token),
                name: name.to_string(),
                created_at: utcnow_iso(),
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
            };
            t.keys.insert(key.key_id.clone(), key.clone());
            t.keys_by_hash.insert(key.key_hash.clone(), key.key_id.clone());
            self.persist_keys(&t)?;
            (key, token)
        };
        self.audit(
            "key.created",
            &[
                ("key_id", key.key_id.clone().into()),
                ("principal_id", principal_id.into()),
                ("key_prefix", key.key_prefix.clone().into()),
                ("by", by.map(|b| Value::String(b.into())).unwrap_or(Value::Null)),
            ],
        );
        Ok((key, token))
    }

    /// List keys, optionally for one principal. Plaintext is never returned.
    pub fn list_keys(&self, principal_id: Option<&str>) -> Vec<ApiKey> {
        let keys: Vec<ApiKey> = self.tables.lock().keys.values().cloned().collect();
        match principal_id {
            Some(pid) => keys.into_iter().filter(|k| k.principal_id == pid).collect(),
            None => keys,
        }
    }

    /// Mark a key revoked. Idempotent.
    pub fn revoke_key(&self, key_id: &str, by: Option<&str>) -> Result<ApiKey, AuthStoreError> {
        let key = {
            let mut t = self.tables.lock();
            let mut key = t
                .keys
                .get(key_id)
                .cloned()
                .ok_or_else(|| AuthStoreError::NotFound(format!("Key '{key_id}' not found")))?;
            if !key.is_revoked() {
                key.revoked_at = Some(utcnow_iso());
                let hash = key.key_hash.clone();
                t.keys.insert(key_id.to_string(), key.clone());
                t.keys_by_hash.shift_remove(&hash);
                self.persist_keys(&t)?;
            }
            key
        };
        self.audit(
            "key.revoked",
            &[
                ("key_id", key_id.into()),
                ("principal_id", key.principal_id.clone().into()),
                ("by", by.map(|b| Value::String(b.into())).unwrap_or(Value::Null)),
            ],
        );
        Ok(key)
    }

    /// True when `hash` is still in the by-hash index (tests).
    pub fn has_hash(&self, hash: &str) -> bool {
        self.tables.lock().keys_by_hash.contains_key(hash)
    }

    pub fn data_dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn bootstrap_creates_files_and_never_persists_plaintext() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("auth_data");
        let (store, token) = AuthStore::bootstrap(Some(&dir), None, "root")?;
        assert!(token.starts_with(TOKEN_PREFIX));
        assert!(token.len() - TOKEN_PREFIX.len() >= 40);
        let principals: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.join(PRINCIPALS_FILE))?)?;
        let keys: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join(KEYS_FILE))?)?;
        assert_eq!(principals.len(), 1);
        assert_eq!(principals[0]["role"], "admin");
        assert_eq!(principals[0]["namespaces"], Value::Null);
        assert_eq!(keys[0]["principal_id"], principals[0]["id"]);
        for entry in std::fs::read_dir(&dir)? {
            let content = std::fs::read_to_string(entry?.path())?;
            assert!(!content.contains(&token));
            assert!(!content.contains(&token[TOKEN_PREFIX.len()..]));
        }
        let log = std::fs::read_to_string(dir.join(AUDIT_LOG_FILE))?;
        assert!(log.contains("\"event\": \"principal.created\""));
        assert!(log.contains("\"event\": \"key.created\""));
        assert!(
            regex::Regex::new(r"a2x_pat_[A-Za-z0-9_-]{40,}")?
                .find(&log)
                .is_none()
        );
        assert!(matches!(
            AuthStore::bootstrap(Some(&dir), None, "root"),
            Err(AuthStoreError::AlreadyInitialized(_))
        ));
        assert!(store.authenticate(&token)?.is_admin());
        Ok(())
    }

    #[test]
    fn explicit_token_and_prefix_check() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let target = format!("{TOKEN_PREFIX}{}", "x".repeat(43));
        let (store, returned) = AuthStore::bootstrap(Some(tmp.path()), Some(&target), "root")?;
        assert_eq!(returned, target);
        assert!(store.authenticate(&target)?.is_admin());
        let other = tempfile::tempdir()?;
        assert!(matches!(
            AuthStore::bootstrap(Some(other.path()), Some("not_a_pat_token"), "root"),
            Err(AuthStoreError::Invalid(_))
        ));
        Ok(())
    }

    #[test]
    fn load_or_none_round_trip() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("auth_data");
        assert!(AuthStore::load_or_none(Some(&dir))?.is_none());
        let (store1, token) = AuthStore::bootstrap(Some(&dir), None, "root")?;
        let (p, ptoken) = store1.create_principal(
            "alice",
            "provider",
            Some(vec!["ds".into()]),
            "",
            Some("bootstrap"),
        )?;
        let store2 = AuthStore::load_or_none(Some(&dir))?.required()?;
        assert_eq!(
            store1.authenticate(&token)?.principal_id,
            store2.authenticate(&token)?.principal_id
        );
        assert_eq!(store2.get_principal(&p.id).required()?.handle, "alice");
        assert_eq!(store2.authenticate(&ptoken)?.role, Role::Provider);
        Ok(())
    }

    #[test]
    fn principal_and_key_rules() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let (store, _) = AuthStore::bootstrap(Some(tmp.path()), None, "root")?;
        assert!(
            store
                .create_principal("a", "admin", Some(vec![]), "", None)
                .is_err()
        );
        assert!(store.create_principal("b", "user", None, "", None).is_err());
        assert!(
            store
                .create_principal("c", "wizard", Some(vec![]), "", None)
                .is_err()
        );
        let (p, tok) = store.create_principal("dup", "user", Some(vec![]), "", None)?;
        assert!(matches!(
            store.create_principal("dup", "user", Some(vec![]), "", None),
            Err(AuthStoreError::Invalid(_))
        ));
        assert!(
            store
                .update_principal(&p.id, NamespacesUpdate::Unset, Some("admin"), None, None, None)
                .is_err()
        );
        let up = store.update_principal(
            &p.id,
            NamespacesUpdate::Set(None),
            Some("admin"),
            None,
            None,
            None,
        )?;
        assert!(up.is_admin());
        assert!(
            store
                .update_principal(&p.id, NamespacesUpdate::Unset, Some("user"), None, None, None)
                .is_err()
        );
        store.update_principal(&p.id, NamespacesUpdate::Unset, None, Some(true), None, None)?;
        assert!(store.authenticate(&tok).is_err());
        let re =
            store.update_principal(&p.id, NamespacesUpdate::Unset, None, Some(false), Some("n"), None)?;
        assert_eq!(re.disabled_at, None);
        assert_eq!(re.note, "n");
        assert!(store.authenticate(&tok).is_ok());
        assert!(
            store
                .update_principal("u_missing", NamespacesUpdate::Unset, None, None, None, None)
                .is_err()
        );

        let (key, ktok) = store.create_key(&p.id, "laptop", Some(&p.id))?;
        assert_eq!(store.list_keys(Some(&p.id)).len(), 2);
        assert!(store.authenticate(&ktok).is_ok());
        let revoked = store.revoke_key(&key.key_id, None)?;
        assert!(revoked.is_revoked());
        assert!(!store.has_hash(&hash_token(&ktok)));
        assert!(store.authenticate(&ktok).is_err());
        assert_eq!(
            store.revoke_key(&key.key_id, None)?.revoked_at,
            revoked.revoked_at
        );
        assert!(store.revoke_key("k_nope", None).is_err());
        assert!(store.create_key("u_nope", "", None).is_err());
        assert!(store.authenticate("").is_err());
        assert!(store.authenticate("ghp_x").is_err());
        assert!(store.authenticate("a2x_pat_unknown").is_err());
        Ok(())
    }
}
