// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Small helpers shared across modules.
//!
//! [`pyjson`] renders JSON the way Python's `json.dumps` does so hashes and
//! log lines stay byte compatible. The value helpers mirror Python's
//! `str()`, `bool()` and `int()` coercions used by the original code.

pub mod pyjson;

use serde_json::Value;

/// Wall clock seconds since the Unix epoch as a float, like `time.time()`.
pub fn now_wall() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// UTC timestamp with second precision, like `2026-04-28T10:00:00Z`.
pub fn utcnow_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Python `str(value)` for a JSON value (`True`, `None`, repr for lists).
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => py_repr(value),
    }
}

/// Python `repr(value)` for a JSON value decoded by `json.loads`.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if s.contains('\'') && !s.contains('"') {
                format!("\"{}\"", s.replace('\\', "\\\\"))
            } else {
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
            }
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python truthiness of a JSON value.
pub fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `int(value)` for a JSON value. Returns `None` where Python would
/// raise `TypeError` or `ValueError`.
pub fn py_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64().map(|f| f.trunc() as i64)
            }
        }
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn python_string_coercions() -> TestResult {
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(["a", 1])), "['a', 1]");
        assert_eq!(py_str(&json!({"k": "v"})), "{'k': 'v'}");
        Ok(())
    }

    #[test]
    fn python_int_and_truthy() -> TestResult {
        assert_eq!(py_int(&json!(1.9)), Some(1));
        assert_eq!(py_int(&json!("7")), Some(7));
        assert_eq!(py_int(&json!("x")), None);
        assert_eq!(py_int(&json!([1])), None);
        assert!(py_truthy(&json!("a")));
        assert!(!py_truthy(&json!(0)));
        assert!(!py_truthy(&json!({})));
        Ok(())
    }
}
