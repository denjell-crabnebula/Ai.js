// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Carrier-neutral user credential storage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{A4PError, CredentialStoreFormatError};
use crate::types::JsonDict;
use crate::util::sorted_value;
pub use crate::util::utc_now_iso;

/// Schema version written to and required from JSON credential files.
pub const CREDENTIAL_STORE_SCHEMA_VERSION: i64 = 2;

/// One registered user credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserCredentialRecord {
    /// Owner user id.
    pub user_id: String,
    /// Credential id, unique across the store.
    pub credential_id: String,
    /// Signature method that owns this record (`ed25519` or `webauthn`).
    pub signature_method: String,
    /// Method specific public key object.
    pub public_key: JsonDict,
    /// Method specific state, for example the WebAuthn sign count.
    #[serde(default)]
    pub details: JsonDict,
    /// Free form metadata supplied at registration.
    #[serde(default)]
    pub metadata: JsonDict,
    /// Creation time as `%Y-%m-%dT%H:%M:%SZ`.
    #[serde(default)]
    pub created_at: String,
}

impl UserCredentialRecord {
    /// Build a record with empty details, metadata and creation time.
    pub fn new(
        user_id: impl Into<String>,
        credential_id: impl Into<String>,
        signature_method: impl Into<String>,
        public_key: JsonDict,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            credential_id: credential_id.into(),
            signature_method: signature_method.into(),
            public_key,
            details: JsonDict::new(),
            metadata: JsonDict::new(),
            created_at: String::new(),
        }
    }

    /// Return the record as a camelCase JSON object.
    pub fn to_json(&self) -> JsonDict {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => JsonDict::new(),
        }
    }
}

/// Storage of user credential records.
pub trait A4PCredentialStore: Send + Sync {
    /// Persist or replace a user credential record.
    fn save(&self, record: UserCredentialRecord) -> Result<(), A4PError>;
    /// Return one credential record by credential id.
    fn get(&self, credential_id: &str) -> Result<Option<UserCredentialRecord>, A4PError>;
    /// Return all credential records registered for a user.
    fn list_for_user(&self, user_id: &str) -> Result<Vec<UserCredentialRecord>, A4PError>;
    /// Return all credential records.
    fn list_all(&self) -> Result<Vec<UserCredentialRecord>, A4PError>;
}

/// Single process credential store that keeps insertion order.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    records: Mutex<Vec<UserCredentialRecord>>,
}

impl InMemoryCredentialStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store pre-populated with records.
    pub fn with_records(records: Vec<UserCredentialRecord>) -> Self {
        let store = Self::new();
        for record in records {
            store.insert(record);
        }
        store
    }

    fn insert(&self, record: UserCredentialRecord) {
        let mut records = self.records.lock();
        if let Some(existing) = records
            .iter_mut()
            .find(|item| item.credential_id == record.credential_id)
        {
            *existing = record;
        } else {
            records.push(record);
        }
    }

    fn replace_all(&self, records: Vec<UserCredentialRecord>) {
        let mut unique: Vec<UserCredentialRecord> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for record in records {
            if let Some(position) = index.get(&record.credential_id) {
                unique[*position] = record;
            } else {
                index.insert(record.credential_id.clone(), unique.len());
                unique.push(record);
            }
        }
        *self.records.lock() = unique;
    }
}

impl A4PCredentialStore for InMemoryCredentialStore {
    fn save(&self, record: UserCredentialRecord) -> Result<(), A4PError> {
        self.insert(record);
        Ok(())
    }

    fn get(&self, credential_id: &str) -> Result<Option<UserCredentialRecord>, A4PError> {
        Ok(self
            .records
            .lock()
            .iter()
            .find(|record| record.credential_id == credential_id)
            .cloned())
    }

    fn list_for_user(&self, user_id: &str) -> Result<Vec<UserCredentialRecord>, A4PError> {
        Ok(self
            .records
            .lock()
            .iter()
            .filter(|record| record.user_id == user_id)
            .cloned()
            .collect())
    }

