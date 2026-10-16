// SPDX-License-Identifier: AGPL-3.0-or-later
//! The script stage: turn the extracted article into a narration script (FR-TTS-01),
//! after checking it is short enough (FR-TTS-06) and in a supported language
//! (FR-TTS-07).

use std::time::Duration;

use rtm_article::{Article, Limits};
use rtm_blobs::{BlobKind, BlobStore, script_key};
use rtm_domain::{FailureCode, Item, ScriptRecord, Stage};
use rtm_narrate::{NARRATOR_VERSION, Options, narrate, phrases_for};
use rtm_ports::StageOutput;

use crate::{StageFailure, StageFuture, StageRunner};

/// FR-TTS-06: about 3 h 15 min of audio at ~155 words a minute.
pub const DEFAULT_MAX_WORDS: u64 = 30_000;

pub struct ScriptStage {
    blobs: BlobStore,
    options: Options,
    max_words: u64,
}

fn internal(message: &str) -> StageFailure {
    StageFailure {
        code: FailureCode::Internal,
        message: message.into(),
    }
}

impl ScriptStage {
    pub fn new(blobs: BlobStore, options: Options, max_words: u64) -> Self {
        Self {
            blobs,
            options,
            max_words,
        }
    }

    async fn script(&self, item: &Item) -> Result<StageOutput, StageFailure> {
        let record = item
            .article
            .as_ref()
            .ok_or_else(|| internal("There's no extracted article to narrate."))?;
        // FR-TTS-06: checked before synthesis, so a long article fails in seconds, not
        // after hours of work.
        if record.words > self.max_words {
            return Err(StageFailure {
                code: FailureCode::TooLong,
                message: format!(
                    "The article is {} words long; the limit is {} words.",
                    record.words, self.max_words
                ),
            });
        }
        let bytes = self
            .blobs
            .get(BlobKind::Article, &record.key)
            .await
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not read article");
                internal("The extracted article couldn't be read; it will be retried.")
            })?;
        let article: Article = serde_json::from_slice::<Article>(&bytes)
            .ok()
            .and_then(|a| a.validate(&Limits::default()).ok())
            .ok_or_else(|| internal("The extracted article is unreadable."))?;
        let Some(phrases) = phrases_for(article.lang.as_deref()) else {
            return Err(StageFailure {
                code: FailureCode::LanguageUnsupported,
                message: format!(
                    "The article is in a language this server can't narrate yet ({}). Only \
                     English is supported.",
                    article.lang.as_deref().unwrap_or("unknown")
                ),
            });
        };
        let script = narrate(&article, phrases, &self.options);
        let options = serde_json::to_vec(&self.options).map_err(|_| internal("Bad options."))?;
        let key = script_key(&record.key, NARRATOR_VERSION, &options);
        let json =
            serde_json::to_vec(&script).map_err(|_| internal("Couldn't store the script."))?;
        self.blobs
            .put(BlobKind::Script, &key, json)
            .await
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not store script");
                internal("The script couldn't be stored; it will be retried.")
            })?;
        Ok(StageOutput::Script(ScriptRecord {
            key,
            words: script.word_count() as u64,
        }))
    }
}

impl StageRunner for ScriptStage {
    fn stage(&self) -> Stage {
        Stage::Script
    }

    fn lease(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a> {
        Box::pin(self.script(item))
    }
}
