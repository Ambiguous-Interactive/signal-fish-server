# Multi-stage Dockerfile for Signal Fish Server
# Optimized with cargo-chef for dependency caching and mold linker for speed
# Zero external runtime dependencies (no database, no cloud services)

# Stage 1: Chef - Install cargo-chef for dependency management
# Using bookworm (Debian 12) which has mold in its repositories
FROM rust:1.88-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

# Stage 2: Planner - Analyze dependencies
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY third_party ./third_party
COPY build.rs ./
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Builder - Build dependencies (cached) then source
FROM chef AS builder

# Install build dependencies: mold linker (2-5x faster than default ld) and clang
RUN apt-get update && apt-get install -y --no-install-recommends \
    mold \
    clang \
    && rm -rf /var/lib/apt/lists/*

# Configure mold as the linker via clang
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="clang"
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"

# Copy the recipe from planner stage
COPY --from=planner /app/recipe.json recipe.json

# Copy local path dependencies required by cargo chef cook
# These are referenced in Cargo.toml and must exist for dependency resolution
COPY third_party ./third_party

# Build dependencies ONLY - this layer is cached until Cargo.toml/Cargo.lock change
RUN cargo chef cook --release --recipe-path recipe.json

# Copy actual source code
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY benches ./benches
COPY third_party ./third_party

# Build the application - only recompiles when source changes
RUN cargo build --release --locked

# Stage 4: Runtime image (distroless-like slim Debian)
FROM debian:bookworm-slim AS runtime

# Create non-root user
RUN useradd -m -u 10001 appuser

# Install minimal runtime dependencies for TLS and health checks only
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the built binary
COPY --from=builder /app/target/release/signal-fish-server ./signal-fish-server

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
