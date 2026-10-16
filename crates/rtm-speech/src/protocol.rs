// SPDX-License-Identifier: AGPL-3.0-or-later
//! The HTTP contract between the server and the `rtm tts` process (ADR-0007):
//! `POST /synthesize` with a JSON [`SynthesizeRequest`] returns `audio/wav` (16-bit mono
//! PCM) with the engine fingerprint in [`FINGERPRINT_HEADER`]; `GET /voices` returns
//! [`VoicesResponse`]; `GET /healthz`.

use serde::{Deserialize, Serialize};

use crate::Voice;

/// The longest text one request may carry. The synthesise stage cuts pieces well below
/// this; engines chunk further.
pub const MAX_TEXT_CHARS: usize = 4_000;

/// Response header naming the engine fingerprint that produced the audio, so callers
/// can check it matches the key they cached it under.
pub const FINGERPRINT_HEADER: &str = "x-rtm-engine-fingerprint";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesizeRequest {
    pub text: String,
    pub voice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceInfo {
    #[serde(flatten)]
    pub voice: Voice,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoicesResponse {
    pub engine: String,
    pub sample_rate: u32,
    pub voices: Vec<VoiceInfo>,
}
