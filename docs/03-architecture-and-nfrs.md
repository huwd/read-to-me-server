# 03 — Architecture, Technical Assumptions & NFRs

## 1. Technical assumptions

| # | Assumption | Consequence if wrong | How we validate |
|---|-----------|----------------------|-----------------|
| <a id="a1"></a>A1 | TTS inference dominates end-to-end time and cost (≥ 90%); the web/API tier is not the bottleneck. | Language choice for the API matters less than we think. | Benchmark spike: time each stage for 20 articles. |
| <a id="a2"></a>A2 | Kokoro-class models run at ≥ 3× real time on a modern 4-core CPU, ≥ 50× on a consumer/datacentre GPU. | Self-host on CPU becomes too slow for long articles. | Benchmark on reference machines ([doc 05 §6](05-quality-cicd-observability.md#6-benchmark-plan-to-retire-assumptions-a1a3-in-week-1)). |
| <a id="a3"></a>A3 | Most target articles are server-rendered HTML fetchable without JS or cookies. | Need a headless-browser fetcher (heavier, riskier). | Measure on golden corpus; track `EXTRACTION_LOW_CONFIDENCE` rate per domain. |
| <a id="a4"></a>A4 | Podcast apps reliably handle private feeds with token-in-URL, Range requests, and MP3. | Need basic-auth feeds or a different delivery path. | Manual matrix test: Apple Podcasts, Overcast, Pocket Casts, AntennaPod, Castro. |
| <a id="a5"></a>A5 | Single-user self-host load is tiny (≤ 50 items/day). | — | — |
| <a id="a6"></a>A6 | An instance serves ≤ 10 users (a household). | ⚑ Hosting impact: larger multi-tenant load would need autoscaling workers and possibly a broker. | Not tested; hosting is not a target. |
| <a id="a7"></a>A7 | Users accept minutes, not seconds, of latency (it's asynchronous: "it'll be in your feed shortly"). | Need streaming synthesis + progressive episodes. | Dogfooding; time-to-feed metric. |

## 2. Architectural style

**A modular monolith (one process, one SQLite database) with an isolated, swappable inference process, organised as hexagonal (ports & adapters), executing a durable staged pipeline.**

```
                 ┌──────────────────────────── rtm (Rust, single binary, roles via flag) ───────────────────────────┐
  Apps / CLI ──► │  API (axum)  ──► SQLite (state, jobs, outbox)    ◄── Workers (tokio tasks, same process)       │
  Shortcuts      │    │ OIDC/JWT validation                               │                                       │
  Podcast apps ─►│  Feed + enclosure server (Range, ETag)                 ├─► Egress guard ─► Internet            │
                 │                                                        ├─► Sandbox: extractors (subprocess)    │
                 └────────────────────────────────────────────────────────┼───────────────────────────────────────┘
                                                                          ├─► SpeechEngine port ──► TTS process (Rust + ONNX Runtime, Kokoro; CPU/GPU)
                                                                          ├─► Encoder (ffmpeg, fixed args, sandboxed)
                                                                          └─► Object store port ──► local FS | S3/R2/MinIO
```

### Why these patterns

| Pattern | Applied to | Rationale |
|---------|-----------|-----------|
| **Modular monolith** (one deployable, strict internal module boundaries) | Server | One container for self-hosters; avoids distributed-systems tax. Modules (`auth`, `submission`, `fetch`, `extract`, `narrate`, `synth`, `encode`, `feed`, `storage`) communicate through traits, so any could be split out later. |
| **Hexagonal / ports & adapters** | Every external dependency: IdP, DB, object store, TTS, fetcher, clock | Testability (in-memory fakes in CI, no model weights needed), and the extensibility the brief demands. |
| **Triage + Chain-of-responsibility + Registry** | Extractors | Each extractor scores a document; registry picks the best. New adapters are additive; no `if domain == …` sprawl. |
| **Strategy** | `SpeechEngine`, `Encoder`, `ObjectStore`, `Fetcher` | Swap Kokoro↔Piper↔cloud API; local FS↔S3. |
| **Durable staged pipeline / state machine** | Item lifecycle | Each stage is idempotent, persisted, retryable with backoff; crash-safe; status is just the state. Stages: `Submitted → Fetched → Extracted → Scripted → Synthesised → Encoded → Published` (+ `Failed{stage,code}`). |
| **SQLite as queue** (single writer connection, atomic claim via `UPDATE … RETURNING`) + **transactional outbox** | Jobs, events (SSE/webhooks) | No Redis/Kafka/Postgres; exactly-once *effects* via idempotent stages + outbox in the same transaction as state changes; in-process broadcast to SSE after commit. See [ADR-0003](adr/0003-sqlite-datastore-and-queue.md). |
| **Content-addressed storage (CAS)** | Source docs, articles, audio | Dedupe and caching fall out naturally ([FR-CACHE](01-product-spec.md#510-content-addressed-storage--re-use-cache)); immutable blobs make HTTP caching (ETag, long max-age) trivial; refcount for deletes. |
| **Privilege separation / sandboxing** | Parsing untrusted HTML/PDF, ffmpeg | Parsers are the most likely RCE/DoS surface. Run them in a short-lived subprocess with seccomp, no network, rlimits, read-only FS. |
| **Separate inference process** | TTS | Native ONNX Runtime code isolated from the feed server (crash/memory isolation); optional GPU image; independently replaceable. Same Rust workspace; HTTP contract. See [ADR-0007](adr/0007-tts-engine-rust-kokoro.md). |
| **Multi-user data model** | Schema | Every row carries `user_id`; row-level authorisation in the repository layer. Needed anyway for household members and sharing. ⚑ Hosting impact: a hosted service would add tenant/org scoping and Postgres with RLS ([ADR-0003](adr/0003-sqlite-datastore-and-queue.md)); the `user_id` discipline keeps that possible without a rewrite. |
| **API-first (OpenAPI 3.1)** | Server ↔ any client | One contract for the web UI, shortcuts and any future app (client strategy deferred, [doc 07 Q3](07-roadmap-and-open-questions.md#q3)); contract tests in CI. |
| **12-factor configuration** | Deployment | One image, behaviour via env/config; works the same on a Pi, a NUC or a VPS. |

## 3. Key domain model

```text
User(id, oidc_sub, created_at, role, quotas)
Feed(id, user_id, title, token_hash, token_rotated_at, prune_policy, artwork_key)
Item(id, user_id, feed_id, source_kind{url|html|pdf}, source_url, canonical_url,
     status, failure_code, failure_message, progress,
     source_blob, article_blob, audio_blob,          -- CAS keys
     extractor_id, extractor_version, engine_id, engine_version, voice,
     title, byline, published_at, word_count, duration_ms, bytes,
     created_at, published_feed_at, deleted_at)
Job(id, item_id, stage, attempts, run_after, locked_by, locked_until, last_error)
Blob(key, kind, size, content_type, refcount, created_at)
AccessToken(id, user_id, name, hash, scopes, expires_at, last_used_at)   -- PATs
Share(id, item_id, from_user, to_user, status, expires_at)               -- stretch
OutboxEvent(id, user_id, type, payload, created_at, delivered_at)
```

## 4. Extraction subsystem (the triage/adapter contract)

```rust
/// Raw input after fetch or upload. Never trusted.
pub struct SourceDocument {
    pub bytes: Bytes,
    pub content_type: MediaType,     // sniffed + declared, reconciled
    pub final_url: Option<Url>,      // after redirects
    pub headers: HeaderSnapshot,     // filtered allow-list
}

pub struct Confidence(f32);          // 0.0..=1.0
pub enum Specificity { Generic, Format, Site }

#[async_trait]
pub trait Extractor: Send + Sync {
    fn id(&self) -> &'static str;            // "generic-html", "pdf", "site/substack"
    fn version(&self) -> u32;                // bump on behaviour change → new cache key
    fn specificity(&self) -> Specificity;
    /// Cheap. Must not do I/O. Looks at URL, content-type, a bounded prefix of bytes, meta tags.
    fn score(&self, doc: &TriageView<'_>) -> Option<Confidence>;
    /// Runs inside the sandbox.
    async fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError>;
}

pub struct Article {
    pub title: String,
    pub byline: Option<String>,
    pub site_name: Option<String>,
    pub published: Option<OffsetDateTime>,
    pub canonical_url: Option<Url>,
    pub lang: Option<LanguageTag>,
    pub blocks: Vec<Block>,
}
pub enum Block {
    Heading { level: u8, text: Inline },
    Paragraph(Inline),
    List { ordered: bool, items: Vec<Inline> },
    Quote(Vec<Block>),
    Code { lang: Option<String>, text: String },
    Figure { alt: Option<String>, caption: Option<Inline> },
    Table { caption: Option<Inline>, rows: Vec<Vec<Inline>> },
    Separator,
}
```

Triage order of evidence: explicit user hint → site adapters (domain + DOM fingerprints, e.g. `meta[name=generator]=Ghost`) → structured data (JSON-LD `Article.articleBody`, OpenGraph) → format (PDF, plain text) → generic readability heuristic. Every extraction result is scored post-hoc (word count, link density, boilerplate phrase classifier) and a low score can trigger fallback to the next candidate.

Downstream of extraction, a **Narrator** turns `Article` into a `Script` (`Vec<Utterance { text, pause_after_ms, voice }>`) and a **Normaliser** handles numbers/dates/abbreviations per language. Keeping `Article` structured (not flat text) is what makes chapters, transcripts and per-block reading rules possible.

## 5. Pipeline flow & failure handling

1. API validates input, writes `Item(status=queued)` + `Job(stage=fetch)` + outbox event in one transaction → `202`.
2. Worker claims a job (atomic `UPDATE … RETURNING` on the single write connection; lease with heartbeat). Executes stage. On success writes next job + state + outbox event in one transaction.
3. Retryable errors (network, 5xx, engine busy) → exponential backoff with jitter, max attempts per stage. Terminal errors (blocked destination, low confidence, too long) → `Failed` immediately with a user-facing code.
4. Lease expiry returns a crashed job to the queue. Stages are idempotent because outputs are content-addressed (re-running writes the same key).
5. Synthesis is the long stage: chunked, progress reported, chunks cached by hash so a crash mid-article resumes rather than restarting.
6. `Published` → feed `ETag` changes → outbox row committed → in-process broadcast → SSE.

## 6. Re-use & caching design (per instance)

Content addressing is primarily a reliability feature on a single instance: crash-safe resumption, cheap retries, and no re-synthesis when an article is resubmitted or shared within the household.

| Layer | Key | Hit means |
|-------|-----|-----------|
| Fetch | none across users (per-user only, short TTL) | — never share fetched bytes across users |
| Extract | `H(source_bytes) ‖ extractor@ver` | Skip extraction |
| Synthesis (chunk) | `H(normalised_chunk_text) ‖ engine@ver ‖ voice ‖ params` | Skip synthesis for that chunk — resumes after a crash, cheap retries and re-voicing |
| Episode audio | `H(script) ‖ engine@ver ‖ voice ‖ encoder@ver` | Re-use full MP3 |

**Privacy constraints between users on one instance:** keys are derived only from content the requester already has, so a hit never discloses new content. A **timing side channel** still exists ("this was fast, so someone else read this article") — acceptable between household members for public articles, but uploaded documents (PDFs) are never re-used across users (per-user salt in the key).

⚑ Hosting impact: on a central service the same keys would enable a cross-tenant cache, with sharper copyright (redistribution) and privacy (side-channel) consequences — see [doc 02 §3](02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits). Not built. Hash with BLAKE3; keys never exposed in public URLs without an additional per-user signature.

## 7. Non-functional requirements

Targets are for the **reference self-host machine** (4 vCPU, 8 GB RAM, no GPU, SSD) unless marked otherwise. Hosting-only targets were removed; see [§8.2](#82--if-hosting-were-ever-revisited-reference-only--not-a-target) for what would change. Each NFR has an SLI so it can be measured ([doc 05](05-quality-cicd-observability.md)).

### Performance
| ID | Requirement | Target |
|----|-------------|--------|
| <a id="nfr-p1"></a>NFR-P1 | Submission ack latency | p95 ≤ 300 ms, p99 ≤ 1 s |
| <a id="nfr-p2"></a>NFR-P2 | Time-to-feed, 5,000-word article | p50 ≤ 5 min, p95 ≤ 10 min (CPU); with an optional local GPU p95 ≤ 3 min |
| <a id="nfr-p2b"></a>NFR-P2b | Time-to-feed, 30,000-word article (the limit, ≈ 3 h 15 min audio), with an idle queue | p95 ≤ 65 min (CPU, assuming ≥ 3× real time); with an optional local GPU p95 ≤ 6 min |
| <a id="nfr-p3"></a>NFR-P3 | Synthesis real-time factor | ≥ 3× (CPU), ≥ 50× (GPU) |
| <a id="nfr-p4"></a>NFR-P4 | Feed fetch latency | p95 ≤ 200 ms (304s ≤ 50 ms) |
| <a id="nfr-p5"></a>NFR-P5 | Enclosure time-to-first-byte with Range | p95 ≤ 300 ms |
| <a id="nfr-p6"></a>NFR-P6 | API server idle footprint | ≤ 100 MB RSS, ≤ 1% CPU idle (TTS service excluded) |
| <a id="nfr-p7"></a>NFR-P7 | TTS process memory | ≤ 2 GB RSS with one model loaded |

### Reliability & availability
| ID | Requirement | Target |
|----|-------------|--------|
| <a id="nfr-r1"></a>NFR-R1 | Job success rate (excluding user-attributable failures: blocked, low confidence, too long, 4xx source) | ≥ 99% |
| <a id="nfr-r2"></a>NFR-R2 | No job lost on crash/restart | 100% (verified by chaos test that kills workers mid-stage) |
| <a id="nfr-r3"></a>NFR-R3 | Feed and enclosure serving stays up while workers or the TTS service are busy, crashed or restarting | Verified by chaos test; podcast apps poll and show "feed broken" on errors |
| <a id="nfr-r4"></a>NFR-R4 | Containers restart automatically and the stack recovers without manual steps after a host reboot | Verified in E2E |
| <a id="nfr-r5"></a>NFR-R5 | Backup & restore | Built-in daily online backup (`VACUUM INTO`, RPO ≤ 24 h, configurable; optional Litestream for continuous); automatic backup before migrations; restore tested in CI. Audio is re-generable, so object-store backup is optional. |

### Scalability
| ID | Requirement | Target |
|----|-------------|--------|
| <a id="nfr-s1"></a>NFR-S1 | Configurable worker concurrency within the single server process; TTS process can run with multiple inference threads or replicas | Correct with ≥ 4 concurrent workers; DB passes `PRAGMA integrity_check` after chaos tests |
| <a id="nfr-s2"></a>NFR-S2 | Per-user fairness (household) | The queue picks items round-robin across users, but an item runs to completion once synthesis starts (no chunk interleaving yet). **Known limitation:** a short item can wait behind a long one, up to about an hour on the reference CPU for a 30,000-word item. Revisit with chunk-level scheduling ([doc 07 "Deferred"](07-roadmap-and-open-questions.md#4-deferred-revisit-later)). |

### Security & privacy (detail in doc 06)
| ID | Requirement |
|----|-------------|
| <a id="nfr-sec1"></a>NFR-SEC1 | OWASP ASVS 5.0 Level 2 for the server; MASVS L1 for any native client, if built. |
| <a id="nfr-sec2"></a>NFR-SEC2 | All untrusted parsing sandboxed (no network, rlimits, seccomp, timeout). |
| <a id="nfr-sec3"></a>NFR-SEC3 | No article content or full URLs in logs/traces by default — domain only; content opt-in for debug with retention ≤ 7 days. |
| <a id="nfr-sec4"></a>NFR-SEC4 | TLS 1.2+ everywhere external; HSTS; secure headers. |
| <a id="nfr-sec5"></a>NFR-SEC5 | Secrets never in images or repo; loaded from env/secret store; rotated. |
| <a id="nfr-sec6"></a>NFR-SEC6 | Data deletion: deleting an item removes its audio within 24 h (immediately from feed); the admin can delete a user and all their data immediately. ⚑ Hosting impact: a public service would add GDPR obligations (privacy notice, DPA, retention schedule). |

### Maintainability & operability
| ID | Requirement |
|----|-------------|
| <a id="nfr-m1"></a>NFR-M1 | Line coverage ≥ 80% on core crates; 100% of FRs mapped to at least one automated test. |
| <a id="nfr-m2"></a>NFR-M2 | Adding a site extractor requires changes in ≤ 2 files + a golden test fixture. |
| <a id="nfr-m6"></a>NFR-M6 | Adding a language requires only a new language module (normaliser rules, lexicon, voice mapping) + fixtures; no pipeline or core changes. Verified by a test that registers a toy "pseudo-language" module in CI. |
| <a id="nfr-m7"></a>NFR-M7 | Adding a speech engine requires only a new `SpeechEngine` adapter + registry entry + contract tests; no pipeline changes. Verified by the fake engine used in CI. |
| <a id="nfr-m3"></a>NFR-M3 | Upgrades cause ≤ 1 min downtime; each migration stays compatible with the previous release so a rollback works. |
| <a id="nfr-m4"></a>NFR-M4 | Self-host upgrade = pull + restart; automatic migrations; documented rollback. |
| <a id="nfr-m5"></a>NFR-M5 | Every service exposes `/healthz` (liveness), `/readyz` (readiness, checks DB/object store/TTS), OTLP telemetry. |

### Portability & compatibility
| ID | Requirement |
|----|-------------|
| <a id="nfr-c1"></a>NFR-C1 | Images for `linux/amd64` and `linux/arm64` (Raspberry Pi 5 / Apple silicon / Graviton). |
| <a id="nfr-c2"></a>NFR-C2 | Feeds work in Apple Podcasts, Overcast, Pocket Casts, AntennaPod, Castro, Podcast Addict. |
| <a id="nfr-c3"></a>NFR-C3 | If native clients are built: iOS ≥ 17, Android ≥ 10 (API 29). |
| <a id="nfr-c4"></a>NFR-C4 | English only at launch. Language-specific behaviour (normaliser, G2P/lexicon, voice) is isolated behind per-language registries from day one ([FR-TTS-07](01-product-spec.md#fr-tts-07)). |

### Resource protection
| ID | Requirement |
|----|-------------|
| <a id="nfr-dollar1"></a>NFR-$1 | Runs within the reference machine's limits alongside other homelab services ([NFR-P6](#nfr-p6)/[P7](#nfr-p7)). |
| <a id="nfr-dollar2"></a>NFR-$2 | Per-user caps on characters/day, concurrent jobs and storage so one user (or a runaway script) can't exhaust the host. |

### Accessibility
| ID | Requirement |
|----|-------------|
| <a id="nfr-a1"></a>NFR-A1 | Any user-facing UI (web UI; apps if built) meets WCAG 2.2 AA: VoiceOver/TalkBack labels, Dynamic Type/font scaling, contrast. The core audience includes assistive-tech users. |
| <a id="nfr-a2"></a>NFR-A2 | Every episode has a transcript (`podcast:transcript`). |

## 8. Deployment topologies

### 8.1 Self-hosted (docker compose)
```
services:
  rtm         # Rust binary: API + workers + feed server in one process; owns the SQLite database (local volume)
  tts         # same workspace, `--role tts`: Kokoro via ONNX Runtime (CPU image; optional CUDA image)
  # identity: bring your own OIDC provider — configured via RTM_OIDC_ISSUER / RTM_OIDC_AUDIENCE / RTM_OIDC_CLIENT_IDS
  # storage: local volume via object_store "file" backend by default; MinIO/Garage optional
  # TLS: bring your own reverse proxy (Caddy/Traefik) — sample Caddyfile provided
```
- Reference host: any 4 vCPU / 8 GB box (Hetzner CX/CAX-class VPS ~€5–15/month, a NUC, a Pi 5 8 GB with slower synthesis).
- Optional `grafana/otel-lgtm` profile for observability in one container.
- Backups: built-in daily `VACUUM INTO` to a backups volume (keeps last N); optional Litestream to S3-compatible storage; documented restore drill. The database volume must be local disk, not NFS/SMB.

### 8.2 ⚑ If hosting were ever revisited (reference only — not a target)

Kept so the consequences of today's decisions stay visible. Nothing here is built or planned. Decisions that would need revisiting: SQLite → Postgres storage adapter ([ADR-0003](adr/0003-sqlite-datastore-and-queue.md)), tenant scoping + RLS ([§2](#2-architectural-style)), quotas as billing tiers ([FR-SUB-06](01-product-spec.md#fr-sub-06)), robots.txt default ([FR-FET-05](01-product-spec.md#fr-fet-05)), cross-tenant caching ([§6](#6-re-use--caching-design-per-instance)), managed IdP and open sign-up ([ADR-0004](adr/0004-auth-oidc-resource-server.md), [FR-AUTH-07](01-product-spec.md#fr-auth-07)), contributor terms ([ADR-0010 F1](adr/0010-licence-and-third-party-policy.md#f1)), model-weights licensing ([ADR-0010 §5](adr/0010-licence-and-third-party-policy.md#5-model-weights-policy)), GDPR and copyright takedown ([doc 06](06-security-and-pentest.md)).

Design principle if it happened: **pay for GPU only while synthesising; never pay for egress on audio.**

| Concern | Choice (initial) | Why | Rough monthly cost at small scale* |
|---------|-----------------|-----|-------------------|
| API + workers | 2× small instances on Fly.io / Hetzner / Cloud Run | Cheap, always-on, simple | $10–40 |
| Database | Managed Postgres (Neon, Crunchy Bridge, Supabase, or Hetzner + backups) | PITR, low ops | $0–30 |
| TTS inference | Serverless GPU (Modal / RunPod Serverless / Cloud Run GPU) scaling 0→N; CPU fallback pool for small jobs | GPU only billed during synthesis | usage-based; ~$0 idle |
| Object storage + CDN | Cloudflare R2 (zero egress) + CDN; or Bunny storage/CDN | Audio is egress-heavy (a 35-min MP3 ≈ 17 MB; each listen downloads it) | $1–10 |
| Identity | Managed OIDC (Zitadel Cloud, WorkOS AuthKit, Auth0, Clerk) — choose by free-tier MAU and passkey support | Don't run your own IdP in prod early | $0 until thousands of MAU |
| Observability | Grafana Cloud free tier / Honeycomb free + Sentry free | | $0–30 |
| Email (account, share invites) | Postmark / SES | | $0–15 |

\*Estimates only, to be replaced by measured figures; provider prices change.

Scaling path when needed: split `api` and `worker` roles into separate deployments → move queue to a dedicated broker (NATS JetStream / SQS) only if Postgres queue contention is measured → regional CDN already handles feed/audio read load (feeds are tiny and cacheable per token with short TTL).
