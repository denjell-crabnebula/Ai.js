// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Cross-domain multi-parent assignment.
//!
//! After the hierarchical build, identifies services that should also be
//! discoverable from other top-level domains and links them to the best
//! leaf of that domain.

use std::sync::Arc;

use a2x_common::{ChatMessage, LlmBackend, parse_json_response};
use indexmap::IndexMap;
use serde_json::Value;

use super::progress::BuildSink;
use super::prompts::fill;
use crate::taxonomy::{ClassFile, ROOT_ID, ServiceRecord, ServicesIndex, TaxonomyFile};
use crate::util::{run_bounded, truncate_chars};

pub const SYSTEM_CROSS_DOMAIN: &str = "You are an expert at API service classification. You identify services that should be discoverable from multiple functional domains.";

pub const IDENTIFY_CROSS_DOMAIN_TEMPLATE: &str = r#"You are reviewing services in the category "{source_category_name}" ({source_category_desc}).

These services currently belong to this category. Some of them may ALSO be relevant to users browsing OTHER top-level domains.

TOP-LEVEL DOMAINS (excluding current):
{other_domains_text}

SERVICES IN THIS CATEGORY:
{services_text}

For each service, decide if it should ALSO appear in one of the other top-level domains listed above.
A service should be cross-listed if a user looking for that type of functionality would reasonably browse the other domain.

RULES:
- Only suggest cross-domain assignments that are genuinely useful for discoverability
- Maximum 1 additional domain per service
- Do NOT suggest cross-listing for services that clearly belong to only one domain
- Be selective: typically 10-30% of services benefit from cross-listing

Output JSON:
```json
{{
  "cross_assignments": [
    {{
      "service_id": "...",
      "target_domain_id": "...",
      "reason": "brief explanation"
    }}
  ]
}}
```

If no services need cross-listing, return: {{"cross_assignments": []}}"#;

pub const PLACE_IN_DOMAIN_TEMPLATE: &str = r#"Place this service into the best leaf sub-category within the target domain.

SERVICE:
ID: {service_id}
Name: {service_name}
Description: {service_description}

TARGET DOMAIN: {domain_name} ({domain_desc})

AVAILABLE SUB-CATEGORIES:
{subcategories_text}

Select the single best-fit sub-category. Output JSON:
```json
{{
  "service_id": "{service_id}",
  "target_category_id": "...",
  "confidence": 85
}}
```

If no sub-category fits (confidence < 40), set target_category_id to "{domain_id}" (place at domain root)."#;

/// Top-level domain metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DomainInfo {
    pub name: String,
    pub description: String,
}

/// A Phase 1 candidate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossCandidate {
    pub service_id: String,
    pub target_domain_id: String,
    pub reason: String,
}

/// `service_id -> additional category ids`.
pub type Additions = IndexMap<String, Vec<String>>;

/// Identifies and assigns cross-domain multi-parent relationships.
pub struct CrossDomainAssigner<'a> {
    llm: Arc<dyn LlmBackend>,
    workers: usize,
    sink: &'a BuildSink,
}

impl<'a> CrossDomainAssigner<'a> {
    pub fn new(llm: Arc<dyn LlmBackend>, workers: usize, sink: &'a BuildSink) -> Self {
        Self { llm, workers, sink }
    }

