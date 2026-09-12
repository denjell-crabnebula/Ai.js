// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Build events: log lines, progress bars and phase changes.
//!
//! The Python builder logs through `logging` and draws an in-place
//! progress bar on stdout. The backend captures log records and streams
//! them over SSE as `HH:MM:SS  message`. Here every message goes through a
//! [`BuildSink`] so callers can print, log or stream them.

use std::fmt;
use std::io::Write;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Severity of a log event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

/// Build phase recorded in `taxonomy.json` as `build_status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildPhase {
    Bfs,
    CrossDomain,
    Complete,
}

impl BuildPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            BuildPhase::Bfs => "bfs",
            BuildPhase::CrossDomain => "cross_domain",
            BuildPhase::Complete => "complete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bfs" => Some(BuildPhase::Bfs),
            "cross_domain" => Some(BuildPhase::CrossDomain),
            "complete" => Some(BuildPhase::Complete),
            _ => None,
        }
    }
}

impl fmt::Display for BuildPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An event emitted while building.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildEvent {
    /// A log line, identical to the Python `logger` message.
    Log { level: LogLevel, message: String },
    /// A progress update. `message` is the bar line Python logs, for
    /// example `████░░ 23.1% [425/1839] assigned`.
    Progress {
        done: usize,
        total: usize,
        message: String,
    },
    /// The build entered a new phase.
    Phase { phase: BuildPhase, message: String },
}

impl BuildEvent {
    /// The human readable text of the event.
    pub fn message(&self) -> &str {
        match self {
            BuildEvent::Log { message, .. } => message,
            BuildEvent::Progress { message, .. } => message,
            BuildEvent::Phase { message, .. } => message,
        }
    }

    /// `HH:MM:SS  message`, the line the backend streams over SSE.
    pub fn timestamped(&self) -> String {
        format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), self.message())
    }
}

/// Render a progress bar.
///
/// Returns `(terminal_line, log_message)`: the terminal line starts with
/// `\r` for in-place drawing, the log message is what Python logs.
pub fn format_progress(
    done: usize,
    total: usize,
    width: usize,
    prefix: &str,
    suffix: &str,
) -> (String, String) {
    let filled = (width * done)
        .checked_div(total)
        .map_or(width, |ratio| ratio.min(width));
    let bar: String = "█".repeat(filled) + &"░".repeat(width.saturating_sub(filled));
    let pct = if total == 0 {
        100.0
    } else {
        done as f64 / total as f64 * 100.0
    };
    let mut line = if prefix.is_empty() {
        format!("\r  {bar} {pct:5.1}% [{done}/{total}]")
    } else {
        format!("\r  {prefix} {bar} {pct:5.1}% [{done}/{total}]")
    };
    if !suffix.is_empty() {
        line.push(' ');
        line.push_str(suffix);
    }
    line.push_str(&" ".repeat(10));
    let mut log_msg = if prefix.is_empty() {
        format!(" {bar} {pct:5.1}% [{done}/{total}]")
    } else {
        format!("  {prefix} {bar} {pct:5.1}% [{done}/{total}]")
    };
    if !suffix.is_empty() {
        log_msg.push(' ');
        log_msg.push_str(suffix);
    }
    (line, log_msg.trim().to_string())
}

type SinkFn = dyn Fn(BuildEvent) + Send + Sync;

/// Destination for [`BuildEvent`]s.
#[derive(Clone)]
pub struct BuildSink {
    inner: Arc<SinkFn>,
}

impl Default for BuildSink {
    fn default() -> Self {
        Self::tracing()
    }
}

impl fmt::Debug for BuildSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BuildSink")
    }
}

impl BuildSink {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(BuildEvent) + Send + Sync + 'static,
    {
        Self { inner: Arc::new(f) }
    }

