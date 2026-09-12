// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Persistent execution-usage storage for A4P intent tokens.

use std::collections::HashMap;
#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(feature = "sqlite")]
use std::time::Duration;

use parking_lot::Mutex;
#[cfg(feature = "sqlite")]
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(feature = "sqlite")]
use crate::errors::A4PError;
use crate::errors::IntentTokenUsageStoreError;
use crate::util::now_epoch;

/// Default SQLite path relative to the working directory.
pub const DEFAULT_INTENT_TOKEN_USAGE_DB_PATH: &str = ".a4p/intent_token_usage.sqlite3";

/// Atomic "check limit and increment" storage for intent token executions.
pub trait A4PIntentTokenUsageStore: Send + Sync {
    /// Atomically consume one execution and return `(consumed, executions_used)`.
    ///
    /// Implementations must fail with `IntentTokenUsageStoreError` when the
    /// store is unavailable or its state disagrees with the signed token.
    fn consume(
        &self,
        token_id: &str,
        max_executions: i64,
        expire_at_epoch: i64,
    ) -> Result<(bool, i64), IntentTokenUsageStoreError>;
}

/// Return the configured SQLite path for intent-token usage state.
pub fn default_intent_token_usage_db_path() -> String {
    default_intent_token_usage_db_path_in(ap_support::env::current())
}

