// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 0 spike (issue #17): Kokoro-82M in Rust, as ADR-0007 proposes.
//!
//! text → paragraphs → sentence chunks (≤ 510 tokens) → misaki-rs phonemes → Kokoro ONNX
//! (via `ort`) → 24 kHz mono WAV with short pauses between chunks and paragraphs.
//!
//! Usage:
//!   tts-spike --text FILE --out FILE.wav [--model models/model.onnx] [--voice bf_emma]
//!             [--lang gb|us] [--speed 1.0] [--threads N]
//!
//! Prints one JSON line of stats: audio seconds, synthesis seconds, real-time factor,
//! chunks, unknown-phoneme count and peak RSS.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use misaki_rs::{G2P, Language};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

const SAMPLE_RATE: u32 = 24_000;
/// Model context is 512 positions including the two pad tokens.
const MAX_PHONEMES: usize = 500;
const SENTENCE_PAUSE_MS: u32 = 150;
const PARAGRAPH_PAUSE_MS: u32 = 500;

struct Args {
    text: PathBuf,
    out: PathBuf,
    model: PathBuf,
    voice: String,
    lang: Language,
    speed: f32,
    threads: usize,
}

fn args() -> Args {
    let mut a = Args {
        text: PathBuf::new(),
        out: PathBuf::from("out/out.wav"),
        model: PathBuf::from("models/model.onnx"),
        voice: "bf_emma".into(),
        lang: Language::EnglishGB,
        speed: 1.0,
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--text" => a.text = v.into(),
            "--out" => a.out = v.into(),
            "--model" => a.model = v.into(),
            "--voice" => a.voice = v,
            "--lang" => a.lang = if v == "us" { Language::EnglishUS } else { Language::EnglishGB },
            "--speed" => a.speed = v.parse().expect("speed"),
            "--threads" => a.threads = v.parse().expect("threads"),
            other => panic!("unknown flag {other}"),
        }
    }
    assert!(!a.text.as_os_str().is_empty(), "--text is required");
    a
}

/// Phoneme → token id, from the model's tokenizer.json.
fn vocab(models: &std::path::Path) -> HashMap<char, i64> {
    let t: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(models.join("tokenizer.json")).expect("tokenizer.json"))
            .expect("tokenizer json");
    t["model"]["vocab"]
        .as_object()
        .expect("vocab")
        .iter()
        .filter_map(|(k, v)| Some((k.chars().next()?, v.as_i64()?)))
        .collect()
}

/// Voice styles: 510 rows × 256 floats; row = number of phoneme tokens.
fn voice(models: &std::path::Path, name: &str) -> Vec<[f32; 256]> {
    let bytes = std::fs::read(models.join("voices").join(format!("{name}.bin"))).expect("voice file");
    bytes
        .chunks_exact(256 * 4)
        .map(|row| {
            let mut out = [0f32; 256];
            for (i, b) in row.chunks_exact(4).enumerate() {
                out[i] = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            out
        })
        .collect()
}

/// Split a paragraph into sentence-ish pieces, then pack them into chunks whose phoneme
/// strings fit the model. Overlong single sentences are split at commas, then words.
/// Sentences end at `.`, `!`, `?` or `…` (plus any closing quotes or brackets) followed by
/// whitespace, so decimals like "3.5" and "e.g." mid-word stay intact.
fn sentences(paragraph: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = paragraph.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i].1, '.' | '!' | '?' | '…') {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j].1, '.' | '!' | '?' | '…' | '"' | '\'' | '”' | '’' | ')' | ']') {
                j += 1;
            }
            if j == chars.len() || chars[j].1.is_whitespace() {
                let end = if j == chars.len() { paragraph.len() } else { chars[j].0 };
                out.push(&paragraph[start..end]);
                start = end;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    if start < paragraph.len() {
        out.push(&paragraph[start..]);
    }
    out
}

