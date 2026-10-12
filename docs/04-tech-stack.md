# 04 — Tech Stack & Rationale

Guiding principles: (1) boring where it doesn't matter, (2) memory-safe where we parse hostile input, (3) every external dependency behind a port so choices are reversible, (4) one container footprint small enough for a Raspberry Pi.

## 1. Language choice: Rust for the core — with eyes open

| Option | For | Against | Verdict |
|--------|-----|---------|---------|
| **Rust** | Memory safety for parsing untrusted HTML/PDF; tiny idle footprint (great for self-host); single static binary; excellent async (tokio); strong typing for the state machine; the author wants to learn it | Slower to iterate; extraction/NLP ecosystem thinner than Python; compile times | **Chosen for the core server** |
| Go | Fast to build, simple concurrency, good HTTP/SSRF-safe dialer patterns, small binaries | GC fine here; weaker type modelling; no learning goal | Strong runner-up — choose if delivery speed beats learning |
| Python (FastAPI) | Best extraction (Trafilatura) and TTS (Kokoro reference impl) ecosystems; fastest MVP | Heavier runtime, packaging pain, larger attack surface for an internet-facing service | Not shipped. Allowed only for throwaway comparison/benchmark tools (e.g. the Phase 0 TTS comparison, [ADR-0007](adr/0007-tts-engine-rust-kokoro.md)) |
| TypeScript (Node/Bun) | Shared language with any web UI; Mozilla Readability native | Parsing safety, runtime footprint | Not chosen |

**Honest note:** performance won't be decided by the API language — synthesis dominates (assumption [A1](03-architecture-and-nfrs.md#a1)). Rust earns its place through safety, footprint and correctness, not raw speed. The whole shipped system is Rust, TTS included ([ADR-0007](adr/0007-tts-engine-rust-kokoro.md)); the main risk there is pronunciation quality, which the Phase 0 spike measures against the Python reference implementation.

## 2. Server (Rust)

| Concern | Choice | Rationale / alternatives |
|---------|--------|-------------------------|
| Async runtime | `tokio` | De facto standard. |
| HTTP framework | `axum` + `tower`/`tower-http` | Ergonomic, tower middleware for timeouts, rate limiting, compression, tracing, CORS, request IDs. (alt: actix-web) |
| OpenAPI | Spec-first `openapi.yaml` (3.1) + `utoipa` to verify handlers match, or generate server stubs | Spec is the contract for any client (web UI, shortcuts, future apps); CI diff-checks it. |
| DB access | `sqlx` (SQLite, compile-time checked queries, embedded migrations) | No ORM magic; offline query metadata committed for CI. (alt: SeaORM, diesel) |
| Database | **SQLite ≥ 3.45** (bundled), WAL mode, single write connection | One file, no extra container, trivial backups; ideal for one household instance. SQLite only — no second backend. See [ADR-0003](adr/0003-sqlite-datastore-and-queue.md). ⚑ Hosting impact: would need a Postgres adapter. |
| Job queue | Small in-house SQLite queue module (leases, retries with backoff, per-user fairness, priorities) | Simple to own given a single writer; `apalis` (sqlite backend) evaluated but not assumed. |
| HTTP client | `reqwest` (rustls) with a **custom DNS resolver + connector** implementing the egress guard | Pin vetted IPs, manual redirects. Wrap in our `SafeFetcher` port; fuzz it. |
| IP classification | `ipnet` + explicit deny tables (IANA special-purpose registries) | Don't rely on `is_global()` alone (historically unstable). |
| HTML parsing | `html5ever` / `scraper` (spec-compliant parser) | Same parsing algorithm as browsers → fewer parser-differential surprises. |
| Generic extraction | Evaluate `dom_smoothie` / `readability`-family crates against the golden corpus; use Trafilatura (Python) only as an offline benchmark in the corpus tooling, never shipped | Pick by F1 on *our* corpus, not by stars. |
| PDF | `pdfium-render` (PDFium, BSD-style) in the sandbox; alt `pdf-extract`/`lopdf` for simple text PDFs | PDFium is robust; poppler is GPL — avoid for licensing. Layout heuristics (columns, headers/footers, hyphenation) are an adapter concern. |
| Sanitisation | `ammonia` (only if we ever emit HTML, e.g. feed descriptions — default is plain text) | |
| Text normalisation | Own module + `unicode-normalization`, `whatlang`/`lingua` for language detection | Normalisation rules per language behind a trait. |
| Hashing | `blake3` | Fast, CAS keys. |
| Object storage | `object_store` crate (local FS, S3/R2/MinIO, GCS, Azure) | Local filesystem by default; S3-compatible (MinIO/Garage, or a cloud bucket) for people who prefer it. |
| Audio encoding | `ffmpeg` (LGPL build) as a subprocess with **fixed argv** (no shell), loudnorm filter, libmp3lame; ID3 chapters via `id3` crate | Mature and correct; sandboxed. Alt: pure-Rust `mp3lame-encoder` binding. |
| Feed generation | `quick-xml` writer with our own typed model (or `rss` crate + namespace extensions) | Namespaces (`itunes`, `podcast`) need control; all text XML-escaped by the writer. |
| Auth (resource server) | `jsonwebtoken` + JWKS cache (`openidconnect` crate for discovery) | Validate `iss`, `aud`, `exp`, `nbf`, `alg` allow-list (RS256/ES256/EdDSA only). |
| PATs / feed tokens | 256-bit random (`rand` OsRng), stored as BLAKE3/HMAC hash, prefixed (`rtm_pat_…`, `rtm_feed_…`) for secret scanning | Prefixes let GitHub secret scanning and our CLI scanner ([ADR-0013](adr/0013-secret-scanner.md)) spot leaks. |
| Config | `figment` or `config` + env | 12-factor. |
| Errors | `thiserror` (libs), `anyhow` only at the binary edge; RFC 9457 problem+json responses | |
| Telemetry | `tracing`, `tracing-subscriber`, `tracing-opentelemetry`, `opentelemetry-otlp` | Traces, metrics, logs via OTLP. |
| Sandbox | Subprocess re-exec of the same binary in a `extract-worker` mode + `seccompiler`/landlock + rlimits; or run in a separate locked-down container | No network namespace, read-only FS, CPU/mem/time caps. |

