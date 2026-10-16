// SPDX-License-Identifier: AGPL-3.0-or-later
//! Project tooling for read-to-me, run as `cargo xtask <command>`.

use std::path::PathBuf;
use std::process::ExitCode;

use xtask::{commits, docs_links, kokoro_models};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  docs-links fix     add definition anchors and turn references in the docs into links
  docs-links check   fail on broken relative links/anchors or unlinked references
  commits [RANGE]    check commit messages in RANGE (default origin/main..HEAD) against
                     the commit standards: Conventional Commits, 50/72, DCO sign-off
  kokoro-models DIR  download the pinned Kokoro model files into DIR and verify them";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repo root")
        .to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let root = repo_root();
    let result = match args.as_slice() {
        ["docs-links", "fix"] => docs_links::fix(&root).map(|report| {
            for m in &report.missing_anchors {
                eprintln!("missing anchor: {m}");
            }
            println!("{} file(s) changed", report.changed_files);
            report.missing_anchors.is_empty()
        }),
        ["docs-links", "check"] => docs_links::check(&root).map(|report| {
            for p in &report.problems {
                eprintln!("{p}");
            }
            println!(
                "{} internal links checked, {} problems",
                report.links_checked,
                report.problems.len()
            );
            report.problems.is_empty()
        }),
        ["commits"] | ["commits", _] => {
            let range = args.get(1).copied().unwrap_or("origin/main..HEAD");
            commits::check(&root, range).map(|report| {
                for (sha, subject, problems) in &report.failures {
                    eprintln!("{sha} {subject}");
                    for p in problems {
                        eprintln!("    - {p}");
                    }
                }
                println!(
                    "{} commit(s) checked in {range}, {} with problems",
                    report.checked,
                    report.failures.len()
                );
                report.failures.is_empty()
            })
        }
        ["kokoro-models", dir] => {
            kokoro_models::fetch(&root, std::path::Path::new(dir)).map(|got| {
                println!("{} file(s) downloaded; all verified", got.len());
                true
            })
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
