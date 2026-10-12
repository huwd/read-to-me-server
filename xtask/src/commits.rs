// SPDX-License-Identifier: AGPL-3.0-or-later
//! `commits`: check commit messages against the project's commit standards.
//!
//! - Conventional Commits subject: `type(scope)!: description`, with the allowed types
//! - subject at most 50 characters, no trailing full stop
//! - a blank line between subject and body
//! - body lines wrapped at 72 characters (trailers and lines containing URLs exempt)
//! - a DCO `Signed-off-by: Name <email>` trailer (see CONTRIBUTING.md)
//!
//! Merge commits are skipped. Commits authored by bots in [`BOT_AUTHORS`] (Dependabot)
//! only get the subject format and type check: they can't add a DCO sign-off, and their
//! bodies carry release notes. Judgement calls the standard also makes (imperative mood,
//! a body for non-trivial changes, one logical change per commit) are left to review.

use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

pub const TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore",
];
pub const SUBJECT_MAX: usize = 50;
/// Authors exempt from sign-off, length and body rules (subject format still applies).
pub const BOT_AUTHORS: &[&str] = &["dependabot[bot]"];
pub const BODY_WIDTH: usize = 72;

static SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?<type>[a-z]+)(?:\((?<scope>[a-z0-9][a-z0-9-]*)\))?!?: (?<desc>\S.*)$")
        .expect("valid regex")
});
static TRAILER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9-]*: \S").expect("valid regex"));
static SIGNOFF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^Signed-off-by: [^<\n]+ <[^@>\s]+@[^>\s]+>$").expect("valid regex")
});

/// Subject format and type only (the part bots must also follow).
pub fn lint_subject_format(subject: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match SUBJECT.captures(subject) {
        None => problems.push(format!(
            "subject must be `type(scope): description` (got {subject:?})"
        )),
        Some(c) => {
            let ty = &c["type"];
            if !TYPES.contains(&ty) {
                problems.push(format!(
                    "type `{ty}` not allowed; use one of {}",
                    TYPES.join(", ")
                ));
            }
        }
    }
    problems
}

/// Problems with one commit message (empty when it conforms).
pub fn lint_message(message: &str) -> Vec<String> {
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or("").trim_end();
    let mut problems = lint_subject_format(subject);
    let len = subject.chars().count();
    if len > SUBJECT_MAX {
        problems.push(format!(
            "subject is {len} characters; maximum is {SUBJECT_MAX}"
        ));
    }
    if subject.ends_with('.') {
        problems.push("subject must not end with a full stop".to_string());
    }

    let rest: Vec<&str> = lines.collect();
    if let Some(first) = rest.first()
        && !first.trim().is_empty()
    {
        problems.push("leave a blank line between the subject and the body".to_string());
    }
    for (i, line) in rest.iter().enumerate() {
        let width = line.chars().count();
        let exempt = TRAILER.is_match(line) || line.contains("://");
        if width > BODY_WIDTH && !exempt {
            problems.push(format!(
                "body line {} is {width} characters; wrap at {BODY_WIDTH}",
                i + 2
            ));
        }
    }

    if !SIGNOFF.is_match(message) {
        problems.push("missing DCO trailer `Signed-off-by: Name <email>`".to_string());
    }
    problems
}

pub struct CommitReport {
    pub checked: usize,
    /// (short sha, subject, problems)
    pub failures: Vec<(String, String, Vec<String>)>,
}

/// Check every non-merge commit in `range` (anything `git log` accepts, e.g. `base..head`).
pub fn check(root: &Path, range: &str) -> io::Result<CommitReport> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--no-merges", "--format=%h%x00%an%x00%B%x1e", range])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git log {range} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut report = CommitReport {
        checked: 0,
        failures: Vec::new(),
    };
    for record in text.split('\x1e') {
        let record = record.trim_start_matches('\n');
        let mut fields = record.splitn(3, '\0');
        let (Some(sha), Some(author), Some(message)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        report.checked += 1;
        let problems = if BOT_AUTHORS.contains(&author) {
            lint_subject_format(message.lines().next().unwrap_or("").trim_end())
        } else {
            lint_message(message.trim_end())
        };
        if !problems.is_empty() {
            let subject = message.lines().next().unwrap_or("").to_string();
            report.failures.push((sha.to_string(), subject, problems));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNOFF_LINE: &str = "Signed-off-by: Huw Diprose <mail@huwdiprose.co.uk>";

    fn msg(subject: &str, body: &str) -> String {
        format!("{subject}\n\n{body}\n\n{SIGNOFF_LINE}")
    }

    #[test]
    fn accepts_conforming_messages() {
        for subject in [
            "docs(adr): record SQLite as the only datastore",
            "chore: initialise repository",
            "feat(api)!: drop legacy token format",
            "build(deps): add cargo-deny licence allow-list",
        ] {
            assert_eq!(
                lint_message(&msg(subject, "Why it matters.")),
                Vec::<String>::new(),
                "{subject}"
            );
        }
        // Trailers and URLs may exceed the body width.
        let long_url = format!("See https://example.com/{}", "x".repeat(80));
        let m = format!(
            "ci: add checks\n\n{long_url}\n\nClaude-Session: https://claude.ai/code/session_0123456789abcdefghijklmnopqrstuv\n{SIGNOFF_LINE}"
        );
        assert!(lint_message(&m).is_empty(), "{:?}", lint_message(&m));
    }

    #[test]
    fn rejects_bad_type_and_format() {
        assert!(
            lint_message(&msg("tools: add things", "x"))[0].contains("type `tools` not allowed")
        );
        assert!(lint_message(&msg("Add things", "x"))[0].contains("subject must be"));
        assert!(lint_message(&msg("docs(ADR): caps scope", "x"))[0].contains("subject must be"));
        assert!(lint_message(&msg("docs:no space", "x"))[0].contains("subject must be"));
    }

    #[test]
    fn rejects_long_subject_and_full_stop() {
        let p = lint_message(&msg(
            "docs: initial product specification, viability and more",
            "x",
        ));
        assert!(p.iter().any(|p| p.contains("maximum is 50")), "{p:?}");
        let p = lint_message(&msg("docs: add spec.", "x"));
        assert!(p.iter().any(|p| p.contains("full stop")), "{p:?}");
    }

    #[test]
    fn rejects_body_problems() {
        let p = lint_message(&format!("docs: add spec\nno blank line\n\n{SIGNOFF_LINE}"));
        assert!(p.iter().any(|p| p.contains("blank line")), "{p:?}");
        let p = lint_message(&msg("docs: add spec", &"word ".repeat(20)));
        assert!(p.iter().any(|p| p.contains("wrap at 72")), "{p:?}");
    }

    #[test]
    fn bot_subjects_only_need_the_format() {
        assert!(
            lint_subject_format(
                "build(deps): bump the cargo group across 1 directory with 3 updates"
            )
            .is_empty()
        );
        assert!(!lint_subject_format("Bump regex from 1.11.1 to 1.11.2").is_empty());
    }

    #[test]
    fn requires_signoff() {
        let p = lint_message("docs: add spec\n\nWhy.");
        assert!(p.iter().any(|p| p.contains("Signed-off-by")), "{p:?}");
        let p = lint_message("docs: add spec\n\nWhy.\n\nSigned-off-by: <mail@example.com>");
        assert!(p.iter().any(|p| p.contains("Signed-off-by")), "{p:?}");
    }
}
