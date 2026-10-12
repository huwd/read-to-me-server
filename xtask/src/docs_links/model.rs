// SPDX-License-Identifier: AGPL-3.0-or-later
//! The docs estate: which files are covered, their headings, and the anchors they define.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::slug::Slugger;

static ANCHOR_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a id="([^"]+)"></a>"#).expect("valid regex"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("valid regex"));
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#{1,6} ").expect("valid regex"));
static SECTION_NUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)*)\.?\s").expect("valid regex"));
static FR_GROUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+\.\d+ .*\(([A-Z]+)\b").expect("valid regex"));

pub const ADR_INDEX: &str = "docs/adr/README.md";

/// Is this a Markdown heading line?
pub fn is_heading(line: &str) -> bool {
    HEADING.is_match(line)
}

/// Is this line a code fence delimiter?
pub fn is_fence(line: &str) -> bool {
    line.starts_with("```")
}

/// Heading text as GitHub renders it for slugging.
pub fn clean_heading(h: &str) -> String {
    let h = ANCHOR_ID.replace_all(h, "");
    let h = LINK.replace_all(&h, "$1");
    h.replace('`', "")
        .replace("**", "")
        .replace("~~", "")
        .trim()
        .to_string()
}

/// Repo-relative paths (always `/`-separated) of every Markdown file the tools cover.
pub fn doc_files(root: &Path) -> io::Result<Vec<String>> {
    let mut files = vec!["README.md".to_string(), "CONTRIBUTING.md".to_string()];
    for dir in ["docs", "docs/adr"] {
        let mut names: Vec<String> = std::fs::read_dir(root.join(dir))?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".md"))
            .map(|n| format!("{dir}/{n}"))
            .collect();
        names.sort();
        files.extend(names);
    }
    files.retain(|f| root.join(f).is_file());
    Ok(files)
}