### Crate layout (Cargo workspace)
```
crates/
  rtm-domain       # pure types: Item, Article, Script, state machine; no I/O
  rtm-ports        # traits: Extractor, SpeechEngine, ObjectStore, Fetcher, Clock, IdP
  rtm-fetch        # SafeFetcher + egress guard
  rtm-extract      # triage registry + built-in extractors
  rtm-narrate      # Article → Script, normaliser
  rtm-synth        # SpeechEngine clients (tts-http, fake)
  rtm-tts          # TTS process: Kokoro via `ort`, G2P (espeak-ng + lexicon), voice loading, chunking
  rtm-encode       # ffmpeg wrapper, ID3/chapters
  rtm-feed         # RSS model + writer
  rtm-store        # sqlx SQLite repositories, queue, outbox, backups
  rtm-api          # axum routes, auth middleware
  rtm-bin          # main: roles server (api+workers+feed)|tts|extract-worker|migrate|backup
xtask/             # dev tasks: golden corpus eval, fixtures
```

## 3. TTS process (Rust) — see [ADR-0007](adr/0007-tts-engine-rust-kokoro.md)

| Concern | Choice | Rationale |
|---------|--------|-----------|
| Default model | **Kokoro-82M** (Apache-2.0 weights) — "koriko" in the brief | Excellent quality-for-size; runs well on CPU; multiple English voices. |
| Runtime | Rust, Kokoro ONNX export via `ort` (ONNX Runtime, pinned and checksum-verified); separate process/container from the same workspace | Small images, one toolchain, crash isolation from the feed server. Python reference used only in a throwaway comparison tool during the spike. |
| G2P / phonemisation | espeak-ng (GPL-3.0-or-later, AGPL-compatible) + our own lexicon overrides; port `misaki`'s English lexicon G2P to Rust if the spike shows a pronunciation gap | The main quality risk of the Rust path; measured in Phase 0 ([ADR-0007](adr/0007-tts-engine-rust-kokoro.md)). |
| Contract | `POST /synthesize {text, voice, speed, format: pcm16@24k}` → audio; `GET /voices`; `/healthz`; batched chunks | Engine-agnostic; our Rust `SpeechEngine` port targets this. |
| Alternatives behind the same port (**not built initially** — Kokoro is the only shipped engine, [doc 07 Q6](07-roadmap-and-open-questions.md#q6)) | Later candidates: Piper (very light, Pi-friendly), Chatterbox / F5-type models (higher quality, heavier, check licences), cloud APIs (ElevenLabs, OpenAI, Azure, Google, Polly) with the user's own key | Non-commercial-licensed models (e.g. original Coqui XTTS licence) are never bundled; self-hosters may install them for personal use (⚑ Hosting impact: unusable on a paid service). |
| Model supply chain | Pinned model files by SHA-256; **safetensors/ONNX/`.npy` only, never pickle** — upstream `.pt` voice packs converted once in a sandboxed step and published as release artefacts; mirrored to our own storage | Prevents model-file code execution and silent upstream changes. |
| Model licensing | Each model declares `weights_licence` and `commercial_use` in the engine registry; only permissively-licensed weights are bundled; non-commercial/research-only models are optional user installs, with their licence shown in the UI (⚑ Hosting impact: a hosted build would refuse them) | See [ADR-0010 §5](adr/0010-licence-and-third-party-policy.md). |
| Images | `tts-cpu` (default) and `tts-cuda` variants | Self-hosters with a GPU get the speed-up. |

## 4. Identity (OAuth 2.1 / OIDC from day one)

| Context | Choice | Why |
|---------|--------|-----|
| Protocol | OAuth 2.1 (Auth Code + PKCE only), OIDC for identity; RFC 8252 for native apps; RFC 9068 JWT access tokens; DPoP (RFC 9449) as a later hardening | Modern baseline; no implicit or password grants. |
| Provider | **Not specified.** Generic OIDC configuration: issuer URL, audience, allowed client IDs; everything else from discovery + JWKS | The self-hoster brings their own provider. CI tests against a standards-compliant mock IdP; no provider-specific code paths. |
| Swappability | We depend only on OIDC discovery + JWKS, so any standards-compliant IdP works | ⚑ Hosting impact: a public service would use a managed IdP (open sign-up, social login — Apple sign-in becomes mandatory on iOS if any social login is offered). No code change expected, only config. |
| Automation | Personal access tokens (scoped, expiring) for Shortcuts/CLI/bookmarklet | Share-sheet automation can't do interactive OAuth. |
| Feed access | Capability URL tokens (separate from OAuth) | Podcast apps don't support OAuth. |

## 5. Native apps (post-MVP)

> **Deferred.** The client strategy is not decided ([doc 07 Q3](07-roadmap-and-open-questions.md#q3), [ADR-0012](adr/README.md)). Options still open: fully native Swift/Kotlin apps (table below, kept as a reference), thin native wrappers loading a server-hosted web UI (keeping native share-sheet and secure token storage), or shortcuts + web UI only. Nothing in this section is a commitment.

| Concern | iOS (Swift) | Android (Kotlin) |
|---------|-------------|------------------|
| UI | SwiftUI | Jetpack Compose (Material 3) |
| Architecture | MVVM + Swift Concurrency; Observation framework | MVVM/UDF, Coroutines + Flow, Hilt |
| API client | Apple `swift-openapi-generator` from `openapi.yaml` | `openapi-generator` (kotlin + ktor/okhttp) or Retrofit |
| Auth | `ASWebAuthenticationSession` (+ AppAuth-iOS optional), tokens in Keychain with `kSecAttrAccessibleAfterFirstUnlock`, shared to the extension via App Group keychain access group | AppAuth-Android using Custom Tabs; tokens encrypted with Android Keystore-backed key (Tink) in DataStore |
| Share-in | Share Extension accepting URLs, text with URLs, PDFs, HTML (`NSExtensionActivationRule`) | `ACTION_SEND` / `ACTION_SEND_MULTIPLE` intent filters for `text/plain`, `application/pdf`, `text/html` |
| Offline queue | Background `URLSession` upload; local SwiftData queue | WorkManager with retry/backoff |
| Status | SSE while foregrounded; APNs push on publish/fail | SSE; FCM push |
| Feed onboarding | "Add to Podcasts" deep links (`podcast://`, `overcast://x-callback-url/add?url=`, `pktc://subscribe/`, AntennaPod intent) + copy feed URL | |
| Telemetry | Sentry + OpenTelemetry Swift (traces propagate `traceparent` to server) | Sentry + OTel Android |
| Lint/test | SwiftLint, SwiftFormat, XCTest/Swift Testing, XCUITest, snapshot tests | ktlint, detekt, Android Lint, JUnit5, Turbine, Compose UI tests, Paparazzi |
| Distribution | Fastlane → TestFlight → App Store | Fastlane / Gradle Play Publisher → internal track → Play |

**Cheaper alternatives to weigh first:** (a) a small responsive web UI served by the server (status + prune) plus iOS Shortcut / Android HTTP-shortcut share actions using a PAT; (b) Kotlin Multiplatform for shared networking/auth/domain with native UIs. Recommendation: (a) for MVP, then two native apps with KMP *only* if logic duplication actually hurts (the apps are thin, so it may not).

## 6. Infrastructure & delivery

| Concern | Choice | Rationale |
|---------|--------|-----------|
| Source & CI | GitHub + GitHub Actions | Already here; attestations; no long-lived secrets in CI. |
| Container base | `gcr.io/distroless/cc` or Chainguard/Wolfi static; non-root; read-only rootfs | Minimal attack surface, small image. |
| Builds | `cargo-chef` layer caching; `cross`/`cargo-zigbuild` or native arm64 runners for multi-arch | |
| Registry | GHCR | Free for public images; signed with cosign. |
| Deploy (owner's instance) | Pull the signed release image on the homelab/VPS (e.g. a small deploy script or Watchtower-style updater with signature verification); config in a private repo | The "prod" environment is the owner's own instance. ⚑ Hosting impact: a public service would add IaC (OpenTofu) and a managed platform — see [doc 03 §8.2](03-architecture-and-nfrs.md#82--if-hosting-were-ever-revisited-reference-only--not-a-target). |
| Reverse proxy (self-host) | Caddy (automatic TLS) sample config | |
| Feature flags | OpenFeature SDK with a simple file/env provider → Unleash/Flagd later | |
| Local dev | `docker compose` dev profile, `just` task runner, `mise` for toolchain pinning, fake TTS engine for speed | |
| Docs | mdBook for user/self-host docs; ADRs in [`docs/adr/`](adr/README.md) (MADR template) | |
| Licensing | Server `AGPL-3.0-or-later`, apps/SDKs `Apache-2.0` if built; inbound allow-list in [`deny.toml`](../deny.toml); SPDX headers checked by `reuse lint` | [ADR-0010](adr/0010-licence-and-third-party-policy.md) |
