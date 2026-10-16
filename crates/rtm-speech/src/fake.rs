// SPDX-License-Identifier: AGPL-3.0-or-later
//! A deterministic engine for tests and CI: no model, realistic durations.
//!
//! Each word becomes a short tone whose pitch comes from the word's hash and whose
//! length grows with the word, at roughly the 160 words a minute Kokoro speaks, with
//! gaps between words and longer ones between sentences. The same text and voice always
//! give the same samples.

use crate::segment::sentences;
use crate::{Pcm, SpeechEngine, SpeechError, Voice};

pub struct FakeEngine;

const SAMPLE_RATE: u32 = 24_000;
const WORD_GAP_MS: u32 = 60;
const SENTENCE_GAP_MS: u32 = 150;

/// The voices the fake engine pretends to have: the Kokoro ones we use.
const VOICES: [(&str, &str); 2] = [("bf_emma", "en-GB"), ("bm_george", "en-GB")];

fn tone(word: &str, voice: &str, out: &mut Vec<i16>) {
    let hash = blake3::hash(format!("{voice}\u{0}{word}").as_bytes());
    let pitch = 180.0 + f32::from(hash.as_bytes()[0]) * 1.5; // 180 to 562 Hz
    let ms = 120 + 45 * word.chars().count().min(20) as u32;
    let n = SAMPLE_RATE * ms / 1000;
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE as f32;
        // A short fade in and out avoids clicks.
        let edge = (i.min(n - i) as f32 / 240.0).min(1.0);
        let sample = (t * pitch * std::f32::consts::TAU).sin() * 8_000.0 * edge;
        out.push(sample as i16);
    }
}

impl SpeechEngine for FakeEngine {
    fn id(&self) -> &str {
        "fake"
    }

    fn fingerprint(&self, voice: &str) -> Option<String> {
        self.has_voice(voice).then(|| format!("fake@1/{voice}"))
    }

    fn voices(&self) -> Vec<Voice> {
        VOICES
            .iter()
            .map(|(id, lang)| Voice {
                id: (*id).into(),
                lang: (*lang).into(),
            })
            .collect()
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn synthesize(&self, text: &str, voice: &str) -> Result<Pcm, SpeechError> {
        if !self.has_voice(voice) {
            return Err(SpeechError::UnknownVoice(voice.into()));
        }
        let mut samples = Vec::new();
        let sentences = sentences(text);
        for (s, sentence) in sentences.iter().enumerate() {
            let words: Vec<&str> = sentence.split_whitespace().collect();
            for (w, word) in words.iter().enumerate() {
                tone(word, voice, &mut samples);
                if w + 1 < words.len() {
                    samples.extend(Pcm::silence(SAMPLE_RATE, WORD_GAP_MS));
                }
            }
            if s + 1 < sentences.len() {
                samples.extend(Pcm::silence(SAMPLE_RATE, SENTENCE_GAP_MS));
            }
        }
        Ok(Pcm {
            sample_rate: SAMPLE_RATE,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_realistic() {
        let text =
            "Canals are among the oldest pieces of engineered infrastructure. They still work.";
        let a = FakeEngine.synthesize(text, "bf_emma").unwrap();
        assert_eq!(a, FakeEngine.synthesize(text, "bf_emma").unwrap());
        assert_ne!(a, FakeEngine.synthesize(text, "bm_george").unwrap());
        // 13 words at a speaking pace: between 120 and 220 words a minute.
        let minutes = a.duration_ms() as f64 / 60_000.0;
        let wpm = 13.0 / minutes;
        assert!((120.0..220.0).contains(&wpm), "{wpm} wpm");
        assert!(a.samples.iter().any(|s| s.unsigned_abs() > 4_000), "silent");
    }

    #[test]
    fn unknown_voices_are_refused() {
        assert_eq!(
            FakeEngine.synthesize("Hi.", "nobody"),
            Err(SpeechError::UnknownVoice("nobody".into()))
        );
        assert_eq!(FakeEngine.fingerprint("nobody"), None);
        assert_eq!(
            FakeEngine.fingerprint("bf_emma").as_deref(),
            Some("fake@1/bf_emma")
        );
    }
}
