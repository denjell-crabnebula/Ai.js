// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Public contracts and helpers for A4P user-signature methods.

pub mod contracts;
pub mod ed25519;
pub mod webauthn;

pub use contracts::{
    A4PUserSignatureMethod, A4PUserSigner, UserSignature, UserSignatureContext, UserSigningInput,
    attach_user_signature, canonical_user_authorization_payload, sign_user_signature,
    user_authorization_challenge, verify_user_signature,
};
pub use ed25519::{ED25519_SIGNATURE_METHOD, Ed25519UserSigner, RegisteredEd25519Method, ed25519_public_jwk};
pub use webauthn::{WEBAUTHN_SIGNATURE_METHOD, WebAuthnSignatureMethod, WebAuthnUserSigner};
