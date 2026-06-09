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

NESTED_CARGO_TARGET_DIR="${NESTED_CARGO_TARGET_DIR:-$REPO_ROOT/target/no-panic-policy-scan}"
NESTED_CARGO_ENV_VARS=(
    RUSTFLAGS
    CARGO_ENCODED_RUSTFLAGS
    RUSTDOCFLAGS
    CARGO_TARGET_DIR
    ASAN_OPTIONS
    LSAN_OPTIONS
    UBSAN_OPTIONS
    TSAN_OPTIONS
    MIRIFLAGS
)

run_nested_cargo() {
    local inherited=""
    local var
    for var in "${NESTED_CARGO_ENV_VARS[@]}"; do
        if [ -n "${!var+x}" ]; then
            if [ -n "$inherited" ]; then
                inherited="$inherited $var"
            else
                inherited="$var"
            fi
        fi
    done

    if [ -n "$inherited" ]; then
        log "Scrubbing inherited Cargo instrumentation env for nested Cargo: $inherited"
    fi
    log "Nested Cargo command: cargo $*"
    log "Nested Cargo target dir: $NESTED_CARGO_TARGET_DIR"

    (
        for var in "${NESTED_CARGO_ENV_VARS[@]}"; do
            unset "$var"
        done
        export CARGO_TARGET_DIR="$NESTED_CARGO_TARGET_DIR"
        cargo "$@"
    )
}

# ============================================================================
# CLIPPY CHECKS - Deny panic-related lints
# ============================================================================
check_clippy() {
    log "Running clippy with panic-related lints as errors..."

    if ! command -v cargo >/dev/null 2>&1; then
        warn "cargo not found; skipping clippy checks"
        return 0
    fi

    # Run clippy with strict panic-prevention lints on library and binary code.
    # We use --lib --bins instead of --all-targets to avoid flagging test code,
    # where .unwrap(), .expect(), and panic!() are acceptable.
    # These lints catch code that could panic at runtime:
    # - clippy::panic: explicit panic!() calls
    # - clippy::unwrap_used: .unwrap() calls
    # - clippy::expect_used: .expect() calls
    # - clippy::todo: todo!() macros
    # - clippy::unimplemented: unimplemented!() macros
    # - clippy::unreachable: unreachable!() macros
    # - clippy::indexing_slicing: unchecked array/slice indexing
    if run_nested_cargo clippy --locked --lib --bins --all-features -- \
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
    log "Running syn-based panic-prone macro scan for production Rust..."

    if ! command -v cargo >/dev/null 2>&1; then
        warn "cargo not found; skipping syn-based pattern scan"
        return 0
    fi

    if ! run_nested_cargo test --locked --test no_panic_policy_scan --quiet; then
        error "Syn-based panic-prone macro scan failed"
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
