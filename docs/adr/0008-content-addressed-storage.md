# ADR-0008: Content-addressed storage and cache-key derivation

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 01 §5.10](../01-product-spec.md#510-content-addressed-storage--re-use-cache) ([FR-CACHE-*](../01-product-spec.md#510-content-addressed-storage--re-use-cache)), [doc 03 §6](../03-architecture-and-nfrs.md#6-re-use--caching-design-per-instance), [ADR-0003](0003-sqlite-datastore-and-queue.md) (SQLite), [ADR-0006](0006-extractor-triage-contract.md), [ADR-0007](0007-tts-engine-rust-kokoro.md)

## Context

Each item produces several large intermediates: source bytes, the `Article`, synthesised audio segments and the final MP3. On a single instance we want:
- crash-safe resumption, so a crash 2 hours into a 3-hour synthesis doesn't restart from zero;
- idempotent pipeline stages;
- no re-synthesis when the same article is resubmitted, retried or shared within the household;
- privacy between users on the same instance.

## Decision

### Blob store
- Every intermediate and output is an **immutable blob** in the object store (`object_store` crate; local filesystem by default, S3-compatible optional), addressed by its key.
- **Layout:** `blobs/<kind>/<first 2 hex>/<key>`, where `kind` ∈ `source | article | segment | episode | transcript | chapters`.
- **Metadata** lives in SQLite: a `blobs(key, kind, size, content_type, refcount, created_at)` table.
- **Writes are atomic:** write to a temp path, fsync, then rename. A blob that already exists is never rewritten.

### Key derivation
All keys are **BLAKE3** hashes, rendered as hex, over a canonical, length-prefixed encoding of their inputs:

| Kind | Key inputs |
|------|-----------|
| `source` | raw source bytes ‖ *owner salt* |
| `article` | source key ‖ final URL ‖ source kind ‖ declared charset ‖ `extractor_id@version` ‖ `article_schema_version` ([Amendment 1](#amendment-1-2026-10-16-article-keys-cover-every-extraction-input)) |
| `segment` (TTS piece) | normalised segment text ‖ language ‖ `engine_id@version` ‖ model hash ‖ voice id + voice-file hash ‖ synthesis params |
| `episode` (MP3) | ordered segment keys ‖ pause plan ‖ `encoder@version` ‖ encoder params ‖ chapter plan |
| `transcript`, `chapters` | episode key ‖ format version |

- **Owner salt:**
  - for URL-fetched public content, it's empty. Two household members who fetch the same article share the downstream blobs ([doc 03 §6](../03-architecture-and-nfrs.md#6-re-use--caching-design-per-instance));
  - for uploaded documents (post-MVP), it's a per-user secret, so uploads are never shared or detectable across users.
- **Version inputs:** any change to an extractor, the model, a voice, the normaliser or the encoder changes the version input and therefore the keys. Stale results are never reused by accident.

### Lifecycle
- **References:** `items` reference `episode`, `article` and `source` keys. Shares (stretch) add references to the same episode.
- **Refcounts:** maintained in the same transaction that adds or removes item references.
- **Garbage collection:** a scheduled task deletes blobs with `refcount = 0` that are older than a grace period (default 24 h, so in-flight work isn't collected). A weekly mark-and-sweep reconciles the store with the database, finding orphans after crashes.
- **Segments** are referenced by the in-progress item until encoding completes, then released (they're kept only for the grace period, which covers retries).
- **Sources** are released after extraction unless the user opted to keep them for "report a problem" ([FR-EXT-07](../01-product-spec.md#fr-ext-07)).

### Exposure
- Keys never appear in public URLs. Enclosures are served under the feed token and item ID ([ADR-0009](0009-audio-format-and-feed.md)). A key isn't a capability.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Store per item, by ID, with no content addressing | Loses crash resumption at segment level, idempotency and household reuse. |
| SHA-256 instead of BLAKE3 | Fine, but slower. BLAKE3 is fast on large audio and has a mature Rust crate; no compliance need for SHA-2. |
| Blobs in SQLite | Bloats the database and its backups; audio is large and immutable, so a filesystem or object store is the right home. |
| URL-keyed cache | Leaks across users (a cache hit reveals what others read) and breaks when the content behind a URL changes. |

## Consequences

- Crash resumption, retries and re-voicing are cheap. Sharing an item costs no synthesis.
- Storage is reclaimed by GC rather than immediately on delete. The item disappears from the feed at once ([FR-MGMT-01](../01-product-spec.md#fr-mgmt-01)), and the audio goes within the grace period ([NFR-SEC6](../03-architecture-and-nfrs.md#nfr-sec6)'s 24 h).
- Backups: the database is small and backed up by [ADR-0003](0003-sqlite-datastore-and-queue.md); blobs are regenerable, so backing them up is optional.
- ⚑ Hosting impact: the same keys would allow a cross-tenant cache, with the copyright and privacy issues in [doc 02 §3](../02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits). Not built.

## Amendment 1 (2026-10-16): article keys cover every extraction input

The original `article` key, source key ‖ extractor ‖ schema version, assumed extraction reads only the source bytes. It doesn't. It also reads:
- **the final URL**, from which a missing title is derived and the canonical URL taken;
- **the source kind**, which chooses the extractor family;
- **the declared charset**, which changes how the bytes decode.

So identical bytes fetched from different URLs, or served with different charsets, produced different articles under one key. The blob store never rewrites, so the second item recorded its own extraction while pointing at the first item's article ([PR #36](https://github.com/huwd/read-to-me-server/pull/36), found in review).

**Decision:** the `article` key covers everything extraction reads: source key ‖ final URL ‖ source kind ‖ declared charset (with a presence tag, so absent and empty differ) ‖ `extractor_id@version` ‖ `article_schema_version`. The table above is updated.

**Rule for every key in this ADR:** a key must cover every input the producing stage reads, not just the obvious one. Later stages (segments, episodes) should be checked against this rule when they're built.

**Consequences:** sharing between household members still works for the common case, the same URL. Identical bytes reached through different URLs no longer share an article. That's the correct trade, because the articles can genuinely differ.
