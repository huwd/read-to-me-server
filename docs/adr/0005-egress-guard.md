# ADR-0005: Egress guard (resolve → validate → pin) for all outbound fetches

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 01 §5.3](../01-product-spec.md#53-fetching-fet) ([FR-FET-*](../01-product-spec.md#53-fetching-fet)), [doc 06 §3.1](../06-security-and-pentest.md#31-egress--ssrf-highest-priority) and [§4.2](../06-security-and-pentest.md#42-key-mitre-attck-techniques-to-test) ([T1190](../06-security-and-pentest.md#t1190), [T1552.005](../06-security-and-pentest.md#t1552-005), [T1090](../06-security-and-pentest.md#t1090)), [ADR-0002](0002-rust-modular-monolith.md)

## Context

The server's core feature is fetching a URL the user chooses. On a self-hosted instance it sits inside a home network next to routers, NAS boxes and admin panels; on a VPS it sits next to the cloud metadata service. Server-side request forgery (SSRF) is the top risk in the threat model.

Common defences fail in known ways:
- **Validating the hostname string** misses DNS names that resolve to private addresses, and alternative IP encodings (`0x7f000001`, `017700000001`, IPv4-mapped IPv6).
- **Resolve-then-connect** leaves a gap for DNS rebinding.
- **Letting the HTTP client follow redirects** bypasses the checks.

## Decision

All outbound HTTP (articles, robots.txt, and any future images or favicons) goes through **one** component, `SafeFetcher` in `rtm-fetch`. The rest of the codebase has no other way to make outbound requests. A Semgrep rule and a `clippy::disallowed_methods` config ban `reqwest::get` and `Client::new` outside `rtm-fetch`.

### Algorithm (per hop)
1. **Parse and normalise** the URL with the `url` crate (WHATWG). Accept only `http` and `https`; reject userinfo; length ≤ 2,048; ports 80/443 only, unless allowed in config.
2. **Resolve** the host ourselves (`hickory-resolver`) and get **all** A/AAAA records.
3. **Validate every address** against the deny-list:
   - IANA special-purpose IPv4/IPv6 registries: loopback, RFC 1918, CGNAT, link-local (incl. `169.254.169.254`, `fd00:ec2::254`), ULA, multicast, documentation, benchmarking, reserved;
   - IPv4-mapped/translated IPv6 (`::ffff:0:0/96`, `64:ff9b::/96`), decoded and the embedded IPv4 checked.

   If **any** record is denied, the fetch is rejected (no "try the next one"). The config can add extra deny CIDRs, and can add allow CIDRs only behind an explicitly named "unsafe" option.
4. **Connect to the vetted IP** via a custom `reqwest` resolver/connector that returns exactly the validated addresses. TLS SNI and certificate validation still use the hostname.
5. **Redirects are manual:** at most 5, and each `Location` re-enters the algorithm at step 1.
6. **Limits:**
   - connect 5 s, total 30 s;
   - body 10 MB for HTML (PDF limits come with upload);
   - streaming decompression with a ratio cap of 10× and an absolute cap (the ratio cap is dropped by [Amendment 1](#amendment-1-2026-10-15-no-separate-decompression-ratio-limit));
   - header size cap;
   - `Content-Type` allow-list, reconciled with sniffing.
7. **No ambient credentials:** no cookies, no auth headers, no proxy credentials, honest User-Agent (`read-to-me/<version> (+<project URL>)`).
8. **robots.txt** ([FR-FET-07](../01-product-spec.md#fr-fet-07)/[08](../01-product-spec.md#fr-fet-08)) is fetched through the same path and cached per origin.

### Defence in depth
- **Uniform failures:** the user sees `DESTINATION_NOT_ALLOWED` / `FETCH_FAILED` without internal detail. Fetched bodies are never echoed back through the API, so the server is not an open proxy ([T1090](../06-security-and-pentest.md#t1090)).
- **Every block** is logged as a security event and counted by `rtm_egress_blocked_total{reason}`.
- **Network layer (optional, documented):** a compose profile can put the server on a network whose firewall only permits public egress. On cloud VPSes, use IMDSv2 with hop limit 1 or block metadata at the host.

### Testing
- Property tests over IP encodings and CIDR edges.
- Integration tests with a stub DNS server for rebinding (TTL 0, record flips) and redirect chains.
- Fuzzing of URL parsing and validation (nightly).
- The acceptance scenarios [AC-FET-01](../01-product-spec.md#ac-fet)..05.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Hostname/regex validation only | Bypassed by DNS and IP-encoding tricks. |
| Rely on `IpAddr::is_global()` | Historically unstable, and doesn't cover every special-purpose range we care about. |
| Outbound forward proxy (e.g. Smokescreen) as a separate container | Strong, but adds a container and config for self-hosters. The in-process guard does the same job; a proxy can be added later as an optional extra layer. |
| Headless browser fetch | Huge attack surface and resource cost; not in scope ([FR-FET-06](../01-product-spec.md#fr-fet-06)). |

## Consequences

- One small, heavily tested choke point for all outbound traffic.
- Some legitimate sites on non-standard ports, or reachable only over private networks (e.g. an intranet), are blocked by default; an explicit unsafe allow-list exists for self-hosters who need it.
- Every future feature that fetches anything must use `SafeFetcher`; enforced by lint rules.

## Amendment 1 (2026-10-15): no separate decompression ratio limit

Implementing the guard ([PR #31](https://github.com/huwd/read-to-me-server/pull/31)) showed the 10× ratio cap in step 6 costs more than it protects.

**Decision:** the body limit (10 MB for HTML) is measured on the **decompressed** bytes as they stream in, and there is no separate ratio limit. [FR-FET-03](../01-product-spec.md#fr-fet-03) and [doc 06 §3.2](../06-security-and-pentest.md#32-parsing--processing-untrusted-content) are updated to match.

**Why:**
- **The absolute limit already stops bombs.** Decompression stops at 10 MB, so a bomb costs milliseconds and at most 10 MB of memory per fetch.
- **10× would reject real articles.** Ordinary HTML compresses 5–10× with gzip and more with brotli, and pages that embed large JSON (framework state, inline SVG) regularly exceed 10×. A ratio safe for real pages (50–100×) adds almost nothing on top of a 10 MB limit.
- **It would complicate the guard.** The HTTP client decompresses transparently and never sees compressed byte counts. Counting them means running the gzip, brotli and deflate decoders ourselves, inside the most security-sensitive component.

**Consequences:**
- The compression ratio is still worth observing where it can be measured cheaply: from the declared `Content-Length` (compressed bytes) against the decompressed size. A histogram and a security event for extreme ratios (over 100×) are added with the metrics work ([#26](https://github.com/huwd/read-to-me-server/issues/26)); they record, they don't fail fetches.
- **Revisit when the limit rises.** PDF upload ([FR-SUB-04](../01-product-spec.md#fr-sub-04)) brings a 50 MB limit. That is the point at which a ratio limit would bound expansion meaningfully, so reconsider it then.
