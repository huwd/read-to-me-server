# ADR-0002: Rust modular monolith with a separate TTS process

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 03 §2](../03-architecture-and-nfrs.md#2-architectural-style), [doc 04 §1–2](../04-tech-stack.md#1-language-choice-rust-for-the-core--with-eyes-open), [ADR-0003](0003-sqlite-datastore-and-queue.md) (SQLite), [ADR-0007](0007-tts-engine-rust-kokoro.md) (TTS), [ADR-0005](0005-egress-guard.md) (egress), [ADR-0006](0006-extractor-triage-contract.md) (extractors)

## Context

The system fetches untrusted content, parses it, runs ML inference, encodes audio and serves a feed. The target is one self-hosted instance for a person or household ([doc 07 Q2](../07-roadmap-and-open-questions.md#q2)), often on modest hardware. Needs:
- a small footprint and a simple `docker compose` setup;
- safety when parsing hostile input;
- isolation of native inference code;
- extension points (extractors, engines, languages) that don't need core changes.

## Decision

### Language
**Rust** for everything shipped, including TTS ([ADR-0007](0007-tts-engine-rust-kokoro.md)). Reasons: memory safety on untrusted input, a small idle footprint, single static binaries and the owner's preference. Python is allowed only in throwaway tooling (spike comparisons, offline corpus benchmarks) that is never shipped.

### Process model

| Process | Built from | Responsibilities | Talks to |
|---------|-----------|------------------|----------|
| **`rtm server`** | `rtm-bin` | HTTP API, auth, pipeline workers (tokio tasks), feed + enclosure serving, scheduled jobs (backups, prune, GC) | Owns the SQLite database ([ADR-0003](0003-sqlite-datastore-and-queue.md)); object store; TTS over HTTP; spawns extractor sandboxes and ffmpeg |
| **`rtm tts`** | same workspace | Kokoro inference via ONNX Runtime | Nothing but its HTTP listener (no DB, no internet) |
| **`rtm extract-worker`** | same binary, re-exec'd | Parses one source document into an `Article` inside a sandbox | stdin/stdout only (no network, no DB) |
| `ffmpeg` | upstream LGPL build | Encode PCM → MP3 | stdin/stdout only |

- The server is a **single process**. Only one process writes the database, which is SQLite's constraint and a simplification we want.
- Native or risky code (inference, parsing, encoding) runs **outside** the server process. A crash there fails one item, not the feed ([NFR-R3](../03-architecture-and-nfrs.md#nfr-r3)).

### Internal structure: hexagonal (ports & adapters)
- `rtm-domain` holds pure types and the item state machine, with no I/O.
- `rtm-ports` defines traits for every external dependency: `Extractor`, `SpeechEngine`, `ObjectStore`, `Fetcher`, `Repository`/`Queue`, `TokenVerifier`, `Clock`.
- Adapters live in their own crates (`rtm-fetch`, `rtm-extract`, `rtm-tts`, `rtm-store`, …). The domain never depends on an adapter.
- Every port has an in-memory or fake adapter, so CI needs no model weights, network or containers for most tests.
- Registries (extractors, speech engines, languages) are the extension points. Adding one is additive ([NFR-M2](../03-architecture-and-nfrs.md#nfr-m2)/[M6](../03-architecture-and-nfrs.md#nfr-m6)/[M7](../03-architecture-and-nfrs.md#nfr-m7)).

The crate layout is as in [doc 04 §2](../04-tech-stack.md#2-server-rust).

### Pipeline
The item lifecycle is a persisted state machine (`Submitted → Fetched → Extracted → Scripted → Synthesised → Encoded → Published`, or `Failed{stage, code}`):
- every stage is idempotent;
- outputs are content-addressed ([ADR-0008](0008-content-addressed-storage.md));
- retries use backoff;
- leases recover crashed work.

See [doc 03 §5](../03-architecture-and-nfrs.md#5-pipeline-flow--failure-handling).

## Alternatives considered

| Option | Why not |
|--------|---------|
| Microservices (API, worker, feed as separate services) | A distributed-systems tax with no benefit at household scale; conflicts with SQLite's single writer. |
| Everything in one process, TTS included | ONNX Runtime is native C++; a crash or OOM would take the feed down; no independent GPU image. |
| Go | Strong runner-up (fast to build, good HTTP). Rust chosen for parsing safety, type modelling and the owner's learning goal. |
| Python | Best ML/extraction ecosystem, but heavier images and packaging, and a weaker fit for a hardened internet-facing service. |

## Consequences

- One server container plus one TTS container; extraction and encoding add no containers.
- The process boundaries are also security boundaries ([doc 06 §3.2](../06-security-and-pentest.md#32-parsing--processing-untrusted-content)).
- The server can't be scaled to several replicas; acceptable for the target. ⚑ Hosting impact: horizontal scaling would need a different datastore ([ADR-0003](0003-sqlite-datastore-and-queue.md)) and splitting API from workers.
- Rust compile times and a steeper learning curve slow early iteration.

## Amendment 1 (2026-10-12): languages for project tooling and CI

- **Our own tooling is Rust.** That covers CI helpers, docs checks, code generation and corpus evaluation. It lives in the `xtask` crate in the Cargo workspace and runs as `cargo xtask <command>` (see `xtask/README.md`). CI already needs the Rust toolchain, so this adds no runtime.
- **Third-party tools are used as pinned binaries, containers or GitHub Actions, whatever language they're written in.** Examples: Semgrep, actionlint, hadolint, Spectral, release-please. GitHub's runners ship Node for JavaScript-based Actions, but we don't write or maintain Node, TypeScript or Python for CI.
- **Python** remains allowed only in throwaway spike tooling (the [ADR-0007](0007-tts-engine-rust-kokoro.md) TTS comparison, offline corpus benchmarks). It's never used in a CI gate.
- **TypeScript** would come in only if the client strategy ([ADR-0012](README.md)) chooses a TypeScript web UI; tooling for that UI would be decided then.
- First application: the docs link tooling, initially written in Python, was ported to `cargo xtask docs-links {fix,check}`.
