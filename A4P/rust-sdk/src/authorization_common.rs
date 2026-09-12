// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared internal helpers for the intent and operation authorization services.

use serde_json::Value;

use crate::errors::A4PError;
use crate::types::JsonDict;

/// Map a verification reason to a stable code with the given prefix.
///
/// This reproduces `authorization_common.error_code()` from the Python SDK.
pub fn error_code(reason: &str, prefix: &str) -> String {
    let lower = reason.to_lowercase();
    if lower.contains("expired") {
        return format!("{prefix}_EXPIRED");
    }
    if lower.contains("usage") || lower.contains("execution") || lower.contains("limit") {
        return format!("{prefix}_USAGE_EXCEEDED");
    }
    if lower.contains("signature") {
        return format!("{prefix}_SIGNATURE_INVALID");
    }
    if lower.contains("mismatch")
        || lower.contains("scope")
        || lower.contains("actions")
        || lower.contains("param")
    {
        return format!("{prefix}_SCOPE_MISMATCH");
    }
    format!("{prefix}_INVALID")
}

/// True when the signed mandate equals the pending mandate apart from `signatures.user`.
pub fn mandate_matches_pending(
    signed_mandate: &JsonDict,
    pending_mandate: &JsonDict,
    normalize: impl Fn(&JsonDict) -> Result<JsonDict, A4PError>,
) -> bool {
    let (Ok(mut signed), Ok(mut pending)) = (normalize(signed_mandate), normalize(pending_mandate)) else {
        return false;
    };
    clear_user_signature(&mut signed);
    clear_user_signature(&mut pending);
    signed == pending
}

fn clear_user_signature(mandate: &mut JsonDict) {
    let signatures = mandate
        .entry("signatures")
        .or_insert_with(|| Value::Object(JsonDict::new()));
    if let Value::Object(map) = signatures {
        map.insert("user".into(), Value::Object(JsonDict::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn error_code_mapping_follows_reason_text() -> TestResult {
        assert_eq!(error_code("Mandate has expired", "MANDATE"), "MANDATE_EXPIRED");
        assert_eq!(
            error_code("Token execution usage exceeded", "TOKEN"),
            "TOKEN_USAGE_EXCEEDED"
        );
        assert_eq!(
            error_code("User signature missing", "MANDATE"),
            "MANDATE_SIGNATURE_INVALID"
        );
        assert_eq!(
            error_code("Token subject mismatch", "TOKEN"),
            "TOKEN_SCOPE_MISMATCH"
        );
        assert_eq!(error_code("Invalid token type", "TOKEN"), "TOKEN_INVALID");
        Ok(())
    }
}
