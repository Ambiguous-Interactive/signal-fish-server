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

    # Run clippy with strict panic-prevention lints on library code only.
    # We use --lib instead of --all-targets to avoid flagging test code,
    # where .unwrap(), .expect(), and panic!() are acceptable.
    # These lints catch code that could panic at runtime:
    # - clippy::panic: explicit panic!() calls
    # - clippy::unwrap_used: .unwrap() calls
    # - clippy::expect_used: .expect() calls
    # - clippy::todo: todo!() macros
    # - clippy::unimplemented: unimplemented!() macros
    # - clippy::unreachable: unreachable!() macros
    # - clippy::indexing_slicing: unchecked array/slice indexing
    if cargo clippy --lib --all-features -- \
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

# filter_test_code - Filters out matches that are inside #[cfg(test)] modules.
#
# Reads grep output in the format "file:line:content" and for each match,
# checks whether the line falls within a #[cfg(test)] module in that file.
# Since test modules are conventionally placed at the bottom of Rust source
# files, any line at or after a #[cfg(test)] annotation is considered test code.
# Also filters out lines inside #[test] functions and files under tests/.
filter_test_code() {
    while IFS= read -r match_line; do
        # Extract file path and line number from grep output (file:line:content)
        local file line_num
        file=$(echo "$match_line" | cut -d: -f1)
        line_num=$(echo "$match_line" | cut -d: -f2)

        # Skip files in tests/ directory and test module files
        case "$file" in
            tests/*) continue ;;
            *_tests.rs|*_test.rs) continue ;;
        esac

        # Find the line number where #[cfg(test)] appears in this file.
        # If the match is at or after that line, it's test code — skip it.
        local cfg_test_line
        cfg_test_line=$(grep -n '#\[cfg(test)\]' "$file" 2>/dev/null | head -1 | cut -d: -f1)
        if [ -n "$cfg_test_line" ] && [ "$line_num" -ge "$cfg_test_line" ]; then
            continue
        fi

        # Also check if the line is inside a #[test] function by scanning
        # backwards from the match line for #[test] or #[tokio::test] attributes
        local in_test_fn
        in_test_fn=$(head -n "$line_num" "$file" 2>/dev/null | tac | awk '
            /^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/ {
                # We hit a function definition before finding a test attribute
                print "no"; exit
            }
            /^[[:space:]]*#\[test\]/ || /^[[:space:]]*#\[tokio::test/ {
                print "yes"; exit
            }
        ')
        if [ "$in_test_fn" = "yes" ]; then
            continue
        fi

        # This match is in production code — keep it
        echo "$match_line"
    done
}

check_patterns() {
    log "Scanning for panic-prone patterns in src/..."

    local issues=0

    # Check for explicit panic! macros (excluding tests and comments)
    PANIC_MATCHES=$(grep -rn 'panic!' src/ --include="*.rs" 2>/dev/null \
        | grep -v '//.*panic!' \
        | grep -v '#\[should_panic\]' \
        | filter_test_code || true)
    PANIC_COUNT=$(echo "$PANIC_MATCHES" | grep -c . || echo "0")
    PANIC_COUNT=$(echo "$PANIC_COUNT" | tr -d '[:space:]')
    if [ "$PANIC_COUNT" -gt 0 ]; then
        warn "Found $PANIC_COUNT panic! macro(s) in production code:"
        echo "$PANIC_MATCHES" | head -10
        issues=$((issues + 1))
    fi

    # Check for todo! macros (excluding test code)
    TODO_MATCHES=$(grep -rn 'todo!' src/ --include="*.rs" 2>/dev/null \
        | grep -v '//.*todo!' \
        | filter_test_code || true)
    TODO_COUNT=$(echo "$TODO_MATCHES" | grep -c . || echo "0")
    TODO_COUNT=$(echo "$TODO_COUNT" | tr -d '[:space:]')
    if [ "$TODO_COUNT" -gt 0 ]; then
        error "Found $TODO_COUNT todo! macro(s) in production code (must be completed before merge):"
        echo "$TODO_MATCHES" | head -10
        issues=$((issues + 1))
    fi

    # Check for unimplemented! macros (excluding test code)
    UNIMPL_MATCHES=$(grep -rn 'unimplemented!' src/ --include="*.rs" 2>/dev/null \
        | grep -v '//.*unimplemented!' \
        | filter_test_code || true)
    UNIMPL_COUNT=$(echo "$UNIMPL_MATCHES" | grep -c . || echo "0")
    UNIMPL_COUNT=$(echo "$UNIMPL_COUNT" | tr -d '[:space:]')
    if [ "$UNIMPL_COUNT" -gt 0 ]; then
        error "Found $UNIMPL_COUNT unimplemented! macro(s) in production code:"
        echo "$UNIMPL_MATCHES" | head -10
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
