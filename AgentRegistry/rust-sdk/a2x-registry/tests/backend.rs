// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/backend/`: front end API contract, providers route and
//! the warmup status endpoint.

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use axum::http::{Method, StatusCode};
use common::lite_app;
use serde_json::{Value, json};

fn write_apikey(app: &common::TestApp) -> TestResult {
    std::fs::write(
        &app.state.llm_apikey_path,
        json!({"providers": [
            {"name": "deepseek", "base_url": "x", "model": "deepseek-chat", "api_keys": ["sk-test"]},
            {"name": "aliyun", "base_url": "y", "model": "deepseek-v3.2", "api_keys": ["sk-test"]}
        ]})
        .to_string(),
    )?;
    Ok(())
}

#[tokio::test]
async fn page_load_endpoints_resolve() -> TestResult {
    let app = lite_app().await?;
    write_apikey(&app)?;
    let r = app.get("/api/providers").await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for path in [
        "/api/warmup-status",
        "/api/datasets",
        "/api/datasets/embedding-models",
    ] {
        let r = app.get(path).await?;
        assert_eq!(r.status, StatusCode::OK, "{path}");
    }
    let r = app.get("/api/nope").await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json(), json!({"detail": "Not Found"}));
    let r = app
        .request(Method::PUT, "/api/datasets", &[], Some(json!({})))
        .await?;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn per_dataset_endpoints_are_wired() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let r = app.get(&format!("/api/datasets/{ds}/taxonomy")).await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["detail"], format!("No taxonomy for dataset '{ds}'"));
    let r = app.get(&format!("/api/datasets/{ds}/default-queries")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"source": "", "queries": []}));
    let r = app
        .get(&format!("/api/datasets/{ds}/services?fields=brief"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = app.get(&format!("/api/datasets/{ds}/vector-config")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"dataset": ds, "embedding_model": "all-MiniLM-L6-v2", "embedding_dim": 384})
    );
    let r = app.get(&format!("/api/datasets/{ds}/build/status")).await?;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .post(
            &format!("/api/datasets/{ds}/vector-config"),
            json!({"embedding_model": "shibing624/text2vec-base-chinese"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["embedding_dim"], 768);
    assert_eq!(r.json()["message"], "配置已保存，向量索引将在后台重建");
    let r = app
        .post(
            &format!("/api/datasets/{ds}/vector-config"),
            json!({"embedding_model": "mystery"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .post(
            &format!("/api/datasets/{ds}/vector-config"),
            json!({"embedding_model": "mystery", "embedding_dim": 7}),
        )
        .await?;
    assert_eq!(r.json()["embedding_dim"], 7);

    // Taxonomy tree once files exist, and default queries with $ref.
    let tax = app.database_dir().join(&ds).join("taxonomy");
    std::fs::create_dir_all(&tax)?;
    std::fs::write(
        tax.join("taxonomy.json"),
        json!({"root": "root", "categories": {"root": {"children": [], "services": ["s1"]}}}).to_string(),
    )?;
    std::fs::write(
        tax.join("class.json"),
        json!({"categories": {"root": {"name": "Root"}}}).to_string(),
    )?;
    let r = app.get(&format!("/api/datasets/{ds}/taxonomy")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["name"], "Root");
    assert_eq!(r.json()["services"], json!(["s1"]));
    std::fs::write(
        app.database_dir().join(&ds).join("query/default_queries.json"),
        json!([{"query": "帮我预订航班", "query_en": "Book a flight"}, {"query": "x"}]).to_string(),
    )?;
    let r = app.get(&format!("/api/datasets/{ds}/default-queries")).await?;
    assert_eq!(
        r.json()["source"],
        format!("database/{ds}/query/default_queries.json")
    );
    assert_eq!(r.json()["queries"][1], json!({"query": "x", "query_en": ""}));
    Ok(())
}

#[tokio::test]
async fn providers_route() -> TestResult {
    let app = lite_app().await?;
    write_apikey(&app)?;
    let r = app.get("/api/providers").await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(body["current"], "deepseek");
    let names: Vec<&str> = body["providers"]
        .as_array()
        .required()?
        .iter()
        .map(|p| -> TestResult<_> { Ok(p["name"].as_str().required()?) })
        .collect::<TestResult<Vec<_>>>()?;
    assert_eq!(names, vec!["deepseek", "aliyun"]);
    assert_eq!(body["providers"][1]["model"], "deepseek-v3.2");

    let r = app.post("/api/providers/aliyun", json!({})).await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json(), json!({"status": "ok", "current": "aliyun"}));
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&app.state.llm_apikey_path)?)?;
    assert_eq!(cfg["providers"][0]["name"], "aliyun");
    assert_eq!(app.get("/api/providers").await?.json()["current"], "aliyun");

    let r = app.post("/api/providers/nope", json!({})).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"error": "Unknown provider: nope", "valid": ["aliyun", "deepseek"]})
    );
    Ok(())
}

