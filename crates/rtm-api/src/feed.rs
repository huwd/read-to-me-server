// SPDX-License-Identifier: AGPL-3.0-or-later
//! The private podcast feed (FR-FEED-01..08, ADR-0009): feed tokens, the RSS document,
//! episode enclosures with `Range` support, and the default artwork.
//!
//! Feed URLs are capabilities: whoever has the token can read the feed, so tokens are
//! 256-bit random, stored only as BLAKE3 hashes, and rotatable. Request logs carry the
//! matched route (`/feeds/{file}`), never the token.

use std::io::SeekFrom;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesText, Event};
use rtm_blobs::{BlobKind, BlobStore};
use rtm_domain::{Item, ItemId, Millis, UserId};
use rtm_ports::{Feed, FeedRepository, ItemRepository};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use crate::problem::Problem;

/// FR-FEED-07: the most recent items shown.
pub const FEED_ITEMS: u32 = 100;

/// The bundled default artwork (FR-FEED-08): 1400 × 1400 PNG.
const ARTWORK: &[u8] = include_bytes!("../assets/artwork.png");

const TOKEN_PREFIX: &str = "rtm_feed_";

/// What the feed routes need.
pub struct FeedState<R> {
    pub store: Arc<R>,
    pub blobs: BlobStore,
    /// Where the server is reached from outside, for absolute URLs in the feed.
    pub public_url: Url,
}

impl<R> Clone for FeedState<R> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            blobs: self.blobs.clone(),
            public_url: self.public_url.clone(),
        }
    }
}

/// A new feed token: 256 random bits, prefixed so secret scanners recognise it.
fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{TOKEN_PREFIX}{hex}")
}

/// How a token is stored and looked up.
pub fn token_hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

fn feed_url(public: &Url, token: &str) -> String {
    format!(
        "{}/feeds/{token}.xml",
        public.as_str().trim_end_matches('/')
    )
}

/// `POST /v1/feed/token`: create or rotate the caller's feed token and return the feed
/// URL. This is the only time the token is shown; rotating invalidates the old URL at
/// once (FR-FEED-05).
pub async fn rotate_token<R: FeedRepository>(
    State(state): State<FeedState<R>>,
    Extension(user): Extension<UserId>,
) -> Response {
    let token = new_token();
    match state
        .store
        .set_feed_token(&user, &token_hash(&token), now())
        .await
    {
        Ok(_) => {
            let body = serde_json::json!({ "feed_url": feed_url(&state.public_url, &token) });
            let mut response = (StatusCode::CREATED, axum::Json(body)).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(e) => {
            tracing::error!(error = %e, "could not set feed token");
            Problem::internal().into_response()
        }
    }
}

fn now() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| {
            Millis::try_from(d.as_millis()).unwrap_or(Millis::MAX)
        })
}

