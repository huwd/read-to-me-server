// SPDX-License-Identifier: AGPL-3.0-or-later
//! The synthesise stage (FR-TTS-04): voice the script piece by piece through the
//! `rtm tts` process, caching each piece's audio, and write the speech plan that
//! encoding assembles into one episode.
//!
//! Each piece is a blob keyed by its text, language and the engine's fingerprint for
//! the voice, so a crash or retry resumes where it stopped, and the same text in the
//! same voice is never synthesised twice.

use std::time::Duration;

use rtm_blobs::{BlobKind, BlobStore, plan_key, segment_key};
use rtm_domain::{FailureCode, Item, SpeechRecord, Stage};
use rtm_narrate::Script;
use rtm_ports::StageOutput;
use rtm_speech::Pcm;
use rtm_speech::client::{ClientError, TtsClient};
use rtm_speech::plan::{PlannedSegment, SpeechPlan};
use rtm_speech::segment::pieces;
use serde::Serialize;

use crate::{StageFailure, StageFuture, StageRunner};

/// Bump on any change to how scripts become plans: it's part of the plan's key.
pub const PLANNER_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct SynthesiseConfig {
    pub voice: String,
    /// Longest piece sent to the engine, in characters: the unit of caching and resume.
    pub piece_chars: usize,
    /// Silence between pieces of one utterance (they split between sentences).
    pub piece_gap_ms: u32,
}

impl Default for SynthesiseConfig {
    fn default() -> Self {
        Self {
            voice: "bf_emma".into(),
            piece_chars: 1_500,
            piece_gap_ms: 150,
        }
    }
}

pub struct SynthesiseStage {
    client: TtsClient,
    blobs: BlobStore,
    config: SynthesiseConfig,
}

fn failure(code: FailureCode, message: &str) -> StageFailure {
    StageFailure {
        code,
        message: message.into(),
    }
}

fn internal(message: &str) -> StageFailure {
    failure(FailureCode::Internal, message)
}

fn engine_trouble(item: &Item, e: &ClientError) -> StageFailure {
    tracing::warn!(item = %item.id, error = %e, "TTS service problem");
    match e {
        ClientError::Unreachable(_) => failure(
            FailureCode::EngineUnavailable,
            "The speech engine isn't available; it will be retried.",
        ),
        _ => internal("Speech synthesis failed; it will be retried."),
    }
}

impl SynthesiseStage {
    pub fn new(client: TtsClient, blobs: BlobStore, config: SynthesiseConfig) -> Self {
        Self {
            client,
            blobs,
            config,
        }
    }

    /// One piece's audio and its duration: from the cache if present, otherwise
    /// synthesised and stored.
    async fn segment(
        &self,
        item: &Item,
        text: &str,
        lang: &str,
        fingerprint: &str,
        sample_rate: u32,
    ) -> Result<(String, u64), StageFailure> {
        let key = segment_key(text, lang, fingerprint);
        if let Ok(bytes) = self.blobs.get(BlobKind::Segment, &key).await {
            if let Ok(pcm) = Pcm::from_wav(&bytes)
                && pcm.sample_rate == sample_rate
            {
                return Ok((key.to_string(), pcm.duration_ms()));
            }
            tracing::warn!(item = %item.id, %key, "unreadable cached segment; synthesising again");
        }
        let (pcm, produced_by) = self
            .client
            .synthesize(text, &self.config.voice)
            .await
            .map_err(|e| engine_trouble(item, &e))?;
        // The engine changed under us (new model or voice files): this audio doesn't
        // belong under the key we computed. Retry, and pick up the new fingerprint.
        if produced_by != fingerprint {
            tracing::warn!(item = %item.id, expected = fingerprint, got = %produced_by, "engine changed mid-synthesis");
            return Err(failure(
                FailureCode::EngineUnavailable,
                "The speech engine changed during synthesis; it will be retried.",
            ));
        }
        if pcm.sample_rate != sample_rate || pcm.samples.is_empty() {
            return Err(internal("The speech engine returned unusable audio."));
        }
        let duration = pcm.duration_ms();
        // A real failure here would be surprising, but if two items race to store
        // the same segment the blob store keeps the first, which is identical.
        self.blobs
            .put(BlobKind::Segment, &key, pcm.to_wav())
            .await
            .map_err(|e| {
                tracing::error!(item = %item.id, error = %e, "could not store segment");
                internal("Audio couldn't be stored; it will be retried.")
            })?;
        Ok((key.to_string(), duration))
    }

    async fn synthesise(&self, item: &Item) -> Result<StageOutput, StageFailure> {
        let record = item
            .script
            .as_ref()
            .ok_or_else(|| internal("There's no script to synthesise."))?;
        let bytes = self
            .blobs
            .get(BlobKind::Script, &record.key)
            .await
            .map_err(|_| internal("The script couldn't be read; it will be retried."))?;
        let script: Script =
            serde_json::from_slice(&bytes).map_err(|_| internal("The script is unreadable."))?;

        let voices = self
            .client
            .voices()
            .await
            .map_err(|e| engine_trouble(item, &e))?;
        let Some(voice) = voices
            .voices
            .iter()
            .find(|v| v.voice.id == self.config.voice)
        else {
            tracing::error!(voice = %self.config.voice, engine = %voices.engine, "voice not offered by the TTS service");
            return Err(failure(
                FailureCode::EngineUnavailable,
                "The configured voice isn't available; it will be retried.",
            ));
        };
        let fingerprint = voice.fingerprint.clone();

        let mut segments = Vec::new();
        for utterance in &script.utterances {
            let parts = pieces(&utterance.text, self.config.piece_chars);
            let last = parts.len().saturating_sub(1);
            for (n, text) in parts.iter().enumerate() {
                let (key, duration_ms) = self
                    .segment(item, text, &script.lang, &fingerprint, voices.sample_rate)
                    .await?;
                let pause_after_ms = if n == last {
                    utterance.pause_after_ms
                } else {
                    self.config.piece_gap_ms
                };
                segments.push(PlannedSegment {
                    key,
                    duration_ms,
                    pause_after_ms,
                });
            }
        }
        if segments.is_empty() {
            return Err(internal("The script has nothing to say."));
        }

        let plan = SpeechPlan {
            sample_rate: voices.sample_rate,
            segments,
        };
        let params = serde_json::to_vec(&(PLANNER_VERSION, &self.config))
            .map_err(|_| internal("Bad configuration."))?;
        let key = plan_key(&record.key, &fingerprint, &params);
        let json = serde_json::to_vec(&plan).map_err(|_| internal("Couldn't store the plan."))?;
        self.blobs
            .put(BlobKind::Plan, &key, json)
            .await
            .map_err(|_| internal("The speech plan couldn't be stored; it will be retried."))?;
        Ok(StageOutput::Speech(SpeechRecord {
            plan_key: key,
            duration_ms: plan.duration_ms(),
            segments: plan.segments.len() as u64,
        }))
    }
}

impl StageRunner for SynthesiseStage {
    fn stage(&self) -> Stage {
        Stage::Synthesise
    }

    /// Long articles take tens of minutes, so the worker heartbeats within this.
    fn lease(&self) -> Duration {
        Duration::from_secs(300)
    }

    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a> {
        Box::pin(self.synthesise(item))
    }
}
