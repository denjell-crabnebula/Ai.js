// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use a2x_search::build::compute_service_hash;
use a2x_search::taxonomy::{ServiceRecord, TaxonomyFile, index_services};
use a2x_search::testing::FakeLlm;
use a2x_search::{
    AutoHierarchicalConfig, BuildEvent, BuildPhase, BuildSink, IncrementalBuilder, ResumeMode,
    TaxonomyBuilder,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

fn services() -> Vec<ServiceRecord> {
    vec![
        ServiceRecord::new("t1", "Flight Search", "Search flights between cities"),
        ServiceRecord::new("t2", "Flight Booking", "Book flight tickets"),
        ServiceRecord::new("t3", "Hotel Booking", "Reserve hotel rooms"),
        ServiceRecord::new("t4", "Car Rental", "Rent a car for trips"),
        ServiceRecord::new("f1", "Stock Prices", "Get stock market prices"),
        ServiceRecord::new("f2", "Currency Exchange", "Convert currency rates"),
        ServiceRecord::new("f3", "Bitcoin Prices", "Get bitcoin prices"),
        ServiceRecord::new("f4", "Loan Calculator", "Calculate loan payments"),
        ServiceRecord::new("d1", "Restaurant Finder", "Find a restaurant nearby"),
        ServiceRecord::new("d2", "Recipe Search", "Search cooking recipe ideas"),
        ServiceRecord::new("d3", "Food Delivery", "Order food delivery"),
        ServiceRecord::new(
            "g1",
            "Universal Search",
            "Search anything about flights, stocks and food",
        ),
    ]
}

/// Keyword to category-name fragments, used by the fake classifier.
const KEYWORDS: &[(&str, &[&str])] = &[
    ("flight", &["travel", "flights"]),
    ("hotel", &["travel", "hotels"]),
    ("car ", &["travel", "cars"]),
    ("stock", &["finance", "market"]),
    ("bitcoin", &["finance", "market"]),
    ("currency", &["finance", "currency"]),
    ("loan", &["finance", "loan"]),
    ("restaurant", &["food"]),
    ("recipe", &["food"]),
    ("food", &["food"]),
];

fn root_categories_json(with_keywords: bool, third_name: &str) -> String {
    let kw = |k: &str| {
        if with_keywords {
            format!(", \"associated_keywords\": [\"{k}\"]")
        } else {
            String::new()
        }
    };
    format!(
        r#"```json
{{"dimension": "functional domain", "categories": [
  {{"id": "cat_sub1", "name": "Travel & Tourism", "description": "Trips", "boundary": "Not finance", "decision_rule": "If travel"{}}},
  {{"id": "cat_sub2", "name": "Finance & Banking", "description": "Money", "boundary": "Not travel", "decision_rule": "If money"{}}},
  {{"id": "cat_sub3", "name": "{third_name}", "description": "Meals", "boundary": "", "decision_rule": ""{}}}
]}}
```"#,
        kw("travel"),
        kw("finance"),
        kw("food")
    )
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let s = text.find(start)? + start.len();
    let e = text[s..].find(end)? + s;
    Some(&text[s..e])
}

/// The scripted LLM for build tests.
fn build_llm() -> TestResult<FakeLlm> {
    let validations = AtomicUsize::new(0);
    Ok(FakeLlm::new()
        .on_fn(|p| {
            if !p.contains("You are designing categories for a group of") {
                return None;
            }
            Some(if p.contains("PARENT CATEGORY: \"Travel & Tourism\"") {
                r#"{"categories": [{"id": "cat_sub1_sub1", "name": "Flights", "description": "Air travel", "boundary": "", "decision_rule": ""}, {"id": "cat_sub1_sub2", "name": "Hotels & Cars", "description": "Stays and rides", "boundary": "", "decision_rule": ""}]}"#.to_string()
            } else if p.contains("PARENT CATEGORY: \"Finance & Banking\"") {
                r#"{"categories": [{"id": "cat_sub2_sub1", "name": "Market Prices", "description": "Prices", "boundary": "", "decision_rule": ""}, {"id": "cat_sub2_sub2", "name": "Currency & Loans", "description": "Money", "boundary": "", "decision_rule": ""}]}"#.to_string()
            } else {
                root_categories_json(false, "Food & Dining")
            })
        })
        .on_fn(|p| {
            if !p.contains("Extract functional domain keywords") {
                return None;
            }
            let block = between(p, "SERVICES:\n", "\n\nOutput ONLY valid JSON")?;
            let mut items = Vec::new();
            for line in block.lines() {
                let id = between(line, "- [", "]")?;
                let lower = line.to_lowercase();
                let mut kws = Vec::new();
                for (k, names) in KEYWORDS {
                    if lower.contains(k) {
                        kws.push(format!("\"{}\"", names[0]));
                    }
                }
                kws.dedup();
                items.push(format!("{{\"service_id\": \"{id}\", \"keywords\": [{}]}}", kws.join(", ")));
            }
            Some(format!("{{\"extractions\": [{}]}}", items.join(", ")))
        })
        .on_fn(|p| {
            p.contains("functional domain keywords extracted from")
                .then(|| root_categories_json(true, "General Utilities"))
        })
        .on_fn(move |p| {
            if !p.contains("Review these top-level categories") {
                return None;
            }
            let n = validations.fetch_add(1, Ordering::SeqCst);
            Some(if n == 0 {
                r#"{"validations": [{"id": "cat_sub1", "valid": true}, {"id": "cat_sub2", "valid": true}, {"id": "cat_sub3", "valid": false, "violation_type": "catch_all", "reason": "Utilities is a catch-all"}]}"#.to_string()
            } else {
                r#"{"validations": [{"id": "cat_sub1", "valid": true}, {"id": "cat_sub2", "valid": true}, {"id": "cat_sub3", "valid": true}]}"#.to_string()
            })
        })
        .on_fn(|p| {
            p.contains("You designed top-level categories but some VIOLATED")
                .then(|| root_categories_json(true, "Food & Dining"))
        })
        .on_fn(|p| {
            if !p.contains("Classify this API service into the sub-categories below.") {
                return None;
            }
            let subs = between(p, "SUB-CATEGORIES:\n", "\n\nSERVICE DESCRIPTION:")?;
            let desc = between(p, "SERVICE DESCRIPTION:\n", "\n\nINSTRUCTIONS")?.to_lowercase();
            let cats: Vec<(&str, String)> = subs
                .lines()
                .filter(|l| !l.starts_with(' ') && l.contains(": "))
                .filter_map(|l| l.split_once(": ").map(|(id, name)| (id, name.to_lowercase())))
                .collect();
            let mut picked: Vec<&str> = Vec::new();
            for (k, names) in KEYWORDS {
                if desc.contains(k) {
                    for (id, name) in &cats {
                        if names.iter().any(|n| name.contains(n)) && !picked.contains(id) {
                            picked.push(id);
                        }
                    }
                }
            }
            let ids = picked.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(", ");
            Some(format!("Sure!\n```json\n{{\"reasoning\": \"kw\", \"category_ids\": [{ids}]}}\n```"))
        })
        .on_fn(|p| {
            p.contains("You defined sub-categories for").then(|| {
                r#"{"changes_summary": "no change", "subcategories": [
  {"id": "cat_sub1", "name": "Travel & Tourism", "description": "Trips", "boundary": "Not finance", "decision_rule": "If travel"},
  {"id": "cat_sub2", "name": "Finance & Banking", "description": "Money", "boundary": "Not travel", "decision_rule": "If money"},
  {"id": "cat_sub3", "name": "Food & Dining", "description": "Meals", "boundary": "", "decision_rule": ""}]}"#.to_string()
            })
        })
        .on_fn(|p| {
            if !p.contains("You are reviewing services in the category") {
                return None;
            }
            Some(if p.contains("- d3: Food Delivery") {
                r#"{"cross_assignments": [{"service_id": "d3", "target_domain_id": "cat_sub1", "reason": "travellers eat"}]}"#.to_string()
            } else {
                r#"{"cross_assignments": []}"#.to_string()
            })
        })
        .on_fn(|p| {
            if !p.contains("Place this service into the best leaf sub-category") {
                return None;
            }
            let subs = between(p, "AVAILABLE SUB-CATEGORIES:\n", "\n\nSelect the single")?;
            let first = subs.lines().last()?.strip_prefix("- ")?.split(':').next()?;
            Some(format!(r#"{{"service_id": "d3", "target_category_id": "{first}", "confidence": 85}}"#))
        }))
}

