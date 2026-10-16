// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store and queue against real SQLite databases in temporary directories.

use std::collections::HashSet;
use std::sync::Arc;

use rtm_domain::{FailureCode, ItemState, Stage, UserId};
use rtm_ports::{FailOutcome, ItemRepository, JobQueue, StageOutput, StoreError};
use rtm_store::{QueuePolicy, Store};

/// A plausible output for completing `job`: fetch records a source, extract an
/// article, script a script,
/// synthesise a speech plan; later stages record nothing yet.
fn output_for(job: &rtm_ports::Job) -> rtm_ports::StageOutput {
    match job.stage {
        rtm_domain::Stage::Fetch => rtm_ports::StageOutput::Source(rtm_domain::Source {
            key: rtm_domain::BlobKey::from_hash([7; 32]),
            final_url: "https://example.org/final".into(),
            kind: rtm_domain::SourceKind::Html,
            charset: Some("utf-8".into()),
            size: 42,
        }),
        rtm_domain::Stage::Extract => rtm_ports::StageOutput::Article(rtm_domain::ArticleRecord {
            key: rtm_domain::BlobKey::from_hash([8; 32]),
            extractor: "generic-html@1".into(),
            title: "A title".into(),
            title_derived: false,
            words: 1234,
        }),
        rtm_domain::Stage::Script => rtm_ports::StageOutput::Script(rtm_domain::ScriptRecord {
            key: rtm_domain::BlobKey::from_hash([9; 32]),
            words: 1240,
        }),
        rtm_domain::Stage::Synthesise => rtm_ports::StageOutput::Speech(rtm_domain::SpeechRecord {
            plan_key: rtm_domain::BlobKey::from_hash([10; 32]),
            duration_ms: 480_000,
            segments: 12,
        }),
        _ => rtm_ports::StageOutput::Nothing,
    }
}

const LEASE: i64 = 60_000;

struct Db {
    store: Store,
    _dir: tempfile::TempDir,
}

async fn db_with(policy: QueuePolicy) -> Db {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("rtm.db"), policy)
        .await
        .unwrap();
    Db { store, _dir: dir }
}

async fn db() -> Db {
    db_with(QueuePolicy::default()).await
}

fn user(name: &str) -> UserId {
    UserId(name.to_string())
}

#[tokio::test]
async fn submit_queues_fetch_and_items_are_private() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 1_000)
        .await
        .unwrap();
    assert_eq!(item.state, ItemState::Pending { next: Stage::Fetch });

    let got = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(got, item);
    assert!(db.store.get(&user("bob"), item.id).await.unwrap().is_none());

    let job = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 1_000, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (job.item, job.stage, job.attempt),
        (item.id, Stage::Fetch, 1)
    );
}

