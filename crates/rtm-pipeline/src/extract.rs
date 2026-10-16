// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extract stage: run the sandboxed extract worker over the fetched source and
//! store the article it returns (ADR-0006).
//!
//! The worker is a child process (normally `rtm extract-worker`) with a cleared
//! environment, killed if it runs too long. Everything it sends back is bounded and
//! validated again here before use.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use rtm_article::{Limits, SCHEMA_VERSION};
use rtm_blobs::{BlobKind, BlobStore, article_key};
use rtm_domain::{ArticleRecord, FailureCode, Item, Stage};
use rtm_extract::protocol::{RequestHeader, Response, write_request};
use rtm_ports::StageOutput;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::{StageFailure, StageFuture, StageRunner};

#[derive(Debug, Clone)]
pub struct ExtractConfig {
    /// Wall-clock limit for the worker; it's killed after this (ADR-0006: 30 s).
    pub timeout: Duration,
    /// Most bytes read from the worker's stdout.
    pub max_output: usize,
    /// FR-EXT-05.
    pub min_words: usize,
    pub limits: Limits,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output: 8 * 1024 * 1024,
            min_words: 200,
            limits: Limits::default(),
        }
    }
}

/// How much of the worker's stderr is kept for the log.
const MAX_STDERR: usize = 4 * 1024;

pub struct ExtractStage {
    program: PathBuf,
    args: Vec<OsString>,
    blobs: BlobStore,
    config: ExtractConfig,
}

fn internal(message: &str) -> StageFailure {
    StageFailure {
        code: FailureCode::Internal,
        message: message.into(),
    }
}

/// Read to the end, keeping at most `max` bytes, and say whether anything was dropped.
/// It keeps draining past `max` so the child never blocks on a full pipe; a child that
/// writes forever is stopped by the timeout.
async fn read_bounded(
    mut from: impl AsyncRead + Unpin,
    max: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut overflowed = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = from.read(&mut chunk).await?;
        if n == 0 {
            return Ok((kept, overflowed));
        }
        let room = max - kept.len();
        kept.extend_from_slice(&chunk[..n.min(room)]);
        overflowed |= n > room;
    }
}

impl ExtractStage {
    /// `program` and `args` start a worker, normally this binary with `extract-worker`.
    pub fn new(
        program: impl Into<PathBuf>,
        args: Vec<OsString>,
        blobs: BlobStore,
        config: ExtractConfig,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            blobs,
            config,
        }
    }

    /// Run the worker over `source`, returning its parsed response.
    async fn run_worker(
        &self,
        item: &Item,
        header: &RequestHeader,
        source: Vec<u8>,
    ) -> Result<Response, StageFailure> {
        let mut request = Vec::with_capacity(source.len() + 256);
        write_request(&mut request, header, &source)
            .map_err(|_| internal("Couldn't prepare the page for extraction."))?;
        drop(source);

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not start the extract worker");
                internal("The extractor couldn't be started; it will be retried.")
            })?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let exchange = async {
            let write = async move {
                // A worker that exits early closes the pipe; its exit status tells why.
                let _ = stdin.write_all(&request).await;
                drop(stdin);
            };
            let (_, out, err, status) = tokio::join!(
                write,
                read_bounded(stdout, self.config.max_output),
                read_bounded(stderr, MAX_STDERR),
                child.wait(),
            );
            (out, err, status)
        };
        let Ok((out, err, status)) = tokio::time::timeout(self.config.timeout, exchange).await
        else {
            tracing::warn!(item = %item.id, "extract worker timed out; killed");
            return Err(internal("Extraction took too long; it will be retried."));
        };
        let stderr = err.map(|(kept, _)| kept).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr);
        let status = status.map_err(|_| internal("The extractor failed; it will be retried."))?;
        if !status.success() {
            tracing::warn!(item = %item.id, %status, stderr = %stderr.trim(), "extract worker failed");
            return Err(internal("The extractor failed; it will be retried."));
        }
        let Ok((out, false)) = out else {
            tracing::warn!(item = %item.id, "extract worker output missing or too large");
            return Err(internal("The extractor returned too much data."));
        };
        serde_json::from_slice(&out).map_err(|e| {
            tracing::warn!(item = %item.id, error = %e, "extract worker sent an invalid response");
            internal("The extractor returned something unreadable; it will be retried.")
        })
    }

    async fn extract(&self, item: &Item) -> Result<StageOutput, StageFailure> {
        let source = item
            .source
            .as_ref()
            .ok_or_else(|| internal("There's no fetched page to extract from."))?;
        let bytes = self
            .blobs
            .get(BlobKind::Source, &source.key)
            .await
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not read source");
                internal("The fetched page couldn't be read; it will be retried.")
            })?;
        let header = RequestHeader {
            kind: source.kind.into(),
            charset: source.charset.clone(),
            final_url: Some(source.final_url.clone()),
            min_words: self.config.min_words,
        };
        match self.run_worker(item, &header, bytes).await? {
            Response::Extracted {
                article,
                extractor,
                version,
                derived_title,
            } => {
                // The worker is untrusted: validate again, and count for ourselves.
                let article = article.validate(&self.config.limits).map_err(|e| {
                    tracing::warn!(item = %item.id, error = %e, "extracted article failed validation");
                    internal("The extractor returned an unusable article.")
                })?;
                let words = article.word_count();
                if words < self.config.min_words || article.title.is_empty() {
                    return Err(internal("The extractor returned an unusable article."));
                }
                let extractor = format!("{extractor}@{version}");
                let key = article_key(&source.key, &extractor, SCHEMA_VERSION);
                let json = serde_json::to_vec(&article)
                    .map_err(|_| internal("Couldn't store the article."))?;
                self.blobs
                    .put(BlobKind::Article, &key, json)
                    .await
                    .map_err(|e| {
                        tracing::error!(item = %item.id, error = %e, "could not store article");
                        internal("The article couldn't be stored; it will be retried.")
                    })?;
                Ok(StageOutput::Article(ArticleRecord {
                    key,
                    extractor,
                    title: article.title,
                    title_derived: derived_title,
                    words: words as u64,
                }))
            }
            Response::LowConfidence {
                best_words,
                reasons,
            } => {
                tracing::info!(item = %item.id, best_words, ?reasons, "low-confidence extraction");
                Err(StageFailure {
                    code: FailureCode::ExtractionLowConfidence,
                    message: format!(
                        "Couldn't find enough article text on the page (found about {best_words} \
                         words; at least {} are needed). It may be a short post, a paywall or a \
                         page that needs JavaScript.",
                        self.config.min_words
                    ),
                })
            }
            Response::Unsupported {} => Err(StageFailure {
                code: FailureCode::UnsupportedContentType,
                message: "There's no extractor for this kind of page.".into(),
            }),
        }
    }
}

impl StageRunner for ExtractStage {
    fn stage(&self) -> Stage {
        Stage::Extract
    }

    fn lease(&self) -> Duration {
        self.config.timeout * 2
    }

    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a> {
        Box::pin(self.extract(item))
    }
}
