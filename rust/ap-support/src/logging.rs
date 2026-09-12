// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Log levels, output formats and subscriber installation for binaries and
//! examples. Libraries only emit `tracing` events; the executable decides how
//! they are filtered and rendered by calling [`init`] once at startup.
//!
//! ```no_run
//! use ap_support::logging::{LogConfig, LogFormat, LogLevel};
//!
//! ap_support::logging::init(&LogConfig::new(LogLevel::Debug).format(LogFormat::Json))
//!     .expect("first subscriber");
//! ```
//!
//! The level resolves in this order: an explicit directive string (the
//! `RUST_LOG` syntax, for example `info,mcp_sdk=trace`) wins over the plain
//! level, which wins over the built-in default of `info`.

use std::fmt;
use std::str::FromStr;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::env::EnvSource;

/// Environment variable read by [`LogConfig::from_env`] for the level.
pub const LEVEL_VAR: &str = "AP_LOG_LEVEL";
/// Environment variable read by [`LogConfig::from_env`] for the format.
pub const FORMAT_VAR: &str = "AP_LOG_FORMAT";
/// Environment variable read by [`LogConfig::from_env`] for filter directives.
pub const FILTER_VAR: &str = "RUST_LOG";

/// How much to log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum LogLevel {
    /// Nothing.
    Off,
    /// Failures only.
    Error,
    /// Failures and recoverable problems.
    Warn,
    /// Normal operation: connections, requests, lifecycle.
    #[default]
    Info,
    /// Protocol-level detail: message ids, method names, state changes.
    Debug,
    /// Everything, including message bodies.
    Trace,
}

impl LogLevel {
    /// All levels from quietest to loudest.
    pub const ALL: [LogLevel; 6] = [
        LogLevel::Off,
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];

    /// The level's name as accepted by [`FromStr`].
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    /// One step louder, saturating at [`LogLevel::Trace`].
    pub fn louder(self) -> LogLevel {
        let index = self as usize;
        LogLevel::ALL[(index + 1).min(LogLevel::ALL.len() - 1)]
    }

    /// One step quieter, saturating at [`LogLevel::Off`].
    pub fn quieter(self) -> LogLevel {
        let index = self as usize;
        LogLevel::ALL[index.saturating_sub(1)]
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogLevel {
    type Err = InvalidValue;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        LogLevel::ALL
            .into_iter()
            .find(|l| l.as_str().eq_ignore_ascii_case(text.trim()))
            .ok_or_else(|| InvalidValue::new("log level", text, "off, error, warn, info, debug, trace"))
    }
}

impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Off => LevelFilter::OFF,
            LogLevel::Error => LevelFilter::ERROR,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Trace => LevelFilter::TRACE,
        }
    }
}

/// How log lines are rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum LogFormat {
    /// One line per event with level, target and fields.
    #[default]
    Text,
    /// Like `Text` with less padding.
    Compact,
    /// Multi-line, indented, for reading during development.
    Pretty,
    /// One JSON object per line, for log collectors.
    Json,
}

impl LogFormat {
    /// The format's name as accepted by [`FromStr`].
    pub fn as_str(self) -> &'static str {
        match self {
            LogFormat::Text => "text",
            LogFormat::Compact => "compact",
            LogFormat::Pretty => "pretty",
            LogFormat::Json => "json",
        }
    }
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogFormat {
    type Err = InvalidValue;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        [
            LogFormat::Text,
            LogFormat::Compact,
            LogFormat::Pretty,
            LogFormat::Json,
        ]
        .into_iter()
        .find(|f| f.as_str().eq_ignore_ascii_case(text.trim()))
        .ok_or_else(|| InvalidValue::new("log format", text, "text, compact, pretty, json"))
    }
}

/// A level or format name that is not recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidValue {
    what: &'static str,
    given: String,
    expected: &'static str,
}

impl InvalidValue {
    fn new(what: &'static str, given: &str, expected: &'static str) -> Self {
        Self {
            what,
            given: given.to_string(),
            expected,
        }
    }
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown {} {:?}; expected one of {}",
            self.what, self.given, self.expected
        )
    }
}

impl std::error::Error for InvalidValue {}

