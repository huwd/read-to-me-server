# xtask

Project tooling for read-to-me, written in Rust and run through Cargo (the [`cargo xtask`](https://github.com/matklad/cargo-xtask) pattern). It's never shipped. Using Rust means CI needs only the Rust toolchain for our own tooling ([ADR-0002](../docs/adr/0002-rust-modular-monolith.md), Amendment 1).

```sh
cargo xtask docs-links fix     # add definition anchors; turn references in the docs into links
cargo xtask docs-links check   # what CI runs: broken links/anchors or unlinked references fail
cargo xtask commits [RANGE]    # commit standards + DCO for RANGE (default origin/main..HEAD)
cargo test -p xtask            # tool's own tests
```

## commits

Checks every non-merge commit in a git range against the commit standards in [CONTRIBUTING.md](../CONTRIBUTING.md):
- a Conventional Commits subject with an allowed type and optional lowercase scope;
- a subject of at most 50 characters, with no trailing full stop;
- a blank line after the subject;
- body lines of at most 72 characters (trailers and lines containing URLs are exempt);
- a `Signed-off-by: Name <email>` trailer.

Imperative mood, whether a body is needed, and one-change-per-commit are judgement calls left to review. CI runs it over the pull request's commits.

## docs-links

Covers `README.md`, `CONTRIBUTING.md`, `docs/*.md` and `docs/adr/*.md`.

**`fix`** runs in two steps.
1. **Anchors:** it inserts `<a id>` anchors at definitions:
   - FR items (doc 01), NFR and assumption rows (doc 03), open-question rows (doc 07);
   - follow-up rows in ADR-0010;
   - AC group headings (doc 01);
   - threat rows (doc 06), whose ATT&CK and ATLAS IDs also link to MITRE.
2. **Links:** it turns every reference outside headings and code into a relative link:
   - ADRs (including `§N` and `Fn`), docs and sections, bare `§N` within a file;
   - FR, NFR, AC, Q and assumptions;
   - threats (to doc 06, or to MITRE inside doc 06) and CWE IDs;
   - code spans naming `deny.toml`, `CONTRIBUTING.md` or `docs/adr/`.

It's idempotent, and exits non-zero if a reference points at an anchor that doesn't exist.

**`check`** fails if a relative link points at a missing file or anchor, or if any of those references outside headings and code isn't a link.

**Anchors** are computed like GitHub's renderer (the `github-slugger` algorithm). `tests/fixtures/github_slugger_cases.jsonl` holds 196 cases generated from github-slugger 2.0.0 and is checked in the tests.

### Conventions it relies on
- Requirements are defined as `- **FR-XXX-NN**` list items (doc 01) and `| NFR-… |` table rows (doc 03).
- Open questions are `| Qn |` rows (doc 07); ADR-0010 follow-ups are `| Fn |` rows.
- Threat techniques are the first cell of the doc 06 tables.
- A new ID scheme or reference form needs a rule in `src/docs_links/linkify.rs` and in the `UNLINKED` pattern in `src/docs_links/check.rs`, plus a test.
