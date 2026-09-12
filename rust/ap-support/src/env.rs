// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Configuration from the environment: sources, a typed parser and a lockdown
//! policy that keeps dangerous settings out.
//!
//! - [`EnvSource`] abstracts where variables come from: the process
//!   ([`ProcessEnv`]), an in-memory map ([`MapEnv`]) or a policy-filtered view
//!   ([`LockedEnv`]). Library code reads through [`current`], which is the
//!   process environment until a binary calls [`install`].
//! - [`VarSpec`] describes one variable: its type, default, secrecy and the
//!   [`Rule`]s a value must satisfy. [`EnvPolicy`] collects specs for a crate or
//!   a binary and can [`EnvPolicy::check`] an environment or wrap it.
//! - [`Lockdown`] is the enforcement level. `Open` checks nothing.
//!   `Restricted` hides values that break a rule and logs why. `Locked` also
//!   refuses unknown variables in the crates' namespaces, forbids trace
//!   logging, requires production settings and reports every violation so a
//!   binary can refuse to start.
//!
//! Nothing here ever writes to the process environment, so no `unsafe` is
//! needed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

// ── sources ─────────────────────────────────────────────────────────────────

/// A read-only source of environment variables.
pub trait EnvSource: Send + Sync {
    /// The value of `key`, or `None` when it is unset or not valid Unicode.
    fn get(&self, key: &str) -> Option<String>;

    /// The names of every variable the source can enumerate. Sources that
    /// cannot enumerate return an empty list.
    fn names(&self) -> Vec<String> {
        Vec::new()
    }

    /// The value of `key` when it is set and not blank after trimming.
    fn get_non_blank(&self, key: &str) -> Option<String> {
        self.get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

/// Parse the value of `key` from `env`. `Ok(None)` when unset or blank; `Err`
/// carries the raw text and the parse error when the value is present but
/// invalid.
pub fn parse_var<T: FromStr>(env: &dyn EnvSource, key: &str) -> Result<Option<T>, ParseError<T::Err>> {
    match env.get_non_blank(key) {
        None => Ok(None),
        Some(raw) => raw.parse::<T>().map(Some).map_err(|source| ParseError {
            key: key.to_string(),
            raw,
            source,
        }),
    }
}

/// The process environment (`std::env::var`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn names(&self) -> Vec<String> {
        std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .collect()
    }
}

/// An in-memory environment for tests and embedding.
#[derive(Clone, Debug, Default)]
pub struct MapEnv {
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    /// An empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }

    /// Insert or replace a variable.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.vars.insert(key.into(), value.into());
        self
    }

    /// Remove a variable.
    pub fn remove(&mut self, key: &str) -> &mut Self {
        self.vars.remove(key);
        self
    }
}

impl<const N: usize> From<[(&str, &str); N]> for MapEnv {
    fn from(pairs: [(&str, &str); N]) -> Self {
        let mut env = MapEnv::new();
        for (k, v) in pairs {
            env.set(k, v);
        }
        env
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn names(&self) -> Vec<String> {
        self.vars.keys().cloned().collect()
    }
}

impl<T: EnvSource + ?Sized> EnvSource for &T {
    fn get(&self, key: &str) -> Option<String> {
        (**self).get(key)
    }

    fn names(&self) -> Vec<String> {
        (**self).names()
    }
}

impl<T: EnvSource + ?Sized> EnvSource for Arc<T> {
    fn get(&self, key: &str) -> Option<String> {
        (**self).get(key)
    }

    fn names(&self) -> Vec<String> {
        (**self).names()
    }
}

impl<T: EnvSource + ?Sized> EnvSource for Box<T> {
    fn get(&self, key: &str) -> Option<String> {
        (**self).get(key)
    }

    fn names(&self) -> Vec<String> {
        (**self).names()
    }
}

/// A variable was present but could not be parsed.
#[derive(Debug)]
pub struct ParseError<E> {
    /// The variable name.
    pub key: String,
    /// The raw text that failed to parse.
    pub raw: String,
    /// The parser's error.
    pub source: E,
}

impl<E: fmt::Display> fmt::Display for ParseError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={:?} is invalid: {}", self.key, self.raw, self.source)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ParseError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

// ── the current source ──────────────────────────────────────────────────────

static CURRENT: OnceLock<Box<dyn EnvSource>> = OnceLock::new();
static PROCESS: ProcessEnv = ProcessEnv;

/// The environment library code reads from: the process environment until
/// [`install`] replaces it, typically with a [`LockedEnv`].
pub fn current() -> &'static dyn EnvSource {
    match CURRENT.get() {
        Some(env) => env.as_ref(),
        None => &PROCESS,
    }
}

