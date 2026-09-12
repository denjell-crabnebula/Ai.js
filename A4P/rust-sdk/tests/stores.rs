// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_stores.py`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;

use a4p::A4PError;
use a4p::credential_store::{A4PCredentialStore, JsonFileCredentialStore, UserCredentialRecord};
use a4p::intent::usage_store::{A4PIntentTokenUsageStore, SQLiteIntentTokenUsageStore};
use common::obj;
use serde_json::{Value, json};

fn now() -> i64 {
    a4p::util::now_epoch()
}

#[test]
fn json_file_credential_store_persists_records() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("credentials.json");
    let store = JsonFileCredentialStore::new(&path)?;
    let mut record = UserCredentialRecord::new(
        "demo-user",
        "cred-1",
        "webauthn",
        obj(json!({"format": "cose", "value": "public-key"})),
    );
    record.details = obj(json!({"signCount": 1, "rpId": "localhost", "origin": "http://localhost:8970"}));

    store.save(record.clone())?;
    let mut updated = record.clone();
    updated.details.insert("signCount".into(), Value::from(2));
    store.save(updated)?;

    let loaded = JsonFileCredentialStore::new(&path)?.get("cred-1")?.required()?;
    assert_eq!(loaded.details["signCount"], 2);
    assert_eq!(loaded.user_id, "demo-user");
    let payload: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["credentials"].as_array().required()?.len(), 1);
    assert_eq!(payload["credentials"][0]["credentialId"], "cred-1");
    Ok(())
}

#[test]
fn json_file_credential_store_refreshes_cross_process_records() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("credentials.json");
    let server_store = JsonFileCredentialStore::new(&path)?;
    let authorizer_store = JsonFileCredentialStore::new(&path)?;
    let mut record = UserCredentialRecord::new(
        "demo-user",
        "cred-1",
        "webauthn",
        obj(json!({"format": "cose", "value": "public-key"})),
    );
    record.details = obj(json!({"signCount": 1}));

    authorizer_store.save(record)?;

    let loaded = server_store.get("cred-1")?.required()?;
    assert_eq!(loaded.user_id, "demo-user");
    let ids: Vec<String> = server_store
        .list_for_user("demo-user")?
        .into_iter()
        .map(|item| item.credential_id)
        .collect();
    assert_eq!(ids, vec!["cred-1".to_string()]);
    assert_eq!(server_store.list_all()?.len(), 1);
    Ok(())
}

#[test]
fn json_file_credential_store_rejects_legacy_format() -> TestResult {
    let legacy_payloads = [
        json!([]),
        json!({"credentials": []}),
        json!([{"userId": "demo-user", "credentialId": "legacy"}]),
    ];
    for legacy in legacy_payloads {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, serde_json::to_string(&legacy)?)?;
        let error = JsonFileCredentialStore::new(&path).err_or_fail()?;
        assert!(matches!(error, A4PError::CredentialStoreFormat(_)), "{legacy}");
        assert!(
            error
                .to_string()
                .contains("delete the old credential file and register credentials again"),
            "{error}"
        );
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("credentials.json");
    std::fs::write(&path, r#"{"schemaVersion": 2, "credentials": {}}"#)?;
    let error = JsonFileCredentialStore::new(&path).err_or_fail()?;
    assert!(error.to_string().contains("'credentials' must be a list"));
    Ok(())
}

#[test]
fn sqlite_usage_store_consumes_atomically_across_connections() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("usage.sqlite3");
    let stores = [
        Arc::new(SQLiteIntentTokenUsageStore::new(Some(&path))?),
        Arc::new(SQLiteIntentTokenUsageStore::new(Some(&path))?),
    ];
    let expire_at_epoch = now() + 60;
    let handles: Vec<_> = stores
        .iter()
        .map(|store| {
            let store = store.clone();
            std::thread::spawn(move || store.consume("token-1", 1, expire_at_epoch))
        })
        .collect();
    let mut results: Vec<(bool, i64)> = Vec::new();
    for handle in handles {
        let outcome = handle
            .join()
            .map_err(|_| ap_support::testing::TestFailure::new("consume thread panicked"))?;
        results.push(outcome?);
    }
    results.sort();
    assert_eq!(results, vec![(false, 1), (true, 1)]);
    Ok(())
}

#[test]
fn sqlite_usage_store_removes_expired_records() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("usage.sqlite3");
    let store = SQLiteIntentTokenUsageStore::new(Some(&path))?;
    let future = now() + 60;
    assert_eq!(store.consume("expired-token", 1, future)?, (true, 1));

    let connection = rusqlite::Connection::open(&path)?;
    connection.execute(
        "UPDATE intent_token_usage SET expire_at_epoch = 1 WHERE token_id = ?1",
        rusqlite::params!["expired-token"],
    )?;
    drop(connection);

    assert_eq!(store.consume("active-token", 1, future)?, (true, 1));
    let connection = rusqlite::Connection::open(&path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM intent_token_usage WHERE token_id = ?1",
        rusqlite::params!["expired-token"],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn sqlite_usage_store_detects_policy_mismatch_and_validates_arguments() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("usage.sqlite3");
    let store = SQLiteIntentTokenUsageStore::new(Some(&path))?;
    let future = now() + 60;
    assert_eq!(store.consume("token-1", 2, future)?, (true, 1));
    let error = store.consume("token-1", 3, future).err_or_fail()?;
    assert!(error.to_string().contains("does not match the signed token"));
    assert!(store.consume("", 1, future).is_err());
    assert!(store.consume("token-1", 0, future).is_err());
    assert!(store.consume("token-1", 1, 0).is_err());
    assert!(SQLiteIntentTokenUsageStore::with_timeout(Some(&path), 0.0).is_err());
    assert!(SQLiteIntentTokenUsageStore::new(Some(std::path::Path::new(" "))).is_err());
    Ok(())
}
