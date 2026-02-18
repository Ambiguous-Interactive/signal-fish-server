#!/usr/bin/env bash
# Signal Fish Server - MSRV Consistency Checker
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Validates that all configuration files use the same Rust version
# as defined in Cargo.toml (the single source of truth for MSRV).
#
# This script is run:
# - By CI (`.github/workflows/ci.yml` msrv job)
# - Locally before committing MSRV changes
# - As part of pre-commit hooks (optional)
#
# Exit codes:
#   0 = All checks passed
#   1 = MSRV inconsistency detected
#   2 = Invalid usage or missing files

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m' # No Color
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

# Find repository root (supports running from any subdirectory)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BLUE}MSRV Consistency Checker${NC}"
echo "Repository: $REPO_ROOT"
echo ""

# Extract MSRV from Cargo.toml (single source of truth)
if [ ! -f Cargo.toml ]; then
    echo -e "${RED}ERROR: Cargo.toml not found in repository root${NC}"
    exit 2
fi

MSRV=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "(.+)"/\1/')

if [ -z "$MSRV" ]; then
    echo -e "${RED}ERROR: Could not extract rust-version from Cargo.toml${NC}"
    echo "Expected format: rust-version = \"1.88.0\""
    exit 2
fi

echo -e "${BLUE}Canonical MSRV (from Cargo.toml):${NC} ${GREEN}$MSRV${NC}"
echo ""
echo "Verifying consistency across configuration files..."
echo "=========================================="

# Track failures
FAILED=0
CHECKS=0

# Helper function to report check results
check_file() {
    local file="$1"
    local expected="$2"
    local actual="$3"
    local field="$4"

    CHECKS=$((CHECKS + 1))

    if [ "$actual" = "$expected" ]; then
        echo -e "${GREEN}✓ PASS${NC}: $file ($field=$actual)"
    else
        echo -e "${RED}✗ FAIL${NC}: $file ($field=${RED}$actual${NC}, expected ${GREEN}$expected${NC})"
        FAILED=$((FAILED + 1))
    fi
}

check_missing() {
    local file="$1"

    CHECKS=$((CHECKS + 1))

    echo -e "${YELLOW}⚠ WARNING${NC}: $file not found"
}

# Check 1: rust-toolchain.toml
if [ -f rust-toolchain.toml ]; then
    TOOLCHAIN_VERSION=$(grep '^channel = ' rust-toolchain.toml | sed -E 's/channel = "(.+)"/\1/')
    check_file "rust-toolchain.toml" "$MSRV" "$TOOLCHAIN_VERSION" "channel"
else
    check_missing "rust-toolchain.toml"
    FAILED=$((FAILED + 1))
fi

# Check 2: clippy.toml
if [ -f clippy.toml ]; then
    CLIPPY_MSRV=$(grep '^msrv = ' clippy.toml | sed -E 's/msrv = "(.+)"/\1/')
    check_file "clippy.toml" "$MSRV" "$CLIPPY_MSRV" "msrv"
else
    check_missing "clippy.toml"
fi

# Check 3: Dockerfile (production build)
if [ -f Dockerfile ]; then
    # Extract Rust version from FROM rust:X.Y line (handles both 1.88 and 1.88.0 formats)
    DOCKERFILE_RUST=$(grep '^FROM rust:' Dockerfile | head -1 | sed -E 's/FROM rust:([0-9]+\.[0-9]+).*/\1/')
    # Normalize MSRV to major.minor for comparison (1.88.0 -> 1.88)
    MSRV_SHORT=$(echo "$MSRV" | sed -E 's/([0-9]+\.[0-9]+).*/\1/')
    check_file "Dockerfile" "$MSRV_SHORT" "$DOCKERFILE_RUST" "rust"
else
    check_missing "Dockerfile"
fi

# Check 4: .devcontainer/Dockerfile (informational only - may use newer Rust)
if [ -f .devcontainer/Dockerfile ]; then
    # Extract MSRV comment if present
    if grep -q "# Project MSRV:" .devcontainer/Dockerfile; then
        DEVCONTAINER_COMMENT=$(grep "# Project MSRV:" .devcontainer/Dockerfile | sed -E 's/.*MSRV: ([0-9]+\.[0-9]+\.[0-9]+).*/\1/')
        if [ "$DEVCONTAINER_COMMENT" = "$MSRV" ]; then
            echo -e "${GREEN}✓ INFO${NC}: .devcontainer/Dockerfile (MSRV comment correct: $DEVCONTAINER_COMMENT)"
        else
            echo -e "${YELLOW}⚠ INFO${NC}: .devcontainer/Dockerfile (MSRV comment: ${YELLOW}$DEVCONTAINER_COMMENT${NC}, current MSRV: ${GREEN}$MSRV${NC})"
            echo "  Note: Devcontainer may use newer Rust; this is informational only."
        fi
    else
        echo -e "${YELLOW}⚠ INFO${NC}: .devcontainer/Dockerfile (no MSRV comment found)"
        echo "  Consider adding: # Project MSRV: $MSRV"
    fi
else
    # Devcontainer is optional, not a failure
    :
fi

echo "=========================================="
echo ""

# Summary
if [ "$FAILED" -ne 0 ]; then
    echo -e "${RED}FAILED${NC}: $FAILED of $CHECKS checks failed"
    echo ""
    echo -e "${YELLOW}To fix MSRV inconsistencies:${NC}"
    echo ""
    echo "1. Update rust-toolchain.toml:"
    echo "   channel = \"$MSRV\""
    echo ""
    echo "2. Update clippy.toml:"
    echo "   msrv = \"$MSRV\""
    echo ""
    echo "3. Update Dockerfile:"
    echo "   FROM rust:$MSRV-bookworm"
    echo ""
    echo "4. Update .devcontainer/Dockerfile (optional):"
    echo "   # Project MSRV: $MSRV"
    echo ""
    echo "See .llm/skills/msrv-and-toolchain-management.md for detailed guidance."
    echo ""
    exit 1
else
    echo -e "${GREEN}SUCCESS${NC}: All $CHECKS MSRV consistency checks passed ✓"
    echo ""
    echo "All configuration files are consistent with MSRV: $MSRV"
    exit 0
fi
