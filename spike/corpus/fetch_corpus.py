#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Phase 0 spike: build an extraction test corpus from an OPML feed list.

Throwaway tooling (ADR-0002 Amendment 1): Python standard library only, never
shipped, deleted after the spike.

What it does
  1. Reads every feed in an OPML file (duplicates removed).
  2. Picks the most recent items across all feeds (default 95, at most 5 per feed).
  3. Adds the hand-picked long-form URLs below.
  4. Downloads each article's raw HTML, respecting robots.txt (RFC 9309), and
     saves feed-supplied content where present (near-ground truth for scoring).

Usage
  python3 spike/corpus/fetch_corpus.py path/to/subscriptions.xml
  python3 spike/corpus/fetch_corpus.py subscriptions.xml --dry-run   # select only

Output (default spike/corpus/data/, git-ignored; never commit it):
  data/manifest.json            one entry per article
  data/NNN-<slug>/source.html   raw bytes as served
  data/NNN-<slug>/feed.html     content from the feed, if any
  data/NNN-<slug>/meta.json     URL, feed, dates, status, robots decision, ...
"""

from __future__ import annotations  # Python 3.9 compatibility for type hints

# --- Hand-picked long-form articles (tagged "long-form" in the manifest) ------
LONG_FORM_URLS = [
    "https://www.lrb.co.uk/the-paper/v37/n03/margaret-macmillan/a-bit-of-chaos",
    "https://www.lrb.co.uk/the-paper/v29/n14/jonathan-wright/breathtaking-co-ordination",
    "https://www.lrb.co.uk/the-paper/v40/n20/simon-wren-lewis/bait-and-switch",
    "https://www.lrb.co.uk/the-paper/v43/n24/john-lanchester/as-the-lock-rattles",
    "https://newleftreview.org/issues/ii119/articles/4444",
]

# --- Settings -----------------------------------------------------------------
TOTAL_FROM_FEEDS = 95
MAX_PER_FEED = 5
USER_AGENT = "read-to-me-spike/0.1 (+https://github.com/huwd/read-to-me-server)"
ROBOTS_TOKEN = "read-to-me"          # product token matched in robots.txt
TIMEOUT_SECONDS = 30
MAX_BYTES = 10 * 1024 * 1024         # matches FR-FET-03's HTML limit
WORKERS = 8                          # parallel hosts
PER_HOST_DELAY_SECONDS = 1.0         # politeness between requests to one host
FULL_TEXT_MIN_WORDS = 300            # feed content at least this long counts as full text

import argparse
import concurrent.futures
import datetime as dt
import email.utils
import html
import json
import re
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import urllib.robotparser
import xml.etree.ElementTree as ET
from pathlib import Path

NS = {
    "atom": "http://www.w3.org/2005/Atom",
    "content": "http://purl.org/rss/1.0/modules/content/",
    "dc": "http://purl.org/dc/elements/1.1/",
    "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "rss1": "http://purl.org/rss/1.0/",
}


# --- HTTP ---------------------------------------------------------------------
_host_locks: dict[str, threading.Lock] = {}
_host_last: dict[str, float] = {}
_host_guard = threading.Lock()


def _polite(host: str) -> threading.Lock:
    with _host_guard:
        return _host_locks.setdefault(host, threading.Lock())


def fetch(url: str, accept: str = "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5"):
    """GET with size cap and per-host politeness. Returns (status, final_url, content_type, body, error)."""
    host = urllib.parse.urlsplit(url).netloc
    lock = _polite(host)
    with lock:
        wait = _host_last.get(host, 0) + PER_HOST_DELAY_SECONDS - time.monotonic()
        if wait > 0:
            time.sleep(wait)
        req = urllib.request.Request(url, headers={
            "User-Agent": USER_AGENT,
            "Accept": accept,
            "Accept-Encoding": "identity",
        })
        try:
            with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as resp:
                body = resp.read(MAX_BYTES + 1)
                error = "too large (truncated)" if len(body) > MAX_BYTES else None
                return resp.status, resp.geturl(), resp.headers.get("Content-Type", ""), body[:MAX_BYTES], error
        except urllib.error.HTTPError as e:
            return e.code, url, e.headers.get("Content-Type", "") if e.headers else "", b"", f"HTTP {e.code}"
        except Exception as e:  # network, TLS, timeout
            return None, url, "", b"", f"{type(e).__name__}: {e}"
        finally:
            _host_last[host] = time.monotonic()


# --- robots.txt (RFC 9309) ----------------------------------------------------
_robots: dict[str, urllib.robotparser.RobotFileParser | None] = {}
_robots_guard = threading.Lock()


def robots_allows(url: str) -> tuple[bool, str]:
    parts = urllib.parse.urlsplit(url)
    origin = f"{parts.scheme}://{parts.netloc}"
    with _robots_guard:
        cached = origin in _robots
    if not cached:
        status, _, _, body, error = fetch(origin + "/robots.txt", accept="text/plain,*/*")
        if status is not None and 200 <= status < 300:
            rp = urllib.robotparser.RobotFileParser()
            rp.parse(body.decode("utf-8", "replace").splitlines())
            parser = rp
        elif status is not None and 400 <= status < 500:
            parser = None  # RFC 9309: unavailable -> everything allowed
        else:
            parser = "unreachable"  # RFC 9309: unreachable -> assume disallowed
        with _robots_guard:
            _robots[origin] = parser
    parser = _robots[origin]
    if parser is None:
        return True, "no robots.txt (4xx)"
    if parser == "unreachable":
        return False, "robots.txt unreachable"
    allowed = parser.can_fetch(ROBOTS_TOKEN, url)
    return allowed, "allowed by robots.txt" if allowed else "disallowed by robots.txt"


# --- OPML and feeds -----------------------------------------------------------
def read_opml(path: Path) -> list[dict]:
    feeds, seen = [], set()
    for node in ET.parse(path).iter("outline"):
        url = (node.get("xmlUrl") or "").strip()
        if url and url not in seen:
            seen.add(url)
            feeds.append({"title": node.get("title") or node.get("text") or url, "url": url})
    return feeds


def _text(el) -> str:
    return (el.text or "").strip() if el is not None else ""


def _date(value: str) -> dt.datetime | None:
    value = value.strip()
    if not value:
        return None
    try:
        d = email.utils.parsedate_to_datetime(value)  # RFC 822 (RSS)
    except (TypeError, ValueError):
        try:
            d = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))  # ISO 8601 (Atom)
        except ValueError:
            return None
    return d if d.tzinfo else d.replace(tzinfo=dt.timezone.utc)


def parse_feed(body: bytes, feed: dict) -> list[dict]:
    root = ET.fromstring(body)
    items = []
    if root.tag == f"{{{NS['atom']}}}feed":
        for e in root.findall("atom:entry", NS):
            link = next((l.get("href") for l in e.findall("atom:link", NS)
                         if l.get("rel", "alternate") == "alternate" and l.get("href")), "")
            content = e.find("atom:content", NS)
            if content is None:
                content = e.find("atom:summary", NS)
            items.append({
                "title": _text(e.find("atom:title", NS)),
                "url": link,
                "published": _date(_text(e.find("atom:published", NS)) or _text(e.find("atom:updated", NS))),
                "content": _text(content),
            })
    else:  # RSS 2.0 or RSS 1.0 (RDF)
        nodes = root.findall("./channel/item") or root.findall("rss1:item", NS)
        for i in nodes:
            link = _text(i.find("link")) or _text(i.find("rss1:link", NS))
            guid = i.find("guid")
            if not link and guid is not None and guid.get("isPermaLink", "true") == "true":
                link = _text(guid)
            content = _text(i.find("content:encoded", NS)) or _text(i.find("description")) \
                or _text(i.find("rss1:description", NS))
            items.append({
                "title": _text(i.find("title")) or _text(i.find("rss1:title", NS)),
                "url": link,
                "published": _date(_text(i.find("pubDate")) or _text(i.find("dc:date", NS))),
                "content": content,
            })
    for it in items:
        it["feed_title"], it["feed_url"] = feed["title"], feed["url"]
    return [it for it in items if it["url"].startswith(("http://", "https://"))]


def collect_items(feeds: list[dict]) -> tuple[list[dict], list[dict]]:
    items, problems = [], []

    def one(feed):
        allowed, why = robots_allows(feed["url"])
        if not allowed:
            return feed, [], why
        status, _, _, body, error = fetch(feed["url"], accept="application/rss+xml,application/atom+xml,application/xml,text/xml,*/*")
        if error or not body:
            return feed, [], error or f"empty response ({status})"
        try:
            return feed, parse_feed(body, feed), None
        except ET.ParseError as e:
            return feed, [], f"feed parse error: {e}"

    with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
        for feed, found, problem in pool.map(one, feeds):
            print(f"  {'!' if problem else ' '} {len(found):3d} items  {feed['title'][:60]}" + (f"  ({problem})" if problem else ""))
            items.extend(found)
            if problem:
                problems.append({"feed": feed["title"], "url": feed["url"], "problem": problem})
    return items, problems


def select(items: list[dict], total: int, per_feed: int, exclude: set[str]) -> list[dict]:
    epoch = dt.datetime.min.replace(tzinfo=dt.timezone.utc)
    items = sorted(items, key=lambda i: i["published"] or epoch, reverse=True)
    chosen, per, seen = [], {}, set(exclude)
    for it in items:
        if len(chosen) >= total:
            break
        if it["url"] in seen or per.get(it["feed_url"], 0) >= per_feed:
            continue
        seen.add(it["url"])
        per[it["feed_url"]] = per.get(it["feed_url"], 0) + 1
        chosen.append(it)
    return chosen


# --- Articles -----------------------------------------------------------------
def words(html_text: str) -> int:
    text = re.sub(r"(?is)<(script|style|noscript|template)\b.*?</\1\s*>", " ", html_text)
    text = re.sub(r"<[^>]+>", " ", text)
    return len(html.unescape(text).split())


def slug(text: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return s[:50] or "untitled"


def download(n: int, item: dict, out: Path) -> dict:
    folder = out / f"{n:03d}-{slug(item.get('title') or item['url'])}"
    folder.mkdir(parents=True, exist_ok=True)
    meta = {
        "id": folder.name,
        "kind": item["kind"],
        "url": item["url"],
        "title": item.get("title", ""),
        "feed_title": item.get("feed_title"),
        "feed_url": item.get("feed_url"),
        "published": item["published"].isoformat() if item.get("published") else None,
        "fetched_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
    }
    feed_html = item.get("content") or ""
    if feed_html:
        (folder / "feed.html").write_text(feed_html, encoding="utf-8")
    meta["feed_content_words"] = words(feed_html) if feed_html else 0
    meta["feed_has_full_text"] = meta["feed_content_words"] >= FULL_TEXT_MIN_WORDS

    allowed, why = robots_allows(item["url"])
    meta["robots"] = why
    if not allowed:
        unreachable = why == "robots.txt unreachable"
        meta.update(status="robots-unreachable" if unreachable else "robots-disallowed", http_status=None)
    else:
        status, final_url, ctype, body, error = fetch(item["url"])
        meta.update(http_status=status, final_url=final_url, content_type=ctype, bytes=len(body), error=error)
        if body:
            (folder / "source.html").write_bytes(body)
            # Rough size of all text in the page (scripts/styles removed), including
            # anything hidden by CSS/JS paywalls: compare with the expected article length.
            meta["source_words"] = words(body.decode("utf-8", "replace"))
            if not meta["title"]:
                t = re.search(rb"<title[^>]*>(.*?)</title>", body, re.S | re.I)
                if t:
                    meta["title"] = html.unescape(t.group(1).decode("utf-8", "replace")).strip()
        ok = status is not None and 200 <= status < 300 and body and not error
        meta["status"] = "ok" if ok else "failed"
    (folder / "meta.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return meta


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("opml", type=Path, help="OPML export of your feeds")
    ap.add_argument("--out", type=Path, default=Path(__file__).resolve().parent / "data")
    ap.add_argument("--total", type=int, default=TOTAL_FROM_FEEDS)
    ap.add_argument("--per-feed", type=int, default=MAX_PER_FEED)
    ap.add_argument("--dry-run", action="store_true", help="select articles but don't download them")
    args = ap.parse_args()

    long_form = [u.strip() for u in LONG_FORM_URLS if u.strip()]
    if len(long_form) != 5:
        print(f"note: LONG_FORM_URLS has {len(long_form)} URL(s); the plan expects 5.", file=sys.stderr)

    feeds = read_opml(args.opml)
    print(f"Reading {len(feeds)} feeds ...")
    items, feed_problems = collect_items(feeds)
    chosen = select(items, args.total, args.per_feed, set(long_form))
    for it in chosen:
        it["kind"] = "feed"
    chosen += [{"kind": "long-form", "url": u, "title": "", "published": None} for u in long_form]
    print(f"\nSelected {len(chosen)} articles ({len(chosen) - len(long_form)} from feeds, {len(long_form)} long-form).")
    if chosen and chosen[0].get("published"):
        dated = [c["published"] for c in chosen if c.get("published")]
        print(f"Feed items published between {min(dated):%Y-%m-%d} and {max(dated):%Y-%m-%d}.")
    if args.dry_run:
        for n, it in enumerate(chosen, 1):
            print(f"  {n:3d}  {it.get('feed_title') or it['kind']:<32.32}  {it['url']}")
        return 0

    args.out.mkdir(parents=True, exist_ok=True)
    print(f"Downloading to {args.out} ...")
    with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
        futures = [pool.submit(download, n, it, args.out) for n, it in enumerate(chosen, 1)]
        manifest = []
        for f in concurrent.futures.as_completed(futures):
            m = f.result()
            manifest.append(m)
            mark = {"ok": " ", "failed": "!", "robots-disallowed": "R", "robots-unreachable": "?"}[m["status"]]
            print(f"  {mark} {m['id'][:60]}" + (f"  ({m.get('error') or m['robots']})" if m["status"] != "ok" else ""))
    manifest.sort(key=lambda m: m["id"])
    summary = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "feeds_read": len(feeds),
        "feed_problems": feed_problems,
        "articles": len(manifest),
        "ok": sum(m["status"] == "ok" for m in manifest),
        "failed": sum(m["status"] == "failed" for m in manifest),
        "robots_disallowed": sum(m["status"] == "robots-disallowed" for m in manifest),
        "robots_unreachable": sum(m["status"] == "robots-unreachable" for m in manifest),
        "feed_full_text": sum(m["feed_has_full_text"] for m in manifest),
        "items": manifest,
    }
    (args.out / "manifest.json").write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"\nDone: {summary['ok']} ok, {summary['failed']} failed, {summary['robots_disallowed']} blocked by robots.txt, "
          f"{summary['robots_unreachable']} skipped (robots.txt unreachable), "
          f"{len(feed_problems)} feed problems; {summary['feed_full_text']} have full text in the feed.")
    print(f"Manifest: {args.out / 'manifest.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
