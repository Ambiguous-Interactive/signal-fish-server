#!/usr/bin/env bash
# Signal Fish Server - Local CI Runner
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Runs all CI checks locally before pushing to catch issues early.
# This script mirrors the GitHub Actions CI workflow checks.
#
# Usage:
#   ./scripts/run-local-ci.sh           # Run all checks
#   ./scripts/run-local-ci.sh --fast    # Skip slow checks (tests, clippy, nested policy scans)
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
            echo "  --fast    Skip slow checks (tests, clippy, nested policy scans)"
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
    echo -e "${YELLOW}Mode: Fast (skipping tests, clippy, and nested policy scans)${NC}"
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
        return 0
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
        if grep -qE '(^|\]) WARN(:|])|WARN:' <<< "$output"; then
            echo "$output"
        fi
        PASSED_CHECKS+=("$name")
        echo ""
        return 0
    else
        echo -e "${RED}✗ FAIL${NC}: $name"
        echo "$output"
        FAILED_CHECKS+=("$name")
        echo ""
        return 0
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
    if [ "$FAST_MODE" = false ]; then
        run_check "clippy-all" "Running clippy (all features)" \
            cargo clippy --locked --all-targets --all-features -- -D warnings
    fi
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
if [ "$FAST_MODE" = false ] && [ -f scripts/check-no-panics.sh ]; then
    run_check_quiet "no-panics" "Checking for panic-prone patterns" \
        scripts/check-no-panics.sh
fi

# Check 10: Hook readiness and worktree preflight
if command -v pwsh > /dev/null 2>&1; then
    run_check_quiet "hook-readiness" "Checking git hook readiness" \
        pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1

    run_check_quiet "pre-commit-preflight" "Running fast worktree-scoped pre-commit policies" \
        pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree

    run_check_quiet "pre-push-preflight" "Running fast worktree-scoped pre-push policies" \
        pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree
else
    echo -e "${RED}✗ FAIL${NC}: hook-readiness (PowerShell 7+ 'pwsh' not found)"
    echo ""
    FAILED_CHECKS+=("hook-readiness")
fi

# Check 11: CI Configuration Validation (AWK, shell, markdown links, tooling parity)
if [ -f scripts/validate-ci.sh ]; then
    run_check_quiet "ci-validation" "Validating CI configuration (AWK, shell, links, tooling parity)" \
        scripts/validate-ci.sh --quiet
fi

# Check 12: GitHub Actions syntax validation
if command -v actionlint > /dev/null 2>&1; then
    ACTIONLINT_WORKFLOWS=()
    for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
        [ -f "$workflow" ] && ACTIONLINT_WORKFLOWS+=("$workflow")
    done

    run_check_quiet "actionlint" "Validating GitHub Actions workflow syntax" \
        actionlint "${ACTIONLINT_WORKFLOWS[@]}"
else
    echo -e "${YELLOW}⚠ SKIP${NC}: actionlint (actionlint not installed)"
    echo ""
fi

# Check 13: Markdown Linting
if [ -f scripts/check-markdown.sh ]; then
    if [ "$FIX_MODE" = true ]; then
        echo -e "${BOLD}${BLUE}[markdown]${NC} Fixing markdown files"
        MARKDOWN_ARGS=(fix)
    else
        echo -e "${BOLD}${BLUE}[markdown]${NC} Checking markdown files"
        MARKDOWN_ARGS=()
    fi

    if MARKDOWN_OUTPUT=$(scripts/check-markdown.sh "${MARKDOWN_ARGS[@]}" 2>&1); then
        echo -e "${GREEN}✓ PASS${NC}: markdown"
        PASSED_CHECKS+=("markdown")
        echo ""
    else
        MARKDOWN_STATUS=$?
        if [ "$MARKDOWN_STATUS" -eq 2 ]; then
            echo -e "${RED}✗ FAIL${NC}: markdown (pinned markdownlint-cli2 unavailable or version mismatch)"
            echo "$MARKDOWN_OUTPUT"
            echo ""
            FAILED_CHECKS+=("markdown")
        else
            echo -e "${RED}✗ FAIL${NC}: markdown"
            echo "$MARKDOWN_OUTPUT"
            echo ""
            FAILED_CHECKS+=("markdown")
        fi
    fi
else
    echo -e "${RED}✗ FAIL${NC}: markdown (scripts/check-markdown.sh not found)"
    echo ""
    FAILED_CHECKS+=("markdown")
fi

# Check 14: Agent Skills package policy
if [ -f scripts/check-agent-skill-files.sh ]; then
    run_check_quiet "agent-skill-files" "Checking Agent Skill file sizes" \
        scripts/check-agent-skill-files.sh
else
    echo -e "${RED}✗ FAIL${NC}: agent-skill-files (scripts/check-agent-skill-files.sh not found)"
    echo ""
    FAILED_CHECKS+=("agent-skill-files")
fi

if [ -f scripts/validate-agent-skills.sh ]; then
    run_check_quiet "agent-skills" "Validating Agent Skills packages" \
        scripts/validate-agent-skills.sh
else
    echo -e "${RED}✗ FAIL${NC}: agent-skills (scripts/validate-agent-skills.sh not found)"
    echo ""
    FAILED_CHECKS+=("agent-skills")
fi

if [ -f scripts/generate-skills-index.sh ]; then
    run_check_quiet "agent-skills-index" "Checking Agent Skills catalog freshness" \
        scripts/generate-skills-index.sh --check
else
    echo -e "${RED}✗ FAIL${NC}: agent-skills-index (scripts/generate-skills-index.sh not found)"
    echo ""
    FAILED_CHECKS+=("agent-skills-index")
fi

# Check 15: README Badge Style Consistency
if [ -f scripts/check-readme-badges.sh ]; then
    run_check_quiet "readme-badges" "Checking Shields badge style consistency in README" \
        scripts/check-readme-badges.sh README.md
fi

# Check 16: Dockerfile shell portability
if [ -f scripts/check-dockerfile-portability.sh ]; then
    run_check_quiet "dockerfile-portability" "Checking Dockerfile shell portability" \
        scripts/check-dockerfile-portability.sh --quiet
fi

# Check 17: Dependency Advisory Check
if [ -f scripts/check-advisories.sh ]; then
    run_check_quiet "advisories" "Checking for RUSTSEC dependency advisories" \
        scripts/check-advisories.sh
else
    echo -e "${YELLOW}⚠ SKIP${NC}: advisories (scripts/check-advisories.sh not found)"
    echo ""
fi

# Check 18: Documentation + changelog consistency
if [ -f scripts/check-doc-consistency.sh ]; then
    run_check_quiet "doc-consistency" "Checking docs/changelog/version consistency" \
        scripts/check-doc-consistency.sh
else
    echo -e "${RED}✗ FAIL${NC}: doc-consistency (scripts/check-doc-consistency.sh not found)"
    echo ""
    FAILED_CHECKS+=("doc-consistency")
fi

# Check 19: Documentation consistency policy tests
if [ "$FAST_MODE" = false ]; then
    run_check "doc-policy-tests" "Running docs/changelog policy tests" \
        cargo test --locked --test doc_consistency_policy_tests --test doc_consistency_script_tests
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
