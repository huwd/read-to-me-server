// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lease heartbeats: a stage longer than its lease keeps the job, and a stage whose
//! lease is lost is abandoned rather than finished.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rtm_domain::{BlobKey, FailureCode, Item, ItemId, Millis, Source, SourceKind, Stage, UserId};
use rtm_pipeline::{StageFuture, StageRunner, Turn, Worker};
use rtm_ports::{FailOutcome, ItemRepository, Job, JobQueue, StageOutput, StoreError, StoreResult};
use rtm_store::{QueuePolicy, Store};

/// A fetch stage that takes `takes` and records when it starts and whether it ran to
/// the end.
struct Slow {
    lease: Duration,
    takes: Duration,
    started: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl StageRunner for Slow {
    fn stage(&self) -> Stage {
        Stage::Fetch
    }
    fn lease(&self) -> Duration {
        self.lease
    }
    fn run<'a>(&'a self, _: &'a Item) -> StageFuture<'a> {
        Box::pin(async move {
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(self.takes).await;
            self.finished.store(true, Ordering::SeqCst);
            Ok(StageOutput::Source(Source {
                key: BlobKey::from_hash([1; 32]),
                final_url: "https://example.test/".into(),
                kind: SourceKind::Html,
                charset: None,
                size: 1,
            }))
        })
    }
}

async fn store() -> (Arc<Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
        .await
        .unwrap();
    store
        .submit(&UserId("owner".into()), "https://example.test/", 0)
        .await
        .unwrap();
    (Arc::new(store), dir)
}

#[tokio::test]
async fn a_stage_longer_than_its_lease_keeps_its_job() {
    let (store, _dir) = store().await;
    let (started, finished) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let stage = Slow {
        lease: Duration::from_millis(300),
        takes: Duration::from_millis(1_000),
        started: Arc::clone(&started),
        finished: Arc::clone(&finished),
    };
    let worker = Worker::new("w1", Arc::clone(&store), Arc::new(stage));
    let running = tokio::spawn(async move { worker.run_once().await.unwrap() });
    while !started.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Another worker keeps trying to take the job; the heartbeats must stop it.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(900) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as Millis;
        let stolen = store.claim("w2", &[Stage::Fetch], now, 300).await.unwrap();
        assert!(
            stolen.is_none(),
            "the job was taken while its stage was running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(running.await.unwrap(), Turn::Completed(Stage::Fetch));
    assert!(finished.load(Ordering::SeqCst));
}

/// A store whose heartbeats report the lease as lost, as if another worker had taken
/// the job after this one stalled.
struct Losing(Arc<Store>);

impl ItemRepository for Losing {
    async fn submit(&self, user: &UserId, url: &str, now: Millis) -> StoreResult<Item> {
        self.0.submit(user, url, now).await
    }
    async fn get(&self, user: &UserId, id: ItemId) -> StoreResult<Option<Item>> {
        self.0.get(user, id).await
    }
}

impl JobQueue for Losing {
    async fn claim(
        &self,
        worker: &str,
        stages: &[Stage],
        now: Millis,
        lease_ms: Millis,
    ) -> StoreResult<Option<Job>> {
        self.0.claim(worker, stages, now, lease_ms).await
    }
    async fn heartbeat(&self, _: &Job, _: &str, _: Millis, _: Millis) -> StoreResult<Millis> {
        Err(StoreError::LeaseLost)
    }
    async fn complete(
        &self,
        job: &Job,
        worker: &str,
        output: StageOutput,
        now: Millis,
    ) -> StoreResult<Item> {
        self.0.complete(job, worker, output, now).await
    }
    async fn fail(
        &self,
        job: &Job,
        worker: &str,
        code: FailureCode,
        message: &str,
        now: Millis,
    ) -> StoreResult<FailOutcome> {
        self.0.fail(job, worker, code, message, now).await
    }
}

#[tokio::test]
async fn a_lost_lease_abandons_the_stage() {
    let (store, _dir) = store().await;
    let finished = Arc::new(AtomicBool::new(false));
    let stage = Slow {
        lease: Duration::from_millis(300),
        takes: Duration::from_secs(5),
        started: Arc::new(AtomicBool::new(false)),
        finished: Arc::clone(&finished),
    };
    let worker = Worker::new("w1", Arc::new(Losing(store)), Arc::new(stage));
    let started = Instant::now();
    assert_eq!(worker.run_once().await.unwrap(), Turn::Abandoned);
    // Abandoned at the first heartbeat, about a third of the lease in, not after the
    // stage's five seconds.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(!finished.load(Ordering::SeqCst));
}
