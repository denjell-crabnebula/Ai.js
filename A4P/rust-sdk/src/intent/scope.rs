// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Intent action scope normalization and matching.

use serde_json::Value;

use crate::errors::A4PError;
use crate::types::JsonDict;
use crate::util::{as_int, str_trimmed};

/// Normalize an action `params` constraint: `"*"`, `null` (to `{}`) or an object.
pub fn normalize_params_constraint(value: Option<&Value>) -> Result<Value, A4PError> {
    match value {
        Some(Value::String(text)) if text == "*" => Ok(Value::String("*".into())),
        None | Some(Value::Null) => Ok(Value::Object(JsonDict::new())),
        Some(Value::Object(map)) => Ok(Value::Object(map.clone())),
        Some(_) => Err(A4PError::value("Action params must be an object or '*'")),
    }
}

/// Normalize one action spec into `{name, params, allowExtraParams}`.
pub fn normalize_action_spec(value: &Value) -> Result<JsonDict, A4PError> {
    let Value::Object(map) = value else {
        return Err(A4PError::value(
            "Intent actions must be objects with name and params",
        ));
    };
    let name = str_trimmed(map, "name");
    if name.is_empty() {
        return Err(A4PError::value("Intent action name missing"));
    }
    let allow_extra = match map.get("allowExtraParams") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err(A4PError::value(
                "Intent action allowExtraParams must be a boolean",
            ));
        }
    };
    let mut normalized = JsonDict::new();
    normalized.insert("name".into(), Value::String(name));
    normalized.insert("params".into(), normalize_params_constraint(map.get("params"))?);
    normalized.insert("allowExtraParams".into(), Value::Bool(allow_extra));
    Ok(normalized)
}

/// Normalize a non-empty list of action specs.
pub fn normalize_action_specs(actions: Option<&Value>) -> Result<Vec<JsonDict>, A4PError> {
    let Some(Value::Array(items)) = actions else {
        return Err(A4PError::value("Intent actions must be a list"));
    };
    let normalized = items
        .iter()
        .map(normalize_action_spec)
        .collect::<Result<Vec<_>, _>>()?;
    if normalized.is_empty() {
        return Err(A4PError::value("Intent actions must not be empty"));
    }
    Ok(normalized)
}

fn positive_int(value: Option<&Value>, field_name: &str) -> Result<i64, A4PError> {
    match value.and_then(as_int) {
        Some(number) if number > 0 => Ok(number),
        _ => Err(A4PError::value(format!(
            "{field_name} must be a positive integer"
        ))),
    }
}

/// Normalize `executionPolicy`; only `maxExecutions` is supported, other fields are dropped.
pub fn normalize_execution_policy(policy: Option<&Value>) -> Result<Option<JsonDict>, A4PError> {
    match policy {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => {
            let max_executions = map.get("maxExecutions");
            if max_executions.is_none() || max_executions == Some(&Value::Null) {
                return Err(A4PError::value("executionPolicy must include maxExecutions"));
            }
            let mut normalized = JsonDict::new();
            normalized.insert(
                "maxExecutions".into(),
                Value::from(positive_int(max_executions, "executionPolicy.maxExecutions")?),
            );
            Ok(Some(normalized))
        }
        Some(_) => Err(A4PError::value("Intent executionPolicy must be an object")),
    }
}

/// Normalize a full intent scope: `{actions, executionPolicy?}`.
pub fn normalize_intent_scope(intent: Option<&Value>) -> Result<JsonDict, A4PError> {
    let empty = JsonDict::new();
    let raw = match intent {
        Some(Value::Object(map)) => map,
        _ => &empty,
    };
    let actions = normalize_action_specs(raw.get("actions"))?;
    let mut normalized = JsonDict::new();
    normalized.insert(
        "actions".into(),
        Value::Array(actions.into_iter().map(Value::Object).collect()),
    );
    if raw.contains_key("executionPolicy") {
        let Some(policy) = normalize_execution_policy(raw.get("executionPolicy"))? else {
            return Err(A4PError::value("executionPolicy must include maxExecutions"));
        };
        normalized.insert("executionPolicy".into(), Value::Object(policy));
    }
    Ok(normalized)
}

/// Case sensitive glob matching with Python `fnmatch.fnmatchcase` semantics.
///
/// `*` matches any run of characters, `?` one character, `[seq]` a set with
/// ranges and `[!seq]` a negated set. Every other character is literal.
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    match_here(&name, &pattern)
}

fn match_here(name: &[char], pattern: &[char]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    match pattern[0] {
        '*' => {
            let rest = &pattern[1..];
            (0..=name.len()).any(|skip| match_here(&name[skip..], rest))
        }
        '?' => !name.is_empty() && match_here(&name[1..], &pattern[1..]),
        '[' => match parse_bracket(pattern) {
            Some((matcher, consumed)) => {
                !name.is_empty() && matcher(name[0]) && match_here(&name[1..], &pattern[consumed..])
            }
            None => !name.is_empty() && name[0] == '[' && match_here(&name[1..], &pattern[1..]),
        },
        literal => !name.is_empty() && name[0] == literal && match_here(&name[1..], &pattern[1..]),
    }
}

type CharMatcher = Box<dyn Fn(char) -> bool>;