/// Make `env` the source [`current`] returns, for the rest of the process.
/// Fails when a source was already installed.
pub fn install(env: impl EnvSource + 'static) -> Result<(), AlreadyInstalled> {
    CURRENT.set(Box::new(env)).map_err(|_| AlreadyInstalled)
}

/// [`install`] was called twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInstalled;

impl fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an environment source is already installed for this process")
    }
}

impl std::error::Error for AlreadyInstalled {}

// ── lockdown levels ─────────────────────────────────────────────────────────

/// How strictly an [`EnvPolicy`] is enforced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Lockdown {
    /// No checks; every variable is passed through as is.
    Open,
    /// Values that break a rule are hidden (the default applies) and logged.
    #[default]
    Restricted,
    /// As `Restricted`, plus: unknown variables in the crates' namespaces are
    /// hidden, trace logging and non-production tiers are refused, secrets
    /// that a production deployment needs are required, and every violation
    /// is reported so the binary can refuse to start.
    Locked,
}

impl Lockdown {
    /// The level's name as accepted by [`FromStr`].
    pub fn as_str(self) -> &'static str {
        match self {
            Lockdown::Open => "open",
            Lockdown::Restricted => "restricted",
            Lockdown::Locked => "locked",
        }
    }
}

impl fmt::Display for Lockdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Lockdown {
    type Err = InvalidLockdown;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(Lockdown::Open),
            "restricted" => Ok(Lockdown::Restricted),
            "locked" => Ok(Lockdown::Locked),
            other => Err(InvalidLockdown(other.to_string())),
        }
    }
}

/// A lockdown level name that is not `open`, `restricted` or `locked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLockdown(String);

impl fmt::Display for InvalidLockdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown lockdown level {:?}; expected open, restricted or locked",
            self.0
        )
    }
}

impl std::error::Error for InvalidLockdown {}

// ── variable specifications ─────────────────────────────────────────────────

/// The shape a variable's value must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any text.
    Text,
    /// A signed integer.
    Integer,
    /// A decimal number.
    Float,
    /// `true`, `false`, `1`, `0`, `yes`, `no`, `on`, `off`.
    Bool,
    /// A TCP port, 1 to 65535.
    Port,
    /// A host name or IP address to bind or connect to.
    Host,
    /// An `http` or `https` URL.
    Url,
    /// A file system path.
    Path,
}

impl Kind {
    fn check(self, value: &str) -> Result<(), String> {
        match self {
            Kind::Text => Ok(()),
            Kind::Integer => value
                .parse::<i64>()
                .map(|_| ())
                .or_else(|_| {
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|f| f.fract() == 0.0)
                        .map(|_| ())
                        .ok_or(())
                })
                .map_err(|_| "is not an integer".to_string()),
            Kind::Float => value
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(|_| ())
                .ok_or_else(|| "is not a number".to_string()),
            Kind::Bool => match value.to_ascii_lowercase().as_str() {
                "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off" => Ok(()),
                _ => Err("is not a boolean".to_string()),
            },
            Kind::Port => match value.parse::<i64>() {
                Ok(p) if (1..=65535).contains(&p) => Ok(()),
                _ => Err("is not a port in 1..=65535".to_string()),
            },
            Kind::Host => {
                if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c == '/') {
                    Err("is not a host name or address".to_string())
                } else {
                    Ok(())
                }
            }
            Kind::Url => {
                if value.starts_with("http://") || value.starts_with("https://") {
                    Ok(())
                } else {
                    Err("is not an http or https URL".to_string())
                }
            }
            Kind::Path => {
                if value.trim().is_empty() {
                    Err("is empty".to_string())
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// A constraint a value must satisfy from a given lockdown level upwards.
#[derive(Clone)]
pub struct Rule {
    check: Check,
    from: Lockdown,
}

/// A caller-supplied check: `Err(reason)` fails it.
type CustomCheck = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
enum Check {
    IntRange(i64, i64),
    FloatRange(f64, f64),
    OneOf(&'static [&'static str]),
    ListOf(&'static [&'static str]),
    LoopbackOnly,
    HttpsRemote,
    SafePath,
    NoDevelopmentKey(&'static [&'static str]),
    NotContaining(&'static [&'static str]),
    ProductionTier,
    Required,
    Custom(CustomCheck),
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rule({} from {})", self.describe(), self.from)
    }
}