/// Everything [`init`] needs.
#[derive(Clone, Debug)]
pub struct LogConfig {
    level: LogLevel,
    format: LogFormat,
    filter: Option<String>,
    with_target: bool,
    with_time: bool,
    ansi: Option<bool>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: LogFormat::Text,
            filter: None,
            with_target: false,
            with_time: true,
            ansi: None,
        }
    }
}

impl LogConfig {
    /// A configuration at `level` with the default text format.
    pub fn new(level: LogLevel) -> Self {
        Self {
            level,
            ..Self::default()
        }
    }

    /// A configuration from the environment: `AP_LOG_LEVEL`, `AP_LOG_FORMAT`
    /// and `RUST_LOG` (directives). Unset variables keep the defaults; an
    /// invalid value is an error so a typo does not silence logging.
    pub fn from_env(env: &dyn EnvSource) -> Result<Self, InvalidValue> {
        let mut config = Self::default();
        if let Some(level) = env.get_non_blank(LEVEL_VAR) {
            config.level = level.parse()?;
        }
        if let Some(format) = env.get_non_blank(FORMAT_VAR) {
            config.format = format.parse()?;
        }
        config.filter = env.get_non_blank(FILTER_VAR);
        Ok(config)
    }

    /// Set the level.
    pub fn level(mut self, level: LogLevel) -> Self {
        self.level = level;
        self
    }

    /// Set the format.
    pub fn format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    /// Set filter directives in `RUST_LOG` syntax (for example
    /// `warn,a4p=debug`). They override the plain level.
    pub fn filter(mut self, directives: impl Into<String>) -> Self {
        self.filter = Some(directives.into());
        self
    }

    /// Show the event's target (module path). Off by default.
    pub fn with_target(mut self, on: bool) -> Self {
        self.with_target = on;
        self
    }

    /// Show timestamps. On by default.
    pub fn with_time(mut self, on: bool) -> Self {
        self.with_time = on;
        self
    }

    /// Force colour on or off. By default colour follows whether stderr is a
    /// terminal.
    pub fn ansi(mut self, on: bool) -> Self {
        self.ansi = Some(on);
        self
    }

    /// The configured level.
    pub fn get_level(&self) -> LogLevel {
        self.level
    }

    /// The configured format.
    pub fn get_format(&self) -> LogFormat {
        self.format
    }

    /// The configured filter directives, if any.
    pub fn get_filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    fn env_filter(&self) -> EnvFilter {
        let builder = EnvFilter::builder().with_default_directive(LevelFilter::from(self.level).into());
        match &self.filter {
            Some(directives) => builder.parse_lossy(directives),
            None => builder.parse_lossy(""),
        }
    }
}

/// Installing a subscriber failed, usually because one is already installed.
#[derive(Debug)]
pub struct InitError(String);

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not install the log subscriber: {}", self.0)
    }
}

impl std::error::Error for InitError {}

/// Install the global `tracing` subscriber described by `config`, writing to
/// stderr. Call once at startup; a second call fails with [`InitError`].
pub fn init(config: &LogConfig) -> Result<(), InitError> {
    use std::io::IsTerminal;

    let ansi = match config.ansi {
        Some(on) => on,
        None => std::io::stderr().is_terminal(),
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(config.env_filter())
        .with_target(config.with_target)
        .with_ansi(ansi)
        .with_writer(std::io::stderr);
    let result = match (config.format, config.with_time) {
        (LogFormat::Text, true) => builder.try_init(),
        (LogFormat::Text, false) => builder.without_time().try_init(),
        (LogFormat::Compact, true) => builder.compact().try_init(),
        (LogFormat::Compact, false) => builder.compact().without_time().try_init(),
        (LogFormat::Pretty, true) => builder.pretty().try_init(),
        (LogFormat::Pretty, false) => builder.pretty().without_time().try_init(),
        (LogFormat::Json, true) => builder.json().try_init(),
        (LogFormat::Json, false) => builder.json().without_time().try_init(),
    };
    result.map_err(|e| InitError(e.to_string()))
}

/// Install a subscriber configured from the process environment, falling
/// back to `info` text output. Never fails the caller: a bad variable is
/// reported on stderr and the defaults are used, and an already installed
/// subscriber is left alone.
pub fn init_from_env() {
    let config = match LogConfig::from_env(&crate::env::ProcessEnv) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("logging: {e}; using defaults");
            LogConfig::default()
        }
    };
    let _ = init(&config);
}

