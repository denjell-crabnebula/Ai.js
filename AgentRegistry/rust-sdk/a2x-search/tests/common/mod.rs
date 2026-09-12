// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared fixtures for integration tests.
use ap_support::testing::TestResult;
use std::path::Path;
use std::sync::Arc;

use a2x_search::taxonomy::{
    CategoryInfo, ClassFile, ServiceRecord, ServicesIndex, TaxonomyFile, index_services,
};
use a2x_search::testing::FakeLlm;
use serde_json::json;

/// A small hand-made taxonomy for search tests.
pub fn search_taxonomy() -> TestResult<(TaxonomyFile, ClassFile, Vec<ServiceRecord>)> {
    let taxonomy: TaxonomyFile = serde_json::from_value(json!({
        "version": "2.0-hierarchical",
        "root": "root",
        "build_status": "complete",
        "categories": {
            "root": {"children": ["cat_travel", "cat_finance", "cat_food"], "services": ["svc_generic_search"]},
            "cat_travel": {"children": ["cat_flights", "cat_hotels"], "services": []},
            "cat_flights": {"children": [], "services": ["svc_flight_search", "svc_flight_book"]},
            "cat_hotels": {"children": [], "services": ["svc_hotel_book"]},
            "cat_finance": {"children": [], "services": ["svc_stock_price", "svc_currency"]},
            "cat_food": {"children": [], "services": ["svc_restaurant"]}
        }
    }))?;
    let mut classes = ClassFile::default();
    let mut put = |id: &str, name: &str, desc: &str| {
        classes
            .categories
            .insert(id.to_string(), CategoryInfo::new(name, desc));
    };
    put(
        "root",
        "All API Services",
        "Root node containing all API services across all functional domains",
    );
    put("cat_travel", "Travel & Tourism", "Flights, hotels and trips");
    put("cat_flights", "Flights", "Search and book flights");
    put("cat_hotels", "Hotels", "Reserve hotel rooms");
    put("cat_finance", "Finance", "Stock and currency prices");
    put("cat_food", "Food", "Restaurants and recipes");
    let services = vec![
        ServiceRecord::new(
            "svc_flight_search",
            "Flight Search",
            "Search flights between cities",
        ),
        ServiceRecord::new("svc_flight_book", "Flight Booking", "Book flight tickets"),
        ServiceRecord::new("svc_hotel_book", "Hotel Booking", "Reserve hotel rooms"),
        ServiceRecord::new("svc_stock_price", "Stock Prices", "Get stock prices"),
        ServiceRecord::new("svc_currency", "Currency Exchange", "Convert currency rates"),
        ServiceRecord::new("svc_restaurant", "Restaurant Finder", "Find restaurants nearby"),
        ServiceRecord::new("svc_generic_search", "Universal Search", "Search anything"),
    ];
    Ok((taxonomy, classes, services))
}

pub fn search_services_index() -> TestResult<ServicesIndex> {
    Ok(index_services(search_taxonomy()?.2))
}

/// Write the search fixture as `service.json` and `taxonomy/{taxonomy,class}.json`.
pub fn write_search_dataset(dir: &Path) -> TestResult {
    let (taxonomy, classes, services) = search_taxonomy()?;
    std::fs::create_dir_all(dir.join("taxonomy"))?;
    std::fs::write(dir.join("service.json"), serde_json::to_string_pretty(&services)?)?;
    std::fs::write(
        dir.join("taxonomy").join("taxonomy.json"),
        serde_json::to_string_pretty(&taxonomy)?,
    )?;
    std::fs::write(
        dir.join("taxonomy").join("class.json"),
        serde_json::to_string_pretty(&classes)?,
    )?;
    Ok(())
}

/// Extract the `Query: ...` line from a search prompt.
fn query_of(prompt: &str) -> Option<String> {
    prompt
        .lines()
        .find_map(|l| l.strip_prefix("Query: ").map(str::to_string))
}

/// A fake LLM that answers category and service prompts by selecting the
/// numbered lines that share a word with the query.
pub fn word_overlap_llm() -> TestResult<FakeLlm> {
    Ok(FakeLlm::new().on_fn(|prompt| {
        let query = query_of(prompt)?;
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        let mut picks = Vec::new();
        for line in prompt.lines() {
            let lower = line.to_lowercase();
            let Some((num, rest)) = lower.split_once(". ") else {
                continue;
            };
            let Ok(n) = num.parse::<usize>() else { continue };
            if words.iter().any(|w| rest.contains(w.as_str())) {
                picks.push(n.to_string());
            }
        }
        Some(if picks.is_empty() {
            "NONE".to_string()
        } else {
            picks.join(",")
        })
    }))
}

pub fn shared(llm: FakeLlm) -> Arc<FakeLlm> {
    Arc::new(llm)
}
