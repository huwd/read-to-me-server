// SPDX-License-Identifier: AGPL-3.0-or-later
//! GitHub-compatible heading anchors, matching the `github-slugger` npm package
//! (lowercase, drop everything except letters, marks, numbers, connector
//! punctuation, spaces and hyphens, then spaces to hyphens; duplicates get `-1`, `-2`, …).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

static STRIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{M}\p{N}\p{Pc} -]").expect("valid regex"));

/// Slug for a single heading, ignoring duplicates.
pub fn slug(heading: &str) -> String {
    STRIP
        .replace_all(&heading.to_lowercase(), "")
        .replace(' ', "-")
}

/// Stateful slugger for one document, so repeated headings get unique anchors.
#[derive(Default)]
pub struct Slugger {
    occurrences: HashMap<String, usize>,
}

impl Slugger {
    pub fn slug(&mut self, heading: &str) -> String {
        let base = slug(heading);
        let mut result = base.clone();
        while self.occurrences.contains_key(&result) {
            let n = self.occurrences.get_mut(&base).expect("base seen first");
            *n += 1;
            result = format!("{base}-{n}");
        }
        self.occurrences.insert(result.clone(), 0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_numbered() {
        let mut s = Slugger::default();
        assert_eq!(s.slug("Intro"), "intro");
        assert_eq!(s.slug("Intro"), "intro-1");
        assert_eq!(s.slug("Intro"), "intro-2");
        assert_eq!(s.slug("intro-1"), "intro-1-1");
    }
}
