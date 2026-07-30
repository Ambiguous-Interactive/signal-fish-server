#!/usr/bin/env bash
# Signal Fish Server - MSRV Consistency Checker
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Validates that server configuration files use the Rust version defined in
# Cargo.toml. The Godot/WASM fixture has a separate floor imposed by its exact
# released adapter dependency.
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

MSRV=$(bash scripts/read-toml-string.sh Cargo.toml rust-version package || true)
FORTRESS_WASM_MSRV_REQUIRED="1.94.0"

if [ -z "$MSRV" ]; then
    echo -e "${RED}ERROR: Could not extract rust-version from Cargo.toml${NC}"
    echo "Expected a TOML assignment like: rust-version = \"1.88.0\""
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
    TOOLCHAIN_VERSION=$(bash scripts/read-toml-string.sh rust-toolchain.toml channel toolchain || true)
    check_file "rust-toolchain.toml" "$MSRV" "$TOOLCHAIN_VERSION" "channel"
else
    check_missing "rust-toolchain.toml"
    FAILED=$((FAILED + 1))
fi

# Check 2: clippy.toml
if [ -f clippy.toml ]; then
    CLIPPY_MSRV=$(bash scripts/read-toml-string.sh clippy.toml msrv || true)
    check_file "clippy.toml" "$MSRV" "$CLIPPY_MSRV" "msrv"
else
    check_missing "clippy.toml"
fi

# Check 3: Dockerfile (production build)
if [ -f Dockerfile ]; then
    # Extract the Rust version from the first `FROM ... rust:X.Y ...` stage.
    # Tolerant of build flags before the image (`FROM --platform=$BUILDPLATFORM
    # rust:1.88-bookworm`), an `AS <stage>` suffix, digests, and both 1.88 and
    # 1.88.0 forms. Anchoring on ` rust:` (whitespace-preceded) rather than the
    # start of the line is what makes a multi-arch `--platform` base -- a legit
    # Dockerfile change -- not silently read as an empty version. POSIX classes
    # only (no \b / \s) so it behaves identically under BSD sed (macOS CI).
    DOCKERFILE_RUST=$(sed -nE '/^FROM[[:space:]].*rust:[0-9]/ { s/.*[[:space:]]rust:([0-9]+\.[0-9]+).*/\1/; p; q; }' Dockerfile)
    # Normalize MSRV to major.minor for comparison (1.88.0 -> 1.88)
    MSRV_SHORT=$(echo "$MSRV" | sed -E 's/([0-9]+\.[0-9]+).*/\1/')
    check_file "Dockerfile" "$MSRV_SHORT" "$DOCKERFILE_RUST" "rust"
else
    check_missing "Dockerfile"
fi

# Check 4: clients/native/Cargo.toml (standalone reference client)
# ADR-0004: the client lives outside the root package but pins the SAME
# rust-version as the server; enforce the pin so the claim cannot drift.
# A missing manifest is a hard failure: if clients/native ever moves, this
# check must be updated rather than silently dropping coverage.
if [ -f clients/native/Cargo.toml ]; then
    CLIENT_MSRV=$(bash scripts/read-toml-string.sh clients/native/Cargo.toml rust-version package || true)
    check_file "clients/native/Cargo.toml" "$MSRV" "$CLIENT_MSRV" "rust-version"
else
    check_missing "clients/native/Cargo.toml"
    FAILED=$((FAILED + 1))
fi

# Check 5: fuzz/Cargo.toml (standalone coverage-guided fuzz package)
# The nightly-only sanitizer runner is separate, but the package and locked
# graph must still type-check at the server MSRV in the always-on CI gate.
if [ -f fuzz/Cargo.toml ]; then
    FUZZ_MSRV=$(bash scripts/read-toml-string.sh fuzz/Cargo.toml rust-version package || true)
    check_file "fuzz/Cargo.toml" "$MSRV" "$FUZZ_MSRV" "rust-version"
else
    check_missing "fuzz/Cargo.toml"
    FAILED=$((FAILED + 1))
fi

# Check 6: clients/fortress/Cargo.toml (standalone interoperability fixture)
# This crate also runs under the repository toolchain in CI. Keep its declared
# floor aligned so a server MSRV change cannot strand the issue-242 gate.
if [ -f clients/fortress/Cargo.toml ]; then
    FORTRESS_MSRV=$(bash scripts/read-toml-string.sh clients/fortress/Cargo.toml rust-version package || true)
    check_file "clients/fortress/Cargo.toml" "$MSRV" "$FORTRESS_MSRV" "rust-version"
else
    check_missing "clients/fortress/Cargo.toml"
    FAILED=$((FAILED + 1))
fi

# Check 7: clients/fortress-wasm/Cargo.toml (Godot/WASM interoperability fixture)
# signal-fish-client-godot 0.9.0 requires Rust 1.94. Keep this standalone
# fixture honest about that higher dependency-imposed floor; it builds with the
# separately pinned nightly toolchain in fortress-wasm-interop.yml.
if [ -f clients/fortress-wasm/Cargo.toml ]; then
    FORTRESS_WASM_MSRV=$(bash scripts/read-toml-string.sh clients/fortress-wasm/Cargo.toml rust-version package || true)
    check_file "clients/fortress-wasm/Cargo.toml" "$FORTRESS_WASM_MSRV_REQUIRED" "$FORTRESS_WASM_MSRV" "rust-version"
else
    check_missing "clients/fortress-wasm/Cargo.toml"
    FAILED=$((FAILED + 1))
fi

# Check 8: .devcontainer/Dockerfile (informational only - may use newer Rust)
if [ -f .devcontainer/Dockerfile ]; then
    # Extract MSRV comment if present
    if grep -q "# Project MSRV:" .devcontainer/Dockerfile; then
        # Tolerant of `1.88` and `1.88.0` and extra spacing; `-n ... p` yields an
        # empty value on a malformed comment instead of echoing the whole line
        # (same brittle-parse class hardened for the production Dockerfile above).
        DEVCONTAINER_COMMENT=$(grep "# Project MSRV:" .devcontainer/Dockerfile | sed -nE 's/.*MSRV:[[:space:]]*([0-9]+\.[0-9]+(\.[0-9]+)?).*/\1/p')
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
    echo "4. Update clients/native/Cargo.toml:"
    echo "   rust-version = \"$MSRV\""
    echo ""
    echo "5. Update fuzz/Cargo.toml:"
    echo "   rust-version = \"$MSRV\""
    echo ""
    echo "6. Update clients/fortress/Cargo.toml:"
    echo "   rust-version = \"$MSRV\""
    echo ""
    echo "7. Update clients/fortress-wasm/Cargo.toml:"
    echo "   rust-version = \"$FORTRESS_WASM_MSRV_REQUIRED\""
    echo ""
    echo "8. Update .devcontainer/Dockerfile (optional):"
    echo "   # Project MSRV: $MSRV"
    echo ""
    echo "See .llm/skills/msrv-management/SKILL.md for detailed guidance."
    echo ""
    exit 1
else
    echo -e "${GREEN}SUCCESS${NC}: All $CHECKS MSRV consistency checks passed ✓"
    echo ""
    echo "All server configuration files are consistent with MSRV: $MSRV"
    echo "Godot/WASM fixture is consistent with adapter MSRV: $FORTRESS_WASM_MSRV_REQUIRED"
    exit 0
fi
