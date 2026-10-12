// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keep the planning docs cross-linked (see `xtask/README.md`).

mod check;
mod linkify;
pub mod model;
pub mod slug;

use std::io;
use std::path::Path;

pub use check::CheckReport;
pub use linkify::FixReport;

/// Add definition anchors and turn references into relative links, in place.
pub fn fix(root: &Path) -> io::Result<FixReport> {
    let mut docs = model::Docs::load(root)?;
    linkify::fix(&mut docs)
}

/// Report broken relative links/anchors and references that aren't links.
pub fn check(root: &Path) -> io::Result<CheckReport> {
    let docs = model::Docs::load(root)?;
    check::check(&docs)
}
