// SPDX-License-Identifier: AGPL-3.0-or-later
//! The private feed: tokens, the RSS document, enclosures with Range, and artwork
//! (FR-FEED-01..08, AC-FEED-02/03/04, ADR-0009).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, TOKEN, app};
use rtm_blobs::BlobKind;
use rtm_domain::{BlobKey, EpisodeRecord, Item, Stage, UserId};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};

/// When test items are published (2025-10-09).
const PUBLISHED_AT: i64 = 1_760_000_000_000;

/// Stand-in MP3 bytes: the feed serves whatever the episode blob holds.
fn episode_bytes() -> Vec<u8> {
    (0..10_000u32).map(|i| (i % 251) as u8).collect()
}

/// Publish an item for `user` with an episode blob.
async fn published(app: &App, user: &str, url: &str, title: &str) -> Item {
    let owner = UserId(user.into());
    let item = app.store.submit(&owner, url, 0).await.unwrap();
    let key = BlobKey::from_hash(*blake3::hash(url.as_bytes()).as_bytes());
    app.blobs
        .put(BlobKind::Episode, &key, episode_bytes())
        .await
        .unwrap();
    for stage in Stage::ALL {
        let job = app
            .store
            .claim("w", &[stage], PUBLISHED_AT, 60_000)
            .await
            .unwrap()
            .unwrap();
        let output = match stage {
            Stage::Fetch => StageOutput::Source(rtm_domain::Source {
                key: BlobKey::from_hash([1; 32]),
                final_url: url.into(),
                kind: rtm_domain::SourceKind::Html,
                charset: None,
                size: 1,
            }),
            Stage::Extract => StageOutput::Article(rtm_domain::ArticleRecord {
                key: BlobKey::from_hash([2; 32]),
                extractor: "generic-html@1".into(),
                title: title.into(),
                title_derived: false,
                words: 1234,
            }),
            Stage::Script => StageOutput::Script(rtm_domain::ScriptRecord {
                key: BlobKey::from_hash([3; 32]),
                words: 1240,
            }),
            Stage::Synthesise => StageOutput::Speech(rtm_domain::SpeechRecord {
                plan_key: BlobKey::from_hash([4; 32]),
                duration_ms: 480_400,
                segments: 9,
            }),
            Stage::Encode => StageOutput::Episode(EpisodeRecord {
                key: key.clone(),
                bytes: episode_bytes().len() as u64,
                duration_ms: 480_400,
            }),
            Stage::Publish => StageOutput::Nothing,
        };
        app.store
            .complete(&job, "w", output, PUBLISHED_AT)
            .await
            .unwrap();
    }
    app.store.get(&owner, item.id).await.unwrap().unwrap()
}

fn child<'a, 'input>(
    parent: roxmltree::Node<'a, 'input>,
    ns: Option<&str>,
    name: &str,
) -> roxmltree::Node<'a, 'input> {
    parent
        .children()
        .find(|n| n.tag_name().name() == name && n.tag_name().namespace() == ns)
        .unwrap_or_else(|| panic!("no {name}"))
}

