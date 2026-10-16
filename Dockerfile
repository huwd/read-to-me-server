# syntax=docker/dockerfile:1
# Container images (ADR-0015). Two targets:
#   rtm  the server, extract worker and a minimal ffmpeg (≤ 50 MB, doc 05 §2.1)
#   tts  `rtm tts` with the Kokoro engine and its pinned model files
# Base images are pinned by multi-arch index digest; Dependabot keeps them current.
# Builds run natively on each architecture, never under emulation.

# ---- Rust build, with dependencies cached separately from the source (cargo-chef) ----

FROM rust:1.97.0-slim-trixie@sha256:686a437ead83701e8f871e66e838c3ec55f46b5fc235b025756396ac823bdc51 AS chef
# The image's toolchain is the one rust-toolchain.toml names; this stops rustup
# fetching the lint components the file lists, which builds don't need.
ENV RUSTUP_TOOLCHAIN=1.97.0 \
    CARGO_TERM_COLOR=never \
    CARGO_PROFILE_RELEASE_STRIP=symbols
RUN cargo install --locked cargo-chef@0.1.78
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build-rtm
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --package rtm-bin --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --package rtm-bin \
 && install -D -m 0755 target/release/rtm /out/rtm \
 && mkdir -p /out/data

FROM chef AS build-tts
# espeak-ng builds from source (cmake, clang) and clones sonic at a pinned commit
# (git); ort fetches ONNX Runtime, checked against its pinned SHA-256 (ADR-0014).
# espeak-rs-sys formats its generated bindings with rustfmt.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake make clang libclang-dev git \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add rustfmt
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --package rtm-bin --features rtm-bin/kokoro \
      --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --package rtm-bin --features rtm-bin/kokoro \
 && install -D -m 0755 target/release/rtm /out/rtm

# The Kokoro model files, downloaded at the pinned revision and verified against
# crates/rtm-kokoro/models.sha256 by the same tool developers use.
FROM chef AS models
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo xtask kokoro-models /models

# ---- ffmpeg: only what the encode stage uses (ADR-0009, ADR-0015) ----

FROM rust:1.97.0-slim-trixie@sha256:686a437ead83701e8f871e66e838c3ec55f46b5fc235b025756396ac823bdc51 AS ffmpeg
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ARG FFMPEG_VERSION=9.0.2
# The release tarball's SHA-256, after checking its signature against the FFmpeg
# release key (FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8).
ARG FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates xz-utils make \
      libmp3lame-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /ffmpeg
RUN curl -sSfL -o ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
 && echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c - \
 && tar -xJf ffmpeg.tar.xz --strip-components=1 \
 && rm ffmpeg.tar.xz
# Static, audio only: WAV in, loudness normalisation, MP3 (LAME) out, local files
# only. Everything else, including the network, is compiled out.
RUN ./configure \
      --disable-everything --disable-autodetect --disable-network \
      --disable-doc --disable-debug --disable-ffplay --disable-ffprobe \
      --disable-avdevice --disable-x86asm \
      --enable-static --disable-shared --extra-ldflags=-static \
      --enable-libmp3lame \
      --enable-protocol=file \
      --enable-demuxer=wav \
      --enable-decoder=pcm_s16le \
      --enable-filter=abuffer,abuffersink,aformat,anull,aresample,loudnorm \
      --enable-encoder=libmp3lame,pcm_s16le \
      --enable-muxer=mp3,null \
 && make -j"$(nproc)" ffmpeg \
 && strip ffmpeg \
 && install -D -m 0755 ffmpeg /out/ffmpeg

# ---- Runtime images ----

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2 AS tts
LABEL org.opencontainers.image.title="read-to-me tts" \
      org.opencontainers.image.description="Kokoro speech synthesis for read-to-me" \
      org.opencontainers.image.source="https://github.com/huwd/read-to-me-server" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later AND Apache-2.0"
COPY --from=build-tts /out/rtm /usr/local/bin/rtm
COPY --from=models /models /models
ENV RTM_TTS_ENGINE=kokoro \
    RTM_KOKORO_DIR=/models \
    RTM_TTS_LISTEN_ADDR=0.0.0.0:8090
USER nonroot
EXPOSE 8090
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s \
  CMD ["/usr/local/bin/rtm", "healthcheck", "127.0.0.1:8090"]
ENTRYPOINT ["/usr/local/bin/rtm"]
CMD ["tts"]

# The default target.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2 AS rtm
LABEL org.opencontainers.image.title="read-to-me" \
      org.opencontainers.image.description="Read-it-later articles as a private podcast feed" \
      org.opencontainers.image.source="https://github.com/huwd/read-to-me-server" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=ffmpeg /out/ffmpeg /usr/local/bin/ffmpeg
COPY --from=build-rtm /out/rtm /usr/local/bin/rtm
COPY --from=build-rtm --chown=nonroot:nonroot /out/data /data
ENV RTM_DATA_DIR=/data \
    RTM_LISTEN_ADDR=0.0.0.0:8080 \
    RTM_FFMPEG=/usr/local/bin/ffmpeg
USER nonroot
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=10s \
  CMD ["/usr/local/bin/rtm", "healthcheck", "127.0.0.1:8080"]
ENTRYPOINT ["/usr/local/bin/rtm"]
CMD ["server"]
