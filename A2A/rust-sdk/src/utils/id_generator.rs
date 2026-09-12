// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Identifier generators, the port of `src/utils/id_generator.h`.

use super::generate_uuid;

/// Context passed to an identifier generator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdGeneratorContext {
    /// Task identifier, if any.
    pub task_id: Option<String>,
    /// Context identifier, if any.
    pub context_id: Option<String>,
}

/// Simple identifier generator interface.
pub trait IdGenerator: Send + Sync {
    /// Generate a new identifier.
    fn generate(&self, ctx: &IdGeneratorContext) -> String;
}

/// UUID-based generator that ignores the context.
#[derive(Clone, Copy, Debug, Default)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn generate(&self, _ctx: &IdGeneratorContext) -> String {
        generate_uuid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn uuid_generator_returns_unique_non_empty_ids() -> TestResult {
        let g = UuidGenerator;
        let a = g.generate(&IdGeneratorContext::default());
        let b = g.generate(&IdGeneratorContext {
            task_id: Some("t".into()),
            context_id: Some("c".into()),
        });
        assert!(!a.is_empty());
        assert_ne!(a, b);
        Ok(())
    }
}