/// A token-shaped path segment, before any lookup. Anything else is a plain 404.
fn token_from(segment: &str) -> Option<&str> {
    let hex = segment.strip_prefix(TOKEN_PREFIX)?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(segment)
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn open_feed<R: FeedRepository>(state: &FeedState<R>, token: &str) -> Result<Feed, Response> {
    let token = token_from(token).ok_or_else(not_found)?;
    match state.store.feed_by_token(&token_hash(token)).await {
        Ok(Some(feed)) => Ok(feed),
        Ok(None) => Err(not_found()),
        Err(e) => {
            tracing::error!(error = %e, "feed lookup failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

/// HTTP-date (RFC 9110 §5.6.7), e.g. "Sun, 06 Nov 1994 08:49:37 GMT".
fn http_date(ms: Millis) -> String {
    let format = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    time::OffsetDateTime::from_unix_timestamp(ms / 1000)
        .ok()
        .and_then(|t| t.format(format).ok())
        .unwrap_or_default()
}

/// RFC 2822 date for `pubDate`.
fn rss_date(ms: Millis) -> String {
    time::OffsetDateTime::from_unix_timestamp(ms / 1000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc2822)
                .ok()
        })
        .unwrap_or_default()
}

/// Whether an `If-None-Match` header matches `etag` (weak comparison, RFC 9110 §13.1.2).
fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    value.trim() == "*"
        || value
            .split(',')
            .map(|t| t.trim().trim_start_matches("W/"))
            .any(|t| t == etag)
}

/// The episode's plain-text description (ADR-0009): byline, source and length.
fn description(item: &Item) -> String {
    let mut parts = Vec::new();
    if let Some(article) = &item.article {
        parts.push(format!("{} words.", article.words));
    }
    parts.push(format!("Read from {}", item.source_url));
    parts.join(" ")
}

type Xml = Writer<Vec<u8>>;

fn text(w: &mut Xml, name: &str, value: &str) -> std::io::Result<()> {
    w.create_element(name)
        .write_text_content(BytesText::new(value))
        .map(|_| ())
}

/// The RSS document for `feed`'s published `items`.
fn render(feed: &Feed, items: &[Item], base: &str, token: &str) -> Vec<u8> {
    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    let feed_base = format!("{base}/feeds/{token}");
    let result: std::io::Result<()> = (|| {
        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        w.create_element("rss")
            .with_attributes([
                ("version", "2.0"),
                ("xmlns:itunes", "http://www.itunes.com/dtds/podcast-1.0.dtd"),
                ("xmlns:podcast", "https://podcastindex.org/namespace/1.0"),
            ])
            .write_inner_content(|w| {
                w.create_element("channel").write_inner_content(|w| {
                    text(w, "title", "read-to-me")?;
                    text(w, "link", base)?;
                    text(
                        w,
                        "description",
                        "Articles you've sent to read-to-me, read aloud.",
                    )?;
                    text(w, "language", "en")?;
                    text(w, "itunes:author", "read-to-me")?;
                    text(w, "itunes:explicit", "false")?;
                    w.create_element("itunes:image")
                        .with_attribute(("href", format!("{feed_base}/artwork.png").as_str()))
                        .write_empty()?;
                    w.create_element("image").write_inner_content(|w| {
                        text(w, "url", &format!("{feed_base}/artwork.png"))?;
                        text(w, "title", "read-to-me")?;
                        text(w, "link", base)
                    })?;
                    // FR-FEED-03: private.
                    text(w, "itunes:block", "Yes")?;
                    text(w, "podcast:locked", "yes")?;
                    text(w, "podcast:guid", &feed.guid)?;
                    for item in items {
                        let (Some(episode), Some(published)) = (&item.episode, item.published_at)
                        else {
                            continue;
                        };
                        let title = item
                            .article
                            .as_ref()
                            .map_or(item.source_url.as_str(), |a| a.title.as_str());
                        w.create_element("item").write_inner_content(|w| {
                            text(w, "title", title)?;
                            w.create_element("guid")
                                .with_attribute(("isPermaLink", "false"))
                                .write_text_content(BytesText::new(&item.id.to_string()))?;
                            text(w, "link", &item.source_url)?;
                            text(w, "pubDate", &rss_date(published))?;
                            text(w, "description", &description(item))?;
                            w.create_element("enclosure")
                                .with_attributes([
                                    ("url", format!("{feed_base}/items/{}.mp3", item.id).as_str()),
                                    ("length", episode.bytes.to_string().as_str()),
                                    ("type", "audio/mpeg"),
                                ])
                                .write_empty()?;
                            text(
                                w,
                                "itunes:duration",
                                &episode.duration_ms.div_ceil(1000).to_string(),
                            )?;
                            text(w, "itunes:episodeType", "full")
                        })?;
                    }
                    Ok(())
                })?;
                Ok(())
            })?;
        Ok(())
    })();
    result.expect("writing XML to memory can't fail");
    w.into_inner()
}

fn feed_headers(response: &mut Response) {
    let h = response.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
}

/// `GET /feeds/{token}.xml` (FR-FEED-01/06).
pub async fn feed<R: FeedRepository + ItemRepository>(
    State(state): State<FeedState<R>>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = file.strip_suffix(".xml") else {
        return not_found();
    };
    let feed = match open_feed(&state, token).await {
        Ok(feed) => feed,
        Err(response) => return response,
    };
    let items = match state.store.published(&feed.user, FEED_ITEMS).await {
        Ok(items) => items,
        Err(e) => {
            tracing::error!(error = %e, "could not list feed items");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let base = state.public_url.as_str().trim_end_matches('/');
    let body = render(&feed, &items, base, token);
    let etag = format!("\"{}\"", &blake3::hash(&body).to_hex()[..32]);
    let mut response = if matches_etag(&headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/rss+xml; charset=utf-8"),
            )],
            body,
        )
            .into_response()
    };
    feed_headers(&mut response);
    let h = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    if let Some(latest) = items.iter().filter_map(|i| i.published_at).max()
        && let Ok(v) = HeaderValue::from_str(&http_date(latest))
    {
        h.insert(header::LAST_MODIFIED, v);
    }
    response
}

/// `GET /feeds/{token}/artwork.png` (FR-FEED-08).
pub async fn artwork<R: FeedRepository>(
    State(state): State<FeedState<R>>,
    Path(token): Path<String>,
) -> Response {
    if let Err(response) = open_feed(&state, &token).await {
        return response;
    }
    let mut response = ([(header::CONTENT_TYPE, "image/png")], ARTWORK).into_response();
    feed_headers(&mut response);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    response
}

/// A single byte range within a file of `len` bytes (RFC 9110 §14.1.2): start and
/// end, inclusive. `Err` if it can't be satisfied; `Ok(None)` if there is no usable
/// single range, so the whole file is sent.
fn byte_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return Ok(None);
    };
    // Multiple ranges are allowed to be answered with the whole representation.
    if spec.contains(',') {
        return Ok(None);
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    let (start, end) = (start.trim(), end.trim());
    let range = match (start.is_empty(), end.is_empty()) {
        // "-n": the last n bytes.
        (true, false) => {
            let n: u64 = end.parse().map_err(|_| ())?;
            if n == 0 || len == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len - 1)
        }
        // "a-": from a to the end.
        (false, true) => (start.parse().map_err(|_| ())?, len.saturating_sub(1)),
        // "a-b".
        (false, false) => {
            let a: u64 = start.parse().map_err(|_| ())?;
            let b: u64 = end.parse().map_err(|_| ())?;
            if b < a {
                return Err(());
            }
            (a, b.min(len.saturating_sub(1)))
        }
        (true, true) => return Err(()),
    };
    if range.0 >= len {
        return Err(());
    }
    Ok(Some(range))
}

