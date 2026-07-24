#!/usr/bin/env bash
# check-tooling-parity.sh - Enforce CI/devcontainer tooling parity.
#
# Verifies:
#   1. doc-validation workflow tool versions match devcontainer Dockerfile ARGs.
#   2. Devcontainer installs required modern tooling (yq, taplo, fd).
#   3. Release binaries and image-build caches remain portable and efficient.
#   4. Devcontainer feature set includes Docker CLI support with resilient settings.
#   5. Workspace build/dependency outputs use fast named volumes.
#   6. Devcontainer uses cargo-binstall for heavy cargo tools to keep rebuilds fast.
#   7. Post-create keeps required Rust tool verification and opt-in warm-up behavior.
#
# Usage:
#   ./scripts/check-tooling-parity.sh
#   ./scripts/check-tooling-parity.sh --quiet

set -euo pipefail

QUIET=false

for arg in "$@"; do
    case "$arg" in
        --quiet|-q)
            QUIET=true
            ;;
        --help|-h)
            cat <<'USAGE'
Usage: ./scripts/check-tooling-parity.sh [--quiet]

Checks that CI-required tooling and version pins stay synchronized with
the devcontainer configuration.
USAGE
            exit 0
            ;;
        *)
            echo "Unknown option: $arg" >&2
            exit 2
            ;;
    esac
done

if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    BLUE=''
    BOLD=''
    NC=''
fi

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

DOC_VALIDATION_WORKFLOW=".github/workflows/doc-validation.yml"
DEVCONTAINER_DOCKERFILE=".devcontainer/Dockerfile"
DEVCONTAINER_JSON=".devcontainer/devcontainer.json"
DEVCONTAINER_POST_CREATE=".devcontainer/post-create.sh"

ERRORS=0

info() {
    if [ "$QUIET" = false ]; then
        printf '%b[INFO]%b %s\n' "$BLUE" "$NC" "$1"
    fi
}

ok() {
    if [ "$QUIET" = false ]; then
        printf '%b[PASS]%b %s\n' "$GREEN" "$NC" "$1"
    fi
}

error_item() {
    ERRORS=$((ERRORS + 1))
    printf '%b[FAIL]%b %s\n' "$RED" "$NC" "$1"
}

require_file() {
    local file="$1"
    if [ ! -f "$file" ]; then
        error_item "Required file is missing: $file"
        return 1
    fi
    ok "Found required file: $file"
    return 0
}

