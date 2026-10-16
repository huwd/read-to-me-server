// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Kokoro-82M speech engine (ADR-0007, ADR-0014): the ONNX model through ONNX
//! Runtime (`ort`), pronunciation through our `misaki-rs` fork with the upstream Misaki
//! lexicons, and the spike's text handling.
//!
//! The model session and the G2P instances live on one dedicated inference thread:
//! `ort` sessions need exclusive access to run, and misaki-rs's G2P isn't `Send`.
//! [`KokoroEngine::synthesize`] hands requests to that thread and waits for the audio.

pub mod manifest;
pub mod text;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;

use misaki_rs::{G2P, Language};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use rtm_speech::{Pcm, SpeechEngine, SpeechError, Voice};

/// Bump on any change here that alters the audio for the same text and model: it's part
/// of the fingerprint, so cached segments from older versions are never reused.
pub const ENGINE_VERSION: u32 = 1;

/// The `misaki-rs` fork commit, part of the fingerprint. Must match Cargo.toml; a test
/// checks.
pub const MISAKI_REV: &str = "8ac866e079e95acfb53b2a8a36f9c9bc8dd176c6";

const SAMPLE_RATE: u32 = 24_000;
/// The model's context is 512 positions including the two padding tokens.
const MAX_TOKENS: usize = 510;
/// Silence between chunks of one request (they split between sentences).
const CHUNK_PAUSE_MS: u32 = 150;
/// Style vectors: one 256-float row per possible token count.
const STYLE_WIDTH: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Verify(#[from] manifest::VerifyError),
    #[error("reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("loading the model: {0}")]
    Model(String),
}

struct Request {
    text: String,
    voice: String,
    reply: mpsc::Sender<Result<Pcm, SpeechError>>,
}

pub struct KokoroEngine {
    requests: Mutex<mpsc::Sender<Request>>,
    voices: Vec<Voice>,
    fingerprints: HashMap<String, String>,
}

/// The language a voice speaks, from Kokoro's naming: `b` for British, `a` for American.
fn voice_language(id: &str) -> Option<(Language, &'static str)> {
    match id.as_bytes().first()? {
        b'b' => Some((Language::EnglishGB, "en-GB")),
        b'a' => Some((Language::EnglishUS, "en-US")),
        _ => None,
    }
}

/// Phoneme → token id, from the model's tokenizer.json.
fn vocabulary(json: &str) -> Result<HashMap<char, i64>, LoadError> {
    let t: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| LoadError::Invalid(format!("tokenizer.json: {e}")))?;
    let vocab = t["model"]["vocab"]
        .as_object()
        .ok_or_else(|| LoadError::Invalid("tokenizer.json has no model.vocab".into()))?;
    Ok(vocab
        .iter()
        .filter_map(|(k, v)| Some((k.chars().next()?, v.as_i64()?)))
        .collect())
}

/// A voice file: rows of 256 little-endian f32s, one per token count.
fn styles(bytes: &[u8]) -> Result<Vec<Vec<f32>>, LoadError> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(STYLE_WIDTH * 4) {
        return Err(LoadError::Invalid("a voice file has the wrong size".into()));
    }
    Ok(bytes
        .chunks_exact(STYLE_WIDTH * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        })
        .collect())
}

/// Everything the inference thread owns.
struct Inference {
    session: Session,
    inputs: Vec<String>,
    output: String,
    vocab: HashMap<char, i64>,
    styles: HashMap<String, Vec<Vec<f32>>>,
    g2p: HashMap<&'static str, G2P>,
    overrides: Vec<(String, String)>,
}

impl Inference {
    fn phonemes(&self, g2p: &G2P, text: &str) -> Option<String> {
        g2p.g2p(text).ok().map(|(p, _)| text::kokoro_symbols(&p))
    }

    fn tokens(&self, phonemes: &str) -> Vec<i64> {
        phonemes
            .chars()
            .filter_map(|c| self.vocab.get(&c).copied())
            .collect()
    }