impl Rule {
    fn new(check: Check, from: Lockdown) -> Self {
        Self { check, from }
    }

    /// The integer value must lie in `min..=max`. Enforced from `Restricted`.
    pub fn int_range(min: i64, max: i64) -> Self {
        Self::new(Check::IntRange(min, max), Lockdown::Restricted)
    }

    /// The number must lie in `min..=max`. Enforced from `Restricted`.
    pub fn float_range(min: f64, max: f64) -> Self {
        Self::new(Check::FloatRange(min, max), Lockdown::Restricted)
    }

    /// The value must be one of `choices` (case-insensitive). Enforced from `Restricted`.
    pub fn one_of(choices: &'static [&'static str]) -> Self {
        Self::new(Check::OneOf(choices), Lockdown::Restricted)
    }

    /// A comma-separated list whose items must all be in `choices`. Enforced from `Restricted`.
    pub fn list_of(choices: &'static [&'static str]) -> Self {
        Self::new(Check::ListOf(choices), Lockdown::Restricted)
    }

    /// A bind address must be a loopback address unless the policy allows
    /// public binds. Enforced from `Restricted`.
    pub fn loopback_only() -> Self {
        Self::new(Check::LoopbackOnly, Lockdown::Restricted)
    }

    /// A URL to a non-loopback host must use `https`. Enforced from `Restricted`.
    pub fn https_remote() -> Self {
        Self::new(Check::HttpsRemote, Lockdown::Restricted)
    }

    /// A path must not point at the file system root or a system directory
    /// and must not contain `..`. Enforced from `Restricted`.
    pub fn safe_path() -> Self {
        Self::new(Check::SafePath, Lockdown::Restricted)
    }

    /// A key must not be one of the built-in development seeds. Enforced from `Restricted`.
    pub fn no_development_key(labels: &'static [&'static str]) -> Self {
        Self::new(Check::NoDevelopmentKey(labels), Lockdown::Restricted)
    }

    /// The value must not contain any of `needles` (case-insensitive). Enforced from `Locked`.
    pub fn not_containing(needles: &'static [&'static str]) -> Self {
        Self::new(Check::NotContaining(needles), Lockdown::Locked)
    }

    /// An environment-tier variable must name production when set. Enforced from `Locked`.
    pub fn production_tier() -> Self {
        Self::new(Check::ProductionTier, Lockdown::Locked)
    }

    /// The variable must be set. Enforced from `Locked`.
    pub fn required() -> Self {
        Self::new(Check::Required, Lockdown::Locked)
    }

    /// Any predicate; `Err(reason)` fails the check. Enforced from `Restricted`.
    pub fn custom(check: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self::new(Check::Custom(Arc::new(check)), Lockdown::Restricted)
    }

    /// Enforce this rule only from `level` upwards.
    pub fn from(mut self, level: Lockdown) -> Self {
        self.from = level;
        self
    }

    /// A short description for reports and documentation.
    pub fn describe(&self) -> String {
        match &self.check {
            Check::IntRange(a, b) => format!("integer in {a}..={b}"),
            Check::FloatRange(a, b) => format!("number in {a}..={b}"),
            Check::OneOf(c) => format!("one of {}", c.join(", ")),
            Check::ListOf(c) => format!("comma-separated items from {}", c.join(", ")),
            Check::LoopbackOnly => "loopback address unless public binds are allowed".into(),
            Check::HttpsRemote => "https for non-loopback hosts".into(),
            Check::SafePath => "a path outside system directories, without ..".into(),
            Check::NoDevelopmentKey(_) => "not a built-in development key".into(),
            Check::NotContaining(n) => format!("not containing {}", n.join(", ")),
            Check::ProductionTier => "prod or production".into(),
            Check::Required => "required".into(),
            Check::Custom(_) => "custom check".into(),
        }
    }

    fn evaluate(&self, value: Option<&str>, policy: &EnvPolicy) -> Result<(), String> {
        let Some(value) = value else {
            return match self.check {
                Check::Required => Err("must be set".into()),
                _ => Ok(()),
            };
        };
        match &self.check {
            Check::IntRange(min, max) => match value.parse::<f64>() {
                Ok(n) if n.fract() == 0.0 && n >= *min as f64 && n <= *max as f64 => Ok(()),
                _ => Err(format!("must be an integer in {min}..={max}")),
            },
            Check::FloatRange(min, max) => match value.parse::<f64>() {
                Ok(n) if n.is_finite() && n >= *min && n <= *max => Ok(()),
                _ => Err(format!("must be a number in {min}..={max}")),
            },
            Check::OneOf(choices) => {
                if choices.iter().any(|c| c.eq_ignore_ascii_case(value)) {
                    Ok(())
                } else {
                    Err(format!("must be one of {}", choices.join(", ")))
                }
            }
            Check::ListOf(choices) => {
                let bad: Vec<&str> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty() && !choices.iter().any(|c| c.eq_ignore_ascii_case(item)))
                    .collect();
                if bad.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "contains unknown items {}; allowed: {}",
                        bad.join(", "),
                        choices.join(", ")
                    ))
                }
            }
            Check::LoopbackOnly => {
                if policy.allow_public_bind || is_loopback_host(value) {
                    Ok(())
                } else {
                    Err("binds a non-loopback address; pass --allow-public-bind to permit it".into())
                }
            }
            Check::HttpsRemote => {
                let host = value
                    .strip_prefix("http://")
                    .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or(""))
                    .map(|authority| authority.rsplit('@').next().unwrap_or(""))
                    .map(strip_port);
                match host {
                    Some(h) if !is_loopback_host(h) => Err("uses plain http to a non-loopback host".into()),
                    _ => Ok(()),
                }
            }
            Check::SafePath => {
                let normalized = value.trim().trim_end_matches('/');
                let forbidden = [
                    "", "/", "/etc", "/proc", "/sys", "/dev", "/boot", "/bin", "/sbin", "/usr", "/lib",
                    "/root",
                ];
                if forbidden.contains(&normalized)
                    || forbidden
                        .iter()
                        .any(|f| !f.is_empty() && f != &"/" && normalized.starts_with(&format!("{f}/")))
                {
                    Err("points at a system directory".into())
                } else if normalized.split(['/', '\\']).any(|part| part == "..") {
                    Err("contains a .. component".into())
                } else {
                    Ok(())
                }
            }
            Check::NoDevelopmentKey(labels) => {
                if labels.iter().any(|l| *l == value.trim()) {
                    Err("is the built-in development key".into())
                } else {
                    Ok(())
                }
            }
            Check::NotContaining(needles) => {
                let lower = value.to_ascii_lowercase();
                match needles.iter().find(|n| lower.contains(&n.to_ascii_lowercase())) {
                    Some(n) => Err(format!("must not contain {n:?} under lockdown")),
                    None => Ok(()),
                }
            }
            Check::ProductionTier => match value.trim().to_ascii_lowercase().as_str() {
                "prod" | "production" => Ok(()),
                _ => Err("must be prod or production under lockdown".into()),
            },
            Check::Required => Ok(()),
            Check::Custom(check) => check(value),
        }
    }
}

fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    authority.rsplit_once(':').map_or(authority, |(host, _)| host)
}

fn is_loopback_host(host: &str) -> bool {
    let host = strip_port(host.trim());
    if host.eq_ignore_ascii_case("localhost") || host == "::1" {
        return true;
    }
    host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// One environment variable: what it is, what it may hold, and how sensitive it is.
#[derive(Clone, Debug)]
pub struct VarSpec {
    name: &'static str,
    kind: Kind,
    description: &'static str,
    default: Option<&'static str>,
    secret: bool,
    rules: Vec<Rule>,
}

impl VarSpec {
    /// A specification for `name` holding a value of `kind`.
    pub fn new(name: &'static str, kind: Kind, description: &'static str) -> Self {
        Self {
            name,
            kind,
            description,
            default: None,
            secret: false,
            rules: Vec::new(),
        }
    }

    /// The value used when the variable is unset, for documentation.
    pub fn default(mut self, default: &'static str) -> Self {
        self.default = Some(default);
        self
    }

    /// Redact the value in reports and logs.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Add a rule.
    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// The variable name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The value kind.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The description.
    pub fn description(&self) -> &'static str {
        self.description
    }

    /// The documented default, if any.
    pub fn default_value(&self) -> Option<&'static str> {
        self.default
    }

    /// Whether the value is redacted.
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// The rules, for documentation.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    fn display_value(&self, value: &str) -> String {
        if self.secret {
            "[redacted]".to_string()
        } else {
            value.to_string()
        }
    }
}

