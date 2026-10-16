// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extract stage against scripted workers: `sh` scripts standing in for
//! `rtm extract-worker`, so the stage's handling of every kind of worker behaviour is
//! tested without the real extractors. (The real worker is tested in `rtm-bin`.)

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use rtm_article::Article;
use rtm_blobs::{BlobKind, BlobStore};
use rtm_domain::{FailureCode, Item, ItemState, Source, SourceKind, Stage, UserId};
use rtm_pipeline::{ExtractConfig, ExtractStage, Turn, Worker};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};
use rtm_store::{QueuePolicy, Store};

struct Setup {
    store: Arc<Store>,
    blobs: BlobStore,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
            .await
            .unwrap(),
    );
    let blobs = BlobStore::new(dir.path().join("blobs"));
    Setup {
        store,
        blobs,
        _dir: dir,
    }
}

fn owner() -> UserId {
    UserId("owner".into())
}

impl Setup {
    /// An item that has been fetched and is waiting for extraction.
    async fn fetched_item(&self) -> Item {
        self.store
            .submit(&owner(), "https://example.test/a", 0)
            .await
            .unwrap();
        let key = self
            .blobs
            .put_source(b"<p>page</p>".to_vec(), b"")
            .await
            .unwrap();
        let job = self
            .store
            .claim("f", &[Stage::Fetch], 1, 60_000)
            .await
            .unwrap()
            .unwrap();
        let source = Source {
            key,
            final_url: "https://example.test/a".into(),
            kind: SourceKind::Html,
            charset: None,
            size: 11,
        };
        self.store
            .complete(&job, "f", StageOutput::Source(source), 2)
            .await
            .unwrap()
    }

    fn worker(&self, script: &str, timeout: Duration) -> Worker<Store> {
        let stage = ExtractStage::new(
            "/bin/sh",
            vec![OsString::from("-c"), OsString::from(script)],
            self.blobs.clone(),
            ExtractConfig {
                timeout,
                max_output: 64 * 1024,
                min_words: 5,
                ..ExtractConfig::default()
            },
        );
        Worker::new("x", Arc::clone(&self.store), Arc::new(stage))
    }

    async fn reload(&self, item: &Item) -> Item {
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }
}

/// The absolute path of a tool, since workers run with no PATH.
fn tool(name: &str) -> String {
    std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|dir| std::path::Path::new(dir).join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not found"))
        .display()
        .to_string()
}

/// A worker script that reads its request and prints `response`.
fn replying(response: &str) -> String {
    let quoted = response.replace('\'', "'\\''");
    format!("{} >/dev/null; printf '%s' '{quoted}'", tool("cat"))
}

/// An extracted-article response whose first paragraph is `text`.
fn article(text: &str) -> String {
    serde_json::json!({
        "result": "extracted",
        "extractor": "generic-html",
        "version": 1,
        "derived_title": false,
        "article": {
            "title": "Canals",
            "byline": "Ada",
            "site_name": null,
            "published": null,
            "canonical_url": null,
            "lang": "en",
            "blocks": [
                {"type": "paragraph", "text": text},
                {"type": "paragraph", "text": "\u{0}"}
            ]
        }
    })
    .to_string()
}

/// Untidy text the server must clean: runs of spaces and a zero-width space.
const UNTIDY: &str = "Locks   raise boats\u{200B} up hills.";

#[tokio::test]
async fn a_valid_article_is_cleaned_stored_and_recorded() {
    let s = setup().await;
    let item = s.fetched_item().await;
    let turn = s
        .worker(&replying(&article(UNTIDY)), Duration::from_secs(10))
        .run_once()
        .await
        .unwrap();
    assert_eq!(turn, Turn::Completed(Stage::Extract));

    let item = s.reload(&item).await;
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Script
        }
    );
    let record = item.article.expect("article recorded");
    assert_eq!(record.extractor, "generic-html@1");
    assert_eq!(record.title, "Canals");
    // Counted by the server, after cleaning: "Canals" + "Locks raise boats up hills."
    assert_eq!(record.words, 6);
    assert_eq!(
        record.key,
        rtm_blobs::article_key(
            &item.source.unwrap().key,
            "generic-html@1",
            rtm_article::SCHEMA_VERSION
        )
    );

    let stored: Article =
        serde_json::from_slice(&s.blobs.get(BlobKind::Article, &record.key).await.unwrap())
            .unwrap();
    // Cleaned again here: whitespace collapsed, the zero-width space and the
    // NUL-only paragraph gone.
    assert_eq!(stored.blocks.len(), 1);
    assert_eq!(
        serde_json::to_value(&stored.blocks[0]).unwrap()["text"],
        "Locks raise boats up hills."
    );
}

