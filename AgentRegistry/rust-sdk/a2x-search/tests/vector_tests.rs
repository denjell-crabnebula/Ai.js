// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a2x_search::taxonomy::QueryObject;
use a2x_search::vector::{EmbeddingModel, HashingEmbedding, VectorStore, collection_name_for_dataset};
use a2x_search::{
    DEFAULT_EMBEDDING_MODEL, VectorEvaluator, VectorIndexBuilder, VectorSearch, VectorSearchConfig,
    embedding_models,
};
use common::{search_taxonomy, write_search_dataset};

#[tokio::test]
async fn vector_search_with_hashing_embedding() -> TestResult {
    let dir = tempfile::tempdir()?;
    write_search_dataset(dir.path())?;
    let persist = dir.path().join("chroma");
    let config = VectorSearchConfig::new(
        &dir.path().join("service.json"),
        &collection_name_for_dataset("Test-DS"),
        &persist,
        "hashing-64",
    );
    let model: Arc<dyn EmbeddingModel> = Arc::new(HashingEmbedding::named("hashing-64", 64));
    let search = VectorSearch::new(config.clone(), model.clone()).await?;
    assert_eq!(search.store().count(), 7);
    assert_eq!(search.store().stored_embedding_model(), Some("hashing-64"));
    assert!(persist.join("test_ds.json").exists());

    let (results, stats) = search.search("book flight tickets", 3).await?;
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].id, "svc_flight_book");
    assert_eq!(results[0].name, "Flight Booking");
    assert_eq!(stats.llm_calls, 0);
    let (results, _) = search.search("reserve hotel rooms", 100).await?;
    assert_eq!(results[0].id, "svc_hotel_book");
    assert_eq!(results.len(), 7);

    // Reopening reuses the persisted index without re-embedding.
    let reopened = VectorSearch::new(config.clone(), model.clone()).await?;
    assert_eq!(reopened.store().count(), 7);
    // force_rebuild clears and re-embeds
    let rebuilt = VectorSearch::new(config.with_force_rebuild(true), model).await?;
    assert_eq!(rebuilt.store().count(), 7);
    Ok(())
}

#[tokio::test]
async fn store_sync_semantics_match_backend_flow() -> TestResult {
    // Mirrors SearchService.sync_vector: model mismatch -> full rebuild,
    // otherwise upsert changed descriptions and delete removed ids.
    let dir = tempfile::tempdir()?;
    let (_, _, services) = search_taxonomy()?;
    let model = HashingEmbedding::named("m1", 32);
    let builder = VectorIndexBuilder::new("ds", dir.path(), "m1");
    let mut store = builder.build_from_services(&services, false, &model).await?;
    assert_eq!(store.count(), 7);

    let existing = store.get_all_docs();
    let mut target = existing.clone();
    target.insert("svc_new".into(), "Brand new service".into());
    target.insert("svc_currency".into(), "Changed description".into());
    target.shift_remove("svc_restaurant");
    let to_delete: Vec<String> = existing
        .keys()
        .filter(|k| !target.contains_key(*k))
        .cloned()
        .collect();
    let to_upsert: Vec<(String, String)> = target
        .iter()
        .filter(|(id, desc)| existing.get(*id) != Some(desc))
        .map(|(id, d)| (id.clone(), d.clone()))
        .collect();
    assert_eq!(to_delete, vec!["svc_restaurant"]);
    assert_eq!(to_upsert.len(), 2);
    store.delete_ids(&to_delete)?;
    let ids: Vec<String> = to_upsert.iter().map(|(i, _)| i.clone()).collect();
    let texts: Vec<String> = to_upsert.iter().map(|(_, t)| t.clone()).collect();
    let embeddings = model.embed(&texts).await?;
    store.upsert(&ids, &texts, &embeddings)?;
    assert_eq!(store.count(), 7);
    assert_eq!(store.get("svc_currency").required()?.text, "Changed description");

    // model mismatch detection
    let reopened = VectorStore::open("ds", dir.path(), Some("m2"))?;
    assert_eq!(reopened.stored_embedding_model(), Some("m1"));
    assert_ne!(reopened.stored_embedding_model(), Some("m2"));
    let mut reopened = reopened;
    reopened.clear()?;
    let fresh = VectorStore::open("ds", dir.path(), Some("m2"))?;
    assert_eq!(fresh.stored_embedding_model(), Some("m2"));
    assert_eq!(fresh.count(), 0);
    Ok(())
}

