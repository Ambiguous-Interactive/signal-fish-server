# Signal Fish Server — Development Container
# https://github.com/Ambiguous-Interactive/signal-fish-server

FROM mcr.microsoft.com/devcontainers/rust:2-bookworm
# Project MSRV: 1.94.0 — this image must provide >= 1.94

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG TARGETARCH
ARG RUST_NIGHTLY=nightly-2026-08-01
ARG ACTIONLINT_VERSION=1.7.12
ARG ACTIONLINT_AMD64_SHA256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
ARG ACTIONLINT_ARM64_SHA256=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6
ARG LYCHEE_VERSION=0.24.2
ARG LYCHEE_X86_64_MUSL_SHA256=73657a111819a30c47c08352896796f23d64e4eb2b3ed39b6d32149241566fc5
ARG LYCHEE_AARCH64_MUSL_SHA256=5d0b0e3aeab240f41920c633a6eaf97599be6eedda034b36e858ede7dba5e535
# Official GitHub MCP server (github/github-mcp-server), pinned for reproducible
# builds. Wired into every agent harness via .vscode/mcp.json, .mcp.json,
# opencode.json, and ~/.codex/config.toml (see .devcontainer/post-create.sh).
# SHA256s are the Linux x86_64/arm64 lines of the release's checksums.txt
# (github-mcp-server_2.0.2_checksums.txt), cross-checked against the release
# asset digests.
ARG GITHUB_MCP_VERSION=2.0.2
ARG GITHUB_MCP_X86_64_SHA256=815e27d931c796af0c97f24fd211e14f8a4607d4528377719a5ad2352086b0b0
ARG GITHUB_MCP_ARM64_SHA256=73f5e76c6a3b5e0d731e1134a7c38575df6ec96c646192095a3be2d4d0d6c1fc
ARG POWERSHELL_VERSION=7.6.7
ARG POWERSHELL_X64_SHA256=6896f9e3a6b0a354bf470835a96da76d2f801cd50dcf047ad7c4563bf93bf431
ARG POWERSHELL_ARM64_SHA256=ba465efdfb0984c1c5dc5ed9eb0b79a7e91a1b52c45f99377058c0659b84d3f1
# Keep YQ/TAPLO versions in sync with .github/workflows/doc-validation.yml.
ARG YQ_VERSION=v4.54.1
ARG YQ_AMD64_SHA256=8e34fc298390875de416e6a4afcb8cabeceb25d9aa8506c1a2f9353cf702ea5f
ARG YQ_ARM64_SHA256=189088da0c6429ec5178dfaab1a114805f6cab0b61b165ab236efedf1d57a71b
ARG TAPLO_CLI_VERSION=0.10.0

# System dependencies for building with mold linker, TLS, validation tooling,
# and the native libraries required by the PowerShell/.NET runtime on Debian 12.
RUN apt-get update -o Acquire::Retries=5 \
    && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends -o Acquire::Retries=5 \
    mold \
    clang \
    pkg-config \
    libssl-dev \
    curl \
    git \
    ca-certificates \
    gawk \
    libgssapi-krb5-2 \
    libicu72 \
    libssl3 \
    libstdc++6 \
    fd-find \
    ripgrep \
    shellcheck \
    musl-tools \
    tzdata \
    yamllint \
    zlib1g \
    default-jre-headless \
    python3-z3 \
    && ln -sf /usr/bin/fdfind /usr/local/bin/fd \
    && apt-get autoremove -y && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

