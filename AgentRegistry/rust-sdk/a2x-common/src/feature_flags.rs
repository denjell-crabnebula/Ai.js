// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Runtime probe for optional backend features.
//!
//! The Python package detected optional pip extras at import time. In Rust
//! every subsystem is compiled in, so availability is a runtime property set
//! by the process at startup (for example, vector search is available only
//! when an embedding backend is configured). Handlers call [`require`] at
//! their entry point so a missing feature becomes a structured 503.
//!
//! Known features: `vector` and `evaluation`. Both default to available.
//! The environment variable `A2X_REGISTRY_DISABLED_FEATURES` (comma
//! separated) can turn features off without code changes.

use once_cell::sync::Lazy;
use parking_lot::RwLock;

use crate::errors::A2xError;

/// An optional capability of the registry that can be switched off per process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Vector search and its embedding pipeline.
    Vector,
    /// Retrieval evaluation tooling.
    Evaluation,
}

impl Feature {
    /// Every feature, in declaration order.
    pub const ALL: [Feature; 2] = [Feature::Vector, Feature::Evaluation];

    /// The feature's name as used in configuration and error messages.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Vector => "vector",
            Feature::Evaluation => "evaluation",
        }
    }

    /// The optional-dependency group that provides the feature.
    pub fn extras(self) -> &'static str {
        self.name()
    }

    /// Look a feature up by name.
    pub fn parse(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

static STATE: Lazy<RwLock<[bool; Feature::ALL.len()]>> = Lazy::new(|| {
    let disabled: Vec<String> = ap_support::env::current()
        .get("A2X_REGISTRY_DISABLED_FEATURES")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut state = [true; Feature::ALL.len()];
    for feature in Feature::ALL {
        state[feature.index()] = !disabled.iter().any(|d| d == feature.name());
    }
    RwLock::new(state)
});

/// Names of all known features.
pub fn known() -> Vec<&'static str> {
    Feature::ALL.iter().map(|f| f.name()).collect()
}

/// Return true if `feature` is available.
pub fn has(feature: Feature) -> bool {
    STATE.read()[feature.index()]
}

/// Mark a feature available or unavailable for this process.
pub fn set_available(feature: Feature, available: bool) {
    STATE.write()[feature.index()] = available;
}

/// Return an error if `feature` is unavailable.
pub fn require(feature: Feature) -> Result<(), A2xError> {
    if has(feature) {
        Ok(())
    } else {
        Err(A2xError::feature_not_installed(feature.name(), feature.extras()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn toggling_features() -> TestResult {
        assert!(has(Feature::Evaluation));
        set_available(Feature::Evaluation, false);
        assert!(!has(Feature::Evaluation));
        let err = require(Feature::Evaluation).err_or_fail()?;
        assert!(err.feature_body().is_some());
        set_available(Feature::Evaluation, true);
        assert!(require(Feature::Evaluation).is_ok());
        Ok(())
    }

    #[test]
    fn unknown_feature_names_do_not_parse() -> TestResult {
        assert_eq!(Feature::parse("nope"), None);
        assert_eq!(Feature::parse("vector"), Some(Feature::Vector));
        assert_eq!(known(), vec!["vector", "evaluation"]);
        Ok(())
    }
}