// ── policy ──────────────────────────────────────────────────────────────────

/// A set of variable specifications plus the namespaces they cover.
#[derive(Clone, Debug, Default)]
pub struct EnvPolicy {
    specs: BTreeMap<&'static str, VarSpec>,
    prefixes: BTreeSet<&'static str>,
    allow_public_bind: bool,
}

impl EnvPolicy {
    /// An empty policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// The policy for the variables `ap-support` itself defines (logging and
    /// lockdown), which every binary's policy should include.
    pub fn base() -> Self {
        Self::new()
            .prefix("AP_")
            .var(
                VarSpec::new("AP_LOG_LEVEL", Kind::Text, "Log level")
                    .default("info")
                    .rule(Rule::one_of(&["off", "error", "warn", "info", "debug", "trace"]))
                    .rule(Rule::not_containing(&["trace"])),
            )
            .var(
                VarSpec::new("AP_LOG_FORMAT", Kind::Text, "Log output format")
                    .default("text")
                    .rule(Rule::one_of(&["text", "compact", "pretty", "json"])),
            )
            .var(
                VarSpec::new("RUST_LOG", Kind::Text, "Log filter directives")
                    .rule(Rule::not_containing(&["trace"])),
            )
            .var(
                VarSpec::new("AP_ENV_LOCKDOWN", Kind::Text, "Environment lockdown level")
                    .default("restricted")
                    .rule(Rule::one_of(&["open", "restricted", "locked"])),
            )
    }

    /// Add or replace a variable specification.
    pub fn var(mut self, spec: VarSpec) -> Self {
        self.specs.insert(spec.name, spec);
        self
    }

    /// Declare a namespace prefix; under `Locked`, variables with the prefix
    /// that have no specification are treated as violations.
    pub fn prefix(mut self, prefix: &'static str) -> Self {
        self.prefixes.insert(prefix);
        self
    }

    /// Merge another policy's specifications and prefixes into this one.
    pub fn merge(mut self, other: EnvPolicy) -> Self {
        self.specs.extend(other.specs);
        self.prefixes.extend(other.prefixes);
        self.allow_public_bind |= other.allow_public_bind;
        self
    }

    /// Permit bind addresses other than loopback.
    pub fn allow_public_bind(mut self, allow: bool) -> Self {
        self.allow_public_bind = allow;
        self
    }

    /// The specifications, sorted by name.
    pub fn specs(&self) -> impl Iterator<Item = &VarSpec> {
        self.specs.values()
    }

    /// The specification for `name`, if any.
    pub fn spec(&self, name: &str) -> Option<&VarSpec> {
        self.specs.get(name)
    }

    fn covers(&self, name: &str) -> bool {
        self.prefixes.iter().any(|p| name.starts_with(p))
    }

    /// Evaluate one variable at `level`. `Ok(())` when the value is absent or
    /// acceptable; `Err` names the failed rule.
    pub fn evaluate(&self, name: &str, value: Option<&str>, level: Lockdown) -> Result<(), Violation> {
        if level == Lockdown::Open {
            return Ok(());
        }
        let Some(spec) = self.specs.get(name) else {
            return if level == Lockdown::Locked && self.covers(name) && value.is_some() {
                Err(Violation {
                    name: name.to_string(),
                    value: value.map(|v| v.to_string()),
                    reason: "is not a known variable; check the spelling".into(),
                    severity: Severity::Unknown,
                })
            } else {
                Ok(())
            };
        };
        let shown = value.map(|v| spec.display_value(v));
        if let Some(raw) = value
            && let Err(reason) = spec.kind.check(raw.trim())
        {
            return Err(Violation {
                name: name.to_string(),
                value: shown,
                reason,
                severity: Severity::Invalid,
            });
        }
        for rule in &spec.rules {
            if level < rule.from {
                continue;
            }
            if let Err(reason) = rule.evaluate(value.map(str::trim), self) {
                return Err(Violation {
                    name: name.to_string(),
                    value: shown,
                    reason,
                    severity: Severity::Dangerous,
                });
            }
        }
        Ok(())
    }

