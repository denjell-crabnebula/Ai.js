// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Error analysis report for A2X evaluation results.
//!
//! Classifies each missed tool as
//!
//! - A: navigation failure (the tool's category was never visited)
//! - B: selection failure (a correct category was visited but the tool
//!   was not selected)
//!
//! and writes `error_analysis.json` and `error_analysis.md` next to
//! `evaluation_results.json`. Output shapes and the (Chinese) markdown
//! headings match the Python report.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::taxonomy::{ClassFile, ROOT_ID, ServicesIndex, TaxonomyFile, index_services, load_services};
use crate::util::{read_json, write_json};

/// Load taxonomy, class data and services named in `config.json`.
pub fn load_taxonomy_and_services(results_dir: &Path) -> Result<(TaxonomyFile, ClassFile, ServicesIndex)> {
    let config_path = results_dir.join("config.json");
    let config: Value = read_json(&config_path)?;
    let path_of = |key: &str| -> Result<PathBuf> {
        config
            .get(key)
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "{} has no dataset paths ('{key}'); rerun the evaluation",
                    config_path.display()
                ))
            })
    };
    let taxonomy = TaxonomyFile::load(&path_of("taxonomy_path")?)?;
    let classes = ClassFile::load(&path_of("class_path")?)?;
    let services = index_services(load_services(&path_of("service_path")?)?);
    Ok((taxonomy, classes, services))
}

/// Root-first path to `cat_id`, always starting with `root`.
pub fn get_category_path(cat_id: &str, parent_map: &HashMap<String, String>) -> Vec<String> {
    let mut path = vec![cat_id.to_string()];
    let mut current = cat_id.to_string();
    while let Some(p) = parent_map.get(&current) {
        current = p.clone();
        path.insert(0, current.clone());
    }
    if path[0] != ROOT_ID {
        path.insert(0, ROOT_ID.to_string());
    }
    path
}

fn category_info(cat_id: &str, classes: &ClassFile) -> Value {
    json!({
        "id": cat_id,
        "name": classes.name_of(cat_id).to_string().replace(cat_id, classes.categories.get(cat_id).and_then(|c| c.name.as_deref()).unwrap_or("Unknown")),
        "description": classes.description_of(cat_id, "No description"),
    })
}

fn category_name_or_unknown<'a>(cat_id: &str, classes: &'a ClassFile) -> &'a str {
    classes
        .categories
        .get(cat_id)
        .and_then(|c| c.name.as_deref())
        .unwrap_or("Unknown")
}

fn format_category_path(path: &[String], classes: &ClassFile) -> String {
    path.iter()
        .map(|id| format!("{id}({})", category_name_or_unknown(id, classes)))
        .collect::<Vec<_>>()
        .join(" → ")
}

fn tool_info(tool_id: &str, services: &ServicesIndex) -> (String, String) {
    match services.get(tool_id) {
        Some(s) => (s.name.clone(), s.description_or("No description").to_string()),
        None => ("Unknown".into(), "No description".into()),
    }
}

fn cut(text: &str, n: usize) -> String {
    if text.chars().count() > n {
        format!("{}...", text.chars().take(n).collect::<String>())
    } else {
        text.to_string()
    }
}

