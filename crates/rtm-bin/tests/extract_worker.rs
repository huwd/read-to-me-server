// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `rtm extract-worker`: its protocol, its sandbox limits, and the extract
//! stage running it.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use rtm_blobs::{BlobKind, BlobStore};
use rtm_domain::{ItemState, Source, SourceKind, Stage, UserId};
use rtm_extract::protocol::{RequestHeader, Response, SourceKindWire, write_request};
use rtm_pipeline::{ExtractConfig, ExtractStage, Turn, Worker};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};
use rtm_store::{QueuePolicy, Store};

const RTM: &str = env!("CARGO_BIN_EXE_rtm");

fn fixture() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rtm-extract/tests/fixtures/structured.html"),
    )
    .unwrap()
}

fn header() -> RequestHeader {
    RequestHeader {
        kind: SourceKindWire::Html,
        charset: Some("utf-8".into()),
        final_url: Some("https://waterways.example/canals".into()),
        min_words: 200,
    }
}

fn run_worker(request: &[u8]) -> (std::process::ExitStatus, Vec<u8>, String) {
    let mut child = Command::new(RTM)
        .arg("extract-worker")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(request).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status,
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_worker_extracts_over_stdin_and_stdout() {
    let mut request = Vec::new();
    write_request(&mut request, &header(), &fixture()).unwrap();
    let (status, stdout, stderr) = run_worker(&request);
    assert!(status.success(), "{status}: {stderr}");
    let Response::Extracted {
        article,
        extractor,
        version,
        ..
    } = serde_json::from_slice(&stdout).unwrap()
    else {
        panic!("expected an article: {}", String::from_utf8_lossy(&stdout));
    };
    assert_eq!((extractor.as_str(), version), ("generic-html", 1));
    assert_eq!(article.title, "The Quiet Engineering of Canals");
    // Nothing but the response on stdout: no logs.
    assert!(stdout.starts_with(b"{\"result\""));
}

#[test]
fn a_bad_request_fails_cleanly() {
    let (status, stdout, stderr) = run_worker(b"\xff\xff\xff\xffnot a request");
    assert_eq!(status.code(), Some(65));
    assert!(stdout.is_empty());
    assert!(stderr.contains("bad request"), "{stderr}");
}

/// The worker limits itself before reading anything (ADR-0006).
#[cfg(target_os = "linux")]
#[test]
fn the_worker_runs_under_resource_limits() {
    let mut child = Command::new(RTM)
        .arg("extract-worker")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The worker is now blocked reading stdin, with its limits applied.
    let limits = (0..50)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let text = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).ok()?;
            text.lines()
                .any(|l| l.starts_with("Max open files") && l.contains(" 16 "))
                .then_some(text)
        })
        .expect("limits never applied");
    drop(child.stdin.take());
    let _ = child.wait();

    let limit = |name: &str| -> String {
        let line = limits.lines().find(|l| l.starts_with(name)).unwrap();
        line[name.len()..]
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(limit("Max address space"), "536870912 536870912");
    assert_eq!(limit("Max cpu time"), "20 21");
    assert_eq!(limit("Max file size"), "0 0");
    assert_eq!(limit("Max core file size"), "0 0");
    assert_eq!(limit("Max open files"), "16 16");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("bad request"), "{stderr}");
}

/// The extract stage, start to finish, with the real worker.
#[tokio::test]
async fn the_extract_stage_runs_the_real_worker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
            .await
            .unwrap(),
    );
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let owner = UserId("owner".into());
    let item = store
        .submit(&owner, "https://waterways.example/canals", 0)
        .await
        .unwrap();
    let page = fixture();
    let size = page.len() as u64;
    let key = blobs.put_source(page, b"").await.unwrap();
    let job = store
        .claim("f", &[Stage::Fetch], 1, 60_000)
        .await
        .unwrap()
        .unwrap();
    let source = Source {
        key,
        final_url: "https://waterways.example/canals".into(),
        kind: SourceKind::Html,
        charset: Some("utf-8".into()),
        size,
    };
    store
        .complete(&job, "f", StageOutput::Source(source), 2)
        .await
        .unwrap();

    let stage = ExtractStage::new(
        RTM,
        vec![OsString::from("extract-worker")],
        blobs.clone(),
        ExtractConfig::default(),
    );
    let worker = Worker::new("x", Arc::clone(&store), Arc::new(stage));
    assert_eq!(
        worker.run_once().await.unwrap(),
        Turn::Completed(Stage::Extract)
    );

    let item = store.get(&owner, item.id).await.unwrap().unwrap();
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Script
        }
    );
    let record = item.article.unwrap();
    assert_eq!(record.extractor, "generic-html@1");
    assert_eq!(record.title, "The Quiet Engineering of Canals");
    assert!(record.words > 250, "{}", record.words);
    let stored: rtm_article::Article =
        serde_json::from_slice(&blobs.get(BlobKind::Article, &record.key).await.unwrap()).unwrap();
    assert_eq!(stored.title, record.title);
    assert_eq!(stored.word_count() as u64, record.words);
}

