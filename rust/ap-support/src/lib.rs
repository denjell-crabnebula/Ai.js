// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared building blocks for the agent-protocol Rust SDKs.
//!
//! - [`bail!`], [`ensure!`], [`some_or_bail!`], [`ok_or_bail!`] and
//!   [`try_or_log!`]: return early with a typed error instead of unwrapping.
//! - [`mod@env`]: read configuration from the process environment or from an
//!   in-memory map, so tests never mutate the real environment.
//! - [`json`]: `json_object` style helpers for `serde_json` values.
//! - [`testing`]: a `Result`-based test style ([`testing::TestResult`],
//!   [`testing::OptionExt`], [`testing::ResultExt`], [`some!`], [`err!`]) that
//!   keeps `unwrap` out of test code.
//! - [`logging`] (feature `logging`): levels, formats and a one-call
//!   subscriber setup; with feature `cli` also a `clap` argument group.

#![forbid(unsafe_code)]

pub mod env;
pub mod json;
pub mod testing;

#[cfg(feature = "logging")]
pub mod logging;

/// Return early from the enclosing function with `Err(err.into())`.
///
/// ```
/// use ap_support::bail;
///
/// fn parse(text: &str) -> Result<u32, String> {
///     if text.is_empty() {
///         bail!("empty input");
///     }
///     text.parse().map_err(|e| format!("{e}"))
/// }
/// assert_eq!(parse(""), Err("empty input".to_string()));
/// ```
#[macro_export]
macro_rules! bail {
    ($err:expr $(,)?) => {
        return ::core::result::Result::Err(::core::convert::Into::into($err))
    };
}

/// Return early with `Err(err.into())` unless `cond` holds.
///
/// ```
/// use ap_support::ensure;
///
/// fn checked_div(a: u32, b: u32) -> Result<u32, String> {
///     ensure!(b != 0, "division by zero");
///     Ok(a / b)
/// }
/// assert_eq!(checked_div(1, 0), Err("division by zero".to_string()));
/// ```
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $err:expr $(,)?) => {
        if !$cond {
            $crate::bail!($err);
        }
    };
}

/// Take the value out of an `Option`, or return early with `Err(err.into())`.
///
/// ```
/// use ap_support::some_or_bail;
///
/// fn first(items: &[u8]) -> Result<u8, String> {
///     let first = some_or_bail!(items.first(), "no items");
///     Ok(*first)
/// }
/// assert_eq!(first(&[]), Err("no items".to_string()));
/// ```
#[macro_export]
macro_rules! some_or_bail {
    ($opt:expr, $err:expr $(,)?) => {
        match $opt {
            ::core::option::Option::Some(value) => value,
            ::core::option::Option::None => $crate::bail!($err),
        }
    };
}

/// Take the value out of a `Result`, or return early with an error built
/// from the original one by the closure-like `|e| expr` argument.
///
/// ```
/// use ap_support::ok_or_bail;
///
/// fn port(text: &str) -> Result<u16, String> {
///     let port = ok_or_bail!(text.parse::<u16>(), |e| format!("bad port {text:?}: {e}"));
///     Ok(port)
/// }
/// assert!(port("x").is_err());
/// ```
#[macro_export]
macro_rules! ok_or_bail {
    ($res:expr, |$e:ident| $err:expr $(,)?) => {
        match $res {
            ::core::result::Result::Ok(value) => value,
            ::core::result::Result::Err($e) => $crate::bail!($err),
        }
    };
}

/// Take the value out of a `Result`; on error log it at `warn` level with
/// `context` and evaluate to `fallback` instead. For best-effort paths where
/// a failure must not stop the caller.
///
/// ```
/// use ap_support::try_or_log;
///
/// let parsed: u32 = try_or_log!("x".parse::<u32>(), "config value is not a number", 0);
/// assert_eq!(parsed, 0);
/// ```
#[macro_export]
macro_rules! try_or_log {
    ($res:expr, $context:expr, $fallback:expr $(,)?) => {
        match $res {
            ::core::result::Result::Ok(value) => value,
            ::core::result::Result::Err(error) => {
                $crate::__private::tracing::warn!(error = %error, "{}", $context);
                $fallback
            }
        }
    };
}

/// Take the value out of an `Option` in a test, failing the test with the
/// expression text when it is `None`. Requires the test to return
/// [`testing::TestResult`].
///
/// ```
/// use ap_support::{some, testing::TestResult};
///
/// fn test() -> TestResult {
///     let items = vec![1];
///     let first = some!(items.first());
///     assert_eq!(*first, 1);
///     Ok(())
/// }
/// assert!(test().is_ok());
/// ```
#[macro_export]
macro_rules! some {
    ($opt:expr $(,)?) => {
        match $opt {
            ::core::option::Option::Some(value) => value,
            ::core::option::Option::None => {
                return ::core::result::Result::Err(
                    $crate::testing::TestFailure::new(::core::concat!(
                        "expected Some, got None: ",
                        ::core::stringify!($opt)
                    ))
                    .into(),
                )
            }
        }
    };
}

/// Take the error out of a `Result` in a test, failing the test when it is
/// `Ok`. Requires the test to return [`testing::TestResult`].
///
/// ```
/// use ap_support::{err, testing::TestResult};
///
/// fn test() -> TestResult {
///     let failed: Result<u8, &str> = Err("boom");
///     let error = err!(failed);
///     assert_eq!(error, "boom");
///     Ok(())
/// }
/// assert!(test().is_ok());
/// ```
#[macro_export]
macro_rules! err {
    ($res:expr $(,)?) => {
        match $res {
            ::core::result::Result::Err(error) => error,
            ::core::result::Result::Ok(value) => {
                return ::core::result::Result::Err(
                    $crate::testing::TestFailure::new(::std::format!(
                        "expected Err, got Ok({:?}): {}",
                        value,
                        ::core::stringify!($res)
                    ))
                    .into(),
                )
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
    pub use tracing;
}
