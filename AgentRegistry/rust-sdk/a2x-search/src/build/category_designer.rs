// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Category design and refinement for taxonomy nodes.
//!
//! - `design_categories`: initial subcategory scheme, keyword based for
//!   large nodes and description based for small nodes
//! - `refine_categories`: adjust subcategories from classification feedback
//! - root categories are validated and repaired by the LLM

use std::collections::HashSet;
use std::path::PathBuf;

use a2x_common::{ChatMessage, LlmBackend, parse_json_response};
use indexmap::IndexMap;
use serde_json::Value;

use super::config::AutoHierarchicalConfig;
use super::keyword_extractor::{KeywordCounts, KeywordExtractor};
use super::progress::BuildSink;
use super::prompts::{
    Assignments, CATEGORY_DESIGN_TEMPLATE, ClassificationStats, DESIGN_FROM_DESCRIPTIONS_TEMPLATE, NodeInfo,
    REDESIGN_VIOLATED_CATEGORIES_TEMPLATE, REFINE_SUBCATEGORIES_TEMPLATE, SYSTEM_CATEGORY_DESIGN,
    SYSTEM_DESIGN_FROM_DESCRIPTIONS, SYSTEM_REFINE_NODE, SYSTEM_VALIDATE_ROOT_CATEGORIES, Subcategories,
    SubcategoryDef, VALIDATE_ROOT_CATEGORIES_TEMPLATE, fill, format_categories_for_prompt,
    format_keywords_for_design, format_node_context_for_design, format_problem_services,
    format_services_for_prompt, format_subcategory_stats, generic_threshold,
};
use crate::error::{Error, Result};
use crate::taxonomy::ServiceRecord;
use crate::util::{read_json, write_json};

/// Designs and refines subcategories for one node.
pub struct CategoryDesigner<'a> {
    llm: &'a dyn LlmBackend,
    config: &'a AutoHierarchicalConfig,
    sink: &'a BuildSink,
}

impl<'a> CategoryDesigner<'a> {
    pub fn new(llm: &'a dyn LlmBackend, config: &'a AutoHierarchicalConfig, sink: &'a BuildSink) -> Self {
        Self { llm, config, sink }
    }

    fn keywords_path(&self) -> PathBuf {
        self.config.output_dir.join("keywords.json")
    }

    /// Design initial categories for a node.
    ///
    /// Nodes with more than `keyword_threshold` services go through keyword
    /// extraction; smaller nodes are designed from descriptions. Root
    /// keywords are cached in `keywords.json`.
    pub async fn design_categories(
        &self,
        services: &[ServiceRecord],
        node_info: Option<&NodeInfo>,
        is_root: bool,
        max_categories: Option<usize>,
    ) -> Result<Subcategories> {
        let max_categories = max_categories.unwrap_or_else(|| self.config.get_max_categories());
        let parent_id = if is_root {
            "cat".to_string()
        } else {
            node_info
                .map(|n| n.id.clone())
                .unwrap_or_else(|| "sub".to_string())
        };
        let design_node_info = if is_root { None } else { node_info };

        let n_services = services.len();
        let threshold = self.config.keyword_threshold;
        let strategy = if n_services > threshold {
            "keyword-based"
        } else {
            "description-based"
        };
        let node_label = design_node_info
            .map(|n| format!(", node={}", n.name_or("Unknown")))
            .unwrap_or_default();
        self.sink.log(format!(
            "Category Design: {n_services} services, threshold={threshold}, strategy={strategy}{node_label}"
        ));

        if n_services > threshold {
            self.design_from_keywords(services, design_node_info, &parent_id, max_categories, is_root)
                .await
        } else {
            self.design_from_descriptions(services, design_node_info, &parent_id, max_categories)
                .await
        }
    }

