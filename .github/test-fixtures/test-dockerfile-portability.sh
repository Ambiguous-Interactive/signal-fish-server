#!/usr/bin/env bash
# Test Script for Dockerfile Shell Portability Checker
#
# Creates temporary Dockerfiles with known portability issues and verifies
# that scripts/check-dockerfile-portability.sh correctly detects them.
#
# Usage:
#   ./.github/test-fixtures/test-dockerfile-portability.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo "$SCRIPT_DIR/../..")
CHECKER="$REPO_ROOT/scripts/check-dockerfile-portability.sh"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

if [ ! -x "$CHECKER" ]; then
    echo -e "${RED}[FAIL]${NC} $CHECKER not found or not executable"
    exit 1
fi

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

TESTS_PASSED=0
TESTS_FAILED=0

assert_fails() {
    local test_name="$1"
    local dockerfile="$2"
    local expected_pattern="$3"

    echo "$dockerfile" > "$TEMP_DIR/Dockerfile"
    local output
    if output=$("$CHECKER" --quiet 2>&1); then
        echo -e "${RED}[FAIL]${NC} $test_name: expected failure but got success"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    else
        if echo "$output" | grep -qiE "$expected_pattern"; then
            echo -e "${GREEN}[PASS]${NC} $test_name"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "${RED}[FAIL]${NC} $test_name: failed but missing expected pattern '$expected_pattern'"
            echo "  Output: $output"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    fi
}

