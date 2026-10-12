# ADR-0009: Audio format, enclosure serving and feed namespaces

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 01 §5.6](../01-product-spec.md#56-encoding--storage-enc) ([FR-ENC-*](../01-product-spec.md#56-encoding--storage-enc)), §5.7 ([FR-FEED-*](../01-product-spec.md#57-feed-feed)); [NFR-C2](../03-architecture-and-nfrs.md#nfr-c2); [ADR-0004](0004-auth-oidc-resource-server.md) (feed tokens), [ADR-0007](0007-tts-engine-rust-kokoro.md) (TTS), [ADR-0008](0008-content-addressed-storage.md) (CAS)

## Context

Episodes must play in every mainstream podcast app (Apple Podcasts, Overcast, Pocket Casts, AntennaPod, Castro, Podcast Addict), seek instantly, show chapters, and stay private. Items can run up to 30,000 words (≈ 3 h 15 min, [FR-TTS-06](../01-product-spec.md#fr-tts-06)). Kokoro outputs 24 kHz mono PCM.

## Decision

### Audio
- **MP3, MPEG-1 Layer III, CBR 64 kbps, mono, 44.1 kHz**, encoded by `ffmpeg` with libmp3lame (LGPL build, [ADR-0010](0010-licence-and-third-party-policy.md)).
  - **Why resample from 24 kHz:** MP3 at 24 kHz is MPEG-2 "LSF", which some players and hardware handle poorly. 44.1 kHz MPEG-1 is universally supported.
  - **Why CBR:** constant bit rate makes seeking accurate and byte offsets predictable for Range requests. VBR seeking is unreliable in some apps.
  - **Size:** 64 kbps ≈ 28.8 MB per hour, so a 30,000-word item is ≈ 94 MB.
- **Loudness:** two-pass `loudnorm` to **−16 LUFS integrated, −1.0 dBTP true peak** (the podcast norm).
- **Tags:** ID3v2.3 (broadest compatibility):
  - title, artist (byline or site), album (feed title), date;
  - comment (source URL);
  - **chapters** (`CHAP`/`CTOC` frames) from headings, with timings taken from synthesis segment durations.
- **Episode artwork:** none embedded in the MVP; feed-level artwork applies ([FR-FEED-08](../01-product-spec.md#fr-feed-08)).

### Enclosure serving
- Served **by the server**, not by redirects to storage: `GET /feeds/<feed_token>/items/<item_id>.mp3`.
- Supports `Range` (single range; `206`/`416`), `ETag` (the episode key), `Last-Modified` and `Accept-Ranges: bytes`. Also sets `Content-Type: audio/mpeg`, `Cache-Control: private, max-age=86400`, `X-Robots-Tag: noindex`.
- **Why not signed object-store URLs:**
  - podcast apps keep enclosure URLs for months, so expiring URLs break downloads;
  - serving ourselves keeps one auth model (the feed token);
  - it gives listen-through metrics from the byte ranges served ([doc 05 §4.4](../05-quality-cicd-observability.md#44-product-success-metrics));
  - it's cheap at household scale.
- Deleted items return `410 Gone`.

### Feed
- **RSS 2.0** with the `itunes:` (Apple) and `podcast:` (Podcasting 2.0) namespaces, written by our typed `quick-xml` writer (all text escaped; descriptions plain text only).
- **Channel:**
  - title, description, `language` (BCP 47), `itunes:image` (bundled default, [FR-FEED-08](../01-product-spec.md#fr-feed-08)), `itunes:author`;
  - `itunes:explicit` false;
  - privacy markers: `itunes:block` yes, `podcast:locked` yes, `podcast:guid` (a stable UUID per feed).
- **Item:**
  - `title`; `guid isPermaLink="false"` (the item ID); `pubDate` (publication time in the feed);
  - `enclosure` (URL, length, type);
  - `itunes:duration` (seconds), `itunes:episodeType` full;
  - `description`: plain-text byline, site, source URL and word count;
  - `link`: the source URL.
- **Transcripts and chapters** (`podcast:transcript`, `podcast:chapters`) are served under the same feed token:
  - transcript as **WebVTT**, with sentence timings from synthesis segments, which are known at no extra cost; plain text as a fallback;
  - chapters as **JSON** (Podcasting 2.0 chapters format), matching the ID3 chapters.
- **HTTP:** `ETag` and `Last-Modified` on the feed with `304` support; `Cache-Control: private, max-age=300`; `X-Robots-Tag: noindex, nofollow`.
- **Ordering and size:** newest first, at most N items (default 100, [FR-FEED-07](../01-product-spec.md#fr-feed-07)).

### Validation
- CI validates the feed XML against our structural rules and a third-party podcast feed validator (nightly).
- ffprobe checks every encoded file in E2E: codec, rate, CBR, duration within tolerance, and that chapters are present.
- A manual app-compatibility checklist runs per release ([NFR-C2](../03-architecture-and-nfrs.md#nfr-c2)).

## Alternatives considered

| Option | Why not |
|--------|---------|
| AAC/M4A | Slightly better quality per bit and well supported, but chapter support in M4A varies more across apps, and MP3 is the safest common denominator. Could be offered later as an option. |
| Opus | Excellent quality at low bitrate, but not supported by Apple Podcasts enclosures. |
| Keep 24 kHz | Saves a resample step but risks the MPEG-2 LSF compatibility issues above. |
| VBR MP3 | Smaller files, but less reliable seeking. |
| ID3v2.4 | Better text encoding, but v2.3 has the widest player support; chapters work in both. |

## Consequences

- Maximum compatibility, with predictable sizes (~29 MB/h).
- Chapters and timed transcripts come almost free from synthesis timing, an accessibility win ([NFR-A2](../03-architecture-and-nfrs.md#nfr-a2)).
- The server handles all audio egress; fine at household scale. ⚑ Hosting impact: a public service would move enclosures to a CDN with long-lived signed URLs or token-checking edge logic.
