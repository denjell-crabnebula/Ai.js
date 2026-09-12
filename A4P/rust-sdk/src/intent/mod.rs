// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Intent authorization domain: mandates, scope matching, tokens, usage and the service.

pub mod mandate;
pub mod scope;
pub mod service;
pub mod signing;
pub mod token;
pub mod usage_store;

pub use mandate::{
    CreateIntentMandate, DEFAULT_INTENT_MANDATE_VALIDITY_SECONDS, IntentDisplayTextRenderer,
    VerifyIntentMandate, create_intent_mandate, intent_mandate_core_payload, intent_user_signature_context,
    normalize_intent_mandate, sign_server_mandate, verify_intent_mandate,
};
pub use scope::{normalize_execution_policy, normalize_intent_scope, params_match_intent_scope};
pub use service::IntentAuthorizationService;
pub use signing::{intent_server_signing_key, intent_server_trusted_key};
pub use token::{VerifyIntentToken, issue_intent_token, params_match_intent_token, verify_intent_token};
#[cfg(feature = "sqlite")]
pub use usage_store::SQLiteIntentTokenUsageStore;
pub use usage_store::{
    A4PIntentTokenUsageStore, DEFAULT_INTENT_TOKEN_USAGE_DB_PATH, InMemoryIntentTokenUsageStore,
    default_intent_token_usage_db_path,
};
