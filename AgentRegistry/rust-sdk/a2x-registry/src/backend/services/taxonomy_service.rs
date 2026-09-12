// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Taxonomy tree reader for the front end visualization.
//!
//! Builds a nested tree from `taxonomy/taxonomy.json` and
//! `taxonomy/class.json`, cached in memory per dataset.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

/// Cached taxonomy trees keyed by dataset.
pub struct TaxonomyService {
    database_dir: PathBuf,
    cache: Mutex<HashMap<String, Value>>,
}

impl TaxonomyService {
    pub fn new(database_dir: impl Into<PathBuf>) -> Self {
        Self {
            database_dir: database_dir.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Load the tree for `dataset`. Errors when the files are missing.
    pub fn get_taxonomy_tree(&self, dataset: &str) -> std::io::Result<Value> {
        if let Some(t) = self.cache.lock().get(dataset) {
            return Ok(t.clone());
        }
        let ds_dir = self.database_dir.join(dataset).join("taxonomy");
        let tree = build_tree(&ds_dir.join("taxonomy.json"), &ds_dir.join("class.json"))?;
        self.cache.lock().insert(dataset.to_string(), tree.clone());
        Ok(tree)
    }

    /// Forget a cached tree (after a rebuild or delete).
    pub fn invalidate(&self, dataset: &str) {
        self.cache.lock().remove(dataset);
    }
}

fn read(path: &Path) -> std::io::Result<Value> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}

fn build_tree(taxonomy_path: &Path, class_path: &Path) -> std::io::Result<Value> {
    let taxonomy = read(taxonomy_path)?;
    let classes = read(class_path)?
        .get("categories")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let categories = taxonomy
        .get("categories")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let root_id = taxonomy
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or("root")
        .to_string();
    Ok(build_node(&root_id, &categories, &classes))
}

fn build_node(cat_id: &str, categories: &Map<String, Value>, classes: &Map<String, Value>) -> Value {
    let cat_data = categories
        .get(cat_id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cat_class = classes
        .get(cat_id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let children_ids: Vec<String> = cat_data
        .get("children")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let service_ids: Vec<Value> = cat_data
        .get("services")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut node = Map::new();
    node.insert("id".into(), Value::String(cat_id.into()));
    node.insert(
        "name".into(),
        cat_class
            .get("name")
            .cloned()
            .unwrap_or(Value::String(cat_id.into())),
    );
    node.insert(
        "description".into(),
        cat_class
            .get("description")
            .cloned()
            .unwrap_or(Value::String(String::new())),
    );
    node.insert("service_count".into(), json!(service_ids.len()));
    node.insert(
        "children".into(),
        Value::Array(
            children_ids
                .iter()
                .map(|c| build_node(c, categories, classes))
                .collect(),
        ),
    );
    if !service_ids.is_empty() && children_ids.is_empty() {
        node.insert("services".into(), Value::Array(service_ids));
    }
    Value::Object(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn builds_nested_tree() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let tax = tmp.path().join("ds/taxonomy");
        std::fs::create_dir_all(&tax)?;
        std::fs::write(
            tax.join("taxonomy.json"),
            json!({"root": "root", "categories": {
                "root": {"children": ["c1"], "services": []},
                "c1": {"children": [], "services": ["s1", "s2"]}
            }})
            .to_string(),
        )?;
        std::fs::write(
            tax.join("class.json"),
            json!({"categories": {"c1": {"name": "Cat", "description": "d"}}}).to_string(),
        )?;
        let svc = TaxonomyService::new(tmp.path());
        let tree = svc.get_taxonomy_tree("ds")?;
        assert_eq!(tree["name"], "root");
        assert_eq!(tree["children"][0]["name"], "Cat");
        assert_eq!(tree["children"][0]["service_count"], 2);
        assert_eq!(tree["children"][0]["services"], json!(["s1", "s2"]));
        assert!(tree.get("services").is_none());
        assert!(svc.get_taxonomy_tree("nope").is_err());
        Ok(())
    }
}