#[test]
fn embedding_table_is_lite_safe() -> TestResult {
    let models = embedding_models();
    assert!(models.contains_key(DEFAULT_EMBEDDING_MODEL));
    assert_eq!(DEFAULT_EMBEDDING_MODEL, "all-MiniLM-L6-v2");
    let json = serde_json::to_value(&models)?;
    assert_eq!(
        json["paraphrase-multilingual-MiniLM-L12-v2"]["language"],
        "multilingual"
    );
    Ok(())
}

#[tokio::test]
async fn vector_evaluator_writes_results() -> TestResult {
    let dir = tempfile::tempdir()?;
    write_search_dataset(dir.path())?;
    let queries = vec![
        QueryObject {
            id: "q1".into(),
            query: "book flight tickets".into(),
            correct_tools: vec![a2x_search::taxonomy::CorrectTool {
                id: "svc_flight_book".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        QueryObject {
            id: "q2".into(),
            query: "find restaurants nearby".into(),
            correct_tools: vec![
                a2x_search::taxonomy::CorrectTool {
                    id: "svc_restaurant".into(),
                    ..Default::default()
                },
                a2x_search::taxonomy::CorrectTool {
                    id: "missing".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    ];
    let query_file = dir.path().join("query.json");
    std::fs::write(&query_file, serde_json::to_string(&queries)?)?;

    let config = VectorSearchConfig::new(
        &dir.path().join("service.json"),
        "eval",
        &dir.path().join("chroma"),
        "hashing",
    );
    let evaluator = VectorEvaluator::new(
        config,
        2,
        Some(vec![1, 2]),
        Arc::new(HashingEmbedding::named("hashing", 64)),
    )
    .await?;
    let out = dir.path().join("results");
    let (qm, overall) = evaluator.evaluate_batch(&query_file, None, Some(&out)).await?;
    assert_eq!(qm.len(), 2);
    assert_eq!(qm[0].found_tools.len(), 2);
    assert_eq!(qm[0].found_tools[0], "svc_flight_book");
    assert_eq!(qm[0].metrics_at_k["1"].hit, 1.0);
    assert_eq!(qm[0].metrics_at_k["1"].mrr, 1.0);
    assert!((qm[1].recall - 0.5).abs() < 1e-9);
    assert_eq!(overall.total_queries, 2);
    assert_eq!(overall.hit_rate, 1.0);
    assert_eq!(overall.metrics_at_k.len(), 2);

    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json"))?)?;
    assert_eq!(summary["top_k"], 2);
    assert!(summary["metrics_at_k"]["2"]["ndcg"].is_number());
    let config_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("config.json"))?)?;
    assert_eq!(config_json["embedding_model"], "hashing");
    assert_eq!(config_json["top_k_list"], serde_json::json!([1, 2]));
    let results: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("evaluation_results.json"))?)?;
    assert_eq!(results["query_metrics"][0]["query_id"], "q1");
    assert!(results["overall_metrics"]["metrics_at_k"]["1"]["precision"].is_number());

    // max_queries truncation and empty run
    let (qm, overall) = evaluator.evaluate_batch(&query_file, Some(1), None).await?;
    assert_eq!(qm.len(), 1);
    assert_eq!(overall.total_queries, 1);
    let (qm, overall) = evaluator.evaluate_batch(&query_file, Some(0), None).await?;
    assert!(qm.is_empty());
    assert_eq!(overall.total_queries, 0);
    Ok(())
}
