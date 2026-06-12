#!/usr/bin/env bash
# check-tooling-parity.sh - Enforce CI/devcontainer tooling parity.
#
# Verifies:
#   1. doc-validation workflow tool versions match devcontainer Dockerfile ARGs.
#   2. Devcontainer installs required modern tooling (yq, taplo, fd).
#   3. Devcontainer feature set includes Docker CLI support with resilient settings.
#   4. Devcontainer uses cargo-binstall for heavy cargo tools to keep rebuilds fast.
#   5. Post-create keeps required Rust tool verification and opt-in warm-up behavior.
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
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-deny --version" "Devcontainer smoke checks cargo-deny"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-tarpaulin --version" "Devcontainer smoke checks cargo-tarpaulin"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-watch --version" "Devcontainer smoke checks cargo-watch"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-expand --version" "Devcontainer smoke checks cargo-expand"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo llvm-cov --version" "Devcontainer smoke checks cargo-llvm-cov"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "cargo-nextest --version" "Devcontainer smoke checks cargo-nextest"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "fd --version;" "Devcontainer smoke checks fd"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "yq --version;" "Devcontainer smoke checks yq"
assert_contains_literal "$DEVCONTAINER_DOCKERFILE" "taplo --version" "Devcontainer smoke checks taplo"
assert_contains_literal "$DEVCONTAINER_JSON" "ghcr.io/devcontainers/features/docker-outside-of-docker:1" "Devcontainer enables Docker CLI feature"
assert_contains_literal "$DEVCONTAINER_JSON" '"moby": false' "Devcontainer uses Docker CE path for docker-outside-of-docker reliability"
assert_contains_literal "$DEVCONTAINER_POST_CREATE" "verify_required_rust_tools" "Post-create verifies required Rust tools"
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