    /// Run cross-domain assignment. Returns extra category ids per service.
    pub async fn assign(
        &self,
        taxonomy: &TaxonomyFile,
        class_data: &ClassFile,
        services_index: &ServicesIndex,
    ) -> Additions {
        self.sink.log("CROSS-DOMAIN MULTI-PARENT ASSIGNMENT");
        let root_children = taxonomy.children(ROOT_ID);
        if root_children.len() < 2 {
            self.sink
                .log("Only 1 top-level domain, skipping cross-domain assignment");
            return Additions::new();
        }

        let domains: IndexMap<String, DomainInfo> = root_children
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    DomainInfo {
                        name: class_data.name_of(id).to_string(),
                        description: class_data.description_of(id, "").to_string(),
                    },
                )
            })
            .collect();

        let candidates = self
            .phase1_identify(taxonomy, class_data, services_index, &domains)
            .await;
        if candidates.is_empty() {
            self.sink.log("No cross-domain candidates identified");
            return Additions::new();
        }
        self.sink.log(format!(
            "Phase 1 complete: {} cross-domain candidates",
            candidates.len()
        ));

        let additions = self
            .phase2_place(&candidates, taxonomy, class_data, services_index, &domains)
            .await;
        self.sink.log(format!(
            "Phase 2 complete: {} services assigned to additional categories",
            additions.len()
        ));
        additions
    }

    async fn phase1_identify(
        &self,
        taxonomy: &TaxonomyFile,
        class_data: &ClassFile,
        services_index: &ServicesIndex,
        domains: &IndexMap<String, DomainInfo>,
    ) -> Vec<CrossCandidate> {
        let mut domain_ids: Vec<&String> = domains.keys().collect();
        domain_ids.sort();
        let mut leaf_tasks: Vec<(String, String, Vec<String>)> = Vec::new();
        for domain_id in domain_ids {
            for leaf_id in taxonomy.leaves_under(domain_id) {
                let service_ids = taxonomy.services(&leaf_id);
                if service_ids.is_empty() {
                    continue;
                }
                leaf_tasks.push((leaf_id, domain_id.clone(), service_ids.to_vec()));
            }
        }
        self.sink.log(format!(
            "Phase 1: scanning {} leaf categories for cross-domain candidates",
            leaf_tasks.len()
        ));

        let mut jobs = Vec::new();
        for (leaf_id, domain_id, service_ids) in leaf_tasks {
            let services: Vec<ServiceRecord> = service_ids
                .iter()
                .filter_map(|sid| services_index.get(sid).cloned())
                .collect();
            if services.is_empty() {
                continue;
            }
            let leaf_name = class_data.name_of(&leaf_id).to_string();
            let leaf_desc = class_data.description_of(&leaf_id, "").to_string();
            let other_domains: Vec<(String, DomainInfo)> = domains
                .iter()
                .filter(|(did, _)| **did != domain_id)
                .map(|(did, info)| (did.clone(), info.clone()))
                .collect();
            let llm = Arc::clone(&self.llm);
            jobs.push(async move {
                identify_for_category(llm, leaf_name, leaf_desc, services, other_domains).await
            });
        }

        let total = jobs.len();
        let mut all_candidates = Vec::new();
        let mut completed = 0usize;
        run_bounded(self.workers, jobs, |result: Vec<CrossCandidate>| {
            completed += 1;
            all_candidates.extend(result);
            self.sink.progress(
                completed,
                total,
                "",
                &format!("{} candidates", all_candidates.len()),
            );
        })
        .await;
        all_candidates
    }

    async fn phase2_place(
        &self,
        candidates: &[CrossCandidate],
        taxonomy: &TaxonomyFile,
        class_data: &ClassFile,
        services_index: &ServicesIndex,
        domains: &IndexMap<String, DomainInfo>,
    ) -> Additions {
        let mut by_domain: IndexMap<String, Vec<&CrossCandidate>> = IndexMap::new();
        for cand in candidates {
            if !cand.target_domain_id.is_empty() && domains.contains_key(&cand.target_domain_id) {
                by_domain
                    .entry(cand.target_domain_id.clone())
                    .or_default()
                    .push(cand);
            }
        }
        self.sink.log(format!(
            "Phase 2: placing services in {} target domains",
            by_domain.len()
        ));

        let mut domain_leaves: IndexMap<String, IndexMap<String, DomainInfo>> = IndexMap::new();
        for domain_id in by_domain.keys() {
            let mut leaf_info: IndexMap<String, DomainInfo> = IndexMap::new();
            for lid in taxonomy.leaves_under(domain_id) {
                leaf_info.insert(
                    lid.clone(),
                    DomainInfo {
                        name: class_data.name_of(&lid).to_string(),
                        description: class_data.description_of(&lid, "").to_string(),
                    },
                );
            }
            if leaf_info.is_empty() {
                leaf_info.insert(domain_id.clone(), domains[domain_id].clone());
            }
            domain_leaves.insert(domain_id.clone(), leaf_info);
        }

        let mut jobs = Vec::new();
        for (domain_id, cands) in &by_domain {
            let leaves = domain_leaves[domain_id].clone();
            let domain_info = domains[domain_id].clone();
            for cand in cands {
                let Some(svc) = services_index.get(&cand.service_id).cloned() else {
                    continue;
                };
                let llm = Arc::clone(&self.llm);
                let domain_id = domain_id.clone();
                let domain_info = domain_info.clone();
                let leaves = leaves.clone();
                jobs.push(async move {
                    let sid = svc.id.clone();
                    (
                        sid,
                        place_in_domain(llm, &svc, &domain_id, &domain_info, &leaves).await,
                    )
                });
            }
        }

        let total = jobs.len();
        let mut additions = Additions::new();
        let mut completed = 0usize;
        run_bounded(self.workers, jobs, |(svc_id, target): (String, String)| {
            completed += 1;
            if !target.is_empty() {
                additions.entry(svc_id).or_default().push(target);
            }
            let n_placed: usize = additions.values().map(Vec::len).sum();
            self.sink
                .progress(completed, total, "", &format!("{n_placed} placed"));
        })
        .await;
        additions
    }
}