fn chunks(g2p: &G2P, paragraph: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let fits = |s: &str| g2p.g2p(s).map(|(p, _)| p.chars().count() <= MAX_PHONEMES).unwrap_or(false);
    for s in sentences(paragraph) {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        let candidate = if current.is_empty() { s.to_string() } else { format!("{current} {s}") };
        if fits(&candidate) {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if fits(s) {
            current = s.to_string();
        } else {
            // Overlong sentence: greedily pack words.
            for word in s.split_whitespace() {
                let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
                if fits(&candidate) {
                    current = candidate;
                } else {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    current = word.to_string();
                }
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Pronunciation overrides: `word<TAB>phonemes` lines (`#` comments). Missing file = none.
fn overrides(path: &std::path::Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t').map(|(w, p)| (w.trim().to_string(), p.trim().to_string())))
        .collect()
}

/// Rewrite whole-word matches as Misaki inline pronunciations: `[word](/phonemes/)`.
fn apply_overrides(text: &str, overrides: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (word, phonemes) in overrides {
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(word.as_str()) {
            let before = rest[..i].chars().next_back();
            let after = rest[i + word.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            result.push_str(&rest[..i]);
            if boundary(before) && boundary(after) {
                result.push_str(&format!("[{word}](/{phonemes}/)"));
            } else {
                result.push_str(word);
            }
            rest = &rest[i + word.len()..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}

/// Map IPA diphthongs and affricates (as produced by espeak, joined with U+200D or not) to
/// the single symbols in Kokoro's vocabulary, following Misaki's conventions.
fn kokoro_symbols(p: &str) -> String {
    let p = p.replace('\u{200D}', "");
    [
        ("eɪ", "A"), ("aɪ", "I"), ("əʊ", "Q"), ("oʊ", "O"), ("aʊ", "W"), ("ɔɪ", "Y"),
        ("dʒ", "ʤ"), ("tʃ", "ʧ"), ("əl", "ᵊl"), ("iə", "ɪə"), ("eə", "ɛː"),
    ]
    .iter()
    .fold(p, |acc, (from, to)| acc.replace(from, to))
}

/// Minimal text normalisation (FR-TTS-02 will own this properly): typographic quotes and
/// apostrophes to ASCII, which misaki-rs otherwise marks as unknown, and odd spaces to plain.
fn normalise(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{02BC}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' | '\u{00AB}' | '\u{00BB}' => '"',
            '\u{00A0}' | '\u{2009}' | '\u{200A}' | '\u{202F}' => ' ',
            '\u{200B}' | '\u{00AD}' => '\u{0}',
            other => other,
        })
        .filter(|&c| c != '\u{0}')
        .collect()
}

fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).map(str::to_owned))
        .and_then(|l| l.split_whitespace().nth(1).and_then(|kb| kb.parse::<f64>().ok()))
        .map_or(0.0, |kb| kb / 1024.0)
}

fn main() {
    let a = args();
    let models = a.model.parent().expect("model dir").to_path_buf();
    let vocab = vocab(&models);
    let styles = voice(&models, &a.voice);
    let g2p = G2P::new(a.lang);

    let load = Instant::now();
    let builder = Session::builder().expect("session builder");
    let builder = builder
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())
        .expect("optimisation level");
    let mut builder = builder
        .with_intra_threads(a.threads)
        .map_err(|e| e.to_string())
        .expect("threads");
    let mut session = builder.commit_from_file(&a.model).expect("load model");
    let load_s = load.elapsed().as_secs_f64();
    let input_names: Vec<String> = session.inputs().iter().map(|i| i.name().to_string()).collect();
    let output_name = session.outputs()[0].name().to_string();

    let text = normalise(&std::fs::read_to_string(&a.text).expect("text file"));
    let text = apply_overrides(&text, &overrides(std::path::Path::new("pronunciations.tsv")));
    let paragraphs: Vec<&str> = text.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect();

    let mut audio: Vec<f32> = Vec::new();
    let mut n_chunks = 0usize;
    let mut unknown = 0usize;
    let mut skipped = 0usize;
    let synth = Instant::now();
    for (pi, paragraph) in paragraphs.iter().enumerate() {
        for (ci, chunk) in chunks(&g2p, paragraph).iter().enumerate() {
            let Ok((phonemes, _)) = g2p.g2p(chunk) else {
                skipped += 1;
                continue;
            };
            let phonemes = kokoro_symbols(&phonemes);
            unknown += phonemes.matches('❓').count();
            if std::env::var_os("TTS_DEBUG").is_some() {
                eprintln!("TEXT: {chunk}\nPHON: {phonemes}\n");
            }
            let mut ids: Vec<i64> = vec![0];
            ids.extend(phonemes.chars().filter_map(|c| vocab.get(&c).copied()));
            ids.truncate(MAX_PHONEMES + 1);
            let n_tokens = ids.len() - 1;
            ids.push(0);
            if n_tokens == 0 {
                skipped += 1;
                continue;
            }
            let style = styles[n_tokens.min(styles.len() - 1)];
            let len = ids.len();
            let inputs = ort::inputs![
                input_names[0].as_str() => Tensor::from_array(([1usize, len], ids)).expect("ids"),
                input_names[1].as_str() => Tensor::from_array(([1usize, 256], style.to_vec())).expect("style"),
                input_names[2].as_str() => Tensor::from_array(([1usize], vec![a.speed])).expect("speed"),
            ];
            let outputs = session.run(inputs).expect("inference");
            let (_, wave) = outputs[output_name.as_str()].try_extract_tensor::<f32>().expect("waveform");
            if ci > 0 {
                audio.extend(std::iter::repeat_n(0.0, (SAMPLE_RATE * SENTENCE_PAUSE_MS / 1000) as usize));
            }
            audio.extend_from_slice(wave);
            n_chunks += 1;
        }
        if pi + 1 < paragraphs.len() {
            audio.extend(std::iter::repeat_n(0.0, (SAMPLE_RATE * PARAGRAPH_PAUSE_MS / 1000) as usize));
        }
    }
    let synth_s = synth.elapsed().as_secs_f64();

    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir).expect("out dir");
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&a.out, spec).expect("wav");
    for s in &audio {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).expect("sample");
    }
    w.finalize().expect("finalize");

    let audio_s = audio.len() as f64 / SAMPLE_RATE as f64;
    println!(
        "{}",
        serde_json::json!({
            "text": a.text, "model": a.model, "voice": a.voice, "threads": a.threads,
            "words": text.split_whitespace().count(), "paragraphs": paragraphs.len(),
            "chunks": n_chunks, "skipped_chunks": skipped, "unknown_phonemes": unknown,
            "load_s": (load_s * 100.0).round() / 100.0,
            "audio_s": (audio_s * 10.0).round() / 10.0,
            "synth_s": (synth_s * 10.0).round() / 10.0,
            "rtf": ((audio_s / synth_s) * 100.0).round() / 100.0,
            "peak_rss_mb": peak_rss_mb().round(),
        })
    );
}
