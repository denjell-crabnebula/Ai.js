// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! SDK diagnostic logging, the port of `include/a2a_log.h`.
//!
//! Messages are formatted with the same prefix as the C++ SDK and delivered
//! to a process-wide callback. Without a custom callback the message goes to
//! [`tracing`] at the matching level.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

/// Log severity levels, ordered from most to least verbose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum A2aLogLevel {
    /// Debug.
    Debug = 3,
    /// Info (default threshold).
    Info = 4,
    /// Warning.
    Warn = 5,
    /// Error.
    Error = 6,
    /// Fatal.
    Fatal = 7,
}

impl A2aLogLevel {
    /// Numeric value of the level.
    pub const fn value(self) -> i32 {
        self as i32
    }

    fn from_value(v: i32) -> Option<A2aLogLevel> {
        match v {
            3 => Some(A2aLogLevel::Debug),
            4 => Some(A2aLogLevel::Info),
            5 => Some(A2aLogLevel::Warn),
            6 => Some(A2aLogLevel::Error),
            7 => Some(A2aLogLevel::Fatal),
            _ => None,
        }
    }
}

/// User-provided callback invoked for each formatted log line.
pub type A2aLogCallback = fn(A2aLogLevel, String);

static LOG_LEVEL: AtomicI32 = AtomicI32::new(A2aLogLevel::Info as i32);
static LOG_CALLBACK: OnceLock<A2aLogCallback> = OnceLock::new();

/// Default log sink: forwards the line to `tracing`.
pub fn a2a_tracing_impl(level: A2aLogLevel, message: String) {
    if level < get_log_level() {
        return;
    }
    match level {
        A2aLogLevel::Debug => tracing::debug!(target: "a2a_sdk", "{message}"),
        A2aLogLevel::Info => tracing::info!(target: "a2a_sdk", "{message}"),
        A2aLogLevel::Warn => tracing::warn!(target: "a2a_sdk", "{message}"),
        A2aLogLevel::Error | A2aLogLevel::Fatal => tracing::error!(target: "a2a_sdk", "{message}"),
    }
}

/// Register a custom log callback, process-wide and once only.
///
/// Returns 0 on success and -1 when a callback was already registered.
pub fn set_log_callback(callback: A2aLogCallback) -> i32 {
    if LOG_CALLBACK.set(callback).is_ok() { 0 } else { -1 }
}

/// Set the minimum severity that is emitted. Returns 0 on success.
pub fn set_log_level(level: A2aLogLevel) -> i32 {
    LOG_LEVEL.store(level.value(), Ordering::SeqCst);
    0
}

/// Set the minimum severity from a raw value. Returns -1 for invalid values.
pub fn set_log_level_value(level: i32) -> i32 {
    match A2aLogLevel::from_value(level) {
        Some(l) => set_log_level(l),
        None => -1,
    }
}

/// Current minimum log level.
pub fn get_log_level() -> A2aLogLevel {
    A2aLogLevel::from_value(LOG_LEVEL.load(Ordering::SeqCst)).unwrap_or(A2aLogLevel::Info)
}

/// Name of a log level, for example `INFO`.
pub fn get_log_level_name(level: A2aLogLevel) -> &'static str {
    match level {
        A2aLogLevel::Debug => "DEBUG",
        A2aLogLevel::Info => "INFO",
        A2aLogLevel::Warn => "WARN",
        A2aLogLevel::Error => "ERROR",
        A2aLogLevel::Fatal => "FATAL",
    }
}

/// Current wall-clock time as `YYYY-MM-DD HH:MM:SS.mmm`.
pub fn get_current_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// Format and dispatch a log line. Used by the `a2a_log!` macro.
pub fn log_internal(level: A2aLogLevel, file: &str, func: &str, line: u32, message: &str) {
    if level < get_log_level() {
        return;
    }
    let filename = file.rsplit('/').next().unwrap_or(file);
    let tid = format!("{:?}", std::thread::current().id());
    let tid = tid
        .trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .to_string();
    let prefix = format!(
        "[{}] [{}] [{}] {}::{}:[{}] ",
        get_current_timestamp(),
        tid,
        get_log_level_name(level),
        filename,
        func,
        line
    );
    let full = format!("{prefix}{message}");
    match LOG_CALLBACK.get() {
        Some(cb) => cb(level, full),
        None => a2a_tracing_impl(level, full),
    }
}

/// Log a diagnostic message with file and line metadata.
#[macro_export]
macro_rules! a2a_log {
    ($level:expr_2021, $($arg:tt)*) => {
        $crate::log::log_internal($level, file!(), module_path!(), line!(), &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn level_values_and_names() -> TestResult {
        assert_eq!(A2aLogLevel::Debug.value(), 3);
        assert_eq!(A2aLogLevel::Fatal.value(), 7);
        assert_eq!(get_log_level_name(A2aLogLevel::Warn), "WARN");
        assert!(A2aLogLevel::Debug < A2aLogLevel::Info);
        Ok(())
    }

    #[test]
    fn set_and_get_level() -> TestResult {
        let old = get_log_level();
        assert_eq!(set_log_level(A2aLogLevel::Error), 0);
        assert_eq!(get_log_level(), A2aLogLevel::Error);
        assert_eq!(set_log_level_value(99), -1);
        assert_eq!(get_log_level(), A2aLogLevel::Error);
        set_log_level(old);
        Ok(())
    }

    #[test]
    fn timestamp_format() -> TestResult {
        let ts = get_current_timestamp();
        assert_eq!(ts.len(), 23);
        assert_eq!(&ts[10..11], " ");
        assert_eq!(&ts[19..20], ".");
        Ok(())
    }
}
