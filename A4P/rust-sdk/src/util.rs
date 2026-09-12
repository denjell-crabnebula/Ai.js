// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Small helpers that reproduce Python conventions used across the SDK.

use base64::Engine;
use base64::alphabet;
use base64::engine::general_purpose::GeneralPurposeConfig;
use base64::engine::{DecodePaddingMode, GeneralPurpose};
use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
use rand::RngCore;
use serde_json::Value;

use crate::types::JsonDict;

const B64URL_ENCODE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_encode_padding(false),
);

const B64URL_DECODE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Encode bytes as unpadded base64url.
pub fn b64url_encode(data: &[u8]) -> String {
    B64URL_ENCODE.encode(data)
}

/// Decode base64url with or without padding, like Python `urlsafe_b64decode` with padding added.
pub fn b64url_decode(data: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let trimmed = data.trim_end_matches('=');
    B64URL_DECODE.decode(trimmed.as_bytes())
}

/// Python `secrets.token_urlsafe(n)`: `n` random bytes as unpadded base64url.
pub fn token_urlsafe(n_bytes: usize) -> String {
    b64url_encode(&random_bytes(n_bytes))
}

/// Python `secrets.token_hex(n)`: `n` random bytes as lowercase hex.
pub fn token_hex(n_bytes: usize) -> String {
    hex::encode(random_bytes(n_bytes))
}

/// Cryptographically secure random bytes.
pub fn random_bytes(n_bytes: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n_bytes];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

/// Python truthiness of a JSON value.
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python `str(value)` for a JSON value.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Python `str(mapping.get(key) or "")`.
pub fn str_or_empty(map: &JsonDict, key: &str) -> String {
    match map.get(key) {
        Some(value) if is_truthy(value) => py_str(value),
        _ => String::new(),
    }
}

/// Python `str(mapping.get(key) or "").strip()`.
pub fn str_trimmed(map: &JsonDict, key: &str) -> String {
    str_or_empty(map, key).trim().to_string()
}

/// Return the nested object at `key`, or `None` when absent or not an object.
pub fn get_object<'a>(map: &'a JsonDict, key: &str) -> Option<&'a JsonDict> {
    map.get(key).and_then(Value::as_object)
}

/// Return a clone of the nested object at `key`, or an empty object.
pub fn object_or_empty(map: &JsonDict, key: &str) -> JsonDict {
    get_object(map, key).cloned().unwrap_or_default()
}

/// Python `dict(value or {})` for a value that should be an object.
pub fn dict_or_empty(value: Option<&Value>) -> JsonDict {
    value.and_then(Value::as_object).cloned().unwrap_or_default()
}

/// True when the JSON number is an integer and not a boolean.
pub fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        _ => None,
    }
}

/// Current UTC time as `%Y-%m-%dT%H:%M:%SZ`.
pub fn utc_now_iso() -> String {
    format_iso_z(now_epoch())
}

/// Current UNIX time in whole seconds.
pub fn now_epoch() -> i64 {
    Utc::now().timestamp()
}

/// Format a UNIX timestamp as `%Y-%m-%dT%H:%M:%SZ`.
pub fn format_iso_z(epoch: i64) -> String {
    match Utc.timestamp_opt(epoch, 0).single() {
        Some(time) => time.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        None => String::new(),
    }
}

/// Parse `%Y-%m-%dT%H:%M:%SZ` into a UNIX timestamp, like `calendar.timegm(time.strptime(...))`.
pub fn parse_iso_z(text: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%SZ").ok()?;
    Some(naive.and_utc().timestamp())
}

/// Format a UTC ISO timestamp as Beijing local time text.
pub fn format_beijing_display_time(utc_iso: &str) -> String {
    let beijing_time = parse_iso_z(utc_iso).and_then(|epoch| {
        let beijing = FixedOffset::east_opt(8 * 3600)?;
        beijing.timestamp_opt(epoch, 0).single()
    });
    match beijing_time {
        Some(time) => format!("{} 北京时间", time.format("%Y-%m-%d %H:%M:%S")),
        None => utc_iso.to_string(),
    }
}

/// Python `json.dumps(value, ensure_ascii=False, sort_keys=True)` with default separators.
pub fn python_json_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_python_json(value, &mut out);
    out
}

fn write_python_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_python_json(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_python_json(item, out);
            }
            out.push(']');
        }
        other => match crate::canonical::canonical_json_value(other) {
            Ok(text) => out.push_str(&text),
            Err(_) => out.push_str(&other.to_string()),
        },
    }
}

/// Return a copy of the JSON value with every object key sorted recursively.
pub fn sorted_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = JsonDict::new();
            for key in keys {
                sorted.insert(key.clone(), sorted_value(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_value).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use serde_json::json;

    #[test]
    fn base64url_round_trips_with_and_without_padding() -> TestResult {
        let encoded = b64url_encode(b"credential-id");
        assert_eq!(encoded, "Y3JlZGVudGlhbC1pZA");
        assert_eq!(b64url_decode(&encoded)?, b"credential-id");
        assert_eq!(b64url_decode("Y3JlZGVudGlhbC1pZA==")?, b"credential-id");
        assert_eq!(token_urlsafe(32).len(), 43);
        assert_eq!(token_hex(16).len(), 32);
        Ok(())
    }

    #[test]
    fn python_string_semantics() -> TestResult {
        let map = json!({"a": " x ", "b": 0, "c": 5, "d": true, "e": null})
            .as_object()
            .required()?
            .clone();
        assert_eq!(str_trimmed(&map, "a"), "x");
        assert_eq!(str_or_empty(&map, "b"), "");
        assert_eq!(str_or_empty(&map, "c"), "5");
        assert_eq!(str_or_empty(&map, "d"), "True");
        assert_eq!(str_or_empty(&map, "e"), "");
        assert_eq!(str_or_empty(&map, "missing"), "");
        Ok(())
    }

    #[test]
    fn time_helpers_round_trip() -> TestResult {
        assert_eq!(parse_iso_z("2026-07-22T04:00:00Z"), Some(1784692800));
        assert_eq!(format_iso_z(1784692800), "2026-07-22T04:00:00Z");
        assert_eq!(
            format_beijing_display_time("2026-07-22T04:00:00Z"),
            "2026-07-22 12:00:00 北京时间"
        );
        assert_eq!(format_beijing_display_time("bad"), "bad");
        assert!(parse_iso_z("not-a-timestamp").is_none());
        Ok(())
    }

    #[test]
    fn python_json_dumps_uses_default_separators() -> TestResult {
        let value = json!({"b": [1, "值"], "a": {"y": 1.5, "x": null}});
        assert_eq!(
            python_json_dumps(&value),
            "{\"a\": {\"x\": null, \"y\": 1.5}, \"b\": [1, \"值\"]}"
        );
        Ok(())
    }
}