#[tokio::test]
async fn completing_every_stage_publishes_the_item() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let mut now = 0;
    for stage in Stage::ALL {
        now += 10;
        let job = db
            .store
            .claim("w1", &rtm_domain::Stage::ALL, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.stage, stage);
        let after = db
            .store
            .complete(&job, "w1", output_for(&job), now)
            .await
            .unwrap();
        assert_eq!(after.updated_at, now);
    }
    let done = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(done.state, ItemState::Published);
    assert!(
        db.store
            .claim("w1", &rtm_domain::Stage::ALL, now, LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_leased_job_is_not_claimed_twice_until_the_lease_expires() {
    let db = db().await;
    db.store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let first = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.store
            .claim("w2", &rtm_domain::Stage::ALL, LEASE - 1, LEASE)
            .await
            .unwrap()
            .is_none()
    );

    // w1 crashed: after the lease expires, w2 gets the job, as attempt 2.
    let second = db
        .store
        .claim("w2", &rtm_domain::Stage::ALL, LEASE, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((second.id, second.attempt), (first.id, 2));

    // w1's late result is rejected; w2's is accepted.
    assert!(matches!(
        db.store
            .complete(&first, "w1", output_for(&first), LEASE + 1)
            .await,
        Err(StoreError::LeaseLost)
    ));
    db.store
        .complete(&second, "w2", output_for(&second), LEASE + 1)
        .await
        .unwrap();
}

#[tokio::test]
async fn heartbeat_extends_a_held_lease_only() {
    let db = db().await;
    db.store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let job = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();

    let until = db
        .store
        .heartbeat(&job, "w1", LEASE - 1, LEASE)
        .await
        .unwrap();
    assert_eq!(until, 2 * LEASE - 1);
    assert!(
        db.store
            .claim("w2", &rtm_domain::Stage::ALL, LEASE + 1, LEASE)
            .await
            .unwrap()
            .is_none()
    );

    assert!(matches!(
        db.store.heartbeat(&job, "w2", LEASE + 1, LEASE).await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store.heartbeat(&job, "w1", 2 * LEASE, LEASE).await,
        Err(StoreError::LeaseLost)
    ));
}

#[tokio::test]
async fn retryable_failures_back_off_then_fail_the_item() {
    let policy = QueuePolicy {
        max_attempts: 3,
        base_backoff_ms: 1_000,
        max_backoff_ms: 60_000,
    };
    let db = db_with(policy).await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();

    let mut now = 0;
    for attempt in 1..=2 {
        let job = db
            .store
            .claim("w1", &rtm_domain::Stage::ALL, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.attempt, attempt);
        let outcome = db
            .store
            .fail(&job, "w1", FailureCode::FetchFailed, "timed out", now)
            .await
            .unwrap();
        let FailOutcome::Retrying { run_after } = outcome else {
            panic!("expected a retry, got {outcome:?}");
        };
        let delay = run_after - now;
        let full = 1_000 << (attempt - 1);
        assert!(
            (full / 2..=full).contains(&delay),
            "attempt {attempt}: {delay}"
        );

        // Not runnable before the backoff ends.
        assert!(
            db.store
                .claim("w1", &rtm_domain::Stage::ALL, run_after - 1, LEASE)
                .await
                .unwrap()
                .is_none()
        );
        now = run_after;
    }

    let job = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, now, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.attempt, 3);
    let outcome = db
        .store
        .fail(&job, "w1", FailureCode::FetchFailed, "timed out", now)
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Failed);

    let failed = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(
        failed.state,
        ItemState::Failed {
            stage: Stage::Fetch,
            code: FailureCode::FetchFailed
        }
    );
    assert_eq!(failed.failure_message.as_deref(), Some("timed out"));
    assert!(
        db.store
            .claim("w1", &rtm_domain::Stage::ALL, now + 1_000_000, LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn terminal_failures_fail_the_item_at_once() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "http://10.0.0.1/", 0)
        .await
        .unwrap();
    let job = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();
    let outcome = db
        .store
        .fail(
            &job,
            "w1",
            FailureCode::DestinationNotAllowed,
            "private address",
            1,
        )
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Failed);
    let failed = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert!(matches!(
        failed.state,
        ItemState::Failed {
            code: FailureCode::DestinationNotAllowed,
            ..
        }
    ));
}

#[tokio::test]
async fn a_job_that_keeps_crashing_its_worker_fails_the_item() {
    let policy = QueuePolicy {
        max_attempts: 2,
        ..QueuePolicy::default()
    };
    let db = db_with(policy).await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();

    // Two leases expire without a result.
    db.store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();
    db.store
        .claim("w2", &rtm_domain::Stage::ALL, LEASE, LEASE)
        .await
        .unwrap()
        .unwrap();
    // The third claim finds it over the limit and fails the item instead.
    assert!(
        db.store
            .claim("w3", &rtm_domain::Stage::ALL, 2 * LEASE, LEASE)
            .await
            .unwrap()
            .is_none()
    );

    let failed = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(
        failed.state,
        ItemState::Failed {
            stage: Stage::Fetch,
            code: FailureCode::Internal
        }
    );
}