async fn identify_for_category(
    llm: Arc<dyn LlmBackend>,
    leaf_name: String,
    leaf_desc: String,
    services: Vec<ServiceRecord>,
    other_domains: Vec<(String, DomainInfo)>,
) -> Vec<CrossCandidate> {
    let services_text = services
        .iter()
        .map(|svc| {
            let desc = truncate_chars(svc.description_or("No description"), 150);
            format!("- {}: {} — {desc}", svc.id, svc.name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut sorted_domains = other_domains;
    sorted_domains.sort_by(|a, b| a.0.cmp(&b.0));
    let other_domains_text = sorted_domains
        .iter()
        .map(|(did, info)| format!("- {did}: {} — {}", info.name, info.description))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = fill(
        IDENTIFY_CROSS_DOMAIN_TEMPLATE,
        &[
            ("source_category_name", &leaf_name),
            ("source_category_desc", &leaf_desc),
            ("other_domains_text", &other_domains_text),
            ("services_text", &services_text),
        ],
    );
    let response = llm
        .call(
            &[
                ChatMessage::system(SYSTEM_CROSS_DOMAIN),
                ChatMessage::user(prompt),
            ],
            0.1,
            Some(2000),
        )
        .await;
    if !response.success {
        return Vec::new();
    }
    let Some(result) = parse_json_response(&response.content) else {
        return Vec::new();
    };
    result
        .get("cross_assignments")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| CrossCandidate {
                    service_id: str_field(c, "service_id"),
                    target_domain_id: str_field(c, "target_domain_id"),
                    reason: str_field(c, "reason"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Place a service in the best leaf of the target domain. Returns `""`
/// when the LLM fails or confidence is below 40.
async fn place_in_domain(
    llm: Arc<dyn LlmBackend>,
    service: &ServiceRecord,
    domain_id: &str,
    domain_info: &DomainInfo,
    leaves: &IndexMap<String, DomainInfo>,
) -> String {
    let mut items: Vec<_> = leaves.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    let subcategories_text = items
        .iter()
        .map(|(cat_id, info)| format!("- {cat_id}: {} — {}", info.name, info.description))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = fill(
        PLACE_IN_DOMAIN_TEMPLATE,
        &[
            ("service_id", &service.id),
            ("service_name", &service.name),
            ("service_description", service.description_or("No description")),
            ("domain_name", &domain_info.name),
            ("domain_desc", &domain_info.description),
            ("subcategories_text", &subcategories_text),
            ("domain_id", domain_id),
        ],
    );
    let response = llm
        .call(
            &[
                ChatMessage::system(SYSTEM_CROSS_DOMAIN),
                ChatMessage::user(prompt),
            ],
            0.0,
            Some(300),
        )
        .await;
    if !response.success {
        return String::new();
    }
    let Some(result) = parse_json_response(&response.content) else {
        return String::new();
    };
    let target_id = str_field(&result, "target_category_id");
    let confidence = result.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
    if confidence < 40.0 {
        return String::new();
    }
    target_id
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}
