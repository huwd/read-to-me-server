# Phase 0: extraction corpus

Throwaway spike tooling for [issue #3](https://github.com/huwd/read-to-me-server/issues/3). It builds the corpus used to test riskiest assumption 1 in [doc 02](../../docs/02-product-viability.md): is extraction clean on *your* reading, unattended, at least 80% of the time? It's Python standard library only, as allowed for spike tools by [ADR-0002](../../docs/adr/0002-rust-modular-monolith.md) Amendment 1, and is deleted when the spike ends.

## Run it

Requires Python 3.9+; nothing to install.

1. Put your five long-form URLs in `LONG_FORM_URLS` at the top of `fetch_corpus.py`.
2. Check what would be fetched, without downloading articles:
   ```sh
   python3 spike/corpus/fetch_corpus.py ~/Downloads/subscriptions.xml --dry-run
   ```
3. Build the corpus:
   ```sh
   python3 spike/corpus/fetch_corpus.py ~/Downloads/subscriptions.xml
   ```

It takes a few minutes: about 100 feeds, then about 100 articles, with at most one request per second to any one host.

## What it does

- **Feeds:** reads every feed in the OPML (duplicates removed). Handles RSS 2.0, RSS 1.0/RDF and Atom.
- **Selection:** picks the **95 most recent** items across all feeds, with at most **5 per feed** so a few prolific sites don't dominate. Change with `--total` and `--per-feed`.
- **Long-form:** adds your hand-picked URLs, tagged `"kind": "long-form"`.
- **robots.txt:** respected as the product will (RFC 9309, user-agent token `read-to-me`). A blocked article is recorded but never requested.
- **Fetching:** sends an honest User-Agent and no cookies. Each page is capped at 10 MB and saved exactly as served.

## Output (`spike/corpus/data/`, git-ignored)

| File | Contents |
|------|----------|
| `manifest.json` | Run summary (ok / failed / blocked by robots.txt, feed problems) and one entry per article |
| `NNN-<slug>/source.html` | The article's raw HTML |
| `NNN-<slug>/feed.html` | The content the feed supplied, if any. When it's long (≥ 300 words, `feed_has_full_text: true`), it's near-ground truth for scoring extraction, so fewer articles need hand-checking. |
| `NNN-<slug>/meta.json` | URL, final URL, feed, publish date, HTTP status, content type, robots.txt decision, errors |

**Don't commit `data/` or your OPML** (both are ignored here). The articles are third-party copyright, and the OPML is your reading list. Keep the corpus locally or in a private repo; [doc 05 §3](../../docs/05-quality-cicd-observability.md#3-test-strategy-pyramid) expects real-world cases to stay private.

## Next

The corpus feeds the extraction spike: candidate Rust extractors, scored against `feed.html` where available and by hand otherwise. The five long-form articles also feed the Kokoro listening test and the speed benchmark.