// `-v` raises the level one step per occurrence, `--quiet` lowers it; both act on
// `--log-level`. `--log-filter` (or `RUST_LOG`) takes directives that override
// the level per target. Flatten the struct into a `clap` parser:
//
//     #[derive(Parser)]
//     struct Cli {
//         #[command(flatten)]
//         logging: ap_support::logging::LoggingArgs,
//     }
//
// and call `cli.logging.init()` once at startup.
/// Command-line logging options.
#[cfg(feature = "cli")]
#[derive(Clone, Debug, clap::Args)]
#[command(next_help_heading = "Logging")]
pub struct LoggingArgs {
    /// Log level: off, error, warn, info, debug or trace.
    #[arg(long, global = true, env = LEVEL_VAR, default_value = "info", value_name = "LEVEL")]
    pub log_level: LogLevel,

    /// Log output format: text, compact, pretty or json.
    #[arg(long, global = true, env = FORMAT_VAR, default_value = "text", value_name = "FORMAT")]
    pub log_format: LogFormat,

    /// Filter directives (RUST_LOG syntax), for example `info,a4p=debug`.
    #[arg(long, global = true, env = FILTER_VAR, value_name = "DIRECTIVES")]
    pub log_filter: Option<String>,

    /// Raise the log level one step per occurrence.
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Lower the log level one step per occurrence.
    #[arg(long, global = true, action = clap::ArgAction::Count)]
    pub quiet: u8,

    /// Include the event target (module path) in each line.
    #[arg(long, global = true)]
    pub log_target: bool,
}

#[cfg(feature = "cli")]
impl LoggingArgs {
    /// The effective level after `-v` and `-q` adjustments.
    pub fn level(&self) -> LogLevel {
        let mut level = self.log_level;
        for _ in 0..self.verbose {
            level = level.louder();
        }
        for _ in 0..self.quiet {
            level = level.quieter();
        }
        level
    }

    /// The configuration these arguments describe.
    pub fn config(&self) -> LogConfig {
        let mut config = LogConfig::new(self.level())
            .format(self.log_format)
            .with_target(self.log_target);
        if let Some(filter) = &self.log_filter {
            config = config.filter(filter.clone());
        }
        config
    }

    /// Install the subscriber described by these arguments.
    pub fn init(&self) -> Result<(), InitError> {
        init(&self.config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::testing::TestResult;

    #[test]
    fn levels_parse_and_step() -> TestResult {
        assert_eq!("DEBUG".parse::<LogLevel>()?, LogLevel::Debug);
        assert!("loud".parse::<LogLevel>().is_err());
        assert_eq!(LogLevel::Info.louder(), LogLevel::Debug);
        assert_eq!(LogLevel::Trace.louder(), LogLevel::Trace);
        assert_eq!(LogLevel::Error.quieter(), LogLevel::Off);
        assert_eq!(LogLevel::Off.quieter(), LogLevel::Off);
        Ok(())
    }

    #[test]
    fn config_from_env() -> TestResult {
        let env = MapEnv::from([
            ("AP_LOG_LEVEL", "warn"),
            ("AP_LOG_FORMAT", "json"),
            ("RUST_LOG", "a4p=trace"),
        ]);
        let config = LogConfig::from_env(&env)?;
        assert_eq!(config.get_level(), LogLevel::Warn);
        assert_eq!(config.get_format(), LogFormat::Json);
        assert_eq!(config.get_filter(), Some("a4p=trace"));
        assert!(LogConfig::from_env(&MapEnv::from([("AP_LOG_LEVEL", "loud")])).is_err());
        Ok(())
    }

    #[cfg(feature = "cli")]
    #[test]
    fn cli_args_adjust_level() -> TestResult {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            logging: LoggingArgs,
        }
        let cli = Cli::try_parse_from(["x", "-vv", "--log-format", "compact"])?;
        assert_eq!(cli.logging.level(), LogLevel::Trace);
        assert_eq!(cli.logging.config().get_format(), LogFormat::Compact);
        let cli = Cli::try_parse_from(["x", "--log-level", "debug", "--quiet"])?;
        assert_eq!(cli.logging.level(), LogLevel::Info);
        Ok(())
    }
}
