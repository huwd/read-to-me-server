# 07 — Roadmap, Decisions & Open Questions

## 1. Phased roadmap

| Phase | Goal | Key deliverables | Exit criteria |
|-------|------|------------------|---------------|
| **0. Spike (1 week)** — ✅ approved 2026-10-11 | Retire riskiest assumptions | Throwaway scripts: extraction over your last 100 saved articles; Kokoro listening test; RTF benchmark on reference hardware; **Rust/ONNX vs Python-reference TTS comparison tool** ([ADR-0007](adr/0007-tts-engine-rust-kokoro.md) validation) | Go/no-go; ADRs for TTS engine, extractor baseline; numbers in docs [02](02-product-viability.md)/[03](03-architecture-and-nfrs.md) replaced with measurements |
| **1. Foundations (iteration 0)** | Quality stack before product code | Repo conventions, CI ([doc 05 §2.1–2.3](05-quality-cicd-observability.md#21-on-every-pr-target--10-min-wall-clock)), minimal server (`/healthz`), then the walking skeleton: one URL end to end into the feed through every component, with each stage trivial ([#12](https://github.com/huwd/read-to-me-server/issues/12)), OTel → LGTM, compose stack, staging env, threat model v1, ADRs 0001–0010 | Walking skeleton runs in CI: a signed multi-arch image under compose turns a fixture URL into a playable feed episode, traced, dashboarded, alerting tested |
| **2. MVP server** | URL → private podcast feed, single user | OIDC resource server (generic provider config), PATs, egress guard, generic-html extractor + triage registry, narrator/normaliser, Kokoro service, ffmpeg encoder, feed + Range serving, item CRUD/prune, SSE, iOS Shortcut + Android share-shortcut recipes | MVP success criteria ([doc 01 §7](01-product-spec.md#7-product-success-criteria-mvp-exit)) |
| **3. Self-host v1.0** | Something others can run | Hardening, docs (mdBook), upgrade/rollback, backup sidecar, pentest round 1, minimal web UI (status + prune) | Pentest exit criteria; [AC-OPS](01-product-spec.md#ac-ops) pass on clean host; 3 external beta self-hosters |
| **4. Extensibility** | Better extraction, more inputs | HTML/PDF upload, site adapters (top 10 domains from the owner's instance and user reports), JSON-LD path, chapters + transcripts, multiple feeds, voices, Wallabag/Readeck/Karakeep integrations | Golden corpus F1 ≥ 0.93; PDF adapter on 30-paper corpus |
| **5. Clients** | Better capture & management UX | Depends on [Q3](#q3) (web UI + shortcuts / native wrappers / native apps): sign-in, share-in (URL/PDF/HTML), status, feed management/prune, "add to podcast app" onboarding | [AC-APP](01-product-spec.md#ac-app) pass; MASVS L1 if native |
| **6. Household & sharing (stretch)** | Several people on one instance | Admin user management, per-user feeds, share/accept/decline/block between users on the instance | [AC-SHARE](01-product-spec.md#ac-share) pass |

A hosted public service is **not on the roadmap** ([Q2](#q2)). [Doc 03 §8.2](03-architecture-and-nfrs.md#82--if-hosting-were-ever-revisited-reference-only--not-a-target) lists what would change if it were ever revisited; ⚑ Hosting impact notes across the docs mark the affected decisions.

## 2. ADRs

All are drafted; see the [ADR index](adr/README.md) for status. Original backlog:
1. Record architecture decisions (MADR).
2. Rust modular monolith (single process) + separate Rust TTS process.
3. ✅ [SQLite as the only datastore and job queue](adr/0003-sqlite-datastore-and-queue.md) — accepted.
4. OAuth 2.1/OIDC resource server with generic provider config (no bundled IdP); PATs and feed capability tokens.
5. Egress guard design (resolve-pin-validate).
6. Extractor triage/adapter contract and versioning.
7. ✅ [Default TTS engine: Kokoro in Rust (ONNX Runtime), separate process](adr/0007-tts-engine-rust-kokoro.md) — accepted, pending spike validation.
8. Content-addressed storage and cache-key derivation.
9. Audio format (MP3 64 kbps mono, −16 LUFS) and feed namespaces.
10. ~~Licence for the project and third-party licence policy~~ → **Accepted:** [ADR-0010](adr/0010-licence-and-third-party-policy.md).
11. Observability: OTel + LGTM on the instance; alerts via ntfy/email; no phone-home telemetry.
12. Client strategy (web UI + shortcuts / native wrappers around the web UI / fully native) — **deferred**, see [Q3](#q3).
13. Secret scanner: Gitleaks vs Betterleaks — [proposed](adr/0013-secret-scanner.md).
14. ✅ [Kokoro's pronunciation fork and native build](adr/0014-kokoro-dependencies-and-build.md): `misaki-rs` fork pinned by commit; Kokoro behind a `kokoro` cargo feature with its own CI job — accepted.

## 3. Open questions for you

| # | Question | Why it matters | My default recommendation |
|---|----------|----------------|---------------------------|
| <a id="q1"></a>Q1 | ~~Licence~~ | **Decided** in [ADR-0010](adr/0010-licence-and-third-party-policy.md): server `AGPL-3.0-or-later`, apps/SDKs `Apache-2.0`, docs `CC-BY-4.0`. Contributor terms: DCO ([ADR-0010 F1](adr/0010-licence-and-third-party-policy.md#f1)). | — |
| <a id="q2"></a>Q2 | ~~Is the hosted service a real goal?~~ | **Decided (2026-10-11): no.** Self-hosted (single user / household) is the only target. Hosting is not designed for; decisions that would affect it carry a ⚑ Hosting impact note. | — |
| <a id="q3"></a>Q3 | Client strategy: (a) web UI + shortcuts only, (b) **thin native wrappers** (Swift/Kotlin shells loading the server's web UI, with native share-sheet, sign-in and token storage), or (c) fully native apps with generated API clients? | Biggest effort/value lever; also decides whether client SDKs exist at all. | **Deferred** — owner not ready to decide. Revisit after the MVP server; (b) is a promising middle ground. Recorded in [ADR-0012](adr/README.md) when decided. |
| <a id="q4"></a>Q4 | ~~robots.txt policy~~ | **Decided 2026-10-11:** respect robots.txt by default (RFC 9309); an instance-wide "disrespectful mode" flag/config turns it off; blocked users can supply the HTML/PDF themselves. [FR-FET-07](01-product-spec.md#fr-fet-07)/[08](01-product-spec.md#fr-fet-08). | — |
| <a id="q5"></a>Q5 | ~~Languages at launch~~ | **Decided 2026-10-11:** English only at launch, with language isolated behind per-language registries so adding one is a module + fixtures ([FR-TTS-07](01-product-spec.md#fr-tts-07), [NFR-M6](03-architecture-and-nfrs.md#nfr-m6)). | — |
| <a id="q6"></a>Q6 | ~~Bring-your-own cloud TTS keys~~ | **Decided 2026-10-11:** yes eventually, but Kokoro is the only engine initially. Build the engine port and registry for it ([NFR-M7](03-architecture-and-nfrs.md#nfr-m7)); build no other engines now. | — |
| <a id="q7"></a>Q7 | ~~Long articles~~ | **Decided 2026-10-11:** limit 30,000 words (≈ 3 h 15 min), configurable; one episode per item with chapters; nothing appears in the feed until the whole file is ready. Parts/progressive publishing and chunk-level scheduling deferred. [FR-TTS-04](01-product-spec.md#fr-tts-04)/[06](01-product-spec.md#fr-tts-06), [NFR-P2b](03-architecture-and-nfrs.md#nfr-p2b), [NFR-S2](03-architecture-and-nfrs.md#nfr-s2). | — |
| <a id="q8"></a>Q8 | ~~Artwork~~ | **Decided 2026-10-11:** one bundled default image; custom per-feed upload once a web UI exists; favicon/lead-image artwork deferred ([FR-FEED-08](01-product-spec.md#fr-feed-08)). | — |
| <a id="q9"></a>Q9 | ~~LLM involvement~~ | **Decided 2026-10-11:** no LLM anywhere in the pipeline. Revisiting needs a new ADR and a threat-model update. | — |

## 4. Deferred: revisit later

Things discussed and deliberately parked, so they aren't lost.

| Item | Why deferred | Revisit when |
|------|--------------|--------------|
| HTML / PDF upload ([FR-SUB-04](01-product-spec.md#fr-sub-04)) | Keeps the MVP small. Until it lands, a robots.txt block can only be bypassed with the instance-wide override | After MVP (phase 4) |
| Multi-part / progressive publishing of long items | Simplicity: one item = one episode, published when complete | If waiting for long items proves annoying in daily use |
| Chunk-level fair scheduling (interleaving items in TTS) | Simplicity; known limitation recorded in [NFR-S2](03-architecture-and-nfrs.md#nfr-s2) | If short items regularly wait behind long ones |
| Client strategy: web UI / native wrappers / native apps ([Q3](#q3), [ADR-0012](adr/README.md)) | Owner not ready to decide | After the MVP server works |
| Additional speech engines, incl. cloud with the user's own key ([Q6](#q6)) | Stay focused on Kokoro | After v1.0 |
| Languages beyond English ([Q5](#q5)) | English first; architecture ready | When a contributor or the owner needs one |
| Custom feed artwork upload; favicon/lead-image episode art ([Q8](#q8)) | Needs a web UI | With the web UI |
| Secret scanner choice ([ADR-0013](adr/0013-secret-scanner.md)) | Betterleaks v2 not yet stable | Iteration 0 |