/// `GET /feeds/{token}/items/{id}.mp3` (FR-FEED-04, ADR-0009): the episode, with
/// single `Range` requests answered with 206 so podcast apps can seek.
pub async fn enclosure<R: FeedRepository + ItemRepository>(
    State(state): State<FeedState<R>>,
    Path((token, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let feed = match open_feed(&state, &token).await {
        Ok(feed) => feed,
        Err(response) => return response,
    };
    let Some(id) = file
        .strip_suffix(".mp3")
        .and_then(|id| id.parse::<ItemId>().ok())
    else {
        return not_found();
    };
    let item = match state.store.get(&feed.user, id).await {
        Ok(Some(item)) => item,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(error = %e, "enclosure lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let (Some(episode), Some(published)) = (&item.episode, item.published_at) else {
        return not_found();
    };
    let path = state.blobs.local_path(BlobKind::Episode, &episode.key);
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        // Published but the audio is gone: it was deleted (FR-FEED-04).
        return StatusCode::GONE.into_response();
    };
    let len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let etag = format!("\"{}\"", &episode.key.as_str()[..32]);
    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let mut response = match byte_range(range_header, len) {
        Err(()) => {
            let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
        Ok(Some((start, end))) => {
            if file.seek(SeekFrom::Start(start)).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let stream = tokio_util::io::ReaderStream::new(file.take(end - start + 1));
            let mut r = (StatusCode::PARTIAL_CONTENT, Body::from_stream(stream)).into_response();
            let h = r.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                h.insert(header::CONTENT_RANGE, v);
            }
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start + 1));
            r
        }
        Ok(None) => {
            let stream = tokio_util::io::ReaderStream::new(file);
            let mut r = Body::from_stream(stream).into_response();
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            r
        }
    };
    let h = response.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&http_date(published)) {
        h.insert(header::LAST_MODIFIED, v);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_random_and_recognisable() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(a.starts_with("rtm_feed_") && a.len() == 9 + 64);
        assert_eq!(token_from(&a), Some(a.as_str()));
        for bad in [
            "rtm_feed_short",
            "nope",
            &format!("{a}x"),
            &a.replace("rtm_feed_", "rtm_pat_"),
        ] {
            assert_eq!(token_from(bad), None, "{bad}");
        }
        assert_ne!(token_hash(&a), token_hash(&b));
    }

    #[test]
    fn ranges() {
        let r = |h: &str| byte_range(Some(h), 1000);
        assert_eq!(r("bytes=0-99"), Ok(Some((0, 99))));
        assert_eq!(r("bytes=500-"), Ok(Some((500, 999))));
        assert_eq!(r("bytes=-100"), Ok(Some((900, 999))));
        assert_eq!(r("bytes=900-5000"), Ok(Some((900, 999))));
        assert_eq!(r("bytes=-5000"), Ok(Some((0, 999))));
        assert_eq!(r("bytes=0-0"), Ok(Some((0, 0))));
        assert_eq!(r("bytes=0-1,5-9"), Ok(None));
        assert_eq!(byte_range(None, 1000), Ok(None));
        assert_eq!(r("items=0-1"), Ok(None));
        for bad in [
            "bytes=1000-",
            "bytes=5-1",
            "bytes=-0",
            "bytes=-",
            "bytes=x-1",
            "bytes=1",
        ] {
            assert_eq!(r(bad), Err(()), "{bad}");
        }
        assert_eq!(byte_range(Some("bytes=0-"), 0), Err(()));
    }

    #[test]
    fn dates() {
        assert_eq!(http_date(784_111_777_000), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(rss_date(784_111_777_000), "Sun, 06 Nov 1994 08:49:37 +0000");
    }

    #[test]
    fn etag_matching() {
        let mut h = HeaderMap::new();
        assert!(!matches_etag(&h, "\"a\""));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"x\", W/\"a\""),
        );
        assert!(matches_etag(&h, "\"a\""));
        h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(matches_etag(&h, "\"b\""));
    }
}
