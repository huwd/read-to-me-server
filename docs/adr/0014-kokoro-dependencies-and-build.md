# ADR-0014: Kokoro's pronunciation fork and native build

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-tts-engine-rust-kokoro.md) (Kokoro in Rust; Amendment 1 chose `misaki-rs` with upstream lexicons), [ADR-0010](0010-licence-and-third-party-policy.md) (licence policy), [issue #18](https://github.com/huwd/read-to-me-server/issues/18) (pronunciation parity), [issue #23](https://github.com/huwd/read-to-me-server/issues/23) (TTS process), [doc 05](../05-quality-cicd-observability.md) (CI)

## Context

[ADR-0007](0007-tts-engine-rust-kokoro.md) Amendment 1 settled *what* the Kokoro engine uses: ONNX Runtime through the `ort` crate, and `misaki-rs` for pronunciation, carrying the **upstream Misaki lexicons** plus two patches (keep "—" and "…" as tokens; trim the full stop espeak appends to fallback words). The Phase 0 spike made that work with a script that copied the crate from the Cargo registry and the lexicons from a Python virtual environment. That's fine for a spike but not for the product. Two questions remained.

**1. Where the patched `misaki-rs` lives.**
- The upstream lexicons (`gb_gold`, `gb_silver`, `us_gold`, `us_silver`) come to about 12 MB of JSON. The crate's tagger weights add 5.5 MB.
- **Licences:**
  - Misaki and its data are Apache-2.0 and `misaki-rs` is MIT, both on the [ADR-0010](0010-licence-and-third-party-policy.md) allow-list;
  - its espeak-ng fallback (`espeak-rs`) is GPL-3.0-or-later, which is compatible with this project's AGPL-3.0-or-later.
- Issue #18 plans further rule patches "in a `misaki-rs` fork, offered upstream".

**2. How the native build fits CI and everyday work.**
- **`ort` downloads ONNX Runtime at build time.** It verifies the archive against a SHA-256 recorded in the crate itself (`ort-sys`'s `dist.tsv`), so a version pinned in `Cargo.lock` pins the binaries by hash, as [ADR-0007](0007-tts-engine-rust-kokoro.md) requires. The binaries are pyke's builds of Microsoft's ONNX Runtime, served from `cdn.pyke.io`.
- **`espeak-rs` builds espeak-ng from source,** which needs cmake and libclang.
- Together they add minutes to a clean build, and system packages that most contributors and the default CI job don't otherwise need.

## Decision

### `misaki-rs` is a fork on GitHub, pinned by commit
- **A fork of `misaki-rs` under the owner's GitHub account** carries the upstream Misaki lexicons and our patches as ordinary commits, each one offered upstream where it makes sense.
- **This workspace depends on it by git URL and pinned `rev`.** `Cargo.lock` records the exact commit.
- **[`deny.toml`](../../deny.toml) allows exactly that one git source** (`allow-git`); every other git source is still refused.
- **#18's pronunciation patches land in the fork**, with tests there, then the pinned `rev` moves forward here.

### Kokoro is behind a `kokoro` cargo feature
- **The Kokoro engine and its dependencies** (`ort`, the `misaki-rs` fork, espeak-ng) compile only with the `kokoro` feature.
- **The default build** (`cargo build`, `cargo test`, the main CI job) has the fake engine only. It needs no model, no ONNX Runtime and no cmake or libclang, and it stays fast.
- **Release images build with `--features kokoro`.** A server without the feature can still run with an external `rtm tts` process that has it.
- **A separate CI job builds, lints and unit-tests with `--features kokoro`** on every PR that touches the workspace. It installs cmake and clang, and caches the ONNX Runtime download. This stops the feature-gated code from rotting unseen.
- **Real synthesis tests stay ignored by default.** They need the model files, so they run locally and nightly ([doc 05](../05-quality-cicd-observability.md)'s real-model synthesis test), not on every PR.

## Alternatives considered

| Option | Why not |
|---|---|
| Vendor the patched crate and lexicons in this repository | About 18 MB of JSON in this repository's history permanently. #18's patches would mix with application history and be harder to offer upstream. |
| Patch the published crate in place and fetch the lexicons at build time, pinned by SHA-256 | Keeps this repository small, but every build needs the network and a custom fetch step, and there's still nowhere natural for #18's rule patches. |
| Use the published `misaki-rs` unchanged | Its espeak-generated lexicons mispronounce common words ("Covid", "coronavirus") and write diphthongs in a notation Kokoro lacks; see [ADR-0007](0007-tts-engine-rust-kokoro.md) Amendment 1. |
| Always build Kokoro | Every contributor and every CI run would need cmake, libclang and the ONNX Runtime download, for code most changes don't touch. |
| Build ONNX Runtime ourselves | Full control of the binaries, but a long C++ build to maintain. `ort`'s hash-verified downloads meet [ADR-0007](0007-tts-engine-rust-kokoro.md)'s pinning requirement; revisit if supply-chain needs change, e.g. if pyke's CDN becomes unreliable. |

## Consequences

- **This repository stays small,** and the pronunciation work has a natural home that can feed upstream.
- **One more repository to maintain.** Rebasing the fork on upstream releases is a deliberate step, recorded by moving the pinned `rev`.
- **A build with `kokoro` depends on two external sources:** GitHub, for the fork, and pyke's CDN, for ONNX Runtime. Both are pinned: by commit, and by SHA-256 respectively.
- **The default build can't produce real speech.** Anyone who wants to hear output builds with `--features kokoro` and fetches the model files, which are pinned by SHA-256 and never committed ([ADR-0007](0007-tts-engine-rust-kokoro.md)).
- **CI gains one job.** It's slower than the default job, but it runs in parallel and only builds and tests the feature, without the model.
- ⚑ **Hosting impact:** none beyond [ADR-0007](0007-tts-engine-rust-kokoro.md). A GPU variant would be another feature or image built the same way.