assert_passes() {
    local test_name="$1"
    local dockerfile="$2"

    echo "$dockerfile" > "$TEMP_DIR/Dockerfile"
    local output
    if output=$("$CHECKER" --quiet 2>&1); then
        echo -e "${GREEN}[PASS]${NC} $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}[FAIL]${NC} $test_name: expected success but got failure"
        echo "  Output: $output"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

assert_warns() {
    local test_name="$1"
    local dockerfile="$2"
    local expected_pattern="$3"

    echo "$dockerfile" > "$TEMP_DIR/Dockerfile"
    local output
    output=$("$CHECKER" --quiet 2>&1) || true
    if echo "$output" | grep -qiE "$expected_pattern"; then
        echo -e "${GREEN}[PASS]${NC} $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}[FAIL]${NC} $test_name: missing expected warning pattern '$expected_pattern'"
        echo "  Output: $output"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

assert_no_warn() {
    local test_name="$1"
    local dockerfile="$2"
    local unwanted_pattern="$3"

    echo "$dockerfile" > "$TEMP_DIR/Dockerfile"
    local output
    output=$("$CHECKER" --quiet 2>&1) || true
    if echo "$output" | grep -qiE "$unwanted_pattern"; then
        echo -e "${RED}[FAIL]${NC} $test_name: found unwanted pattern '$unwanted_pattern'"
        echo "  Output: $output"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    else
        echo -e "${GREEN}[PASS]${NC} $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    fi
}

# Override the checker to search in TEMP_DIR
cd "$TEMP_DIR"
git init --quiet .

echo ""
echo "=========================================="
echo "  Dockerfile Portability Checker Tests"
echo "=========================================="
echo ""

# --- Brace expansion tests ---
assert_fails "Brace expansion in RUN" \
    "FROM debian:bookworm
RUN rm -rf /path/{cache,src}" \
    "brace expansion"

assert_fails "Brace expansion in multi-line RUN" \
    "FROM debian:bookworm
RUN apt-get update \\
    && rm -rf /path/{cache,src}" \
    "brace expansion"

# --- Double bracket tests ---
assert_fails "Double brackets [[ ]] in RUN" \
    "FROM debian:bookworm
RUN [[ -f /etc/os-release ]] && echo found" \
    "double.bracket|\[\["

# --- Array tests ---
assert_fails "Bash arrays in RUN" \
    "FROM debian:bookworm
RUN myarray=(one two three)" \
    "array"

# --- Source command tests ---
assert_fails "'source' instead of '.' in RUN" \
    "FROM debian:bookworm
RUN source /etc/profile" \
    "source"

# --- Process substitution tests ---
assert_fails "Process substitution in RUN" \
    "FROM debian:bookworm
RUN diff <(cat /etc/passwd) /dev/null" \
    "process substitution"

# --- find without -type f tests ---
assert_warns "find without -type f" \
    "FROM debian:bookworm
RUN find /usr -name '*.so' -exec ls {} +" \
    "find.*-type f"

# find with -type d should still warn (not restricting to regular files)
assert_warns "find with -type d still warns" \
    "FROM debian:bookworm
RUN find /usr -type d -name '*.so' -exec ls {} +" \
    "find.*-type f"

# find with -type l should still warn
assert_warns "find with -type l still warns" \
    "FROM debian:bookworm
RUN find /usr -type l -name '*.conf'" \
    "find.*-type f"

# find with -type f should NOT warn
assert_no_warn "find with -type f does not warn" \
    "FROM debian:bookworm
RUN find /usr -type f -name '*.so' -exec ls {} +" \
    "find.*-type f"

# find with -type f at end of command (no trailing flags)
assert_no_warn "find with -type f at end of line" \
    "FROM debian:bookworm
RUN find /usr -name '*.so' -type f" \
    "find.*-type f"

# --- SHELL directive suppresses errors ---
assert_passes "SHELL bash makes brace expansion valid" \
    'FROM debian:bookworm
SHELL ["/bin/bash", "-c"]
RUN rm -rf /path/{cache,src}'

# --- Clean Dockerfiles pass ---
assert_passes "Clean Dockerfile passes" \
    "FROM debian:bookworm
RUN apt-get update && apt-get install -y curl \\
    && rm -rf /var/lib/apt/lists/*"

assert_passes "Multi-stage clean Dockerfile passes" \
    "FROM rust:1.88 AS builder
WORKDIR /app
RUN cargo build --release
FROM debian:bookworm-slim
RUN useradd -m appuser
COPY --from=builder /app/target/release/server /usr/local/bin/
CMD [\"./server\"]"

# --- Shell variable expansion is NOT brace expansion ---
assert_passes "Shell variable \${VAR} is not brace expansion" \
    'FROM debian:bookworm
RUN echo "${HOME}" && ls "${PATH}"'

# --- bash -c makes bash features valid ---
assert_passes "bash -c allows brace expansion" \
    'FROM debian:bookworm
RUN bash -c "rm -rf /path/{cache,src}"'

# --- Empty Dockerfile (no RUN commands) ---
assert_passes "Dockerfile with no RUN commands" \
    "FROM debian:bookworm
CMD [\"echo\", \"hello\"]"

# --- --files flag tests ---
echo ""
echo "--- --files flag tests ---"

# Create a clean Dockerfile in a subdirectory
mkdir -p "$TEMP_DIR/subdir"
echo "FROM debian:bookworm
RUN echo hello" > "$TEMP_DIR/subdir/Dockerfile.clean"

# Create a bad Dockerfile in the main dir
echo "FROM debian:bookworm
RUN rm -rf /path/{cache,src}" > "$TEMP_DIR/Dockerfile"

# --files with a specific clean file should pass
if output=$("$CHECKER" --quiet --files "$TEMP_DIR/subdir/Dockerfile.clean" 2>&1); then
    echo -e "${GREEN}[PASS]${NC} --files with clean file passes"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "${RED}[FAIL]${NC} --files with clean file: expected pass"
    echo "  Output: $output"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi

# --files with a bad file should fail
if output=$("$CHECKER" --quiet --files "$TEMP_DIR/Dockerfile" 2>&1); then
    echo -e "${RED}[FAIL]${NC} --files with bad file: expected failure"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "${GREEN}[PASS]${NC} --files with bad file correctly fails"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi

# --files with no arguments should fail with exit 2
if output=$("$CHECKER" --files 2>&1); then
    echo -e "${RED}[FAIL]${NC} --files with no args: expected failure"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "${GREEN}[PASS]${NC} --files with no args correctly fails"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi

# --files with multiple files: clean + bad should fail
if output=$("$CHECKER" --quiet --files "$TEMP_DIR/subdir/Dockerfile.clean" "$TEMP_DIR/Dockerfile" 2>&1); then
    echo -e "${RED}[FAIL]${NC} --files with mixed clean+bad: expected failure"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "${GREEN}[PASS]${NC} --files with mixed clean+bad correctly fails"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi

# --files with nonexistent file should warn
if output=$("$CHECKER" --quiet --files "/nonexistent/Dockerfile" 2>&1); then
    if echo "$output" | grep -qi "not found"; then
        echo -e "${GREEN}[PASS]${NC} --files with nonexistent file warns"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}[FAIL]${NC} --files with nonexistent file: expected warning"
        echo "  Output: $output"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
else
    # Also acceptable: exit with warning
    echo -e "${GREEN}[PASS]${NC} --files with nonexistent file warns"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi

echo ""
echo "=========================================="
if [ "$TESTS_FAILED" -gt 0 ]; then
    echo -e "${RED}FAILED: $TESTS_FAILED test(s) failed, $TESTS_PASSED passed${NC}"
    exit 1
else
    echo -e "${GREEN}ALL PASSED: $TESTS_PASSED test(s) passed${NC}"
    exit 0
fi
