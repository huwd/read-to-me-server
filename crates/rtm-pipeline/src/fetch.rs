// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fetch stage: download the submitted URL through the egress guard and store it.

use rtm_blobs::BlobStore;
use rtm_domain::{FailureCode, Item, Source, SourceKind};
use rtm_fetch::{Kind, SafeFetcher};
use rtm_ports::StageOutput;
use url::Url;

use crate::StageFailure;

pub struct FetchStage {
    fetcher: SafeFetcher,
    blobs: BlobStore,
}

impl FetchStage {
    pub fn new(fetcher: SafeFetcher, blobs: BlobStore) -> Self {
        Self { fetcher, blobs }
    }

    pub async fn run(&self, item: &Item) -> Result<StageOutput, StageFailure> {
        let url = Url::parse(&item.source_url).map_err(|_| StageFailure {
            code: FailureCode::Internal,
            message: "The stored URL could not be read.".into(),
        })?;
        let fetched = self.fetcher.fetch_article(&url).await.map_err(|e| {
            tracing::info!(item = %item.id, error = %e, "fetch failed");
            StageFailure {
                code: e.code(),
                message: e.user_message(),
            }
        })?;
        let size = fetched.body.len() as u64;
        // Public URL-fetched content has an empty owner salt (ADR-0008).
        let key = self
            .blobs
            .put_source(fetched.body.to_vec(), b"")
            .await
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not store source");
                StageFailure {
                    code: FailureCode::Internal,
                    message: "The page was fetched but couldn't be stored; it will be retried."
                        .into(),
                }
            })?;
        Ok(StageOutput::Source(Source {
            key,
            final_url: fetched.final_url.to_string(),
            kind: match fetched.kind {
                Kind::Html => SourceKind::Html,
                Kind::PlainText => SourceKind::PlainText,
            },
            charset: fetched.charset,
            size,
        }))
    }
}