    /// Run the model over one window of at most [`MAX_TOKENS`] tokens.
    fn infer(&mut self, ids: &[i64], styles: &[Vec<f32>]) -> Result<Vec<f32>, SpeechError> {
        let failed = |e: ort::Error| SpeechError::Failed(e.to_string());
        let count = ids.len();
        let style = &styles[count.min(styles.len() - 1)];
        let mut padded = Vec::with_capacity(count + 2);
        padded.push(0);
        padded.extend_from_slice(ids);
        padded.push(0);
        let inputs = ort::inputs![
            self.inputs[0].as_str() => Tensor::from_array(([1usize, count + 2], padded)).map_err(failed)?,
            self.inputs[1].as_str() => Tensor::from_array(([1usize, STYLE_WIDTH], style.clone())).map_err(failed)?,
            self.inputs[2].as_str() => Tensor::from_array(([1usize], vec![1.0f32])).map_err(failed)?,
        ];
        let outputs = self.session.run(inputs).map_err(failed)?;
        let (_, wave) = outputs[self.output.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(failed)?;
        Ok(wave.to_vec())
    }

    fn synthesize(&mut self, text: &str, voice: &str) -> Result<Pcm, SpeechError> {
        let (_, lang) =
            voice_language(voice).ok_or_else(|| SpeechError::UnknownVoice(voice.into()))?;
        let text = text::apply_overrides(&text::normalise(text), &self.overrides);
        let g2p = &self.g2p[lang];
        let chunks = text::chunks(&text, |candidate| {
            self.phonemes(g2p, candidate)
                .is_some_and(|p| self.tokens(&p).len() <= MAX_TOKENS)
        });
        let styles = self
            .styles
            .get(voice)
            .ok_or_else(|| SpeechError::UnknownVoice(voice.into()))?
            .clone();
        // First every chunk's tokens (reads the G2P), then inference (needs the session
        // exclusively).
        let mut token_chunks: Vec<Vec<i64>> = Vec::new();
        for chunk in &chunks {
            let Some(phonemes) = self.phonemes(g2p, chunk) else {
                tracing::warn!(chunk = %chunk, "no phonemes; skipped");
                continue;
            };
            let unknown = phonemes.matches('❓').count();
            if unknown > 0 {
                tracing::debug!(unknown, chunk = %chunk, "unknown phonemes");
            }
            let ids = self.tokens(&phonemes);
            if ids.len() > MAX_TOKENS {
                // Only a single "word" too long for the model gets here (chunks packs
                // whole words): say all of it, in model-sized windows, rather than
                // dropping the rest and caching the shortened audio.
                tracing::warn!(tokens = ids.len(), "overlong word spoken in pieces");
            }
            if !ids.is_empty() {
                token_chunks.push(ids);
            }
        }
        let mut samples: Vec<i16> = Vec::new();
        for ids in &token_chunks {
            if !samples.is_empty() {
                samples.extend(Pcm::silence(SAMPLE_RATE, CHUNK_PAUSE_MS));
            }
            for window in token_windows(ids) {
                let wave = self.infer(window, &styles)?;
                samples.extend(
                    wave.iter()
                        .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16),
                );
            }
        }
        if samples.is_empty() {
            return Err(SpeechError::BadText(
                "nothing in the text could be spoken".into(),
            ));
        }
        Ok(Pcm {
            sample_rate: SAMPLE_RATE,
            samples,
        })
    }
}

/// A chunk's tokens in windows the model can take. Every token is in exactly one
/// window, in order: nothing is dropped.
fn token_windows(ids: &[i64]) -> impl Iterator<Item = &[i64]> {
    ids.chunks(MAX_TOKENS)
}

fn short(hash: &str) -> &str {
    &hash[..12]
}