#[tokio::test]
async fn users_take_turns() {
    let db = db().await;
    for i in 0..3 {
        db.store
            .submit(&user("ann"), &format!("https://example.org/{i}"), i)
            .await
            .unwrap();
    }
    db.store
        .submit(&user("bob"), "https://example.org/b", 10)
        .await
        .unwrap();

    // Ann submitted first, but Bob is served before Ann's second job.
    let mut order = Vec::new();
    for now in 100..104 {
        let job = db
            .store
            .claim("w1", &rtm_domain::Stage::ALL, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        order.push(job.user.0);
    }
    assert_eq!(order, ["ann", "bob", "ann", "ann"]);
}

#[tokio::test]
async fn concurrent_claims_never_hand_out_the_same_job() {
    let db = db().await;
    for i in 0..50 {
        let who = user(["ann", "bob", "cat"][i % 3]);
        db.store
            .submit(&who, &format!("https://example.org/{i}"), 0)
            .await
            .unwrap();
    }
    let store = Arc::new(db.store);
    let mut tasks = Vec::new();
    for w in 0..8 {
        let store = Arc::clone(&store);
        tasks.push(tokio::spawn(async move {
            let worker = format!("w{w}");
            let mut mine = Vec::new();
            while let Some(job) = store
                .claim(&worker, &rtm_domain::Stage::ALL, 1, LEASE)
                .await
                .unwrap()
            {
                mine.push(job.id);
            }
            mine
        }));
    }
    let mut seen = HashSet::new();
    for t in tasks {
        for id in t.await.unwrap() {
            assert!(seen.insert(id), "job {id} claimed twice");
        }
    }
    assert_eq!(seen.len(), 50);
}

#[tokio::test]
async fn data_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rtm.db");
    let item = {
        let store = Store::open(&path, QueuePolicy::default()).await.unwrap();
        store
            .submit(&user("ann"), "https://example.org/a", 0)
            .await
            .unwrap()
    };
    let store = Store::open(&path, QueuePolicy::default()).await.unwrap();
    assert_eq!(store.get(&user("ann"), item.id).await.unwrap(), Some(item));
    assert!(
        store
            .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn backoff_grows_and_is_capped() {
    let policy = QueuePolicy {
        max_attempts: 10,
        base_backoff_ms: 1_000,
        max_backoff_ms: 8_000,
    };
    for seed in [0, 1, 999, u64::MAX] {
        for (attempt, full) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (9, 8_000)] {
            let d = policy.backoff(attempt, seed);
            assert!(
                (full / 2..=full).contains(&d),
                "attempt {attempt} seed {seed}: {d}"
            );
        }
    }
}

/// A worker can reclaim its own job after its lease expired (it was slow, not dead). Its
/// first attempt must then be fenced off from the second.
#[tokio::test]
async fn a_stale_attempt_cannot_touch_the_same_workers_new_lease() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let first = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();
    let second = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, LEASE, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.id, first.attempt, second.attempt), (second.id, 1, 2));

    let now = LEASE + 1;
    assert!(matches!(
        db.store.heartbeat(&first, "w1", now, LEASE).await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store
            .fail(&first, "w1", FailureCode::FetchFailed, "stale", now)
            .await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store
            .fail(
                &first,
                "w1",
                FailureCode::DestinationNotAllowed,
                "stale",
                now
            )
            .await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store
            .complete(&first, "w1", output_for(&first), now)
            .await,
        Err(StoreError::LeaseLost)
    ));

    // The live attempt is untouched: still leased until its original expiry.
    assert!(
        db.store
            .claim("w2", &rtm_domain::Stage::ALL, 2 * LEASE - 1, LEASE)
            .await
            .unwrap()
            .is_none()
    );
    let after = db
        .store
        .complete(&second, "w1", output_for(&second), now)
        .await
        .unwrap();
    assert_eq!(after.id, item.id);
    assert_eq!(
        after.state,
        ItemState::Pending {
            next: Stage::Extract
        }
    );
}

