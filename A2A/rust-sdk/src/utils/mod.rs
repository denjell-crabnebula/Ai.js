// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Utilities: identifiers, message and artifact builders, task helpers.

pub mod artifact;
pub mod helpers;
pub mod id_generator;
pub mod message;

/// Generate a random RFC 4122 version 4 UUID in lowercase hyphenated form.
pub fn generate_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub use artifact::{new_artifact, new_data_artifact, new_text_artifact};
pub use helpers::{
    append_artifact_to_task, are_modalities_compatible, build_text_artifact, create_task_obj, is_final,
    is_final_event, is_final_or_interrupted, is_interrupted, make_error, validate_or_throw,
};
pub use id_generator::{IdGenerator, IdGeneratorContext, UuidGenerator};
pub use message::{get_message_text, get_text_parts, new_agent_parts_message, new_agent_text_message};

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn uuid_is_version_4_and_lowercase() -> TestResult {
        let u = generate_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(u, u.to_lowercase());
        for i in [8, 13, 18, 23] {
            assert_eq!(&u[i..=i], "-");
        }
        assert_ne!(u, generate_uuid());
        Ok(())
    }
}
