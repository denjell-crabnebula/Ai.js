// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `test_cli_token_io.py` and `test_resolve_credentials.py`.
//!
//! The Python suite monkeypatches `Path.home()`; here every helper takes an
//! explicit path inside a temp dir instead.

use ap_support::testing::{OptionExt, TestResult};
use std::fs;
use std::path::PathBuf;

use a2x_registry_client::{
    CliToken, ClientError, DEFAULT_BASE_URL, read_cli_token, remove_cli_token, resolve_credentials,
    write_cli_token,
};

fn isolated_home() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let cfg = dir
        .path()
        .join("home")
        .join(".a2x_registry_client")
        .join("cli_token.json");
    Ok((dir, cfg))
}

#[test]
fn write_then_read_roundtrip() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    let path = write_cli_token("a2x_pat_token_xxxxx", "http://example/", Some(&cfg))?;
    assert!(path.exists());
    let read = read_cli_token(Some(&path)).required()?;
    assert_eq!(
        read,
        CliToken {
            base_url: Some("http://example/".into()),
            api_key: Some("a2x_pat_token_xxxxx".into())
        }
    );
    Ok(())
}

#[test]
fn write_creates_parent_dir() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    assert!(!cfg.parent().required()?.exists());
    write_cli_token(&"a2x_pat_x".repeat(6), "http://x/", Some(&cfg))?;
    assert!(cfg.exists());
    assert!(!cfg.with_file_name("cli_token.json.tmp").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_sets_0600_perms() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, cfg) = isolated_home()?;
    let path = write_cli_token(&"a2x_pat_x".repeat(6), "http://x/", Some(&cfg))?;
    let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
    Ok(())
}

#[test]
fn read_missing_returns_none() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    assert!(read_cli_token(Some(&cfg)).is_none());
    Ok(())
}

#[test]
fn remove_is_idempotent() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    assert!(!remove_cli_token(Some(&cfg)));
    write_cli_token(&"a2x_pat_x".repeat(6), "http://x/", Some(&cfg))?;
    assert!(remove_cli_token(Some(&cfg)));
    assert!(!remove_cli_token(Some(&cfg)));
    Ok(())
}

#[test]
fn write_rejects_empty_api_key() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    assert!(matches!(
        write_cli_token("", "http://x/", Some(&cfg)),
        Err(ClientError::InvalidArgument { .. })
    ));
    assert!(matches!(
        write_cli_token("   ", "http://x/", Some(&cfg)),
        Err(ClientError::InvalidArgument { .. })
    ));
    Ok(())
}

#[test]
fn read_corrupt_file_returns_none() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    fs::create_dir_all(cfg.parent().required()?)?;
    fs::write(&cfg, "garbage")?;
    assert!(read_cli_token(Some(&cfg)).is_none());
    Ok(())
}

#[test]
fn read_non_dict_json_returns_none() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    fs::create_dir_all(cfg.parent().required()?)?;
    fs::write(&cfg, r#"["a", "b"]"#)?;
    assert!(read_cli_token(Some(&cfg)).is_none());
    Ok(())
}

#[test]
fn read_ignores_unknown_keys() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    fs::create_dir_all(cfg.parent().required()?)?;
    fs::write(
        &cfg,
        r#"{"api_key": "a2x_pat_k", "base_url": "http://f/", "extra": 1}"#,
    )?;
    let t = read_cli_token(Some(&cfg)).required()?;
    assert_eq!(t.api_key.as_deref(), Some("a2x_pat_k"));
    Ok(())
}

// ── Precedence rules (test_resolve_credentials.py) ────────────────────────

#[test]
fn no_config_no_explicit_returns_defaults() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    let (key, url) = resolve_credentials(None, None, Some(&cfg));
    assert_eq!(key, None);
    assert_eq!(url, DEFAULT_BASE_URL);
    Ok(())
}

#[test]
fn file_provides_both_fields() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    write_cli_token(
        &"a2x_pat_test_x".repeat(3),
        "http://from-file.example/",
        Some(&cfg),
    )?;
    let (key, url) = resolve_credentials(None, None, Some(&cfg));
    assert!(key.required()?.starts_with("a2x_pat_"));
    assert_eq!(url, "http://from-file.example/");
    Ok(())
}

#[test]
fn explicit_api_key_overrides_file() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    write_cli_token(
        &format!("a2x_pat_FILE{}", "x".repeat(39)),
        "http://from-file/",
        Some(&cfg),
    )?;
    let explicit = format!("a2x_pat_EXPLICIT{}", "y".repeat(35));
    let (key, url) = resolve_credentials(Some(&explicit), None, Some(&cfg));
    assert!(key.required()?.starts_with("a2x_pat_EXPLICIT"));
    assert_eq!(url, "http://from-file/");
    Ok(())
}

#[test]
fn explicit_base_url_overrides_file() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    write_cli_token(&"a2x_pat_x".repeat(6), "http://from-file/", Some(&cfg))?;
    let (key, url) = resolve_credentials(None, Some("http://from-args/"), Some(&cfg));
    assert!(key.required()?.starts_with("a2x_pat_"));
    assert_eq!(url, "http://from-args/");
    Ok(())
}

#[test]
fn both_explicit_bypasses_file_read() -> TestResult {
    // A path to an unreadable location proves the file is never touched.
    let bogus = PathBuf::from("/definitely/not/here/cli_token.json");
    let (key, url) = resolve_credentials(Some(&"a2x_pat_x".repeat(6)), Some("http://x/"), Some(&bogus));
    assert!(key.required()?.starts_with("a2x_pat_"));
    assert_eq!(url, "http://x/");
    Ok(())
}

#[test]
fn explicit_args_dont_lose_when_file_corrupt() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    fs::create_dir_all(cfg.parent().required()?)?;
    fs::write(&cfg, "not valid json {{")?;
    let (key, url) = resolve_credentials(Some(&"a2x_pat_x".repeat(6)), Some("http://x/"), Some(&cfg));
    assert!(key.required()?.starts_with("a2x_pat_"));
    assert_eq!(url, "http://x/");
    // And with only one explicit arg, the corrupt file degrades to defaults.
    let (key, url) = resolve_credentials(None, Some("http://y/"), Some(&cfg));
    assert_eq!(key, None);
    assert_eq!(url, "http://y/");
    Ok(())
}

#[test]
fn blank_file_values_are_ignored() -> TestResult {
    let (_dir, cfg) = isolated_home()?;
    fs::create_dir_all(cfg.parent().required()?)?;
    fs::write(&cfg, r#"{"api_key": "  ", "base_url": ""}"#)?;
    let (key, url) = resolve_credentials(None, None, Some(&cfg));
    assert_eq!(key, None);
    assert_eq!(url, DEFAULT_BASE_URL);
    Ok(())
}
