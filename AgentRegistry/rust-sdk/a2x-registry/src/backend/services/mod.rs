// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Backend services: the search facade and the taxonomy tree reader.

pub mod search_service;
pub mod taxonomy_service;

pub use search_service::SearchService;
pub use taxonomy_service::TaxonomyService;
