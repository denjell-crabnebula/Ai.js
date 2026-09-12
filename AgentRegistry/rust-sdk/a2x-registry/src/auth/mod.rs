// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Static API key authentication.
//!
//! Per namespace opt-in: a registry stays fully anonymous until
//! `a2x-registry auth init` runs, and a namespace stays anonymous unless it
//! was created with `auth_required=true`. The store, models and token
//! helpers are framework free; [`extractors`] and [`router`] carry the
//! axum specifics.

pub mod cli;
pub mod errors;
pub mod extractors;
pub mod models;
pub mod router;
pub mod store;
pub mod tokens;

pub use errors::{AuthenticationError, AuthorizationError};
pub use extractors::{Authorize, RequireAdmin, RequireAdminOrAnon, RequireAdminStrict, RequirePrincipal};
pub use models::{ApiKey, Principal};
pub use store::{AuthStore, default_data_dir};
pub use tokens::{TOKEN_PREFIX, generate_token, hash_token, token_prefix};
