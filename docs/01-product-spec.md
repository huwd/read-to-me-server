# 01 — Product Specification & Acceptance Criteria

> Working name: **read-to-me**. A self-hostable service that turns long-form web articles and documents into natural-sounding narrated audio, published to a private podcast feed.

## 1. Problem statement (one paragraph)

People save more long-form reading than they ever get through. Their listening time (commute, exercise, chores) is mostly spent in a podcast app. read-to-me connects the two: send it an article and, a few minutes later, the article shows up as an episode in your own podcast app, read aloud verbatim by a good-quality neural voice. You get the player you already use (speed control, offline, CarPlay/Android Auto, sync) without a new app.

> **Deployment target:** a self-hosted instance for one person or a household. Running a hosted service for the public is **not a target**. Where a requirement or decision would matter if that ever changed, it is flagged **⚑ Hosting impact** — informational only, not something to build for.

## 2. Personas

| ID | Persona | Description | Phase |
|----|---------|-------------|-------|
| P1 | **Self-hoster** | Technically capable; runs a homelab or a small VPS; values privacy and owning their data. Also the instance admin. | MVP |
| P2 | **Household member** | Has an account on someone else's self-hosted instance (partner, family, friend). Not technical. | Post-MVP |
| P3 | **Recipient** | Another user on the same instance who is sent an item and chooses to accept it. | Stretch |

Not targeted: a paying user of a hosted service, or an operator running one. (⚑ Hosting impact: these personas would bring billing, abuse handling, legal and SLO obligations that are deliberately absent from this spec.)

## 3. Scope

### In scope (MVP)
- Authenticated HTTP API (OAuth 2.1 / OIDC from day one).
- Submit a URL; server fetches it safely, extracts the article, narrates it, encodes audio, publishes to a private RSS/podcast feed.
- Job status (queued → … → published / failed with reason).
- Item management: list, view, delete (prune), retry.
- One private feed per user, accessible to podcast apps via an unguessable, revocable feed token.
- Docker image + `docker compose` bundle for self-hosting.
- Full CI/CD, observability and security baseline (see docs [05](05-quality-cicd-observability.md) and [06](06-security-and-pentest.md)).

