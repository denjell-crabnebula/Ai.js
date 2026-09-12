// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Synthetic admin context used by the sweeper for hard deletes, so the
//! same code path as an admin `DELETE /services/{sid}` runs.

use a2x_common::AuthContext;

/// Principal id recorded for sweeper driven deletes.
pub const SYSTEM_PRINCIPAL_ID: &str = "_system";

/// The synthetic admin context.
pub fn system_ctx() -> AuthContext {
    AuthContext::admin(SYSTEM_PRINCIPAL_ID)
}