/// SQLite reuses the highest rowid after a delete, so the next stage's job often gets
/// the ID of the job just completed. A late call holding the old job must not act on it.
#[tokio::test]
async fn a_stale_job_cannot_touch_a_new_job_that_reuses_its_id() {
    let db = db().await;
    db.store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let fetch = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 0, LEASE)
        .await
        .unwrap()
        .unwrap();
    db.store
        .complete(&fetch, "w1", output_for(&fetch), 1)
        .await
        .unwrap();
    let extract = db
        .store
        .claim("w1", &rtm_domain::Stage::ALL, 2, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(extract.stage, Stage::Extract);
    assert_eq!(
        extract.id, fetch.id,
        "precondition: SQLite reused the job ID"
    );
    assert_eq!(extract.attempt, fetch.attempt);

    assert!(matches!(
        db.store.heartbeat(&fetch, "w1", 3, LEASE).await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store
            .fail(&fetch, "w1", FailureCode::FetchFailed, "stale", 3)
            .await,
        Err(StoreError::LeaseLost)
    ));
    assert!(matches!(
        db.store.complete(&fetch, "w1", output_for(&fetch), 3).await,
        Err(StoreError::LeaseLost)
    ));

    // The extract job kept its lease (no retry, no extension) and completes normally.
    assert!(
        db.store
            .claim("w2", &rtm_domain::Stage::ALL, LEASE + 1, LEASE)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.store.heartbeat(&extract, "w1", 4, LEASE).await.unwrap(),
        4 + LEASE
    );
    db.store
        .complete(&extract, "w1", output_for(&extract), 5)
        .await
        .unwrap();
}

#[tokio::test]
async fn users_take_turns_even_within_one_millisecond() {
    let db = db().await;
    for i in 0..3 {
        db.store
            .submit(&user("ann"), &format!("https://example.org/a{i}"), i)
            .await
            .unwrap();
    }
    for i in 0..3 {
        db.store
            .submit(&user("bob"), &format!("https://example.org/b{i}"), 10 + i)
            .await
            .unwrap();
    }
    let mut order = Vec::new();
    for _ in 0..6 {
        let job = db
            .store
            .claim("w1", &rtm_domain::Stage::ALL, 100, LEASE)
            .await
            .unwrap()
            .unwrap();
        order.push(job.user.0);
    }
    assert_eq!(order, ["ann", "bob", "ann", "bob", "ann", "bob"]);
}