fn write_services(dir: &Path) -> TestResult<std::path::PathBuf> {
    let ds = dir.join("DS");
    std::fs::create_dir_all(&ds)?;
    let path = ds.join("service.json");
    std::fs::write(&path, serde_json::to_string_pretty(&services())?)?;
    Ok(path)
}

fn config(dir: &Path) -> TestResult<AutoHierarchicalConfig> {
    let service_path = write_services(dir)?;
    let mut c = AutoHierarchicalConfig::new(&service_path);
    assert_eq!(c.dataset_name(), "DS");
    c.output_dir = dir.join("DS").join("taxonomy");
    c.max_service_size = 3;
    c.max_depth = Some(2);
    c.delete_threshold = 1;
    c.workers = 4;
    Ok(c)
}

fn collecting_sink() -> (BuildSink, Arc<Mutex<Vec<BuildEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    (BuildSink::new(move |e| e2.lock().push(e)), events)
}

#[tokio::test]
async fn full_build_writes_taxonomy_files() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = config(dir.path())?;
    let llm = Arc::new(build_llm()?);
    let (sink, events) = collecting_sink();
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    let outcome = builder
        .build(ResumeMode::No, sink, CancellationToken::new())
        .await?;
    assert!(!outcome.skipped);
    assert_eq!(outcome.nodes_split, 3);

    let out = &config.output_dir;
    let taxonomy = TaxonomyFile::load(&out.join("taxonomy.json"))?;
    assert_eq!(taxonomy.build_status.as_deref(), Some("complete"));
    assert_eq!(taxonomy.version, "2.0-hierarchical");
    assert_eq!(taxonomy.children("root"), ["cat_sub1", "cat_sub2", "cat_sub3"]);
    assert_eq!(taxonomy.services("root"), ["g1"]);
    assert_eq!(taxonomy.children("cat_sub1"), ["cat_sub1_sub1", "cat_sub1_sub2"]);
    assert_eq!(taxonomy.services("cat_sub1_sub1"), ["t1", "t2"]);
    // d3 was cross-listed into the first leaf of the travel domain
    assert_eq!(taxonomy.services("cat_sub1_sub2"), ["d3", "t3", "t4"]);
    assert_eq!(taxonomy.children("cat_sub2"), ["cat_sub2_sub1", "cat_sub2_sub2"]);
    assert_eq!(taxonomy.services("cat_sub2_sub1"), ["f1", "f3"]);
    assert_eq!(taxonomy.services("cat_sub2_sub2"), ["f2", "f4"]);
    assert!(taxonomy.children("cat_sub3").is_empty());
    assert_eq!(taxonomy.services("cat_sub3"), ["d1", "d2", "d3"]);

    let class: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("class.json"))?)?;
    assert_eq!(class["version"], "2.0-hierarchical");
    assert_eq!(class["categories"]["root"]["name"], "All API Services");
    assert_eq!(class["categories"]["cat_sub1"]["name"], "Travel & Tourism");
    assert_eq!(class["categories"]["cat_sub1"]["boundary"], "Not finance");
    assert_eq!(class["categories"]["cat_sub1"]["decision_rule"], "If travel");
    assert!(class["categories"]["cat_sub3"].get("boundary").is_none());
    assert!(
        class["categories"]["cat_sub1_sub1"]
            .get("associated_keywords")
            .is_none()
    );

    let assignments: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("assignments.json"))?)?;
    assert_eq!(
        assignments["t1"]["category_ids"],
        serde_json::json!(["cat_sub1_sub1"])
    );
    assert_eq!(assignments["t1"]["reasoning"], "kw");
    assert_eq!(assignments["g1"]["category_ids"], serde_json::json!([]));

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("build_config.json"))?)?;
    assert_eq!(saved["max_service_size"], 3);
    assert_eq!(
        saved["service_hash"],
        serde_json::json!(compute_service_hash(&services()))
    );
    assert_eq!(
        saved["service_path"],
        serde_json::json!(config.service_path.to_string_lossy())
    );
    assert!(!out.join("keywords.json").exists());

    let s = &outcome.summary;
    assert_eq!(s.total_categories, 7);
    assert_eq!(s.leaf_nodes, 5);
    assert_eq!(s.max_depth, 2);
    assert_eq!(s.depth_distribution, vec![(1, 3), (2, 4)]);
    assert_eq!(s.total_service_assignments, 13);

    // refinement ran for the root (generic g1), not for converged children
    assert_eq!(
        llm.count_prompts_containing("You defined sub-categories for \"All API Services\""),
        2
    );
    assert_eq!(
        llm.count_prompts_containing("You defined sub-categories for \"Travel & Tourism\""),
        0
    );
    assert_eq!(
        llm.count_prompts_containing("Classify this API service"),
        12 * 3 + 4 + 4
    );
    assert_eq!(
        llm.count_prompts_containing("You are reviewing services in the category"),
        5
    );

    let events = events.lock();
    let phases: Vec<BuildPhase> = events
        .iter()
        .filter_map(|e| match e {
            BuildEvent::Phase { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        vec![BuildPhase::Bfs, BuildPhase::CrossDomain, BuildPhase::Complete]
    );
    let progress = events
        .iter()
        .find(|e| matches!(e, BuildEvent::Progress { .. }))
        .required()?;
    let msg = progress.message();
    assert!(msg.contains("% [") && msg.ends_with(" assigned"), "{msg}");
    assert!(msg.starts_with('█') || msg.starts_with('░'));
    assert!(events.iter().any(|e| {
        e.message()
            .starts_with("SPLITTING [1, 0 queued]: root (All API Services) - 12 services (depth=0)")
    }));
    assert!(
        events
            .iter()
            .any(|e| e.message() == "Cross-domain: added 1 service-category links for 1 services")
    );
    assert!(
        events
            .iter()
            .any(|e| e.message() == "Node root converged at iteration 3")
    );
    assert!(!events.iter().any(|e| e.message().contains("Deleting")));
    Ok(())
}

#[tokio::test]
async fn smart_resume_skips_resumes_and_rebuilds() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path())?;
    config.max_service_size = 4;
    config.max_depth = Some(1);
    let llm = Arc::new(build_llm()?);

    // Cancel as soon as the first classification prompt appears: the root
    // split still completes and is checkpointed, then the build stops.
    let cancel = CancellationToken::new();
    let cancelling = Arc::new(build_llm()?.observe({
        let c = cancel.clone();
        move |p| {
            if p.contains("Classify this API service") {
                c.cancel();
            }
        }
    }));
    let mut builder = TaxonomyBuilder::new(config.clone(), cancelling);
    let err = builder
        .build(ResumeMode::No, BuildSink::null(), cancel)
        .await
        .err_or_fail()?;
    assert!(err.is_cancelled());
    let partial = TaxonomyFile::load(&config.output_dir.join("taxonomy.json"))?;
    assert_eq!(partial.build_status.as_deref(), Some("bfs"));
    assert_eq!(partial.children("root").len(), 3);

    // Resume: no design prompt is needed, BFS is already done.
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    let outcome = builder
        .build(ResumeMode::Yes, BuildSink::null(), CancellationToken::new())
        .await?;
    assert!(!outcome.skipped);
    assert_eq!(outcome.nodes_split, 0);
    assert_eq!(outcome.taxonomy.build_status.as_deref(), Some("complete"));
    assert_eq!(llm.count_prompts_containing("You are designing categories"), 0);
    assert_eq!(llm.count_prompts_containing("You are reviewing services"), 3);
    // cross-domain placed d3 at the travel domain root (it is a leaf domain)
    assert_eq!(
        outcome.taxonomy.services("cat_sub1"),
        ["d3", "t1", "t2", "t3", "t4"]
    );

    // Complete + same config: skipped.
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    let outcome = builder
        .build(ResumeMode::Yes, BuildSink::null(), CancellationToken::new())
        .await?;
    assert!(outcome.skipped);
    assert_eq!(outcome.taxonomy.categories.len(), 4);
    assert_eq!(llm.count_prompts_containing("You are designing categories"), 0);

    // Config change: full rebuild.
    let mut changed = config.clone();
    changed.generic_ratio = 0.5;
    let mut builder = TaxonomyBuilder::new(changed, llm.clone());
    let outcome = builder
        .build(ResumeMode::Yes, BuildSink::null(), CancellationToken::new())
        .await?;
    assert!(!outcome.skipped);
    assert_eq!(outcome.nodes_split, 1);
    assert_eq!(llm.count_prompts_containing("You are designing categories"), 1);
    Ok(())
}

