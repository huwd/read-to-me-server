// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite storage and job queue (ADR-0003): WAL mode, one write connection through which
//! every write goes, and a small read-only pool.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use rtm_domain::{
    ArticleRecord, FailureCode, Item, ItemId, ItemState, Millis, ScriptRecord, Source, Stage,
    UserId,
};
use rtm_ports::{FailOutcome, ItemRepository, Job, JobQueue, StageOutput, StoreError, StoreResult};
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
             source_key, source_final_url, source_kind, source_charset, source_size, \
             article_key, extractor, title, title_derived, article_words, \
             script_key, script_words, \
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
    let source = match row.get::<Option<&str>, _>("source_key") {
        Some(key) => Some(Source {
            key: parse(key)?,
            final_url: row.get("source_final_url"),
            kind: parse(row.get::<&str, _>("source_kind"))?,
            charset: row.get("source_charset"),
            size: u64::try_from(row.get::<i64, _>("source_size")).map_err(storage)?,
        }),
        None => None,
    };
    let article = match row.get::<Option<&str>, _>("article_key") {
        Some(key) => Some(ArticleRecord {
            key: parse(key)?,
            extractor: row.get("extractor"),
            title: row.get("title"),
            title_derived: row.get::<i64, _>("title_derived") == 1,
            words: u64::try_from(row.get::<i64, _>("article_words")).map_err(storage)?,
        }),
        None => None,
    };
    let script = match row.get::<Option<&str>, _>("script_key") {
        Some(key) => Some(ScriptRecord {
            key: parse(key)?,
            words: u64::try_from(row.get::<i64, _>("script_words")).map_err(storage)?,
        }),
        None => None,
    };
    Ok(Item {
        id: parse(row.get::<&str, _>("id"))?,
        user: UserId(row.get("user_id")),
        source_url: row.get("source_url"),
        state,
        failure_message: row.get("failure_message"),
        source,
        article,
        script,
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

/// Matches the job row only while this exact lease is live: the same job (its ID may be
/// reused once deleted, so item and stage too), the same attempt (the same worker may
/// have reclaimed it after its lease expired), the same worker, and not yet expired.
/// Binds `?1`–`?6`; see [`bind_lease`].
macro_rules! lease_held {
    () => {
        "id = ?1 AND item_id = ?2 AND stage = ?3 AND attempts = ?4 AND locked_by = ?5 \
         AND locked_until > ?6"
    };
}

type Query<'q> = sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>;

fn bind_lease<'q>(query: Query<'q>, job: &Job, worker: &str, now: Millis) -> Query<'q> {
    query
        .bind(job.id)
        .bind(job.item.to_string())
        .bind(job.stage.as_str())
        .bind(i64::from(job.attempt))
        .bind(worker.to_string())
        .bind(now)
}

