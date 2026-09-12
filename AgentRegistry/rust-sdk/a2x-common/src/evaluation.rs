// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared evaluation utilities across search methods.

use std::collections::HashSet;
use std::path::Path;

/// Precision, recall and hit derived from expected vs found id lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetMetrics {
    pub correct: Vec<String>,
    pub missed: Vec<String>,
    pub wrong: Vec<String>,
    pub precision: f64,
    pub recall: f64,
    pub hit: bool,
}

/// Compute precision, recall and hit from expected vs found tool lists.
/// Output vectors are sorted for deterministic results.
pub fn compute_set_metrics(expected: &[String], found: &[String]) -> SetMetrics {
    let expected_set: HashSet<&String> = expected.iter().collect();
    let found_set: HashSet<&String> = found.iter().collect();
    let mut correct: Vec<String> = expected_set
        .intersection(&found_set)
        .map(|s| (*s).clone())
        .collect();
    let mut missed: Vec<String> = expected_set
        .difference(&found_set)
        .map(|s| (*s).clone())
        .collect();
    let mut wrong: Vec<String> = found_set
        .difference(&expected_set)
        .map(|s| (*s).clone())
        .collect();
    correct.sort();
    missed.sort();
    wrong.sort();
    let precision = if found.is_empty() {
        0.0
    } else {
        correct.len() as f64 / found.len() as f64
    };
    let recall = if expected.is_empty() {
        0.0
    } else {
        correct.len() as f64 / expected.len() as f64
    };
    let hit = !correct.is_empty();
    SetMetrics {
        correct,
        missed,
        wrong,
        precision,
        recall,
        hit,
    }
}

/// Generate a standardized evaluation output directory path:
/// `results/{date}_{method}[-{mode}]_{dataset}[-{suffix}]_{count}`.
///
/// The dataset name is the parent directory of `service_path`, lowercased
/// with underscores removed. The query suffix comes from the query filename
/// when it is not `query.json`. When `max_queries` is `None` the count is
/// read from the query file (a JSON array).
pub fn generate_output_dir(
    method: &str,
    service_path: &Path,
    query_file: &Path,
    max_queries: Option<usize>,
    mode: Option<&str>,
) -> std::io::Result<String> {
    let date_str = chrono::Local::now().format("%Y%m%d").to_string();
    let method_tag = match mode {
        Some(m) => format!("{method}-{}", m.replace('_', "")),
        None => method.to_string(),
    };
    let dataset_name = service_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_lowercase().replace('_', ""))
        .unwrap_or_default();
    let query_stem = query_file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let query_suffix = if query_stem == "query" {
        String::new()
    } else {
        format!("-{}", query_stem.replace("query_", "").replace("query", ""))
    };
    let query_count = match max_queries {
        Some(n) if n > 0 => n,
        _ => {
            let text = std::fs::read_to_string(query_file)?;
            let v: serde_json::Value = serde_json::from_str(&text).map_err(std::io::Error::other)?;
            v.as_array().map(|a| a.len()).unwrap_or(0)
        }
    };
    Ok(format!(
        "results/{date_str}_{method_tag}_{dataset_name}{query_suffix}_{query_count}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn metrics() -> TestResult {
        let m = compute_set_metrics(&s(&["a", "b"]), &s(&["b", "c", "d"]));
        assert_eq!(m.correct, s(&["b"]));
        assert_eq!(m.missed, s(&["a"]));
        assert_eq!(m.wrong, s(&["c", "d"]));
        assert!((m.precision - 1.0 / 3.0).abs() < 1e-9);
        assert!((m.recall - 0.5).abs() < 1e-9);
        assert!(m.hit);
        let empty = compute_set_metrics(&[], &[]);
        assert_eq!(empty.precision, 0.0);
        assert!(!empty.hit);
        Ok(())
    }

    #[test]
    fn output_dir_naming() -> TestResult {
        let dir = tempfile::tempdir()?;
        let q = dir.path().join("query_cn.json");
        std::fs::write(&q, "[1,2,3]")?;
        let out = generate_output_dir(
            "a2x",
            &dir.path().join("ToolRet_clean").join("service.json"),
            &q,
            None,
            Some("get_one"),
        )?;
        assert!(out.starts_with("results/"));
        assert!(out.ends_with("_a2x-getone_toolretclean-cn_3"), "{out}");
        let out = generate_output_dir(
            "vector",
            Path::new("db/publicMCP/service.json"),
            Path::new("query.json"),
            Some(50),
            None,
        )?;
        assert!(out.ends_with("_vector_publicmcp_50"), "{out}");
        Ok(())
    }
}