    /// Check every specified variable, and under `Locked` every enumerable
    /// variable in the declared namespaces, against `env`.
    pub fn check(&self, env: &dyn EnvSource, level: Lockdown) -> Report {
        let mut report = Report {
            level,
            violations: Vec::new(),
            seen: Vec::new(),
        };
        if level == Lockdown::Open {
            return report;
        }
        let mut names: BTreeSet<String> = self.specs.keys().map(|k| k.to_string()).collect();
        if level == Lockdown::Locked {
            names.extend(env.names().into_iter().filter(|n| self.covers(n)));
        }
        for name in names {
            let value = env.get(&name);
            if let Some(v) = &value {
                let shown = self
                    .specs
                    .get(name.as_str())
                    .map_or_else(|| v.clone(), |s| s.display_value(v));
                report.seen.push((name.clone(), shown));
            }
            if let Err(violation) = self.evaluate(&name, value.as_deref(), level) {
                report.violations.push(violation);
            }
        }
        report
    }

    /// Wrap `inner` so that every read is checked at `level`.
    pub fn lock(self, inner: impl EnvSource + 'static, level: Lockdown) -> LockedEnv {
        LockedEnv {
            inner: Box::new(inner),
            policy: Arc::new(self),
            level,
            reported: Mutex::new(BTreeSet::new()),
        }
    }

    /// A Markdown table of the variables, for documentation.
    pub fn markdown_table(&self) -> String {
        let mut out =
            String::from("| Variable | Kind | Default | Rules | Description |\n|---|---|---|---|---|\n");
        for spec in self.specs.values() {
            let rules: Vec<String> = spec
                .rules
                .iter()
                .map(|r| format!("{} (from {})", r.describe(), r.from))
                .collect();
            out.push_str(&format!(
                "| `{}` | {:?} | {} | {} | {}{} |\n",
                spec.name,
                spec.kind,
                spec.default.map_or(String::new(), |d| format!("`{d}`")),
                rules.join("; "),
                spec.description,
                if spec.secret { " (secret)" } else { "" }
            ));
        }
        out
    }
}

/// How serious a violation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The value cannot be parsed as the declared kind.
    Invalid,
    /// The value breaks a safety rule.
    Dangerous,
    /// The variable is in a covered namespace but has no specification.
    Unknown,
}

/// A variable that failed a check.
#[derive(Clone, Debug)]
pub struct Violation {
    /// The variable name.
    pub name: String,
    /// The value, redacted for secrets.
    pub value: Option<String>,
    /// Why it was refused.
    pub reason: String,
    /// The class of failure.
    pub severity: Severity,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Some(v) => write!(f, "{}={:?} {}", self.name, v, self.reason),
            None => write!(f, "{} {}", self.name, self.reason),
        }
    }
}

/// The outcome of [`EnvPolicy::check`].
#[derive(Clone, Debug)]
pub struct Report {
    /// The level the check ran at.
    pub level: Lockdown,
    /// Every failed variable.
    pub violations: Vec<Violation>,
    /// Every variable that was set, with secrets redacted.
    pub seen: Vec<(String, String)>,
}

impl Report {
    /// True when nothing failed.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "environment lockdown: {} ({} variable(s) set, {} violation(s))",
            self.level,
            self.seen.len(),
            self.violations.len()
        )?;
        for v in &self.violations {
            writeln!(f, "  refused {v}")?;
        }
        Ok(())
    }
}

/// An [`EnvSource`] that applies an [`EnvPolicy`] to every read: a value that
/// breaks a rule at the configured level reads as unset, and the refusal is
/// logged once per variable.
pub struct LockedEnv {
    inner: Box<dyn EnvSource>,
    policy: Arc<EnvPolicy>,
    level: Lockdown,
    reported: Mutex<BTreeSet<String>>,
}

impl LockedEnv {
    /// The enforcement level.
    pub fn level(&self) -> Lockdown {
        self.level
    }

