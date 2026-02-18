#!/usr/bin/env bash
# check-no-panics.sh - Checks for panic-prone patterns in Rust production code
#
# This script enforces zero-panic production code by:
# 1. Running clippy with strict panic-related lints
# 2. Scanning for explicit panic patterns in source code
#
# Exit codes:
#   0 - No panic-prone patterns found
#   1 - Panic-prone patterns detected (blocks commit/CI)

set -euo pipefail

# Move to repo root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

log() { echo "[no-panics] $*"; }
error() { echo "[no-panics] ERROR: $*" >&2; }
warn() { echo "[no-panics] WARNING: $*" >&2; }

FAILED=0

# ============================================================================
# CLIPPY CHECKS - Deny panic-related lints
# ============================================================================
check_clippy() {
    log "Running clippy with panic-related lints as errors..."

    if ! command -v cargo >/dev/null 2>&1; then
        warn "cargo not found; skipping clippy checks"
        return 0
    fi

    # Run clippy with strict panic-prevention lints
    # These lints catch code that could panic at runtime:
    # - clippy::panic: explicit panic!() calls
    # - clippy::unwrap_used: .unwrap() calls
    # - clippy::expect_used: .expect() calls
    # - clippy::todo: todo!() macros
    # - clippy::unimplemented: unimplemented!() macros
    # - clippy::unreachable: unreachable!() macros
    # - clippy::indexing_slicing: unchecked array/slice indexing
    if cargo clippy --all-targets --all-features -- \
        -D clippy::panic \
        -D clippy::unwrap_used \
        -D clippy::expect_used \
        -D clippy::todo \
        -D clippy::unimplemented \
        -D clippy::unreachable \
        -D clippy::indexing_slicing \
        2>&1; then
        log "Clippy panic checks passed"
        return 0
    else
        error "Clippy detected panic-prone patterns"
        return 1
    fi
}

# ============================================================================
# PATTERN SCANNING - Quick grep-based checks
# ============================================================================
check_patterns() {
    log "Scanning for panic-prone patterns in src/..."

    local issues=0

    # Check for explicit panic! macros (excluding tests and comments)
    PANIC_COUNT=$(grep -rn 'panic!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*panic!' | grep -v '#\[should_panic\]' | grep -v 'test' | wc -l || echo "0")
    if [ "$PANIC_COUNT" -gt 0 ]; then
        warn "Found $PANIC_COUNT panic! macro(s) in production code:"
        grep -rn 'panic!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*panic!' | grep -v '#\[should_panic\]' | grep -v 'test' | head -10 || true
        issues=$((issues + 1))
    fi

    # Check for todo! macros
    TODO_COUNT=$(grep -rn 'todo!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*todo!' | wc -l || echo "0")
    if [ "$TODO_COUNT" -gt 0 ]; then
        error "Found $TODO_COUNT todo! macro(s) in production code (must be completed before merge):"
        grep -rn 'todo!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*todo!' | head -10 || true
        issues=$((issues + 1))
    fi

    # Check for unimplemented! macros
    UNIMPL_COUNT=$(grep -rn 'unimplemented!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*unimplemented!' | wc -l || echo "0")
    if [ "$UNIMPL_COUNT" -gt 0 ]; then
        error "Found $UNIMPL_COUNT unimplemented! macro(s) in production code:"
        grep -rn 'unimplemented!' src/ --include="*.rs" 2>/dev/null | grep -v '//.*unimplemented!' | head -10 || true
        issues=$((issues + 1))
    fi

    # Count .unwrap() usage (warning, not error - clippy is the source of truth)
    UNWRAP_COUNT=$(grep -rc '\.unwrap()' src/ --include="*.rs" 2>/dev/null | awk -F: '{sum+=$2} END {print sum+0}')
    if [ "$UNWRAP_COUNT" -gt 0 ]; then
        warn "Found $UNWRAP_COUNT .unwrap() call(s) - these will be caught by clippy"
    fi

    # Count .expect() usage (warning, not error - clippy is the source of truth)
    EXPECT_COUNT=$(grep -rc '\.expect(' src/ --include="*.rs" 2>/dev/null | awk -F: '{sum+=$2} END {print sum+0}')
    if [ "$EXPECT_COUNT" -gt 0 ]; then
        warn "Found $EXPECT_COUNT .expect() call(s) - these will be caught by clippy"
    fi

    if [ "$issues" -gt 0 ]; then
        return 1
    fi

    log "Pattern scan passed"
    return 0
}

# ============================================================================
# SUMMARY
# ============================================================================
print_summary() {
    echo ""
    echo "========================================"
    echo "NO-PANICS CHECK SUMMARY"
    echo "========================================"
    if [ "$FAILED" -eq 0 ]; then
        echo "Status: PASSED"
        echo ""
        echo "Your code is free of panic-prone patterns."
    else
        echo "Status: FAILED"
        echo ""
        echo "Your code contains patterns that could panic at runtime."
        echo ""
        echo "To fix:"
        echo "  - Replace .unwrap() with .ok_or()? or .unwrap_or_default()"
        echo "  - Replace .expect() with proper error handling"
        echo "  - Remove todo!(), unimplemented!(), panic!() macros"
        echo "  - Use .get() instead of [index] for array access"
        echo ""
        echo "See .llm/context.md 'Defensive Programming' section for patterns."
    fi
    echo "========================================"
}

# ============================================================================
# MAIN
# ============================================================================
main() {
    log "Checking for panic-prone patterns in production Rust code..."
    echo ""

    # Run pattern scanning first (fast)
    if ! check_patterns; then
        FAILED=1
    fi

    # Run clippy checks (more thorough but slower)
    if ! check_clippy; then
        FAILED=1
    fi

    print_summary
    exit $FAILED
}

# Allow running specific checks via arguments
case "${1:-all}" in
    clippy)
        check_clippy
        exit $?
        ;;
    patterns)
        check_patterns
        exit $?
        ;;
    all|*)
        main
        ;;
esac
