// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Runtime lease models, re-exported from the shared lease table under the
//! heartbeat names: `HeartbeatLease` is `Lease`, `HBState` is `LeaseState`.

pub use a2x_common::lease::{Lease as HeartbeatLease, LeaseState as HBState};
