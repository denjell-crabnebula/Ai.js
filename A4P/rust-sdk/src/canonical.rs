// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON used by A4P signatures and commitments.
//!
//! The output matches Python
//! `json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)`
//! byte for byte: object keys sorted by code point, no whitespace, raw UTF-8,
//! integers printed as integers and floats printed with Python `repr` rules.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::errors::A4PError;
use crate::types::JsonDict;

/// Python error text for non-finite floats.
pub const OUT_OF_RANGE_FLOAT: &str = "Out of range float values are not JSON compliant";

/// Return the canonical JSON text for a JSON object.
pub fn canonical_json(payload: &JsonDict) -> Result<String, A4PError> {
    let mut out = String::new();
    write_object(payload, &mut out)?;
    Ok(out)
}

/// Return the canonical JSON text for any JSON value.
pub fn canonical_json_value(value: &Value) -> Result<String, A4PError> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_object(map: &JsonDict, out: &mut String) -> Result<(), A4PError> {
    let sorted: BTreeMap<&str, &Value> = map.iter().map(|(key, value)| (key.as_str(), value)).collect();
    out.push('{');
    let mut first = true;
    for (key, value) in sorted {
        if !first {
            out.push(',');
        }
        first = false;
        write_string(key, out);
        out.push(':');
        write_value(value, out)?;
    }
    out.push('}');
    Ok(())
}

fn write_value(value: &Value, out: &mut String) -> Result<(), A4PError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                out.push_str(&int.to_string());
            } else if let Some(int) = number.as_u64() {
                out.push_str(&int.to_string());
            } else if let Some(float) = number.as_f64() {
                out.push_str(&python_float_repr(float)?);
            } else {
                return Err(A4PError::value(OUT_OF_RANGE_FLOAT));
            }
        }
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => write_object(map, out)?,
    }
    Ok(())
}

fn write_string(text: &str, out: &mut String) {
    // serde_json escapes exactly the same set as Python with ensure_ascii=False:
    // quote, backslash and control characters below 0x20, using lowercase hex.
    match serde_json::to_string(text) {
        Ok(encoded) => out.push_str(&encoded),
        Err(_) => {
            out.push('"');
            out.push_str(text);
            out.push('"');
        }
    }
}

/// Format a float exactly like Python `float.__repr__`.
///
/// Python uses the shortest round-trip digits, fixed notation when the decimal
/// exponent is between -4 and 16, and `e-05` / `e+16` style exponents otherwise.
pub fn python_float_repr(value: f64) -> Result<String, A4PError> {
    if !value.is_finite() {
        return Err(A4PError::value(OUT_OF_RANGE_FLOAT));
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        });
    }
    // Rust `{:e}` prints the shortest round-trip mantissa, for example "1.5e-7".
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    // decpt is the position of the decimal point relative to the digit string.
    let decpt = exponent + 1;
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let exp = decpt - 1;
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(digits);
    } else if (decpt as usize) >= digits.len() {
        out.push_str(digits);
        for _ in 0..(decpt as usize - digits.len()) {
            out.push('0');
        }
        out.push_str(".0");
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_and_keeps_utf8() -> TestResult {
        let payload = json!({"b": [1, {"z": "值", "a": true}], "a": null, "中": "x\"\n\u{1}"});
        let text = canonical_json(payload.as_object().required()?)?;
        assert_eq!(
            text,
            "{\"a\":null,\"b\":[1,{\"a\":true,\"z\":\"值\"}],\"中\":\"x\\\"\\n\\u0001\"}"
        );
        Ok(())
    }

    #[test]
    fn floats_match_python_repr() -> TestResult {
        let cases = [
            (1.0, "1.0"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (-2.5, "-2.5"),
            (1e22, "1e+22"),
            (0.0, "0.0"),
            (100.0, "100.0"),
            (12345.678, "12345.678"),
        ];
        for (value, expected) in cases {
            assert_eq!(python_float_repr(value)?, expected, "{value}");
        }
        assert!(
            python_float_repr(f64::NAN)
                .err_or_fail()?
                .to_string()
                .contains("Out of range float")
        );
        assert!(python_float_repr(f64::INFINITY).is_err());
        Ok(())
    }

    #[test]
    fn integers_print_as_integers() -> TestResult {
        let payload = json!({"n": 2000, "neg": -3, "big": 9007199254740993_i64, "f": 2.0});
        let text = canonical_json(payload.as_object().required()?)?;
        assert_eq!(text, "{\"big\":9007199254740993,\"f\":2.0,\"n\":2000,\"neg\":-3}");
        Ok(())
    }
}
