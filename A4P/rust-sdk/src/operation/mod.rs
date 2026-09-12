// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! One-time operation authorization domain: mandates and the service.

pub mod mandate;
pub mod service;
pub mod signing;

pub use mandate::{
    CreateOperationMandate, DEFAULT_MANDATE_VALIDITY_SECONDS, OperationDisplayTextRenderer,
    VerifyOperationMandate, create_operation_mandate, normalize_operation, normalize_operation_mandate,
    operation_mandate_core_payload, operation_user_signature_context, sign_server_mandate,
    verify_operation_mandate_for_completion,
};
pub use service::OperationAuthorizationService;
pub use signing::{operation_server_signing_key, operation_server_trusted_key};
