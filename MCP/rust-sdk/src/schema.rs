// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Minimal JSON Schema validator used for tool input and output checks.
//!
//! The C++ SDK validates tool arguments and structured results with
//! `nlohmann::json-schema`. This module implements the subset of draft-07
//! that tool schemas use in practice: `type`, `properties`, `required`,
//! `additionalProperties`, `items`, `enum`, `const`, numeric and string
//! bounds, `pattern`, `minItems`, `maxItems`, `anyOf`, `oneOf`, `allOf`
//! and `not`. Unknown keywords are ignored, like a lenient validator.

use serde_json::Value;

/// Validate `instance` against `schema`. Returns the first violation found.
pub fn validate(schema: &Value, instance: &Value) -> Result<(), String> {
    validate_at(schema, instance, "")
}

fn path_or_root(path: &str) -> &str {
    if path.is_empty() { "<root>" } else { path }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn matches_type(expected: &str, v: &Value) -> bool {
    match expected {
        "null" => v.is_null(),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => match v {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
            }
            _ => false,
        },
        _ => true,
    }
}

fn validate_at(schema: &Value, instance: &Value, path: &str) -> Result<(), String> {
    let obj = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(format!("{}: schema forbids any value", path_or_root(path))),
        Value::Object(o) => o,
        _ => return Ok(()),
    };

    if let Some(t) = obj.get("type") {
        let ok = match t {
            Value::String(s) => matches_type(s, instance),
            Value::Array(list) => list
                .iter()
                .any(|x| x.as_str().map(|s| matches_type(s, instance)).unwrap_or(false)),
            _ => true,
        };
        if !ok {
            return Err(format!(
                "{}: expected type {} but found {}",
                path_or_root(path),
                t,
                type_name(instance)
            ));
        }
    }

    if let Some(Value::Array(values)) = obj.get("enum") {
        if !values.iter().any(|v| v == instance) {
            return Err(format!(
                "{}: value is not one of the allowed values",
                path_or_root(path)
            ));
        }
    }
    if let Some(c) = obj.get("const") {
        if c != instance {
            return Err(format!(
                "{}: value does not match the constant",
                path_or_root(path)
            ));
        }
    }

    if let Value::Number(n) = instance {
        let value = n.as_f64().unwrap_or(0.0);
        if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
            if value < min {
                return Err(format!(
                    "{}: {} is below minimum {}",
                    path_or_root(path),
                    value,
                    min
                ));
            }
        }
        if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
            if value > max {
                return Err(format!(
                    "{}: {} exceeds maximum {}",
                    path_or_root(path),
                    value,
                    max
                ));
            }
        }
        if let Some(min) = obj.get("exclusiveMinimum").and_then(Value::as_f64) {
            if value <= min {
                return Err(format!("{}: {} is not above {}", path_or_root(path), value, min));
            }
        }
        if let Some(max) = obj.get("exclusiveMaximum").and_then(Value::as_f64) {
            if value >= max {
                return Err(format!("{}: {} is not below {}", path_or_root(path), value, max));
            }
        }
    }

    if let Value::String(s) = instance {
        let len = s.chars().count();
        if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
            if (len as u64) < min {
                return Err(format!("{}: string shorter than {}", path_or_root(path), min));
            }
        }
        if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
            if (len as u64) > max {
                return Err(format!("{}: string longer than {}", path_or_root(path), max));
            }
        }
        if let Some(pattern) = obj.get("pattern").and_then(Value::as_str) {
            match regex::Regex::new(pattern) {
                Ok(re) => {
                    if !re.is_match(s) {
                        return Err(format!(
                            "{}: string does not match pattern {}",
                            path_or_root(path),
                            pattern
                        ));
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "{}: invalid pattern {}: {}",
                        path_or_root(path),
                        pattern,
                        e
                    ));
                }
            }
        }
    }

    if let Value::Array(items) = instance {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                return Err(format!(
                    "{}: array has fewer than {} items",
                    path_or_root(path),
                    min
                ));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (items.len() as u64) > max {
                return Err(format!(
                    "{}: array has more than {} items",
                    path_or_root(path),
                    max
                ));
            }
        }
        if let Some(item_schema) = obj.get("items") {
            for (i, item) in items.iter().enumerate() {
                validate_at(item_schema, item, &format!("{path}/{i}"))?;
            }
        }
    }

    if let Value::Object(fields) = instance {
        if let Some(Value::Array(required)) = obj.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !fields.contains_key(name) {
                    return Err(format!(
                        "{}: required property '{}' not found",
                        path_or_root(path),
                        name
                    ));
                }
            }
        }
        let properties = obj.get("properties").and_then(Value::as_object);
        if let Some(props) = properties {
            for (name, sub) in props {
                if let Some(v) = fields.get(name) {
                    validate_at(sub, v, &format!("{path}/{name}"))?;
                }
            }
        }
        if let Some(additional) = obj.get("additionalProperties") {
            for (name, v) in fields {
                let declared = properties.map(|p| p.contains_key(name)).unwrap_or(false);
                if declared {
                    continue;
                }
                match additional {
                    Value::Bool(false) => {
                        return Err(format!(
                            "{}: additional property '{}' is not allowed",
                            path_or_root(path),
                            name
                        ));
                    }
                    Value::Object(_) => validate_at(additional, v, &format!("{path}/{name}"))?,
                    _ => {}
                }
            }
        }
    }

    if let Some(Value::Array(all)) = obj.get("allOf") {
        for sub in all {
            validate_at(sub, instance, path)?;
        }
    }
    if let Some(Value::Array(any)) = obj.get("anyOf") {
        if !any.iter().any(|sub| validate_at(sub, instance, path).is_ok()) {
            return Err(format!(
                "{}: value does not match any schema in anyOf",
                path_or_root(path)
            ));
        }
    }
    if let Some(Value::Array(one)) = obj.get("oneOf") {
        let count = one
            .iter()
            .filter(|sub| validate_at(sub, instance, path).is_ok())
            .count();
        if count != 1 {
            return Err(format!(
                "{}: value matches {} schemas in oneOf",
                path_or_root(path),
                count
            ));
        }
    }
    if let Some(not) = obj.get("not") {
        if validate_at(not, instance, path).is_ok() {
            return Err(format!(
                "{}: value matches the forbidden schema",
                path_or_root(path)
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};
    use serde_json::json;

    #[test]
    fn required_and_types() -> TestResult {
        let schema = json!({"type": "object", "properties": {"user_query": {"type": "string"}},
            "required": ["user_query"]});
        assert!(validate(&schema, &json!({"user_query": "hi"})).is_ok());
        let err = validate(&schema, &json!({})).err_or_fail()?;
        assert!(err.contains("required property 'user_query'"));
        let err = validate(&schema, &json!({"user_query": 5})).err_or_fail()?;
        assert!(err.contains("expected type"));
        assert!(validate(&schema, &json!([1, 2])).is_err());
        Ok(())
    }

    #[test]
    fn numeric_and_string_bounds() -> TestResult {
        let schema = json!({"type": "integer", "minimum": 1, "maximum": 100});
        assert!(validate(&schema, &json!(50)).is_ok());
        assert!(validate(&schema, &json!(0)).is_err());
        assert!(validate(&schema, &json!(101)).is_err());
        assert!(validate(&schema, &json!(1.5)).is_err());
        let s = json!({"type": "string", "minLength": 2, "pattern": "^a"});
        assert!(validate(&s, &json!("ab")).is_ok());
        assert!(validate(&s, &json!("a")).is_err());
        assert!(validate(&s, &json!("bb")).is_err());
        Ok(())
    }

    #[test]
    fn arrays_enums_and_combinators() -> TestResult {
        let schema = json!({"type": "array", "items": {"type": "number"}, "minItems": 1});
        assert!(validate(&schema, &json!([1, 2.5])).is_ok());
        assert!(validate(&schema, &json!([])).is_err());
        assert!(validate(&schema, &json!(["x"])).is_err());
        let e = json!({"enum": ["a", "b"]});
        assert!(validate(&e, &json!("a")).is_ok());
        assert!(validate(&e, &json!("c")).is_err());
        let any = json!({"anyOf": [{"type": "string"}, {"type": "null"}]});
        assert!(validate(&any, &json!(null)).is_ok());
        assert!(validate(&any, &json!(1)).is_err());
        let one = json!({"oneOf": [{"type": "number"}, {"type": "integer"}]});
        assert!(validate(&one, &json!(1)).is_err());
        assert!(validate(&one, &json!(1.5)).is_ok());
        let not = json!({"not": {"type": "string"}});
        assert!(validate(&not, &json!("s")).is_err());
        assert!(validate(&not, &json!(2)).is_ok());
        Ok(())
    }

    #[test]
    fn additional_properties() -> TestResult {
        let schema = json!({"type": "object", "properties": {"a": {}}, "additionalProperties": false});
        assert!(validate(&schema, &json!({"a": 1})).is_ok());
        assert!(validate(&schema, &json!({"b": 1})).is_err());
        let typed = json!({"type": "object", "additionalProperties": {"type": "string"}});
        assert!(validate(&typed, &json!({"x": "y"})).is_ok());
        assert!(validate(&typed, &json!({"x": 1})).is_err());
        assert!(validate(&json!(true), &json!(1)).is_ok());
        assert!(validate(&json!(false), &json!(1)).is_err());
        Ok(())
    }
}
