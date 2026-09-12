// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Curated default queries per dataset, read from
//! `database/{dataset}/query/default_queries.json`. A file of the form
//! `{"$ref": "relative/path.json"}` redirects (one level) to a shared list.

use std::path::Path;

use serde_json::Value;

/// Return `(queries, source)` where `source` is the resolved path relative
/// to `home` (forward slashes). Missing file: `([], "")`.
pub fn get_default_queries(home: &Path, database_dir: &Path, dataset: &str) -> (Vec<Value>, String) {
    let path = database_dir
        .join(dataset)
        .join("query")
        .join("default_queries.json");
    if !path.exists() {
        return (Vec::new(), String::new());
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Vec::new(), String::new());
    };
    let Ok(mut data) = serde_json::from_str::<Value>(&text) else {
        return (Vec::new(), String::new());
    };
    let mut source = path
        .strip_prefix(home)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
    if let Some(reference) = data.get("$ref").and_then(Value::as_str).map(str::to_string) {
        let ref_path = home.join(&reference);
        source = reference.replace('\\', "/");
        if !ref_path.exists() {
            return (Vec::new(), source);
        }
        data = std::fs::read_to_string(&ref_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
    }
    match data {
        Value::Array(items) => (items, source),
        _ => (Vec::new(), source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn reads_and_follows_ref() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path();
        let db = home.join("database");
        std::fs::create_dir_all(db.join("a/query"))?;
        std::fs::create_dir_all(db.join("b/query"))?;
        std::fs::write(
            db.join("a/query/default_queries.json"),
            json!([{"query": "q"}]).to_string(),
        )?;
        std::fs::write(
            db.join("b/query/default_queries.json"),
            json!({"$ref": "database/a/query/default_queries.json"}).to_string(),
        )?;
        let (q, src) = get_default_queries(home, &db, "a");
        assert_eq!(q.len(), 1);
        assert_eq!(src, "database/a/query/default_queries.json");
        let (q, src) = get_default_queries(home, &db, "b");
        assert_eq!(q.len(), 1);
        assert_eq!(src, "database/a/query/default_queries.json");
        assert_eq!(get_default_queries(home, &db, "c"), (Vec::new(), String::new()));
        Ok(())
    }
}
