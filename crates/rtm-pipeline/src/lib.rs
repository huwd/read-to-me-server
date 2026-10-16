// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pipeline workers (docs/03 §5): claim a job, run its stage, record the result.
//!
//! Stages are idempotent: their outputs are content-addressed blobs written before the
//! job is completed, so a stage re-run after a crash writes the same keys again.

mod extract;
mod fetch;
mod script;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rtm_domain::{FailureCode, Item, Millis, Stage};
use rtm_ports::{FailOutcome, ItemRepository, Job, JobQueue, StageOutput, StoreError};
use tokio::sync::watch;

pub use extract::{ExtractConfig, ExtractStage};
pub use fetch::FetchStage;
pub use script::{DEFAULT_MAX_WORDS, ScriptStage};

/// Why a stage failed: the code decides retry or terminal; the message is shown to the
/// user, so it must not carry internal detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageFailure {
    pub code: FailureCode,
    pub message: String,
}

/// The future a stage returns.
pub type StageFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StageOutput, StageFailure>> + Send + 'a>>;

/// One pipeline stage, as run by a [`Worker`].
pub trait StageRunner: Send + Sync {
    fn stage(&self) -> Stage;
    /// How long a job is leased for: comfortably longer than the stage can take, so no
    /// heartbeat is needed.
    fn lease(&self) -> Duration;
    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a>;
}

/// How long an idle worker waits before looking for work again.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

fn now() -> Millis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
        Millis::try_from(d.as_millis()).unwrap_or(Millis::MAX)
    })
}

fn millis(d: Duration) -> Millis {
    Millis::try_from(d.as_millis()).unwrap_or(Millis::MAX)
}

/// What a worker did on one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    Idle,
    Completed(Stage),
    Retrying(Stage, FailureCode),
    Failed(Stage, FailureCode),
    /// The lease was lost, or the item vanished; nothing was recorded.
    Abandoned,
}

/// A worker that runs one stage's jobs.
pub struct Worker<S> {
    name: String,
    store: Arc<S>,
    runner: Arc<dyn StageRunner>,
}

impl<S: ItemRepository + JobQueue + 'static> Worker<S> {
    pub fn new(name: impl Into<String>, store: Arc<S>, runner: Arc<dyn StageRunner>) -> Self {
        Self {
            name: name.into(),
            store,
            runner,
        }
    }

    /// Claim and run at most one job.
    pub async fn run_once(&self) -> Result<Turn, StoreError> {
        let Some(job) = self
            .store
            .claim(
                &self.name,
                &[self.runner.stage()],
                now(),
                millis(self.runner.lease()),
            )
            .await?
        else {
            return Ok(Turn::Idle);
        };
        let Some(item) = self.store.get(&job.user, job.item).await? else {
            return Ok(Turn::Abandoned);
        };
        let result = self.run_stage(&job, &item).await;
        self.record(&job, result).await
    }

    async fn run_stage(&self, job: &Job, item: &Item) -> Result<StageOutput, StageFailure> {
        if job.stage == self.runner.stage() {
            self.runner.run(item).await
        } else {
            Err(StageFailure {
                code: FailureCode::Internal,
                message: format!("no handler for the {} stage", job.stage.as_str()),
            })
        }
    }

    async fn record(
        &self,
        job: &Job,
        result: Result<StageOutput, StageFailure>,
    ) -> Result<Turn, StoreError> {
        let outcome = match result {
            Ok(output) => self
                .store
                .complete(job, &self.name, output, now())
                .await
                .map(|_| Turn::Completed(job.stage)),
            Err(failure) => self
                .store
                .fail(job, &self.name, failure.code, &failure.message, now())
                .await
                .map(|outcome| match outcome {
                    FailOutcome::Retrying { .. } => Turn::Retrying(job.stage, failure.code),
                    FailOutcome::Failed => Turn::Failed(job.stage, failure.code),
                }),
        };
        match outcome {
            Err(StoreError::LeaseLost) => Ok(Turn::Abandoned),
            other => other,
        }
    }

    /// Work until `shutdown` turns true. A job already running finishes first.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        while !*shutdown.borrow() {
            let turn = self.run_once().await;
            match &turn {
                Ok(Turn::Idle) => {}
                Ok(turn) => {
                    tracing::info!(worker = %self.name, ?turn, "job finished");
                    continue;
                }
                Err(e) => tracing::error!(worker = %self.name, error = %e, "queue error"),
            }
            // Idle or erroring: wait a moment, or stop if asked.
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.changed() => {}
            }
        }
        tracing::info!(worker = %self.name, "worker stopped");
    }
}
