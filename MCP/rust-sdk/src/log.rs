// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! SDK diagnostic logging, port of `mcp_log.h`.
//!
//! Internally the crate logs through `tracing`. This module keeps the C++
//! API shape: a global level, an optional callback that receives formatted
//! lines and the `mcp_log!` macro. When no callback is installed lines go
//! to `tracing` at the mapped level, never to stdout, so stdio transports
//! stay clean.

use std::sync::atomic::{AtomicU8, Ordering};

use parking_lot::RwLock;

/// Diagnostic log levels with the numeric values of `MCP_LOG_LEVEL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum McpLogLevel {
    /// Verbose diagnostics.
    Debug = 3,
    /// Informational messages (default threshold).
    Info = 4,
    /// Warnings.
    Warn = 5,
    /// Errors.
    Error = 6,
    /// Fatal conditions.
    Fatal = 7,
}

impl McpLogLevel {
    /// Map a raw numeric level. Returns `None` outside the valid range.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            3 => Some(McpLogLevel::Debug),
            4 => Some(McpLogLevel::Info),
            5 => Some(McpLogLevel::Warn),
            6 => Some(McpLogLevel::Error),
            7 => Some(McpLogLevel::Fatal),
            _ => None,
        }
    }

    /// Upper case level name (`GetLogLevelName`).
    pub const fn name(self) -> &'static str {
        match self {
            McpLogLevel::Debug => "DEBUG",
            McpLogLevel::Info => "INFO",
            McpLogLevel::Warn => "WARN",
            McpLogLevel::Error => "ERROR",
            McpLogLevel::Fatal => "FATAL",
        }
    }
}

/// Name of a raw numeric level, `"UNKNOWN"` outside the valid range.
pub fn log_level_name(level: u8) -> &'static str {
    McpLogLevel::from_u8(level)
        .map(McpLogLevel::name)
        .unwrap_or("UNKNOWN")
}

/// Callback receiving formatted log lines (`McpLogCallback`).
pub type LogCallback = fn(McpLogLevel, String);

static LOG_LEVEL: AtomicU8 = AtomicU8::new(McpLogLevel::Info as u8);
static LOG_CALLBACK: RwLock<Option<LogCallback>> = RwLock::new(None);

/// Set the global threshold. Returns `-1` for an invalid raw level.
pub fn set_log_level(level: u8) -> i32 {
    if McpLogLevel::from_u8(level).is_none() {
        return -1;
    }
    LOG_LEVEL.store(level, Ordering::SeqCst);
    0
}

/// Set the global threshold with a typed level.
pub fn set_log_level_typed(level: McpLogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::SeqCst);
}

/// The current global threshold.
pub fn get_log_level() -> McpLogLevel {
    McpLogLevel::from_u8(LOG_LEVEL.load(Ordering::SeqCst)).unwrap_or(McpLogLevel::Info)
}

/// Install a callback. `None` restores the default `tracing` output.
pub fn set_log_callback(callback: Option<LogCallback>) -> i32 {
    *LOG_CALLBACK.write() = callback;
    0
}

/// The currently installed callback, if any.
pub fn get_log_callback() -> Option<LogCallback> {
    *LOG_CALLBACK.read()
}

/// Timestamp in the `YYYY-MM-DD HH:MM:SS.mmm` format used by `MCP_LOG`.
pub fn current_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// Emit a log line. Used by the `mcp_log!` macro.
pub fn emit(level: McpLogLevel, file: &str, line: u32, message: &str) {
    if level < get_log_level() {
        return;
    }
    let filename = file.rsplit('/').next().unwrap_or(file);
    let formatted = format!(
        "[{}] [{:?}] [{}] {}:[{}] {}",
        current_timestamp(),
        std::thread::current().id(),
        level.name(),
        filename,
        line,
        message
    );
    if let Some(cb) = get_log_callback() {
        cb(level, formatted);
        return;
    }
    match level {
        McpLogLevel::Debug => tracing::debug!(target: "mcp_sdk", "{message}"),
        McpLogLevel::Info => tracing::info!(target: "mcp_sdk", "{message}"),
        McpLogLevel::Warn => tracing::warn!(target: "mcp_sdk", "{message}"),
        McpLogLevel::Error | McpLogLevel::Fatal => tracing::error!(target: "mcp_sdk", "{message}"),
    }
}

/// Log through the SDK diagnostic logger, like the C++ `MCP_LOG` macro.
///
/// ```
/// use mcp_sdk::log::McpLogLevel;
/// mcp_sdk::mcp_log!(McpLogLevel::Info, "server started on port {}", 8000);
/// ```
#[macro_export]
macro_rules! mcp_log {
    ($level:expr_2021, $($arg:tt)*) => {
        $crate::log::emit($level, file!(), line!(), &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use parking_lot::Mutex;

    static CAPTURE: Mutex<Vec<(McpLogLevel, String)>> = Mutex::new(Vec::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn capture(level: McpLogLevel, message: String) {
        CAPTURE.lock().push((level, message));
    }

    #[test]
    fn level_set_get_and_invalid() -> TestResult {
        let _guard = TEST_LOCK.lock();
        let original = get_log_level();
        assert_eq!(set_log_level(McpLogLevel::Debug as u8), 0);
        assert_eq!(get_log_level(), McpLogLevel::Debug);
        assert_eq!(set_log_level(McpLogLevel::Fatal as u8), 0);
        assert_eq!(get_log_level(), McpLogLevel::Fatal);
        assert_eq!(set_log_level(2), -1);
        assert_eq!(get_log_level(), McpLogLevel::Fatal);
        assert_eq!(set_log_level(8), -1);
        set_log_level_typed(original);
        Ok(())
    }

    #[test]
    fn level_names() -> TestResult {
        assert_eq!(log_level_name(3), "DEBUG");
        assert_eq!(log_level_name(4), "INFO");
        assert_eq!(log_level_name(5), "WARN");
        assert_eq!(log_level_name(6), "ERROR");
        assert_eq!(log_level_name(7), "FATAL");
        assert_eq!(log_level_name(0), "UNKNOWN");
        Ok(())
    }

    #[test]
    fn callback_receives_filtered_lines() -> TestResult {
        let _guard = TEST_LOCK.lock();
        let original = get_log_level();
        CAPTURE.lock().clear();
        assert_eq!(set_log_callback(Some(capture)), 0);
        assert!(get_log_callback().is_some());
        set_log_level_typed(McpLogLevel::Info);
        mcp_log!(McpLogLevel::Debug, "hidden {}", 1);
        mcp_log!(McpLogLevel::Info, "Test message");
        {
            let lines = CAPTURE.lock();
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].0, McpLogLevel::Info);
            assert!(lines[0].1.contains("[INFO]"));
            assert!(lines[0].1.ends_with("Test message"));
        }
        assert_eq!(set_log_callback(None), 0);
        assert!(get_log_callback().is_none());
        set_log_level_typed(original);
        Ok(())
    }

    #[test]
    fn timestamp_format() -> TestResult {
        let ts = current_timestamp();
        assert_eq!(ts.len(), 23);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], " ");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], ".");
        Ok(())
    }
}