/// Remove a job, but only while `worker` still holds this lease on it.
async fn take_job(
    tx: &mut Transaction<'_, Sqlite>,
    job: &Job,
    worker: &str,
    now: Millis,
) -> StoreResult<()> {
    let query = sqlx::query(concat!("DELETE FROM jobs WHERE ", lease_held!()));
    let done = bind_lease(query, job, worker, now)
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
            source: None,
            article: None,
            script: None,
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
    async fn claim(
        &self,
        worker: &str,
        stages: &[Stage],
        now: Millis,
        lease_ms: Millis,
    ) -> StoreResult<Option<Job>> {
        // Bound as one JSON array parameter; SQL text is never built at runtime.
        let stages = format!(
            "[{}]",
            stages
                .iter()
                .map(|s| format!("\"{}\"", s.as_str()))
                .collect::<Vec<_>>()
                .join(",")
        );
        loop {
            let mut tx = self.begin().await?;
            // Runnable: due, and either never leased or with an expired lease (a crashed
            // worker). Users whose last turn was earliest go first (users never served yet
            // before anyone); then oldest due first.
            let row = sqlx::query(
                "UPDATE jobs SET attempts = attempts + 1, locked_by = ?1, locked_until = ?2 \
                 WHERE id = ( \
                   SELECT j.id FROM jobs j LEFT JOIN queue_turns t ON t.user_id = j.user_id \
                   WHERE j.run_after <= ?3 AND (j.locked_until IS NULL OR j.locked_until <= ?3) \
                     AND j.stage IN (SELECT value FROM json_each(?4)) \
                   ORDER BY COALESCE(t.turn, 0), j.run_after, j.id LIMIT 1) \
                 RETURNING id, item_id, user_id, stage, attempts, locked_until",
            )
            .bind(worker)
            .bind(now + lease_ms)
            .bind(now)
            .bind(&stages)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            let Some(row) = row else {
                return Ok(None);
            };
            let job = job_from_row(&row)?;
            sqlx::query(
                // Turns are a strictly increasing sequence, not a time: claims in the same
                // millisecond must still order users.
                "INSERT INTO queue_turns (user_id, turn) \
                 VALUES (?1, (SELECT COALESCE(MAX(turn), 0) + 1 FROM queue_turns)) \
                 ON CONFLICT (user_id) DO UPDATE SET turn = excluded.turn",
            )
            .bind(&job.user.0)
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
        let query = sqlx::query(concat!(
            "UPDATE jobs SET locked_until = ?7 WHERE ",
            lease_held!()
        ));
        let done = bind_lease(query, job, worker, now)
            .bind(until)
            .execute(&self.write)
            .await
            .map_err(storage)?;
        if done.rows_affected() == 1 {
            Ok(until)
        } else {
            Err(StoreError::LeaseLost)
        }
    }

    async fn complete(
        &self,
        job: &Job,
        worker: &str,
        output: StageOutput,
        now: Millis,
    ) -> StoreResult<Item> {
        // Each stage records exactly its own output.
        match (job.stage, &output) {
            (Stage::Fetch, StageOutput::Source(_))
            | (Stage::Extract, StageOutput::Article(_))
            | (Stage::Script, StageOutput::Script(_)) => {}
            (Stage::Fetch | Stage::Extract | Stage::Script, _)
            | (_, StageOutput::Source(_) | StageOutput::Article(_) | StageOutput::Script(_)) => {
                return Err(StoreError::WrongOutput { stage: job.stage });
            }
            (_, StageOutput::Nothing) => {}
        }
        let mut tx = self.begin().await?;
        take_job(&mut tx, job, worker, now).await?;
        let mut item = load_item(&mut tx, job.item).await?;
        match output {
            StageOutput::Source(source) => {
                sqlx::query(
                    "UPDATE items SET source_key = ?, source_final_url = ?, source_kind = ?, \
                 source_charset = ?, source_size = ? WHERE id = ?",
                )
                .bind(source.key.as_str())
                .bind(&source.final_url)
                .bind(source.kind.as_str())
                .bind(&source.charset)
                .bind(i64::try_from(source.size).map_err(storage)?)
                .bind(item.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
                item.source = Some(source);
            }
            StageOutput::Article(article) => {
                sqlx::query(
                "UPDATE items SET article_key = ?, extractor = ?, title = ?, title_derived = ?, \
                 article_words = ? WHERE id = ?",
            )
            .bind(article.key.as_str())
            .bind(&article.extractor)
            .bind(&article.title)
            .bind(i64::from(article.title_derived))
            .bind(i64::try_from(article.words).map_err(storage)?)
            .bind(item.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
                item.article = Some(article);
            }
            StageOutput::Script(script) => {
                sqlx::query("UPDATE items SET script_key = ?, script_words = ? WHERE id = ?")
                    .bind(script.key.as_str())
                    .bind(i64::try_from(script.words).map_err(storage)?)
                    .bind(item.id.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
                item.script = Some(script);
            }
            StageOutput::Nothing => {}
        }
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
            let query = sqlx::query(concat!(
                "UPDATE jobs SET run_after = ?7, locked_by = NULL, locked_until = NULL, \
                 last_error = ?8 WHERE ",
                lease_held!()
            ));
            let done = bind_lease(query, job, worker, now)
                .bind(run_after)
                .bind(format!("{}: {message}", code.as_str()))
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
