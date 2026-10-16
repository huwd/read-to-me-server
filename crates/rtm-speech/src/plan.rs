// SPDX-License-Identifier: AGPL-3.0-or-later
//! The speech plan: which audio segments make an episode, in order, with the silence
//! after each. Synthesis writes it; encoding assembles the episode from it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechPlan {
    pub sample_rate: u32,
    pub segments: Vec<PlannedSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedSegment {
    /// The segment's blob key (16-bit mono WAV).
    pub key: String,
    pub duration_ms: u64,
    pub pause_after_ms: u32,
}

impl SpeechPlan {
    /// Segments and pauses together.
    pub fn duration_ms(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| s.duration_ms + u64::from(s.pause_after_ms))
            .sum()
    }
}
