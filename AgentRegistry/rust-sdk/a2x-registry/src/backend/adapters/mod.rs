// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Adapters wiring the optional `a2x-search` and `a2x-cluster` crates into
//! the engine and cluster traits. Each is compiled only with its feature.

#[cfg(feature = "cluster")]
pub mod cluster;
#[cfg(feature = "search")]
pub mod search;
