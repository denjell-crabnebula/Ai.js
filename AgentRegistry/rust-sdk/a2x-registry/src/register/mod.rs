// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Registration module: models, per dataset file store, format validation,
//! agent card fetching and the multi dataset [`service::RegistryService`].

pub mod agent_card;
pub mod embedding;
pub mod errors;
pub mod models;
pub mod service;
pub mod store;
pub mod validation;

pub use errors::RegistryError;
pub use models::*;
pub use service::{MutationHook, MutationOp, RegistryService, ServiceChangeListener, UnhealthyCheck};
pub use store::{LeaseConfig, RegistryStore, generate_service_id};
pub use validation::{
    DEFAULT_FORMAT_CONFIG, FormatValidator, SUPPORTED_SERVICE_TYPES, ValidationResult,
    normalize_format_config, validate_agent_card, validate_service,
};

/// Bundled template for a global `user_config.json`.
pub const USER_CONFIG_EXAMPLE: &str = include_str!("user_config.example.json");
