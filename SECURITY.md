# Security policy

read-to-me fetches URLs chosen by its users and parses content from the open web, so we take reports seriously. The threat model and planned controls are in [doc 06](docs/06-security-and-pentest.md).

## Reporting a vulnerability

**Please don't open a public issue.** Report privately through GitHub's [private vulnerability reporting](https://github.com/huwd/read-to-me-server/security/advisories/new) (*Security → Report a vulnerability*).

Please include:
- what an attacker can do, and under what conditions;
- steps or a proof of concept to reproduce it;
- the version or commit, and how the instance is deployed (e.g. docker compose, reverse proxy).

## What to expect

This is a small, volunteer-run project, so these are goals rather than guarantees:
- acknowledgement within **5 working days**;
- an initial assessment within **14 days**;
- a fix or mitigation for confirmed high or critical issues as soon as practical, coordinated with you.

Fixes are published as GitHub Security Advisories, with a CVE where warranted. Reporters are credited unless they'd rather not be.

## Scope

**In scope:**
- the server and its compose bundle;
- the TTS process;
- the release images;
- the project's own tooling and CI configuration.

**Particularly interesting:**
- **SSRF and egress-guard bypasses** ([ADR-0005](docs/adr/0005-egress-guard.md));
- **escapes from the parsing sandbox** ([ADR-0006](docs/adr/0006-extractor-triage-contract.md));
- **authentication and token handling** ([ADR-0004](docs/adr/0004-auth-oidc-resource-server.md)), including feed tokens;
- **access to another user's items or feeds**;
- **injection into feeds** consumed by podcast apps.

**Out of scope:**
- vulnerabilities in your OIDC provider, reverse proxy or host;
- denial of service that needs an authenticated user to exceed the quotas they've been given;
- findings that need a modified build or physical access to the host.

## Supported versions

There is no release yet. Once there is, the latest minor release receives security fixes.
