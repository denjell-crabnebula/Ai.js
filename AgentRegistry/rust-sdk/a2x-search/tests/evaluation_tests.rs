// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::sync::Arc;

use a2x_search::evaluation::{EvaluateOptions, save_error_report};
use a2x_search::taxonomy::ServiceRecord;
use a2x_search::testing::FakeLlm;
use a2x_search::{A2xEvaluator, A2xSearchConfig, SearchMode, TraditionalEvaluator, TraditionalSearch};
use common::{shared, word_overlap_llm, write_search_dataset};
use serde_json::json;

fn write_queries(dir: &std::path::Path) -> TestResult<std::path::PathBuf> {
    let queries = json!([
        {"id": "q1", "query": "flight ticket", "correct_tools": [{"id": "svc_flight_book", "name": "Flight Booking"}]},
        {"id": "q2", "query": "stock prices", "correct_tools": [{"id": "svc_stock_price"}, {"id": "svc_hotel_book"}]},
        {"id": "q3", "query": "restaurants nearby", "correct_tools": [{"id": "svc_restaurant"}]}
    ]);
    let path = dir.join("query").join("query.json");
    std::fs::create_dir_all(path.parent().required()?)?;
    std::fs::write(&path, serde_json::to_string_pretty(&queries)?)?;
    Ok(path)
}

#[tokio::test]
async fn a2x_evaluator_writes_all_files_and_resumes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let ds = dir.path().join("DS");
    write_search_dataset(&ds)?;
    let query_file = write_queries(&ds)?;
    let out = dir.path().join("results").join("run1");

    let llm = shared(word_overlap_llm()?);
    let config = A2xSearchConfig::for_dataset_dir(&ds)
        .with_mode(SearchMode::GetAll)
        .with_max_workers(3);
    let evaluator = A2xEvaluator::new(config, llm.clone())?;
    let opts = EvaluateOptions {
        query_file: query_file.clone(),
        max_queries: None,
        output_dir: Some(out.clone()),
        experiment_id: Some("exp1".into()),
        notes: "workers=3".into(),
    };
    let (qm, overall) = evaluator.evaluate_batch(&opts).await?;
    assert_eq!(qm.len(), 3);
    assert_eq!(qm[0].query_id, "q1");
    assert_eq!(qm[0].found_tools, vec!["svc_flight_search", "svc_flight_book"]);
    assert!((qm[0].precision - 0.5).abs() < 1e-9);
    assert_eq!(qm[0].recall, 1.0);
    assert_eq!(qm[0].llm_calls, 3);
    assert_eq!(qm[0].visited_categories, 2);
    assert_eq!(qm[0].pruned_categories, 3);
    assert_eq!(
        qm[0].visited_category_ids.as_deref(),
        Some(&["root".to_string(), "cat_travel".into(), "cat_flights".into()][..])
    );
    assert_eq!(qm[1].missed_tools, vec!["svc_hotel_book"]);
    assert_eq!(overall.total_queries, 3);
    assert_eq!(overall.hit_rate, 1.0);
    assert!((overall.avg_recall - (1.0 + 0.5 + 1.0) / 3.0).abs() < 1e-9);
    assert!(overall.avg_f1 > 0.0);

    for name in [
        "config.json",
        "evaluation_results.json",
        "summary.json",
        "partial_results.jsonl",
        "error_analysis.json",
        "error_analysis.md",
    ] {
        assert!(out.join(name).exists(), "{name} missing");
    }
    let config_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("config.json"))?)?;
    assert_eq!(config_json["experiment_id"], "exp1");
    assert_eq!(config_json["search_config"]["mode"], "get_all");
    assert_eq!(config_json["search_config"]["max_workers"], 3);
    assert_eq!(config_json["query_count"], 3);
    assert_eq!(config_json["notes"], "workers=3");
    assert!(
        config_json["taxonomy_path"]
            .as_str()
            .required()?
            .ends_with("taxonomy.json")
    );
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json"))?)?;
    assert_eq!(summary["dataset"], "DS");
    assert_eq!(summary["mode"], "get_all");
    assert_eq!(summary["hit_rate"], 1.0);
    let results: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("evaluation_results.json"))?)?;
    assert_eq!(results["query_metrics"].as_array().required()?.len(), 3);
    assert_eq!(results["overall_metrics"]["total_queries"], 3);
    assert!(results["elapsed_time"].is_number());

    let analysis: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("error_analysis.json"))?)?;
    assert_eq!(analysis["summary"]["total_queries"], 3);
    assert_eq!(analysis["summary"]["perfect_queries"], 2);
    assert_eq!(analysis["summary"]["error_queries"], 1);
    assert_eq!(analysis["summary"]["category_A_count"], 1);
    assert_eq!(analysis["summary"]["total_wrong_tools"], 1);
    let detail = &analysis["detailed_errors"][0];
    assert_eq!(detail["query_id"], "q2");
    assert_eq!(detail["error_type"], "A_navigation_failure");
    assert_eq!(
        detail["missed_tools_A_navigation"][0]["tool_id"],
        "svc_hotel_book"
    );
    assert_eq!(
        detail["correct_tools_detail"][1]["category_paths"][0]["path_str"],
        "root(All API Services) → cat_travel(Travel & Tourism) → cat_hotels(Hotels)"
    );
    assert_eq!(
        analysis["error_categories"]["A_navigation_failure"][0]["query_id"],
        "q2"
    );
    let md = std::fs::read_to_string(out.join("error_analysis.md"))?;
    assert!(md.starts_with("# Error Analysis Report\n"));
    assert!(md.contains("| Category A (Navigation Failure) | 1 |"));
    assert!(md.contains("# Category A: 没有进入正确分类"));
    assert!(md.contains("**请求内容**: stock prices"));
    assert!(md.contains("- 路径: root(All API Services) → cat_finance(Finance)"));

    // Checkpoint lines carry the original index.
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(out.join("partial_results.jsonl"))?
        .lines()
        .map(|l| -> TestResult<_> { Ok(serde_json::from_str(l)?) })
        .collect::<TestResult<Vec<_>>>()?;
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|l| l["_index"].is_number()));

    // A second run resumes from the checkpoint and does no new LLM calls.
    let calls_before = llm.calls();
    let (qm2, overall2) = evaluator.evaluate_batch(&opts).await?;
    assert_eq!(qm2.len(), 3);
    assert_eq!(overall2.total_queries, 3);
    assert_eq!(llm.calls(), calls_before);
    assert_eq!(qm2[2].query_id, "q3");

    // Regenerating the report from disk works standalone.
    let md_path = save_error_report(&out)?;
    assert!(md_path.ends_with("error_analysis.md"));
    assert!(save_error_report(&dir.path().join("nowhere")).is_err());
    Ok(())
}