### In scope (post-MVP, designed-for now)
- Upload of raw HTML and PDF documents.
- Site-specific extraction adapters, improved heuristics.
- Multiple feeds per user, voice selection, chapters, transcripts. (Playback speed is deliberately left to the podcast player.)
- Client apps with share-sheet integration — strategy deferred (native, native wrappers around a web UI, or web UI + shortcuts; [doc 07 Q3](07-roadmap-and-open-questions.md#q3)).
- A handful of users per instance (household), each with their own feeds.
- Sharing items between users on the same instance (with recipient acceptance).

### Explicitly out of scope
- Summarisation / rewriting of content. Narration is **verbatim** (naive reproduction).
- **Any LLM in the pipeline** (extraction, clean-up, pronunciation or otherwise). Decided in [doc 07 Q9](07-roadmap-and-open-questions.md#q9); adding one would need a new ADR and a threat-model update ([doc 06 §4.4](06-security-and-pentest.md#44-mitre-atlas-ml-specific)).
- Bypassing paywalls, logins, or bot protection on the server's side. If a user can read it, they can send the HTML/PDF themselves.
- Public (non-private) podcast publishing or directory submission.
- Voice cloning of real people.
- Operating a public hosted service (multi-tenant SaaS, billing, sign-up). ⚑ Hosting impact notes record where decisions would affect this if it is ever revisited.

## 4. Glossary

| Term | Meaning |
|------|---------|
| **Submission** | A user request to process a source (URL, HTML upload, PDF upload). |
| **Source document** | The raw bytes fetched/uploaded, plus metadata (final URL, content-type, headers). |
| **Triage** | Selecting the best extractor for a source document. |
| **Extractor / Adapter** | A pluggable component that turns a source document into an `Article`. |
| **Article** | A structured, sanitised document model (title, byline, date, sections, paragraphs, lists, quotes, code, figures, tables). Never raw HTML. |
| **Narration script** | The `Article` rendered into a sequence of speakable segments with pauses, after text normalisation. |
| **Episode** | The encoded audio + metadata as it appears in the feed. |
| **Feed** | An RSS 2.0 document with iTunes and Podcasting 2.0 namespaces, scoped to one user. |
| **Feed token** | A high-entropy, revocable secret embedded in the feed URL (podcast apps cannot do OAuth). |

## 5. Functional requirements

Requirement IDs are stable and referenced from tests (`#[test] // FR-SUB-01`) and from the acceptance criteria below.

### 5.1 Authentication & authorisation (AUTH)
- <a id="fr-auth-01"></a>**FR-AUTH-01** All API endpoints except health checks and the feed/enclosure endpoints require an OAuth 2.1 bearer access token issued by a configured OIDC provider.
- <a id="fr-auth-02"></a>**FR-AUTH-02** The server acts as an OAuth **resource server**: it validates JWT access tokens (signature via JWKS, `iss`, `aud`, `exp`, `nbf`) and maps `sub` to a local user.
- <a id="fr-auth-03"></a>**FR-AUTH-03** Native apps authenticate with Authorization Code + PKCE using the system browser (RFC 8252). No embedded web views, no password grant.
- <a id="fr-auth-04"></a>**FR-AUTH-04** Scopes: `items:read`, `items:write`, `feeds:read`, `feeds:admin`. Endpoints enforce least privilege.
- <a id="fr-auth-05"></a>**FR-AUTH-05** Users can create scoped, expiring **personal access tokens** (PATs) for automation (iOS Shortcuts, CLI, browser bookmarklet). PATs are stored hashed, shown once, revocable.
- <a id="fr-auth-06"></a>**FR-AUTH-06** The server works with any standards-compliant OIDC provider, configured generically (issuer URL, expected audience, allowed client IDs; endpoints and keys via OIDC discovery + JWKS). No provider is bundled, assumed or special-cased; the self-hoster brings their own.
- <a id="fr-auth-07"></a>**FR-AUTH-07** First-user bootstrap: the first authenticated `sub` (or one configured via env `RTM_ADMIN_SUB`) becomes admin; subsequent sign-ins are rejected unless the admin has allow-listed them (household members). There is no open sign-up.

### 5.2 Submission (SUB)
- <a id="fr-sub-01"></a>**FR-SUB-01** `POST /v1/items` with `{ "url": "https://…" }` creates a submission and returns `202 Accepted` with an item ID and status URL.
- <a id="fr-sub-02"></a>**FR-SUB-02** Only `http`/`https` schemes are accepted; URL length ≤ 2,048 chars; userinfo (`user:pass@`) rejected.
- <a id="fr-sub-03"></a>**FR-SUB-03** Submissions are idempotent per user via an `Idempotency-Key` header; resubmitting the same canonical URL within a configurable window returns the existing item unless `force=true`.
- <a id="fr-sub-04"></a>**FR-SUB-04** (post-MVP) `POST /v1/items` multipart with an HTML or PDF file (≤ 20 MB default) plus optional `source_url` for attribution.
- <a id="fr-sub-05"></a>**FR-SUB-05** Optional per-submission options: voice, title override, target feed.
- <a id="fr-sub-06"></a>**FR-SUB-06** Per-user quotas (submissions/day, characters/day, storage). Exceeding returns `429` with `Retry-After`. Quotas default to generous values and exist mainly to bound runaway jobs and protect the host. (⚑ Hosting impact: quotas become the primary abuse/cost control and would need plan tiers.)

### 5.3 Fetching (FET)
- <a id="fr-fet-01"></a>**FR-FET-01** The server fetches URLs through an **egress guard**: DNS resolved once, every resolved address checked against a deny-list (loopback, RFC1918, link-local incl. `169.254.169.254`, CGNAT, ULA, multicast, IPv4-mapped IPv6, etc.), connection pinned to the vetted IP (prevents DNS rebinding).
- <a id="fr-fet-02"></a>**FR-FET-02** Redirects followed manually (max 5), each hop re-validated against [FR-FET-01](#fr-fet-01) and [FR-SUB-02](#fr-sub-02).
- <a id="fr-fet-03"></a>**FR-FET-03** Limits: connect timeout 5 s, total 30 s, max body 10 MB (HTML) / 50 MB (PDF), **measured after decompression**. There is no separate compression-ratio limit ([ADR-0005 Amendment 1](adr/0005-egress-guard.md#amendment-1-2026-10-15-no-separate-decompression-ratio-limit)).
- <a id="fr-fet-04"></a>**FR-FET-04** Content-Type allow-list: `text/html`, `application/xhtml+xml`, `application/pdf`, `text/plain`. Sniffed content must agree with the declared type.
- <a id="fr-fet-05"></a>**FR-FET-05** Honest User-Agent identifying the service (product token `read-to-me`) and a project URL. No impersonation of browsers.
- <a id="fr-fet-07"></a>**FR-FET-07** **robots.txt is respected by default** (RFC 9309), matching our product token and falling back to `*`:
  - fetched through the same egress guard, capped at 500 KiB, cached per origin for 24 h;
  - a 4xx robots.txt means "allowed"; 5xx or unreachable means "disallowed" for now, and the item is retried later;
  - a disallowed URL fails with `ROBOTS_DISALLOWED`, and the message tells the user they can supply the HTML or PDF themselves ([FR-SUB-04](#fr-sub-04), post-MVP). Until upload exists, the only way round a block is the instance-wide override in [FR-FET-08](#fr-fet-08).
- <a id="fr-fet-08"></a>**FR-FET-08** **"Disrespectful mode"**: an instance-wide setting (`RTM_FETCH_IGNORE_ROBOTS=true` in config, or the `--ignore-robots` server flag) turns robots.txt checks off. It is off by default, logs a warning at startup, and is shown on the instance's status page. It is not a per-request option.
- <a id="fr-fet-06"></a>**FR-FET-06** No cookies, no credentials, no JavaScript execution in MVP. (A headless-browser fetcher is a possible later adapter, run in a separate sandbox.)

### 5.4 Triage & extraction (EXT)
- <a id="fr-ext-01"></a>**FR-EXT-01** Extraction uses a **triage → adapter** pattern: every registered extractor returns a confidence score for a source document; the highest score above a threshold wins; ties broken by specificity (site-specific > format-specific > generic).
- <a id="fr-ext-02"></a>**FR-EXT-02** Default extractors: `generic-html` (readability-style heuristic), `plain-text`. Post-MVP: `pdf`, site adapters (e.g. Substack, Medium, Wikipedia, arXiv, GitHub README, Ghost, WordPress via REST/JSON-LD).
- <a id="fr-ext-03"></a>**FR-EXT-03** Extractors output an `Article` model — never HTML. The model includes title, byline, published date, canonical URL, lead image (alt text), and ordered blocks.
- <a id="fr-ext-04"></a>**FR-EXT-04** Extractors are versioned; the extractor id+version is recorded on the item (for reproducibility, caching keys and bug reports).
- <a id="fr-ext-05"></a>**FR-EXT-05** If no extractor exceeds the threshold, or extracted text < 200 words (configurable), the job fails with `EXTRACTION_LOW_CONFIDENCE` and a user-visible explanation; the user may force processing anyway.
- <a id="fr-ext-06"></a>**FR-EXT-06** Extraction runs in a sandboxed worker process with CPU/memory/time limits (see [doc 06](06-security-and-pentest.md)).
- <a id="fr-ext-07"></a>**FR-EXT-07** Users can report "extraction was wrong" on an item; the report stores the source document (if retention allows) for the golden corpus.

### 5.5 Narration & synthesis (TTS)
- <a id="fr-tts-01"></a>**FR-TTS-01** The `Article` is converted to a narration script: title, byline ("By …"), date, then body. Headings get longer pauses; lists are read with ordinal cues; block quotes optionally announced; code blocks summarised as "Code block omitted" by default (configurable); tables read as "Table omitted" in MVP; image captions read if present.
- <a id="fr-tts-02"></a>**FR-TTS-02** Text normalisation: numbers, dates, currencies, units, common abbreviations, URLs (read as domain only), emoji stripped, Unicode NFC, homoglyph and control-character removal.
- <a id="fr-tts-03"></a>**FR-TTS-03** The TTS engine sits behind a `SpeechEngine` port. **Kokoro (open weights, Apache-2.0) is the only engine shipped initially.** The port, engine registry and configuration are designed so other engines can be added later as self-contained adapters, including cloud APIs with the user's own key (stored as a secret, never logged). None are built now.
- <a id="fr-tts-04"></a>**FR-TTS-04** Text is segmented at sentence boundaries into model-sized pieces internally (Kokoro has a bounded input length). Pieces are cached by hash so a crash resumes mid-article, then concatenated with deterministic silence padding into **one** audio file. Pieces are never published on their own: an item appears in the feed only when its whole episode is encoded (no partial or multi-part episodes; see [doc 07 "Deferred"](07-roadmap-and-open-questions.md#4-deferred-revisit-later)).
- <a id="fr-tts-05"></a>**FR-TTS-05** Text sent to the engine is plain text only — any markup/SSML the engine supports is generated by us from the script, never passed through from user content.
- <a id="fr-tts-06"></a>**FR-TTS-06** Max article length: **30,000 words** by default (≈ 3 h 15 min of audio at ~155 words/min), configurable via `RTM_MAX_WORDS`. It's checked after extraction and before synthesis; longer input fails with `TOO_LONG`, stating the word count and the limit. Each item becomes a single episode with chapter markers from its headings.
- <a id="fr-tts-07"></a>**FR-TTS-07** **English only at launch, built for more languages.** Language is first-class throughout:
  - the `Article` carries a detected or declared language (BCP 47 tag);
  - normaliser, G2P/lexicon and voice are each selected per language through a registry;
  - adding a language means adding a language module (normaliser rules, lexicon, voice mapping) plus test fixtures, with no changes to pipeline or core code ([NFR-M6](03-architecture-and-nfrs.md#nfr-m6));
  - content in an unsupported language fails with `LANGUAGE_UNSUPPORTED` rather than being read badly in an English voice (an override lets the user force it).

### 5.6 Encoding & storage (ENC)
- <a id="fr-enc-01"></a>**FR-ENC-01** Output MP3 (MPEG-1 Layer III, CBR, mono, 64 kbps, 44.1 kHz) for maximal podcast-app compatibility ([ADR-0009](adr/0009-audio-format-and-feed.md)).
- <a id="fr-enc-02"></a>**FR-ENC-02** ID3v2 tags: title, artist (author/site), album (feed name), chapter frames (CHAP/CTOC) from headings.
- <a id="fr-enc-03"></a>**FR-ENC-03** Audio stored in an object store (local filesystem or S3-compatible) under a content-addressed key.
- <a id="fr-enc-04"></a>**FR-ENC-04** Loudness normalised to −16 LUFS (podcast norm) ±1 LU.

### 5.7 Feed (FEED)
- <a id="fr-feed-01"></a>**FR-FEED-01** Each user has ≥1 feed at `GET /feeds/{feed_token}.xml`. RSS 2.0 + `itunes:` + `podcast:` namespaces. Valid per common feed validators.
- <a id="fr-feed-02"></a>**FR-FEED-02** Each episode: title, `guid` (stable, `isPermaLink=false`), `pubDate`, enclosure (url, length, type), `itunes:duration`, description (plain text + link to source), `podcast:transcript` (text/plain or VTT), `podcast:chapters` (JSON).
- <a id="fr-feed-03"></a>**FR-FEED-03** Feed marked private: `itunes:block yes`, `<podcast:locked>yes</podcast:locked>`, `X-Robots-Tag: noindex`.
- <a id="fr-feed-04"></a>**FR-FEED-04** Enclosures are served by the server under the feed token, supporting HTTP Range requests (mandatory; podcast apps seek), `ETag` and `410 Gone` for deleted items ([ADR-0009](adr/0009-audio-format-and-feed.md)).
- <a id="fr-feed-05"></a>**FR-FEED-05** Feed tokens are ≥128-bit random, stored hashed, rotatable. Rotation invalidates the old URL (with optional grace period).
- <a id="fr-feed-06"></a>**FR-FEED-06** Feed responses support `ETag`/`If-None-Match` and `Last-Modified`; podcast apps poll frequently.
- <a id="fr-feed-07"></a>**FR-FEED-07** Feed shows the most recent N items (default 100); older items remain in the API until pruned.
- <a id="fr-feed-08"></a>**FR-FEED-08** Artwork:
  - every feed uses one bundled default image (square PNG, 3000×3000, within Apple Podcasts' 1400–3000 px range), and episodes inherit it;
  - later, once a web UI exists, the user can upload a custom image per feed;
  - per-episode artwork (site favicon or lead image) is deferred, and would be fetched through the egress guard.

### 5.8 Status (STAT)
- <a id="fr-stat-01"></a>**FR-STAT-01** `GET /v1/items/{id}` returns status: `queued`, `fetching`, `extracting`, `synthesising` (with % progress), `encoding`, `published`, `failed` (with machine code + human message), `deleted`. While synthesising, the status includes an estimated completion time based on the measured real-time factor.
- <a id="fr-stat-02"></a>**FR-STAT-02** `GET /v1/items` lists items with filters (status, date) and cursor pagination.
- <a id="fr-stat-03"></a>**FR-STAT-03** Server-Sent Events stream `GET /v1/events` for live status (apps may fall back to polling). Push notifications (APNs/FCM) post-MVP.

### 5.9 Item & feed management (MGMT)
- <a id="fr-mgmt-01"></a>**FR-MGMT-01** Delete an item: removes it from the feed immediately; audio deleted from storage (or refcount decremented when content-addressed and shared).
- <a id="fr-mgmt-02"></a>**FR-MGMT-02** Bulk prune: by age ("older than 30 days"), by "listened" heuristic (enclosure fully downloaded), by status (failed), or by selection.
- <a id="fr-mgmt-03"></a>**FR-MGMT-03** Auto-prune policy per feed (max items / max age / max storage).
- <a id="fr-mgmt-04"></a>**FR-MGMT-04** Edit item metadata (title) and move between feeds.
- <a id="fr-mgmt-05"></a>**FR-MGMT-05** Retry a failed item (optionally with forced extractor).
- <a id="fr-mgmt-06"></a>**FR-MGMT-06** Feed CRUD (post-MVP multiple feeds): create, rename, rotate token, delete (cascades or reassigns items).
- <a id="fr-mgmt-07"></a>**FR-MGMT-07** Export: all items' metadata as JSON and audio as a zip/OPML-referenced list (data portability).

### 5.10 Content-addressed storage & re-use (CACHE)
- <a id="fr-cache-01"></a>**FR-CACHE-01** Pipeline stages are content-addressed: `extract_key = H(source bytes, extractor id+version)`, `audio_key = H(normalised script, engine id+version, voice, encoder settings)`. On a single instance this gives crash-safe resumption, cheap retries, and no re-synthesis when the same article is resubmitted or shared.
- <a id="fr-cache-02"></a>**FR-CACHE-02** A cache hit re-uses existing audio; a new episode record points to the shared blob (refcounted).
- <a id="fr-cache-03"></a>**FR-CACHE-03** Re-use between users on the same instance is keyed **only on content the requester already supplied/fetched** (never on URL alone), so it cannot reveal or hand out content a user did not themselves obtain. Uploaded documents are never re-used across users. See [doc 03 §6](03-architecture-and-nfrs.md#6-re-use--caching-design-per-instance).
- ⚑ Hosting impact: the same keys would allow a cross-tenant cache on a central service, which raises copyright (redistribution) and privacy (timing side-channel) issues — see [doc 02 §3](02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits) and [doc 03 §6](03-architecture-and-nfrs.md#6-re-use--caching-design-per-instance). Not built.

### 5.11 Sharing (SHARE, stretch)
- <a id="fr-share-01"></a>**FR-SHARE-01** A user can share a published item with another user by handle/email.
- <a id="fr-share-02"></a>**FR-SHARE-02** The recipient sees a pending share; on accept, the item appears in their chosen feed (audio blob shared, no recompute); on decline nothing is stored for them.
- <a id="fr-share-03"></a>**FR-SHARE-03** Recipients can block senders; senders are rate-limited; shares expire after 14 days.

## 6. Acceptance criteria (Gherkin)

Each scenario is an automated acceptance test in the E2E suite ([doc 05](05-quality-cicd-observability.md)). IDs map to FRs.

### <a id="ac-auth"></a>AC-AUTH — Authentication
```gherkin
Feature: Authenticated access

  Scenario: AC-AUTH-01 Unauthenticated request is rejected
    Given I have no access token
    When I POST a URL to /v1/items
    Then the response status is 401
    And the response includes a WWW-Authenticate: Bearer header
    And no item is created

  Scenario: AC-AUTH-02 Token from wrong issuer or audience is rejected
    Given I hold a validly signed token whose "aud" is not this server
    When I call GET /v1/items
    Then the response status is 401

  Scenario: AC-AUTH-03 Insufficient scope
    Given I hold a token with only the "items:read" scope
    When I POST a URL to /v1/items
    Then the response status is 403

  Scenario: AC-AUTH-04 Native app sign-in uses PKCE
    Given the iOS app is freshly installed
    When I tap "Sign in"
    Then the system browser session opens at the IdP authorise endpoint
    And the request includes code_challenge with method S256
    And after consent the app holds tokens in the Keychain

  Scenario: AC-AUTH-05 Users cannot see each other's items
    Given user A has item X
    When user B requests GET /v1/items/X
    Then the response status is 404
```

### <a id="ac-sub"></a>AC-SUB — Submit and publish (the core journey)
```gherkin
Feature: Turn an article into a podcast episode

  Scenario: AC-SUB-01 Happy path
    Given I am authenticated with scope "items:write"
    And "https://fixtures.test/long-article" serves a 3,000-word article
    When I POST {"url": "https://fixtures.test/long-article"} to /v1/items
    Then I receive 202 with an item id and a status URL within 300 ms
    And within 10 minutes the item status is "published"
    And my feed contains an episode titled with the article title
    And the enclosure is a valid MP3 whose duration is between 15 and 30 minutes
    And an ASR transcript of the first 60 s matches the article opening with WER < 10%

  Scenario: AC-SUB-02 Idempotent resubmission
    Given I submitted URL U and it was published as item X
    When I submit URL U again without force
    Then I receive 200 with item X
    And no new job is enqueued

  Scenario: AC-SUB-03 Non-article page
    Given the URL serves a page with fewer than 200 words of body text
    When I submit it
    Then the item fails with code "EXTRACTION_LOW_CONFIDENCE"
    And the status message tells me I can force processing or upload the document

  Scenario: AC-SUB-04 Quota exceeded
    Given I have used my daily character quota
    When I submit another URL
    Then the response status is 429 with a Retry-After header

  Scenario: AC-SUB-05 Article over the length limit
    Given the URL serves an article of 31,000 words
    When I submit it
    Then no synthesis is started
    And the item fails with code "TOO_LONG" stating 31,000 words and the 30,000-word limit

  Scenario: AC-SUB-06 Long article arrives as one episode
    Given the URL serves an article of 28,000 words
    When I submit it
    Then the feed shows nothing for it while it is processing
    And the status shows synthesis progress and an estimated completion time
    And when published the feed contains exactly one episode of roughly 3 hours with chapter markers
```

### <a id="ac-fet"></a>AC-FET — Safe fetching
```gherkin
Feature: The fetcher cannot be turned against internal networks

  Scenario Outline: AC-FET-01 Internal destinations are blocked
    When I submit "<url>"
    Then the item fails with code "DESTINATION_NOT_ALLOWED"
    And no TCP connection is attempted to the resolved address
    And a "egress_blocked" security event is logged

    Examples:
      | url                                             |
      | http://127.0.0.1/                               |
      | http://[::1]/                                   |
      | http://169.254.169.254/latest/meta-data/        |
      | http://10.0.0.5/                                |
      | http://0x7f000001/                              |
      | http://rebind.fixtures.test/ (A record flips)   |
      | http://public.test/redirect?to=http://10.0.0.1/ |
      | file:///etc/passwd                              |
      | gopher://example.com/                           |

  Scenario: AC-FET-02 Oversized response
    Given the URL streams 500 MB of HTML
    When I submit it
    Then the fetch aborts after 10 MB
    And the item fails with code "SOURCE_TOO_LARGE"

  Scenario: AC-FET-03 Decompression bomb
    Given the URL returns a 1 MB gzip body that inflates to 10 GB
    When I submit it
    Then the item fails with code "SOURCE_TOO_LARGE" without exhausting memory

  Scenario: AC-FET-04 robots.txt is respected by default
    Given the origin's robots.txt disallows "read-to-me" for the article path
    When I submit the URL
    Then no request is made for the article itself
    And the item fails with code "ROBOTS_DISALLOWED"
    And the message suggests uploading the HTML or PDF instead

  Scenario: AC-FET-05 Disrespectful mode
    Given the server runs with RTM_FETCH_IGNORE_ROBOTS=true
    And the origin's robots.txt disallows "read-to-me"
    When I submit the URL
    Then the article is fetched and processed
    And the server logged a startup warning that robots.txt is being ignored
```

### <a id="ac-ext"></a>AC-EXT — Extraction
```gherkin
Feature: Extensible extraction

  Scenario: AC-EXT-01 Site adapter beats generic
    Given a site adapter for "example-blog.test" is registered
    When I submit a URL on example-blog.test
    Then the item records extractor "site/example-blog" with its version

  Scenario: AC-EXT-02 Boilerplate removed
    Given a golden-corpus article with navigation, ads, cookie banner and comments
    When it is extracted
    Then the extracted body text has token-level F1 ≥ 0.90 against the ground truth

  Scenario: AC-EXT-03 Hostile markup is inert
    Given an article containing <script>, inline event handlers, and SSML-like tags in body text
    When it is processed
    Then no script content appears in the narration
    And SSML-like tags are spoken as literal text or removed, never interpreted
    And the feed description contains no HTML
```

### <a id="ac-feed"></a>AC-FEED — Feed
```gherkin
Feature: Private podcast feed

  Scenario: AC-FEED-01 Feed works in real podcast apps
    Given I have 3 published items
    When I add my feed URL to Apple Podcasts, Overcast, Pocket Casts and AntennaPod
    Then all 3 episodes appear with correct titles, durations and artwork
    And seeking to 50% of an episode starts playback within 3 s

  Scenario: AC-FEED-02 Feed validates
    When the feed is checked with the CI feed validator
    Then there are no errors

  Scenario: AC-FEED-03 Token rotation
    Given I rotate my feed token
    When the old feed URL is requested
    Then the response is 404 (after any grace period)
    And the new feed URL returns the feed

  Scenario: AC-FEED-04 Conditional GET
    Given the feed has not changed
    When a client sends If-None-Match with the current ETag
    Then the response is 304 with no body
```

### <a id="ac-mgmt"></a>AC-MGMT — Manage and prune
```gherkin
Feature: Feed management

  Scenario: AC-MGMT-01 Delete removes from feed
    Given item X is in my feed
    When I DELETE /v1/items/X
    Then the next feed fetch does not contain X
    And X's enclosure URL returns 404 or 410

  Scenario: AC-MGMT-02 Bulk prune by age
    Given I have 50 items, 20 older than 30 days
    When I prune items older than 30 days
    Then 20 items are deleted and 30 remain

  Scenario: AC-MGMT-03 Auto-prune keeps the feed bounded
    Given my feed auto-prune policy is "max 25 items"
    When my 26th item is published
    Then the oldest item is removed from the feed
```

### <a id="ac-app"></a>AC-APP — Client apps (post-MVP; pending the client-strategy decision, doc 07 Q3)
```gherkin
Feature: Share into the app

  Scenario: AC-APP-01 iOS share sheet
    Given I am signed in to the iOS app
    And I am reading an article in Safari
    When I tap Share → read-to-me
    Then a confirmation shows within 2 s saying the item was queued
    And the item appears in the app's queue with live status

  Scenario: AC-APP-02 Android share intent
    Given I am signed in to the Android app
    When I share a URL from Chrome to read-to-me
    Then the item is queued and a notification confirms it

  Scenario: AC-APP-03 Share a PDF file
    Given I share a PDF from Files to the app
    Then the PDF is uploaded and queued as an item

  Scenario: AC-APP-04 Offline submission
    Given my phone is offline
    When I share a URL to the app
    Then the submission is queued locally and sent when connectivity returns

  Scenario: AC-APP-05 Prune from the app
    Given I select 5 items in the app
    When I tap Delete and confirm
    Then they disappear from the list and from the feed
```

### <a id="ac-share"></a>AC-SHARE — Sharing (stretch)
```gherkin
Feature: Share an item with another user

  Scenario: AC-SHARE-01 Consent required
    Given I share item X with user B
    Then X does not appear in B's feed
    And B sees a pending share from me
    When B accepts
    Then X appears in B's chosen feed without re-synthesis

  Scenario: AC-SHARE-02 Block
    Given B has blocked me
    When I share an item with B
    Then the request appears to succeed to me but B never sees it
```

### <a id="ac-ops"></a>AC-OPS — Self-hosting
```gherkin
Feature: Self-hosting

  Scenario: AC-OPS-01 One-command install
    Given a fresh Linux host with Docker
    And an existing OIDC provider with a client registered for read-to-me
    When I set the issuer, audience and client ID in the sample .env
    And I run "docker compose up -d"
    Then within 5 minutes I can sign in and publish a first article
    And the stack runs on a 4 vCPU / 8 GB RAM machine with no GPU

  Scenario: AC-OPS-02 Upgrade
    Given version N is running with data
    When I pull version N+1 and restart
    Then migrations run automatically and all existing items remain in the feed
```

## 7. Product success criteria (MVP exit)

The MVP is "done" when:
1. All [AC-AUTH](#ac-auth), [AC-SUB](#ac-sub), [AC-FET](#ac-fet), [AC-EXT-02/03](#ac-ext), [AC-FEED](#ac-feed), [AC-MGMT-01/02](#ac-mgmt) scenarios pass in CI.
2. The owner has used it daily for 4 weeks with ≥ 80% of submissions published without manual intervention.
3. Extraction F1 ≥ 0.90 on a golden corpus of ≥ 100 articles from ≥ 30 sites.
4. Time-to-feed p95 ≤ 10 min for a 5,000-word article on the reference self-host machine (no GPU).
5. Zero open high/critical findings from the security baseline (SAST, dependency scan, ZAP baseline, threat model review).