/// Extract `bytes` as fetched from `url` with `charset`, returning the item's record
/// and the stored article.
async fn extract_as(
    bytes: &[u8],
    url: &str,
    kind: SourceKind,
    charset: Option<&str>,
    store: &Arc<Store>,
    blobs: &BlobStore,
) -> (rtm_domain::ArticleRecord, rtm_article::Article) {
    let owner = UserId("owner".into());
    let item = store.submit(&owner, url, 0).await.unwrap();
    let key = blobs.put_source(bytes.to_vec(), b"").await.unwrap();
    let job = store
        .claim("f", &[Stage::Fetch], 1, 60_000)
        .await
        .unwrap()
        .unwrap();
    let source = Source {
        key,
        final_url: url.into(),
        kind,
        charset: charset.map(Into::into),
        size: bytes.len() as u64,
    };
    store
        .complete(&job, "f", StageOutput::Source(source), 2)
        .await
        .unwrap();
    let stage = ExtractStage::new(
        RTM,
        vec![OsString::from("extract-worker")],
        blobs.clone(),
        ExtractConfig::default(),
    );
    let worker = Worker::new("x", Arc::clone(store), Arc::new(stage));
    assert_eq!(
        worker.run_once().await.unwrap(),
        Turn::Completed(Stage::Extract)
    );
    let record = store
        .get(&owner, item.id)
        .await
        .unwrap()
        .unwrap()
        .article
        .unwrap();
    let stored =
        serde_json::from_slice(&blobs.get(BlobKind::Article, &record.key).await.unwrap()).unwrap();
    (record, stored)
}

/// Codex's reproduction on #36: identical bytes extracted under different source
/// details must not share an article, or an item would record one extraction while
/// pointing at another's.
#[tokio::test]
async fn identical_bytes_with_different_sources_get_their_own_articles() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
            .await
            .unwrap(),
    );
    let blobs = BlobStore::new(dir.path().join("blobs"));

    // The same untitled text at two URLs: titles are derived from each URL.
    let text = "word ".repeat(250).into_bytes();
    let mut keys = Vec::new();
    for (url, title) in [
        (
            "https://example.test/still-water-history",
            "Still water history",
        ),
        (
            "https://example.test/completely-different-title",
            "Completely different title",
        ),
    ] {
        let (record, stored) =
            extract_as(&text, url, SourceKind::PlainText, None, &store, &blobs).await;
        assert_eq!(record.title, title);
        assert_eq!(
            stored.title, title,
            "stored article belongs to another item"
        );
        keys.push(record.key);
    }

    // The same bytes under two declared charsets decode differently.
    let mut page = b"Cafe notes\n\n".to_vec();
    for _ in 0..60 {
        page.extend_from_slice(b"The caf\xe9 owner talks about the trade. ");
    }
    for (charset, expect) in [
        (Some("windows-1252"), "café"),
        (Some("utf-8"), "caf\u{FFFD}"),
    ] {
        let (record, stored) = extract_as(
            &page,
            "https://example.test/cafe-notes",
            SourceKind::PlainText,
            charset,
            &store,
            &blobs,
        )
        .await;
        let body = serde_json::to_string(&stored.blocks).unwrap();
        assert!(body.contains(expect), "{charset:?}: {body}");
        keys.push(record.key);
    }

    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 4, "articles shared a key");
}