/// Relative link target from `src` to `dst` (both repo-relative), with optional fragment.
pub fn rel(src: &str, dst: &str, anchor: Option<&str>) -> String {
    let frag = anchor.map(|a| format!("#{a}")).unwrap_or_default();
    if src == dst {
        return frag;
    }
    let mut src_dir: Vec<&str> = src.split('/').collect();
    src_dir.pop();
    let dst_parts: Vec<&str> = dst.split('/').collect();
    let common = src_dir
        .iter()
        .zip(&dst_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<&str> = vec![".."; src_dir.len() - common];
    parts.extend(&dst_parts[common..]);
    format!("{}{frag}", parts.join("/"))
}

/// Resolve a relative link `target` (without fragment) written in `src` to a repo-relative path.
pub fn resolve(src: &str, target: &str) -> String {
    let mut parts: Vec<&str> = src.split('/').collect();
    parts.pop();
    for p in target.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Everything the link tools need to know about the docs.
pub struct Docs {
    pub root: PathBuf,
    pub files: Vec<String>,
    /// "01" → "docs/01-product-spec.md"
    pub doc: BTreeMap<String, String>,
    /// "0010" → "docs/adr/0010-….md"
    pub adr: BTreeMap<String, String>,
    pub anchors: HashMap<String, HashSet<String>>,
    /// file → section number ("5.4") → slug
    pub sections: HashMap<String, HashMap<String, String>>,
    /// FR prefix ("EXT") → slug of its section in doc 01
    pub fr_groups: HashMap<String, String>,
    /// slug of the "Deferred" section in doc 07
    pub deferred: Option<String>,
}

impl Docs {
    pub fn load(root: &Path) -> io::Result<Self> {
        let files = doc_files(root)?;
        let mut doc = BTreeMap::new();
        let mut adr = BTreeMap::new();
        for f in &files {
            let name = f.rsplit('/').next().unwrap_or(f);
            let starts_digit = name.starts_with(|c: char| c.is_ascii_digit());
            if f.starts_with("docs/adr/") && starts_digit && name.len() > 4 {
                adr.insert(name[..4].to_string(), f.clone());
            } else if f.starts_with("docs/") && !f.starts_with("docs/adr/") && starts_digit {
                doc.insert(name[..2].to_string(), f.clone());
            }
        }
        let mut docs = Docs {
            root: root.to_path_buf(),
            files,
            doc,
            adr,
            anchors: HashMap::new(),
            sections: HashMap::new(),
            fr_groups: HashMap::new(),
            deferred: None,
        };
        docs.reindex()?;
        Ok(docs)
    }

    /// Recompute headings, slugs and anchors from the files on disk.
    pub fn reindex(&mut self) -> io::Result<()> {
        self.anchors.clear();
        self.sections.clear();
        self.fr_groups.clear();
        self.deferred = None;
        let d01 = self.doc.get("01").cloned();
        let d07 = self.doc.get("07").cloned();
        for f in &self.files {
            let text = self.read(f)?;
            let mut slugger = Slugger::default();
            let mut ids: HashSet<String> = ANCHOR_ID
                .captures_iter(&text)
                .map(|c| c[1].to_string())
                .collect();
            let mut sections = HashMap::new();
            let mut fence = false;
            for line in text.lines() {
                if is_fence(line) {
                    fence = !fence;
                    continue;
                }
                if fence || !is_heading(line) {
                    continue;
                }
                let h = clean_heading(line.trim_start_matches('#').trim());
                let s = slugger.slug(&h);
                if let Some(c) = SECTION_NUM.captures(&h) {
                    sections
                        .entry(c[1].to_string())
                        .or_insert_with(|| s.clone());
                }
                if Some(f) == d01.as_ref()
                    && let Some(c) = FR_GROUP.captures(&h)
                {
                    self.fr_groups.insert(c[1].to_string(), s.clone());
                }
                if Some(f) == d07.as_ref() && h.contains("Deferred") && self.deferred.is_none() {
                    self.deferred = Some(s.clone());
                }
                ids.insert(s);
            }
            self.anchors.insert(f.clone(), ids);
            self.sections.insert(f.clone(), sections);
        }
        Ok(())
    }

    pub fn read(&self, f: &str) -> io::Result<String> {
        std::fs::read_to_string(self.root.join(f))
    }

    pub fn write(&self, f: &str, text: &str) -> io::Result<()> {
        std::fs::write(self.root.join(f), text)
    }

    pub fn has_anchor(&self, file: &str, anchor: &str) -> bool {
        self.anchors.get(file).is_some_and(|a| a.contains(anchor))
    }

    pub fn section(&self, file: &str, number: &str) -> Option<String> {
        let number = number.trim_start_matches('§');
        let number = number.split('–').next().unwrap_or(number);
        self.sections.get(file)?.get(number).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths() {
        assert_eq!(rel("docs/01-a.md", "docs/03-b.md", Some("x")), "03-b.md#x");
        assert_eq!(
            rel("docs/adr/0001-a.md", "docs/03-b.md", None),
            "../03-b.md"
        );
        assert_eq!(
            rel("docs/adr/0001-a.md", "deny.toml", None),
            "../../deny.toml"
        );
        assert_eq!(
            rel("README.md", "docs/adr/README.md", None),
            "docs/adr/README.md"
        );
        assert_eq!(rel("docs/01-a.md", "docs/01-a.md", Some("y")), "#y");
    }

    #[test]
    fn resolve_paths() {
        assert_eq!(resolve("docs/adr/0001-a.md", "../03-b.md"), "docs/03-b.md");
        assert_eq!(
            resolve("docs/adr/0001-a.md", "../../deny.toml"),
            "deny.toml"
        );
        assert_eq!(resolve("README.md", "docs/01-a.md"), "docs/01-a.md");
    }

    #[test]
    fn heading_cleanup() {
        assert_eq!(
            clean_heading(r#"<a id="ac-sub"></a>AC-SUB — `code` and [link](x.md) **bold**"#),
            "AC-SUB — code and link bold"
        );
    }
}