#[tokio::test]
async fn keyword_based_root_with_validation_and_keyword_reuse() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path())?;
    config.keyword_threshold = 0;
    config.keyword_batch_size = 5;
    config.max_service_size = 4;
    config.enable_cross_domain = false;
    let llm = Arc::new(build_llm()?);
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    let outcome = builder
        .build(ResumeMode::No, BuildSink::null(), CancellationToken::new())
        .await?;
    assert_eq!(outcome.nodes_split, 1);
    assert_eq!(
        llm.count_prompts_containing("Extract functional domain keywords"),
        3
    );
    assert_eq!(
        llm.count_prompts_containing("Review these top-level categories"),
        2
    );
    assert_eq!(
        llm.count_prompts_containing("some VIOLATED the functional-domain-only rule"),
        1
    );
    let redesign = llm
        .prompts()
        .into_iter()
        .find(|p| p.contains("some VIOLATED"))
        .required()?;
    assert!(redesign.contains("KEYWORDS FROM VIOLATED CATEGORIES:\n- food: 4 services"));
    assert_eq!(outcome.class_data.name_of("cat_sub3"), "Food & Dining");
    let second_batch = llm
        .prompts()
        .into_iter()
        .filter(|p| p.contains("Extract functional domain keywords"))
        .nth(1)
        .required()?;
    // root services are sorted by id, so the first batch is d1..d3, f1, f2
    assert!(
        second_batch.contains("EXISTING KEYWORDS (reuse when applicable):\n- food (count: 3)\n- finance (count: 2)\n\nSERVICES:\n- [f3]"),
        "{second_batch}"
    );

    let keywords: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.output_dir.join("keywords.json"))?)?;
    assert_eq!(keywords["travel"], 5);
    assert_eq!(keywords["finance"], 5);
    assert_eq!(keywords["food"], 4);
    let keys: Vec<&String> = keywords.as_object().required()?.keys().collect();
    // sorted by count, stable for ties (finance was seen before travel)
    assert_eq!(keys, vec!["finance", "travel", "food"]);

    // Rebuild reusing the cached keywords: no extraction calls.
    let before = llm.calls();
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    builder
        .build(ResumeMode::Keyword, BuildSink::null(), CancellationToken::new())
        .await?;
    assert!(llm.calls() > before);
    assert_eq!(
        llm.count_prompts_containing("Extract functional domain keywords"),
        3
    );
    assert!(config.output_dir.join("keywords.json").exists());

    // A plain rebuild deletes keywords.json and extracts again.
    let mut builder = TaxonomyBuilder::new(config.clone(), llm.clone());
    builder
        .build(ResumeMode::No, BuildSink::null(), CancellationToken::new())
        .await?;
    assert_eq!(
        llm.count_prompts_containing("Extract functional domain keywords"),
        6
    );
    Ok(())
}

