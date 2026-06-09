#!/usr/bin/env bash
# Test Script for Documentation Consistency Changelog Gate
#
# Exercises the is_internal_path() classification and changelog gate logic
# in scripts/check-doc-consistency.sh using --changed-files to pass
# explicit file lists (no git staging required).
#
# NOTE: The checker also runs version sync (gate 1), changelog format
# (gate 2), and protocol anti-drift (gate 3) against real repo files,
# so this script must be run from the repository root.
#
# Usage:
#   ./.github/test-fixtures/test-doc-consistency.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo "$SCRIPT_DIR/../..")
CHECKER="$REPO_ROOT/scripts/check-doc-consistency.sh"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

if [ ! -x "$CHECKER" ]; then
    echo -e "${RED}[FAIL]${NC} $CHECKER not found or not executable"
    exit 1
fi

TESTS_PASSED=0
TESTS_FAILED=0

# ---------------------------------------------------------------------------
# Assertion helpers
# ---------------------------------------------------------------------------

# Assert that the checker exits 0 for a given --changed-files list.
# Usage: assert_gate_passes <test_name> <file1> [file2 ...]
assert_gate_passes() {
    local test_name="$1"
    shift
    local output
    if output=$("$CHECKER" --changed-files "$@" 2>&1); then
        echo -e "${GREEN}[PASS]${NC} $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}[FAIL]${NC} $test_name: expected pass but checker exited non-zero"
        echo "  Output (last 5 lines):"
        echo "$output" | tail -5 | sed 's/^/    /'
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# Assert that the checker exits non-zero and output matches a pattern.
# Usage: assert_gate_fails <test_name> <expected_pattern> <file1> [file2 ...]
assert_gate_fails() {
    local test_name="$1"
    local expected_pattern="$2"
    shift 2
    local output
    if output=$("$CHECKER" --changed-files "$@" 2>&1); then
        echo -e "${RED}[FAIL]${NC} $test_name: expected failure but checker exited 0"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    else
        if grep -qiE "$expected_pattern" <<< "$output"; then
            echo -e "${GREEN}[PASS]${NC} $test_name"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "${RED}[FAIL]${NC} $test_name: failed but missing expected pattern '$expected_pattern'"
            echo "  Output (last 5 lines):"
            echo "$output" | tail -5 | sed 's/^/    /'
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    fi
}

# Assert that the checker exits 0 with --skip-changelog-gate.
# Usage: assert_skip_passes <test_name> <file1> [file2 ...]
assert_skip_passes() {
    local test_name="$1"
    shift
    local output
    if output=$("$CHECKER" --skip-changelog-gate --changed-files "$@" 2>&1); then
        if grep -qiE "changelog gate skipped" <<< "$output"; then
            echo -e "${GREEN}[PASS]${NC} $test_name"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "${RED}[FAIL]${NC} $test_name: passed but missing 'changelog gate skipped' message"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    else
        echo -e "${RED}[FAIL]${NC} $test_name: expected pass with --skip-changelog-gate but checker exited non-zero"
        echo "  Output (last 5 lines):"
        echo "$output" | tail -5 | sed 's/^/    /'
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# Run tests from the repo root so gates 1-3 can find real files.
cd "$REPO_ROOT"

echo ""
echo "=========================================="
echo "  Doc Consistency Changelog Gate Tests"
echo "=========================================="

# ===================================================================
# Section 1: Internal path classification (each should PASS alone)
# ===================================================================
echo ""
echo -e "${YELLOW}--- Internal path classification ---${NC}"

# Data-driven table: each entry is an internal path that should pass
# without a CHANGELOG.md entry.
INTERNAL_PATHS=(
    # Dot-directories
    ".github/workflows/ci.yml"
    ".githooks/pre-commit"
    ".devcontainer/Dockerfile"
    ".config/cargo-deny.toml"
    ".vscode/settings.json"
    ".claude/settings.json"
    # Source-adjacent directories
    "scripts/check-foo.sh"
    "tests/unit_test.rs"
    "test-fixtures/data.json"
    ".llm/skills/foo.md"
    "target/debug/binary"
    "progress/notes.md"
    # Standalone internal files
    "Cargo.lock"
    "PLAN.md"
    "AGENTS.md"
    "pre-push.txt"
    "logs_2025.zip"
    # Linter/tool config (glob patterns)
    ".markdownlint.yaml"
    ".markdownlintrc"
    ".markdownlint.json"
    ".markdownlintignore"
    ".markdownlint-version"
    ".lychee.toml"
    ".lycheecache"
    ".typos.toml"
    ".yamllint.yml"
    # VCS/Docker ignore files
    ".gitignore"
    ".dockerignore"
    # CI/infrastructure docs
    "docs/ci-cd-testing.md"
    "docs/test-suite-analysis-ci-config.md"
    "docs/git-hooks-guide.md"
    "docs/hooks-quick-reference.md"
    "docs/pre-commit-hooks-summary.md"
    "docs/development.md"
    # Tool configuration files
    "clippy.toml"
    "deny.toml"
    "tarpaulin.toml"
    "rust-toolchain.toml"
    "mkdocs.yml"
    "requirements-docs.txt"
)

for path in "${INTERNAL_PATHS[@]}"; do
    assert_gate_passes "Internal: $path" "$path"
done

# ===================================================================
# Section 2: Non-internal path classification (each should FAIL alone)
# ===================================================================
echo ""
echo -e "${YELLOW}--- Non-internal path classification ---${NC}"

# Data-driven table: each entry is a non-internal path that should
# trigger a changelog gate failure when CHANGELOG.md is absent.
NON_INTERNAL_PATHS=(
    # Production source code
    "src/main.rs"
    "src/server.rs"
    "build.rs"
    "benches/benchmark.rs"
    # Project manifests
    "Cargo.toml"
    # User-facing docs
    "README.md"
    "docs/guide.md"
    "docs/library-usage.md"
    "docs/testing-guide.md"
    # Deployment artifacts
    "Dockerfile"
    "docker-compose.yml"
    "config.example.json"
    # Legal
    "LICENSE"
)

for path in "${NON_INTERNAL_PATHS[@]}"; do
    assert_gate_fails "Non-internal: $path" "non-internal changes without CHANGELOG" "$path"
done

# ===================================================================
# Section 3: Changelog gate behavior
# ===================================================================
echo ""
echo -e "${YELLOW}--- Changelog gate behavior ---${NC}"

# Table-driven gate tests.
# Format: "expected_result|test_name|file1 file2 ..."
#   expected_result: pass | fail
GATE_TESTS=(
    "pass|Only internal files changed|scripts/check-foo.sh .github/workflows/ci.yml tests/unit_test.rs"
    "fail|Non-internal file without CHANGELOG|src/main.rs"
    "pass|Non-internal file with CHANGELOG|src/main.rs CHANGELOG.md"
    "fail|Mix of internal + non-internal without CHANGELOG|scripts/check-foo.sh src/main.rs .github/workflows/ci.yml"
    "pass|Mix of internal + non-internal with CHANGELOG|scripts/check-foo.sh src/main.rs .github/workflows/ci.yml CHANGELOG.md"
    "fail|Multiple non-internal without CHANGELOG|src/main.rs Cargo.toml README.md"
    "pass|Multiple non-internal with CHANGELOG|src/main.rs Cargo.toml README.md CHANGELOG.md"
)

for entry in "${GATE_TESTS[@]}"; do
    IFS='|' read -r expected test_name files <<< "$entry"
    # shellcheck disable=SC2086
    # Word splitting on $files is intentional -- each space-separated
    # token becomes a separate argument to --changed-files.
    if [ "$expected" = "pass" ]; then
        assert_gate_passes "$test_name" $files
    else
        assert_gate_fails "$test_name" "non-internal changes without CHANGELOG" $files
    fi
done

# ===================================================================
# Section 4: --skip-changelog-gate flag
# ===================================================================
echo ""
echo -e "${YELLOW}--- --skip-changelog-gate flag ---${NC}"

assert_skip_passes "Skip gate: Cargo.toml + Cargo.lock (dependency bump scenario)" "Cargo.toml" "Cargo.lock"
assert_skip_passes "Skip gate: non-internal without CHANGELOG" "src/main.rs"
assert_skip_passes "Skip gate: multiple non-internal without CHANGELOG" "src/main.rs" "Cargo.toml" "README.md"
assert_skip_passes "Skip gate: mix of internal + non-internal without CHANGELOG" "scripts/foo.sh" "src/main.rs"

# ===================================================================
# Section 5: Edge cases
# ===================================================================
echo ""
echo -e "${YELLOW}--- Edge cases ---${NC}"

# Deeply nested internal paths.
assert_gate_passes "Deep nested .github" ".github/workflows/deep/nested.yml"
assert_gate_passes "Deep nested scripts" "scripts/sub/dir/deep.sh"
assert_gate_passes "Deep nested .llm" ".llm/skills/sub/deep/file.md"
assert_gate_passes "Deep nested tests" "tests/integration/sub/test.rs"
assert_gate_passes "Deep nested target" "target/release/build/dep/out"
assert_gate_passes "Deep nested .devcontainer" ".devcontainer/features/some-feature/install.sh"

# Verify CHANGELOG.md alone does not cause a failure.
assert_gate_passes "CHANGELOG.md alone" "CHANGELOG.md"

# Verify internal + CHANGELOG.md together passes.
assert_gate_passes "Internal + CHANGELOG.md" "scripts/foo.sh" "CHANGELOG.md"

# ===================================================================
# Summary
# ===================================================================
echo ""
echo "=========================================="
if [ "$TESTS_FAILED" -gt 0 ]; then
    echo -e "${RED}FAILED: $TESTS_FAILED test(s) failed, $TESTS_PASSED passed${NC}"
    exit 1
else
    echo -e "${GREEN}ALL PASSED: $TESTS_PASSED test(s) passed${NC}"
    exit 0
fi
