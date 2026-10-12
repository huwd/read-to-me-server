# Phase 0: Kokoro TTS in Rust

Throwaway spike for [issue #17](https://github.com/huwd/read-to-me-server/issues/17). It tests riskiest assumption 2 in [doc 02](../../docs/02-product-viability.md), "is Kokoro pleasant enough for 30+ minutes?", and ADR-0007's validation, "is the Rust path as good as the Python reference?". It's a standalone Cargo workspace, kept out of the product's lockfile and gates, and is deleted after the spike.

## What's here

| Path | What it is |
|---|---|
| `src/main.rs` | The Rust tool. It runs text through paragraph and sentence chunking, `misaki-rs` pronunciation and Kokoro-82M ONNX via `ort`, and writes a 24 kHz WAV. It prints speed, memory and unknown-phoneme counts. `TTS_PHONEMES_ONLY=1` prints phonemes per paragraph; `TTS_DEBUG=1` prints each chunk. |
| `vendor-misaki.sh` | Vendors `misaki-rs` 0.6.0 with the upstream Misaki lexicons and applies the `misaki-rs-*.patch` files. |
| `misaki-rs-keep-dashes.patch` | Keeps "—" and "…" as Kokoro tokens; `misaki-rs` turns them into spaces. |
| `misaki-rs-fallback-trim.patch` | Removes the full stop espeak appends to fallback words. |
| `pronunciations.tsv` | Pronunciation overrides, applied as Misaki inline syntax `[word](/phonemes/)`. |
| `hard-cases.txt` | 40 sentences of heteronyms, names, numbers, dates, money and abbreviations. |
| `reference/` | Throwaway Python (uv): `reference.py` renders with the official Python Kokoro, `asr.py` scores word error rate with faster-whisper, and `phonemes.py` with `parity.py` compares phonemes word by word. |

Model files (`models/`), audio (`out/`) and the vendored crate (`vendor/`) are git-ignored.

## Setup

```sh
cd spike/tts
# Models: onnx-community/Kokoro-82M-v1.0-ONNX (Apache-2.0) into models/: model.onnx,
# tokenizer.json, config.json and voices/<voice>.bin. Record checksums in models/SHA256SUMS.
(cd reference && uv sync)        # Python reference; supplies the upstream Misaki lexicons
./vendor-misaki.sh
cargo build --release            # on NixOS: inside a shell with cmake, clang, libclang and ffmpeg
./target/release/tts-spike --text FILE --out out/file.wav --voice bf_emma --lang gb
```

`misaki-rs` builds espeak-ng from source for its fallback, which needs cmake and libclang. `ort` downloads ONNX Runtime at build time; it's configured to use rustls rather than OpenSSL.

## Results (2026-10-12)

The machine is an AMD Ryzen AI 9 HX 370 (24 threads, 62 GB RAM), running on the CPU only, with no GPU.

### Speed and memory

These figures are for the MacMillan essay: 3,524 words, 21.5 minutes of audio.

| Model | Threads | Speed (× real time) | Peak memory |
|---|---|---|---|
| Full precision (`model.onnx`, 326 MB) | 24 | **7.0×** | 1.4 GB |
| Full precision | 4 | **5.4×** | 1.4 GB |
| 8-bit quantised (`model_quantized.onnx`, 92 MB) | 24 | 2.4× | 1.5 GB |

- **NFR-P3 (≥ 3× on CPU) is met** even on 4 threads. At that rate a 30,000-word essay (about 3 h 15 min of audio) takes about 36 minutes, inside NFR-P2b's 65.
- **NFR-P7 (≤ 2 GB) is met,** but one render (the Wright essay) peaked at 1.73 GB. That's worth watching.
- **The quantised model is about 3× slower on this CPU.** Use full precision.
- **The speaking rate is about 164 words a minute with Emma.** George reads about 8% slower.

### Rust against the Python reference

These figures are for the Lanchester essay (5,209 words, about 30 minutes) read by Emma, on 24 threads. The word error rate comes from faster-whisper `small.en` against the source text.

| Version | Word error rate | Speed | Peak memory | Model load |
|---|---|---|---|---|
| Python reference (`kokoro`, PyTorch on CPU) | 2.46% | 4.6× | 2.5 GB | 9.7 s |
| Rust v1: published `misaki-rs` | 2.85% | 6.3× | 1.5 GB | 0.6 s |
| Rust v2: upstream lexicons, over-broad symbol mapping | 3.55% | 6.5× | 1.4 GB | 0.6 s |
| **Rust v3: all fixes below** | **2.65%** | **7.0×** | **1.4 GB** | **0.4 s** |

On the 40 hard-case sentences, Rust scores 6.8% and Python 7.3%.

**Phoneme parity with Python Misaki:**
- the essay: **95.8%** of words identical, up from 94.3% before the dash and fallback fixes;
- the hard cases: **89.0%**.

The ADR-0007 pass criterion is intelligibility within 1 percentage point of the reference. **Rust v3 passes,** at 0.19 points worse on the essay and slightly better on the hard cases.

### What the spike fixed

| Problem | Effect | Fix |
|---|---|---|
| Typographic quotes and apostrophes (’ “ ”) unknown to `misaki-rs` | 66 unknown phonemes in one essay: "couldn’t", "Germany’s" | Normalise to ASCII before pronunciation |
| **The published `misaki-rs` ships espeak-generated dictionaries,** not Misaki's | "Covid" as "COV-id", wrong stress on "coronavirus" | Vendor with the upstream Misaki lexicons |
| Diphthongs written as IPA pairs joined by U+200D | Kokoro got two vowels instead of one diphthong symbol | Map espeak-style pairs to Kokoro symbols (`A I Q O W Y ʤ ʧ`) |
| Over-broad symbol mapping (v2) | Garbled ordinary words; missing words up from 29 to 67 | Rewrite only espeak-style sequences |
| `misaki-rs` drops "—" and "…" | No pause or intonation at dashes | Patch: keep them as tokens |
| espeak fallback appends "." | Pauses inside sentences after unknown names | Patch: trim it |
| Spaces before punctuation, double spaces | Unlike Misaki's layout, so different pause tokens | Tidy spacing to match |
| The sentence splitter broke "3.5" | Read as "three. five" | End sentences only at punctuation followed by whitespace |
| Names not in any dictionary | "Lanchester", and "Omicron" read as "oh-MY-kron" | Pronunciation overrides |

### Listening (the owner)

- **Emma (`bf_emma`) is preferred** over George (`bm_george`).
- **v3 "sounds pretty good".** Earlier versions were noticeably worse than Python.
- **Remaining audible issue:** "mild to *moderate*" read as the verb. `misaki-rs`'s part-of-speech tagger is simpler than the spaCy tagger Python Misaki uses.

### Remaining differences from Python

See [issue #18](https://github.com/huwd/read-to-me-server/issues/18), the pronunciation parity programme:
- **Text normalisation:**
  - years ("two thousand and twenty" against "twenty twenty");
  - ordinals ("2nd");
  - "£" and "$" amounts;
  - abbreviations ("a.m.", "e.g.");
  - acronyms ("US", "UK's");
  - decimals.
- **Rules:** "the" before vowels, stress on function words, long vowels in espeak-fallback words.
- **The tagger:** heteronyms in ambiguous contexts.

## Verdicts

- **Riskiest assumption 2: held.** After the fixes, the owner finds 30-minute essays with Emma pleasant to listen to.
- **ADR-0007's Rust path is confirmed.** It's within the intelligibility criterion, 1.5× faster than Python at about half the memory, and loads in under a second. The remaining pronunciation gap is understood, and it's tracked in #18.
