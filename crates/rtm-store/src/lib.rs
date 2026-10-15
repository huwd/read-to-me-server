// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite storage and job queue (ADR-0003): WAL mode, one write connection through which
//! every write goes, and a small read-only pool.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use rtm_domain::{FailureCode, Item, ItemId, ItemState, Millis, Stage, UserId};
use rtm_ports::{FailOutcome, ItemRepository, Job, JobQueue, StoreError, StoreResult};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{Row, Sqlite, Transaction};

/// Retry limits and backoff for failed jobs.
#[derive(Debug, Clone, Copy)]
pub struct QueuePolicy {
    /// Attempts per stage, including the first.
    pub max_attempts: u32,
    pub base_backoff_ms: Millis,
    pub max_backoff_ms: Millis,
}

impl Default for QueuePolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff_ms: 30_000,
            max_backoff_ms: 3_600_000,
        }
    }
}

impl QueuePolicy {
    /// Exponential backoff with "equal jitter": half the delay is fixed, half varies with
    /// `seed`, so jobs that fail together don't retry together.
    pub fn backoff(&self, attempt: u32, seed: u64) -> Millis {
        let exp = attempt.saturating_sub(1).min(30);
        let full = self
            .base_backoff_ms
            .saturating_mul(1 << exp)
            .min(self.max_backoff_ms);
        let half = full / 2;
        half + (seed % (half.max(0) as u64 + 1)) as Millis
    }
}

pub struct Store {
    write: SqlitePool,
    read: SqlitePool,
    policy: QueuePolicy,
}

fn storage(e: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Storage(Box::new(e))
}

impl Store {
    /// Open (creating if needed) the database at `path` and run pending migrations.
    pub async fn open(path: &Path, policy: QueuePolicy) -> StoreResult<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .map_err(storage)?;
        sqlx::migrate!().run(&write).await.map_err(storage)?;
        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options.read_only(true))
            .await
            .map_err(storage)?;
        Ok(Self {
            write,
            read,
            policy,
        })
    }

    async fn begin(&self) -> StoreResult<Transaction<'static, Sqlite>> {
        self.write.begin().await.map_err(storage)
    }
}

/// `SELECT … FROM items` with every column `item_from_row` reads, then `$tail`.
macro_rules! select_items {
    ($tail:literal) => {
        concat!(
            "SELECT id, user_id, source_url, state, stage, failure_code, failure_message, \
             created_at, updated_at FROM items ",
            $tail
        )
    };
}

fn parse<T: FromStr>(s: &str) -> StoreResult<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    s.parse().map_err(storage)
}

