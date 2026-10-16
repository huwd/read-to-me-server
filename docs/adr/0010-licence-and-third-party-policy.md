# ADR-0010: Project licence (AGPL-3.0) and third-party licence policy

- **Status:** Accepted (licence confirmed by the owner 2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 02 §3](../02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits) (legal exposure), [doc 04 §3](../04-tech-stack.md#3-tts-process-rust--see-adr-0007) (TTS service), [doc 07 Q1](../07-roadmap-and-open-questions.md#q1), [ADR-0007](0007-tts-engine-rust-kokoro.md) (default TTS engine)

## Context

read-to-me needs an outbound licence, and a policy for the licences it builds on (inbound licences), before any code is written. Once released, the outbound licence is hard to tighten.

Forces:
- **A hosted service is a possibility** ([doc 07 Q2](../07-roadmap-and-open-questions.md#q2)). A permissive licence would let a third party run a closed hosted fork without giving anything back.
- **The self-hosted community** is the primary audience. It expects an OSI-approved open-source licence and is wary of "source-available" licences (BSL, SSPL, FSL) after past relicensing controversies.
- **The dependency stack is mostly permissive** (MIT/Apache-2.0 Rust crates; Kokoro-82M weights and code under Apache-2.0). It also includes **GPL-3.0-or-later espeak-ng** (phonemiser fallback) and **LGPL ffmpeg**.
- **Model weights are licensed separately from code.** Several capable TTS models ship weights under non-commercial or research-only terms, which no project licence can fix.
- **The App Store's terms conflict with GPL-family licences**, which matters for the native iOS app.

Options considered for the server: MIT/Apache-2.0, MPL-2.0, GPL-3.0, AGPL-3.0, and source-available licences (BUSL-1.1, FSL). They are compared in the discussion that led to [doc 07 Q1](../07-roadmap-and-open-questions.md#q1).

## Decision

### 1. Outbound licences

| Component | Licence | SPDX |
|-----------|---------|------|
| Server (all crates in the Cargo workspace), TTS service wrapper, compose bundle, deployment configs | **GNU Affero General Public License v3.0 or later** | `AGPL-3.0-or-later` |
| Native apps (Swift, Kotlin), generated API client SDKs, OpenAPI spec | Apache License 2.0 | `Apache-2.0` |
| Documentation (`docs/`, mdBook) | Creative Commons Attribution 4.0 | `CC-BY-4.0` |

- Every source file carries an SPDX header (`// SPDX-License-Identifier: AGPL-3.0-or-later`), checked in CI with `reuse lint`.
- Every Rust crate sets `license = "AGPL-3.0-or-later"` in `Cargo.toml`.
- We chose **"or later"** over "only" to stay compatible with future AGPL versions and with GPL-3.0-or-later dependencies. If that's ever a concern, it can be revisited while the copyright holder controls all the code.
- The apps use Apache-2.0. They communicate with the server only over the HTTP API and are not combined with server code, so this avoids the App Store/GPL conflict.

### 2. AGPL obligations we accept for our own deployments
- Any network-facing deployment of a **modified** server offers users the corresponding source. The server exposes `GET /source`, which returns a link to the exact commit and tag it was built from, and the web UI and app "About" screens link to it.
- The copyright holder isn't bound by their own licence. However, once external contributions arrive under AGPL, our hosted service is effectively bound too, unless a contributor agreement says otherwise (see [follow-up F1](#f1)).

### 3. Inbound licence policy: code linked or compiled into our artefacts

Enforced by `cargo-deny` ([`deny.toml`](../../deny.toml) at the repo root) for Rust. For the Python TTS image, the same list is enforced over its SBOM. **Anything not on the allow-list fails the build.** Adding a licence needs an amendment to this ADR, or a per-crate `[[licenses.exceptions]]` entry with a justification comment.

**Allowed (SPDX):**

| Category | Licences |
|----------|----------|
| Permissive | `MIT`, `MIT-0`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `0BSD`, `BSL-1.0` (Boost, not Business Source), `Unicode-3.0`, `Unicode-DFS-2016`, `CC0-1.0`, `CDLA-Permissive-2.0` |
| Weak copyleft | `MPL-2.0`, `LGPL-2.1-or-later`, `LGPL-3.0-only`, `LGPL-3.0-or-later` |
| Strong / network copyleft (v3 family) | `GPL-3.0-only`, `GPL-3.0-or-later`, `AGPL-3.0-only`, `AGPL-3.0-or-later` |
| Python image only | `PSF-2.0` (CPython ecosystem) |

**Denied (not on the list; named here for clarity):**

| Licence | Reason |
|---------|--------|
| `GPL-2.0-only`, `LGPL-2.0-only` | Incompatible with the v3 family |
| `BUSL-1.1`, `SSPL-1.0`, `Elastic-2.0`, `FSL-*`, Commons Clause | Use restrictions are incompatible with AGPL |
| `CC-BY-NC-*`, `CC-BY-NC-SA-*`, `CC-BY-ND-*`, any "non-commercial" or "research only" terms | Field-of-use restrictions; incompatible with AGPL |
| No licence / unknown | Copyright defaults to all rights reserved |

**Mixed licence expressions** (e.g. `MIT OR Apache-2.0`) pass if any allowed alternative satisfies the expression.

### 4. Separately-distributed programs (bundled in images, not linked)

Programs that run as separate processes and are shipped together in our container images, or alongside them in the compose bundle, keep their own licences. Bundling them is "mere aggregation" (GPL-3.0 §5): it doesn't make them part of our combined work. Policy:

- **ffmpeg:** LGPL build only. No `--enable-gpl` (unnecessary) and **never `--enable-nonfree`**, since nonfree builds can't be redistributed. Invoked as a subprocess.
- **espeak-ng:** GPL-3.0-or-later. It's compatible with AGPL, so it may be linked or run as a subprocess. Ship its licence and a source link to the pinned version.
- **PostgreSQL, Caddy, Grafana LGTM:** separate services under their own licences (verify each at the pinned version when adding it to compose). Referenced from compose as upstream images; not modified by us.
- **Avoid poppler** (GPL-2.0-or-later). It's not prohibited, but PDFium (BSD-3-Clause/Apache-2.0) is preferred so that the server's licensing stays flexible.

Every image ships `/licenses/THIRD_PARTY_LICENSES` (generated from the SPDX SBOM by `syft` + a script in CI). For each GPL/LGPL component, it includes a link to the exact corresponding source.

### 5. Model weights policy

Weights are licensed separately from code, and the allow-list above doesn't cover them. Each `SpeechEngine` model entry in the engine registry declares:

```toml
[[models]]
id = "kokoro-82m"
version = "1.0"
weights_licence = "Apache-2.0"
commercial_use = true
bundled = true
sha256 = "…"              # pinned; safetensors or ONNX only, never pickle
source = "https://huggingface.co/hexgrad/Kokoro-82M"
```

| Tier | Allowed licences | Rule |
|------|------------------|------|
| **Bundled / default** (shipped in images or fetched automatically) | `Apache-2.0`, `MIT`, `BSD-*`, `CC0-1.0`, `CC-BY-4.0` | Must be OSI-compatible or similarly permissive with no field-of-use restriction |
| **Optional, user-installed** (the self-hoster downloads it and points config at it) | Any, including RAIL/OpenRAIL, CC-BY-NC, research-only | Never bundled or auto-downloaded. The server logs the licence on startup, and the UI shows it when the voice is selected. |
| **Hosted service** | Bundled tier only | The hosted build refuses to start with any model where `commercial_use = false`. Use-restricted (RAIL) licences need explicit legal sign-off. |

The same rules apply to voice packs, G2P dictionaries and lexicons.

### 6. Generated output
The licences of the software and the models don't restrict the generated audio, since Kokoro's Apache-2.0 places no conditions on outputs. Rights in each episode follow the **source article's** copyright, which this ADR doesn't cover; see [doc 02 §3](../02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits) and the future content/takedown policy.

## Consequences

**Positive**
- Genuinely open source (OSI-approved); acceptable to the self-hosted community.
- Discourages closed hosted forks: anyone hosting a modified version must publish their changes.
- Compatible with the whole planned dependency set, including GPL-3.0 espeak-ng.
- Licence violations are caught at build time, not in an audit.
- Apps and SDKs stay App Store-friendly and easy to embed.

**Negative**
- Some organisations ban AGPL internally, which narrows corporate adoption and contributors.
- AGPL source-offer obligations apply to our own hosted service once we accept external contributions without a CLA.
- Copyleft is one-way: we can't later include AGPL server code in the Apache-licensed apps.
- The allow-list will occasionally block a useful crate and need an exception.

**Neutral**
- We can relax to a more permissive licence later, which is easy while one copyright holder controls everything and needs contributor consent afterwards. Tightening is effectively impossible for code already released.

## Amendment 1 (2026-10-11): hosting is not a target

[Doc 07 Q2](../07-roadmap-and-open-questions.md#q2) is now decided: a hosted service is **not pursued**. The decision above stands. AGPL-3.0 still serves the self-hosted project: it stops *others* from running a closed, modified hosted fork, and it stays compatible with espeak-ng. Effects on this ADR:

- **Context, first bullet:** read "a hosted service is a possibility" as "a hosted service by a third party is a possibility". We aren't planning one.
- **[§2](#2-agpl-obligations-we-accept-for-our-own-deployments) and the "own hosted service" consequences:** ⚑ Hosting impact only. They apply to anyone (including us) who runs a modified instance for other people. The `/source` endpoint ([F5](#f5)) is still built, because self-hosters who modify the code and share an instance with their household must offer the source too.
- **[§5](#5-model-weights-policy) "Hosted service" tier:** ⚑ Hosting impact only. The registry fields (`weights_licence`, `commercial_use`) are still recorded so self-hosters can see the licence of each voice. The hosted-build startup check ([F6](#f6)) is not built.
- **F1 (CLA vs DCO):** the main reason for a CLA was to keep commercial licensing open for our own hosted service. With hosting dropped, **DCO was chosen** ([see F1](#f1)).
- **F7 (legal review):** only needed if hosting is ever revisited.

## Amendment 2 (2026-10-11): no Python image; SQLite

- [ADR-0007](0007-tts-engine-rust-kokoro.md) moves TTS to Rust, so **no Python image ships**. The [§3](#3-inbound-licence-policy-code-linked-or-compiled-into-our-artefacts) Python-image clause and the `PSF-2.0` row only apply to throwaway tools, which aren't distributed and aren't gated. If a non-Rust image is ever shipped again, the allow-list applies to its SBOM.
- espeak-ng now sits inside the Rust TTS process (FFI or subprocess), which is fine because GPL-3.0-or-later is compatible with AGPL ([§4](#4-separately-distributed-programs-bundled-in-images-not-linked)).
- ONNX Runtime (MIT) is a native dependency of the TTS image and is listed in `THIRD_PARTY_LICENSES`.
- [ADR-0003](0003-sqlite-datastore-and-queue.md) replaces PostgreSQL with SQLite, bundled through `libsqlite3-sys` (MIT). SQLite itself is public domain, which is compatible. PostgreSQL drops out of [§4](#4-separately-distributed-programs-bundled-in-images-not-linked).

## Amendment 3 (2026-10-16): WTFPL for `language-tokenizer` only

The Kokoro engine's pronunciation library, our `misaki-rs` fork ([ADR-0014](0014-kokoro-dependencies-and-build.md)), depends on `language-tokenizer` 0.1.0, a small text tokenizer licensed under the **WTFPL v2**, which the allow-list rejects.

**About the licence:**
- It grants everything (use, modify, redistribute, relicense) with no conditions, not even attribution.
- The FSF lists it as a free licence **compatible with the GPL family**, so it imposes nothing on our AGPL-3.0-or-later distribution.
- It is **not OSI-approved**, and it has **no warranty disclaimer**. That's why cautious policies avoid it: its legal effect is untested, and an OSI-only redistribution policy would flag it.

**Decision:**
- Allow WTFPL **for `language-tokenizer` only**, through a crate-scoped exception in [`deny.toml`](../../deny.toml) that references this amendment.
- **WTFPL stays off the allow-list.** Any other WTFPL dependency is still rejected and needs its own review.

**Consequences:**
- It's only in builds with the `kokoro` feature ([ADR-0014](0014-kokoro-dependencies-and-build.md)), which includes release images, so it appears in their `THIRD_PARTY_LICENSES` ([F4](#f4)).
- If a redistributor needs an OSI-only bill of materials, the alternative is to replace the tokenizer in our fork. That's a behaviour change to verify with the pronunciation parity tests ([#18](https://github.com/huwd/read-to-me-server/issues/18)).

## Follow-ups

| ID | Action | Owner |
|----|--------|-------|
| <a id="f1"></a>F1 | ✅ **Decided 2026-10-11: DCO.** Contributions need a `Signed-off-by` line, checked in CI; see [`CONTRIBUTING.md`](../../CONTRIBUTING.md). A CLA was rejected: with hosting dropped we don't need relicensing rights, and DCO is lighter for contributors. Consequence: the project is fixed to AGPL-3.0-or-later unless every contributor agrees to change it. | Owner |
| <a id="f2"></a>F2 | Add the official AGPL-3.0 text as `LICENSE` and the Apache-2.0 text as `LICENSE-APACHE` (for the apps/SDK directories), copied verbatim from gnu.org / apache.org. | Iteration 0 |
| <a id="f3"></a>F3 | Add `cargo deny check licenses` and `reuse lint` as required CI checks ([doc 05 §2.1](../05-quality-cicd-observability.md#21-on-every-pr-target--10-min-wall-clock)). | Iteration 0 |
| <a id="f4"></a>F4 | CI step generating `THIRD_PARTY_LICENSES` from the SBOM; fail if any component lacks a licence. | Iteration 0 |
| <a id="f5"></a>F5 | Implement the `/source` endpoint and "About → Source code" links. | MVP |
| <a id="f6"></a>F6 | Engine registry schema with `weights_licence` / `commercial_use`, shown to the user. ~~Hosted-build startup check~~ (⚑ only if hosting is revisited). | MVP |
| <a id="f7"></a>F7 | ~~Legal review before the hosted beta~~ — ⚑ only if hosting is revisited. | — |
