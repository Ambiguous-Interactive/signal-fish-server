# Signal Fish Server — Development Container
# https://github.com/Ambiguous-Interactive/signal-fish-server

FROM mcr.microsoft.com/devcontainers/rust:2-bookworm
# Project MSRV: 1.89.0 — this image must provide >= 1.89

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG TARGETARCH
ARG ACTIONLINT_VERSION=1.7.12
ARG ACTIONLINT_AMD64_SHA256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
ARG ACTIONLINT_ARM64_SHA256=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6
ARG LYCHEE_VERSION=0.24.2
ARG LYCHEE_X86_64_SHA256=1f4e0ef7f6554a6ed33dd7ac144fb2e1bbed98598e7af973042fc5cd43951c9a
ARG LYCHEE_AARCH64_SHA256=91a7bd65685da41b90ccb9bc867a3d649a7818042dae04ff405e55a25bddee4c
ARG POWERSHELL_VERSION=7.6.2
ARG POWERSHELL_X64_SHA256=6cbcfbf20e376aa62ffd91c973493c41a7a52ddfd5a5db3ff9bc12f0d0fe9292
ARG POWERSHELL_ARM64_SHA256=a8d4e386dfafda385d0604045eed03ce6f3a843d45fc8f0b9588b836ca17cdb8
# Keep YQ/TAPLO versions in sync with .github/workflows/doc-validation.yml.
ARG YQ_VERSION=v4.53.2
ARG YQ_AMD64_SHA256=d56bf5c6819e8e696340c312bd70f849dc1678a7cda9c2ad63eebd906371d56b
ARG YQ_ARM64_SHA256=03061b2a50c7a498de2bbb92d7cb078ce433011f085a4994117c2726be4106ea
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
    lychee_target="x86_64-unknown-linux-gnu"; \
    lychee_sha256="$LYCHEE_X86_64_SHA256"; \
    powershell_arch="x64"; \
    powershell_sha256="$POWERSHELL_X64_SHA256"; \
    yq_arch="amd64"; \
    yq_sha256="$YQ_AMD64_SHA256"; \
    ;; \
    arm64) \
    actionlint_arch="arm64"; \
    actionlint_sha256="$ACTIONLINT_ARM64_SHA256"; \
    lychee_target="aarch64-unknown-linux-gnu"; \
    lychee_sha256="$LYCHEE_AARCH64_SHA256"; \
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
    yq --version; \
    yamllint --version; \
    gawk --version; \
    awk 'BEGIN { print "awk ok" }'; \
    java -version; \
    python3 -c 'import z3; print("z3", z3.get_version_string())'; \
    pwsh -NoLogo -NoProfile -Command '$PSVersionTable.PSEdition; $PSVersionTable.PSVersion.ToString()'

# Rust components
RUN rustup component add \
    rustfmt \
    clippy \
    rust-src \
    llvm-tools-preview

# Nightly toolchain — required only to RUN the coverage-guided fuzz crate
# (fuzz/, libFuzzer needs -Zsanitizer). Kept minimal and out of the pinned
# stable build path; see ADR-0003 and fuzz/README.md.
RUN rustup toolchain install nightly --profile minimal --component rust-src

# Increase Cargo resilience for transient network failures during no-cache image builds.
ENV CARGO_NET_RETRY=10
ENV CARGO_HTTP_TIMEOUT=120

# Cargo tools (installed globally, available to all users). Heavy tools go on
# the cargo-binstall path (prebuilt binaries) to keep no-cache rebuilds fast.
# cargo-mutants (mutation testing) and cargo-fuzz (coverage-guided fuzzing) back
# the mutation.yml / fuzz.yml CI jobs; keep them in dev/CI parity.
RUN cargo install --locked cargo-binstall \
    && cargo binstall --no-confirm --locked \
    cargo-deny \
    cargo-tarpaulin \
    cargo-watch \
    cargo-expand \
    cargo-llvm-cov \
    cargo-nextest \
    cargo-mutants \
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
    && cargo fuzz --help >/dev/null \
    && rm -rf /usr/local/cargo/registry/cache /usr/local/cargo/registry/src

# Fix ownership so the non-root "vscode" user (set via remoteUser in
# devcontainer.json) can write to the cargo registry and rustup toolchains.
# Without this, cargo fetch / cargo check in postCreateCommand fails with
# "Permission denied" because the directories above were populated by root.
RUN chown -R vscode:vscode /usr/local/cargo /usr/local/rustup

# Mold linker configuration — works on both x86_64 and aarch64
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
