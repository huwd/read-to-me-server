// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real Kokoro model. Needs the model files (`cargo xtask kokoro-models <dir>`), so
//! it's ignored by default:
//! `RTM_KOKORO_DIR=<dir> cargo test -p rtm-kokoro --release --test real_model -- --ignored`

use std::path::PathBuf;

use rtm_kokoro::KokoroEngine;
use rtm_speech::SpeechEngine;

fn model_dir() -> PathBuf {
    std::env::var_os("RTM_KOKORO_DIR")
        .map(PathBuf::from)
        .expect("set RTM_KOKORO_DIR to the model directory")
}

#[test]
#[ignore = "needs the Kokoro model files"]
fn kokoro_speaks() {
    let engine = KokoroEngine::load(&model_dir(), 4).unwrap();
    assert!(engine.has_voice("bf_emma"));
    let fingerprint = engine.fingerprint("bf_emma").unwrap();
    assert!(
        fingerprint.starts_with("kokoro@1/model:8fbea51ea711/voice:bf_emma:669fe0647f9d/"),
        "{fingerprint}"
    );

    let text = "John Lanchester wrote about Omicron — and the canal's locks. It rose 3.5% today.";
    let pcm = engine.synthesize(text, "bf_emma").unwrap();
    assert_eq!(pcm.sample_rate, 24_000);
    // About fourteen words at roughly 160 a minute: four to eight seconds.
    let seconds = pcm.duration_ms() as f64 / 1000.0;
    assert!((3.0..9.0).contains(&seconds), "{seconds} s");
    assert!(
        pcm.samples.iter().any(|s| s.unsigned_abs() > 3_000),
        "silent"
    );
    if let Some(out) = std::env::var_os("RTM_KOKORO_WAV") {
        std::fs::write(out, pcm.to_wav()).unwrap();
    }

    // Deterministic for the same input, as segment caching assumes.
    assert_eq!(engine.synthesize(text, "bf_emma").unwrap(), pcm);
    assert!(engine.synthesize("Hello.", "nobody").is_err());
}

/// A "word" too long for the model is spoken in full, not cut off: its audio grows
/// with its length. A long hyphenated run has no spaces, so it's one chunk, but its
/// parts are dictionary words, so its phonemes go far past the 510-token limit.
#[test]
#[ignore = "needs the Kokoro model files"]
fn an_overlong_word_is_not_truncated() {
    let engine = KokoroEngine::load(&model_dir(), 4).unwrap();
    let word = |n: usize| vec!["the-cat-sat-on-the-mat"; n].join("-");
    let short = engine
        .synthesize(&word(10), "bf_emma")
        .unwrap()
        .duration_ms();
    let long = engine
        .synthesize(&word(40), "bf_emma")
        .unwrap()
        .duration_ms();
    // Four times the tokens gives about 3.4 times the audio (fixed overhead at the
    // edges); truncated at 510 tokens it gives about 1.7 times.
    assert!(long * 10 > short * 25, "short {short} ms, long {long} ms");
}