#[tokio::test]
async fn evaluate_single_and_max_queries() -> TestResult {
    let dir = tempfile::tempdir()?;
    let ds = dir.path().join("DS");
    write_search_dataset(&ds)?;
    let query_file = write_queries(&ds)?;
    let evaluator = A2xEvaluator::new(A2xSearchConfig::for_dataset_dir(&ds), shared(word_overlap_llm()?))?;
    let opts = EvaluateOptions {
        query_file,
        max_queries: Some(1),
        output_dir: None,
        experiment_id: None,
        notes: String::new(),
    };
    let (qm, overall) = evaluator.evaluate_batch(&opts).await?;
    assert_eq!(qm.len(), 1);
    assert_eq!(overall.total_queries, 1);
    let q = a2x_search::taxonomy::QueryObject {
        id: "x".into(),
        query: "zzz".into(),
        ..Default::default()
    };
    let m = evaluator.evaluate_single_query(&q).await;
    assert_eq!(m.found_count, 0);
    assert!(!m.hit);
    assert_eq!(m.recall, 0.0);
    Ok(())
}

#[tokio::test]
async fn traditional_evaluator_files() -> TestResult {
    let dir = tempfile::tempdir()?;
    let ds = dir.path().join("DS");
    write_search_dataset(&ds)?;
    let query_file = write_queries(&ds)?;
    let llm = Arc::new(FakeLlm::new().on_fn(|p| {
        let q = p
            .lines()
            .skip_while(|l| *l != "## User Query")
            .nth(2)?
            .to_string();
        let ids: Vec<String> = p
            .lines()
            .filter(|l| l.starts_with("- ["))
            .filter(|l| {
                q.split_whitespace()
                    .any(|w| l.to_lowercase().contains(&w.to_lowercase()))
            })
            .filter_map(|l| l.find(']').map(|end| l[3..end].to_string()))
            .collect();
        Some(json!({"service_ids": ids}).to_string())
    }));
    let evaluator = TraditionalEvaluator::new(&ds.join("service.json"), 2, llm.clone())?;
    let out = dir.path().join("trad");
    let (qm, overall) = evaluator.evaluate_batch(&query_file, None, Some(&out)).await?;
    assert_eq!(qm.len(), 3);
    assert_eq!(qm[0].found_tools, vec!["svc_flight_search", "svc_flight_book"]);
    assert_eq!(qm[0].llm_calls, 1);
    assert_eq!(overall.total_llm_calls, 3);
    assert_eq!(overall.hit_rate, 1.0);
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json"))?)?;
    assert_eq!(summary["method"], "traditional");
    assert_eq!(summary["avg_llm_calls"], 1.0);
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("config.json"))?)?;
    assert_eq!(config["service_count"], 7);
    assert_eq!(config["query_count"], 3);
    assert!(out.join("evaluation_results.json").exists());

    let searcher = Arc::new(TraditionalSearch::from_services(
        vec![ServiceRecord::new("tool_1", "A", "a")],
        llm,
    ));
    let ev = TraditionalEvaluator::with_searcher(searcher, 1);
    let (qm, overall) = ev.evaluate_batch(&query_file, Some(0), None).await?;
    assert!(qm.is_empty());
    assert_eq!(overall.total_queries, 0);
    Ok(())
}
