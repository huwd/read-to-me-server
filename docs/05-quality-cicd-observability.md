# 05 — Quality Stack: CI/CD, Testing & Observability

Rule: **the pipeline exists before the first line of product code.** Iteration 0 delivers a "walking skeleton" — `GET /healthz` returning 200 — that is linted, tested, scanned, built for two architectures, signed, deployed to staging, traced end-to-end and visible on a dashboard. Every later feature rides an already-proven path.

**Environments.** The deployment target is self-hosted, so:
- **staging** = an ephemeral `docker compose` stack started by CI for each merge (optionally also a long-lived staging stack on the owner's homelab);
- **prod** = the owner's own instance, plus the signed release images that other self-hosters pull.

There is no public hosted environment ([doc 07 Q2](07-roadmap-and-open-questions.md#q2)). ⚑ Hosting impact notes mark what would change if there were.

## 1. Repository & workflow conventions

- Trunk-based development; short-lived branches; PRs required; squash merges.
- Branch protection on `main`: required checks (below), 1 review (self-review via Claude/Copilot review acceptable while solo, but checks are never bypassed), linear history, signed commits.
- Conventional Commits → `release-please` generates changelog, semver tags and GitHub Releases.
- `CODEOWNERS`, PR template (what/why/risk/test evidence/FR IDs), issue templates (bug, extraction failure with URL, feature).
- ADRs for every significant decision (`docs/adr/0001-record-architecture-decisions.md` onwards).
- [`SECURITY.md`](../SECURITY.md) (private disclosure policy; security.txt once there's a website), [`CONTRIBUTING.md`](../CONTRIBUTING.md) (DCO sign-off), licence AGPL-3.0-or-later ([ADR-0010](adr/0010-licence-and-third-party-policy.md)).
- Toolchain pinned (`rust-toolchain.toml`, `mise.toml`); `just` recipes mirror CI jobs so "green locally ⇒ green in CI".
- Pre-commit hooks: fmt, clippy (fast), secret scan (Gitleaks or Betterleaks, pending [ADR-0013](adr/0013-secret-scanner.md)), conventional-commit lint.

## 2. CI pipeline (GitHub Actions)

All third-party actions pinned by commit SHA; `permissions: read-all` default with per-job elevation; `step-security/harden-runner` egress audit; caches via `Swatinem/rust-cache`.

### 2.1 On every PR (target: < 10 min wall clock)

| Stage | Job | Gate |
|-------|-----|------|
| Static | `cargo fmt --check` | must pass |
| Static | `cargo clippy --all-targets -- -D warnings` (+ pedantic subset) | must pass |
| Static | DCO: every commit in the PR has a valid `Signed-off-by` | must pass |
| Static | `cargo deny check` (licences allow-list, banned crates, duplicate versions, advisories via RustSec) | must pass |
| Static | `cargo machete` (unused deps) | must pass |
| Static | `sqlx prepare --check` (offline query metadata up to date) | must pass |
| Static | OpenAPI lint (`spectral`) + breaking-change diff (`oasdiff`) vs `main` | breaking change requires label + version bump |
| Static | Semgrep (Rust + generic rules, custom rules: no `Command::new("sh")`, no raw `reqwest::get` outside `rtm-fetch`) | no high findings |
| Static | CodeQL (where language support exists) | no high findings |
| Static | Secret scanning: GitHub secret scanning + push protection, plus a CLI scanner over the diff and full history (Gitleaks or Betterleaks, pending [ADR-0013](adr/0013-secret-scanner.md)) | must pass |
| Static | `actionlint`, `hadolint`, `zizmor` (Actions security lint) | must pass |
| Static | Docs link check: `cargo xtask docs-links check` ([xtask](../xtask/README.md); relative files and `#anchors`), plus a check that every `ADR-NNNN`, `FR-*`, `NFR-*`, `AC-*`, `Q*` and ATT&CK/ATLAS/CWE ID outside headings and code is a link | must pass |
| Test | Unit tests `cargo nextest run` | must pass |
| Test | Property tests (`proptest`): URL canonicalisation, IP classification, XML escaping, normaliser, state machine transitions | must pass |
| Test | Integration tests with real SQLite on temp files, `testcontainers` (MinIO, optional S3 backend), `wiremock` origin servers, **fake TTS engine** (deterministic tone per word, so durations are realistic and fast) | must pass |
| Test | Golden extraction corpus (`insta` snapshots + F1 scoring via `xtask eval-extract`) | F1 must not regress > 0.5 pt vs `main`; snapshot changes need review |
| Test | Coverage (`cargo llvm-cov`) | ≥ 80% lines on core crates; no drop > 1 pt |
| Test | FR traceability check: every `FR-*` in [doc 01](01-product-spec.md) referenced by ≥ 1 test | report; gate from MVP onward |
| Build | Docker build (amd64) with `cargo-chef`; image size budget (≤ 50 MB rtm, excluding TTS) | must pass |
| Security | Trivy/Grype scan of image + `cargo audit` | no fixable high/critical |
| E2E | `docker compose` up (rtm + fake TTS + mock IdP + fixture origin) → Gherkin acceptance suite (`cucumber-rs`) | [AC-AUTH](01-product-spec.md#ac-auth), [AC-SUB](01-product-spec.md#ac-sub), [AC-FET](01-product-spec.md#ac-fet), [AC-FEED](01-product-spec.md#ac-feed), [AC-MGMT](01-product-spec.md#ac-mgmt) must pass |
| E2E | Feed validation: XML schema/namespace checks + podcast-specific rules (our validator + a third-party validator in nightly) | must pass |

### 2.2 On merge to `main`
1. Everything above.
2. Multi-arch build (amd64 + arm64), SBOM (`syft`, SPDX + CycloneDX), provenance attestation (`actions/attest-build-provenance`, SLSA L3-style), **cosign keyless signing**.
3. Push to GHCR as `:sha-<sha>` and `:edge`.
4. Bring up **staging** (ephemeral compose stack from the just-built image, seeded with the previous release's data to exercise migrations); smoke tests; synthetic "submit fixture article → appears in feed" probe.
5. OWASP ZAP baseline + API scan (from OpenAPI) against staging.

### 2.3 Release (tag via release-please)
- Publish the *same signed digest* that passed staging (no rebuild): `:vX.Y.Z`, `:X.Y`, `:latest`, compose bundle, release notes with migration notes.
- Upgrade test: start release N-1 with data, upgrade to N, run the acceptance suite, then roll back to N-1 and check it still starts ([NFR-M3](03-architecture-and-nfrs.md#nfr-m3)/[M4](03-architecture-and-nfrs.md#nfr-m4)).
- Deploy to the owner's instance: pull by digest, verify cosign signature + provenance before starting; automatic rollback to the previous digest if the synthetic probe fails within 30 min.
- ⚑ Hosting impact: a public service would add blue/green or canary traffic shifting and SLO-burn-triggered rollback.

### 2.4 Scheduled (nightly / weekly)
| Job | Cadence | Purpose |
|-----|---------|---------|
| **Real-model synthesis test**: Kokoro (Rust/ONNX) on CPU, amd64 and arm64, 10 articles → ASR round-trip (Whisper) WER ≤ 8%, loudness −16 ±1 LUFS, no clipping, duration within expected words-per-minute band | Nightly | Catch TTS regressions the fake engine can't. |
| Fuzzing (`cargo-fuzz`: URL parser/egress guard, HTML extractor, PDF adapter input, feed writer, normaliser) 30 min each; corpus persisted; crash → issue | Nightly | Untrusted-input robustness. |
| Mutation testing (`cargo-mutants`) on `rtm-fetch`, `rtm-domain`, auth middleware | Weekly | Test-suite quality where it matters most. |
| Live-site extraction canary: 50 real URLs from popular sources; alert on F1 drop / fetch failure | Nightly | Sites change markup; adapters rot. |
| Load test (k6) against staging: 10 rps API, 50 rps feed (several podcast apps polling), 20 queued jobs across 4 users | Weekly | [NFR-P1](03-architecture-and-nfrs.md#nfr-p1)/[P4](03-architecture-and-nfrs.md#nfr-p4)/[S1](03-architecture-and-nfrs.md#nfr-s1)/[S2](03-architecture-and-nfrs.md#nfr-s2) regression. Sized for a household instance. |
| Chaos: `kill -9` the server mid-stage and mid-write, kill the TTS process, TTS 503s → assert no lost/duplicated items and `PRAGMA integrity_check` = ok | Weekly | [NFR-R2](03-architecture-and-nfrs.md#nfr-r2)/[R3](03-architecture-and-nfrs.md#nfr-r3), [NFR-S1](03-architecture-and-nfrs.md#nfr-s1). |
| Podcast app compatibility (manual checklist until automatable) | Per release | [NFR-C2](03-architecture-and-nfrs.md#nfr-c2). |
| Dependency updates (Dependabot for Cargo and Actions, grouped, 7-day cooldown, reviewed like any PR; [`.github/dependabot.yml`](../.github/dependabot.yml)) | Weekly | Keep the supply chain fresh without adopting releases in their riskiest first days. |
| OpenSSF Scorecard | Weekly | Supply-chain posture badge. |
| Backup restore drill: restore the latest built-in backup file into a fresh stack and run the acceptance suite | Weekly in CI; monthly on the owner's instance | [NFR-R5](03-architecture-and-nfrs.md#nfr-r5) is real only if tested. |

### 2.5 Client CI (deferred)
Out of scope for now. The client strategy is undecided: fully native Swift/Kotlin apps, thin native wrappers around a server-hosted web UI, or neither (shortcuts + web UI); see [doc 07 Q3](07-roadmap-and-open-questions.md#q3) and [ADR-0012](adr/README.md). This section will be written once that decision is made. Until then, the server's quality gates treat the OpenAPI spec as the only client contract.

## 3. Test strategy (pyramid)

| Layer | What | Tooling | Approx share |
|-------|------|---------|-------------|
| Unit | Domain logic, state machine, normaliser, triage scoring, IP rules | `nextest`, `proptest`, `insta` | 60% |
| Contract | OpenAPI conformance (server responses validated against spec); SpeechEngine HTTP contract | `schemathesis` (property-based API fuzzing from OpenAPI), Pact-style contract for TTS | 10% |
| Integration | Repos vs real SQLite (temp files), object store vs local FS and MinIO, fetcher vs wiremock + DNS stubs | `testcontainers`, `wiremock`, `hickory-dns` test server | 20% |
| E2E / acceptance | Gherkin scenarios from [doc 01](01-product-spec.md) against compose stack | `cucumber-rs` | 8% |
| Non-functional | Load, chaos, fuzz, mutation, real-model quality | k6, cargo-fuzz, cargo-mutants, Whisper | 2% (scheduled) |

**Golden corpus:** `fixtures/corpus/<site>/<slug>/{source.html, expected.md, meta.toml}` — ≥ 100 articles, ≥ 30 sites, including hostile cases (cookie walls, infinite scroll stubs, multi-page, AMP, footnotes, code-heavy, non-English, malformed HTML, script injection). Licence: store only content we're allowed to (public-domain/CC sources) or synthetic fixtures; private real-world cases stay in a private submodule.

## 4. Observability stack

### 4.1 Plumbing
- **OpenTelemetry everywhere** (server via `tracing-opentelemetry`; TTS process via the same Rust `tracing` stack). W3C `traceparent` accepted from any caller and propagated API → worker (stored on the job so async stages join the same trace) → TTS.
- **Self-host:** optional `grafana/otel-lgtm` container (Grafana + Loki + Tempo + Prometheus/Mimir) with provisioned dashboards shipped in the repo. Prometheus `/metrics` also exposed for existing homelab setups.
- **Owner's instance:** the same LGTM stack (or an existing homelab Grafana/Prometheus); alerts via ntfy/Pushover/email; an external uptime check (e.g. Uptime Kuma or a free uptime service) on the feed URL; optional self-hosted Sentry-compatible error tracking (GlitchTip).
- **Product analytics:** computed from the server's own database and metrics and shown on a provisioned Grafana dashboard. No third-party analytics, and **no phone-home telemetry** from other people's instances. No content, no full URLs: domain + item id only.
- ⚑ Hosting impact: a public service would add managed observability (Grafana Cloud/Honeycomb, Sentry), multi-region synthetics, real paging and product analytics (e.g. PostHog).
- Logs: structured JSON, `trace_id`/`span_id`/`user_id_hash` on every line; redaction layer enforced by a `tracing` filter + test that greps logs during E2E for fixture article text (must be absent).

### 4.2 Golden-signal & health metrics (RED/USE)
| Metric | Type | Labels (bounded cardinality) |
|--------|------|------------------------------|
| `http_server_request_duration_seconds` | histogram | route, method, status_class |
| `rtm_jobs_enqueued_total`, `rtm_jobs_completed_total`, `rtm_jobs_failed_total` | counter | stage, failure_code |
| `rtm_job_stage_duration_seconds` | histogram | stage |
| `rtm_time_to_feed_seconds` | histogram | source_kind, words_bucket |
| `rtm_queue_depth`, `rtm_queue_oldest_age_seconds` | gauge | stage |
| `rtm_tts_realtime_factor` | histogram | engine, voice, hardware |
| `rtm_extract_confidence` | histogram | extractor |
| `rtm_extract_low_confidence_total` | counter | extractor, domain_bucket (top-N domains else "other") |
| `rtm_egress_blocked_total` | counter | reason (private_ip, scheme, redirect, size) — **security signal** |
| `rtm_auth_failures_total` | counter | reason (expired, bad_sig, aud, scope) — **security signal** |
| `rtm_feed_requests_total`, `rtm_enclosure_bytes_total` | counter | status, cache_hit |
| `rtm_storage_bytes` | gauge | kind |
| `rtm_cache_hit_ratio` | gauge | layer (extract/chunk/episode) |
| USE: CPU, memory, disk, DB connections, DB slow queries, GPU utilisation/memory | | per service |

### 4.3 SLOs (personal targets for a self-hosted instance)

These are objectives for the owner's instance and the defaults shipped in the dashboards; alerts notify (ntfy/email) rather than page. ⚑ Hosting impact: a public service would need tighter availability targets, error budgets with paging, and a status page.
| SLO | SLI | Objective | Alerting |
|-----|-----|-----------|----------|
| API availability | non-5xx / total on `/v1/*` | 99% / 28d | Burn-rate alert (slow window only) |
| Submission latency | `POST /v1/items` ≤ 300 ms | 95% | burn-rate |
| Pipeline success | completed / (completed + system-failed) | 99% | burn-rate; plus alert on any stage with > 10 consecutive failures |
| Time-to-feed | 5k-word items published ≤ 10 min (CPU) / 3 min (optional GPU) | 95% | burn-rate |
| Feed availability | non-5xx on feed + enclosure | 99.5% | burn-rate + external uptime check |
| Freshness | `queue_oldest_age` < 15 min | — | threshold alert |

Each alert links to a runbook in `docs/runbooks/` (created alongside the alert, reviewed in PR).

### 4.4 Product success metrics

Measured on the owner's instance (and any household users) from local data. Project-level success (other self-hosters) is measured from public signals only: image pulls, GitHub stars/issues/discussions, external contributors, extractor adapters contributed.

| Tier | Metric | Definition | Target (initial hypothesis) |
|------|--------|-----------|------------------------------|
| **North star** | **Weekly listened audio hours** | Sum of enclosure bytes served (deduped per item per client, Range-aware) ÷ bitrate → approx. hours listened | Growing week on week; for solo dogfood ≥ 3 h/week |
| Activation | Time to first published episode from `docker compose up` on a fresh host | measured in the [AC-OPS-01](01-product-spec.md#ac-ops) E2E test and by beta self-hosters | ≤ 10 min |
| Activation | % new users who add the feed to a podcast app | first feed fetch by a podcast UA within 24 h | ≥ 70% |
| Engagement | Submissions per active user per week | | ≥ 5 |
| Engagement | **Listen-through rate** | items with ≥ 90% of bytes fetched ÷ items published | ≥ 50% (lower ⇒ extraction/voice quality or wrong content) |
| Quality | Unattended success rate | published without retry/force ÷ submitted | ≥ 85% |
| Quality | "Report a problem" rate | reports ÷ published | ≤ 3% |
| Retention | W1 / W4 retention (submitted in week N) | | ≥ 60% / ≥ 40% |
| Channel | Submission source mix | share shortcut / web UI / PAT / API | informs the client decision ([doc 07 Q3](07-roadmap-and-open-questions.md#q3)) |
| Hygiene | Prune usage | % users using prune/auto-prune | signals feed bloat pain |

Guardrail metrics: failure rate, p95 time-to-feed, GitHub issues per release.

Caveat: podcast "listens" are inferred from downloads (podcast apps pre-download). Treat as directional; complement with an optional "mark as listened" action in the API.

### 4.5 Production readiness review (gate before running it as your daily instance and before the v1.0 release)
- [ ] All SLOs defined, dashboards provisioned as code, alerts → runbooks.
- [ ] Synthetic end-to-end probe running in staging and on the owner's instance.
- [ ] Log redaction test passing; PII inventory documented.
- [ ] Backups configured *and* a restore tested.
- [ ] Threat model reviewed; [doc 06](06-security-and-pentest.md) baseline controls verified; pentest round 1 findings (high/critical) closed.
- [ ] Rate limits and quotas enforced and load-tested.
- [ ] Alerts reach the owner's phone (ntfy/Pushover); runbooks exist for each alert.
- [ ] Rollback rehearsed.

## 5. Definition of Done (per story)
Code + tests (unit/integration as appropriate, acceptance scenario if user-facing) · FR IDs referenced · OpenAPI updated · telemetry added (span + metric for new stage/endpoint) · docs/runbook updated · security considerations noted in PR · CI green · smoke-checked in staging.

## 6. Benchmark plan (to retire assumptions A1–A3 in week 1)
- Reference machines: (a) 4 vCPU x86 VPS, (b) Raspberry Pi 5 8 GB, (c) Apple M-series via Docker, (d) optionally a consumer GPU (to document the GPU image's benefit for self-hosters).
- Corpus: 20 articles at 1k / 5k / 15k words.
- Record: per-stage time, RTF, peak RSS, output size, subjective MOS (1–5) over 5 long listens.
- Output: ADR "Default TTS engine and hardware guidance", and real numbers replacing the estimates in docs [02](02-product-viability.md)/[03](03-architecture-and-nfrs.md).
