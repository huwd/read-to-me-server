// SPDX-License-Identifier: AGPL-3.0-or-later
//! The egress guard (ADR-0005): the only way read-to-me makes outbound HTTP requests.

mod policy;

pub use policy::{DenyReason, Policy};

/// Longest URL accepted at any hop (FR-SUB-02).
pub const MAX_URL_LEN: usize = 2_048;
