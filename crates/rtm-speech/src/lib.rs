// SPDX-License-Identifier: AGPL-3.0-or-later
//! Speech synthesis (FR-TTS-03/04, ADR-0007): the [`SpeechEngine`] port that engines
//! implement, text segmentation, WAV audio, the HTTP protocol between the server and
//! the `rtm tts` process, and a deterministic [`FakeEngine`] for tests and CI.

pub mod client;
mod fake;
pub mod plan;
pub mod protocol;
pub mod segment;
mod wav;

use serde::{Deserialize, Serialize};

pub use fake::FakeEngine;
pub use wav::{Pcm, WavError};

/// A voice an engine offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    pub id: String,
    /// BCP 47 language the voice speaks, e.g. "en-GB".
    pub lang: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpeechError {
    #[error("unknown voice {0:?}")]
    UnknownVoice(String),
    #[error("the text can't be spoken: {0}")]
    BadText(String),
    #[error("synthesis failed: {0}")]
    Failed(String),
}

/// A text-to-speech engine (FR-TTS-03). Implementations are CPU- or GPU-heavy and
/// synchronous; callers run them off the async runtime.
pub trait SpeechEngine: Send + Sync {
    fn id(&self) -> &str;

    /// Everything about the engine and `voice` that changes the audio for the same
    /// text: engine version, model hash, voice-file hash, synthesis parameters. Part of
    /// every segment's cache key (ADR-0008). `None` if the voice is unknown.
    fn fingerprint(&self, voice: &str) -> Option<String>;

    fn voices(&self) -> Vec<Voice>;

    fn has_voice(&self, voice: &str) -> bool {
        self.voices().iter().any(|v| v.id == voice)
    }

    fn sample_rate(&self) -> u32;

    /// Speak `text`, splitting it into model-sized chunks as the engine needs and
    /// pausing between sentences. Plain text only (FR-TTS-05).
    fn synthesize(&self, text: &str, voice: &str) -> Result<Pcm, SpeechError>;
}
