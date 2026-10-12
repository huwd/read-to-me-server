# ADR-0001: Record architecture decisions

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner

## Context

read-to-me is planned in detail before any code exists, and many choices (licence, datastore, TTS runtime, hosting stance) have already been debated and changed. Without a record, the reasons get lost and decisions get relitigated, or quietly reversed in code.

## Decision

Record every significant architectural or project decision as an **Architecture Decision Record** in [`docs/adr/`](README.md), using the [MADR](https://adr.github.io/madr/) structure:

- **File name:** `NNNN-short-title.md`, numbered sequentially, never reused.
- **Sections:** Status, Date, Deciders, Related · Context · Decision · Alternatives considered · Consequences (including **⚑ Hosting impact** where relevant) · Follow-ups (optional).
- **Status lifecycle:** `Proposed` → `Accepted` | `Rejected`; later `Deprecated` or `Superseded by ADR-NNNN`. `Deferred` is used for decisions deliberately parked (see [doc 07 §4](../07-roadmap-and-open-questions.md#4-deferred-revisit-later)).
- **Accepted ADRs aren't rewritten.** A small change is recorded as a dated **Amendment** section; a reversal gets a new ADR that supersedes the old one.
- `docs/adr/README.md` is the index and must be updated in the same PR.
- **What counts as "significant":** anything that is hard to reverse, affects more than one module, changes a security or licensing posture, adds a runtime dependency or service, or changes an NFR.
- PRs that implement against an ADR reference it. PRs that contradict an accepted ADR must include the superseding ADR.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Decisions only in the planning docs (01–07) | The docs describe the current state, not why; changes overwrite history. |
| Decisions in issues/PR threads | Hard to find, not versioned with the code. |
| Lightweight log (one table of decisions) | Too little room for context and alternatives on the decisions that matter. |

## Consequences

- Short-term cost: a little writing per decision.
- Long-term benefit: contributors (and future you) can see why things are as they are.
- The planning docs link to ADRs rather than duplicating their reasoning.
