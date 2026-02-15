# Signal Fish Server — Development Container
# https://github.com/Ambiguous-Interactive/signal-fish-server

FROM mcr.microsoft.com/devcontainers/rust:1-bookworm
# Project MSRV: 1.87.0 — this image must provide >= 1.87

# System dependencies for building with mold linker and TLS
RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
    mold \
    clang \
    pkg-config \
    libssl-dev \
    curl \
    git \
    ca-certificates \
    && apt-get autoremove -y && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

# Rust components
RUN rustup component add \
    rustfmt \
    clippy \
    rust-src \
    llvm-tools-preview

# Cargo tools (installed globally, available to all users)
RUN cargo install --locked \
    cargo-deny \
    cargo-tarpaulin \
    cargo-watch \
    cargo-expand \
    cargo-llvm-cov \
    cargo-nextest

# Mold linker configuration — works on both x86_64 and aarch64
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