fn item_from_row(row: &SqliteRow) -> StoreResult<Item> {
    let stage: Option<String> = row.get("stage");
    let stage = stage.as_deref().map(parse::<Stage>).transpose()?;
    let state = match (row.get::<&str, _>("state"), stage) {
        ("pending", Some(next)) => ItemState::Pending { next },
        ("published", None) => ItemState::Published,
        ("failed", Some(stage)) => ItemState::Failed {
            stage,
            code: parse(row.get::<&str, _>("failure_code"))?,
        },
        (state, stage) => {
            return Err(StoreError::Storage(
                format!("inconsistent item row: {state} / {stage:?}").into(),
            ));
        }
    };
    Ok(Item {
        id: parse(row.get::<&str, _>("id"))?,
        user: UserId(row.get("user_id")),
        source_url: row.get("source_url"),
        state,
        failure_message: row.get("failure_message"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn job_from_row(row: &SqliteRow) -> StoreResult<Job> {
    Ok(Job {
        id: row.get("id"),
        item: parse(row.get::<&str, _>("item_id"))?,
        user: UserId(row.get("user_id")),
        stage: parse(row.get::<&str, _>("stage"))?,
        attempt: u32::try_from(row.get::<i64, _>("attempts")).map_err(storage)?,
        lease_until: row.get("locked_until"),
    })
}

/// `(state, stage, failure_code)` columns for an item state.
fn state_columns(state: ItemState) -> (&'static str, Option<&'static str>, Option<&'static str>) {
    match state {
        ItemState::Pending { next } => ("pending", Some(next.as_str()), None),
        ItemState::Published => ("published", None, None),
        ItemState::Failed { stage, code } => ("failed", Some(stage.as_str()), Some(code.as_str())),
    }
}

async fn load_item(tx: &mut Transaction<'_, Sqlite>, id: ItemId) -> StoreResult<Item> {
    let row = sqlx::query(select_items!("WHERE id = ?"))
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?
        .ok_or(StoreError::NotFound)?;
    item_from_row(&row)
}

async fn save_state(
    tx: &mut Transaction<'_, Sqlite>,
    item: &mut Item,
    state: ItemState,
    message: Option<&str>,
    now: Millis,
) -> StoreResult<()> {
    let (s, stage, code) = state_columns(state);
    sqlx::query(
        "UPDATE items SET state = ?, stage = ?, failure_code = ?, failure_message = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(s)
    .bind(stage)
    .bind(code)
    .bind(message)
    .bind(now)
    .bind(item.id.to_string())
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    item.state = state;
    item.failure_message = message.map(str::to_string);
    item.updated_at = now;
    Ok(())
}

/// Remove a job, but only while `worker` still holds its lease.
async fn take_job(
    tx: &mut Transaction<'_, Sqlite>,
    job: &Job,
    worker: &str,
    now: Millis,
) -> StoreResult<()> {
    let done = sqlx::query("DELETE FROM jobs WHERE id = ? AND locked_by = ? AND locked_until > ?")
        .bind(job.id)
        .bind(worker)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    if done.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StoreError::LeaseLost)
    }
}

async fn enqueue(
    tx: &mut Transaction<'_, Sqlite>,
    item: &Item,
    stage: Stage,
    run_after: Millis,
) -> StoreResult<()> {
    sqlx::query("INSERT INTO jobs (item_id, user_id, stage, run_after) VALUES (?, ?, ?, ?)")
        .bind(item.id.to_string())
        .bind(&item.user.0)
        .bind(stage.as_str())
        .bind(run_after)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    Ok(())
}

impl ItemRepository for Store {
    async fn submit(&self, user: &UserId, source_url: &str, now: Millis) -> StoreResult<Item> {
        let item = Item {
            id: ItemId::new(),
            user: user.clone(),
            source_url: source_url.to_string(),
            state: ItemState::submitted(),
            failure_message: None,
            created_at: now,
            updated_at: now,
        };
        let (state, stage, _) = state_columns(item.state);
        let mut tx = self.begin().await?;
        sqlx::query(
            "INSERT INTO items (id, user_id, source_url, state, stage, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item.id.to_string())
        .bind(&user.0)
        .bind(source_url)
        .bind(state)
        .bind(stage)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        enqueue(&mut tx, &item, Stage::Fetch, now).await?;
        tx.commit().await.map_err(storage)?;
        Ok(item)
    }

    async fn get(&self, user: &UserId, id: ItemId) -> StoreResult<Option<Item>> {
        let row = sqlx::query(select_items!("WHERE id = ? AND user_id = ?"))
            .bind(id.to_string())
            .bind(&user.0)
            .fetch_optional(&self.read)
            .await
            .map_err(storage)?;
        row.as_ref().map(item_from_row).transpose()
    }
}

impl JobQueue for Store {
    async fn claim(&self, worker: &str, now: Millis, lease_ms: Millis) -> StoreResult<Option<Job>> {
        loop {
            let mut tx = self.begin().await?;
            // Runnable: due, and either never leased or with an expired lease (a crashed
            // worker). Users whose turn was longest ago go first; then oldest due first.
            let row = sqlx::query(
                "UPDATE jobs SET attempts = attempts + 1, locked_by = ?1, locked_until = ?2 \
                 WHERE id = ( \
                   SELECT j.id FROM jobs j LEFT JOIN queue_turns t ON t.user_id = j.user_id \
                   WHERE j.run_after <= ?3 AND (j.locked_until IS NULL OR j.locked_until <= ?3) \
                   ORDER BY COALESCE(t.last_claimed_at, -1), j.run_after, j.id LIMIT 1) \
                 RETURNING id, item_id, user_id, stage, attempts, locked_until",
            )
            .bind(worker)
            .bind(now + lease_ms)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            let Some(row) = row else {
                return Ok(None);
            };
            let job = job_from_row(&row)?;
            sqlx::query(
                "INSERT INTO queue_turns (user_id, last_claimed_at) VALUES (?1, ?2) \
                 ON CONFLICT (user_id) DO UPDATE SET last_claimed_at = ?2",
            )
            .bind(&job.user.0)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

            // A job that keeps crashing its worker (lease expiring every time) must not
            // loop forever: past the attempt limit, fail the item instead of running it.
            if job.attempt > self.policy.max_attempts {
                let mut item = load_item(&mut tx, job.item).await?;
                let failed = item.state.fail(job.stage, FailureCode::Internal)?;
                save_state(
                    &mut tx,
                    &mut item,
                    failed,
                    Some("worker lease expired repeatedly"),
                    now,
                )
                .await?;
                take_job(&mut tx, &job, worker, now).await?;
                tx.commit().await.map_err(storage)?;
                continue;
            }
            tx.commit().await.map_err(storage)?;
            return Ok(Some(job));
        }
    }

    async fn heartbeat(
        &self,
        job: &Job,
        worker: &str,
        now: Millis,
        lease_ms: Millis,
    ) -> StoreResult<Millis> {
        let until = now + lease_ms;
        let done = sqlx::query(
            "UPDATE jobs SET locked_until = ? WHERE id = ? AND locked_by = ? AND locked_until > ?",
        )
        .bind(until)
        .bind(job.id)
        .bind(worker)
        .bind(now)
        .execute(&self.write)
        .await
        .map_err(storage)?;
        if done.rows_affected() == 1 {
            Ok(until)
        } else {
            Err(StoreError::LeaseLost)
        }
    }

    async fn complete(&self, job: &Job, worker: &str, now: Millis) -> StoreResult<Item> {
        let mut tx = self.begin().await?;
        take_job(&mut tx, job, worker, now).await?;
        let mut item = load_item(&mut tx, job.item).await?;
        let next = item.state.complete(job.stage)?;
        save_state(&mut tx, &mut item, next, None, now).await?;
        if let ItemState::Pending { next } = next {
            enqueue(&mut tx, &item, next, now).await?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(item)
    }

    async fn fail(
        &self,
        job: &Job,
        worker: &str,
        code: FailureCode,
        message: &str,
        now: Millis,
    ) -> StoreResult<FailOutcome> {
        let mut tx = self.begin().await?;
        if code.retryable() && job.attempt < self.policy.max_attempts {
            let seed = job
                .item
                .to_string()
                .bytes()
                .fold(job.attempt as u64, |h, b| {
                    h.wrapping_mul(31).wrapping_add(b as u64)
                });
            let run_after = now + self.policy.backoff(job.attempt, seed);
            let done = sqlx::query(
                "UPDATE jobs SET run_after = ?, locked_by = NULL, locked_until = NULL, \
                 last_error = ? WHERE id = ? AND locked_by = ? AND locked_until > ?",
            )
            .bind(run_after)
            .bind(format!("{}: {message}", code.as_str()))
            .bind(job.id)
            .bind(worker)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            if done.rows_affected() != 1 {
                return Err(StoreError::LeaseLost);
            }
            tx.commit().await.map_err(storage)?;
            return Ok(FailOutcome::Retrying { run_after });
        }
        take_job(&mut tx, job, worker, now).await?;
        let mut item = load_item(&mut tx, job.item).await?;
        let failed = item.state.fail(job.stage, code)?;
        save_state(&mut tx, &mut item, failed, Some(message), now).await?;
        tx.commit().await.map_err(storage)?;
        Ok(FailOutcome::Failed)
    }
}
