# Kerf's headless MCP server, for hosting: remote agents connect over streamable HTTP
# and cut video through the same engine the desktop app uses. One container serves
# one project (one tenant); see deploy/README.md. Build from the repository root:
#
#   docker build -f deploy/Dockerfile -t kerf-mcp .
#
# x86_64 only: the pinned FFmpeg (scripts/fetch-ffmpeg.mjs) has no Linux arm64 build.

ARG RUST_VERSION=1.99.1
ARG BUN_VERSION=1.4.4
ARG DEBIAN_RELEASE=trixie

# ---- frontend: tauri-build embeds it at compile time, even though headless never shows it
FROM oven/bun:${BUN_VERSION}-debian AS frontend
WORKDIR /src/frontend
COPY frontend/package.json frontend/bun.lock ./
RUN bun install --frozen-lockfile
COPY frontend/ ./
RUN bun run build

# ---- the pinned, SHA-256-checked FFmpeg 9 the desktop bundles ship (no whisper filter:
# transcription is the in-process `whisper` feature below)
FROM oven/bun:${BUN_VERSION}-debian AS ffmpeg
RUN apt-get update \
    && apt-get install -y --no-install-recommends xz-utils \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY scripts/fetch-ffmpeg.mjs scripts/
RUN bun scripts/fetch-ffmpeg.mjs x86_64-unknown-linux-gnu

# ---- kerf-app, built like a release bundle: no FFmpeg dev libraries, whisper.cpp in-process
FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev cmake clang libclang-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
# whisper.cpp defaults to -march=native, which would tie the image to the build machine's
# CPU: target AVX2 / FMA / F16C (x86-64-v3, any server CPU of the last decade) instead.
ENV CARGO_INCREMENTAL=0 \
    GGML_NATIVE=OFF \
    GGML_SSE42=ON GGML_AVX=ON GGML_AVX2=ON GGML_BMI2=ON GGML_FMA=ON GGML_F16C=ON
WORKDIR /src
# Only what cargo reads, so a change under deploy/ does not rebuild the binary.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY --from=frontend /src/frontend/build frontend/build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p kerf-app --no-default-features --features whisper \
    && install -D -m 0755 target/release/kerf-app /out/kerf-app

# ---- runtime
FROM debian:${DEBIAN_RELEASE}-slim
# The binary links GTK / WebKitGTK even headless (no display is opened); tini reaps
# what ffmpeg leaves behind and forwards SIGTERM, which kerf-app shuts down on.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-0 ca-certificates fonts-dejavu-core tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ffmpeg /src/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu /usr/local/bin/ffmpeg
COPY --from=ffmpeg /src/crates/kerf-app/binaries/ffprobe-x86_64-unknown-linux-gnu /usr/local/bin/ffprobe
COPY --from=ffmpeg /src/crates/kerf-app/licenses/FFmpeg-LICENSE.txt /usr/share/doc/kerf/FFmpeg-LICENSE.txt
COPY LICENSE.md THIRD-PARTY-NOTICES.md /usr/share/doc/kerf/
COPY --from=build /out/kerf-app /usr/local/bin/kerf-app
COPY deploy/entrypoint.sh /usr/local/bin/kerf-entrypoint
COPY deploy/healthcheck.sh /usr/local/bin/kerf-healthcheck

RUN useradd --system --uid 10001 --user-group --home-dir /data --no-create-home --shell /usr/sbin/nologin kerf \
    && mkdir -p /workspace /data /cache \
    && chown kerf:kerf /workspace /data /cache

# Everything Kerf writes lives on the three volumes, so the root filesystem can be read-only:
# settings, logs and recovery in /data (--data-dir), voiceovers / music / templates in
# /data/share, models, proxies and waveforms in /cache, the agent's media in /workspace.
# KERF_WORKSPACE / KERF_CACHE_DIR / KERF_CACHE_MAX_GB are read by builds that have them.
ENV HOME=/data \
    XDG_CONFIG_HOME=/data/config \
    XDG_DATA_HOME=/data/share \
    XDG_CACHE_HOME=/cache \
    KERF_FFMPEG=/usr/local/bin/ffmpeg \
    KERF_FFPROBE=/usr/local/bin/ffprobe \
    KERF_MCP_ADDR=0.0.0.0:7777 \
    KERF_WORKSPACE=/workspace \
    KERF_CACHE_DIR=/cache

USER kerf
WORKDIR /workspace
VOLUME ["/workspace", "/data", "/cache"]
EXPOSE 7777
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --start-interval=2s --retries=3 CMD ["kerf-healthcheck"]
ENTRYPOINT ["tini", "--", "kerf-entrypoint"]