    async fn design_from_keywords(
        &self,
        services: &[ServiceRecord],
        node_info: Option<&NodeInfo>,
        parent_id: &str,
        max_cats: usize,
        is_root: bool,
    ) -> Result<Subcategories> {
        // keywords.json is a root-only cache. Its presence depends on the
        // resume mode: "no" deletes it, "keyword" and "yes" keep it.
        let mut keywords: Option<KeywordCounts> = None;
        if is_root {
            keywords = self.load_cached_keywords();
        }
        let keywords = match keywords {
            Some(k) => k,
            None => {
                let extractor = KeywordExtractor::new(self.llm, self.config, self.sink);
                let k = extractor.extract(services, node_info).await;
                if is_root {
                    self.save_keywords(&k)?;
                }
                k
            }
        };

        let categories = self
            .call_category_design(&keywords, node_info, parent_id, max_cats, services.len())
            .await?;
        self.print_categories(&categories, "keyword-based");

        if is_root {
            return Ok(self
                .validate_and_fix_root_categories(categories, &keywords, 2)
                .await);
        }
        Ok(categories)
    }

    async fn call_category_design(
        &self,
        keywords: &KeywordCounts,
        node_info: Option<&NodeInfo>,
        parent_id: &str,
        max_cats: usize,
        num_services: usize,
    ) -> Result<Subcategories> {
        let prompt = fill(
            CATEGORY_DESIGN_TEMPLATE,
            &[
                ("n_keywords", &keywords.len().to_string()),
                ("n_services", &num_services.to_string()),
                ("keywords_text", &format_keywords_for_design(keywords)),
                ("node_context_section", &format_node_context_for_design(node_info)),
                ("max_cats", &max_cats.to_string()),
                ("parent_id", parent_id),
            ],
        );
        let response = self
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_CATEGORY_DESIGN),
                    ChatMessage::user(prompt),
                ],
                self.config.temperature_design,
                Some(self.config.max_tokens_design),
            )
            .await;
        if !response.success {
            return Err(Error::Llm(format!(
                "Category design from keywords failed: {}",
                response.error.as_deref().unwrap_or("unknown")
            )));
        }
        let result = parse_json_response(&response.content)
            .ok_or_else(|| Error::Llm("Failed to parse keyword-based category design response".into()))?;
        parse_categories(&result)
    }

    async fn design_from_descriptions(
        &self,
        services: &[ServiceRecord],
        node_info: Option<&NodeInfo>,
        parent_id: &str,
        max_cats: usize,
    ) -> Result<Subcategories> {
        let prompt = fill(
            DESIGN_FROM_DESCRIPTIONS_TEMPLATE,
            &[
                ("service_count", &services.len().to_string()),
                ("node_context_section", &format_node_context_for_design(node_info)),
                ("max_cats", &max_cats.to_string()),
                ("parent_id", parent_id),
                ("services_text", &format_services_for_prompt(services, 150)),
            ],
        );
        let response = self
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_DESIGN_FROM_DESCRIPTIONS),
                    ChatMessage::user(prompt),
                ],
                self.config.temperature_design,
                Some(self.config.max_tokens_design_small),
            )
            .await;
        if !response.success {
            return Err(Error::Llm(format!(
                "Category design from descriptions failed: {}",
                response.error.as_deref().unwrap_or("unknown")
            )));
        }
        let result = parse_json_response(&response.content)
            .ok_or_else(|| Error::Llm("Failed to parse description-based category design response".into()))?;
        let categories = parse_categories(&result)?;
        self.print_categories(&categories, "description-based");
        Ok(categories)
    }

    /// Load root keywords from `{output_dir}/keywords.json` when present.
    pub fn load_cached_keywords(&self) -> Option<KeywordCounts> {
        let path = self.keywords_path();
        if !path.exists() {
            self.sink
                .log(format!("No cached keywords found at {}", path.display()));
            return None;
        }
        match read_json::<KeywordCounts>(&path) {
            Ok(k) => {
                self.sink.log(format!(
                    "Loaded {} keywords from cache: {}",
                    k.len(),
                    path.display()
                ));
                Some(k)
            }
            Err(e) => {
                self.sink.warn(format!("Failed to load cached keywords: {e}"));
                None
            }
        }
    }

    fn save_keywords(&self, keywords: &KeywordCounts) -> Result<()> {
        let path = self.keywords_path();
        write_json(&path, keywords)?;
        self.sink
            .log(format!("Saved {} keywords to {}", keywords.len(), path.display()));
        Ok(())
    }

    fn print_categories(&self, categories: &Subcategories, strategy: &str) {
        let mut lines = vec![format!("Designed {} categories ({strategy}):", categories.len())];
        let mut items: Vec<_> = categories.iter().collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        for (id, info) in items {
            lines.push(format!("  {id}: {}", info.name));
        }
        self.sink.log(lines.join("\n"));
    }

    // =========================================================================
    // Refinement
    // =========================================================================

    /// Refine subcategories from classification feedback. Returns the
    /// current subcategories unchanged when the LLM call or parse fails.
    pub async fn refine_categories(
        &self,
        parent_info: &NodeInfo,
        subcategories: &Subcategories,
        assignments: &Assignments,
        services: &[ServiceRecord],
        stats: &ClassificationStats,
        is_root: bool,
    ) -> Subcategories {
        let max_cats = self.config.get_max_categories();
        let max_tokens = if is_root {
            self.config.max_tokens_design
        } else {
            self.config.max_tokens_design_small
        };
        let n_subcats = subcategories.len();
        let threshold = generic_threshold(n_subcats, self.config.generic_ratio);

        let mut tiny_cats: Vec<(&String, usize)> = subcategories
            .keys()
            .map(|id| (id, stats.cat_counts.get(id).copied().unwrap_or(0)))
            .filter(|(_, count)| *count <= self.config.min_leaf_size)
            .collect();
        tiny_cats.sort();
        let tiny_cats_text = if tiny_cats.is_empty() {
            String::new()
        } else {
            let mut lines = vec![format!(
                "\nTINY SUB-CATEGORIES (≤{} services, need more services or merging):",
                self.config.min_leaf_size
            )];
            for (sub_id, count) in tiny_cats {
                lines.push(format!(
                    "  {sub_id} ({}): {count} services",
                    subcategories[sub_id].name
                ));
            }
            lines.join("\n")
        };

        let prompt = fill(
            REFINE_SUBCATEGORIES_TEMPLATE,
            &[
                ("parent_name", parent_info.name_or("Unknown")),
                ("n_subcategories", &n_subcats.to_string()),
                (
                    "current_subcategories_text",
                    &format_categories_for_prompt(subcategories),
                ),
                ("n_total", &services.len().to_string()),
                ("n_normal", &stats.n_normal.to_string()),
                ("n_generic", &stats.n_generic.to_string()),
                ("n_unclassified", &stats.n_unclassified.to_string()),
                ("generic_threshold", &threshold.to_string()),
                (
                    "subcategory_stats_text",
                    &format_subcategory_stats(subcategories, assignments),
                ),
                ("tiny_cats_text", &tiny_cats_text),
                (
                    "problem_services_text",
                    &format_problem_services(
                        services,
                        assignments,
                        subcategories,
                        self.config.generic_ratio,
                        50,
                    ),
                ),
                ("max_sub", &max_cats.to_string()),
                ("parent_id", &parent_info.id),
            ],
        );
        let response = self
            .llm
            .call(
                &[ChatMessage::system(SYSTEM_REFINE_NODE), ChatMessage::user(prompt)],
                self.config.temperature_design,
                Some(max_tokens),
            )
            .await;
        if !response.success {
            self.sink.warn(format!(
                "Refinement LLM call failed: {}",
                response.error.as_deref().unwrap_or("unknown")
            ));
            return subcategories.clone();
        }
        let Some(result) = parse_json_response(&response.content) else {
            self.sink.warn("Failed to parse refinement response");
            return subcategories.clone();
        };
        let changes = result
            .get("changes_summary")
            .and_then(Value::as_str)
            .unwrap_or("No summary");
        self.sink.log(format!("Refinement: {changes}"));

        let key = if result.get("subcategories").is_some() {
            "subcategories"
        } else {
            "categories"
        };
        let cat_list = result.get(key).and_then(Value::as_array);
        match cat_list {
            Some(list) if !list.is_empty() => {
                let mut refined = Subcategories::new();
                for cat in list {
                    let cat_id = cat
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("cat_{}", refined.len() + 1));
                    refined.insert(cat_id, category_def_from_value(cat, false));
                }
                refined
            }
            _ => {
                self.sink
                    .warn("Refinement returned no subcategories, keeping current");
                subcategories.clone()
            }
        }
    }

    // =========================================================================
    // Root Category Validation
    // =========================================================================

    /// Validate root categories through the LLM and redesign violations,
    /// up to `max_retries` rounds.
    pub async fn validate_and_fix_root_categories(
        &self,
        mut categories: Subcategories,
        keywords: &KeywordCounts,
        max_retries: usize,
    ) -> Subcategories {
        for attempt in 0..max_retries {
            let violations = self.llm_validate_categories(&categories).await;
            if violations.is_empty() {
                self.sink.log(format!(
                    "Root category validation PASSED (attempt {})",
                    attempt + 1
                ));
                return categories;
            }

            let mut lines = vec![format!(
                "Root category validation FAILED (attempt {}/{max_retries}):",
                attempt + 1
            )];
            let mut ids: Vec<&String> = violations.keys().collect();
            ids.sort();
            for cat_id in &ids {
                lines.push(format!(
                    "  {cat_id} ({}): {}",
                    categories[cat_id.as_str()].name,
                    violations[cat_id.as_str()]
                ));
            }
            self.sink.log(lines.join("\n"));

            // Collect keywords from violated categories
            let mut violated_keywords = KeywordCounts::new();
            for cat_id in violations.keys() {
                for kw in categories[cat_id].associated_keywords.iter().flatten() {
                    if let Some(count) = keywords.get(kw) {
                        violated_keywords.insert(kw.clone(), *count);
                    }
                }
            }
            if violated_keywords.is_empty() {
                let mut valid_keywords: HashSet<&String> = HashSet::new();
                for (cat_id, info) in &categories {
                    if !violations.contains_key(cat_id) {
                        valid_keywords.extend(info.associated_keywords.iter().flatten());
                    }
                }
                violated_keywords = keywords
                    .iter()
                    .filter(|(kw, _)| !valid_keywords.contains(kw))
                    .map(|(kw, count)| (kw.clone(), *count))
                    .collect();
            }

            let violated_ids: HashSet<String> = violations.keys().cloned().collect();
            categories = self
                .redesign_violated_categories(categories, &violated_ids, &violated_keywords)
                .await;
            self.print_categories(&categories, &format!("redesigned (attempt {})", attempt + 1));
        }
        categories
    }

    /// Ask the LLM to validate categories. Returns `cat_id -> reason`.
    async fn llm_validate_categories(&self, categories: &Subcategories) -> IndexMap<String, String> {
        let prompt = fill(
            VALIDATE_ROOT_CATEGORIES_TEMPLATE,
            &[("categories_text", &format_categories_for_prompt(categories))],
        );
        let response = self
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_VALIDATE_ROOT_CATEGORIES),
                    ChatMessage::user(prompt),
                ],
                0.0,
                Some(self.config.max_tokens_validate),
            )
            .await;
        if !response.success {
            self.sink.warn(format!(
                "LLM validation call failed: {}",
                response.error.as_deref().unwrap_or("unknown")
            ));
            return IndexMap::new();
        }
        let Some(result) = parse_json_response(&response.content) else {
            self.sink.warn("Failed to parse LLM validation response");
            return IndexMap::new();
        };
        let Some(validations) = result.get("validations").and_then(Value::as_array) else {
            self.sink.warn("Failed to parse LLM validation response");
            return IndexMap::new();
        };
        let mut violations = IndexMap::new();
        for v in validations {
            let valid = v.get("valid").and_then(Value::as_bool).unwrap_or(true);
            if valid {
                continue;
            }
            let cat_id = v.get("id").and_then(Value::as_str).unwrap_or("");
            if categories.contains_key(cat_id) {
                let reason = v
                    .get("reason")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("violation_type").and_then(Value::as_str))
                    .unwrap_or("unknown");
                violations.insert(cat_id.to_string(), reason.to_string());
            }
        }
        violations
    }

    async fn redesign_violated_categories(
        &self,
        categories: Subcategories,
        violated_ids: &HashSet<String>,
        violated_keywords: &KeywordCounts,
    ) -> Subcategories {
        let violated_cats: Subcategories = categories
            .iter()
            .filter(|(id, _)| violated_ids.contains(*id))
            .map(|(id, def)| (id.clone(), def.clone()))
            .collect();
        let violated_kw_text = if violated_keywords.is_empty() {
            "(keywords not available — redistribute based on category descriptions)".to_string()
        } else {
            format_keywords_for_design(violated_keywords)
        };
        let prompt = fill(
            REDESIGN_VIOLATED_CATEGORIES_TEMPLATE,
            &[
                ("all_categories_text", &format_categories_for_prompt(&categories)),
                (
                    "violated_categories_text",
                    &format_categories_for_prompt(&violated_cats),
                ),
                ("violated_keywords_text", &violated_kw_text),
            ],
        );
        let response = self
            .llm
            .call(
                &[
                    ChatMessage::system(SYSTEM_CATEGORY_DESIGN),
                    ChatMessage::user(prompt),
                ],
                self.config.temperature_design,
                Some(self.config.max_tokens_design),
            )
            .await;
        if !response.success {
            self.sink
                .warn("Redesign LLM call failed, keeping current categories");
            return categories;
        }
        let Some(result) = parse_json_response(&response.content) else {
            self.sink
                .warn("Failed to parse redesign response, keeping current categories");
            return categories;
        };
        match parse_categories(&result) {
            Ok(new_categories) if new_categories.len() < 3 => {
                self.sink.warn(format!(
                    "Redesign returned only {} categories, keeping current",
                    new_categories.len()
                ));
                categories
            }
            Ok(new_categories) => new_categories,
            Err(e) => {
                self.sink
                    .warn(format!("Redesign parse error: {e}, keeping current categories"));
                categories
            }
        }
    }
}