impl KokoroEngine {
    /// Load the model and voices from `dir`, after checking every file against its
    /// pinned SHA-256. `threads` is ONNX Runtime's intra-op thread count.
    pub fn load(dir: &Path, threads: usize) -> Result<KokoroEngine, LoadError> {
        manifest::verify(dir)?;
        let read = |p: &str| std::fs::read(dir.join(p)).map_err(|e| LoadError::Io(dir.join(p), e));
        let hashes: HashMap<String, String> = manifest::files()
            .into_iter()
            .map(|f| (f.local, f.sha256))
            .collect();

        let vocab = vocabulary(&String::from_utf8_lossy(&read("tokenizer.json")?))?;
        let mut voice_styles = HashMap::new();
        let mut voices = Vec::new();
        let mut fingerprints = HashMap::new();
        let overrides_tsv = include_str!("../pronunciations.tsv");
        let overrides_hash = sha256_hex(overrides_tsv.as_bytes());
        for id in manifest::voice_ids() {
            let Some((_, lang)) = voice_language(&id) else {
                continue;
            };
            let local = format!("voices/{id}.bin");
            voice_styles.insert(id.clone(), styles(&read(&local)?)?);
            fingerprints.insert(
                id.clone(),
                format!(
                    "kokoro@{ENGINE_VERSION}/model:{}/voice:{id}:{}/misaki:{}/overrides:{}/speed:1",
                    short(&hashes["model.onnx"]),
                    short(&hashes[&local]),
                    &MISAKI_REV[..12],
                    short(&overrides_hash),
                ),
            );
            voices.push(Voice {
                id,
                lang: lang.into(),
            });
        }

        let model = dir.join("model.onnx");
        let (requests, inbox) = mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), LoadError>>();
        std::thread::Builder::new()
            .name("kokoro-inference".into())
            .spawn(move || {
                let model_error = |e: ort::Error| LoadError::Model(e.to_string());
                let session = Session::builder()
                    .map_err(model_error)
                    .and_then(|b| {
                        b.with_optimization_level(GraphOptimizationLevel::Level3)
                            .map_err(|e| LoadError::Model(e.to_string()))
                    })
                    .and_then(|b| {
                        b.with_intra_threads(threads)
                            .map_err(|e| LoadError::Model(e.to_string()))
                    })
                    .and_then(|mut b| b.commit_from_file(&model).map_err(model_error));
                let session = match session {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let inputs: Vec<String> = session
                    .inputs()
                    .iter()
                    .map(|i| i.name().to_string())
                    .collect();
                let output = session.outputs()[0].name().to_string();
                if inputs.len() < 3 {
                    let _ =
                        ready_tx.send(Err(LoadError::Model("the model has too few inputs".into())));
                    return;
                }
                let mut inference = Inference {
                    session,
                    inputs,
                    output,
                    vocab,
                    styles: voice_styles,
                    g2p: HashMap::from([
                        ("en-GB", G2P::new(Language::EnglishGB)),
                        ("en-US", G2P::new(Language::EnglishUS)),
                    ]),
                    overrides: text::parse_overrides(overrides_tsv),
                };
                let _ = ready_tx.send(Ok(()));
                for request in inbox {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        inference.synthesize(&request.text, &request.voice)
                    }))
                    .unwrap_or_else(|_| Err(SpeechError::Failed("the engine panicked".into())));
                    let _ = request.reply.send(result);
                }
            })
            .map_err(|e| LoadError::Model(e.to_string()))?;
        ready_rx
            .recv()
            .map_err(|_| LoadError::Model("the inference thread stopped".into()))??;
        Ok(KokoroEngine {
            requests: Mutex::new(requests),
            voices,
            fingerprints,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl SpeechEngine for KokoroEngine {
    fn id(&self) -> &str {
        "kokoro"
    }

    fn fingerprint(&self, voice: &str) -> Option<String> {
        self.fingerprints.get(voice).cloned()
    }

    fn voices(&self) -> Vec<Voice> {
        self.voices.clone()
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn synthesize(&self, text: &str, voice: &str) -> Result<Pcm, SpeechError> {
        if !self.fingerprints.contains_key(voice) {
            return Err(SpeechError::UnknownVoice(voice.into()));
        }
        let (reply, answer) = mpsc::channel();
        let request = Request {
            text: text.to_string(),
            voice: voice.to_string(),
            reply,
        };
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(request)
            .map_err(|_| SpeechError::Failed("the inference thread has stopped".into()))?;
        answer
            .recv()
            .map_err(|_| SpeechError::Failed("the inference thread has stopped".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_misaki_revision_matches_cargo_toml() {
        assert!(include_str!("../Cargo.toml").contains(MISAKI_REV));
    }

    #[test]
    fn voice_languages_follow_kokoro_naming() {
        assert_eq!(voice_language("bf_emma").map(|v| v.1), Some("en-GB"));
        assert_eq!(voice_language("am_adam").map(|v| v.1), Some("en-US"));
        assert_eq!(voice_language("jf_alpha"), None);
    }

    #[test]
    fn voice_files_and_vocabularies_are_checked() {
        assert!(styles(&[]).is_err());
        assert!(styles(&[0u8; 100]).is_err());
        let one_row: Vec<u8> = (0..STYLE_WIDTH)
            .flat_map(|i| (i as f32).to_le_bytes())
            .collect();
        assert_eq!(styles(&one_row).unwrap()[0][3], 3.0);
        let vocab = vocabulary(r#"{"model":{"vocab":{"a":43,"ʤ":82}}}"#).unwrap();
        assert_eq!((vocab[&'a'], vocab[&'ʤ']), (43, 82));
        assert!(vocabulary("{}").is_err());
    }

    #[test]
    fn overlong_token_runs_are_windowed_not_truncated() {
        let ids: Vec<i64> = (0..1_234).collect();
        let windows: Vec<&[i64]> = token_windows(&ids).collect();
        assert_eq!(windows.len(), 3);
        assert!(
            windows
                .iter()
                .all(|w| !w.is_empty() && w.len() <= MAX_TOKENS)
        );
        assert_eq!(windows.concat(), ids, "a token was lost or reordered");
        assert_eq!(token_windows(&ids[..MAX_TOKENS]).count(), 1);
    }

    #[test]
    fn missing_model_files_are_explained() {
        let err = KokoroEngine::load(Path::new("/nonexistent/kokoro"), 1)
            .err()
            .unwrap();
        assert!(
            err.to_string().contains("cargo xtask kokoro-models"),
            "{err}"
        );
    }
}
