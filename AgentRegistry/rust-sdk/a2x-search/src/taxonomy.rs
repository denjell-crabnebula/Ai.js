// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! On-disk data files shared by build, search and evaluation.
//!
//! - `service.json`: a JSON array of [`ServiceRecord`]
//! - `taxonomy.json`: tree structure and build status ([`TaxonomyFile`])
//! - `class.json`: category metadata ([`ClassFile`])
//! - `query.json`: evaluation queries ([`QueryObject`])
//!
//! Field names match the Python files exactly. Unknown keys are kept in
//! `extra` maps so a round trip through Rust does not drop them.

use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Result;
use crate::util::read_json;

/// Version string written into taxonomy.json and class.json.
pub const TAXONOMY_VERSION: &str = "2.0-hierarchical";

/// Id of the root category.
pub const ROOT_ID: &str = "root";

/// One entry of `service.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ServiceRecord {
    pub fn new(id: impl Into<String>, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: Some(description.into()),
            extra: Map::new(),
        }
    }

    /// Description, or `default` when the field is absent (Python `.get`).
    pub fn description_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.description.as_deref().unwrap_or(default)
    }

    /// Description or the empty string.
    pub fn description_text(&self) -> &str {
        self.description_or("")
    }
}

/// `service_id -> ServiceRecord`, in file order.
pub type ServicesIndex = IndexMap<String, ServiceRecord>;

/// Load `service.json`.
pub fn load_services(path: &Path) -> Result<Vec<ServiceRecord>> {
    read_json(path)
}

/// Build the id index from a service list (later duplicates win).
pub fn index_services(services: Vec<ServiceRecord>) -> ServicesIndex {
    let mut index = ServicesIndex::with_capacity(services.len());
    for s in services {
        index.insert(s.id.clone(), s);
    }
    index
}

/// One node of `taxonomy.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CategoryNode {
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
}

/// `taxonomy.json`: tree structure plus build status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaxonomyFile {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default)]
    pub categories: IndexMap<String, CategoryNode>,
    /// `"bfs"`, `"cross_domain"` or `"complete"`. Absent in hand-made files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_status: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_version() -> String {
    TAXONOMY_VERSION.to_string()
}

fn default_root() -> String {
    ROOT_ID.to_string()
}

impl Default for TaxonomyFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            root: default_root(),
            categories: IndexMap::new(),
            build_status: None,
            extra: Map::new(),
        }
    }
}

impl TaxonomyFile {
    pub fn load(path: &Path) -> Result<Self> {
        read_json(path)
    }

    pub fn node(&self, id: &str) -> Option<&CategoryNode> {
        self.categories.get(id)
    }

    pub fn children(&self, id: &str) -> &[String] {
        self.categories
            .get(id)
            .map(|c| c.children.as_slice())
            .unwrap_or(&[])
    }

    pub fn services(&self, id: &str) -> &[String] {
        self.categories
            .get(id)
            .map(|c| c.services.as_slice())
            .unwrap_or(&[])
    }

    /// `child_id -> parent_id` for every edge.
    pub fn parent_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (cat_id, node) in &self.categories {
            for child in &node.children {
                map.insert(child.clone(), cat_id.clone());
            }
        }
        map
    }

    /// All leaf ids under `node_id`, or `[node_id]` when it is a leaf.
    /// Traversal order matches the Python stack-based walk.
    pub fn leaves_under(&self, node_id: &str) -> Vec<String> {
        let children = self.children(node_id);
        if children.is_empty() {
            return vec![node_id.to_string()];
        }
        let mut leaves = Vec::new();
        let mut stack: Vec<String> = children.to_vec();
        while let Some(cid) = stack.pop() {
            let c_children = self.children(&cid);
            if c_children.is_empty() {
                leaves.push(cid);
            } else {
                stack.extend(c_children.iter().cloned());
            }
        }
        leaves
    }

    /// Every category that lists `service_id` in its `services`.
    pub fn categories_of_service(&self, service_id: &str) -> Vec<String> {
        self.categories
            .iter()
            .filter(|(_, n)| n.services.iter().any(|s| s == service_id))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Path helpers over a parent map: ancestor chains and LCA depth.
#[derive(Clone, Debug, Default)]
pub struct TreeIndex {
    parent_map: HashMap<String, String>,
}

impl TreeIndex {
    pub fn new(taxonomy: &TaxonomyFile) -> Self {
        Self {
            parent_map: taxonomy.parent_map(),
        }
    }

    pub fn parent(&self, id: &str) -> Option<&str> {
        self.parent_map.get(id).map(String::as_str)
    }

    /// Root-first path `[root, ..., node_id]`.
    pub fn ancestors(&self, node_id: &str) -> Vec<String> {
        let mut path = Vec::new();
        let mut current = Some(node_id.to_string());
        while let Some(cur) = current {
            current = self.parent_map.get(&cur).cloned();
            path.push(cur);
        }
        path.reverse();
        path
    }

    /// Depth of the lowest common ancestor of two root-first paths.
    /// Returns -1 when the paths share nothing.
    pub fn lca_depth(path_a: &[String], path_b: &[String]) -> i64 {
        let mut depth: i64 = -1;
        for (a, b) in path_a.iter().zip(path_b.iter()) {
            if a != b {
                break;
            }
            depth += 1;
        }
        depth
    }
}

/// One entry of `class.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CategoryInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_rule: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CategoryInfo {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            description: Some(description.into()),
            ..Default::default()
        }
    }

    pub fn name_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(default)
    }

    pub fn description_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.description.as_deref().unwrap_or(default)
    }

    pub fn boundary_text(&self) -> &str {
        self.boundary.as_deref().unwrap_or("")
    }

    pub fn decision_rule_text(&self) -> &str {
        self.decision_rule.as_deref().unwrap_or("")
    }
}