#[tokio::test]
async fn design_failure_aborts_build() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = config(dir.path())?;
    let llm = Arc::new(FakeLlm::new().fail_on("You are designing categories", "provider down"));
    let mut builder = TaxonomyBuilder::new(config, llm);
    let err = builder
        .build(ResumeMode::No, BuildSink::null(), CancellationToken::new())
        .await
        .err_or_fail()?;
    assert!(
        err.to_string()
            .contains("Category design from descriptions failed: provider down")
    );
    Ok(())
}

#[tokio::test]
async fn incremental_add_and_remove() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path())?;
    config.max_service_size = 4;
    config.max_depth = Some(1);
    config.enable_cross_domain = false;
    let mut builder = TaxonomyBuilder::new(config.clone(), Arc::new(build_llm()?));
    let outcome = builder
        .build(ResumeMode::No, BuildSink::null(), CancellationToken::new())
        .await?;

    let llm = Arc::new(
        FakeLlm::new()
            .on("Which of the following top-level functional domains", "1, 3")
            .on_fn(|p| {
                if !p.contains("Place this service into the best leaf sub-category") {
                    return None;
                }
                Some(if p.contains("TARGET DOMAIN: Travel & Tourism") {
                    r#"{"target_category_id": "cat_sub1", "confidence": 90}"#.to_string()
                } else {
                    r#"{"target_category_id": "bogus", "confidence": 90}"#.to_string()
                })
            }),
    );
    let inc = IncrementalBuilder::new(
        outcome.taxonomy.clone(),
        outcome.class_data.clone(),
        index_services(services()),
        llm.clone(),
        4,
    );
    let mut assigned = inc
        .add_service(ServiceRecord::new("n1", "Train Tickets", "Book train tickets"))
        .await;
    assigned.sort();
    // travel leaf accepted; the bogus target for food falls back to the domain root
    assert_eq!(assigned, vec!["cat_sub1", "cat_sub3"]);
    let t = inc.taxonomy();
    assert!(t.services("cat_sub1").contains(&"n1".to_string()));
    assert!(t.services("cat_sub3").contains(&"n1".to_string()));
    let domain_prompt = llm
        .prompts()
        .into_iter()
        .find(|p| p.contains("DOMAINS:\n1. Travel & Tourism: Trips"))
        .required()?;
    assert!(domain_prompt.contains("2. Finance & Banking: Money\n3. Food & Dining: Meals"));

    let batch = inc
        .add_services_batch(vec![
            ServiceRecord::new("n2", "Bus Tickets", "Book bus tickets"),
            ServiceRecord::new("", "Named Only", "no id"),
        ])
        .await;
    assert_eq!(batch["n2"].len(), 2);
    assert_eq!(batch["Named Only"].len(), 2);

    assert!(inc.remove_service("n1"));
    assert!(!inc.remove_service("n1"));
    let (t, _, idx) = inc.into_parts();
    assert!(!t.services("cat_sub1").contains(&"n1".to_string()));
    assert!(!idx.contains_key("n1"));
    assert!(idx.contains_key("n2"));

    // Flat taxonomy: everything lands under root without LLM calls.
    let flat: TaxonomyFile = serde_json::from_value(
        serde_json::json!({"categories": {"root": {"children": [], "services": []}}}),
    )?;
    let no_llm = Arc::new(FakeLlm::new());
    let inc = IncrementalBuilder::new(flat, Default::default(), Default::default(), no_llm.clone(), 2);
    assert_eq!(
        inc.add_service(ServiceRecord::new("x", "X", "x")).await,
        vec!["root"]
    );
    assert_eq!(no_llm.calls(), 0);
    assert_eq!(inc.taxonomy().services("root"), ["x"]);

    // NONE from the domain selector also places under root.
    let none_llm = Arc::new(FakeLlm::new());
    let inc = IncrementalBuilder::new(
        outcome.taxonomy.clone(),
        outcome.class_data.clone(),
        Default::default(),
        none_llm,
        2,
    );
    assert_eq!(
        inc.add_service(ServiceRecord::new("y", "Y", "y")).await,
        vec!["root"]
    );
    assert!(inc.taxonomy().services("root").contains(&"y".to_string()));
    Ok(())
}
