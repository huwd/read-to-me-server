# ADR-0015: Container images and the compose stack

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-tts-engine-rust-kokoro.md) (Kokoro as a separate process), [ADR-0009](0009-audio-format-and-feed.md) (audio format), [ADR-0014](0014-kokoro-dependencies-and-build.md) (Kokoro's native build), [issue #25](https://github.com/huwd/read-to-me-server/issues/25) (walking skeleton slice 7), [doc 04](../04-tech-stack.md) (container base), [doc 05](../05-quality-cicd-observability.md) §2 (CI), [doc 06](../06-security-and-pentest.md) §2 (hardening)

## Context

The walking skeleton runs end to end from source. Self-hosters, and the owner's own instance, need it as containers instead:
- **[Doc 04](../04-tech-stack.md)** asks for a distroless or similar base, non-root, with a read-only root filesystem.
- **[Doc 05](../05-quality-cicd-observability.md) §2.1** sets a budget of at most 50 MB for the server image, excluding TTS, and asks for an end-to-end test on a compose stack on every PR.
- **[Doc 06](../06-security-and-pentest.md) §2** adds: drop all capabilities, and set `no-new-privileges`.

Two parts of the system don't fit a minimal image without a decision.

**ffmpeg.** The encode stage ([ADR-0009](0009-audio-format-and-feed.md)) runs ffmpeg for loudness normalisation and MP3 encoding.
- **A distribution package** brings in hundreds of codecs, formats and libraries we never use: over 100 MB on its own.
- **Its attack surface:** every demuxer and decoder it carries is parser code that could, in principle, be reached.
- **Scanner noise:** each of those libraries is something a vulnerability scanner (Trivy or Grype, [doc 05](../05-quality-cicd-observability.md)) will report on.

**The Kokoro model files.** The TTS process needs about 330 MB of pinned model and voice files ([ADR-0007](0007-tts-engine-rust-kokoro.md)). They could be:
- baked into the image;
- downloaded into a volume on first start;
- fetched into a volume by hand.

## Decision

### Two images from one Dockerfile
- **`rtm`** (the default target) has the server, the extract worker (the same binary, [ADR-0006](0006-extractor-triage-contract.md)) and ffmpeg. It's built without the `kokoro` feature: it talks to the TTS process over HTTP.
- **`tts`** has `rtm` built with `--features kokoro` ([ADR-0014](0014-kokoro-dependencies-and-build.md)), running `rtm tts`.
- **Both run on `gcr.io/distroless/cc-debian13:nonroot`:** glibc and libstdc++, which ONNX Runtime needs, but no shell or package manager. They run as the `nonroot` user.
- **Base images are pinned by multi-arch index digest** in the `FROM` lines, and Dependabot keeps them current.
- **The Rust build uses `cargo-chef`,** so dependencies are cached in their own layer.
- **The build context is an allowlist** (`.dockerignore`): only the workspace sources. Local data, model files and the spike's corpus can't leak in.

### ffmpeg is built from source with only what the encode stage uses
- **The release tarball is pinned by SHA-256,** recorded after checking the tarball's signature against the FFmpeg release key.
- **It's built statically with `--disable-everything`, then only these enabled:**
  - the file protocol;
  - the WAV demuxer and 16-bit PCM decoder;
  - the `loudnorm` and `aresample` filters, plus the plumbing filters ffmpeg inserts;
  - the LAME MP3 encoder (Debian's static `libmp3lame`);
  - the MP3 and null muxers.
- **Network support isn't compiled in at all.** That's in addition to the `-protocol_whitelist file` the encode stage already passes.
- **The result is a 4.7 MB binary.** The encode stage's exact commands produce the same output with it as with a full build: MP3, 44.1 kHz, mono, 64 kbps, −16.4 LUFS.
- **Cost:** the configure line is ours to maintain. A future encode feature, such as another format or filter, has to be enabled there too. The end-to-end test catches anything missing.

### The model files are baked into the `tts` image
- **The `models` build stage** runs `cargo xtask kokoro-models`, the same tool developers use. It downloads each file at the pinned Hugging Face revision and verifies it against `crates/rtm-kokoro/models.sha256`.
- **The image's signature and provenance then cover the weights** as well as the code. That closes the model-file supply-chain gap in [doc 06](../06-security-and-pentest.md)'s [T1195](../06-security-and-pentest.md#t1195-001) row.
- **The image works offline,** with no first-start download.
- **Cost:** the `tts` image is about 400 MB. It's pulled once per version, and the model layer stays cached across versions until the pins change.

### The compose stack
- **`compose.yaml`** runs the `rtm` and `tts` services. Each is:
  - read-only, with all capabilities dropped and `no-new-privileges`;
  - given a `tmpfs` at `/tmp`;
  - restarted `unless-stopped` ([NFR-R4](../03-architecture-and-nfrs.md#nfr-r4)).
- **The speech service is on an `internal` network:**
  - the server can reach it;
  - nothing else can, and it has no route out. It needs none, because the model is in the image.
- **Configuration comes from `.env`** (`.env.example` documents it). The server's port binds to `127.0.0.1` unless `RTM_BIND` says otherwise.
- **Healthchecks use `rtm healthcheck ADDR`,** a small role of the binary. It asks `/healthz` over a plain TCP connection, since the images have no curl. The server starts only once the speech service is healthy.

### End-to-end test on every PR
- **`e2e/compose.yaml` is layered over `compose.yaml`:**
  - both services run the `rtm` image under test, with the fake speech engine for speed;
  - an nginx container serves a fixture article on a private network, which the egress guard is explicitly told to allow (`RTM_FETCH_UNSAFE_ALLOW`), as an intranet would be.
- **`e2e/run.sh` drives it:**
  1. submit the fixture URL;
  2. wait for `published`;
  3. fetch the feed with a new feed token;
  4. download the enclosure, and check that Range requests work;
  5. check the MP3 against [ADR-0009](0009-audio-format-and-feed.md) with ffprobe and ffmpeg's loudness meter.
- **The server image's size budget is measured compressed:** what a pull transfers, as registries report it. CI fails it over 50 MB. It's 22 MB today.
- **CI also builds the `tts` image on every PR,** with the layer cache, so a broken Kokoro build or model pin is caught before merge.

## Consequences

- **Self-hosters get one command,** `docker compose up -d`, with hardening on by default.
- **The server image is small and narrow:** a shell-less base, a 23 MB binary, and an ffmpeg that can only do what we use it for.
- **Every PR exercises the real image end to end:** the egress guard, the sandboxed extractor, the encoder and the feed, all as shipped.
- **The fake engine stands in for Kokoro in the end-to-end test.** Real speech quality stays with the nightly real-model test ([doc 05](../05-quality-cicd-observability.md) §2.4).
- **Publishing isn't covered here.** That means multi-arch builds, signing, SBOMs, provenance and the registry, which come with the publishing workflow.
- ⚑ **Hosting impact:** none. A hosted service would run the same images under an orchestrator.