extract_workflow_env_value() {
    local key="$1"
    local file="$2"

    awk -v key="$key" '
        BEGIN { in_env = 0 }
        /^env:[[:space:]]*$/ { in_env = 1; next }
        in_env && /^[^[:space:]]/ { in_env = 0 }
        in_env {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            if (line ~ ("^" key ":")) {
                sub(("^" key ":[[:space:]]*"), "", line)
                sub(/[[:space:]]+#.*$/, "", line)
                gsub(/^"|"$/, "", line)
                print line
                exit
            }
        }
    ' "$file"
}

extract_docker_arg_value() {
    local key="$1"
    local file="$2"

    sed -nE "s/^ARG ${key}=([^[:space:]]+).*$/\\1/p" "$file" | head -n 1
}

assert_equal() {
    local label="$1"
    local expected
    local actual

    expected=$(printf '%s' "$2" | tr -d '\r')
    actual=$(printf '%s' "$3" | tr -d '\r')

    if [ "$expected" = "$actual" ]; then
        ok "$label matches: $actual"
    else
        error_item "$label mismatch (expected: $expected, actual: $actual)"
    fi
}

assert_contains_literal() {
    local file="$1"
    local needle="$2"
    local description="$3"

    if grep -Fq -- "$needle" "$file"; then
        ok "$description"
    else
        error_item "$description (missing literal: $needle in $file)"
    fi
}

assert_not_contains_literal() {
    local file="$1"
    local needle="$2"
    local description="$3"

    if grep -Fq -- "$needle" "$file"; then
        error_item "$description (unexpected literal: $needle in $file)"
    else
        ok "$description"
    fi
}

info "Validating CI/devcontainer tooling parity"

require_file "$DOC_VALIDATION_WORKFLOW"
require_file "$DEVCONTAINER_DOCKERFILE"
require_file "$DEVCONTAINER_JSON"
require_file "$DEVCONTAINER_POST_CREATE"

WORKFLOW_YQ_VERSION=$(extract_workflow_env_value "YQ_VERSION" "$DOC_VALIDATION_WORKFLOW")
DOCKERFILE_YQ_VERSION=$(extract_docker_arg_value "YQ_VERSION" "$DEVCONTAINER_DOCKERFILE")
WORKFLOW_TAPLO_VERSION=$(extract_workflow_env_value "TAPLO_CLI_VERSION" "$DOC_VALIDATION_WORKFLOW")
DOCKERFILE_TAPLO_VERSION=$(extract_docker_arg_value "TAPLO_CLI_VERSION" "$DEVCONTAINER_DOCKERFILE")

if [ -z "$WORKFLOW_YQ_VERSION" ]; then
    error_item "Could not extract YQ_VERSION from $DOC_VALIDATION_WORKFLOW"
fi
if [ -z "$DOCKERFILE_YQ_VERSION" ]; then
    error_item "Could not extract ARG YQ_VERSION from $DEVCONTAINER_DOCKERFILE"
fi
if [ -z "$WORKFLOW_TAPLO_VERSION" ]; then
    error_item "Could not extract TAPLO_CLI_VERSION from $DOC_VALIDATION_WORKFLOW"
fi
if [ -z "$DOCKERFILE_TAPLO_VERSION" ]; then
    error_item "Could not extract ARG TAPLO_CLI_VERSION from $DEVCONTAINER_DOCKERFILE"
fi

if [ -n "$WORKFLOW_YQ_VERSION" ] && [ -n "$DOCKERFILE_YQ_VERSION" ]; then
    assert_equal "YQ version parity" "$WORKFLOW_YQ_VERSION" "$DOCKERFILE_YQ_VERSION"
fi
if [ -n "$WORKFLOW_TAPLO_VERSION" ] && [ -n "$DOCKERFILE_TAPLO_VERSION" ]; then
    assert_equal "TAPLO version parity" "$WORKFLOW_TAPLO_VERSION" "$DOCKERFILE_TAPLO_VERSION"
fi

assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "fd-find" "Devcontainer installs fd-find"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "ln -sf /usr/bin/fdfind /usr/local/bin/fd" "Devcontainer maps fdfind to fd"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'yq_linux_${yq_arch}' "Devcontainer installs yq from pinned release binaries"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'cargo install --locked taplo-cli --version "$TAPLO_CLI_VERSION"' "Devcontainer installs pinned taplo-cli"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo install --locked cargo-binstall" "Devcontainer installs cargo-binstall"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo binstall --no-confirm --locked" "Devcontainer uses cargo-binstall for heavy cargo tools"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "Acquire::Retries=5" "Devcontainer apt operations enable retry hardening"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "curl_retry_args=(--retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 20)" "Devcontainer release-binary downloads enable curl retries"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "ENV CARGO_NET_RETRY=10" "Devcontainer configures Cargo network retries"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "ENV CARGO_HTTP_TIMEOUT=120" "Devcontainer configures Cargo HTTP timeout"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'lychee_target="x86_64-unknown-linux-musl"' "Devcontainer uses portable x86_64 MUSL lychee binary"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'lychee_target="aarch64-unknown-linux-musl"' "Devcontainer uses portable aarch64 MUSL lychee binary"
assert_not_contains_literal "$DEVCONTAINER_DOCKERFILE" 'lychee_target="x86_64-unknown-linux-gnu"' "Devcontainer avoids glibc-sensitive x86_64 lychee binary"
assert_not_contains_literal "$DEVCONTAINER_DOCKERFILE" 'lychee_target="aarch64-unknown-linux-gnu"' "Devcontainer avoids glibc-sensitive aarch64 lychee binary"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "signal-fish-cargo-registry" "Devcontainer retains Cargo registry downloads in a BuildKit cache"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "signal-fish-cargo-install-target" "Devcontainer retains Cargo tool compilation outputs in a BuildKit cache"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'cargo_binstall_target="x86_64-unknown-linux-musl"' "Devcontainer uses portable x86_64 MUSL Cargo tool binaries"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'cargo_binstall_target="aarch64-unknown-linux-musl"' "Devcontainer uses portable aarch64 MUSL Cargo tool binaries"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'cargo binstall --no-confirm --locked --target "$cargo_binstall_target"' "Devcontainer applies the portable target to cargo-binstall"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "musl-tools" "Devcontainer supports source fallback for MUSL Cargo tools"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" 'rustup target add "$cargo_binstall_target"' "Devcontainer installs the MUSL Rust standard library for binstall fallback"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "USER vscode" "Devcontainer installs Rust tooling as the non-root runtime user"
assert_not_contains_literal "$DEVCONTAINER_DOCKERFILE" "chown -R vscode:vscode /usr/local/cargo /usr/local/rustup" "Devcontainer avoids a large recursive ownership layer"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-deny --version" "Devcontainer smoke checks cargo-deny"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-tarpaulin --version" "Devcontainer smoke checks cargo-tarpaulin"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-watch --version" "Devcontainer smoke checks cargo-watch"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-expand --version" "Devcontainer smoke checks cargo-expand"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo llvm-cov --version" "Devcontainer smoke checks cargo-llvm-cov"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-nextest --version" "Devcontainer smoke checks cargo-nextest"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo mutants --version" "Devcontainer smoke checks cargo-mutants via cargo plugin entrypoint"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo fuzz --help" "Devcontainer smoke checks cargo-fuzz via cargo plugin entrypoint"
assert_not_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-mutants --version" "Devcontainer avoids unsupported direct cargo-mutants --version smoke check"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "fd --version;" "Devcontainer smoke checks fd"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "yq --version;" "Devcontainer smoke checks yq"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "taplo --version" "Devcontainer smoke checks taplo"
assert_contains_literal "$DEVCONTAINER_JSON" "ghcr.io/devcontainers/features/docker-outside-of-docker:1" "Devcontainer enables Docker CLI feature"
assert_contains_literal "$DEVCONTAINER_JSON" '"moby": false' "Devcontainer uses Docker CE path for docker-outside-of-docker reliability"
assert_contains_literal "$DEVCONTAINER_JSON" 'source=${devcontainerId}-cargo-target,target=${containerWorkspaceFolder}/target,type=volume' "Devcontainer uses a named volume for Cargo build outputs"
assert_contains_literal "$DEVCONTAINER_JSON" 'source=${devcontainerId}-root-node-modules,target=${containerWorkspaceFolder}/node_modules,type=volume' "Devcontainer uses a named volume for root Node dependencies"
assert_contains_literal "$DEVCONTAINER_JSON" 'source=${devcontainerId}-browser-node-modules,target=${containerWorkspaceFolder}/clients/browser/node_modules,type=volume' "Devcontainer uses a named volume for browser-client Node dependencies"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "prepare_worktree_cache_dirs" "Post-create initializes named-volume ownership"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "configure_git_safe_directory" "Post-create trusts the bind-mounted workspace for Git"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "safe.directory" "Post-create handles Git dubious-ownership protection"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "verify_required_rust_tools" "Post-create verifies required Rust tools"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "if ! install_codex_cli; then" "Post-create treats Codex install failures as non-fatal"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "run_with_retries 3 5 cargo fetch" "Post-create retries cargo dependency prefetch"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "is_truthy" "Post-create supports standard truthy warm-up values"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "cargo-mutants" "Post-create required-tools list includes cargo-mutants"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "cargo-fuzz" "Post-create required-tools list includes cargo-fuzz"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "cargo mutants --version" "Post-create validates cargo-mutants via cargo plugin entrypoint"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "cargo fuzz --help" "Post-create validates cargo-fuzz via cargo plugin entrypoint"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "SIGNAL_FISH_WARM_CARGO_CHECK" "Post-create uses opt-in cargo warm-up"

if [ "$ERRORS" -gt 0 ]; then
    echo ""
    printf '%b%bFAILED%b: %d tooling parity issue(s)\n' "$BOLD" "$RED" "$NC" "$ERRORS"
    exit 1
fi

if [ "$QUIET" = false ]; then
    echo ""
    printf '%b%bALL PASSED%b: tooling parity is synchronized\n' "$BOLD" "$GREEN" "$NC"
fi