# Non-Debian dev tools: install pinned upstream release binaries with checksum
# verification. This keeps rebuilds fast and supports both linux/amd64 and linux/arm64.
RUN set -eux; \
    case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
    amd64) \
    actionlint_arch="amd64"; \
    actionlint_sha256="$ACTIONLINT_AMD64_SHA256"; \
    lychee_target="x86_64-unknown-linux-musl"; \
    lychee_sha256="$LYCHEE_X86_64_MUSL_SHA256"; \
    github_mcp_arch="x86_64"; \
    github_mcp_sha256="$GITHUB_MCP_X86_64_SHA256"; \
    powershell_arch="x64"; \
    powershell_sha256="$POWERSHELL_X64_SHA256"; \
    yq_arch="amd64"; \
    yq_sha256="$YQ_AMD64_SHA256"; \
    ;; \
    arm64) \
    actionlint_arch="arm64"; \
    actionlint_sha256="$ACTIONLINT_ARM64_SHA256"; \
    lychee_target="aarch64-unknown-linux-musl"; \
    lychee_sha256="$LYCHEE_AARCH64_MUSL_SHA256"; \
    github_mcp_arch="arm64"; \
    github_mcp_sha256="$GITHUB_MCP_ARM64_SHA256"; \
    powershell_arch="arm64"; \
    powershell_sha256="$POWERSHELL_ARM64_SHA256"; \
    yq_arch="arm64"; \
    yq_sha256="$YQ_ARM64_SHA256"; \
    ;; \
    *) \
    echo "Unsupported target architecture: ${TARGETARCH:-$(dpkg --print-architecture)}" >&2; \
    exit 1; \
    ;; \
    esac; \
    curl_retry_args=(--retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 20); \
    tmp_dir="$(mktemp -d)"; \
    cd "$tmp_dir"; \
    actionlint_archive="actionlint_${ACTIONLINT_VERSION}_linux_${actionlint_arch}.tar.gz"; \
    curl -fsSL "${curl_retry_args[@]}" -o "$actionlint_archive" "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/${actionlint_archive}"; \
    echo "${actionlint_sha256}  ${actionlint_archive}" | sha256sum -c -; \
    tar -xzf "$actionlint_archive" actionlint; \
    install -m 0755 actionlint /usr/local/bin/actionlint; \
    lychee_archive="lychee-${lychee_target}.tar.gz"; \
    lychee_dir="lychee-${lychee_target}"; \
    curl -fsSL "${curl_retry_args[@]}" -o "$lychee_archive" "https://github.com/lycheeverse/lychee/releases/download/lychee-v${LYCHEE_VERSION}/${lychee_archive}"; \
    echo "${lychee_sha256}  ${lychee_archive}" | sha256sum -c -; \
    tar -xzf "$lychee_archive" "${lychee_dir}/lychee"; \
    install -m 0755 "${lychee_dir}/lychee" /usr/local/bin/lychee; \
    github_mcp_archive="github-mcp-server_Linux_${github_mcp_arch}.tar.gz"; \
    curl -fsSL "${curl_retry_args[@]}" -o "$github_mcp_archive" "https://github.com/github/github-mcp-server/releases/download/v${GITHUB_MCP_VERSION}/${github_mcp_archive}"; \
    echo "${github_mcp_sha256}  ${github_mcp_archive}" | sha256sum -c -; \
    tar -xzf "$github_mcp_archive" github-mcp-server; \
    install -m 0755 github-mcp-server /usr/local/bin/github-mcp-server; \
    yq_binary="yq_linux_${yq_arch}"; \
    curl -fsSL "${curl_retry_args[@]}" -o "$yq_binary" "https://github.com/mikefarah/yq/releases/download/${YQ_VERSION}/${yq_binary}"; \
    echo "${yq_sha256}  ${yq_binary}" | sha256sum -c -; \
    install -m 0755 "$yq_binary" /usr/local/bin/yq; \
    powershell_archive="powershell-${POWERSHELL_VERSION}-linux-${powershell_arch}.tar.gz"; \
    curl -fsSL "${curl_retry_args[@]}" -o "$powershell_archive" "https://github.com/PowerShell/PowerShell/releases/download/v${POWERSHELL_VERSION}/${powershell_archive}"; \
    echo "${powershell_sha256}  ${powershell_archive}" | sha256sum -c -; \
    mkdir -p /opt/microsoft/powershell/7; \
    tar -xzf "$powershell_archive" -C /opt/microsoft/powershell/7; \
    chmod 0755 /opt/microsoft/powershell/7/pwsh; \
    ln -sf /opt/microsoft/powershell/7/pwsh /usr/local/bin/pwsh; \
    cd /; \
    rm -rf "$tmp_dir"

# Build-time smoke checks for developer tooling expected by hooks and scripts.
RUN set -eux; \
    fd --version; \
    rg --version; \
    shellcheck --version; \
    actionlint -version; \
    lychee --version; \
    github-mcp-server --version; \
    yq --version; \
    yamllint --version; \
    gawk --version; \
    awk 'BEGIN { print "awk ok" }'; \
    java -version; \
    python3 -c 'import z3; print("z3", z3.get_version_string())'; \
    pwsh -NoLogo -NoProfile -Command '$PSVersionTable.PSEdition; $PSVersionTable.PSVersion.ToString()'

# The base image makes the shared Cargo and rustup roots writable by vscode.
# Install developer toolchains as the final non-root user so ownership is
# correct without a recursive chown layer that duplicates the toolchains.
USER vscode