/// Classify missed tools into navigation (A) and selection (B) failures.
pub fn classify_error(
    missed_tools: &[String],
    visited_category_ids: &[String],
    taxonomy: &TaxonomyFile,
) -> (String, Vec<String>, Vec<String>) {
    let visited: HashSet<&String> = visited_category_ids.iter().collect();
    let mut not_visited = Vec::new();
    let mut visited_but_missed = Vec::new();
    for tool_id in missed_tools {
        let cats = taxonomy.categories_of_service(tool_id);
        if cats.is_empty() {
            not_visited.push(tool_id.clone());
            continue;
        }
        if cats.iter().any(|c| visited.contains(c)) {
            visited_but_missed.push(tool_id.clone());
        } else {
            not_visited.push(tool_id.clone());
        }
    }
    let error_type = if !not_visited.is_empty() && !visited_but_missed.is_empty() {
        "mixed"
    } else if !not_visited.is_empty() {
        "A_navigation_failure"
    } else {
        "B_selection_failure"
    };
    (error_type.to_string(), not_visited, visited_but_missed)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Generate the error analysis document from `evaluation_results.json`.
pub fn generate_error_report(results_path: &Path) -> Result<Value> {
    let results_dir = results_path.parent().unwrap_or(Path::new("."));
    let results: Value = read_json(results_path)?;
    let (taxonomy, classes, services) = load_taxonomy_and_services(results_dir)?;
    let parent_map = taxonomy.parent_map();
    let empty = Vec::new();
    let query_metrics = results
        .get("query_metrics")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut summary = json!({
        "total_queries": query_metrics.len(),
        "perfect_queries": 0,
        "error_queries": 0,
        "category_A_count": 0,
        "category_B_count": 0,
        "mixed_count": 0,
        "total_missed_tools": 0,
        "total_wrong_tools": 0,
    });
    let mut error_categories = json!({
        "A_navigation_failure": [],
        "B_selection_failure": [],
        "mixed": [],
    });
    let mut detailed_errors: Vec<Value> = Vec::new();

    let bump = |summary: &mut Value, key: &str, by: u64| {
        let cur = summary[key].as_u64().unwrap_or(0);
        summary[key] = json!(cur + by);
    };

    for qm in query_metrics {
        let recall = qm.get("recall").and_then(Value::as_f64).unwrap_or(0.0);
        let missed_tools = strings(qm.get("missed_tools"));
        let wrong_tools = strings(qm.get("wrong_tools"));
        bump(&mut summary, "total_missed_tools", missed_tools.len() as u64);
        bump(&mut summary, "total_wrong_tools", wrong_tools.len() as u64);
        if recall == 1.0 {
            bump(&mut summary, "perfect_queries", 1);
            continue;
        }
        bump(&mut summary, "error_queries", 1);

        let query_id = qm.get("query_id").and_then(Value::as_str).unwrap_or("");
        let query_text = qm.get("query").and_then(Value::as_str).unwrap_or("");
        let visited_ids = strings(qm.get("visited_category_ids"));
        let expected_tools = strings(qm.get("expected_tools"));
        let correct_tools = strings(qm.get("correct_tools"));

        let (error_type, tools_a, tools_b) = classify_error(&missed_tools, &visited_ids, &taxonomy);
        match error_type.as_str() {
            "A_navigation_failure" => bump(&mut summary, "category_A_count", 1),
            "B_selection_failure" => bump(&mut summary, "category_B_count", 1),
            _ => bump(&mut summary, "mixed_count", 1),
        }

        let mut correct_detail = Vec::new();
        for tool_id in &expected_tools {
            let (name, desc) = tool_info(tool_id, &services);
            let cats = taxonomy.categories_of_service(tool_id);
            let cat_infos: Vec<Value> = cats.iter().map(|c| category_info(c, &classes)).collect();
            let cat_paths: Vec<Value> = cats
                .iter()
                .map(|c| {
                    let path = get_category_path(c, &parent_map);
                    json!({
                        "path": path,
                        "path_str": format_category_path(&path, &classes),
                        "description": classes.description_of(c, "No description"),
                    })
                })
                .collect();
            correct_detail.push(json!({
                "tool_id": tool_id,
                "tool_name": name,
                "tool_description": cut(&desc, 200),
                "categories": cat_infos,
                "category_paths": cat_paths,
                "was_found": correct_tools.contains(tool_id),
            }));
        }

        let missed_a: Vec<Value> = tools_a
            .iter()
            .map(|tool_id| {
                let (name, desc) = tool_info(tool_id, &services);
                let cats = taxonomy.categories_of_service(tool_id);
                json!({
                    "tool_id": tool_id,
                    "tool_name": name,
                    "tool_description": cut(&desc, 200),
                    "correct_categories": cats.iter().map(|c| category_info(c, &classes)).collect::<Vec<_>>(),
                    "reason": "Category not visited during navigation",
                })
            })
            .collect();
        let visited_set: HashSet<&String> = visited_ids.iter().collect();
        let missed_b: Vec<Value> = tools_b
            .iter()
            .map(|tool_id| {
                let (name, desc) = tool_info(tool_id, &services);
                let cats = taxonomy.categories_of_service(tool_id);
                let visited_correct: Vec<&String> = cats.iter().filter(|c| visited_set.contains(c)).collect();
                json!({
                    "tool_id": tool_id,
                    "tool_name": name,
                    "tool_description": cut(&desc, 200),
                    "correct_categories": cats.iter().map(|c| category_info(c, &classes)).collect::<Vec<_>>(),
                    "visited_correct_categories": visited_correct,
                    "reason": "Category visited but tool not selected",
                })
            })
            .collect();

        let visited_cat_paths: Vec<Value> = visited_ids
            .iter()
            .map(|cat_id| {
                let path = get_category_path(cat_id, &parent_map);
                json!({
                    "cat_id": cat_id,
                    "path": path,
                    "path_str": format_category_path(&path, &classes),
                    "description": cut(classes.description_of(cat_id, "No description"), 200),
                })
            })
            .collect();

        let wrong_detail: Vec<Value> = wrong_tools
            .iter()
            .take(10)
            .map(|tool_id| {
                let (name, desc) = tool_info(tool_id, &services);
                let cats = taxonomy.categories_of_service(tool_id);
                json!({
                    "tool_id": tool_id,
                    "tool_name": name,
                    "tool_description": cut(&desc, 150),
                    "categories": cats.iter().take(3).map(|c| category_info(c, &classes)).collect::<Vec<_>>(),
                })
            })
            .collect();

        let mut entry = Map::new();
        entry.insert("query_id".into(), json!(query_id));
        entry.insert("query".into(), json!(query_text));
        entry.insert("recall".into(), json!(recall));
        entry.insert(
            "precision".into(),
            qm.get("precision").cloned().unwrap_or(json!(0)),
        );
        entry.insert("error_type".into(), json!(error_type));
        entry.insert("visited_categories".into(), json!(visited_ids));
        entry.insert("correct_tools_detail".into(), json!(correct_detail));
        entry.insert("missed_tools_A_navigation".into(), json!(missed_a));
        entry.insert("missed_tools_B_selection".into(), json!(missed_b));
        entry.insert("wrong_tools_detail".into(), json!(wrong_detail));
        entry.insert("visited_category_paths".into(), json!(visited_cat_paths));
        if wrong_tools.len() > 10 {
            entry.insert("wrong_tools_count".into(), json!(wrong_tools.len()));
            entry.insert("wrong_tools_truncated".into(), json!(true));
        }

        if let Some(list) = error_categories[&error_type].as_array_mut() {
            list.push(json!({
                "query_id": query_id,
                "query": cut(query_text, 100),
                "recall": recall,
                "missed_count": missed_tools.len(),
                "wrong_count": wrong_tools.len(),
            }));
        }
        detailed_errors.push(Value::Object(entry));
    }

    Ok(json!({
        "summary": summary,
        "error_categories": error_categories,
        "detailed_errors": detailed_errors,
    }))
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Render the analysis as the markdown report.
pub fn format_report_markdown(analysis: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    let summary = &analysis["summary"];
    let n = |key: &str| summary[key].as_u64().unwrap_or(0);

    lines.push("# Error Analysis Report\n".into());
    lines.push("## Summary\n".into());
    lines.push("| Metric | Value |".into());
    lines.push("|--------|-------|".into());
    lines.push(format!("| Total Queries | {} |", n("total_queries")));
    lines.push(format!(
        "| Perfect Queries (Recall=1) | {} |",
        n("perfect_queries")
    ));
    lines.push(format!("| Error Queries (Recall<1) | {} |", n("error_queries")));
    lines.push(format!(
        "| Category A (Navigation Failure) | {} |",
        n("category_A_count")
    ));
    lines.push(format!(
        "| Category B (Selection Failure) | {} |",
        n("category_B_count")
    ));
    lines.push(format!("| Mixed (A+B) | {} |", n("mixed_count")));
    lines.push(String::new());
    lines.push("## Error Categories\n".into());
    lines.push("- **Category A (Navigation Failure)**: 没有进入正确分类".into());
    lines.push("- **Category B (Selection Failure)**: 进入正确分类但没有选择正确工具".into());
    lines.push(String::new());

    let detailed = arr(analysis, "detailed_errors");
    let by_type = |t: &str| -> Vec<&Value> { detailed.iter().filter(|e| s(e, "error_type") == t).collect() };

    let push_correct_tool = |lines: &mut Vec<String>, tool: &Value, with_paths: bool| {
        lines.push("**正确工具**:".into());
        lines.push(format!("- ID: `{}`", s(tool, "tool_id")));
        lines.push(format!("- 名称: {}", s(tool, "tool_name")));
        lines.push(format!("- 描述: {}", s(tool, "tool_description")));
        lines.push(String::new());
        if with_paths {
            let paths = arr(tool, "category_paths");
            if !paths.is_empty() {
                lines.push("**正确工具分类**:".into());
                for cp in paths {
                    lines.push(format!("- 路径: {}", s(cp, "path_str")));
                    let desc: String = s(cp, "description").chars().take(300).collect();
                    lines.push(format!("- 描述: {desc}..."));
                }
                lines.push(String::new());
            }
        } else {
            let cats = arr(tool, "categories");
            if !cats.is_empty() {
                lines.push("**正确工具分类**:".into());
                for cat in cats {
                    lines.push(format!("- ID: `{}`", s(cat, "id")));
                    lines.push(format!("- 名称: {}", s(cat, "name")));
                    let desc: String = s(cat, "description").chars().take(300).collect();
                    lines.push(format!("- 描述: {desc}..."));
                }
                lines.push(String::new());
            }
        }
    };

    let cat_a = by_type("A_navigation_failure");
    if !cat_a.is_empty() {
        lines.push("---".into());
        lines.push("# Category A: 没有进入正确分类\n".into());
        for (i, error) in cat_a.iter().enumerate() {
            lines.push(format!("## {}. {}\n", i + 1, s(error, "query_id")));
            lines.push(format!("**请求内容**: {}\n", s(error, "query")));
            for tool in arr(error, "correct_tools_detail") {
                if !tool["was_found"].as_bool().unwrap_or(false) {
                    push_correct_tool(&mut lines, tool, true);
                }
            }
            let visited = arr(error, "visited_category_paths");
            if !visited.is_empty() {
                lines.push("**进入的错误分类**:".into());
                let mut seen = HashSet::new();
                for cp in visited {
                    if seen.insert(s(cp, "cat_id")) {
                        lines.push(format!("- 路径: {}", s(cp, "path_str")));
                        lines.push(format!("- 描述: {}", s(cp, "description")));
                        lines.push(String::new());
                    }
                }
            } else {
                lines.push("**进入的错误分类**: 无\n".into());
            }
            lines.push("---\n".into());
        }
    }

    let cat_b = by_type("B_selection_failure");
    if !cat_b.is_empty() {
        lines.push("---".into());
        lines.push("# Category B: 进入正确分类但没有选择正确工具\n".into());
        for (i, error) in cat_b.iter().enumerate() {
            lines.push(format!("## {}. {}\n", i + 1, s(error, "query_id")));
            lines.push(format!("**请求内容**: {}\n", s(error, "query")));
            let visited: Vec<String> = arr(error, "visited_categories")
                .iter()
                .filter_map(Value::as_str)
                .map(|v| format!("'{v}'"))
                .collect();
            lines.push(format!("**访问过的分类**: [{}]\n", visited.join(", ")));
            for tool in arr(error, "correct_tools_detail") {
                if !tool["was_found"].as_bool().unwrap_or(false) {
                    push_correct_tool(&mut lines, tool, false);
                }
            }
            let wrong = arr(error, "wrong_tools_detail");
            if !wrong.is_empty() {
                lines.push("**错误工具**:".into());
                for tool in wrong.iter().take(3) {
                    lines.push(format!("- ID: `{}`", s(tool, "tool_id")));
                    lines.push(format!("- 名称: {}", s(tool, "tool_name")));
                    lines.push(format!("- 描述: {}", s(tool, "tool_description")));
                    if let Some(cat) = arr(tool, "categories").first() {
                        lines.push(format!("- 分类: `{}` ({})", s(cat, "id"), s(cat, "name")));
                    }
                    lines.push(String::new());
                }
                if wrong.len() > 3 {
                    lines.push(format!("... 还有 {} 个错误工具\n", wrong.len() - 3));
                }
            } else {
                lines.push("**错误工具**: 无\n".into());
            }
            lines.push("---\n".into());
        }
    }

    let mixed = by_type("mixed");
    if !mixed.is_empty() {
        lines.push("---".into());
        lines.push("# Mixed: 混合错误 (部分A + 部分B)\n".into());
        for (i, error) in mixed.iter().enumerate() {
            lines.push(format!("## {}. {}\n", i + 1, s(error, "query_id")));
            lines.push(format!("**请求内容**: {}\n", s(error, "query")));
            let missed_a = arr(error, "missed_tools_A_navigation");
            if !missed_a.is_empty() {
                lines.push("### A类错误 (没有进入正确分类):\n".into());
                for tool in missed_a {
                    lines.push("**正确工具**:".into());
                    lines.push(format!("- ID: `{}`", s(tool, "tool_id")));
                    lines.push(format!("- 名称: {}", s(tool, "tool_name")));
                    lines.push(format!("- 描述: {}", s(tool, "tool_description")));
                    let cats = arr(tool, "correct_categories");
                    if !cats.is_empty() {
                        lines.push("**正确工具分类**:".into());
                        for cat in cats {
                            lines.push(format!("- ID: `{}`", s(cat, "id")));
                            lines.push(format!("- 名称: {}", s(cat, "name")));
                        }
                    }
                    lines.push(String::new());
                }
            }
            let missed_b = arr(error, "missed_tools_B_selection");
            if !missed_b.is_empty() {
                lines.push("### B类错误 (进入正确分类但没选择):\n".into());
                for tool in missed_b {
                    lines.push("**正确工具**:".into());
                    lines.push(format!("- ID: `{}`", s(tool, "tool_id")));
                    lines.push(format!("- 名称: {}", s(tool, "tool_name")));
                    lines.push(format!("- 描述: {}", s(tool, "tool_description")));
                    let cats = arr(tool, "correct_categories");
                    if !cats.is_empty() {
                        lines.push("**正确工具分类**:".into());
                        for cat in cats {
                            lines.push(format!("- ID: `{}`", s(cat, "id")));
                            lines.push(format!("- 名称: {}", s(cat, "name")));
                        }
                    }
                    let visited: Vec<String> = arr(tool, "visited_correct_categories")
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|v| format!("'{v}'"))
                        .collect();
                    lines.push(format!("**已访问的正确分类**: [{}]", visited.join(", ")));
                    lines.push(String::new());
                }
            }
            let wrong = arr(error, "wrong_tools_detail");
            if !wrong.is_empty() {
                lines.push("**错误工具**:".into());
                for tool in wrong.iter().take(3) {
                    lines.push(format!("- ID: `{}`", s(tool, "tool_id")));
                    lines.push(format!("- 名称: {}", s(tool, "tool_name")));
                    if let Some(cat) = arr(tool, "categories").first() {
                        lines.push(format!("- 分类: `{}` ({})", s(cat, "id"), s(cat, "name")));
                    }
                    lines.push(String::new());
                }
            }
            lines.push("---\n".into());
        }
    }

    lines.join("\n")
}

/// Generate and save the report for a results directory. Returns the
/// markdown path.
pub fn save_error_report(results_dir: &Path) -> Result<PathBuf> {
    let results_path = results_dir.join("evaluation_results.json");
    if !results_path.exists() {
        return Err(Error::Invalid(format!(
            "No evaluation_results.json in {}",
            results_dir.display()
        )));
    }
    let analysis = generate_error_report(&results_path)?;
    write_json(&results_dir.join("error_analysis.json"), &analysis)?;
    let md_path = results_dir.join("error_analysis.md");
    std::fs::write(&md_path, format_report_markdown(&analysis)).map_err(|e| Error::io(&md_path, e))?;
    let summary = &analysis["summary"];
    tracing::info!("Analysis Summary:");
    tracing::info!("  Total Queries: {}", summary["total_queries"]);
    tracing::info!("  Error Queries: {}", summary["error_queries"]);
    tracing::info!("  Category A (Navigation): {}", summary["category_A_count"]);
    tracing::info!("  Category B (Selection): {}", summary["category_B_count"]);
    tracing::info!("  Mixed: {}", summary["mixed_count"]);
    Ok(md_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn category_path_and_classification() -> TestResult {
        let t: TaxonomyFile = serde_json::from_value(json!({
            "categories": {
                "root": {"children": ["a"], "services": []},
                "a": {"children": ["a1"], "services": []},
                "a1": {"children": [], "services": ["t1", "t2"]}
            }
        }))?;
        let pm = t.parent_map();
        assert_eq!(get_category_path("a1", &pm), vec!["root", "a", "a1"]);
        assert_eq!(get_category_path("orphan", &pm), vec!["root", "orphan"]);
        let (ty, a, b) = classify_error(&["t1".into(), "zz".into()], &["a1".into()], &t);
        assert_eq!(ty, "mixed");
        assert_eq!(a, vec!["zz"]);
        assert_eq!(b, vec!["t1"]);
        let (ty, _, _) = classify_error(&["t1".into()], &[], &t);
        assert_eq!(ty, "A_navigation_failure");
        let (ty, _, _) = classify_error(&[], &[], &t);
        assert_eq!(ty, "B_selection_failure");
        Ok(())
    }
}