/// [`default_intent_token_usage_db_path`] reading from `env`.
pub fn default_intent_token_usage_db_path_in(env: &dyn ap_support::env::EnvSource) -> String {
    env.get("A4P_USAGE_DB_PATH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_INTENT_TOKEN_USAGE_DB_PATH.to_string())
        .trim()
        .to_string()
}

fn validate_consume_arguments(
    token_id: &str,
    max_executions: i64,
    expire_at_epoch: i64,
) -> Result<String, IntentTokenUsageStoreError> {
    let normalized = token_id.trim();
    if normalized.is_empty() {
        return Err(IntentTokenUsageStoreError("token_id must not be empty".into()));
    }
    if max_executions <= 0 {
        return Err(IntentTokenUsageStoreError(
            "max_executions must be a positive integer".into(),
        ));
    }
    if expire_at_epoch <= 0 {
        return Err(IntentTokenUsageStoreError(
            "expire_at_epoch must be a positive integer".into(),
        ));
    }
    Ok(normalized.to_string())
}

#[derive(Debug, Clone, Copy)]
struct UsageRow {
    executions_used: i64,
    executions_limit: i64,
    expire_at_epoch: i64,
}

/// Single process usage store, useful for tests and demos.
#[derive(Debug, Default)]
pub struct InMemoryIntentTokenUsageStore {
    rows: Mutex<HashMap<String, UsageRow>>,
}

impl InMemoryIntentTokenUsageStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl A4PIntentTokenUsageStore for InMemoryIntentTokenUsageStore {
    fn consume(
        &self,
        token_id: &str,
        max_executions: i64,
        expire_at_epoch: i64,
    ) -> Result<(bool, i64), IntentTokenUsageStoreError> {
        let token_id = validate_consume_arguments(token_id, max_executions, expire_at_epoch)?;
        let now = now_epoch();
        let mut rows = self.rows.lock();
        rows.retain(|_, row| row.expire_at_epoch > now);
        match rows.get_mut(&token_id) {
            None => {
                rows.insert(
                    token_id,
                    UsageRow {
                        executions_used: 1,
                        executions_limit: max_executions,
                        expire_at_epoch,
                    },
                );
                Ok((true, 1))
            }
            Some(row) => {
                if row.executions_limit != max_executions || row.expire_at_epoch != expire_at_epoch {
                    return Err(IntentTokenUsageStoreError(
                        "Stored intent token usage policy does not match the signed token".into(),
                    ));
                }
                if row.executions_used >= max_executions {
                    return Ok((false, row.executions_used));
                }
                row.executions_used += 1;
                Ok((true, row.executions_used))
            }
        }
    }
}

/// SQLite-backed token usage store with atomic cross-process consumption.
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SQLiteIntentTokenUsageStore {
    path: PathBuf,
    timeout: Duration,
}

#[cfg(feature = "sqlite")]
impl SQLiteIntentTokenUsageStore {
    /// Open the store at `path`, or at the configured default path when `None`.
    pub fn new(path: Option<&Path>) -> Result<Self, A4PError> {
        Self::with_timeout(path, 5.0)
    }

    /// Open the store with an explicit busy timeout in seconds.
    pub fn with_timeout(path: Option<&Path>, timeout_seconds: f64) -> Result<Self, A4PError> {
        let configured = match path {
            Some(path) => path.to_string_lossy().to_string(),
            None => default_intent_token_usage_db_path(),
        };
        if configured.trim().is_empty() {
            return Err(A4PError::value(
                "Intent token usage database path must not be empty",
            ));
        }
        if timeout_seconds <= 0.0 {
            return Err(A4PError::value(
                "Intent token usage database timeout must be positive",
            ));
        }
        Ok(Self {
            path: PathBuf::from(configured.trim()),
            timeout: Duration::from_secs_f64(timeout_seconds),
        })
    }

    /// The database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn consume_inner(
        &self,
        connection: &Connection,
        token_id: &str,
        max_executions: i64,
        expire_at_epoch: i64,
    ) -> Result<(bool, i64), IntentTokenUsageStoreError> {
        let unavailable = |_error: rusqlite::Error| {
            IntentTokenUsageStoreError("Intent token usage store unavailable".into())
        };
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS intent_token_usage (
                    token_id TEXT PRIMARY KEY,
                    executions_used INTEGER NOT NULL,
                    executions_limit INTEGER NOT NULL,
                    expire_at_epoch INTEGER NOT NULL
                )",
            )
            .map_err(unavailable)?;
        connection
            .execute(
                "DELETE FROM intent_token_usage WHERE expire_at_epoch <= ?1",
                params![now_epoch()],
            )
            .map_err(unavailable)?;
        let row: Option<UsageRow> = connection
            .query_row(
                "SELECT executions_used, executions_limit, expire_at_epoch
                 FROM intent_token_usage WHERE token_id = ?1",
                params![token_id],
                |row| {
                    Ok(UsageRow {
                        executions_used: row.get(0)?,
                        executions_limit: row.get(1)?,
                        expire_at_epoch: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(unavailable)?;

        let Some(row) = row else {
            connection
                .execute(
                    "INSERT INTO intent_token_usage (
                        token_id, executions_used, executions_limit, expire_at_epoch
                    ) VALUES (?1, ?2, ?3, ?4)",
                    params![token_id, 1_i64, max_executions, expire_at_epoch],
                )
                .map_err(unavailable)?;
            return Ok((true, 1));
        };
        if row.executions_limit != max_executions || row.expire_at_epoch != expire_at_epoch {
            return Err(IntentTokenUsageStoreError(
                "Stored intent token usage policy does not match the signed token".into(),
            ));
        }
        if row.executions_used >= max_executions {
            return Ok((false, row.executions_used));
        }
        let executions_used = row.executions_used + 1;
        connection
            .execute(
                "UPDATE intent_token_usage SET executions_used = ?1 WHERE token_id = ?2",
                params![executions_used, token_id],
            )
            .map_err(unavailable)?;
        Ok((true, executions_used))
    }
}

#[cfg(feature = "sqlite")]
impl A4PIntentTokenUsageStore for SQLiteIntentTokenUsageStore {
    fn consume(
        &self,
        token_id: &str,
        max_executions: i64,
        expire_at_epoch: i64,
    ) -> Result<(bool, i64), IntentTokenUsageStoreError> {
        let token_id = validate_consume_arguments(token_id, max_executions, expire_at_epoch)?;
        let unavailable = || IntentTokenUsageStoreError("Intent token usage store unavailable".into());
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|_| unavailable())?;
            }
        }
        let connection = Connection::open(&self.path).map_err(|_| unavailable())?;
        connection.busy_timeout(self.timeout).map_err(|_| unavailable())?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| unavailable())?;
        match self.consume_inner(&connection, &token_id, max_executions, expire_at_epoch) {
            Ok(result) => {
                connection.execute_batch("COMMIT").map_err(|_| unavailable())?;
                Ok(result)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
}
