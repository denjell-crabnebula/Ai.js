// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Prompt templates, formatting helpers and shared types for taxonomy
//! building.
//!
//! Templates are the Python `str.format` strings, verbatim. Placeholders
//! are `{name}`; literal braces are doubled. Use [`fill`] to render them.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::taxonomy::{CategoryInfo, ServiceRecord};
use crate::util::truncate_chars;

// =============================================================================
// Shared types
// =============================================================================

/// A subcategory designed by the LLM.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubcategoryDef {
    pub name: String,
    pub description: String,
    pub boundary: String,
    pub decision_rule: String,
    /// Keywords the LLM associated with this category (keyword based
    /// design only). Used when redesigning violated root categories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub associated_keywords: Option<Vec<String>>,
}

/// `sub_id -> definition`, in LLM output order.
pub type Subcategories = IndexMap<String, SubcategoryDef>;

/// Where one service was classified.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    #[serde(default)]
    pub category_ids: Vec<String>,
    #[serde(default)]
    pub reasoning: String,
}

/// `service_id -> assignment` (the `assignments.json` format).
pub type Assignments = IndexMap<String, Assignment>;

/// Result of classifying a single service.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassificationResult {
    pub service_id: String,
    pub category_ids: Vec<String>,
    pub reasoning: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Result of splitting a single node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeSplitResult {
    pub node_id: String,
    pub subcategories: Subcategories,
    pub assignments: Assignments,
    /// Services that remain at the parent node.
    pub unclassified_service_ids: Vec<String>,
    pub iterations_used: usize,
    pub converged: bool,
}

/// Classification statistics for one iteration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassificationStats {
    pub n_normal: usize,
    pub n_generic: usize,
    pub n_unclassified: usize,
    pub cat_counts: IndexMap<String, usize>,
}

/// Metadata of the node being split (`class.json` entry plus its id).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeInfo {
    pub id: String,
    pub info: CategoryInfo,
}

impl NodeInfo {
    pub fn new(id: impl Into<String>, info: CategoryInfo) -> Self {
        Self { id: id.into(), info }
    }

    pub fn name_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.info.name_or(default)
    }

    pub fn description_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.info.description_or(default)
    }

    pub fn boundary(&self) -> &str {
        self.info.boundary_text()
    }
}

// =============================================================================
// Template rendering
// =============================================================================

/// Render a Python `str.format` template: `{key}` is replaced from `values`,
/// `{{` and `}}` become single braces. Replacement values are inserted
/// verbatim. Unknown keys are left as written.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut key = String::new();
                let mut closed = false;
                for k in chars.by_ref() {
                    if k == '}' {
                        closed = true;
                        break;
                    }
                    key.push(k);
                }
                match values.iter().find(|(name, _)| *name == key) {
                    Some((_, value)) if closed => out.push_str(value),
                    _ => {
                        out.push('{');
                        out.push_str(&key);
                        if closed {
                            out.push('}');
                        }
                    }
                }
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
                out.push('}');
            }
            other => out.push(other),
        }
    }
    out
}

// =============================================================================
// Formatting helpers
// =============================================================================

