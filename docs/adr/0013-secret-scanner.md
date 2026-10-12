# ADR-0013: CLI secret scanner — Gitleaks or Betterleaks

- **Status:** Proposed (decision deferred to Iteration 0)
- **Date:** 2026-10-11
- **Related:** [doc 05 §1](../05-quality-cicd-observability.md#1-repository--workflow-conventions) and [§2.1](../05-quality-cicd-observability.md#21-on-every-pr-target--10-min-wall-clock), [doc 04](../04-tech-stack.md) (token prefixes), [ADR-0010](0010-licence-and-third-party-policy.md) (licence allow-list)

## Context

[Doc 05](../05-quality-cicd-observability.md) calls for a CLI secret scanner in pre-commit hooks and CI, in addition to GitHub secret scanning + push protection. The original plan named **Gitleaks**. Since then:

- **Gitleaks is feature-complete and in security-patch-only mode.** Its README states: *"Gitleaks is feature complete. I'm not merging new features into Gitleaks. Future releases will be security patches only."* and *"I'm shifting my focus to Betterleaks"*. Recent releases include 8.30.0 (Jan 2026) and 8.30.1 (Mar 2026, packaged by Arch).
- **Betterleaks** launched in March 2026. It was created by Zach Rice, Gitleaks' original author, who reportedly no longer controls the Gitleaks repository or name.
  - Licence: MIT, which is on the [ADR-0010](0010-licence-and-third-party-policy.md) allow-list (it would be a dev tool anyway and isn't shipped).
  - Sponsored by Aikido Security, described as community-governed.
  - Pure Go (no CGO/Hyperscan).
  - Rule validation is written in CEL (Common Expression Language).
  - Candidate filtering uses BPE "token efficiency" instead of Shannon entropy.
  - Reported recall on the CredData benchmark is 98.6% against 70.4% for entropy. **This is the project's own benchmark and hasn't been independently verified.**
  - Ships a `.pre-commit-hooks.yaml` and a container image at `ghcr.io/betterleaks/betterleaks`.
  - Network validation and revocation of found credentials are opt-in.
  - Users include Aikido, MegaLinter and CodeRabbit (per its README).
- **Betterleaks is mid-transition between major versions.** Its `main` branch is v2 development and v1 is maintained on `v1.x`. The README says that until the first stable v2 release, package managers and the Docker `latest` tag may still provide v1, and it points v1 users to a migration guide covering CLI, config, report and SDK changes.
  - Launch coverage described v1 as a drop-in replacement for Gitleaks flags and config.
  - The current README names the config file `betterleaks.toml` and doesn't confirm that `.gitleaks.toml` is still accepted.
  - No official GitHub Action is listed.

## Why the decision is deferred

The direction is clear: Gitleaks is frozen and active work has moved to Betterleaks. What isn't clear yet is the **specific version and integration to pin**:

1. The v2 config, CLI and report format aren't stable yet, so adopting now probably means migrating again within months.
2. `latest` image tags and package managers may resolve to v1 or v2 inconsistently, which works against reproducible CI.
3. With no official GitHub Action, integration would be the container image or a pinned binary. That's fine, but it needs designing.
4. The recall improvement is the main reason to switch and has only been measured by the vendor.

We haven't written product code yet, so there's nothing to scan. Deferring costs nothing until Iteration 0 sets up CI.

## Options

| Option | Pros | Cons |
|--------|------|------|
| **A. Gitleaks 8.30.x** | Stable, widely packaged, official Action, well known | Frozen: no new rules or detectors; risk of falling behind new token formats; uncertain long-term stewardship |
| **B. Betterleaks v1.x** | Same author; reportedly compatible with Gitleaks config; active | Already the legacy line; we'd have to migrate to v2 soon |
| **C. Betterleaks v2 (once stable)** | Where the active work is; better reported recall; CEL validation | Not stable as of this writing; config and CLI still changing |
| **D. TruffleHog / others** | Mature, verifies live credentials | AGPL-3.0 (fine as a dev tool, but heavier); different config model; no evidence it beats C |

## Leaning

**Option C**, provided the criteria below are met when Iteration 0 starts. Otherwise **Option A** as an interim measure (pinned by digest), with a dated review.

The layered design means either choice is low-risk. **GitHub secret scanning + push protection is the primary control** and stays enabled regardless; the CLI scanner is defence in depth in pre-commit hooks and in CI history scans.

## Decision criteria (to be checked at Iteration 0)

- [ ] Betterleaks v2 has a stable (non-RC) release, and its image tags are unambiguous.
- [ ] We can pin it by image digest or checksummed binary in CI and in pre-commit.
- [ ] It detects our token prefixes (`rtm_pat_…`, `rtm_feed_…`) with a custom rule. Test with planted fixtures, both positive and negative.
- [ ] On a small seeded test repo (fake AWS/GitHub/Stripe-format keys, our prefixes, and high-entropy decoys), its recall is ≥ Gitleaks' with an acceptable false-positive rate. This gives us our own evidence instead of relying on the vendor's benchmark.
- [ ] SARIF (or a comparable report format) uploads to GitHub code scanning.
- [ ] Activity and governance remain healthy: releases in the last 90 days and responsive security policy.

## Consequences (whichever option wins)

- Network validation of found credentials stays **off** in CI; we don't send candidate secrets to third parties.
- We maintain our own config with our custom rules instead of extending the upstream default, as Betterleaks' README itself advises, so upstream rule changes don't break CI unexpectedly.
- Reviewed again whenever the chosen tool's maintenance status changes.

## Sources
- Gitleaks README (maintenance notice): https://github.com/gitleaks/gitleaks
- Betterleaks README (v1/v2 status, licence, image, sponsor): https://github.com/betterleaks/betterleaks
- BleepingComputer launch coverage: https://www.bleepingcomputer.com/news/security/betterleaks-a-new-open-source-secrets-scanner-to-replace-gitleaks/
- Help Net Security coverage: https://www.helpnetsecurity.com/?p=362355
