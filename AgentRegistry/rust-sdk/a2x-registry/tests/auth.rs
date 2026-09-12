// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/auth/`: bootstrap, role matrix, namespace scoping,
//! holder coercion, key and principal CRUD, audit log and persistence.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use axum::http::StatusCode;
use common::{TestApp, agent_card, bearer, lite_app};
use serde_json::{Value, json};

use a2x_registry::auth::{AuthStore, TOKEN_PREFIX, hash_token};

struct AuthApp {
    app: TestApp,
    admin_token: String,
}

impl AuthApp {
    fn admin(&self) -> String {
        bearer(&self.admin_token)
    }

    async fn auth_dataset(&self) -> TestResult<String> {
        let name = format!("authds_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let r = self
            .app
            .post_h(
                "/api/datasets",
                &[("authorization", &self.admin())],
                json!({"name": name, "auth_required": true}),
            )
            .await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["auth_required"], true);
        Ok(name)
    }

    async fn anon_dataset(&self) -> TestResult<String> {
        let name = format!("anonds_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let r = self.app.post("/api/datasets", json!({"name": name})).await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["auth_required"], false);
        Ok(name)
    }

    /// Provision a principal; returns `(principal_id, token)`.
    async fn provision(&self, role: &str, namespaces: Value) -> TestResult<(String, String)> {
        let handle = format!("{role}_{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let r = self
            .app
            .post_h(
                "/api/auth/principals",
                &[("authorization", &self.admin())],
                json!({"handle": handle, "role": role, "namespaces": namespaces}),
            )
            .await?;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let body = r.json();
        Ok((
            body["principal_id"].as_str().required()?.to_string(),
            body["token"].as_str().required()?.to_string(),
        ))
    }

    async fn provider(&self, ds: &str) -> TestResult<String> {
        Ok(self.provision("provider", json!([ds])).await?.1)
    }

    async fn user(&self, ds: &str) -> TestResult<String> {
        Ok(self.provision("user", json!([ds])).await?.1)
    }

    async fn whoami(&self, token: &str) -> TestResult<Value> {
        let r = self
            .app
            .get_h("/api/auth/whoami", &[("authorization", &bearer(token))])
            .await?;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        Ok(r.json())
    }

    async fn register(&self, ds: &str, token: &str, card: Value) -> TestResult<(StatusCode, Value)> {
        let r = self
            .app
            .post_h(
                &format!("/api/datasets/{ds}/services/a2a"),
                &[("authorization", &bearer(token))],
                json!({"agent_card": card, "dataset": ds, "persistent": true}),
            )
            .await?;
        Ok((r.status, r.json()))
    }

    fn audit(&self) -> TestResult<Vec<Value>> {
        let path = self.app.tmp.path().join("auth_data").join("audit.log");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| -> TestResult<_> { Ok(serde_json::from_str(l)?) })
            .collect::<TestResult<Vec<_>>>()
    }
}

async fn auth_app() -> TestResult<AuthApp> {
    let app = lite_app().await?;
    let (store, token) = AuthStore::bootstrap(Some(&app.tmp.path().join("auth_data")), None, "root")?;
    app.state.set_auth_store(Some(Arc::new(store)));
    Ok(AuthApp {
        app,
        admin_token: token,
    })
}

// ── anon namespace after init ────────────────────────────────────────────

#[tokio::test]
async fn anon_dataset_remains_open_after_init() -> TestResult {
    let a = auth_app().await?;
    let ds = a.anon_dataset().await?;
    let r = a
        .app
        .post(
            &format!("/api/datasets/{ds}/services/a2a"),
            json!({"agent_card": agent_card("anon-write")}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    let r = a.app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert_eq!(r.status, StatusCode::OK);
    let detail = a
        .app
        .get(&format!("/api/datasets/{ds}/services/{sid}"))
        .await?
        .json();
    assert!(detail.get("owner_id").is_none());
    assert!(detail["metadata"].get("owner_id").is_none());
    // Body holder_id is honoured on anonymous namespaces.
    let r = a
        .app
        .post(
            &format!("/api/datasets/{ds}/reservations"),
            json!({"filters": {}, "n": 1, "ttl_seconds": 30, "holder_id": "my_custom_holder"}),
        )
        .await?;
    assert_eq!(r.json()["holder_id"], "my_custom_holder");
    let auth_ds = a.auth_dataset().await?;
    let r = a.app.get(&format!("/api/datasets/{auth_ds}/services")).await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

// ── audit log ────────────────────────────────────────────────────────────

#[tokio::test]
async fn audit_log_events_and_no_plaintext() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    a.provision("user", json!([ds])).await?;
    a.app
        .get_h(
            &format!("/api/datasets/{ds}/services"),
            &[("authorization", "Bearer a2x_pat_obviously_bogus_value")],
        )
        .await?;
    let (other_token, _other) = {
        let other = a.auth_dataset().await?;
        (a.provider(&other).await?, other)
    };
    a.app
        .get_h(
            &format!("/api/datasets/{ds}/services"),
            &[("authorization", &bearer(&other_token))],
        )
        .await?;
    let events = a.audit()?;
    let names: Vec<&str> = events
        .iter()
        .map(|e| -> TestResult<_> { Ok(e["event"].as_str().required()?) })
        .collect::<TestResult<Vec<_>>>()?;
    assert!(names.contains(&"principal.created"));
    assert!(names.iter().filter(|n| **n == "key.created").count() >= 2);
    assert!(
        events.iter().any(|e| e["event"] == "auth.failed"
            && (e["reason"] == "invalid_token" || e["reason"] == "wrong_prefix"))
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "permission.denied" && e["reason"] == "namespace_out_of_scope")
    );
    let raw = std::fs::read_to_string(a.app.tmp.path().join("auth_data/audit.log"))?;
    assert!(
        regex::Regex::new(r"a2x_pat_[A-Za-z0-9_-]{20,}")?
            .find(&raw)
            .is_none()
    );
    assert!(raw.contains("\"event\": \"key.created\""));
    Ok(())
}

// ── auth config toggle ───────────────────────────────────────────────────

#[tokio::test]
async fn auth_config_toggle_and_orphans() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app.get(&format!("/api/datasets/{ds}/auth-config")).await?;
    assert_eq!(r.json(), json!({"dataset": ds, "required": false}));
    let r = app.get("/api/datasets/nope/auth-config").await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post(
            &format!("/api/datasets/{ds}/auth-config"),
            json!({"required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post(
            "/api/datasets",
            json!({"name": "early_bird", "auth_required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(
        r.json()["detail"]
            .as_str()
            .required()?
            .contains("auth_not_initialized")
    );

    let a = auth_app().await?;
    let anon = a.anon_dataset().await?;
    let r = a
        .app
        .post(
            &format!("/api/datasets/{anon}/services/a2a"),
            json!({"agent_card": agent_card("orphan")}),
        )
        .await?;
    let sid = r.json()["service_id"].as_str().required()?.to_string();
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{anon}/auth-config"),
            &[("authorization", &a.admin())],
            json!({"required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"dataset": anon, "required": true, "schema_version": 1})
    );
    assert_eq!(
        a.app
            .get(&format!("/api/datasets/{anon}/auth-config"))
            .await?
            .json()["required"],
        true
    );
    let p = a.provider(&anon).await?;
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{anon}/services/{sid}"),
            &[("authorization", &bearer(&p))],
            json!({"description": "hijacked"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        r.json()["detail"]
            .as_str()
            .required()?
            .to_lowercase()
            .contains("admin")
    );
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{anon}/services/{sid}"),
            &[("authorization", &a.admin())],
            json!({"description": "admin-rescue"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{anon}/auth-config"),
            &[("authorization", &bearer(&p))],
            json!({"required": false}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{anon}/auth-config"),
            &[("authorization", &a.admin())],
            json!({"required": false}),
        )
        .await?;
    assert_eq!(r.json()["required"], false);
    let r = a
        .app
        .post(
            "/api/datasets",
            json!({"name": "needs_admin", "auth_required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a
        .app
        .post_h(
            "/api/datasets",
            &[("authorization", &bearer(&p))],
            json!({"name": "needs_admin2", "auth_required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    Ok(())
}

// ── bootstrap invariants ─────────────────────────────────────────────────

#[test]
fn bootstrap_token_and_files() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("auth_data");
    let (_store, token) = AuthStore::bootstrap(Some(&dir), None, "root")?;
    assert!(token.starts_with(TOKEN_PREFIX));
    assert!(token.len() - TOKEN_PREFIX.len() >= 40);
    let keys: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("api_keys.json"))?)?;
    assert_eq!(keys[0]["key_hash"], hash_token(&token));
    assert!(AuthStore::bootstrap(Some(&dir), None, "root").is_err());
    assert!(AuthStore::load_or_none(Some(&dir))?.is_some());
    assert!(AuthStore::load_or_none(Some(&tmp.path().join("nope")))?.is_none());
    Ok(())
}

// ── holder coercion ──────────────────────────────────────────────────────

#[tokio::test]
async fn holder_id_is_coerced_on_auth_namespaces() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    let u = a.user(&ds).await?;
    let mut card = agent_card("coerce-target");
    card["status"] = json!("online");
    assert_eq!(a.register(&ds, &p, card).await?.0, StatusCode::OK);
    let user_id = a.whoami(&u).await?["principal_id"]
        .as_str()
        .required()?
        .to_string();
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{ds}/reservations"),
            &[("authorization", &bearer(&u))],
            json!({"filters": {}, "n": 1, "ttl_seconds": 60, "holder_id": "attacker_forgery"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["holder_id"], user_id);
    // Release own lease works; provider cannot release the user's holder.
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/reservations/{user_id}"),
            &[("authorization", &bearer(&p))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/reservations/{user_id}"),
            &[("authorization", &bearer(&u))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    // Admin can release anyone's reservation.
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{ds}/reservations"),
            &[("authorization", &bearer(&p))],
            json!({"filters": {}, "n": 1, "ttl_seconds": 60}),
        )
        .await?;
    let p_holder = r.json()["holder_id"].as_str().required()?.to_string();
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/reservations/{p_holder}"),
            &[("authorization", &a.admin())],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    Ok(())
}

// ── key CRUD ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn key_crud_rules() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    let u = a.user(&ds).await?;
    let r = a
        .app
        .post_h(
            "/api/auth/keys",
            &[("authorization", &bearer(&p))],
            json!({"name": "second-laptop"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body = r.json();
    assert!(body["token"].as_str().required()?.starts_with("a2x_pat_"));
    assert_eq!(body["name"], "second-laptop");
    let new_token = body["token"].as_str().required()?.to_string();
    let me = a.whoami(&p).await?;
    assert_eq!(a.whoami(&new_token).await?["principal_id"], me["principal_id"]);

    let p_keys = a
        .app
        .get_h("/api/auth/keys", &[("authorization", &bearer(&p))])
        .await?
        .json();
    assert!(
        p_keys
            .as_array()
            .required()?
            .iter()
            .all(|k| k["principal_id"] == me["principal_id"])
    );
    assert_eq!(p_keys.as_array().required()?.len(), 2);
    assert!(
        p_keys
            .as_array()
            .required()?
            .iter()
            .all(|k| k.get("token").is_none())
    );
    let u_id = a.whoami(&u).await?["principal_id"]
        .as_str()
        .required()?
        .to_string();
    let leak = a
        .app
        .get_h(
            &format!("/api/auth/keys?principal_id={u_id}"),
            &[("authorization", &bearer(&p))],
        )
        .await?
        .json();
    assert!(
        leak.as_array()
            .required()?
            .iter()
            .all(|k| k["principal_id"] == me["principal_id"])
    );
    let all = a
        .app
        .get_h("/api/auth/keys", &[("authorization", &a.admin())])
        .await?
        .json();
    assert!(all.as_array().required()?.len() >= 4);

    // Revoke own key.
    let r = a
        .app
        .delete_h(
            &format!("/api/auth/keys/{}", body["key_id"].as_str().required()?),
            &[("authorization", &bearer(&p))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["revoked_at"].is_string());
    let r = a
        .app
        .get_h("/api/auth/whoami", &[("authorization", &bearer(&new_token))])
        .await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        !a.app
            .state
            .auth_store()
            .required()?
            .has_hash(&hash_token(&new_token))
    );

    // Cannot revoke others' keys; admin can.
    let u_key = a
        .app
        .post_h(
            "/api/auth/keys",
            &[("authorization", &bearer(&u))],
            json!({"name": "victim"}),
        )
        .await?
        .json();
    let r = a
        .app
        .delete_h(
            &format!("/api/auth/keys/{}", u_key["key_id"].as_str().required()?),
            &[("authorization", &bearer(&p))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(
            &format!("/api/auth/keys/{}", u_key["key_id"].as_str().required()?),
            &[("authorization", &a.admin())],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .delete_h("/api/auth/keys/k_missing", &[("authorization", &a.admin())])
        .await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    Ok(())
}

// ── namespace scope ──────────────────────────────────────────────────────

#[tokio::test]
async fn cross_namespace_tokens_are_forbidden() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let other = a.auth_dataset().await?;
    let other_token = a.provider(&other).await?;
    let (status, _) = a
        .register(&ds, &other_token, agent_card("crossns-attack"))
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = a.register(&other, &other_token, agent_card("self-ns-ok")).await?;
    assert_eq!(status, StatusCode::OK);
    let r = a
        .app
        .get_h(
            &format!("/api/datasets/{ds}/services"),
            &[("authorization", &bearer(&other_token))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        r.json()["detail"],
        format!("Principal lacks access to namespace '{ds}'")
    );
    Ok(())
}

// ── owner immutability ───────────────────────────────────────────────────

#[tokio::test]
async fn put_cannot_relabel_owner() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    let (_, body) = a.register(&ds, &p, agent_card("immutable-owner")).await?;
    let sid = body["service_id"].as_str().required()?.to_string();
    let owner = a.whoami(&p).await?["principal_id"]
        .as_str()
        .required()?
        .to_string();
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&p))],
            json!({"description": "ok change", "owner_id": "u_attacker", "service_id": "different", "type": "skill", "source": "user_config"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changed_fields"], json!(["description"]));
    let detail = a
        .app
        .get_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&p))],
        )
        .await?
        .json();
    assert_eq!(detail["owner_id"], owner);
    assert_eq!(detail["type"], "a2a");
    Ok(())
}

// ── persistence round trip ───────────────────────────────────────────────

#[tokio::test]
async fn persistence_roundtrip() -> TestResult {
    let a = auth_app().await?;
    let anon = a.anon_dataset().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    a.app
        .post(
            &format!("/api/datasets/{anon}/services/a2a"),
            json!({"agent_card": agent_card("anon-roundtrip")}),
        )
        .await?;
    let api: Value = serde_json::from_str(&std::fs::read_to_string(
        a.app.database_dir().join(&anon).join("api_config.json"),
    )?)?;
    for svc in api["services"].as_array().required()? {
        assert!(svc.get("owner_id").is_none());
    }
    let (_, body) = a.register(&ds, &p, agent_card("auth-roundtrip")).await?;
    let sid = body["service_id"].as_str().required()?;
    let owner = a.whoami(&p).await?["principal_id"]
        .as_str()
        .required()?
        .to_string();
    let api: Value = serde_json::from_str(&std::fs::read_to_string(
        a.app.database_dir().join(&ds).join("api_config.json"),
    )?)?;
    let entry = api["services"]
        .as_array()
        .required()?
        .iter()
        .find(|s| s["service_id"] == sid)
        .required()?;
    assert_eq!(entry["owner_id"], owner);

    // Restart: a fresh store over the same directory round trips everything.
    let (pid, tok) = a.provision("user", json!([ds])).await?;
    let fresh = AuthStore::load_or_none(Some(&a.app.tmp.path().join("auth_data")))?.required()?;
    assert_eq!(fresh.get_principal(&pid).required()?.role.as_str(), "user");
    assert_eq!(fresh.authenticate(&tok)?.principal_id, pid);
    Ok(())
}

// ── principal CRUD ───────────────────────────────────────────────────────

#[tokio::test]
async fn principal_crud() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let r = a
        .app
        .post_h(
            "/api/auth/principals",
            &[("authorization", &a.admin())],
            json!({"handle": "alice", "role": "provider", "namespaces": [ds]}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body = r.json();
    assert_eq!(body["handle"], "alice");
    assert_eq!(body["role"], "provider");
    assert_eq!(body["namespaces"], json!([ds]));
    assert!(body["token"].as_str().required()?.starts_with("a2x_pat_"));
    assert!(body["key_prefix"].as_str().required()?.starts_with("a2x_pat_"));
    let token = body["token"].as_str().required()?.to_string();
    let plist = a
        .app
        .get_h("/api/auth/principals", &[("authorization", &a.admin())])
        .await?;
    assert_eq!(plist.status, StatusCode::OK);
    assert!(!plist.text().contains(&token));
    assert_eq!(plist.json()[1]["handle"], "alice");

    let r = a
        .app
        .post_h(
            "/api/auth/principals",
            &[("authorization", &a.admin())],
            json!({"handle": "bob", "role": "user"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().to_lowercase().contains("namespaces"));
    let r = a
        .app
        .post_h(
            "/api/auth/principals",
            &[("authorization", &a.admin())],
            json!({"handle": "second_admin", "role": "admin", "namespaces": ["x"]}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().to_lowercase().contains("admin"));
    let r = a
        .app
        .post_h(
            "/api/auth/principals",
            &[("authorization", &a.admin())],
            json!({"handle": "claire", "role": "user", "namespaces": [ds, "does_not_exist"]}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().to_lowercase().contains("unknown namespace"));
    let dup = json!({"handle": "dup", "role": "user", "namespaces": [ds]});
    assert_eq!(
        a.app
            .post_h(
                "/api/auth/principals",
                &[("authorization", &a.admin())],
                dup.clone()
            )
            .await?
            .status,
        StatusCode::CREATED
    );
    let r = a
        .app
        .post_h("/api/auth/principals", &[("authorization", &a.admin())], dup)
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("handle"));
    let r = a
        .app
        .post_h(
            "/api/auth/principals",
            &[("authorization", &bearer(&token))],
            json!({"handle": "sneaky", "role": "user", "namespaces": []}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let u = a.user(&ds).await?;
    assert_eq!(
        a.app
            .get_h("/api/auth/principals", &[("authorization", &bearer(&u))])
            .await?
            .status,
        StatusCode::FORBIDDEN
    );

    // PATCH: add namespace, disable and re-enable.
    let (pid, ptoken) = a.provision("provider", json!([])).await?;
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
            json!({"namespaces": [ds]}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["namespaces"], json!([ds]));
    let r = a
        .app
        .get_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
        )
        .await?;
    assert_eq!(r.json()["id"], pid);
    assert_eq!(
        a.app
            .get_h("/api/auth/principals/u_missing", &[("authorization", &a.admin())])
            .await?
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        a.app
            .patch_h(
                "/api/auth/principals/u_missing",
                &[("authorization", &a.admin())],
                json!({"note": "x"})
            )
            .await?
            .status,
        StatusCode::NOT_FOUND
    );
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
            json!({"disabled": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        a.app
            .get_h("/api/auth/whoami", &[("authorization", &bearer(&ptoken))])
            .await?
            .status,
        StatusCode::UNAUTHORIZED
    );
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
            json!({"disabled": false}),
        )
        .await?;
    assert_eq!(r.json()["disabled_at"], Value::Null);
    let me = a.whoami(&ptoken).await?;
    assert_eq!(me["disabled"], false);
    assert_eq!(me["role"], "provider");
    let events = a.audit()?;
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "principal.updated" && e["principal_id"] == pid)
    );
    Ok(())
}

// ── role transitions ─────────────────────────────────────────────────────

#[tokio::test]
async fn role_transitions() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let (pid, token) = a.provision("provider", json!([ds])).await?;
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
            json!({"role": "admin"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let text = r.text().to_lowercase();
    assert!(text.contains("admin") && text.contains("namespaces"));
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{pid}"),
            &[("authorization", &a.admin())],
            json!({"role": "admin", "namespaces": null}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["role"], "admin");
    assert_eq!(r.json()["namespaces"], Value::Null);
    assert_eq!(
        a.app
            .get_h("/api/auth/principals", &[("authorization", &bearer(&token))])
            .await?
            .status,
        StatusCode::OK
    );
    let (aid, _) = a.provision("admin", Value::Null).await?;
    let r = a
        .app
        .patch_h(
            &format!("/api/auth/principals/{aid}"),
            &[("authorization", &a.admin())],
            json!({"role": "user"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    Ok(())
}

// ── admin role ───────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_can_do_everything() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    assert_eq!(
        a.register(&ds, &a.admin_token.clone(), agent_card("by-admin"))
            .await?
            .0,
        StatusCode::OK
    );
    let (_, body) = a.register(&ds, &p, agent_card("admin-overwrite")).await?;
    let sid = body["service_id"].as_str().required()?;
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &a.admin())],
            json!({"description": "admin updated"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .post_h(
            "/api/datasets",
            &[("authorization", &a.admin())],
            json!({"name": "del_target", "auth_required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .delete_h("/api/datasets/del_target", &[("authorization", &a.admin())])
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    // Provider registration on the auth dataset works and owner_id is set.
    let (_, body) = a.register(&ds, &p, agent_card("by-provider")).await?;
    let sid = body["service_id"].as_str().required()?;
    let me = a.whoami(&p).await?;
    let detail = a
        .app
        .get_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&p))],
        )
        .await?
        .json();
    assert_eq!(detail["owner_id"], me["principal_id"]);
    Ok(())
}

// ── provider role ────────────────────────────────────────────────────────

#[tokio::test]
async fn provider_scope() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    let (_, body) = a.register(&ds, &p, agent_card("self-edit")).await?;
    let sid = body["service_id"].as_str().required()?.to_string();
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&p))],
            json!({"description": "edited"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let p2 = a.provider(&ds).await?;
    let (_, body) = a.register(&ds, &p2, agent_card("p2-owned")).await?;
    let sid2 = body["service_id"].as_str().required()?.to_string();
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{ds}/services/{sid2}"),
            &[("authorization", &bearer(&p))],
            json!({"description": "hijacked"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/services/{sid2}"),
            &[("authorization", &bearer(&p))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&p))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{ds}/auth-config"),
            &[("authorization", &bearer(&p))],
            json!({"required": false}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(&format!("/api/datasets/{ds}"), &[("authorization", &bearer(&p))])
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{ds}/lease-config"),
            &[("authorization", &bearer(&p))],
            json!({"enabled": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Dataset level operations without any token need admin once auth is on.
    let r = a.app.post("/api/providers/x", json!({})).await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

// ── user role ────────────────────────────────────────────────────────────

#[tokio::test]
async fn user_scope() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let p = a.provider(&ds).await?;
    let u = a.user(&ds).await?;
    let mut card = agent_card("u1");
    card["status"] = json!("online");
    let (_, body) = a.register(&ds, &p, card).await?;
    let sid = body["service_id"].as_str().required()?.to_string();
    let r = a
        .app
        .get_h(
            &format!("/api/datasets/{ds}/services"),
            &[("authorization", &bearer(&u))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.json().as_array().required()?.is_empty());
    assert_eq!(
        a.register(&ds, &u, agent_card("user-attempt")).await?.0,
        StatusCode::FORBIDDEN
    );
    let r = a
        .app
        .put_h(
            &format!("/api/datasets/{ds}/services/{sid}"),
            &[("authorization", &bearer(&u))],
            json!({"description": "hacked"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .post_h(
            &format!("/api/datasets/{ds}/reservations"),
            &[("authorization", &bearer(&u))],
            json!({"filters": {}, "n": 1, "ttl_seconds": 60}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let holder = r.json()["holder_id"].as_str().required()?.to_string();
    assert_eq!(r.json()["reservations"][0]["id"], sid);
    let r = a
        .app
        .delete_h(
            &format!("/api/datasets/{ds}/reservations/{holder}"),
            &[("authorization", &bearer(&u))],
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = a
        .app
        .post_h(
            "/api/datasets",
            &[("authorization", &bearer(&u))],
            json!({"name": "user_made", "auth_required": true}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .app
        .delete_h(&format!("/api/datasets/{ds}"), &[("authorization", &bearer(&u))])
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    Ok(())
}

// ── unauthorized paths ───────────────────────────────────────────────────

#[tokio::test]
async fn unauthorized_paths() -> TestResult {
    let a = auth_app().await?;
    let ds = a.auth_dataset().await?;
    let r = a.app.get(&format!("/api/datasets/{ds}/services")).await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers["www-authenticate"], "Bearer");
    assert_eq!(r.json()["detail"], "Authentication required for this namespace");
    for header in [
        "Basic abcd",
        "Bearer ghp_github_style_token_value",
        "Bearer a2x_pat_definitely_not_a_real_key_at_all",
    ] {
        let r = a
            .app
            .get_h(
                &format!("/api/datasets/{ds}/services"),
                &[("authorization", header)],
            )
            .await?;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{header}");
    }
    let r = a.app.get("/api/auth/whoami").await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a
        .app
        .get_h(
            "/api/auth/principals/u_x",
            &[("authorization", "Bearer a2x_pat_nope")],
        )
        .await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let plain = lite_app().await?;
    for path in ["/api/auth/whoami", "/api/auth/principals", "/api/auth/keys"] {
        let r = plain.get(path).await?;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert!(r.text().to_lowercase().contains("auth init"));
    }
    let r = plain.post("/api/auth/keys", json!({"name": "x"})).await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    Ok(())
}