    /// The policy in force.
    pub fn policy(&self) -> &EnvPolicy {
        &self.policy
    }

    /// Check the whole environment against the policy.
    pub fn report(&self) -> Report {
        self.policy.check(self.inner.as_ref(), self.level)
    }
}

impl fmt::Debug for LockedEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockedEnv").field("level", &self.level).finish()
    }
}

impl EnvSource for LockedEnv {
    fn get(&self, key: &str) -> Option<String> {
        let value = self.inner.get(key);
        match self.policy.evaluate(key, value.as_deref(), self.level) {
            Ok(()) => value,
            Err(violation) => {
                if self.reported.lock().insert(key.to_string()) {
                    tracing::warn!(variable = key, reason = %violation.reason, "environment value refused by lockdown; using the default");
                }
                None
            }
        }
    }

    fn names(&self) -> Vec<String> {
        self.inner.names()
    }
}

/// A lockdown check found violations that the level does not tolerate.
#[derive(Debug)]
pub struct LockdownError(pub Report);

impl fmt::Display for LockdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LockdownError {}

/// Check the process environment against `policy`, install a [`LockedEnv`]
/// as [`current`], and return the report. Under `Locked` any violation is an
/// error; under `Restricted` violations are logged and the defaults apply.
pub fn lockdown(policy: EnvPolicy, level: Lockdown) -> Result<Report, LockdownError> {
    let report = policy.check(&ProcessEnv, level);
    if level == Lockdown::Locked && !report.is_clean() {
        return Err(LockdownError(report));
    }
    for violation in &report.violations {
        tracing::warn!(variable = %violation.name, reason = %violation.reason, "environment value refused by lockdown; using the default");
    }
    // A second installation (for example in tests) keeps the first source.
    let _ = install(policy.lock(ProcessEnv, level));
    Ok(report)
}

/// Command-line options for the environment lockdown, to be flattened into a
/// `clap` parser next to `LoggingArgs`.
#[cfg(feature = "cli")]
#[derive(Clone, Debug, clap::Args)]
#[command(next_help_heading = "Environment")]
pub struct EnvArgs {
    /// Environment lockdown level: open, restricted or locked.
    #[arg(
        long,
        global = true,
        env = "AP_ENV_LOCKDOWN",
        default_value = "restricted",
        value_name = "LEVEL"
    )]
    pub env_lockdown: Lockdown,

    /// Permit bind addresses other than loopback.
    #[arg(long, global = true)]
    pub allow_public_bind: bool,

    /// Print the lockdown report (variables seen, with secrets redacted) to stderr.
    #[arg(long, global = true)]
    pub env_report: bool,
}

#[cfg(feature = "cli")]
impl EnvArgs {
    /// Apply the lockdown with `policy` and install the checked environment.
    pub fn apply(&self, policy: EnvPolicy) -> Result<Report, LockdownError> {
        let report = lockdown(
            policy.allow_public_bind(self.allow_public_bind),
            self.env_lockdown,
        )?;
        if self.env_report {
            eprint!("{report}");
            for (name, value) in &report.seen {
                eprintln!("  {name}={value}");
            }
        }
        Ok(report)
    }

    /// Apply the lockdown, and on a refusal print the report to stderr and
    /// exit with status 2.
    pub fn apply_or_exit(&self, policy: EnvPolicy) -> Report {
        match self.apply(policy) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ResultExt, TestResult};

    fn policy() -> EnvPolicy {
        EnvPolicy::base()
            .prefix("X_")
            .var(VarSpec::new("X_PORT", Kind::Port, "port").default("8080"))
            .var(VarSpec::new("X_HOST", Kind::Host, "bind").rule(Rule::loopback_only()))
            .var(VarSpec::new("X_URL", Kind::Url, "remote").rule(Rule::https_remote()))
            .var(VarSpec::new("X_HOME", Kind::Path, "home").rule(Rule::safe_path()))
            .var(VarSpec::new("X_WORKERS", Kind::Integer, "workers").rule(Rule::int_range(1, 64)))
            .var(
                VarSpec::new("X_KEY", Kind::Text, "key")
                    .secret()
                    .rule(Rule::no_development_key(&["dev-seed"]))
                    .rule(Rule::required()),
            )
            .var(VarSpec::new("X_ENV", Kind::Text, "tier").rule(Rule::production_tier()))
    }

