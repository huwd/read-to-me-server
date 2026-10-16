# Contributing

Thanks for your interest in read-to-me.

## Licence and sign-off (DCO)

The server is licensed under **AGPL-3.0-or-later** (see [ADR-0010](docs/adr/0010-licence-and-third-party-policy.md)). Contributions are accepted under the same licence, using the [Developer Certificate of Origin 1.1](https://developercertificate.org/). There's no CLA.

Every commit must carry a sign-off line, certifying that you wrote the change or otherwise have the right to submit it under the project's licence:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it automatically. CI rejects pull requests containing commits without a valid sign-off.

## Commits and branches

- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):
  - `type(scope): description`, where the type is one of `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci` or `chore`;
  - the subject is at most 50 characters, in the imperative, with no full stop;
  - the body is wrapped at 72 characters and explains why, how, side effects and alternatives considered;
  - one logical change per commit.
- **Branches** are named `<type>/<issue-number>-<short-description>`, e.g. `ci/5-pr-checks`.
- **Pull requests** use `Closes #N` when they fully resolve an issue and `Refs #N` otherwise.

`cargo xtask commits` checks the mechanical parts (format, lengths, sign-off) for the commits on your branch; CI runs it on every pull request.

## Before opening a pull request

- Read the relevant planning docs in [`docs/`](docs/) and any ADR your change touches.
- Run the same checks CI runs (`.github/workflows/ci.yml`):
  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --exclude rtm-kokoro --all-targets --locked -- -D warnings
  cargo test --workspace --exclude rtm-kokoro --locked
  cargo deny check licenses bans sources advisories
  cargo xtask docs-links check
  cargo xtask commits
  ```
- Reference requirement IDs (`FR-*`, `NFR-*`) in tests and in the PR description.
- New third-party dependencies must pass the licence allow-list in [`deny.toml`](deny.toml).

## The Kokoro speech engine

The real speech engine is behind the `kokoro` cargo feature ([ADR-0014](docs/adr/0014-kokoro-dependencies-and-build.md)), so ordinary builds and the commands above don't need it. Building it needs **cmake** and **clang/libclang** (espeak-ng is compiled from source), and it downloads ONNX Runtime, verified against a pinned SHA-256.

```sh
cargo clippy -p rtm-kokoro -p rtm-bin --features rtm-bin/kokoro --all-targets --locked -- -D warnings
cargo test -p rtm-kokoro -p rtm-bin --features rtm-bin/kokoro --locked
```

To hear it, fetch the pinned model files (about 330 MB; never commit them), then run the TTS process:

```sh
cargo xtask kokoro-models models/kokoro
cargo build --release -p rtm-bin --features kokoro
RTM_TTS_ENGINE=kokoro RTM_KOKORO_DIR=models/kokoro ./target/release/rtm tts
RTM_KOKORO_DIR=models/kokoro cargo test -p rtm-kokoro --release --test real_model -- --ignored
```

## Security issues

Please don't open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md) for private disclosure.