/// NFR-M3: a database created by an earlier release opens and keeps working.
#[tokio::test]
async fn a_database_from_the_first_schema_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rtm.db");
    {
        let mut first = sqlx::migrate!();
        first.migrations = first.migrations[..1].to_vec().into();
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        first.run(&pool).await.unwrap();
        // Turns were millisecond timestamps then.
        sqlx::query("INSERT INTO queue_turns VALUES ('ann', 1760000000000)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
    let store = Store::open(&path, QueuePolicy::default()).await.unwrap();
    store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    store
        .submit(&user("bob"), "https://example.org/b", 1)
        .await
        .unwrap();
    // Bob has never had a turn, so goes before Ann; then Ann's turn follows on.
    let order: Vec<String> = [
        store
            .claim("w1", &rtm_domain::Stage::ALL, 10, LEASE)
            .await
            .unwrap()
            .unwrap()
            .user
            .0,
        store
            .claim("w1", &rtm_domain::Stage::ALL, 10, LEASE)
            .await
            .unwrap()
            .unwrap()
            .user
            .0,
    ]
    .into();
    assert_eq!(order, ["bob", "ann"]);
}

#[tokio::test]
async fn workers_claim_only_the_stages_they_run() {
    let db = db().await;
    db.store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    // Nothing for an extract-only worker while the item waits on fetch.
    assert!(
        db.store
            .claim("x", &[Stage::Extract], 1, LEASE)
            .await
            .unwrap()
            .is_none()
    );
    let fetch = db
        .store
        .claim("f", &[Stage::Fetch], 1, LEASE)
        .await
        .unwrap()
        .unwrap();
    db.store
        .complete(&fetch, "f", output_for(&fetch), 2)
        .await
        .unwrap();
    // Now the fetch worker has nothing, and the extract worker does.
    assert!(
        db.store
            .claim("f", &[Stage::Fetch], 3, LEASE)
            .await
            .unwrap()
            .is_none()
    );
    let extract = db
        .store
        .claim("x", &[Stage::Extract], 3, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(extract.stage, Stage::Extract);
    assert!(
        db.store
            .claim("none", &[], 3, LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn completing_fetch_records_the_source() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    assert_eq!(item.source, None);
    let job = db
        .store
        .claim("w1", &[Stage::Fetch], 1, LEASE)
        .await
        .unwrap()
        .unwrap();
    let after = db
        .store
        .complete(&job, "w1", output_for(&job), 2)
        .await
        .unwrap();

    let StageOutput::Source(expected) = output_for(&job) else {
        unreachable!()
    };
    assert_eq!(after.source.as_ref(), Some(&expected));
    let stored = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(stored.source, Some(expected));
    assert_eq!(
        stored.state,
        ItemState::Pending {
            next: Stage::Extract
        }
    );
}

#[tokio::test]
async fn stages_cannot_record_outputs_that_are_not_theirs() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let fetch = db
        .store
        .claim("w1", &[Stage::Fetch], 1, LEASE)
        .await
        .unwrap()
        .unwrap();
    // Fetch must record a source.
    assert!(matches!(
        db.store
            .complete(&fetch, "w1", StageOutput::Nothing, 2)
            .await,
        Err(StoreError::WrongOutput {
            stage: Stage::Fetch
        })
    ));
    // Rejected before any change: the lease is still held and the item unchanged.
    let unchanged = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, ItemState::Pending { next: Stage::Fetch });
    db.store
        .complete(&fetch, "w1", output_for(&fetch), 2)
        .await
        .unwrap();

    // Only fetch may record a source.
    let extract = db
        .store
        .claim("w1", &[Stage::Extract], 3, LEASE)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        db.store
            .complete(&extract, "w1", output_for(&fetch), 4)
            .await,
        Err(StoreError::WrongOutput {
            stage: Stage::Extract
        })
    ));
}

#[tokio::test]
async fn completing_extract_records_the_article() {
    let db = db().await;
    let item = db
        .store
        .submit(&user("ann"), "https://example.org/a", 0)
        .await
        .unwrap();
    let fetch = db
        .store
        .claim("w1", &[Stage::Fetch], 1, LEASE)
        .await
        .unwrap()
        .unwrap();
    db.store
        .complete(&fetch, "w1", output_for(&fetch), 2)
        .await
        .unwrap();
    let extract = db
        .store
        .claim("w1", &[Stage::Extract], 3, LEASE)
        .await
        .unwrap()
        .unwrap();

    // Extract must record an article.
    assert!(matches!(
        db.store
            .complete(&extract, "w1", StageOutput::Nothing, 4)
            .await,
        Err(StoreError::WrongOutput {
            stage: Stage::Extract
        })
    ));
    db.store
        .complete(&extract, "w1", output_for(&extract), 4)
        .await
        .unwrap();

    let StageOutput::Article(expected) = output_for(&extract) else {
        unreachable!()
    };
    let stored = db.store.get(&user("ann"), item.id).await.unwrap().unwrap();
    assert_eq!(stored.article, Some(expected));
    assert!(stored.source.is_some(), "the source is kept alongside");
    assert_eq!(
        stored.state,
        ItemState::Pending {
            next: Stage::Script
        }
    );
}