    #[test]
    fn map_env_reads_and_parses() -> TestResult {
        let env = MapEnv::from([("PORT", " 8080 "), ("BLANK", "  ")]);
        assert_eq!(env.get_non_blank("PORT").as_deref(), Some("8080"));
        assert_eq!(env.get_non_blank("BLANK"), None);
        assert_eq!(parse_var::<u16>(&env, "PORT")?, Some(8080));
        assert_eq!(parse_var::<u16>(&env, "MISSING")?, None);
        let error = parse_var::<u16>(&env.with("PORT", "x"), "PORT").err_or_fail()?;
        assert_eq!(error.key, "PORT");
        Ok(())
    }

    #[test]
    fn open_level_checks_nothing() {
        let env = MapEnv::from([("X_HOST", "0.0.0.0"), ("X_WORKERS", "0")]);
        assert!(policy().check(&env, Lockdown::Open).is_clean());
    }

    #[test]
    fn restricted_hides_dangerous_values() {
        let env = MapEnv::from([
            ("X_PORT", "70000"),
            ("X_HOST", "0.0.0.0"),
            ("X_URL", "http://api.example/v1"),
            ("X_HOME", "/etc/registry"),
            ("X_WORKERS", "0"),
            ("X_KEY", "dev-seed"),
            ("X_ENV", "dev"),
        ]);
        let report = policy().check(&env, Lockdown::Restricted);
        let names: Vec<&str> = report.violations.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["X_HOME", "X_HOST", "X_KEY", "X_PORT", "X_URL", "X_WORKERS"]
        );
        assert!(
            report
                .violations
                .iter()
                .all(|v| v.name != "X_KEY" || v.value.as_deref() == Some("[redacted]"))
        );
        let locked = policy().lock(env, Lockdown::Restricted);
        assert_eq!(locked.get("X_HOST"), None);
        assert_eq!(locked.get("X_ENV").as_deref(), Some("dev"));
    }

    #[test]
    fn restricted_passes_safe_values() {
        let env = MapEnv::from([
            ("X_PORT", "8080"),
            ("X_HOST", "127.0.0.1"),
            ("X_URL", "http://localhost:8961"),
            ("X_HOME", "/var/lib/registry"),
            ("X_WORKERS", "8"),
            ("X_KEY", "hex:00"),
        ]);
        assert!(policy().check(&env, Lockdown::Restricted).is_clean());
        let env = MapEnv::from([("X_URL", "https://api.example/v1")]);
        assert!(policy().check(&env, Lockdown::Restricted).is_clean());
    }

    #[test]
    fn locked_requires_production_settings() {
        let env = MapEnv::from([("X_ENV", "dev"), ("X_TYPO", "1"), ("AP_LOG_LEVEL", "trace")]);
        let report = policy().check(&env, Lockdown::Locked);
        let mut names: Vec<&str> = report.violations.iter().map(|v| v.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["AP_LOG_LEVEL", "X_ENV", "X_KEY", "X_TYPO"]);
        assert!(report.violations.iter().any(|v| v.severity == Severity::Unknown));
        let env = MapEnv::from([
            ("X_ENV", "production"),
            ("X_KEY", "hex:00"),
            ("AP_LOG_LEVEL", "info"),
        ]);
        assert!(policy().check(&env, Lockdown::Locked).is_clean());
    }

    #[test]
    fn public_bind_can_be_allowed() {
        let env = MapEnv::from([("X_HOST", "0.0.0.0")]);
        assert!(!policy().check(&env, Lockdown::Restricted).is_clean());
        assert!(
            policy()
                .allow_public_bind(true)
                .check(&env, Lockdown::Restricted)
                .is_clean()
        );
    }

    #[test]
    fn levels_parse() -> TestResult {
        assert_eq!("LOCKED".parse::<Lockdown>()?, Lockdown::Locked);
        assert!("strict".parse::<Lockdown>().is_err());
        assert!(Lockdown::Locked > Lockdown::Restricted);
        Ok(())
    }

    #[test]
    fn documentation_table_lists_variables() {
        let table = policy().markdown_table();
        assert!(table.contains("`X_KEY`") && table.contains("(secret)"));
    }
}
