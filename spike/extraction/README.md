# Phase 0: extraction comparison

Throwaway spike for [issue #13](https://github.com/huwd/read-to-me-server/issues/13). It's a standalone Cargo workspace, so its dependencies stay out of the product's lockfile and licence gates. It's deleted after the spike.

```sh
cd spike/extraction && cargo run --release        # reads ../corpus/data (built by spike/corpus)
```

It writes each extractor's text to `spike/corpus/data/<article>/extracted/` and the full per-article table to `spike/corpus/data/extraction-report.md`. Both are git-ignored, because they contain third-party article text.

## Method

- **Corpus:** 97 usable pages from 53 sites, taken from the owner's feeds (95 most recent, at most 2 per feed) plus 4 LRB essays (#3).
- **Answer key:** 56 pages whose feed carries the full text (≥ 300 words). For the LRB essays, the reference is the essay paragraphs in the HTML, which a paywall hides in the browser.
- **Score:** bag-of-words token overlap, giving precision (how much output is article) and recall (how much article is in the output). **Clean** means F1 ≥ 0.90 (AC-EXT-02).
- **Excerpt detection:** a feed reference is excluded when most extractors return more than 1.6× its length, which means the feed holds only an excerpt. 3 were excluded, leaving 53.

## Results (2026-10-12)

| Extractor | Clean | Mean F1 | Precision | Recall | LRB hidden text | Errors | ms (97 pages) |
|---|---|---|---|---|---|---|---|
| **trafilatura** 0.3.0 | **47/53 (89%)** | **0.926** | 0.990 | 0.903 | 4/4 | 0 | 256 |
| trafilatura (recall mode, fallback) | 47/53 (89%) | 0.926 | 0.989 | 0.904 | 4/4 | 0 | 302 |
| readabilityrs 0.1.4 | 45/53 (85%) | 0.909 | 0.950 | 0.899 | 4/4 | 0 | 651 |
| dom_smoothie 0.18.2 | 43/53 (81%) | 0.909 | 0.970 | 0.888 | 4/4 | 0 | 106 |
| legible 0.5.1 | 43/53 (81%) | 0.908 | 0.969 | 0.888 | 4/4 | 0 | 90 |
| readability 0.3.0 | 36/53 (68%) | 0.802 | 0.960 | 0.770 | 4/4 | 0 | 83 |
| *all visible text (baseline)* | 20/53 (38%) | 0.811 | 0.740 | 0.942 | 4/4 | 0 | 57 |

## What the failures are

- **Server-side paywall:** 2 A Media Operator pages fail for every extractor. The page holds only about 80–100 words of the article, while the feed has 1,000–1,800. Extraction can't recover text that isn't there. The product must notice the truncation (a short result for a long-looking page) and say so (FR-EXT-05), not narrate a fragment.
- **Multi-page articles:** 2 Ars Technica pages fail for every extractor. Only page 1 is fetched. This calls for following `rel="next"` pagination, or a site adapter.
- **Readability-family misses:** 2 Open Culture pages (0.62–0.65) and Ruby Weekly (0.77–0.79). Trafilatura scores 0.98–1.00 on these.
- **readability 0.3.0 collapses on some sites,** scoring 0.00–0.03 on Politico Playbook and 404 Media. It's unmaintained since 2023, so it's not a candidate.
- **Reference noise:** Sinocism scores 0.81–0.83 for everyone, with output the same length as the feed but differently worded. One House of Commons Library page is borderline (trafilatura 0.89).

**Excluding the 4 page-level failures,** trafilatura is clean on 47 of 49 (96%), dom_smoothie on 43 of 49 (88%).

**Hidden paywall text:** every extractor keeps the LRB essays (recall 0.91–0.99). So "full text hidden by CSS" isn't a problem on these pages.

**Very short pages** (6, under 300 words): every extractor returns about 100 words, so FR-EXT-05's 200-word floor flags them as low confidence, as intended.

## Riskiest assumption 1: verdict

**Held.** Unattended extraction is clean on 89% of answer-key pages with the best extractor, and 96% once the page-level failures are excluded, against an 80% target. The remaining risk lies in **fetching** (server-side paywalls, pagination) more than in extraction.

## Recommendation for ADR-0006

- **Default `generic-html`:** `trafilatura`. It has the best quality on every measure and no errors.
  - **Risk:** it's young (first release February 2026, last March 2026, four releases).
  - **Mitigation:** wrap it behind the `Extractor` trait, pin the version, and be ready to fork or vendor it.
- **Fallback candidate:** `dom_smoothie`, which is actively maintained with steady releases since 2025. ADR-0006's quality check can fall back to it, and it's the natural replacement if trafilatura stalls.
- **Follow-ups:**
  - a truncation signal comparing extracted length with page and metadata hints;
  - pagination support (`rel="next"`);
  - add the failing pages to the golden corpus as private fixtures.
