// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Conversions between `serde_json::Value` and JavaScript values.

use serde::Serialize;
use serde_json::{Map, Value};
use wasm_bindgen::prelude::*;

/// Serialize into a plain JavaScript value (objects, not `Map`s).
pub fn to_js<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, JsError> {
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    value
        .serialize(&serializer)
        .map_err(|e| JsError::new(&format!("cannot convert value to JavaScript: {e}")))
}

/// Deserialize any JavaScript value into JSON.
pub fn from_js(value: JsValue) -> Result<Value, JsError> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|e| JsError::new(&format!("cannot convert JavaScript value to JSON: {e}")))
}

/// Deserialize a JavaScript object into a JSON object map.
pub fn object_from_js(value: JsValue, what: &str) -> Result<Map<String, Value>, JsError> {
    match from_js(value)? {
        Value::Object(map) => Ok(map),
        other => Err(JsError::new(&format!(
            "{what} must be a JSON object, got {}",
            kind_name(&other)
        ))),
    }
}

/// Deserialize an optional JavaScript object (`undefined` and `null` map to `None`).
pub fn optional_object_from_js(value: JsValue, what: &str) -> Result<Option<Map<String, Value>>, JsError> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    object_from_js(value, what).map(Some)
}

fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
