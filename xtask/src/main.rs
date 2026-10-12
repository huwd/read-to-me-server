// SPDX-License-Identifier: AGPL-3.0-or-later
//! Project tooling for read-to-me, run as `cargo xtask <command>`.

use std::path::PathBuf;
use std::process::ExitCode;

use xtask::docs_links;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  docs-links fix     add definition anchors and turn references in the docs into links
  docs-links check   fail on broken relative links/anchors or unlinked references";

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
