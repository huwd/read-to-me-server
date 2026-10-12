// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fs;
use std::path::{Path, PathBuf};

use xtask::docs_links::{self, slug};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Slugs must match the `github-slugger` npm package that GitHub's rendering follows.
/// Cases were generated from github-slugger 2.0.0 (every docs heading plus edge cases).
#[test]
fn slugs_match_github_slugger() {
    let fixture = include_str!("fixtures/github_slugger_cases.jsonl");
    let mut n = 0;
    for line in fixture.lines() {
        let [heading, expected]: [String; 2] = serde_json::from_str(line).unwrap();
        assert_eq!(slug::slug(&heading), expected, "heading: {heading:?}");
        n += 1;
    }
    assert!(n > 100, "fixture unexpectedly small: {n}");
}

#[test]
fn repo_docs_pass_check() {
    let report = docs_links::check(&repo_root()).unwrap();
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    assert!(report.links_checked > 100);
}

/// Copy a directory tree, skipping build output and git metadata.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if matches!(entry.file_name().to_str(), Some("target" | ".git")) {
            continue;
        }
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dst);
        } else {
            fs::copy(entry.path(), dst).unwrap();
        }
    }
}

/// A scratch copy of the repository (so every link target exists), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = repo_root();
        copy_tree(&root, &dir);
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fix_is_idempotent_on_repo_docs() {
    let scratch = Scratch::new("idempotent");
    let report = docs_links::fix(&scratch.0).unwrap();
    assert_eq!(report.changed_files, 0);
    assert!(
        report.missing_anchors.is_empty(),
        "{:#?}",
        report.missing_anchors
    );
}

#[test]
fn fix_links_new_references_and_check_catches_them() {
    let scratch = Scratch::new("new-refs");
    let doc = scratch.0.join("docs/02-product-viability.md");
    let mut text = fs::read_to_string(&doc).unwrap();
    text.push_str(
        "\nSee ADR-0005, FR-FET-07/08, NFR-P1/P4, AC-SUB-05, Q7, T1190, AML.T0051, CWE-918, \
         doc 03 §6 and `deny.toml`; RFC 9309 §2 and GPL-3.0 §5 stay plain.\n",
    );
    fs::write(&doc, &text).unwrap();

    let before = docs_links::check(&scratch.0).unwrap();
    assert!(before.problems.iter().any(|p| p.contains("'ADR-0005'")));
    assert!(before.problems.iter().any(|p| p.contains("'T1190'")));

    let report = docs_links::fix(&scratch.0).unwrap();
    assert_eq!(report.changed_files, 1);
    let fixed = fs::read_to_string(&doc).unwrap();
    let last = fixed.trim_end().lines().last().unwrap();
    for expected in [
        "[ADR-0005](adr/0005-egress-guard.md)",
        "[FR-FET-07](01-product-spec.md#fr-fet-07)/[08](01-product-spec.md#fr-fet-08)",
        "[NFR-P1](03-architecture-and-nfrs.md#nfr-p1)/[P4](03-architecture-and-nfrs.md#nfr-p4)",
        "[AC-SUB-05](01-product-spec.md#ac-sub)",
        "[Q7](07-roadmap-and-open-questions.md#q7)",
        "[T1190](06-security-and-pentest.md#t1190)",
        "[AML.T0051](06-security-and-pentest.md#aml-t0051)",
        "[CWE-918](https://cwe.mitre.org/data/definitions/918.html)",
        "[doc 03 §6](03-architecture-and-nfrs.md#6-re-use--caching-design-per-instance)",
        "[`deny.toml`](../deny.toml)",
        "RFC 9309 §2 and GPL-3.0 §5 stay plain",
    ] {
        assert!(last.contains(expected), "missing {expected:?} in {last}");
    }

    let after = docs_links::check(&scratch.0).unwrap();
    assert!(after.problems.is_empty(), "{:#?}", after.problems);
}

#[test]
fn check_reports_broken_anchor() {
    let scratch = Scratch::new("broken");
    let doc = scratch.0.join("docs/02-product-viability.md");
    let mut text = fs::read_to_string(&doc).unwrap();
    text.push_str("\n[broken](03-architecture-and-nfrs.md#nope) and [gone](nowhere.md)\n");
    fs::write(&doc, &text).unwrap();
    let report = docs_links::check(&scratch.0).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("missing anchor 03-architecture-and-nfrs.md#nope"))
    );
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("missing file nowhere.md"))
    );
}
