# check=skip=SecretsUsedInArgOrEnv
# Multi-stage, multi-architecture Dockerfile for Signal Fish Server
# Optimized with cargo-chef for dependency caching.
# Zero external runtime dependencies (no database, no cloud services).
#
# Multi-arch strategy (issue #122): the published image is a single manifest
# list covering linux/amd64, linux/arm64, and linux/arm/v7 so ARM consumers
# (AWS Graviton, Ampere, Raspberry Pi, Apple Silicon under Docker) can pull it.
# The expensive Rust compile is CROSS-COMPILED, not emulated: the
# `chef`/`planner`/`builder` stages pin themselves to the native build host
# (`--platform=$BUILDPLATFORM`) and cross-compile to the target architecture
# buildx injects via $TARGETARCH / $TARGETVARIANT. Only the small `runtime`
# stage runs on the target platform (for `useradd` + a 2-package `apt-get`),
# which buildx executes under QEMU in seconds — the multi-minute Rust build
# never touches emulation.
#
# Supported platforms and their Rust target triples / cross linkers are defined
# in the `builder` stage's arch map below. Keep that map in lockstep with the
# `platforms:` list in .github/workflows/docker-publish.yml and with
# REQUIRED_CONTAINER_PLATFORMS in tests/ci_config_tests.rs.

# Stage 1: Chef - Install cargo-chef for dependency management.
# Pinned to the native build platform so `cargo install` runs without emulation.
# Using bookworm (Debian 12); cross GCC toolchains are available in its repos.
FROM --platform=$BUILDPLATFORM rust:1.89-bookworm AS chef
# Pin cargo-chef to an explicit version (not "latest") for reproducible builds.
# 0.1.77 is the latest stable and trims lints from the generated recipe.json.
# Note: `cargo chef cook` may still emit benign
# `warning: edition is set on library/binary/benchmark ... which is deprecated`
# lines under cargo 1.89. These come from the cargo-chef skeleton's target
# tables, NOT our Cargo.toml (which sets `edition` only in [package]); the build
# succeeds regardless. See .llm/context-docs-and-ci-pitfalls.md.
RUN cargo install cargo-chef --version 0.1.77 --locked
WORKDIR /app

# Stage 2: Planner - Analyze dependencies (host-native; recipe.json is
# architecture-independent so this runs once regardless of target arch).
FROM --platform=$BUILDPLATFORM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY build.rs ./
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Builder - Cross-compile to $TARGETARCH on the native build host.
FROM --platform=$BUILDPLATFORM chef AS builder

# buildx injects these automatically: TARGET* describe the image being built,
# BUILDARCH describes the host this builder runs on. We compare them to decide
# whether to link natively (`cc`) or install + use a cross GCC toolchain. This
# keeps the build correct on an amd64 CI runner (the GitHub default, where
# arm64/armv7 are cross targets) AND on an arm64 host (where amd64 is the cross
# target), rather than hard-coding which arch is "native".
ARG BUILDARCH
ARG TARGETARCH
ARG TARGETVARIANT

# Each arch maps to a Rust target triple, a cross GCC + cross libc dev package,
# and the cross linker binary. The cross libc dev package (which provides the C
# runtime startup objects Scrt1.o / crti.o) is only a recommends of the cross
# GCC, so under `--no-install-recommends` it must be named explicitly or linking
# fails with "cannot find Scrt1.o". `$cross_pkg` is intentionally word-split by
# apt-get. When TARGETARCH == BUILDARCH the build is native: we skip the cross
# toolchain entirely and link with the image's own `cc`.
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu;      cross_pkg="gcc-x86-64-linux-gnu libc6-dev-amd64-cross"; cross_linker=x86_64-linux-gnu-gcc; ;; \
      arm64) triple=aarch64-unknown-linux-gnu;     cross_pkg="gcc-aarch64-linux-gnu libc6-dev-arm64-cross"; cross_linker=aarch64-linux-gnu-gcc; ;; \
      arm) \
        case "$TARGETVARIANT" in \
          v7) triple=armv7-unknown-linux-gnueabihf; cross_pkg="gcc-arm-linux-gnueabihf libc6-dev-armhf-cross"; cross_linker=arm-linux-gnueabihf-gcc; ;; \
          *) echo "Unsupported arm variant: '${TARGETVARIANT:-<none>}' (only v7)" >&2; exit 1; ;; \
        esac; ;; \
      *) echo "Unsupported TARGETARCH: '$TARGETARCH'" >&2; exit 1; ;; \
    esac; \
    if [ "$TARGETARCH" = "$BUILDARCH" ]; then \
      linker=cc; \
    else \
      apt-get update; \
      apt-get install -y --no-install-recommends $cross_pkg; \
      rm -rf /var/lib/apt/lists/*; \
      linker="$cross_linker"; \
    fi; \
    rustup target add "$triple"; \
    echo "$triple" > /tmp/rust-triple; \
    mkdir -p /app/.cargo; \
    printf '[target.%s]\nlinker = "%s"\n' "$triple" "$linker" > /app/.cargo/config.toml

# Copy the recipe from planner stage
COPY --from=planner /app/recipe.json recipe.json

# Build dependencies ONLY for the target triple - this layer is cached until
# Cargo.toml/Cargo.lock (and thus recipe.json) change.
RUN cargo chef cook --release --locked --target "$(cat /tmp/rust-triple)" --recipe-path recipe.json

# Copy actual source code
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY benches ./benches

# Build the application for the target triple - only recompiles when source
# changes. Copy the produced binary to a fixed, arch-independent path so the
# runtime stage's COPY does not need to know the triple.
RUN cargo build --release --locked --target "$(cat /tmp/rust-triple)" \
    && cp "target/$(cat /tmp/rust-triple)/release/signal-fish-server" /app/signal-fish-server

# Stage 4: Runtime image (slim Debian).
# NOT pinned to $BUILDPLATFORM: buildx builds this stage once per target
# platform and pulls the architecture-appropriate debian:bookworm-slim base.
# The only target-arch execution here is `useradd` + a 2-package `apt-get`,
# which runs under QEMU in seconds.
FROM debian:bookworm-slim AS runtime

# Create non-root user
RUN useradd -m -u 10001 appuser

# Install minimal runtime dependencies for TLS and health checks only
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the built binary (fixed path set in the builder stage)
COPY --from=builder /app/signal-fish-server ./signal-fish-server

# Expose the WebSocket signaling server port (TCP)
EXPOSE 3536

# Health check endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3536/v2/health || exit 1

# Use non-root
USER appuser

# Default environment (can be overridden at runtime)
ENV RUST_LOG=info
# Disable auth by default so the container starts without a config file.
# Production deployments should mount a config.json or set auth env vars.
ENV SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false
ENV SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false

# Run the server
CMD ["./signal-fish-server"]