/// `class.json`: category metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassFile {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub categories: IndexMap<String, CategoryInfo>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for ClassFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            categories: IndexMap::new(),
            extra: Map::new(),
        }
    }
}

impl ClassFile {
    pub fn load(path: &Path) -> Result<Self> {
        read_json(path)
    }

    pub fn info(&self, id: &str) -> Option<&CategoryInfo> {
        self.categories.get(id)
    }

    /// Display name, falling back to the id like the Python `.get('name', id)`.
    pub fn name_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.categories.get(id).map(|c| c.name_or(id)).unwrap_or(id)
    }

    pub fn description_of<'a>(&'a self, id: &'a str, default: &'a str) -> &'a str {
        self.categories
            .get(id)
            .map(|c| c.description_or(default))
            .unwrap_or(default)
    }
}

/// A tool reference inside a query's `correct_tools`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CorrectTool {
    #[serde(default)]
    pub id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One entry of `query.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryObject {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub correct_tools: Vec<CorrectTool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl QueryObject {
    pub fn expected_tools(&self) -> Vec<String> {
        self.correct_tools.iter().map(|t| t.id.clone()).collect()
    }
}

/// Load `query.json`, optionally truncating to `max_queries`.
pub fn load_queries(path: &Path, max_queries: Option<usize>) -> Result<Vec<QueryObject>> {
    let mut queries: Vec<QueryObject> = read_json(path)?;
    if let Some(n) = max_queries {
        queries.truncate(n);
    }
    Ok(queries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn service_round_trip_keeps_extra_fields() -> TestResult {
        let raw = json!({"id": "svc_1", "name": "Flights", "description": "Book", "inputSchema": {"a": 1}});
        let s: ServiceRecord = serde_json::from_value(raw.clone())?;
        assert_eq!(s.description_text(), "Book");
        assert_eq!(serde_json::to_value(&s)?, raw);
        let no_desc: ServiceRecord = serde_json::from_value(json!({"id": "x", "name": "n"}))?;
        assert_eq!(no_desc.description_or("No description"), "No description");
        Ok(())
    }

    #[test]
    fn taxonomy_helpers() -> TestResult {
        let t: TaxonomyFile = serde_json::from_value(json!({
            "version": "2.0-hierarchical", "root": "root", "build_status": "complete",
            "categories": {
                "root": {"children": ["a", "b"], "services": ["s0"]},
                "a": {"children": ["a1", "a2"], "services": []},
                "a1": {"children": [], "services": ["s1"]},
                "a2": {"children": [], "services": ["s1", "s2"]},
                "b": {"children": [], "services": ["s3"]}
            }
        }))?;
        assert_eq!(t.build_status.as_deref(), Some("complete"));
        let mut leaves = t.leaves_under("root");
        leaves.sort();
        assert_eq!(leaves, vec!["a1", "a2", "b"]);
        assert_eq!(t.leaves_under("b"), vec!["b"]);
        assert_eq!(t.categories_of_service("s1"), vec!["a1", "a2"]);
        let idx = TreeIndex::new(&t);
        assert_eq!(idx.ancestors("a2"), vec!["root", "a", "a2"]);
        assert_eq!(
            TreeIndex::lca_depth(&idx.ancestors("a1"), &idx.ancestors("a2")),
            1
        );
        assert_eq!(TreeIndex::lca_depth(&idx.ancestors("a1"), &idx.ancestors("b")), 0);
        assert_eq!(TreeIndex::lca_depth(&[], &idx.ancestors("b")), -1);
        let back = serde_json::to_value(&t)?;
        assert_eq!(back["categories"]["a"]["children"], json!(["a1", "a2"]));
        Ok(())
    }

    #[test]
    fn class_file_defaults() -> TestResult {
        let c: ClassFile = serde_json::from_value(
            json!({"version": "2.0-hierarchical", "categories": {"x": {"name": "X"}}}),
        )?;
        assert_eq!(c.name_of("x"), "X");
        assert_eq!(c.name_of("missing"), "missing");
        assert_eq!(c.description_of("x", "No description"), "No description");
        let v = serde_json::to_value(&c)?;
        assert_eq!(v["categories"]["x"], json!({"name": "X"}));
        Ok(())
    }
}
