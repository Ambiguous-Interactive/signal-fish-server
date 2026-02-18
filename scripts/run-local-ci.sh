#!/usr/bin/env bash
# Signal Fish Server - Local CI Runner
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Runs all CI checks locally before pushing to catch issues early.
# This script mirrors the GitHub Actions CI workflow checks.
#
# Usage:
#   ./scripts/run-local-ci.sh           # Run all checks
#   ./scripts/run-local-ci.sh --fast    # Skip slow checks (tests)
#   ./scripts/run-local-ci.sh --fix     # Auto-fix issues where possible
#
# Exit codes:
#   0 = All checks passed
#   1 = One or more checks failed
#   2 = Invalid usage

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m' # No Color
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

# Parse arguments
FAST_MODE=false
FIX_MODE=false

for arg in "$@"; do
    case $arg in
        --fast)
            FAST_MODE=true
            shift
            ;;
        --fix)
            FIX_MODE=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--fast] [--fix]"
            echo ""
            echo "Options:"
            echo "  --fast    Skip slow checks (tests, full clippy)"
            echo "  --fix     Auto-fix issues where possible (fmt, clippy suggestions)"
            echo "  --help    Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            echo "Use --help for usage information"
            exit 2
            ;;
    esac
done

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BOLD}${BLUE}Local CI Runner${NC}"
echo -e "${BLUE}Repository: $REPO_ROOT${NC}"
if [ "$FAST_MODE" = true ]; then
    echo -e "${YELLOW}Mode: Fast (skipping tests and full linting)${NC}"
fi
if [ "$FIX_MODE" = true ]; then
    echo -e "${YELLOW}Mode: Auto-fix enabled${NC}"
fi
echo ""

FAILED_CHECKS=()
PASSED_CHECKS=()

# Helper to run a check
run_check() {
    local name="$1"
    local description="$2"
    shift 2
    local cmd=("$@")

    echo -e "${BOLD}${BLUE}[$name]${NC} $description"

    if "${cmd[@]}"; then
        echo -e "${GREEN}✓ PASS${NC}: $name"
        PASSED_CHECKS+=("$name")
        echo ""
        return 0
    else
        echo -e "${RED}✗ FAIL${NC}: $name"
        FAILED_CHECKS+=("$name")
        echo ""
        return 1
    fi
}

# Helper to run with suppressed output on success
run_check_quiet() {
    local name="$1"
    local description="$2"
    shift 2
    local cmd=("$@")

    echo -e "${BOLD}${BLUE}[$name]${NC} $description"

    local output
    if output=$("${cmd[@]}" 2>&1); then
        echo -e "${GREEN}✓ PASS${NC}: $name"
        PASSED_CHECKS+=("$name")
        echo ""
        return 0
    else
        echo -e "${RED}✗ FAIL${NC}: $name"
        echo "$output"
        FAILED_CHECKS+=("$name")
        echo ""
        return 1
    fi
}

# Check 1: Code Formatting
if [ "$FIX_MODE" = true ]; then
    run_check "format" "Running cargo fmt (auto-fix)" \
        cargo fmt
else
    run_check "format" "Checking code formatting" \
        cargo fmt --check
fi

# Check 2: Clippy (default features)
if [ "$FIX_MODE" = true ]; then
    run_check "clippy-default" "Running clippy with auto-fix (default features)" \
        cargo clippy --fix --allow-dirty --allow-staged --all-targets -- -D warnings || true
else
    if [ "$FAST_MODE" = false ]; then
        run_check "clippy-default" "Running clippy (default features)" \
            cargo clippy --locked --all-targets -- -D warnings
    fi
fi

# Check 3: Clippy (all features)
if [ "$FIX_MODE" = true ]; then
    run_check "clippy-all" "Running clippy with auto-fix (all features)" \
        cargo clippy --fix --allow-dirty --allow-staged --all-targets --all-features -- -D warnings || true
else
    run_check "clippy-all" "Running clippy (all features)" \
        cargo clippy --locked --all-targets --all-features -- -D warnings
fi

# Check 4: Tests (default features)
if [ "$FAST_MODE" = false ]; then
    run_check "test-default" "Running tests (default features)" \
        cargo test --locked
fi

# Check 5: Tests (all features)
if [ "$FAST_MODE" = false ]; then
    run_check "test-all" "Running tests (all features)" \
        cargo test --locked --all-features
fi

# Check 6: MSRV Consistency
if [ -f scripts/check-msrv-consistency.sh ]; then
    run_check_quiet "msrv" "Checking MSRV consistency" \
        scripts/check-msrv-consistency.sh
fi

# Check 7: Workflow Hygiene
if [ -f scripts/check-workflow-hygiene.sh ]; then
    run_check_quiet "workflow-hygiene" "Checking workflow hygiene" \
        scripts/check-workflow-hygiene.sh
fi

# Check 8: AWK Script Validation
if [ -f scripts/validate-workflow-awk.sh ]; then
    run_check_quiet "awk-validation" "Validating AWK scripts in workflows" \
        scripts/validate-workflow-awk.sh
fi

# Check 9: No Panic Patterns
if [ -f scripts/check-no-panics.sh ]; then
    run_check_quiet "no-panics" "Checking for panic-prone patterns" \
        scripts/check-no-panics.sh patterns
fi

# Check 10: CI Configuration Validation (AWK, shell, markdown links)
if [ -f scripts/validate-ci.sh ]; then
    run_check_quiet "ci-validation" "Validating CI configuration (AWK, shell, links)" \
        scripts/validate-ci.sh --quiet
fi

# Check 11: Markdown Linting
if command -v markdownlint-cli2 >/dev/null 2>&1; then
    if [ -f scripts/check-markdown.sh ]; then
        if [ "$FIX_MODE" = true ]; then
            run_check_quiet "markdown" "Fixing markdown files" \
                scripts/check-markdown.sh fix
        else
            run_check_quiet "markdown" "Checking markdown files" \
                scripts/check-markdown.sh
        fi
    fi
else
    echo -e "${YELLOW}⊘ SKIP${NC}: markdown (markdownlint-cli2 not installed)"
    echo ""
fi

# Summary
echo "=========================================="
echo -e "${BOLD}Summary${NC}"
echo ""
echo -e "Passed: ${GREEN}${#PASSED_CHECKS[@]}${NC}"
echo -e "Failed: ${RED}${#FAILED_CHECKS[@]}${NC}"
echo ""

if [ "${#FAILED_CHECKS[@]}" -gt 0 ]; then
    echo -e "${RED}Failed checks:${NC}"
    for check in "${FAILED_CHECKS[@]}"; do
        echo -e "  ${RED}✗${NC} $check"
    done
    echo ""

    if [ "$FIX_MODE" = false ]; then
        echo -e "${YELLOW}Tip: Run with --fix to auto-fix some issues${NC}"
        echo ""
    fi

    echo -e "${RED}CI checks failed. Fix issues before pushing.${NC}"
    exit 1
else
    echo -e "${GREEN}${BOLD}All checks passed!${NC}"
    echo ""
    echo -e "Your code is ready to push. 🚀"
    exit 0
fi
