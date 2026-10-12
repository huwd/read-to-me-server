# ADR-0006: Extractor triage/adapter contract, sandboxing and versioning

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 01 §5.4](../01-product-spec.md#54-triage--extraction-ext) ([FR-EXT-*](../01-product-spec.md#54-triage--extraction-ext)), [doc 03 §4](../03-architecture-and-nfrs.md#4-extraction-subsystem-the-triageadapter-contract), [doc 05 §3](../05-quality-cicd-observability.md#3-test-strategy-pyramid) (golden corpus), [ADR-0002](0002-rust-modular-monolith.md), [ADR-0008](0008-content-addressed-storage.md)

## Context

Extraction quality is the product's biggest quality risk ([doc 02 §3](../02-product-viability.md#3-critique-of-the-proposition-the-uncomfortable-bits)). The brief asks for extensible extraction with sensible defaults and a triage-and-adapter pattern, so that site-specific adapters, and later HTML/PDF uploads, can be added without touching the core. Extraction also parses hostile bytes, so it's a major attack surface.

## Decision

### Contract
```rust
pub trait Extractor: Send + Sync {
    fn id(&self) -> &'static str;           // "generic-html", "site/substack", "pdf"
    fn version(&self) -> u32;               // bump on any output-affecting change
    fn specificity(&self) -> Specificity;   // Site > Format > Generic
    /// Cheap, pure, no I/O. Sees URL, media type, a bounded byte prefix, parsed <head> meta.
    fn score(&self, view: &TriageView<'_>) -> Option<Confidence>; // 0.0..=1.0
    /// Runs inside the sandbox only.
    fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError>;
}
```
- **Output is always the structured `Article` model** ([doc 03 §4](../03-architecture-and-nfrs.md#4-extraction-subsystem-the-triageadapter-contract)): title, byline, site, date, canonical URL, language, and ordered blocks (headings, paragraphs, lists, quotes, code, figures, tables). Never HTML.
- **`Article` is versioned** (`article_schema_version`). It's the boundary between extraction and narration, and both sides depend only on it.

### Triage
1. **Score:** every registered extractor scores the document.
2. **Rank:** candidates are those scoring ≥ the threshold (default 0.5), ranked by `(specificity, confidence)`. A user-supplied hint (`extractor=…` on submit or retry) overrides ranking.
3. **Run:** the top candidate runs in the sandbox.
4. **Quality-check:** the result is scored after the fact. Checks are word count ≥ 200, link density, a boilerplate-phrase classifier and a title present.
5. **Fall back:** if the quality check fails, the next candidate runs (max 3 attempts in total).
6. **Give up:** if nothing passes, the item fails with `EXTRACTION_LOW_CONFIDENCE`, and the user may force the best attempt ([FR-EXT-05](../01-product-spec.md#fr-ext-05)).
7. **Record:** the chosen extractor's `id@version`, its confidence and the quality score are stored on the item.

### Built-in extractors (MVP)
- `generic-html`: readability-style heuristics. The crate (e.g. `dom_smoothie` or a readability port) is chosen by F1 on our golden corpus in Phase 0, wrapped behind this trait.
- `structured-data`: uses JSON-LD `Article.articleBody` / microdata when present and plausible.
- `plain-text`

Site adapters and `pdf` come later, as additional registrations.

### Sandbox boundary
- The server re-executes its own binary as `rtm extract-worker`. The `SourceDocument` goes over stdin (length-prefixed) and the `Article` comes back as JSON on stdout.
- The worker gets:
  - no network (network namespace), no environment secrets, a read-only filesystem except a private tmp;
  - a seccomp allow-list and landlock, where available;
  - `RLIMIT_AS` (default 512 MB), CPU time (default 20 s), a wall-clock kill (30 s) and an output size cap.
- **Scoring also runs in the sandbox** whenever it needs to parse the document; `score` only gets a pre-parsed, bounded `TriageView`.
- The server validates the returned `Article` against the schema, limits and allowed characters before using it.

### Versioning and caching
- The extraction cache key is `H(source_bytes) ‖ extractor_id@version ‖ article_schema_version` ([ADR-0008](0008-content-addressed-storage.md)). Bumping `version` invalidates old results naturally.
- The golden corpus snapshots record `id@version`. CI fails if output changes without a version bump.

### Adding an extractor (NFR-M2)
One new module implementing the trait, plus one registry line, plus golden fixtures (`fixtures/corpus/<site>/…`). No pipeline changes.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Single generic extractor with per-site `if` branches | Unmaintainable; no clear extension point. |
| Extractors as WASM plugins | Strong isolation and third-party plugins, but a heavy runtime and ABI design up front. Revisit if community adapters arrive in volume; the trait makes it a later swap. |
| Extractors as external processes in any language | Same isolation, but brings back polyglot packaging, which [ADR-0002](0002-rust-modular-monolith.md) rejects for shipped code. |
| In-process extraction without a sandbox | Parsers are the likeliest RCE/DoS surface ([doc 06 §3.2](../06-security-and-pentest.md#32-parsing--processing-untrusted-content)). |

## Consequences

- Adding sources is additive, and quality is measurable per extractor.
- Sandbox start-up costs some milliseconds per item; negligible next to synthesis.
- Sandboxing features depend on the host kernel. The server reports in `/readyz` and logs which protections are active, and refuses to start without at least rlimits and the separate process unless explicitly overridden.