/// Parse an LLM design response (`categories` or `subcategories` list).
pub fn parse_categories(result: &Value) -> Result<Subcategories> {
    let key = if result.get("subcategories").is_some() {
        "subcategories"
    } else {
        "categories"
    };
    let list = result.get(key).and_then(Value::as_array);
    let Some(list) = list.filter(|l| !l.is_empty()) else {
        return Err(Error::Llm("Category design returned no categories".into()));
    };
    let mut categories = Subcategories::new();
    for cat in list {
        let cat_id = cat
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("cat_{}", categories.len() + 1));
        categories.insert(cat_id, category_def_from_value(cat, true));
    }
    Ok(categories)
}

fn category_def_from_value(cat: &Value, keep_keywords: bool) -> SubcategoryDef {
    let text = |key: &str, default: &str| -> String {
        cat.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| default.to_string())
    };
    let associated_keywords = if keep_keywords {
        cat.get("associated_keywords").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    SubcategoryDef {
        name: text("name", "Unknown"),
        description: text("description", ""),
        boundary: text("boundary", ""),
        decision_rule: text("decision_rule", ""),
        associated_keywords,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn parse_categories_variants() -> TestResult {
        let r = json!({"categories": [{"id": "cat_sub1", "name": "A", "associated_keywords": ["x"]}, {"name": "B"}]});
        let c = parse_categories(&r)?;
        assert_eq!(c["cat_sub1"].associated_keywords, Some(vec!["x".to_string()]));
        assert_eq!(c["cat_2"].name, "B");
        assert_eq!(c["cat_2"].description, "");
        assert!(parse_categories(&json!({"categories": []})).is_err());
        assert!(parse_categories(&json!({"subcategories": [{"id": "s"}]})).is_ok());
        Ok(())
    }
}
