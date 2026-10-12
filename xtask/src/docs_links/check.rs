// SPDX-License-Identifier: AGPL-3.0-or-later
//! `docs-links check`: broken relative links/anchors and unlinked references.

use std::io;
use std::sync::LazyLock;

use regex::Regex;

use super::model::{Docs, is_fence, is_heading, resolve};

static LINK_TARGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\]\(([^)\s]+)\)").expect("valid regex"));
static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]+:").expect("valid regex"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]*\]\([^)]*\)").expect("valid regex"));
static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]*`").expect("valid regex"));
/// A definition sits right after its anchor, so the anchor and the next token are skipped.
static DEFINITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a id="[^"]*"></a>\S*"#).expect("valid regex"));
static UNLINKED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"ADR-\d{4}|\bN?FR-[A-Z]+(?:-\w+)?|\bAC-[A-Z]+|\bT\d{4}\b|AML\.T\d{4}|CWE-\d+|\b[Dd]ocs? 0\d|\bQ[1-9]\b",
    )
    .expect("valid regex")
});

pub struct CheckReport {
    pub links_checked: usize,
    pub problems: Vec<String>,
}

pub fn check(docs: &Docs) -> io::Result<CheckReport> {
    let mut problems = Vec::new();
    let mut links_checked = 0;
    for f in &docs.files {
        let text = docs.read(f)?;
        let mut fence = false;
        for (i, line) in text.lines().enumerate() {
            let n = i + 1;
            if is_fence(line) {
                fence = !fence;
                continue;
            }
            if fence || is_heading(line) {
                continue;
            }
            for c in LINK_TARGET.captures_iter(line) {
                let target = &c[1];
                if SCHEME.is_match(target) {
                    continue;
                }
                links_checked += 1;
                let (path, frag) = target.split_once('#').unwrap_or((target, ""));
                let dst = if path.is_empty() {
                    f.clone()
                } else {
                    resolve(f, path)
                };
                if !docs.root.join(&dst).exists() {
                    problems.push(format!("{f}:{n}: missing file {target}"));
                } else if !frag.is_empty() && dst.ends_with(".md") && !docs.has_anchor(&dst, frag) {
                    problems.push(format!("{f}:{n}: missing anchor {target}"));
                }
            }
            let bare = LINK.replace_all(line, "");
            let bare = CODE_SPAN.replace_all(&bare, "");
            let bare = DEFINITION.replace_all(&bare, "");
            for m in UNLINKED.find_iter(&bare) {
                problems.push(format!("{f}:{n}: unlinked reference '{}'", m.as_str()));
            }
        }
    }
    Ok(CheckReport {
        links_checked,
        problems,
    })
}
