// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Artifact builders, the port of `src/server/utils_artifact.h`.

use crate::types::{Artifact, Part};

use super::generate_uuid;

/// Build an artifact with a generated id.
pub fn new_artifact(parts: Vec<Part>, name: &str, description: &str) -> Artifact {
    Artifact {
        artifact_id: generate_uuid(),
        parts,
        name: Some(name.to_string()),
        description: if description.is_empty() {
            None
        } else {
            Some(description.to_string())
        },
        ..Default::default()
    }
}

/// Build a text artifact with media type `text/plain`.
pub fn new_text_artifact(name: &str, text: &str, description: &str) -> Artifact {
    new_artifact(
        vec![Part::text(text).with_media_type("text/plain")],
        name,
        description,
    )
}

/// Build a data artifact with media type `application/octet-stream`.
pub fn new_data_artifact(name: &str, data: &str, description: &str) -> Artifact {
    new_artifact(
        vec![
            Part::data(serde_json::Value::String(data.to_string()))
                .with_media_type("application/octet-stream"),
        ],
        name,
        description,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn new_artifact_sets_fields() -> TestResult {
        let a = new_artifact(vec![Part::text("x")], "n", "d");
        assert!(!a.artifact_id.is_empty());
        assert_eq!(a.name.as_deref(), Some("n"));
        assert_eq!(a.description.as_deref(), Some("d"));
        let b = new_artifact(vec![], "n", "");
        assert!(b.description.is_none());
        Ok(())
    }

    #[test]
    fn text_and_data_artifacts() -> TestResult {
        let t = new_text_artifact("n", "hello", "");
        assert_eq!(t.parts[0].text.as_deref(), Some("hello"));
        assert_eq!(t.parts[0].media_type.as_deref(), Some("text/plain"));
        let d = new_data_artifact("n", "bytes", "desc");
        assert_eq!(d.parts[0].data, Some(serde_json::Value::String("bytes".into())));
        assert_eq!(d.parts[0].media_type.as_deref(), Some("application/octet-stream"));
        assert_eq!(d.description.as_deref(), Some("desc"));
        Ok(())
    }
}
