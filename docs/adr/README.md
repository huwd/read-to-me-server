# Architecture Decision Records

Format: [MADR](https://adr.github.io/madr/). One decision per file, numbered, never deleted. A record is superseded by a new one, never rewritten after acceptance.

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | **Accepted** |
| [0002](0002-rust-modular-monolith.md) | Rust modular monolith (single process) + separate Rust TTS process | **Accepted** |
| [0003](0003-sqlite-datastore-and-queue.md) | SQLite as the only datastore and job queue | **Accepted** |
| [0004](0004-auth-oidc-resource-server.md) | OAuth 2.1/OIDC resource server (generic provider config); PATs and feed capability tokens | **Accepted** |
| [0005](0005-egress-guard.md) | Egress guard (resolve → validate → pin) | **Accepted** |
| [0006](0006-extractor-triage-contract.md) | Extractor triage/adapter contract, sandboxing and versioning | **Accepted** |
| [0007](0007-tts-engine-rust-kokoro.md) | Default TTS engine: Kokoro in Rust (ONNX Runtime), separate process | **Accepted** (spike validates) |
| [0008](0008-content-addressed-storage.md) | Content-addressed storage and cache-key derivation | **Accepted** |
| [0009](0009-audio-format-and-feed.md) | Audio format, enclosure serving and feed namespaces | **Accepted** |
| [0010](0010-licence-and-third-party-policy.md) | Project licence (AGPL-3.0) and third-party licence policy | **Accepted** |
| [0011](0011-observability-stack.md) | Observability: OpenTelemetry, optional bundled LGTM, local-only analytics | **Accepted** |
| 0012 | Client strategy (web UI / native wrappers / native apps) | Deferred |
| [0013](0013-secret-scanner.md) | Secret scanner: Gitleaks vs Betterleaks | Proposed |
| [0014](0014-kokoro-dependencies-and-build.md) | Kokoro's pronunciation fork and native build | Accepted |
| [0015](0015-container-images-and-compose.md) | Container images and the compose stack | Accepted |
