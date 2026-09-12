// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

pub mod common;

use ap_support::testing::{OptionExt, TestResult};
use std::collections::HashSet;
use std::sync::Arc;

use a2x_common::LlmBackend;
use a2x_search::search::selector::{MIN_GROUP_SIZE, ServiceSelector};
use a2x_search::search::{NavigationStep, StreamMessage, TerminalNode};
use a2x_search::taxonomy::TreeIndex;
use a2x_search::{A2xSearch, A2xSearchConfig, SearchMode};
use common::{search_services_index, search_taxonomy, shared, word_overlap_llm, write_search_dataset};
use parking_lot::Mutex;

fn searcher(mode: SearchMode, llm: Arc<dyn LlmBackend>) -> TestResult<A2xSearch> {
    let (taxonomy, classes, services) = search_taxonomy()?;
    let config = A2xSearchConfig::for_dataset_dir(std::path::Path::new("db/Test")).with_mode(mode);
    Ok(A2xSearch::from_parts(
        config,
        taxonomy,
        classes,
        a2x_search::taxonomy::index_services(services),
        llm,
    ))
}

#[tokio::test]
async fn get_all_navigates_and_selects() -> TestResult {
    let llm = shared(word_overlap_llm()?);
    let s = searcher(SearchMode::GetAll, llm.clone())?;
    let steps = Arc::new(Mutex::new(Vec::<NavigationStep>::new()));
    let steps2 = steps.clone();
    let cb = move |step: NavigationStep| steps2.lock().push(step);
    let (results, stats) = s.search_with_callback("flight ticket", Some(&cb)).await;

    let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["svc_flight_search", "svc_flight_book"]);
    assert_eq!(results[1].name, "Flight Booking");
    assert_eq!(results[1].description, "Book flight tickets");
    assert_eq!(stats.llm_calls, 3);
    assert_eq!(stats.total_tokens, 30);
    assert_eq!(
        stats.visited_categories,
        vec![
            "All API Services/Travel & Tourism",
            "All API Services/Travel & Tourism/Flights"
        ]
    );
    let pruned: HashSet<&str> = stats.pruned_categories.iter().map(String::as_str).collect();
    assert_eq!(
        pruned,
        [
            "All API Services/Finance",
            "All API Services/Food",
            "All API Services/Travel & Tourism/Hotels"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        stats.visited_category_ids,
        vec!["root", "cat_travel", "cat_flights"]
    );

    let steps = steps.lock();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0].parent_id, "root");
    assert_eq!(steps[0].selected, vec!["cat_travel"]);
    assert_eq!(steps[0].pruned, vec!["cat_finance", "cat_food"]);
    assert_eq!(steps[1].parent_id, "cat_travel");
    assert_eq!(steps[1].selected, vec!["cat_flights"]);
    assert_eq!(steps[2].parent_id, NavigationStep::PHASE2);

    // The merged group contained the root's direct service too.
    let service_prompt = llm
        .prompts()
        .into_iter()
        .find(|p| p.contains("Services:\n"))
        .required()?;
    assert!(service_prompt.contains("3. Universal Search: Search anything"));
    assert!(service_prompt.starts_with("Select ALL services"));
    Ok(())
}

#[tokio::test]
async fn leaf_domain_and_no_match() -> TestResult {
    let llm = shared(word_overlap_llm()?);
    let s = searcher(SearchMode::GetImportant, llm.clone())?;
    let (results, stats) = s.search("stock prices").await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "svc_stock_price");
    assert_eq!(stats.visited_category_ids, vec!["root", "cat_finance"]);
    assert!(llm.prompts()[0].starts_with("Analyze the user's request step by step"));

    let (results, stats) = s.search("zzz qqq").await;
    assert!(results.is_empty());
    // root categories pruned, root direct services still offered to the LLM
    assert_eq!(stats.llm_calls, 2);
    assert_eq!(stats.visited_categories.len(), 0);
    assert_eq!(stats.pruned_categories.len(), 3);
    Ok(())
}

#[tokio::test]
async fn get_one_falls_back_to_get_important() -> TestResult {
    let llm = shared(word_overlap_llm()?);
    let s = searcher(SearchMode::GetOne, llm.clone())?;
    let steps = Arc::new(Mutex::new(Vec::<NavigationStep>::new()));
    let steps2 = steps.clone();
    let cb = move |step: NavigationStep| steps2.lock().push(step);
    let (results, stats) = s.search_with_callback("zzz", Some(&cb)).await;
    assert!(results.is_empty());
    assert_eq!(stats.llm_calls, 4);
    let markers: Vec<String> = steps.lock().iter().map(|s| s.parent_id.clone()).collect();
    assert!(markers.iter().any(|m| m == NavigationStep::FALLBACK));
    assert!(llm.prompts()[0].starts_with("Think about what specific service"));
    assert!(
        llm.prompts()
            .iter()
            .any(|p| p.starts_with("Analyze the user's request"))
    );

    // A successful get_one keeps only one result and includes the boundary hint.
    let (results, _) = s.search("flight ticket").await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "svc_flight_search");
    let prompt = llm
        .prompts()
        .into_iter()
        .rev()
        .find(|p| p.contains("Current path:"))
        .required()?;
    assert!(prompt.contains("Current path: All API Services/Travel & Tourism"));
    Ok(())
}