# npm global installs land in a user-owned prefix so `npm install -g ...`
# never needs sudo (the nvm-managed Node under /usr/local/share/nvm is
# root-owned; without this, global installs fail with EACCES). The agent CLIs
# (codex, opencode, nanocoder) are installed/refreshed through this prefix by
# .devcontainer/lib-agent-tools.sh on create and on every container start.
#
# NEVER set NPM_CONFIG_PREFIX (or PREFIX) via ENV here. Devcontainer features
# install in layers appended AFTER this Dockerfile, and the nvm-based Node
# feature aborts its install when NPM_CONFIG_PREFIX is already set in the
# build environment ("nvm is not compatible with the NPM_CONFIG_PREFIX
# environment variable"), failing every no-cache image build. The prefix is
# runtime-only: devcontainer.json containerEnv + configure_user_npm_prefix.
ENV PATH="/home/vscode/.npm-global/bin:${PATH}"
RUN mkdir -p /home/vscode/.npm-global

# Rust components
RUN rustup component add \
    rustfmt \
    clippy \
    rust-src \
    llvm-tools-preview

# Nightly toolchain — required only to RUN the coverage-guided fuzz crate
# (fuzz/, libFuzzer needs -Zsanitizer). Kept minimal and out of the pinned
# stable build path; see ADR-0003 and fuzz/README.md.
RUN rustup toolchain install "$RUST_NIGHTLY" --profile minimal --component rust-src

# Increase Cargo resilience for transient network failures during no-cache image builds.
ENV CARGO_NET_RETRY=10
ENV CARGO_HTTP_TIMEOUT=120

# Cargo tools (installed globally, available to all users). Heavy tools go on
# the cargo-binstall path (prebuilt binaries) to keep no-cache rebuilds fast.
# BuildKit caches retain registry metadata and compilation outputs across layer
# invalidations without baking those caches into the final image.
# cargo-mutants (mutation testing) and cargo-fuzz (coverage-guided fuzzing) back
# the mutation.yml / fuzz.yml CI jobs; keep them in dev/CI parity.
# cargo-nextest is pinned to the exact version the CI lanes install:
# .config/nextest.toml encodes per-profile timeout and filter semantics that
# drift between nextest versions (issue #597).
# cargo-binstall resolves releases through the unauthenticated GitHub API. A
# degraded API response silently falls back to compiling from source; the musl
# source build is doomed on this image (openssl-sys has no vendored OpenSSL),
# which turns a transient flake into a multi-minute hard failure (issue #599).
# Every amd64-musl tool in the list below has a prebuilt artifact (GitHub
# release or QuickInstall), so the amd64 build disables the source-compile
# strategy and fails fast instead. arm64 keeps source fallback: cargo-watch
# ships no aarch64-musl release artifact.
RUN --mount=type=cache,id=signal-fish-cargo-registry-v2,target=/usr/local/cargo/registry,sharing=locked,uid=1000,gid=1000 \
    --mount=type=cache,id=signal-fish-cargo-git-v2,target=/usr/local/cargo/git,sharing=locked,uid=1000,gid=1000 \
    --mount=type=cache,id=signal-fish-cargo-install-target-${TARGETARCH}-v2,target=/tmp/cargo-install-target,sharing=locked,uid=1000,gid=1000 \
    export CARGO_TARGET_DIR=/tmp/cargo-install-target \
    && binstall_args=() \
    && case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
    amd64) cargo_binstall_target="x86_64-unknown-linux-musl"; binstall_args+=(--disable-strategies compile) ;; \
    arm64) cargo_binstall_target="aarch64-unknown-linux-musl" ;; \
    *) \
    echo "Unsupported target architecture: ${TARGETARCH:-$(dpkg --print-architecture)}" >&2; \
    exit 1; \
    ;; \
    esac \
    && rustup target add "$cargo_binstall_target" \
    && cargo install --locked cargo-binstall \
    && cargo binstall --no-confirm --locked --target "$cargo_binstall_target" "${binstall_args[@]}" \
    cargo-deny \
    cargo-tarpaulin \
    cargo-watch \
    cargo-expand \
    cargo-llvm-cov \
    cargo-nextest@0.9.148 \
    cargo-mutants@27.1.0 \
    cargo-fuzz \
    && cargo install --locked taplo-cli --version "$TAPLO_CLI_VERSION" \
    && taplo --version \
    && cargo-deny --version \
    && cargo-tarpaulin --version \
    && cargo-watch --version \
    && cargo-expand --version \
    && cargo llvm-cov --version \
    && cargo-nextest --version \
    && cargo mutants --version \
    && cargo fuzz --help >/dev/null

# Cache mounts above are external to the image. Remove any registry state
# inherited from the base image so the final development image stays lean.
RUN rm -rf /usr/local/cargo/registry/cache \
    /usr/local/cargo/registry/src \
    /usr/local/cargo/git

# Mold linker configuration — works on both x86_64 and aarch64
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