#[tokio::test]
async fn warmup_status_public_fields() -> TestResult {
    let app = lite_app().await?;
    let r = app.get("/api/warmup-status").await?;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(body["ready"], true);
    assert_eq!(body["stage"], "完成");
    assert_eq!(body["progress"], 100);
    assert_eq!(body["error"], Value::Null);
    app.state.warmup.set("_injected_daemon", json!("handle"));
    let body = app.get("/api/warmup-status").await?.json();
    assert!(body.get("_injected_daemon").is_none());
    assert!(body.as_object().required()?.keys().all(|k| !k.starts_with('_')));
    Ok(())
}

#[tokio::test]
async fn skill_upload_download_delete() -> TestResult {
    let app = lite_app().await?;
    let ds = app.dataset().await?;
    let zip_bytes = {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zf = zip::ZipWriter::new(&mut buf);
        zf.start_file(
            "algorithmic-art/SKILL.md",
            zip::write::SimpleFileOptions::default(),
        )?;
        zf.write_all(
            b"---\nname: algorithmic-art\ndescription: Creating algorithmic art\nlicense: MIT\n---\n# Hi\n",
        )?;
        zf.start_file(
            "algorithmic-art/scripts/a.py",
            zip::write::SimpleFileOptions::default(),
        )?;
        zf.write_all(b"print(1)\n")?;
        zf.finish()?;
        buf.into_inner()
    };
    let boundary = "XBOUNDARY";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skill.zip\"\r\nContent-Type: application/zip\r\n\r\n").as_bytes());
    body.extend_from_slice(&zip_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri(format!("/api/datasets/{ds}/skills"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body.clone()))?;
    let r = app.send(req).await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let resp = r.json();
    assert_eq!(resp["name"], "algorithmic-art");
    assert_eq!(resp["dataset"], ds);
    assert_eq!(resp["status"], "registered");
    let sid = resp["service_id"].as_str().required()?.to_string();
    assert!(sid.starts_with("skill_"));

    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri(format!("/api/datasets/{ds}/skills"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))?;
    assert_eq!(app.send(req).await?.json()["status"], "updated");

    let listed = app.get(&format!("/api/datasets/{ds}/services")).await?.json();
    assert_eq!(listed[0]["type"], "skill");
    assert_eq!(listed[0]["metadata"]["skill_path"], "skills/algorithmic-art");
    assert_eq!(
        listed[0]["metadata"]["files"],
        json!(["SKILL.md", "scripts/a.py"])
    );
    assert_eq!(listed[0]["source"], "skill_folder");

    let r = app.get(&format!("/api/datasets/{ds}/services/{sid}")).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "application/zip");
    assert_eq!(
        r.headers["content-disposition"],
        "attachment; filename=\"algorithmic-art.zip\""
    );
    let r = app
        .get(&format!("/api/datasets/{ds}/skills/algorithmic-art/download"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(r.bytes))?;
    assert_eq!(archive.len(), 2);
    assert_eq!(archive.by_index(0)?.name(), "SKILL.md");
    assert_eq!(
        app.get(&format!("/api/datasets/{ds}/skills/missing/download"))
            .await?
            .status,
        StatusCode::NOT_FOUND
    );

    let r = app
        .put(
            &format!("/api/datasets/{ds}/services/{sid}"),
            json!({"license": "Apache-2.0", "extra": 1}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .put(
            &format!("/api/datasets/{ds}/services/{sid}"),
            json!({"license": "Apache-2.0"}),
        )
        .await?;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let md = std::fs::read_to_string(
        app.database_dir()
            .join(&ds)
            .join("skills/algorithmic-art/SKILL.md"),
    )?;
    assert!(md.contains("license: Apache-2.0"));
    let r = app.delete(&format!("/api/datasets/{ds}/services/{sid}")).await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .delete(&format!("/api/datasets/{ds}/skills/algorithmic-art"))
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"name": "algorithmic-art", "dataset": ds, "service_id": sid, "status": "deleted"})
    );
    assert!(
        app.database_dir()
            .join(&ds)
            .join("removed_skills/algorithmic-art/SKILL.md")
            .exists()
    );
    let r = app
        .delete(&format!("/api/datasets/{ds}/skills/algorithmic-art"))
        .await?;
    assert_eq!(r.json()["status"], "not_found");
    Ok(())
}