/// Parse a `[...]` expression at the start of `pattern`, like `fnmatch.translate`.
fn parse_bracket(pattern: &[char]) -> Option<(CharMatcher, usize)> {
    let mut j = 1;
    if j < pattern.len() && pattern[j] == '!' {
        j += 1;
    }
    if j < pattern.len() && pattern[j] == ']' {
        j += 1;
    }
    while j < pattern.len() && pattern[j] != ']' {
        j += 1;
    }
    if j >= pattern.len() {
        return None;
    }
    let mut body = &pattern[1..j];
    let negate = body.first() == Some(&'!');
    if negate {
        body = &body[1..];
    }
    let mut singles: Vec<char> = Vec::new();
    let mut ranges: Vec<(char, char)> = Vec::new();
    let mut index = 0;
    while index < body.len() {
        if index + 2 < body.len() && body[index + 1] == '-' {
            let (start, end) = (body[index], body[index + 2]);
            if start <= end {
                ranges.push((start, end));
            }
            index += 3;
        } else {
            singles.push(body[index]);
            index += 1;
        }
    }
    let matcher: CharMatcher = Box::new(move |c: char| {
        let hit = singles.contains(&c) || ranges.iter().any(|(start, end)| *start <= c && c <= *end);
        hit != negate
    });
    Some((matcher, j + 1))
}

fn param_value_matches(actual_value: &Value, expected_value: &Value) -> bool {
    if expected_value == &Value::String("*".into()) {
        return true;
    }
    if let (Value::String(actual), Value::String(expected)) = (actual_value, expected_value) {
        return fnmatchcase(actual, expected);
    }
    actual_value == expected_value
}

/// Match an action call against an intent scope.
///
/// Same-name actions are tried in order (OR semantics) and the first mismatch
/// reason is reported when none of them matches.
pub fn params_match_intent_scope(
    intent: &JsonDict,
    action: &str,
    params: Option<&JsonDict>,
) -> Result<(), String> {
    let expected_action = action.trim();
    if expected_action.is_empty() {
        return Err("Expected action missing".into());
    }
    let actual_params = params.cloned().unwrap_or_default();
    let actions = normalize_action_specs(intent.get("actions")).map_err(|error| error.to_string())?;

    let mut first_mismatch_reason: Option<String> = None;
    for allowed in &actions {
        if allowed.get("name").and_then(Value::as_str) != Some(expected_action) {
            continue;
        }
        let constraint = match allowed.get("params") {
            Some(Value::String(text)) if text == "*" => return Ok(()),
            Some(Value::Object(map)) => map,
            _ => {
                first_mismatch_reason.get_or_insert_with(|| "Intent params constraint invalid".to_string());
                continue;
            }
        };
        let allow_extra = allowed
            .get("allowExtraParams")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut mismatch_reason: Option<String> = None;
        if !allow_extra {
            let mut extra: Vec<&String> = actual_params
                .keys()
                .filter(|key| !constraint.contains_key(*key))
                .collect();
            extra.sort();
            if !extra.is_empty() {
                let listed: Vec<String> = extra.iter().map(|key| format!("'{key}'")).collect();
                mismatch_reason = Some(format!(
                    "Unexpected params for action '{expected_action}': [{}]",
                    listed.join(", ")
                ));
            }
        }
        if mismatch_reason.is_none() {
            for (key, expected_value) in constraint {
                let Some(actual_value) = actual_params.get(key) else {
                    mismatch_reason = Some(format!(
                        "Required param '{key}' missing for action '{expected_action}'"
                    ));
                    break;
                };
                if !param_value_matches(actual_value, expected_value) {
                    mismatch_reason = Some(format!("Param '{key}' mismatch for action '{expected_action}'"));
                    break;
                }
            }
        }
        match mismatch_reason {
            None => return Ok(()),
            Some(reason) => {
                first_mismatch_reason.get_or_insert(reason);
            }
        }
    }

    if let Some(reason) = first_mismatch_reason {
        return Err(reason);
    }
    let names: Vec<String> = actions
        .iter()
        .map(|item| {
            format!(
                "'{}'",
                item.get("name").and_then(Value::as_str).unwrap_or_default()
            )
        })
        .collect();
    Err(format!(
        "Action '{expected_action}' not in token actions: [{}]",
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn fnmatch_follows_python_semantics() -> TestResult {
        assert!(fnmatchcase("example.md", "*.md"));
        assert!(!fnmatchcase("example.txt", "*.md"));
        assert!(fnmatchcase("note-1", "note-*"));
        assert!(fnmatchcase("v1.0", "v?.0"));
        assert!(!fnmatchcase("v12.0", "v?.0"));
        assert!(!fnmatchcase("example.md", "Example.MD"));
        assert!(fnmatchcase("b", "[abc]"));
        assert!(!fnmatchcase("d", "[abc]"));
        assert!(fnmatchcase("d", "[!abc]"));
        assert!(fnmatchcase("m", "[a-z]"));
        assert!(!fnmatchcase("M", "[a-z]"));
        assert!(fnmatchcase("]", "[]]"));
        assert!(fnmatchcase("[", "["));
        assert!(fnmatchcase("a[b", "a[b"));
        assert!(fnmatchcase("{a,b}", "{a,b}"));
        assert!(!fnmatchcase("a", "{a,b}"));
        assert!(fnmatchcase("a/b", "*"));
        assert!(fnmatchcase("", "*"));
        assert!(!fnmatchcase("", "?"));
        assert!(fnmatchcase("值-1", "值-*"));
        Ok(())
    }
}
