// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared data models across search methods (a2x, vector, traditional).

use serde::{Deserialize, Serialize};

/// A service found by search.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl SearchResult {
    pub fn new(id: impl Into<String>, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
        }
    }
}
