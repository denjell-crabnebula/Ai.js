// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! JSON rendering that matches Python's `json.dumps` defaults.
//!
//! Python separates items with `", "` and keys with `": "` in compact mode.
//! `serde_json` omits those spaces, so hashes computed over the text would
//! differ. This formatter reproduces the Python layout for the values that
//! the registry hashes or logs (`ensure_ascii=False`, no indent).

use serde::Serialize;
use serde_json::ser::Formatter;

/// Formatter producing `{"a": 1, "b": [1, 2]}` like Python.
pub struct PythonCompact;

impl Formatter for PythonCompact {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

/// Serialize `value` exactly like `json.dumps(value, ensure_ascii=False)`.
pub fn dumps<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PythonCompact);
    value.serialize(&mut ser)?;
    String::from_utf8(out).map_err(serde::ser::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn matches_python_layout() -> TestResult {
        let v = json!([["a", "b"], ["c", "d\n"]]);
        assert_eq!(dumps(&v)?, "[[\"a\", \"b\"], [\"c\", \"d\\n\"]]");
        let o = json!({"ts": "t", "event": "e", "by": null, "n": 1});
        assert_eq!(
            dumps(&o)?,
            "{\"ts\": \"t\", \"event\": \"e\", \"by\": null, \"n\": 1}"
        );
        assert_eq!(dumps(&json!("中文"))?, "\"中文\"");
        Ok(())
    }
}
