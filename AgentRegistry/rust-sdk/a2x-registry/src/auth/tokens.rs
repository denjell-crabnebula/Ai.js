// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Token generation, hashing and prefix helpers.
//!
//! Tokens look like `a2x_pat_<43 char base64url>`: 32 bytes of OS
//! randomness. Only `hash_token(plaintext)` is ever persisted.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const TOKEN_PREFIX: &str = "a2x_pat_";

/// Display prefix length: four characters after the prefix.
pub const DISPLAY_PREFIX_LEN: usize = TOKEN_PREFIX.len() + 4;

/// Generate a fresh plaintext API key.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// `sha256(token)` as lowercase hex.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Short prefix used for display and audit entries.
pub fn token_prefix(token: &str) -> String {
    token.chars().take(DISPLAY_PREFIX_LEN).collect()
}

/// Constant time string comparison (defense in depth on the hot path).
pub fn constant_time_equals(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn token_format() -> TestResult {
        let t = generate_token();
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 43);
        assert_ne!(t, generate_token());
        assert_eq!(hash_token("x").len(), 64);
        assert_eq!(token_prefix(&t).len(), 12);
        assert!(constant_time_equals("abc", "abc"));
        assert!(!constant_time_equals("abc", "abd"));
        assert!(!constant_time_equals("abc", "ab"));
        Ok(())
    }
}