#[tokio::test]
async fn sequential_mode_and_streaming() -> TestResult {
    let llm = shared(word_overlap_llm()?);
    let (taxonomy, classes, services) = search_taxonomy()?;
    let config = A2xSearchConfig::for_dataset_dir(std::path::Path::new("db/Test")).with_parallel(false);
    let s = Arc::new(A2xSearch::from_parts(
        config,
        taxonomy,
        classes,
        a2x_search::taxonomy::index_services(services),
        llm,
    ));
    let mut rx = s.search_streaming("flight ticket");
    let mut messages = Vec::new();
    while let Some(m) = rx.recv().await {
        messages.push(m);
    }
    assert_eq!(messages.len(), 4);
    let first = serde_json::to_value(&messages[0])?;
    assert_eq!(first["type"], "step");
    assert_eq!(first["parent_id"], "root");
    match &messages[3] {
        StreamMessage::Result { results, stats } => {
            assert_eq!(results.len(), 2);
            assert_eq!(stats.llm_calls, 3);
            assert_eq!(stats.visited_categories, 2);
            assert_eq!(stats.pruned_categories, 3);
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    let last = serde_json::to_value(&messages[3])?;
    assert_eq!(last["type"], "result");
    assert_eq!(last["results"][0]["id"], "svc_flight_search");
    Ok(())
}

#[tokio::test]
async fn loads_from_files() -> TestResult {
    let dir = tempfile::tempdir()?;
    write_search_dataset(dir.path())?;
    let config = A2xSearchConfig::for_dataset_dir(dir.path()).with_mode(SearchMode::GetAll);
    let s = A2xSearch::new(config.clone(), shared(word_overlap_llm()?))?;
    assert_eq!(s.config(), &config);
    assert_eq!(s.services().len(), 7);
    let (results, _) = s.search("restaurants nearby").await;
    assert_eq!(results[0].id, "svc_restaurant");
    assert!(
        A2xSearch::new(
            A2xSearchConfig::for_dataset_dir(&dir.path().join("nope")),
            shared(word_overlap_llm()?)
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn merge_small_groups_by_lca() -> TestResult {
    let (taxonomy, _, _) = search_taxonomy()?;
    let tree = TreeIndex::new(&taxonomy);
    let services = search_services_index()?;
    let llm = word_overlap_llm()?;
    let selector = ServiceSelector::new(&llm, &services, &tree, 4, true);

    let big: Vec<String> = (0..MIN_GROUP_SIZE).map(|i| format!("big_{i}")).collect();
    let nodes = vec![
        TerminalNode {
            category_id: "cat_flights".into(),
            service_ids: vec!["svc_flight_search".into(), "svc_flight_book".into()],
        },
        TerminalNode {
            category_id: "cat_food".into(),
            service_ids: big.clone(),
        },
        TerminalNode {
            category_id: "cat_hotels".into(),
            service_ids: vec!["svc_hotel_book".into(), "svc_flight_book".into()],
        },
        TerminalNode {
            category_id: "root".into(),
            service_ids: vec!["svc_generic_search".into()],
        },
    ];
    let deduped = selector.deduplicate(nodes);
    assert_eq!(deduped[2].service_ids, vec!["svc_hotel_book"]);
    let groups = selector.merge_small_groups(deduped);
    // Every group under MIN_GROUP_SIZE is merged into its nearest neighbour
    // (deepest common ancestor first) until one group remains: hotels join
    // flights (LCA travel), then the root service, then the merged travel
    // group joins the big food group.
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.service_ids.len(), MIN_GROUP_SIZE + 4);
    assert_eq!(g.leaf_ids.len(), 4);
    assert_eq!(g.service_ids[0], "big_0");
    assert_eq!(g.service_ids[MIN_GROUP_SIZE], "svc_flight_search");
    assert_eq!(g.service_ids[MIN_GROUP_SIZE + 2], "svc_hotel_book");
    assert_eq!(g.service_ids[MIN_GROUP_SIZE + 3], "svc_generic_search");

    // Groups that already reach MIN_GROUP_SIZE are never merged.
    let other: Vec<String> = (0..MIN_GROUP_SIZE).map(|i| format!("other_{i}")).collect();
    let groups = selector.merge_small_groups(vec![
        TerminalNode {
            category_id: "cat_food".into(),
            service_ids: big,
        },
        TerminalNode {
            category_id: "cat_finance".into(),
            service_ids: other,
        },
    ]);
    assert_eq!(groups.len(), 2);
    assert!(selector.merge_small_groups(vec![]).is_empty());
    Ok(())
}