/// Format services as `- name: description` lines.
pub fn format_services_for_prompt(services: &[ServiceRecord], max_desc_len: usize) -> String {
    services
        .iter()
        .map(|svc| {
            let desc = truncate_chars(svc.description_or("No description"), max_desc_len);
            format!("- {}: {}", svc.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sorted `(id, def)` pairs by id.
fn sorted_subcategories(categories: &Subcategories) -> Vec<(&String, &SubcategoryDef)> {
    let mut items: Vec<_> = categories.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    items
}

/// Format categories for the classification prompt.
pub fn format_categories_for_prompt(categories: &Subcategories) -> String {
    let mut lines = Vec::new();
    for (cat_id, info) in sorted_subcategories(categories) {
        lines.push(format!("{cat_id}: {}", info.name));
        lines.push(format!("  Description: {}", info.description));
        if !info.decision_rule.is_empty() {
            lines.push(format!("  Decision Rule: {}", info.decision_rule));
        }
        if !info.boundary.is_empty() {
            lines.push(format!("  NOT here: {}", info.boundary));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Format the parent boundary line when present.
pub fn format_parent_boundary_section(parent_info: &NodeInfo) -> String {
    let boundary = parent_info.boundary();
    if boundary.is_empty() {
        String::new()
    } else {
        format!("Parent boundary: {boundary}\n")
    }
}

/// Per-subcategory service counts.
pub fn format_subcategory_stats(subcategories: &Subcategories, assignments: &Assignments) -> String {
    let mut cat_counts: IndexMap<&str, usize> = IndexMap::new();
    for result in assignments.values() {
        for cat_id in &result.category_ids {
            *cat_counts.entry(cat_id.as_str()).or_insert(0) += 1;
        }
    }
    sorted_subcategories(subcategories)
        .into_iter()
        .map(|(cat_id, info)| {
            let count = cat_counts.get(cat_id.as_str()).copied().unwrap_or(0);
            format!("  {cat_id} ({}): {count} services", info.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =============================================================================
// Step 1: Keyword Extraction
// =============================================================================

pub const SYSTEM_KEYWORD_EXTRACTION: &str = "You are an expert at identifying which USER DOMAIN an API service belongs to. You extract keywords that describe the FUNCTIONAL DOMAIN — the real-world field or area of life that the end user cares about when using this service.";

pub const KEYWORD_EXTRACTION_TEMPLATE: &str = r#"Extract functional domain keywords for each of these {batch_size} API services.
{node_context_section}
RULES:
1. For each service, extract 1-{max_keywords} keywords describing the USER DOMAIN it serves — the real-world field or area of life the end user cares about.
2. Keywords must name the DOMAIN/FIELD, not the technical operation:
   GOOD: "weather", "stock_market", "restaurant", "email", "flight", "news", "sports", "music", "real_estate"
   BAD: "data_query", "api", "service", "tool", "information", "search", "conversion", "processing", "analytics" (these describe HOW, not WHAT DOMAIN)
   IMPORTANT: "weather" is better than "weather_data_query"; "travel" is better than "travel_search"; "finance" is better than "financial_analytics"
3. Use snake_case, 1-2 words per keyword. Prefer single domain words.
4. REUSE existing keywords from the list below when they fit — do NOT create synonyms.
   For example, if "weather" already exists, do NOT create "weather_forecast" or "weather_data".
   Only create a NEW keyword when no existing keyword adequately describes the service's domain.
5. Focus on the END USER'S PERSPECTIVE: What domain does a person care about when they use this service?
   - A weather API → keyword: "weather" (the user cares about weather, not about "data retrieval")
   - A flight search API → keyword: "travel" or "flight" (the user wants to travel, not to "search")
   - A stock price API → keyword: "stock_market" or "finance" (the user cares about finance)
   - An image recognition API → keyword based on its APPLICATION domain (e.g., "security" for face recognition, "healthcare" for medical imaging), not "image_processing"

EXISTING KEYWORDS (reuse when applicable):
{existing_keywords_text}

SERVICES:
{services_text}

Output ONLY valid JSON:
```json
{{
  "extractions": [
    {{"service_id": "...", "keywords": ["keyword1", "keyword2"]}}
  ]
}}
```"#;

// =============================================================================
// Category Design from Keywords (large nodes)
// =============================================================================

pub const SYSTEM_CATEGORY_DESIGN: &str = "You are an expert at designing functional taxonomy systems for API service registries. You create categories with CLEAR, NON-OVERLAPPING boundaries — like hospital departments where each patient knows exactly which department to go to.";

pub const CATEGORY_DESIGN_TEMPLATE: &str = r#"Below are {n_keywords} functional domain keywords extracted from {n_services} API services,
with their frequency counts (how many services have this keyword):

{keywords_text}
{node_context_section}
Your task: Group these keywords into up to {max_cats} categories based on USER FUNCTIONAL DOMAIN.

CRITICAL RULES:
1. **USER FUNCTIONAL DOMAIN ONLY**: Each category must represent a real-world domain that end users care about.
   ALLOWED: "Travel & Tourism", "Finance & Banking", "Healthcare", "Food & Dining", "Sports", "News & Media", "Weather & Climate", "Entertainment", "Education", "Real Estate", "Automotive", "Communication"
   FORBIDDEN — Technical approach: "Developer Tools", "AI & Machine Learning", "Cloud Services", "Blockchain & Crypto Technology", "API Tools"
   FORBIDDEN — Data type: "Image Processing", "Audio Processing", "Video Services", "Text Analysis", "Data & Utilities"
   FORBIDDEN — Operation method: "Data Query", "Search Services", "Data Conversion", "Validation Services", "Analytics"
   FORBIDDEN — Disguised catch-all: "Data & Utilities", "Information Services", "Technical Services", "Digital Services", "General Tools" — any category whose services lack a common user scenario

2. **CLEAR BOUNDARIES**: Each category must have an explicit boundary statement pointing to other categories

3. **ABSOLUTELY NO CATCH-ALL**: Do NOT create any category that serves as a dumping ground.
   Test: Can you describe a SPECIFIC user scenario that connects ALL services in this category?
   - "I want to book a flight" → Travel ✓ (all travel services share this user context)
   - "I need domain-agnostic data processing" → NO USER SAYS THIS ✗ (catch-all in disguise)
   If a category's description uses words like "domain-agnostic", "general-purpose", "various", "miscellaneous", "cross-domain", it is a catch-all. Hard-to-classify services should be placed into their CLOSEST functional domain, not into a new generic category.

4. **PROTECT SMALL DOMAINS**: Do NOT merge small but distinct domains into larger ones.
   Even if only a few services exist for "Weather", "Astronomy", or "Pets", they deserve their own category if they represent a distinct user need. It is better to have a small category (5-10 services) than to lose it by merging into an unrelated domain.

5. **COMPLETE COVERAGE**: Every keyword must be assignable to at least one category.

For each category provide:
- id: "{parent_id}_sub1", "{parent_id}_sub2", etc.
- name: 2-4 word descriptive name
- description: What services belong here — positive definition, under 200 chars
- boundary: What does NOT belong here — point to other categories
- decision_rule: "If the user wants to [specific user goal], classify here"
- associated_keywords: Which input keywords map to this category

Output ONLY valid JSON:
```json
{{
  "dimension": "functional domain",
  "categories": [
    {{
      "id": "{parent_id}_sub1",
      "name": "...",
      "description": "...",
      "boundary": "...",
      "decision_rule": "...",
      "associated_keywords": ["keyword1", "keyword2"]
    }}
  ]
}}
```"#;

// =============================================================================
// Root Category Validation (LLM-based)
// =============================================================================

pub const SYSTEM_VALIDATE_ROOT_CATEGORIES: &str = "You are a strict taxonomy quality auditor. You identify categories that violate the functional-domain-only constraint for top-level API service classification.";

pub const VALIDATE_ROOT_CATEGORIES_TEMPLATE: &str = r#"Review these top-level categories for an API service registry.

CATEGORIES:
{categories_text}

CHECK EACH CATEGORY against these STRICT rules:

1. **Must be a USER FUNCTIONAL DOMAIN**: The category must represent a real-world area of life that end users care about (e.g., Travel, Finance, Healthcare, Food, Sports).
   VIOLATIONS: categories based on technology (AI, Blockchain, Developer Tools), data type (Image/Audio/Video Processing), or operation (Data Query, Analytics, Conversion).

2. **Must NOT be a catch-all**: Every service in the category must share a concrete user scenario.
   VIOLATIONS: descriptions containing "domain-agnostic", "general-purpose", "various", or categories that are essentially "everything else".

3. **Must NOT overlap significantly**: If two categories cover substantially the same user domain, one should be merged or refined.

For each category, output:
- "valid": true/false
- "violation_type": null or one of "technical_approach", "data_type", "operation_method", "catch_all", "overlap"
- "reason": brief explanation if invalid

Output ONLY valid JSON:
```json
{{
  "validations": [
    {{"id": "cat_X", "valid": true, "violation_type": null, "reason": null}},
    {{"id": "cat_Y", "valid": false, "violation_type": "catch_all", "reason": "Description says domain-agnostic"}}
  ]
}}
```"#;

pub const REDESIGN_VIOLATED_CATEGORIES_TEMPLATE: &str = r#"You designed top-level categories but some VIOLATED the functional-domain-only rule.

ALL CURRENT CATEGORIES:
{all_categories_text}

VIOLATED CATEGORIES (must be fixed):
{violated_categories_text}

KEYWORDS FROM VIOLATED CATEGORIES:
{violated_keywords_text}

YOUR TASK: Redistribute the keywords from violated categories into proper functional domain categories.

RULES:
1. You MUST REMOVE every violated category listed above.
2. For each keyword from violated categories:
   - If it fits an existing valid category, assign it there.
   - If multiple keywords share a distinct user functional domain NOT covered by existing categories, create a NEW functional domain category for them.
   - If a keyword represents a very niche domain, it's OK to create a small category (even for just 5-10 services).
3. Keep all existing VALID categories unchanged (same id, name, description, boundary, decision_rule).
4. Any new categories must be USER FUNCTIONAL DOMAINS (not technical/data-type/operation categories).
5. Do NOT create catch-all categories to absorb leftover keywords.

Output the COMPLETE updated category list (valid + new):
```json
{{
  "dimension": "functional domain",
  "categories": [
    {{
      "id": "...",
      "name": "...",
      "description": "...",
      "boundary": "...",
      "decision_rule": "...",
      "associated_keywords": ["keyword1", "keyword2"]
    }}
  ]
}}
```"#;

// =============================================================================
// Category Design from Service Descriptions (small nodes)
// =============================================================================

pub const SYSTEM_DESIGN_FROM_DESCRIPTIONS: &str = "You are an expert at subdividing a functional domain into clear, non-overlapping sub-categories for API service classification.";

pub const DESIGN_FROM_DESCRIPTIONS_TEMPLATE: &str = r#"You are designing categories for a group of {service_count} API services.
{node_context_section}
Design up to {max_cats} categories with CLEAR, NON-OVERLAPPING boundaries.

MANDATORY RULES:
1. **SINGLE CLASSIFICATION DIMENSION**: All categories MUST use the SAME classification dimension.
   Priority order (use the highest applicable):
   a) Functional domain (e.g., Travel, Finance, Food) - PREFERRED
   b) Data/content type (e.g., Image Processing, Audio Processing) - if functional domain doesn't differentiate
   c) Operation type (e.g., Data Query, Data Conversion) - last resort
   GOOD: All by entity type (Stocks, Crypto, Forex) OR all by use-case (Analysis, Trading, Monitoring)
   BAD: Mixing entity (Stocks) with use-case (Analysis) at the same level
2. **NO TECHNICAL CATEGORIES**: Do NOT create AI, Machine Learning, Cloud, or API-related categories
3. **NO CATCH-ALL**: Do NOT create "Other", "General", "Miscellaneous" categories
4. **CONCISE DESCRIPTIONS**: Each description should be 1-2 sentences (under 200 characters)
5. **NON-OVERLAPPING**: Every pair of categories must have a clear distinguishing criterion

For each category provide:
- id: "{parent_id}_sub1", "{parent_id}_sub2", etc.
- name: 2-4 word descriptive name
- description: What services belong here (positive definition, under 200 chars)
- boundary: What does NOT belong here (pointing to sibling categories)
- decision_rule: "If the service primarily does X, classify here"

Services (name: description):
{services_text}

Output JSON:
```json
{{
  "dimension_used": "brief description of classification dimension chosen",
  "categories": [
    {{
      "id": "{parent_id}_sub1",
      "name": "...",
      "description": "...",
      "boundary": "...",
      "decision_rule": "..."
    }}
  ]
}}
```"#;

// =============================================================================
// Node context helpers
// =============================================================================

/// Node context section for the keyword extraction prompt (empty for root).
pub fn format_node_context_for_keywords(node_info: Option<&NodeInfo>) -> String {
    let Some(node_info) = node_info else {
        return String::new();
    };
    let name = node_info.name_or("Unknown");
    let description = node_info.description_or("");
    format!(
        "\nNODE CONTEXT:\nThese services belong to the category: \"{name}\"\nCategory description: {description}\nFocus on keywords that distinguish services WITHIN this specific domain.\n"
    )
}

/// Parent context section for the category design prompt (empty for root).
pub fn format_node_context_for_design(node_info: Option<&NodeInfo>) -> String {
    let Some(node_info) = node_info else {
        return String::new();
    };
    let name = node_info.name_or("Unknown");
    let description = node_info.description_or("");
    let boundary = node_info.boundary();
    let mut lines = vec![
        format!("\nPARENT CATEGORY: \"{name}\""),
        format!("Parent description: {description}"),
    ];
    if !boundary.is_empty() {
        lines.push(format!("Parent boundary: {boundary}"));
    }
    lines.push("Design subcategories WITHIN this domain.\n".to_string());
    lines.join("\n")
}

/// Keywords sorted by count (descending, stable) as `(kw, count)` pairs.
pub fn sorted_keywords(keywords: &IndexMap<String, u64>) -> Vec<(&String, u64)> {
    let mut items: Vec<(&String, u64)> = keywords.iter().map(|(k, v)| (k, *v)).collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.1));
    items
}

/// Accumulated keywords for the extraction prompt (top `max_keywords`).
pub fn format_keywords_for_prompt(keywords: &IndexMap<String, u64>, max_keywords: usize) -> String {
    if keywords.is_empty() {
        return "(none yet — you are defining the initial keywords)".to_string();
    }
    sorted_keywords(keywords)
        .into_iter()
        .take(max_keywords)
        .map(|(kw, count)| format!("- {kw} (count: {count})"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// All keywords for the category design prompt.
pub fn format_keywords_for_design(keywords: &IndexMap<String, u64>) -> String {
    sorted_keywords(keywords)
        .into_iter()
        .map(|(kw, count)| format!("- {kw}: {count} services"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A batch of services for keyword extraction.
pub fn format_services_batch(services: &[ServiceRecord], max_desc_len: usize) -> String {
    services
        .iter()
        .map(|svc| {
            let desc = truncate_chars(svc.description_or("No description"), max_desc_len);
            format!("- [{}] {}: {}", svc.id, svc.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =============================================================================
// Per-Service Classification within a Node
// =============================================================================

pub const SYSTEM_CLASSIFY_NODE: &str = "You are a precise API service classifier. For each service, identify ALL relevant sub-categories based on functional domain.";

pub const CLASSIFY_SERVICE_IN_NODE_TEMPLATE: &str = r#"Classify this API service into the sub-categories below.

PARENT CATEGORY: {parent_name}
{parent_description}

SUB-CATEGORIES:
{subcategories_text}

SERVICE DESCRIPTION:
{service_description}

INSTRUCTIONS:
1. List ALL sub-categories that are relevant to this service. If the service genuinely fits multiple sub-categories, list all of them.
2. If NO sub-category fits, return an empty list.

Output ONLY valid JSON:
```json
{{
  "reasoning": "brief explanation of classification logic",
  "category_ids": ["cat_X"]
}}
```"#;

// =============================================================================
// Subcategory Refinement
// =============================================================================

pub const SYSTEM_REFINE_NODE: &str = "You are an expert at refining sub-category definitions. You analyze classification feedback and make targeted adjustments to improve coverage and clarity.";

pub const REFINE_SUBCATEGORIES_TEMPLATE: &str = r#"You defined sub-categories for "{parent_name}" but classification reveals problems.

CURRENT SUB-CATEGORIES ({n_subcategories}):
{current_subcategories_text}

CLASSIFICATION FEEDBACK:
- Total services: {n_total}
- Normal (1-{generic_threshold} categories): {n_normal}
- Generic (>{generic_threshold} categories, too many matches): {n_generic}
- Unclassified (0 categories): {n_unclassified}
- Per sub-category distribution:
{subcategory_stats_text}
{tiny_cats_text}

PROBLEMATIC SERVICES:
{problem_services_text}

YOUR TASK: Refine the sub-categories to reduce problematic services. You may:
1. SPLIT a large or vague sub-category into two clearer ones
2. MERGE two overlapping sub-categories to eliminate confusion
3. ADJUST descriptions/boundaries/decision_rules for clarity
4. ADD a new sub-category for services that don't fit existing ones

Focus on:
- GENERIC services (too many matches) → sharpen boundaries to make categories more distinctive
- UNCLASSIFIED services (no matches) → add new categories or broaden existing boundaries to cover them
- TINY sub-categories (too few services) → broaden boundaries to attract more services, or merge with similar categories

CONSTRAINTS:
- Keep up to {max_sub} sub-categories
- Do NOT create catch-all sub-categories ("Other", "General", "Miscellaneous", "Utilities", "Tools")
- Do NOT create disguised catch-all categories with "domain-agnostic", "general-purpose", or "various" in their description
- ALL sibling sub-categories must use the SAME classification dimension (do not mix functional with technical or data-type dimensions)
- PRESERVE existing sub-category IDs for unchanged/adjusted categories
- Only use NEW IDs for genuinely new sub-categories
- CONCISE DESCRIPTIONS: Each description MUST be under 200 characters.

Output the COMPLETE refined sub-category list:
```json
{{
  "changes_summary": "Brief description of what changed",
  "subcategories": [
    {{
      "id": "{parent_id}_sub1",
      "name": "...",
      "description": "...",
      "boundary": "...",
      "decision_rule": "..."
    }}
  ]
}}
```"#;

/// Generic threshold: `max(1, int(n_subcats * generic_ratio))`.
pub fn generic_threshold(n_subcats: usize, generic_ratio: f64) -> usize {
    ((n_subcats as f64 * generic_ratio) as usize).max(1)
}

/// Problematic services grouped by type for the refinement prompt.
pub fn format_problem_services(
    services: &[ServiceRecord],
    assignments: &Assignments,
    subcategories: &Subcategories,
    generic_ratio: f64,
    max_samples: usize,
) -> String {
    let threshold = generic_threshold(subcategories.len(), generic_ratio);
    let mut generic = Vec::new();
    let mut unclassified = Vec::new();

    for (svc_id, result) in assignments {
        let Some(svc) = services.iter().find(|s| &s.id == svc_id) else {
            continue;
        };
        let n_cats = result.category_ids.len();
        let desc = truncate_chars(svc.description_or("No description"), 150);
        if n_cats == 0 {
            unclassified.push(format!("  - {desc}"));
        } else if n_cats > threshold {
            generic.push(format!(
                "  - {desc} → matched: {}",
                result.category_ids.join(", ")
            ));
        }
    }

    if generic.is_empty() && unclassified.is_empty() {
        return "(No problematic services found)".to_string();
    }
    let mut sections = Vec::new();
    if !generic.is_empty() {
        sections.push(format!(
            "GENERIC (>{threshold} categories matched, boundaries too vague): {} services",
            generic.len()
        ));
        sections.extend(generic.into_iter().take(max_samples));
    }
    if !unclassified.is_empty() {
        sections.push(format!(
            "\nUNCLASSIFIED (0 categories, no match found): {} services",
            unclassified.len()
        ));
        sections.extend(unclassified.into_iter().take(max_samples));
    }
    sections.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn fill_handles_braces_and_values() -> TestResult {
        let out = fill("a {x} {{lit}} {y}", &[("x", "1"), ("y", "{z}")]);
        assert_eq!(out, "a 1 {lit} {z}");
        assert_eq!(fill("{missing}", &[]), "{missing}");
        let rendered = fill(
            CLASSIFY_SERVICE_IN_NODE_TEMPLATE,
            &[
                ("parent_name", "P"),
                ("parent_description", "d"),
                ("subcategories_text", "S"),
                ("service_description", "svc"),
            ],
        );
        assert!(rendered.contains("PARENT CATEGORY: P\nd\n"));
        assert!(rendered.contains("{\n  \"reasoning\""));
        assert!(!rendered.contains("{{"));
        Ok(())
    }

    #[test]
    fn category_formatting() -> TestResult {
        let mut subs = Subcategories::new();
        subs.insert(
            "cat_sub2".into(),
            SubcategoryDef {
                name: "B".into(),
                description: "db".into(),
                boundary: "".into(),
                decision_rule: "r".into(),
                associated_keywords: None,
            },
        );
        subs.insert(
            "cat_sub1".into(),
            SubcategoryDef {
                name: "A".into(),
                description: "da".into(),
                boundary: "nb".into(),
                decision_rule: "".into(),
                associated_keywords: None,
            },
        );
        let text = format_categories_for_prompt(&subs);
        assert_eq!(
            text,
            "cat_sub1: A\n  Description: da\n  NOT here: nb\n\ncat_sub2: B\n  Description: db\n  Decision Rule: r\n"
        );
        let mut assignments = Assignments::new();
        assignments.insert(
            "s1".into(),
            Assignment {
                category_ids: vec!["cat_sub1".into(), "cat_sub2".into()],
                reasoning: "".into(),
            },
        );
        assignments.insert("s2".into(), Assignment::default());
        assert_eq!(
            format_subcategory_stats(&subs, &assignments),
            "  cat_sub1 (A): 1 services\n  cat_sub2 (B): 1 services"
        );
        let services = vec![
            ServiceRecord::new("s1", "S1", "one"),
            ServiceRecord::new("s2", "S2", "two"),
        ];
        let problems = format_problem_services(&services, &assignments, &subs, 1.0 / 3.0, 50);
        assert!(problems.starts_with("GENERIC (>1 categories matched, boundaries too vague): 1 services\n  - one → matched: cat_sub1, cat_sub2"));
        assert!(problems.contains("\nUNCLASSIFIED (0 categories, no match found): 1 services\n  - two"));
        assert_eq!(generic_threshold(2, 1.0 / 3.0), 1);
        assert_eq!(generic_threshold(9, 1.0 / 3.0), 3);
        Ok(())
    }

    #[test]
    fn keyword_formatting() -> TestResult {
        let mut kw = IndexMap::new();
        kw.insert("a".to_string(), 1u64);
        kw.insert("b".to_string(), 5u64);
        assert_eq!(
            format_keywords_for_design(&kw),
            "- b: 5 services\n- a: 1 services"
        );
        assert_eq!(format_keywords_for_prompt(&kw, 1), "- b (count: 5)");
        assert!(format_keywords_for_prompt(&IndexMap::new(), 3).starts_with("(none yet"));
        let ctx = format_node_context_for_design(Some(&NodeInfo::new("x", CategoryInfo::new("N", "D"))));
        assert_eq!(
            ctx,
            "\nPARENT CATEGORY: \"N\"\nParent description: D\nDesign subcategories WITHIN this domain.\n"
        );
        assert_eq!(format_node_context_for_design(None), "");
        assert!(
            format_node_context_for_keywords(Some(&NodeInfo::new("x", CategoryInfo::new("N", "D"))))
                .contains("These services belong to the category: \"N\"")
        );
        Ok(())
    }
}
