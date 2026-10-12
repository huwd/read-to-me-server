# ADR-0007: Default TTS engine: Kokoro-82M in Rust (ONNX Runtime), as a separate process

- **Status:** Accepted (Phase 0 spike to confirm quality; see "Validation")
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 03 §2](../03-architecture-and-nfrs.md#2-architectural-style), §7 ([NFR-P2](../03-architecture-and-nfrs.md#nfr-p2)/[P3](../03-architecture-and-nfrs.md#nfr-p3)/[P7](../03-architecture-and-nfrs.md#nfr-p7)); [doc 04 §3](../04-tech-stack.md#3-tts-process-rust--see-adr-0007); [ADR-0010](0010-licence-and-third-party-policy.md) (licences, model-weights policy)

## Context

The first draft put TTS in a Python service wrapping Kokoro's reference implementation (PyTorch + the `misaki` G2P), with a plan to move to Rust later. The argument was quality parity from day one. Against that:
- PyTorch images are gigabytes.
- Python packaging adds a second toolchain and supply chain.
- The owner wants a Rust project.
- Kokoro has an ONNX export, and the `ort` crate can run it from Rust; community Rust ports (e.g. Kokoros) already do this.

The real risk in Rust is **G2P (grapheme-to-phoneme) quality**: Rust ports typically lean on espeak-ng rather than `misaki`, which can mispronounce names and heteronyms, and stress words wrongly. That needs measuring, not guessing.

## Decision

1. **Default engine: Kokoro-82M (Apache-2.0 weights), run from Rust through ONNX Runtime via the `ort` crate.** No Python in any shipped image.
2. **Separate process**, built from the same Cargo workspace (`rtm --role tts`, or a small `rtm-tts` binary), running as its own container in the compose bundle.
   - ONNX Runtime is native C++, so a crash or memory spike can't take down the API and feed server ([NFR-R3](../03-architecture-and-nfrs.md#nfr-r3)).
   - Memory is bounded separately ([NFR-P7](../03-architecture-and-nfrs.md#nfr-p7)).
   - A `tts-cuda` image variant (ONNX Runtime CUDA execution provider) can be swapped in by self-hosters with a GPU. `tts-cpu` is the default.
3. **Contract unchanged:** the `SpeechEngine` port talks HTTP over the internal compose network (Unix socket optional): `POST /synthesize`, `GET /voices`, `/healthz`.
4. **G2P in Rust:**
   - Start with espeak-ng (via FFI bindings or subprocess; GPL-3.0-or-later, compatible with AGPL per [ADR-0010](0010-licence-and-third-party-policy.md)) plus our own lexicon overrides for common problem words.
   - If the spike shows a gap, port `misaki`'s lexicon-based English G2P to Rust (check the licence of its dictionary data first).
5. **Model and voice artefacts:**
   - ONNX model and voice embeddings pinned by SHA-256.
   - Kokoro's upstream voice packs are PyTorch `.pt` files (pickle). We **never load pickle at runtime**. A one-off, sandboxed conversion step produces `safetensors`/`.npy` voice files, which are hashed, published as release artefacts, and are the only form the engine loads.
6. **ONNX Runtime binaries** are pinned and checksum-verified in the image build. We don't let the build silently download "latest".

## Validation (Phase 0 spike)

A **throwaway comparison tool** (`tools/tts-compare/`, Python allowed, never shipped or packaged, excluded from release images) synthesises the same inputs through:
- (a) the Python reference implementation;
- (b) our Rust/ONNX path.

| Measure | Method | Pass for Rust path |
|---------|--------|--------------------|
| Intelligibility | ASR (Whisper) word-error rate on 5 long articles | Within 1 percentage point of reference |
| Hard cases | 100-sentence set: names, heteronyms, numbers, dates, abbreviations | ≤ 5 more ASR errors than reference |
| Listening | Blind A/B on 10 paragraphs | No consistent preference for reference |
| Performance | Real-time factor and peak RSS on the reference machines | Meets [NFR-P3](../03-architecture-and-nfrs.md#nfr-p3)/[P7](../03-architecture-and-nfrs.md#nfr-p7) |

If Rust passes, the comparison tool is deleted after the spike. If it fails on pronunciation, the remedy is improving G2P in Rust (decision step 4), not adopting Python. Shipping a Python engine would need a new ADR.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Python service (previous plan) | Large images, a second toolchain and supply chain, against the owner's preference. Kept only as the spike's reference. |
| TTS in-process inside the main server | A native-code crash would take down feeds; can't swap a GPU image independently. |
| Pure-Rust ML (e.g. `candle`) instead of ONNX Runtime | Would avoid the C++ dependency, but there's no mature Kokoro port we know of. Worth revisiting later behind the same port. |
| Piper as default | Lighter and Pi-friendly, but noticeably less natural for long-form listening. Remains an optional engine. |

## Consequences

**Positive**
- One language and toolchain; much smaller images and memory footprint; simpler supply chain.
- Crash isolation and an independent GPU image.

**Negative**
- We own more inference glue (G2P, voice loading, chunking) than if we wrapped the reference code.
- ONNX Runtime is a native dependency we must build/pin for amd64 and arm64.
- Pronunciation may lag the reference until G2P work is done.

**Licensing** (per [ADR-0010](0010-licence-and-third-party-policy.md))
- Kokoro weights: Apache-2.0.
- ONNX Runtime: MIT.
- espeak-ng: GPL-3.0-or-later (compatible with AGPL).
- Converted voice files keep the upstream Apache-2.0 licence and attribution.

## Amendment 1 (2026-10-12): validated by the Phase 0 spike

The spike ([`spike/tts/`](../../spike/tts/README.md), [issue #17](https://github.com/huwd/read-to-me-server/issues/17)) ran Kokoro-82M from Rust via `ort` on a CPU-only Ryzen AI 9 HX 370 and compared it with the Python reference.

**Results:**
- **Intelligibility:** the Rust path's speech-to-text word error rate on a 30-minute essay was 2.65%, against Python's 2.46%. On 40 hard-case sentences it was 6.8% against 7.3%. Both are within this ADR's 1-point pass criterion.
- **Speed:** 7.0× real time on 24 threads and 5.4× on 4 threads, against Python's 4.6×.
- **Memory and start-up:** peak 1.4–1.7 GB against Python's 2.5 GB; model load 0.4 s against 9.7 s.
- **Listening:** the owner found 30-minute essays pleasant after the fixes below.

**Decisions:**
- **The Rust path is confirmed.** Python is used only to generate test expectations (see below), never shipped.
- **Model:** full precision (`model.onnx`) from `onnx-community/Kokoro-82M-v1.0-ONNX`, pinned by checksum. The 8-bit quantised model was about 3× slower on this CPU.
- **Voices:** load the `voices/*.bin` files from that repository. They're raw little-endian float32 arrays (510 × 256), so the planned conversion from pickled `.pt` voice packs isn't needed. **The default voice is `bf_emma`** (British, female), chosen by the owner over `bm_george`.
- **Pronunciation (amends Decision step 4):** use `misaki-rs`, a Rust port of Misaki, but carrying the **upstream Misaki lexicons**. The published crate ships dictionaries regenerated with espeak-ng, which mispronounce common words ("Covid", "coronavirus") and write diphthongs in a notation Kokoro's vocabulary lacks.
  - The spike vendors the crate with the upstream data and two patches: keeping "—" and "…" as prosody tokens, and trimming the full stop espeak appends to fallback words.
  - espeak-ng remains the fallback for words in no dictionary.
- **Text before pronunciation:** typographic quotes and apostrophes are normalised to ASCII, and names in no dictionary use pronunciation overrides (Misaki's inline `[word](/phonemes/)` syntax).
- **Python Misaki is the pronunciation test oracle.** Phonemes still match on only 89–96% of words. Closing the gap is a tracked programme ([issue #18](https://github.com/huwd/read-to-me-server/issues/18)):
  - fixtures generated by pinned Python Misaki, with a ratcheted CI parity threshold;
  - our own normaliser for numbers, years, currency, abbreviations and acronyms;
  - rule patches in a `misaki-rs` fork, offered upstream;
  - tagger work last.

**Known limitations:**
- `misaki-rs`'s part-of-speech tagger is simpler than the spaCy tagger Python Misaki uses, so some heteronyms are wrong in ambiguous contexts ("mild to *moderate*").
- Synthesis chunks are sentence-sized, so intonation resets between chunks.