    fn list_all(&self) -> Result<Vec<UserCredentialRecord>, A4PError> {
        Ok(self.records.lock().clone())
    }
}

/// Demo grade credential store persisted as `{"schemaVersion": 2, "credentials": [...]}`.
///
/// Every read refreshes from disk so several processes can share one file.
#[derive(Debug)]
pub struct JsonFileCredentialStore {
    path: PathBuf,
    memory: InMemoryCredentialStore,
}

impl JsonFileCredentialStore {
    /// Open or create the store at `path`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, A4PError> {
        let store = Self {
            path: path.as_ref().to_path_buf(),
            memory: InMemoryCredentialStore::new(),
        };
        let records = store.load_records()?;
        store.memory.replace_all(records);
        Ok(store)
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load_records(&self) -> Result<Vec<UserCredentialRecord>, A4PError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| A4PError::runtime(format!("Cannot read credential store: {error}")))?;
        let raw: Value = serde_json::from_str(&text)
            .map_err(|error| A4PError::value(format!("Credential store is not valid JSON: {error}")))?;
        let version_ok = raw
            .as_object()
            .and_then(|map| map.get("schemaVersion"))
            .and_then(Value::as_i64)
            == Some(CREDENTIAL_STORE_SCHEMA_VERSION);
        if !version_ok {
            return Err(CredentialStoreFormatError(
                "Unsupported credential store format; delete the old credential file \
                 and register credentials again"
                    .into(),
            )
            .into());
        }
        let items = raw.get("credentials").and_then(Value::as_array).ok_or_else(|| {
            CredentialStoreFormatError("Credential store 'credentials' must be a list".into())
        })?;
        let mut records = Vec::new();
        for item in items {
            if !item.is_object() {
                return Err(
                    CredentialStoreFormatError("Credential store records must be objects".into()).into(),
                );
            }
            let record: UserCredentialRecord = serde_json::from_value(item.clone())
                .map_err(|error| A4PError::value(format!("Credential store record invalid: {error}")))?;
            records.push(record);
        }
        Ok(records)
    }

    fn refresh(&self) -> Result<(), A4PError> {
        let records = self.load_records()?;
        self.memory.replace_all(records);
        Ok(())
    }

    fn flush(&self) -> Result<(), A4PError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    A4PError::runtime(format!("Cannot create credential store directory: {error}"))
                })?;
            }
        }
        let records: Vec<Value> = self
            .memory
            .list_all()?
            .into_iter()
            .map(|record| Value::Object(record.to_json()))
            .collect();
        let mut payload = JsonDict::new();
        payload.insert(
            "schemaVersion".into(),
            Value::from(CREDENTIAL_STORE_SCHEMA_VERSION),
        );
        payload.insert("credentials".into(), Value::Array(records));
        let text = serde_json::to_string_pretty(&sorted_value(&Value::Object(payload)))
            .map_err(|error| A4PError::runtime(format!("Cannot serialize credential store: {error}")))?;
        std::fs::write(&self.path, text)
            .map_err(|error| A4PError::runtime(format!("Cannot write credential store: {error}")))
    }
}

impl A4PCredentialStore for JsonFileCredentialStore {
    fn save(&self, record: UserCredentialRecord) -> Result<(), A4PError> {
        self.memory.save(record)?;
        self.flush()
    }

    fn get(&self, credential_id: &str) -> Result<Option<UserCredentialRecord>, A4PError> {
        self.refresh()?;
        self.memory.get(credential_id)
    }

    fn list_for_user(&self, user_id: &str) -> Result<Vec<UserCredentialRecord>, A4PError> {
        self.refresh()?;
        self.memory.list_for_user(user_id)
    }

    fn list_all(&self) -> Result<Vec<UserCredentialRecord>, A4PError> {
        self.refresh()?;
        self.memory.list_all()
    }
}
