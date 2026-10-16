// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ports: the interfaces between read-to-me's domain and its adapters. Stages and the API
//! depend on these traits, not on SQLite.

use std::future::Future;

use rtm_domain::{
    ArticleRecord, EpisodeRecord, FailureCode, Item, ItemId, Millis, ScriptRecord, Source,
    SpeechRecord, Stage, TransitionError, UserId,
};

/// A claimed unit of work: run `stage` for `item`. Held under a lease until `lease_until`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub item: ItemId,
    pub user: UserId,
    pub stage: Stage,
    /// Includes the current attempt, so the first run is attempt 1.
    pub attempt: u32,
    pub lease_until: Millis,
}

/// What a completed stage produced, recorded in the same transaction that completes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutput {
    /// The fetch stage's document, already written to the blob store.
    Source(Source),
    /// The extract stage's article, already written to the blob store.
    Article(ArticleRecord),
    /// The script stage's narration script, already written to the blob store.
    Script(ScriptRecord),
    /// The synthesise stage's speech plan, already written to the blob store.
    Speech(SpeechRecord),
    /// The encode stage's episode, already written to the blob store.
    Episode(EpisodeRecord),
    /// Stages whose outputs aren't recorded yet.
    Nothing,
}

/// What happened to a job reported as failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    /// Requeued to run again no earlier than `run_after`.
    Retrying { run_after: Millis },
    /// The item has failed terminally.
    Failed,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    /// The worker's lease expired and the job was reclaimed (or finished) elsewhere.
    #[error("lease lost")]
    LeaseLost,
    #[error(transparent)]
    Transition(#[from] TransitionError),
    /// A stage reported an output that doesn't belong to it.
    #[error("stage {stage:?} cannot produce this output")]
    WrongOutput { stage: Stage },
    #[error("storage: {0}")]
    Storage(Box<dyn std::error::Error + Send + Sync>),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Items as seen by the API.
pub trait ItemRepository: Send + Sync {
    /// Record a new item and queue its first stage, atomically.
    fn submit(
        &self,
        user: &UserId,
        source_url: &str,
        now: Millis,
    ) -> impl Future<Output = StoreResult<Item>> + Send;

    /// An item, only if `user` owns it.
    fn get(
        &self,
        user: &UserId,
        id: ItemId,
    ) -> impl Future<Output = StoreResult<Option<Item>>> + Send;
}

/// A user's private podcast feed (FR-FEED-01/05): reached by a capability token that is
/// stored only as a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub user: UserId,
    /// Stable for the feed's life (`podcast:guid`), across token rotations.
    pub guid: String,
}

/// Feeds and what they publish.
pub trait FeedRepository: Send + Sync {
    /// Set `user`'s feed token (by its hash), replacing any previous one, and return
    /// the feed. The old token stops working at once.
    fn set_feed_token(
        &self,
        user: &UserId,
        token_hash: &str,
        now: Millis,
    ) -> impl Future<Output = StoreResult<Feed>> + Send;

    /// The feed a token hash opens, if any.
    fn feed_by_token(
        &self,
        token_hash: &str,
    ) -> impl Future<Output = StoreResult<Option<Feed>>> + Send;

    /// `user`'s published items, most recently published first, at most `limit`.
    fn published(
        &self,
        user: &UserId,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<Item>>> + Send;
}

/// The durable job queue the pipeline workers pull from (ADR-0003).
pub trait JobQueue: Send + Sync {
    /// Lease the next runnable job for one of `stages`, picking users round-robin
    /// (NFR-S2). Workers name the stages they can run, so other stages wait for theirs.
    fn claim(
        &self,
        worker: &str,
        stages: &[Stage],
        now: Millis,
        lease_ms: Millis,
    ) -> impl Future<Output = StoreResult<Option<Job>>> + Send;

    /// Extend a lease the worker still holds.
    fn heartbeat(
        &self,
        job: &Job,
        worker: &str,
        now: Millis,
        lease_ms: Millis,
    ) -> impl Future<Output = StoreResult<Millis>> + Send;

    /// Record the stage as done with its output: advance the item and queue the next
    /// stage, atomically.
    fn complete(
        &self,
        job: &Job,
        worker: &str,
        output: StageOutput,
        now: Millis,
    ) -> impl Future<Output = StoreResult<Item>> + Send;

    /// Record a failure: retry with backoff if `code` is retryable and attempts remain,
    /// otherwise fail the item.
    fn fail(
        &self,
        job: &Job,
        worker: &str,
        code: FailureCode,
        message: &str,
        now: Millis,
    ) -> impl Future<Output = StoreResult<FailOutcome>> + Send;
}
