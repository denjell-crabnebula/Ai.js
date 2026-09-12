// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A `Result`-based test style with no `unwrap`.
//!
//! Tests return [`TestResult`] and use `?` on every fallible call. Values that
//! are not `Result`s get there through [`OptionExt::required`],
//! [`ResultExt::err_or_fail`] and [`ResultExt::or_fail`], or the [`some!`](crate::some)
//! and [`err!`](crate::err) macros, which name the failing expression in the
//! test output.
//!
//! ```
//! use ap_support::testing::{OptionExt, ResultExt, TestResult};
//!
//! fn parses() -> TestResult {
//!     let value: u32 = "42".parse()?;
//!     let items = [value];
//!     let first = items.first().required()?;
//!     assert_eq!(*first, 42);
//!     let error = "x".parse::<u32>().err_or_fail()?;
//!     assert!(!error.to_string().is_empty());
//!     Ok(())
//! }
//! # assert!(parses().is_ok());
//! ```

use std::error::Error;
use std::fmt;

/// The error type of a test: anything printable.
pub type TestError = Box<dyn Error + Send + Sync + 'static>;

/// The return type of a test: `Ok(())` on success, any error on failure.
pub type TestResult<T = ()> = Result<T, TestError>;

/// A test failure with a message.
#[derive(Debug)]
pub struct TestFailure(String);

impl TestFailure {
    /// A failure with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for TestFailure {}

/// `Option` to `Result` for tests.
pub trait OptionExt<T> {
    /// The value, or a failure saying it was missing.
    fn required(self) -> Result<T, TestFailure>;

    /// The value, or a failure with `context`.
    fn required_as(self, context: &str) -> Result<T, TestFailure>;
}

impl<T> OptionExt<T> for Option<T> {
    fn required(self) -> Result<T, TestFailure> {
        self.ok_or_else(|| TestFailure::new("expected Some, got None"))
    }

    fn required_as(self, context: &str) -> Result<T, TestFailure> {
        self.ok_or_else(|| TestFailure::new(format!("{context}: expected Some, got None")))
    }
}

/// `Result` helpers for tests.
pub trait ResultExt<T, E> {
    /// The error, or a failure when the result was `Ok`.
    fn err_or_fail(self) -> Result<E, TestFailure>
    where
        T: fmt::Debug;

    /// The value, converting any error to a printable failure. For error
    /// types that do not implement `std::error::Error`.
    fn or_fail(self) -> Result<T, TestFailure>
    where
        E: fmt::Debug;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn err_or_fail(self) -> Result<E, TestFailure>
    where
        T: fmt::Debug,
    {
        match self {
            Err(e) => Ok(e),
            Ok(v) => Err(TestFailure::new(format!("expected Err, got Ok({v:?})"))),
        }
    }

    fn or_fail(self) -> Result<T, TestFailure>
    where
        E: fmt::Debug,
    {
        self.map_err(|e| TestFailure::new(format!("{e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_convert_values() -> TestResult {
        assert_eq!(Some(1).required()?, 1);
        assert!(None::<u8>.required().is_err());
        let failed: Result<u8, &str> = Err("boom");
        assert_eq!(failed.err_or_fail()?, "boom");
        assert!(Ok::<u8, &str>(1).err_or_fail().is_err());
        assert_eq!(Ok::<u8, ()>(2).or_fail()?, 2);
        let error = crate::err!(Err::<u8, _>("x"));
        assert_eq!(error, "x");
        let value = crate::some!(Some(3));
        assert_eq!(value, 3);
        Ok(())
    }
}
