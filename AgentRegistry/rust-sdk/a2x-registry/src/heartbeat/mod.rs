// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Service heartbeat leases.
//!
//! Per namespace opt-in: a namespace accepts `lease_ttl` only when its
//! `lease_config.json` has `enabled: true`. The store and sweeper are
//! framework free; [`router`] carries the axum endpoints.

pub mod errors;
pub mod models;
pub mod router;
pub mod store;
pub mod sweeper;
pub mod system_ctx;

pub use errors::{HeartbeatError, HeartbeatErrorCode};
pub use models::{HBState, HeartbeatLease};
pub use store::{HeartbeatStore, LeaseConfigProvider};
pub use sweeper::{HardDeleter, HeartbeatSweeper};
pub use system_ctx::system_ctx;
