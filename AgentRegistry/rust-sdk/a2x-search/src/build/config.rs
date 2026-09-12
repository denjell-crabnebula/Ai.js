// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Configuration for fully automatic hierarchical taxonomy building.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::util::write_json;

/// Fields that do not affect build results (excluded from config comparison).
const NON_BUILD_FIELDS: &[&str] = &["output_dir", "workers", "service_hash"];

/// Configuration for the auto-hierarchical taxonomy builder.
///
/// Field names and defaults match the Python dataclass, and the struct is
/// what `build_config.json` contains.
///
/// Convention: the parent directory of `service_path` is the dataset
/// name, and `output_dir` defaults to `database/{dataset_name}/taxonomy`.
/// Use [`AutoHierarchicalConfig::new`] to get that default; `Default`
/// describes the `ToolRet_clean` dataset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoHierarchicalConfig {
    // Paths
    pub service_path: PathBuf,
    pub output_dir: PathBuf,

    // Keyword extraction
    pub keyword_batch_size: usize,
    pub max_keywords_per_service: usize,
    pub temperature_keywords: f32,

    // Category design
    pub temperature_design: f32,

    // Classify/refine
    /// Services matching more than this ratio of subcategories are "generic".
    pub generic_ratio: f64,
    /// Subcategories with at most this many services are deleted.
    pub delete_threshold: usize,
    pub classification_retries: u32,
    pub max_refine_iterations: usize,
    pub temperature_classify: f32,

    /// Nodes with more services than this use keyword-based design.
    pub keyword_threshold: usize,

    // Tree structure
    /// Max services per leaf node; larger leaves are split further.
    pub max_service_size: usize,
    /// Max subcategories per node split.
    pub max_categories_size: usize,
    /// Max recursion depth; `None` is unlimited.
    pub max_depth: Option<u32>,
    /// Min services for a leaf to be reported as not tiny.
    pub min_leaf_size: usize,

    // Parallelism
    pub workers: usize,

    // LLM max_tokens
    pub max_tokens_design: u32,
    pub max_tokens_design_small: u32,
    pub max_tokens_classify: u32,
    pub max_tokens_validate: u32,
    pub max_tokens_keywords: u32,

    // Cross-domain
    pub enable_cross_domain: bool,

    /// SHA256 of sorted (name, description) pairs, stamped at build end so
    /// the registry can detect a stale taxonomy. Not a build parameter.
    #[serde(default)]
    pub service_hash: Option<String>,
}

impl Default for AutoHierarchicalConfig {
    fn default() -> Self {
        Self::new("database/ToolRet_clean/service.json")
    }
}

impl AutoHierarchicalConfig {
    /// Defaults for `service_path`, with `output_dir` derived from the
    /// dataset name.
    pub fn new(service_path: impl Into<PathBuf>) -> Self {
        let service_path: PathBuf = service_path.into();
        let dataset_name = dataset_name_of(&service_path);
        Self {
            output_dir: PathBuf::from(format!("database/{dataset_name}/taxonomy")),
            service_path,
            keyword_batch_size: 50,
            max_keywords_per_service: 5,
            temperature_keywords: 0.0,
            temperature_design: 0.0,
            generic_ratio: 1.0 / 3.0,
            delete_threshold: 2,
            classification_retries: 2,
            max_refine_iterations: 3,
            temperature_classify: 0.0,
            keyword_threshold: 500,
            max_service_size: 40,
            max_categories_size: 20,
            max_depth: Some(3),
            min_leaf_size: 5,
            workers: 20,
            max_tokens_design: 6000,
            max_tokens_design_small: 4000,
            max_tokens_classify: 300,
            max_tokens_validate: 3000,
            max_tokens_keywords: 4000,
            enable_cross_domain: true,
            service_hash: None,
        }
    }

    pub fn with_output_dir(mut self, output_dir: impl Into<PathBuf>) -> Self {
        self.output_dir = output_dir.into();
        self
    }

    /// Dataset name derived from the parent directory of `service_path`.
    pub fn dataset_name(&self) -> String {
        dataset_name_of(&self.service_path)
    }

    /// Max subcategories per node split.
    pub fn get_max_categories(&self) -> usize {
        self.max_categories_size
    }

    /// Only the parameters that affect build results, as a JSON object.
    pub fn build_params(&self) -> serde_json::Map<String, Value> {
        let mut map = match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        for key in NON_BUILD_FIELDS {
            map.remove(*key);
        }
        map
    }

    /// True when the build parameters equal those in a saved
    /// `build_config.json`. `service_path` is compared by dataset name and
    /// floats with a `1e-3` tolerance.
    pub fn matches_saved_config(&self, saved_config_path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(saved_config_path) else {
            return false;
        };
        let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        for (key, value) in self.build_params() {
            let Some(saved_value) = saved.get(&key) else {
                return false;
            };
            if key == "service_path" {
                let saved_name = saved_value
                    .as_str()
                    .map(|s| dataset_name_of(Path::new(s)))
                    .unwrap_or_default();
                let current_name = value
                    .as_str()
                    .map(|s| dataset_name_of(Path::new(s)))
                    .unwrap_or_default();
                if saved_name != current_name {
                    return false;
                }
                continue;
            }
            if value.is_f64() {
                match (saved_value.as_f64(), value.as_f64()) {
                    (Some(a), Some(b)) if (a - b).abs() <= 1e-3 => {}
                    _ => return false,
                }
            } else if let (Some(a), Some(b)) = (saved_value.as_f64(), value.as_f64()) {
                if a != b {
                    return false;
                }
            } else if *saved_value != value {
                return false;
            }
        }
        true
    }

    /// Write `build_config.json`.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_json(path, self)
    }
}

/// Name of the parent directory of `service_path` (`""` when absent).
pub fn dataset_name_of(service_path: &Path) -> String {
    service_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn defaults_and_dataset_name() -> TestResult {
        let c = AutoHierarchicalConfig::default();
        assert_eq!(c.dataset_name(), "ToolRet_clean");
        assert_eq!(c.output_dir, PathBuf::from("database/ToolRet_clean/taxonomy"));
        assert_eq!(c.max_depth, Some(3));
        assert!((c.generic_ratio - 1.0 / 3.0).abs() < 1e-12);
        let params = c.build_params();
        assert!(!params.contains_key("workers"));
        assert!(!params.contains_key("output_dir"));
        assert!(!params.contains_key("service_hash"));
        assert!(params.contains_key("service_path"));
        assert_eq!(params.len(), 21);
        Ok(())
    }

    #[test]
    fn saved_config_comparison() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("build_config.json");
        let mut c = AutoHierarchicalConfig::new("/abs/path/DS/service.json");
        c.generic_ratio = 0.333;
        c.save(&path)?;
        let text = std::fs::read_to_string(&path)?;
        assert!(text.contains("\"service_hash\": null"));

        let mut other = AutoHierarchicalConfig::new("relative/DS/service.json");
        other.workers = 3;
        other.output_dir = PathBuf::from("elsewhere");
        assert!(other.matches_saved_config(&path));
        other.generic_ratio = 0.5;
        assert!(!other.matches_saved_config(&path));
        let mut third = AutoHierarchicalConfig::new("x/OTHER/service.json");
        third.generic_ratio = 0.333;
        assert!(!third.matches_saved_config(&path));
        assert!(!c.matches_saved_config(Path::new("/nonexistent.json")));
        Ok(())
    }
}