async fn new_feed_url(app: &App) -> String {
    let reply = app
        .send(
            Request::post("/v1/feed/token")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    reply.json["feed_url"].as_str().unwrap().to_string()
}

/// The path part of an absolute feed URL.
fn path(url: &str) -> &str {
    url.strip_prefix("https://rtm.example").unwrap()
}

async fn get(
    app: &App,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::get(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let response = tower::ServiceExt::oneshot(app.router.clone(), req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

#[tokio::test]
async fn the_feed_is_valid_private_rss_with_the_published_episodes() {
    let app = app().await;
    let first = published(
        &app,
        "owner",
        "https://example.org/locks",
        "Locks & Levels <1>",
    )
    .await;
    // Someone else's: not in this feed.
    published(&app, "someone-else", "https://example.org/other", "Other").await;
    // Not published: not in the feed. (Submitted last, so the helper never publishes it.)
    app.store
        .submit(&UserId("owner".into()), "https://example.org/pending", 0)
        .await
        .unwrap();

    let url = new_feed_url(&app).await;
    assert!(url.starts_with("https://rtm.example/feeds/rtm_feed_") && url.ends_with(".xml"));
    let (status, headers, body) = get(&app, path(&url), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/rss+xml; charset=utf-8"
    );
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(headers.contains_key(header::ETAG) && headers.contains_key(header::LAST_MODIFIED));

    // AC-FEED-02: well-formed XML with the expected structure.
    let xml = String::from_utf8(body).unwrap();
    let doc = roxmltree::Document::parse(&xml).expect("well-formed XML");
    let itunes = "http://www.itunes.com/dtds/podcast-1.0.dtd";
    let podcast = "https://podcastindex.org/namespace/1.0";
    let rss = doc.root_element();
    assert_eq!(
        (rss.tag_name().name(), rss.attribute("version")),
        ("rss", Some("2.0"))
    );
    let channel = rss.children().find(|n| n.has_tag_name("channel")).unwrap();
    // FR-FEED-03: private.
    assert_eq!(child(channel, Some(itunes), "block").text(), Some("Yes"));
    assert_eq!(child(channel, Some(podcast), "locked").text(), Some("yes"));
    assert_eq!(
        child(channel, Some(podcast), "guid").text().unwrap().len(),
        36
    );
    let artwork = child(channel, Some(itunes), "image")
        .attribute("href")
        .unwrap();
    assert!(artwork.ends_with("/artwork.png"));

    let items: Vec<_> = channel
        .children()
        .filter(|n| n.has_tag_name("item"))
        .collect();
    assert_eq!(items.len(), 1);
    let item = items[0];
    // Text is escaped, not interpreted.
    assert_eq!(
        child(item, None, "title").text(),
        Some("Locks & Levels <1>")
    );
    let guid = child(item, None, "guid");
    assert_eq!(guid.attribute("isPermaLink"), Some("false"));
    assert_eq!(guid.text(), Some(first.id.to_string().as_str()));
    assert_eq!(
        child(item, None, "link").text(),
        Some("https://example.org/locks")
    );
    assert!(
        child(item, None, "pubDate")
            .text()
            .unwrap()
            .ends_with("+0000")
    );
    let enclosure = child(item, None, "enclosure");
    assert_eq!(enclosure.attribute("type"), Some("audio/mpeg"));
    assert_eq!(enclosure.attribute("length"), Some("10000"));
    let mp3 = enclosure.attribute("url").unwrap();
    assert!(mp3.ends_with(&format!("/items/{}.mp3", first.id)));
    assert_eq!(child(item, Some(itunes), "duration").text(), Some("481"));

    // The enclosure and artwork URLs work.
    let (status, _, body) = get(&app, path(mp3), &[]).await;
    assert_eq!((status, body), (StatusCode::OK, episode_bytes()));
    let (status, headers, body) = get(&app, path(artwork), &[]).await;
    assert_eq!(
        (status, &headers[header::CONTENT_TYPE]),
        (StatusCode::OK, &"image/png".parse().unwrap())
    );
    assert!(body.starts_with(b"\x89PNG"));
}

// AC-FEED-04
#[tokio::test]
async fn unchanged_feeds_answer_304() {
    let app = app().await;
    published(&app, "owner", "https://example.org/a", "A").await;
    let url = new_feed_url(&app).await;
    let (_, headers, _) = get(&app, path(&url), &[]).await;
    let etag = headers[header::ETAG].to_str().unwrap().to_string();
    let (status, headers, body) = get(&app, path(&url), &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(headers[header::ETAG], etag.as_str());
    // A new episode changes the feed.
    published(&app, "owner", "https://example.org/b", "B").await;
    let (status, _, _) = get(&app, path(&url), &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::OK);
}

// FR-FEED-05, AC-FEED-03
#[tokio::test]
async fn rotating_the_token_kills_the_old_url_and_needs_auth() {
    let app = app().await;
    let old = new_feed_url(&app).await;
    let new = new_feed_url(&app).await;
    assert_ne!(old, new);
    assert_eq!(get(&app, path(&old), &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, path(&new), &[]).await.0, StatusCode::OK);

    let unauthenticated = app
        .send(Request::post("/v1/feed/token").body(Body::empty()).unwrap())
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    // Malformed and unknown tokens are plain 404s.
    for p in [
        "/feeds/nonsense.xml",
        "/feeds/rtm_feed_0000.xml",
        &format!("/feeds/rtm_feed_{}.xml", "0".repeat(64)),
        &path(&new).replace(".xml", ".rss"),
    ] {
        assert_eq!(get(&app, p, &[]).await.0, StatusCode::NOT_FOUND, "{p}");
    }
}

// FR-FEED-04, ADR-0009
#[tokio::test]
async fn enclosures_answer_range_requests() {
    let app = app().await;
    let item = published(&app, "owner", "https://example.org/a", "A").await;
    let feed = new_feed_url(&app).await;
    let mp3 = format!(
        "{}/items/{}.mp3",
        path(&feed).trim_end_matches(".xml"),
        item.id
    );
    let bytes = episode_bytes();

    let (status, headers, body) = get(&app, &mp3, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    assert_eq!(headers[header::CONTENT_LENGTH], "10000");
    assert_eq!(headers["x-robots-tag"], "noindex");
    assert!(headers.contains_key(header::ETAG) && headers.contains_key(header::LAST_MODIFIED));

    for (range, start, end) in [
        ("bytes=0-99", 0, 99),
        ("bytes=9900-", 9900, 9999),
        ("bytes=-50", 9950, 9999),
        ("bytes=5000-99999", 5000, 9999),
    ] {
        let (status, headers, body) = get(&app, &mp3, &[("range", range)]).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            headers[header::CONTENT_RANGE],
            format!("bytes {start}-{end}/10000").as_str()
        );
        assert_eq!(body, bytes[start..=end], "{range}");
    }
    for range in ["bytes=10000-", "bytes=500-100"] {
        let (status, headers, _) = get(&app, &mp3, &[("range", range)]).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(headers[header::CONTENT_RANGE], "bytes */10000");
    }
    // Multiple ranges get the whole file.
    let (status, _, body) = get(&app, &mp3, &[("range", "bytes=0-1,5-9")]).await;
    assert_eq!((status, body.len()), (StatusCode::OK, 10_000));
}

#[tokio::test]
async fn enclosures_belong_to_their_feed() {
    let app = app().await;
    let theirs = published(&app, "someone-else", "https://example.org/x", "X").await;
    let mine = published(&app, "owner", "https://example.org/a", "A").await;
    // Submitted after the others, so the publishing helper never picks it up.
    let pending = app
        .store
        .submit(&UserId("owner".into()), "https://example.org/p", 0)
        .await
        .unwrap();
    let base = path(&new_feed_url(&app).await)
        .trim_end_matches(".xml")
        .to_string();
    for id in [
        theirs.id.to_string(),
        pending.id.to_string(),
        "not-an-id".into(),
    ] {
        let (status, _, _) = get(&app, &format!("{base}/items/{id}.mp3"), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }
    // Published, but the audio is gone: 410 Gone.
    let key = mine.episode.unwrap().key;
    std::fs::remove_file(app.blobs.local_path(BlobKind::Episode, &key)).unwrap();
    let (status, _, _) = get(&app, &format!("{base}/items/{}.mp3", mine.id), &[]).await;
    assert_eq!(status, StatusCode::GONE);
}
