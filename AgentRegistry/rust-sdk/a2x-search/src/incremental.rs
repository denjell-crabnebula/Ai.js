// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Incremental taxonomy updates: add or remove services in a built
//! taxonomy without a full rebuild.

use std::sync::Arc;

use a2x_common::{ChatMessage, LlmBackend, parse_json_response};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::Value;

use crate::build::prompts::fill;
use crate::search::prompts::parse_number_list;
use crate::taxonomy::{ClassFile, ROOT_ID, ServiceRecord, ServicesIndex, TaxonomyFile};
use crate::util::run_bounded;

pub const SELECT_DOMAINS_TEMPLATE: &str = r#"Which of the following top-level functional domains are relevant for this service?

SERVICE:
ID: {service_id}
Name: {service_name}
Description: {service_description}

DOMAINS:
{domains_text}

RULES:
- Select ALL domains where a user browsing that domain would benefit from discovering this service
- Most services belong to 1 domain; some cross-cutting services may belong to 2-3
- Be selective: only include domains where the service is genuinely useful

Return ONLY the numbers separated by commas (e.g. "1,3"), or "NONE" if no domain fits."#;

pub const PLACE_IN_CATEGORY_TEMPLATE: &str = r#"Place this service into the best leaf sub-category within the target domain.

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
  "target_category_id": "...",
  "confidence": 85
}}
```

If no sub-category fits (confidence < 40), set target_category_id to "{domain_id}" (place at domain root)."#;

#[derive(Clone, Debug)]
struct DomainInfo {
    name: String,
    description: String,
}

struct State {
    taxonomy: TaxonomyFile,
    class_data: ClassFile,
    services_index: ServicesIndex,
}

/// Add or remove services in an existing A2X taxonomy.
///
/// The taxonomy is owned by the builder while it runs; call
/// [`IncrementalBuilder::into_parts`] to get it back for saving.
pub struct IncrementalBuilder {
    state: Arc<Mutex<State>>,
    llm: Arc<dyn LlmBackend>,
    workers: usize,
}

impl IncrementalBuilder {
    pub fn new(
        taxonomy: TaxonomyFile,
        class_data: ClassFile,
        services_index: ServicesIndex,
        llm: Arc<dyn LlmBackend>,
        workers: usize,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                taxonomy,
                class_data,
                services_index,
            })),
            llm,
            workers,
        }
    }

    /// Current taxonomy (clone).
    pub fn taxonomy(&self) -> TaxonomyFile {
        self.state.lock().taxonomy.clone()
    }

    /// Current class data (clone).
    pub fn class_data(&self) -> ClassFile {
        self.state.lock().class_data.clone()
    }

    /// Current services index (clone).
    pub fn services_index(&self) -> ServicesIndex {
        self.state.lock().services_index.clone()
    }

    /// Take the data back out.
    pub fn into_parts(self) -> (TaxonomyFile, ClassFile, ServicesIndex) {
        let state = Arc::try_unwrap(self.state)
            .map(Mutex::into_inner)
            .unwrap_or_else(|arc| {
                let s = arc.lock();
                State {
                    taxonomy: s.taxonomy.clone(),
                    class_data: s.class_data.clone(),
                    services_index: s.services_index.clone(),
                }
            });
        (state.taxonomy, state.class_data, state.services_index)
    }

    /// Remove a service from every node and the index. Returns true when
    /// it was found.
    pub fn remove_service(&self, service_id: &str) -> bool {
        let mut found = false;
        {
            let mut state = self.state.lock();
            for node in state.taxonomy.categories.values_mut() {
                if let Some(pos) = node.services.iter().position(|s| s == service_id) {
                    node.services.remove(pos);
                    found = true;
                }
            }
            state.services_index.shift_remove(service_id);
        }
        if found {
            tracing::info!("Removed service {service_id}");
        } else {
            tracing::warn!("Service {service_id} not found in taxonomy");
        }
        found
    }

    /// Add a service, placing it in the best leaf of each relevant domain.
    /// Returns the category ids it was assigned to.
    pub async fn add_service(&self, service: ServiceRecord) -> Vec<String> {
        let service_id = if service.id.is_empty() {
            service.name.clone()
        } else {
            service.id.clone()
        };
        if service_id.is_empty() {
            tracing::warn!("Service has no id or name, skipping");
            return Vec::new();
        }
        let mut service = service;
        service.id = service_id.clone();
        self.state
            .lock()
            .services_index
            .insert(service_id.clone(), service.clone());

        let domains: IndexMap<String, DomainInfo> = {
            let state = self.state.lock();
            state
                .taxonomy
                .children(ROOT_ID)
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        DomainInfo {
                            name: state.class_data.name_of(id).to_string(),
                            description: state.class_data.description_of(id, "").to_string(),
                        },
                    )
                })
                .collect()
        };
        if domains.is_empty() {
            self.place_under_root(&service_id);
            return vec![ROOT_ID.to_string()];
        }

        let selected = self.select_domains(&service, &domains).await;
        if selected.is_empty() {
            self.place_under_root(&service_id);
            tracing::info!("Service {service_id} placed under root (no domain matched)");
            return vec![ROOT_ID.to_string()];
        }

        let assigned = self.place_in_domains(&service, &selected, &domains).await;
        if assigned.is_empty() {
            self.place_under_root(&service_id);
            return vec![ROOT_ID.to_string()];
        }

        {
            let mut state = self.state.lock();
            for cat_id in &assigned {
                if let Some(node) = state.taxonomy.categories.get_mut(cat_id) {
                    if !node.services.contains(&service_id) {
                        node.services.push(service_id.clone());
                        node.services.sort();
                    }
                }
            }
        }
        tracing::info!("Service {service_id} assigned to {assigned:?}");
        assigned
    }

    /// Add many services concurrently. Returns `service_id -> categories`.
    pub async fn add_services_batch(&self, services: Vec<ServiceRecord>) -> IndexMap<String, Vec<String>> {
        let mut results = IndexMap::new();
        let jobs: Vec<_> = services
            .into_iter()
            .map(|svc| {
                let this = self.clone_handle();
                async move {
                    let key = if svc.id.is_empty() {
                        svc.name.clone()
                    } else {
                        svc.id.clone()
                    };
                    let assigned = this.add_service(svc).await;
                    (key, assigned)
                }
            })
            .collect();
        run_bounded(self.workers, jobs, |(key, assigned)| {
            results.insert(key, assigned);
        })
        .await;
        results
    }

    fn clone_handle(&self) -> IncrementalBuilder {
        IncrementalBuilder {
            state: Arc::clone(&self.state),
            llm: Arc::clone(&self.llm),
            workers: self.workers,
        }
    }

    fn place_under_root(&self, service_id: &str) {
        let mut state = self.state.lock();
        let root = state.taxonomy.categories.entry(ROOT_ID.to_string()).or_default();
        if !root.services.iter().any(|s| s == service_id) {
            root.services.push(service_id.to_string());
            root.services.sort();
        }
    }

    // ----- internal -----

    async fn select_domains(
        &self,
        service: &ServiceRecord,
        domains: &IndexMap<String, DomainInfo>,
    ) -> Vec<String> {
        let mut domain_ids: Vec<&String> = domains.keys().collect();
        domain_ids.sort();
        let domains_text = domain_ids
            .iter()
            .enumerate()
            .map(|(i, did)| format!("{}. {}: {}", i + 1, domains[*did].name, domains[*did].description))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = fill(
            SELECT_DOMAINS_TEMPLATE,
            &[
                ("service_id", &service.id),
                ("service_name", &service.name),
                ("service_description", service.description_or("No description")),
                ("domains_text", &domains_text),
            ],
        );
        let response = self.llm.call(&[ChatMessage::user(prompt)], 0.0, Some(100)).await;
        if !response.success {
            return Vec::new();
        }
        parse_number_list(&response.content, domain_ids.len())
            .into_iter()
            .map(|idx| domain_ids[idx].clone())
            .collect()
    }

    async fn place_in_domains(
        &self,
        service: &ServiceRecord,
        domain_ids: &[String],
        domains: &IndexMap<String, DomainInfo>,
    ) -> Vec<String> {
        let mut assigned = Vec::new();
        if domain_ids.len() == 1 {
            if let Some(cat) = self
                .place_in_domain(service, &domain_ids[0], &domains[&domain_ids[0]])
                .await
            {
                assigned.push(cat);
            }
            return assigned;
        }
        let jobs: Vec<_> = domain_ids
            .iter()
            .map(|did| {
                let this = self.clone_handle();
                let svc = service.clone();
                let did = did.clone();
                let info = domains[&did].clone();
                async move { this.place_in_domain(&svc, &did, &info).await }
            })
            .collect();
        run_bounded(self.workers.min(domain_ids.len()), jobs, |cat: Option<String>| {
            if let Some(c) = cat {
                assigned.push(c);
            }
        })
        .await;
        assigned
    }

    async fn place_in_domain(
        &self,
        service: &ServiceRecord,
        domain_id: &str,
        domain_info: &DomainInfo,
    ) -> Option<String> {
        let leaf_info: Vec<(String, DomainInfo)> = {
            let state = self.state.lock();
            let leaves = state.taxonomy.leaves_under(domain_id);
            if leaves.is_empty() {
                return Some(domain_id.to_string());
            }
            let mut infos: Vec<(String, DomainInfo)> = leaves
                .into_iter()
                .map(|lid| {
                    let info = DomainInfo {
                        name: state.class_data.name_of(&lid).to_string(),
                        description: state.class_data.description_of(&lid, "").to_string(),
                    };
                    (lid, info)
                })
                .collect();
            infos.sort_by(|a, b| a.0.cmp(&b.0));
            infos
        };
        let subcategories_text = leaf_info
            .iter()
            .map(|(cid, info)| format!("- {cid}: {} — {}", info.name, info.description))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = fill(
            PLACE_IN_CATEGORY_TEMPLATE,
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
        let response = self.llm.call(&[ChatMessage::user(prompt)], 0.0, Some(300)).await;
        if !response.success {
            return None;
        }
        let result = parse_json_response(&response.content)?;
        let target = result
            .get("target_category_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let confidence = result.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
        if confidence < 40.0 || target.is_empty() {
            return Some(domain_id.to_string());
        }
        if !self.state.lock().taxonomy.categories.contains_key(&target) {
            return Some(domain_id.to_string());
        }
        Some(target)
    }
}
