// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Cross-platform atomic JSON file write.
//!
//! Writes to a temporary file next to the target and renames over it, which
//! is atomic on POSIX and Windows. A transient rename failure is retried once
//! and then falls back to a direct overwrite, matching the Python helper.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

/// Write `data` as pretty UTF-8 JSON to `path` atomically. The parent
/// directory must exist. A trailing newline is added for clean diffs.
pub fn atomic_write_json<T: Serialize>(path: &Path, data: &T) -> std::io::Result<()> {
    let mut content = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    content.push('\n');
    atomic_write_bytes(path, content.as_bytes())
}

/// Write raw bytes to `path` atomically.
pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp_path = {
        let mut name = path.file_name().map(|s| s.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        path.with_file_name(name)
    };
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            match std::fs::rename(&tmp_path, path) {
                Ok(()) => Ok(()),
                Err(_) => {
                    std::fs::write(path, content)?;
                    let _ = std::fs::remove_file(&tmp_path);
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn writes_pretty_json_with_newline() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        atomic_write_json(&path, &json!({"b": 1, "a": [1, 2]}))?;
        let text = std::fs::read_to_string(&path)?;
        assert!(text.ends_with('\n'));
        assert!(!dir.path().join("state.json.tmp").exists());
        let back: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(back["a"][1], 2);
        Ok(())
    }
}