// FR-EXT-05
#[tokio::test]
async fn low_confidence_fails_the_item_with_an_explanation() {
    let s = setup().await;
    let item = s.fetched_item().await;
    let reply = replying(
        r#"{"result":"low_confidence","best_words":42,"reasons":["generic-html: only 42 words"]}"#,
    );
    let turn = s
        .worker(&reply, Duration::from_secs(10))
        .run_once()
        .await
        .unwrap();
    assert_eq!(
        turn,
        Turn::Failed(Stage::Extract, FailureCode::ExtractionLowConfidence)
    );
    let message = s.reload(&item).await.failure_message.unwrap();
    assert!(message.contains("42 words"), "{message}");
}

#[tokio::test]
async fn unsupported_documents_fail_the_item() {
    let s = setup().await;
    s.fetched_item().await;
    let turn = s
        .worker(
            &replying(r#"{"result":"unsupported"}"#),
            Duration::from_secs(10),
        )
        .run_once()
        .await
        .unwrap();
    assert_eq!(
        turn,
        Turn::Failed(Stage::Extract, FailureCode::UnsupportedContentType)
    );
}

/// A misbehaving worker never takes the stage down: each case is an internal error,
/// retried with backoff, and the item is left waiting on extraction.
#[tokio::test]
async fn misbehaving_workers_are_retried() {
    for (case, script, timeout) in [
        ("garbage", replying("this is not json"), 10),
        (
            "an article that fails validation",
            replying(&article(UNTIDY).replace(
                r#""blocks":["#,
                r#""blocks":[{"type":"heading","level":9,"text":"x"},"#,
            )),
            10,
        ),
        (
            "too short after cleaning",
            replying(&article("Two words")),
            10,
        ),
        (
            "a crash",
            format!("{} >/dev/null; echo boom >&2; exit 3", tool("cat")),
            10,
        ),
        ("death by signal", "kill -9 $$".into(), 10),
        ("a hang", format!("{} 30", tool("sleep")), 1),
        (
            "oversized output",
            format!(
                "{} >/dev/null; {} -c 200000 /dev/zero",
                tool("cat"),
                tool("head")
            ),
            10,
        ),
        (
            "output that never ends",
            format!("{} >/dev/null; {}", tool("cat"), tool("yes")),
            2,
        ),
        (
            "unknown fields",
            replying(r#"{"result":"unsupported","extra":1}"#),
            10,
        ),
    ] {
        let s = setup().await;
        let item = s.fetched_item().await;
        let started = std::time::Instant::now();
        let turn = s
            .worker(&script, Duration::from_secs(timeout))
            .run_once()
            .await
            .unwrap();
        assert_eq!(
            turn,
            Turn::Retrying(Stage::Extract, FailureCode::Internal),
            "{case}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(timeout + 3),
            "{case} overran its timeout"
        );
        let item = s.reload(&item).await;
        assert_eq!(
            item.state,
            ItemState::Pending {
                next: Stage::Extract
            },
            "{case}"
        );
        assert_eq!(item.article, None, "{case}");
    }
}

/// The worker gets no environment: no tokens, no paths.
#[tokio::test]
async fn the_worker_environment_is_empty() {
    let s = setup().await;
    s.fetched_item().await;
    // Unsupported only if variables from this test's environment are absent (the shell
    // invents a default PATH, so that can't be checked here); garbage otherwise.
    let script = format!(
        r#"{} >/dev/null; if [ -z "$HOME" ] && [ -z "$CARGO_MANIFEST_DIR" ] && [ -z "$USER" ]; then printf '%s' '{{"result":"unsupported"}}'; else echo leaked; fi"#,
        tool("cat")
    );
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "precondition"
    );
    let turn = s
        .worker(&script, Duration::from_secs(10))
        .run_once()
        .await
        .unwrap();
    assert_eq!(
        turn,
        Turn::Failed(Stage::Extract, FailureCode::UnsupportedContentType)
    );
}

#[tokio::test]
async fn the_request_carries_the_source_and_its_details() {
    let s = setup().await;
    s.fetched_item().await;
    // Unsupported only if the request mentions the final URL and ends with the page.
    let script = r#"req=$(CAT); case "$req" in *'"final_url":"https://example.test/a"'*'<p>page</p>') printf '%s' '{"result":"unsupported"}';; *) echo bad;; esac"#;
    let script = script.replace("CAT", &tool("cat"));
    let turn = s
        .worker(&script, Duration::from_secs(10))
        .run_once()
        .await
        .unwrap();
    assert_eq!(
        turn,
        Turn::Failed(Stage::Extract, FailureCode::UnsupportedContentType)
    );
}
