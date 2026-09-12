// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A4P (Agentic Authentication, Authorization and Audit Protocol) SDK.
//!
//! This crate is a Rust port of the A4P Python SDK. It provides:
//!
//! - intent and operation mandates signed by the A4P Server with Ed25519;
//! - reusable intent tokens with scope matching and atomic execution quotas;
//! - user signature methods (`ed25519` and `webauthn`) and credential stores;
//! - the `A4PServer` facade, an axum HTTP transport and a reqwest client;
//! - the local User Authorizer helpers that verify Server-signed mandates.
//!
//! Mandates and tokens are plain JSON objects ([`JsonDict`]) because every
//! signature and WebAuthn challenge is computed over their canonical JSON.

#![warn(missing_docs)]

pub mod authorization_common;
pub mod canonical;
#[cfg(feature = "client")]
pub mod client;
pub mod credential_store;
pub mod errors;
#[cfg(feature = "http-server")]
pub mod http_server;
pub mod intent;
pub mod mandate_security;
pub mod operation;
pub mod security;
pub mod server;
pub mod types;
pub mod user_authorizer;
pub mod user_signature;
pub mod util;

#[cfg(feature = "client")]
pub use client::{A4PClient, default_a4p_base_url, default_a4p_timeout};
pub use credential_store::{
    A4PCredentialStore, CREDENTIAL_STORE_SCHEMA_VERSION, InMemoryCredentialStore, JsonFileCredentialStore,
    UserCredentialRecord,
};
pub use errors::{
    A4PError, A4PProtocolError, CredentialStoreFormatError, IntentTokenUsageStoreError, MandateSecurityError,
};
#[cfg(feature = "http-server")]
pub use http_server::{A4PHTTPServer, a4p_http_host, a4p_http_port};
pub use intent::mandate::IntentDisplayTextRenderer;
#[cfg(feature = "sqlite")]
pub use intent::usage_store::SQLiteIntentTokenUsageStore;
pub use intent::usage_store::{A4PIntentTokenUsageStore, InMemoryIntentTokenUsageStore};
pub use mandate_security::{
    StaticA4PServerTrustStore, canonical_json, derive_user_authorization_challenge, mandate_identifier,
    user_authorization_challenge_base64url, verify_trusted_server_mandate,
};
pub use operation::mandate::OperationDisplayTextRenderer;
pub use server::{A4PServer, A4PServerBuilder};
pub use types::{
    IntentAuthorizationRequest, IntentAuthorizationResponse, IntentMandate, IntentToken, IntoPayload,
    JsonDict, OperationAuthorizationChallenge, OperationAuthorizationCompletionRequest,
    OperationAuthorizationRequest, OperationAuthorizationResult, OperationMandate, TokenVerificationRequest,
    TokenVerificationResponse, UserAuthorizationRequest, UserAuthorizationResponse, VerificationResult,
    to_payload,
};
pub use user_authorizer::{
    A4PUserAuthorizer, ApprovingA4PUserAuthorizer, RejectingA4PUserAuthorizer, approve_user_mandate,
    sign_user_mandate_with_signer, verify_local_user_authorization_request,
};
pub use user_signature::{
    A4PUserSignatureMethod, A4PUserSigner, Ed25519UserSigner, RegisteredEd25519Method, UserSignatureContext,
    WebAuthnSignatureMethod, WebAuthnUserSigner,
};

/// The environment variables this crate reads, with the values a safe
/// deployment accepts. Merge it into a binary's [`ap_support::env::EnvPolicy`].
pub fn env_policy() -> ap_support::env::EnvPolicy {
    use ap_support::env::{EnvPolicy, Kind, Rule, VarSpec};
    let dev_keys: &'static [&'static str] = &[
        intent::signing::INTENT_SERVER_PRIVATE_KEY_LABEL,
        operation::signing::OPERATION_SERVER_PRIVATE_KEY_LABEL,
    ];
    let mut policy = EnvPolicy::new()
        .prefix("A4P_")
        .var(
            VarSpec::new(
                "A4P_SERVER_HOST",
                Kind::Host,
                "Bind address of the A4P HTTP server",
            )
            .default("127.0.0.1")
            .rule(Rule::loopback_only()),
        )
        .var(VarSpec::new("A4P_SERVER_PORT", Kind::Port, "Bind port of the A4P HTTP server").default("8961"))
        .var(
            VarSpec::new("A4P_SERVER_BASE_URL", Kind::Url, "Base URL used by A4PClient")
                .default("http://127.0.0.1:8961")
                .rule(Rule::https_remote()),
        )
        .var(
            VarSpec::new(
                "A4P_HTTP_TIMEOUT_S",
                Kind::Float,
                "Client request timeout in seconds",
            )
            .default("300")
            .rule(Rule::float_range(1.0, 3600.0)),
        )
        .var(
            VarSpec::new("A4P_USAGE_DB_PATH", Kind::Path, "SQLite intent-token usage store")
                .default(".a4p/intent_token_usage.sqlite3")
                .rule(Rule::safe_path()),
        )
        .var(
            VarSpec::new(
                "A4P_USER_AUTHORIZER_BASE_URL",
                Kind::Url,
                "User Authorizer the examples forward to",
            )
            .default("http://localhost:8970")
            .rule(Rule::https_remote()),
        )
        .var(
            VarSpec::new(
                intent::signing::INTENT_SERVER_PRIVATE_KEY_ENV,
                Kind::Text,
                "Intent Server signing key",
            )
            .secret()
            .rule(Rule::no_development_key(dev_keys))
            .rule(Rule::required()),
        )
        .var(
            VarSpec::new(
                operation::signing::OPERATION_SERVER_PRIVATE_KEY_ENV,
                Kind::Text,
                "Operation Server signing key",
            )
            .secret()
            .rule(Rule::no_development_key(dev_keys))
            .rule(Rule::required()),
        );
    for name in ["A4P_ENV", "APP_ENV", "ENV", "PYTHON_ENV"] {
        policy = policy.var(VarSpec::new(name, Kind::Text, "Deployment tier").rule(Rule::production_tier()));
    }
    policy
}
