// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Small helpers for `serde_json` values.

use serde_json::{Map, Value};

/// The map inside a JSON object value. Any other value yields an empty map,
/// so use it where the value is known to be an object, such as a
/// `json!({ ... })` literal.
pub fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// A borrowed view of the map inside a JSON object value, or `None`.
pub fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_extracts_maps() {
        assert_eq!(object(json!({"a": 1})).len(), 1);
        assert!(object(json!([1])).is_empty());
        assert!(as_object(&json!(1)).is_none());
    }
}
