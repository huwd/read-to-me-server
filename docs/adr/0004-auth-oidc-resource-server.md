# ADR-0004: Authentication: OAuth 2.1 / OIDC resource server with generic provider config; PATs; feed capability tokens

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 01 §5.1](../01-product-spec.md#51-authentication--authorisation-auth) ([FR-AUTH-*](../01-product-spec.md#51-authentication--authorisation-auth)), §5.7 ([FR-FEED-05](../01-product-spec.md#fr-feed-05)); [doc 06 §3.4](../06-security-and-pentest.md#34-authentication--authorisation); [ADR-0002](0002-rust-modular-monolith.md)

## Context

The brief requires quality authentication with OAuth from day one. Three kinds of caller reach the server, with different capabilities:
1. **Interactive clients** (a future web UI, native apps or wrappers): can do OAuth 2.1 Authorization Code + PKCE.
2. **Automation** (iOS Shortcuts, Android share shortcuts, CLI, bookmarklet): can send a static header but can't run an interactive OAuth flow.
3. **Podcast apps**: can only fetch a URL; no OAuth, and Basic auth support is patchy.

The self-hoster brings their own OIDC provider. The server must not bundle, assume or special-case one.

## Decision

### 1. OAuth/OIDC: the server is a resource server only
- The server **validates access tokens**; it never issues them, runs login pages or stores passwords. Clients obtain tokens directly from the user's provider (Auth Code + PKCE S256, system browser per RFC 8252 for apps).
- **Generic configuration:**
  - `RTM_OIDC_ISSUER`: the issuer URL; everything else comes from `/.well-known/openid-configuration`;
  - `RTM_OIDC_AUDIENCE`: the expected `aud`;
  - `RTM_OIDC_CLIENT_IDS`: allowed `azp`/`client_id` values;
  - `RTM_OIDC_SCOPE_CLAIM`: default `scope`, configurable for providers that use `scp`.
- **Token formats:**
  - **JWT access tokens** (RFC 9068) are validated locally. Signature checked against JWKS (cached; refreshed on unknown `kid`, rate-limited). Algorithm allow-list `RS256, ES256, EdDSA`; never `none` or HS*. Also checked: `iss`, `aud`, `exp`, `nbf` (≤ 60 s skew) and an allowed `azp`/`client_id`.
  - **Opaque access tokens**: optional token introspection (RFC 7662) when `RTM_OIDC_INTROSPECTION=true`, with results cached until expiry (max 5 min). This keeps the server provider-agnostic, since some providers issue only opaque tokens.
- **Scopes:** `items:read`, `items:write`, `feeds:read`, `feeds:admin`, `admin`. Each endpoint declares the scopes it requires and the middleware enforces them.
- **User mapping:** the local user key is the pair (`iss`, `sub`), never the email. The email or name claims are display-only.

### 2. Who may use the instance (FR-AUTH-07)
- No open sign-up. A token for an unknown (`iss`, `sub`) gets `403` unless it's allowed by:
  - `RTM_ADMIN_SUB` (the bootstrap admin), or
  - an admin-managed allow-list of `sub`s, or a configured group/role claim value (`RTM_OIDC_ALLOWED_GROUP`).
- The first-user race is closed: if `RTM_ADMIN_SUB` isn't set, the server refuses to start in multi-user mode. Single-user mode needs either that variable or an explicit one-time claim code printed to the log.

### 3. Personal access tokens (automation)
- Created by an authenticated user (OAuth), scoped and expiring (default 90 days, max 1 year), with a human name. Shown once.
- **Format:** `rtm_pat_` + 43 base62 characters (256 bits from the OS RNG). The prefix makes leaks detectable by secret scanners ([ADR-0013](0013-secret-scanner.md)).
- Stored as `HMAC-SHA256(server_secret, token)`; compared in constant time; `last_used_at` recorded; revocable individually or all at once.
- Accepted as `Authorization: Bearer rtm_pat_…`. The server tells PATs from OAuth tokens by the prefix.

### 4. Feed capability tokens (podcast apps)
- Each feed has a token in its URL path: `/feeds/rtm_feed_<43 base62>.xml`, and enclosures live under the same token.
- 256-bit; stored hashed as for PATs; rotatable with an optional grace period (default 0, max 7 days).
- Unknown tokens return `404` (not `401`), with uniform timing. Requests are rate-limited per client IP.
- Feed tokens grant read-only access to that feed's XML and audio, nothing else.

### 5. Out of scope here
- Web UI session handling (cookie/BFF) is decided with the client strategy ([ADR-0012](README.md)).
- DPoP sender-constrained tokens are a later hardening, not in the MVP.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Bundle an IdP (e.g. Pocket ID) | Owner decision: assume a provider exists; no provider-specific code or containers. |
| Built-in username/password | Not OAuth, and we'd own password storage, MFA and recovery. |
| Basic auth on feeds | Patchy podcast-app support; credentials end up in app sync clouds anyway. |
| Signed, expiring feed URLs | Podcast apps poll the same URL for months; expiry breaks subscriptions. |
| JWT-only (no introspection) | Excludes providers that issue opaque tokens; introspection is small and optional. |

## Consequences

- Works with any compliant OIDC provider through configuration alone. CI tests against a mock IdP covering JWT, opaque/introspection, key rotation and malformed tokens.
- Three credential types with different blast radii. The security doc's ATT&CK tests ([T1528](../06-security-and-pentest.md#t1528), [T1550.001](../06-security-and-pentest.md#t1550-001), [T1606](../06-security-and-pentest.md#t1606)) cover each.
- Feed tokens are bearer secrets that podcast apps will sync to their vendors' clouds. This is documented to users, and mitigated by rotation and `itunes:block`/`podcast:locked` ([ADR-0009](0009-audio-format-and-feed.md)).
- ⚑ Hosting impact: a public service would add open sign-up, social login, and a managed IdP. These are config changes plus a sign-up flow; no change to token validation.