    /// Forward every event to `tracing` at info level (warnings and
    /// errors at their level).
    pub fn tracing() -> Self {
        Self::new(|event| match &event {
            BuildEvent::Log {
                level: LogLevel::Warning,
                message,
            } => tracing::warn!("{message}"),
            BuildEvent::Log {
                level: LogLevel::Error,
                message,
            } => tracing::error!("{message}"),
            other => tracing::info!("{}", other.message()),
        })
    }

    /// Print like the Python CLI: log lines on their own line, progress
    /// bars redrawn in place with a newline when complete.
    pub fn stdout() -> Self {
        Self::new(|event| {
            let mut out = std::io::stdout().lock();
            match &event {
                BuildEvent::Progress { done, total, .. } => {
                    let (line, _) = format_progress(*done, *total, 30, "", progress_suffix(&event));
                    let _ = out.write_all(line.as_bytes());
                    if done >= total {
                        let _ = out.write_all(b"\n");
                    }
                    let _ = out.flush();
                }
                other => {
                    let _ = writeln!(out, "{}", other.message());
                }
            }
        })
    }

    /// Discard every event.
    pub fn null() -> Self {
        Self::new(|_| {})
    }

    pub fn emit(&self, event: BuildEvent) {
        (self.inner)(event);
    }

    pub fn log(&self, message: impl Into<String>) {
        self.emit(BuildEvent::Log {
            level: LogLevel::Info,
            message: message.into(),
        });
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.emit(BuildEvent::Log {
            level: LogLevel::Warning,
            message: message.into(),
        });
    }

    pub fn error(&self, message: impl Into<String>) {
        self.emit(BuildEvent::Log {
            level: LogLevel::Error,
            message: message.into(),
        });
    }

    /// Emit a progress update (no-op when `total == 0`, like Python).
    pub fn progress(&self, done: usize, total: usize, prefix: &str, suffix: &str) {
        if total == 0 {
            return;
        }
        let (_, message) = format_progress(done, total, 30, prefix, suffix);
        self.emit(BuildEvent::Progress { done, total, message });
    }

    pub fn phase(&self, phase: BuildPhase, message: impl Into<String>) {
        self.emit(BuildEvent::Phase {
            phase,
            message: message.into(),
        });
    }
}

/// Recover the suffix (`"12 assigned"`) from a progress message.
fn progress_suffix(event: &BuildEvent) -> &str {
    match event {
        BuildEvent::Progress { message, .. } => match message.find("] ") {
            Some(idx) => &message[idx + 2..],
            None => "",
        },
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn progress_line_matches_python() -> TestResult {
        let (line, msg) = format_progress(425, 1839, 30, "", "assigned");
        assert_eq!(msg, "██████░░░░░░░░░░░░░░░░░░░░░░░░  23.1% [425/1839] assigned");
        assert!(line.starts_with("\r  ██████"));
        assert!(line.ends_with("assigned          "));
        let (_, msg) = format_progress(3, 3, 6, "", "");
        assert_eq!(msg, "██████ 100.0% [3/3]");
        let (_, msg) = format_progress(0, 10, 4, "kw", "5 keywords");
        assert_eq!(msg, "kw ░░░░   0.0% [0/10] 5 keywords");
        Ok(())
    }

    #[test]
    fn sink_collects_events() -> TestResult {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let sink = BuildSink::new(move |e| s2.lock().push(e));
        sink.log("hello");
        sink.progress(1, 2, "", "x");
        sink.progress(1, 0, "", "");
        sink.phase(BuildPhase::Bfs, "BFS");
        let events = seen.lock();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].message(), "hello");
        assert!(matches!(
            events[1],
            BuildEvent::Progress {
                done: 1,
                total: 2,
                ..
            }
        ));
        let v = serde_json::to_value(&events[2])?;
        assert_eq!(v["type"], "phase");
        assert_eq!(v["phase"], "bfs");
        assert!(events[0].timestamped().ends_with("  hello"));
        Ok(())
    }
}
