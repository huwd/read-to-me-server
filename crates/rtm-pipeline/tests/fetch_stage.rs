// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fetch stage end to end: a real store, blob store and egress guard, against a
//! local site. The site listens on 127.0.0.1, which the guard refuses by default, so
//! these workers opt in for exactly that address and port.

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::routing::get;
use rtm_blobs::{BlobKind, BlobStore};
use rtm_domain::{FailureCode, ItemState, SourceKind, Stage, UserId};
use rtm_fetch::{Config, Lookup, Policy, SafeFetcher};
use rtm_pipeline::{FetchStage, Turn, Worker};
use rtm_ports::ItemRepository;
use rtm_store::{QueuePolicy, Store};
use tokio::net::TcpListener;

const ARTICLE: &str = "<!doctype html><title>Essay</title><p>Words worth hearing.</p>";

async fn start_site() -> u16 {
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /members/\n" }),
        )
        .route(
            "/essay",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    ARTICLE,
                )
            }),
        )
        .route(
            "/members/essay",
            get(|| async { ([(header::CONTENT_TYPE, "text/html")], ARTICLE) }),
        )
        .route("/flaky", get(|| async { StatusCode::SERVICE_UNAVAILABLE }))
        .route("/gone", get(|| async { StatusCode::GONE }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// Every name resolves to the local site.
struct Local;

impl Lookup for Local {
    fn lookup<'a>(
        &'a self,
        _host: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>,
    > {
        Box::pin(async { Ok(vec!["127.0.0.1".parse().unwrap()]) })
    }
}

struct Setup {
    store: Arc<Store>,
    blobs: BlobStore,
    worker: Worker<Store>,
    port: u16,
    _dir: tempfile::TempDir,
}

async fn setup(ignore_robots: bool) -> Setup {
    let port = start_site().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
            .await
            .unwrap(),
    );
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let config = Config {
        policy: Policy::default()
            .unsafe_allow(["127.0.0.1/32".parse().unwrap()])
            .allow_ports([port]),
        ignore_robots,
        ..Config::default()
    };
    let fetcher = SafeFetcher::with_lookup(config, Arc::new(Local));
    let stage = Arc::new(FetchStage::new(fetcher, blobs.clone()));
    let worker = Worker::new("w1", Arc::clone(&store), stage);
    Setup {
        store,
        blobs,
        worker,
        port,
        _dir: dir,
    }
}

fn owner() -> UserId {
    UserId("owner".into())
}

impl Setup {
    async fn submit(&self, url: &str) -> rtm_domain::Item {
        let url = url.replace("PORT", &self.port.to_string());
        self.store.submit(&owner(), &url, 0).await.unwrap()
    }

    async fn reload(&self, item: &rtm_domain::Item) -> rtm_domain::Item {
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }
}

#[tokio::test]
async fn fetching_stores_the_source_and_moves_on_to_extraction() {
    let s = setup(false).await;
    let item = s.submit("http://news.test:PORT/essay").await;

    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Completed(Stage::Fetch)
    );

    let item = s.reload(&item).await;
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Extract
        }
    );
    let source = item.source.expect("source recorded");
    assert_eq!(source.kind, SourceKind::Html);
    assert_eq!(source.charset.as_deref(), Some("utf-8"));
    assert_eq!(source.size, ARTICLE.len() as u64);
    assert_eq!(
        source.final_url,
        format!("http://news.test:{}/essay", s.port)
    );
    let stored = s.blobs.get(BlobKind::Source, &source.key).await.unwrap();
    assert_eq!(stored, ARTICLE.as_bytes());
    assert_eq!(source.key, rtm_blobs::source_key(ARTICLE.as_bytes(), b""));

    // The fetch worker doesn't pick up the extract job.
    assert_eq!(s.worker.run_once().await.unwrap(), Turn::Idle);
}

// AC-FET-01
#[tokio::test]
async fn internal_destinations_fail_the_item() {
    let s = setup(false).await;
    let item = s.submit("http://10.0.0.5/").await;
    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Failed(Stage::Fetch, FailureCode::DestinationNotAllowed)
    );
    let item = s.reload(&item).await;
    assert!(matches!(
        item.state,
        ItemState::Failed {
            code: FailureCode::DestinationNotAllowed,
            ..
        }
    ));
    // The user sees a plain message, not the address or reason.
    let message = item.failure_message.unwrap();
    assert!(!message.contains("10.0.0.5"), "{message}");
}

// AC-FET-04
#[tokio::test]
async fn robots_disallow_fails_the_item_with_advice() {
    let s = setup(false).await;
    let item = s.submit("http://news.test:PORT/members/essay").await;
    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Failed(Stage::Fetch, FailureCode::RobotsDisallowed)
    );
    let item = s.reload(&item).await;
    assert!(item.failure_message.unwrap().contains("upload"));
    assert_eq!(item.source, None);
}

// AC-FET-05
#[tokio::test]
async fn disrespectful_mode_fetches_disallowed_pages() {
    let s = setup(true).await;
    let item = s.submit("http://news.test:PORT/members/essay").await;
    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Completed(Stage::Fetch)
    );
    assert!(s.reload(&item).await.source.is_some());
}

#[tokio::test]
async fn transient_failures_are_retried_and_permanent_ones_are_not() {
    let s = setup(false).await;
    let flaky = s.submit("http://news.test:PORT/flaky").await;
    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Retrying(Stage::Fetch, FailureCode::FetchFailed)
    );
    assert_eq!(
        s.reload(&flaky).await.state,
        ItemState::Pending { next: Stage::Fetch }
    );
    // Backing off: not runnable straight away.
    assert_eq!(s.worker.run_once().await.unwrap(), Turn::Idle);

    let gone = s.submit("http://news.test:PORT/gone").await;
    assert_eq!(
        s.worker.run_once().await.unwrap(),
        Turn::Failed(Stage::Fetch, FailureCode::SourceUnavailable)
    );
    assert!(matches!(
        s.reload(&gone).await.state,
        ItemState::Failed {
            code: FailureCode::SourceUnavailable,
            ..
        }
    ));
}

#[tokio::test]
async fn the_worker_loop_runs_jobs_and_stops_when_asked() {
    let s = setup(false).await;
    let item = s.submit("http://news.test:PORT/essay").await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let Setup { store, worker, .. } = s;
    let running = tokio::spawn(worker.run(stopped));

    // Wait for the fetch to land.
    let mut fetched = false;
    for _ in 0..100 {
        let current = store.get(&owner(), item.id).await.unwrap().unwrap();
        if current.source.is_some() {
            fetched = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(fetched, "the worker never fetched the item");

    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), running)
        .await
        .expect("the worker didn't stop")
        .unwrap();
}
