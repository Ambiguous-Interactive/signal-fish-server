#!/usr/bin/env bash
# Signal Fish Server - Dependency Advisory Checker
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Runs cargo-deny advisory checks to catch RUSTSEC advisories before pushing.
# This script mirrors the CI deny job locally so developers catch issues early.
#
# Usage:
#   ./scripts/check-advisories.sh           # Run advisory checks
#   ./scripts/check-advisories.sh --full    # Run all cargo-deny checks
#
# Exit codes:
#   0 = No advisories found
#   1 = Active advisories detected or tool error
#   2 = cargo-deny not installed (with install instructions)

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    BLUE=''
    NC=''
fi

# Move to repo root (consistent with other scripts in this directory)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

FULL_MODE=false

for arg in "$@"; do
    case $arg in
        --full)
            FULL_MODE=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--full]"
            echo ""
            echo "Options:"
            echo "  --full    Run all cargo-deny checks (advisories, licenses, bans, sources)"
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

# Verify cargo-deny is installed
if ! command -v cargo-deny >/dev/null 2>&1; then
    echo -e "${RED}[FAIL]${NC} cargo-deny is not installed."
    echo ""
    echo "Install it with one of:"
    echo "  cargo install cargo-deny"
    echo "  cargo binstall cargo-deny"
    echo ""
    echo "Or run the full CI check with:"
    echo "  docker run --rm -v \$(pwd):/src -w /src embarkstudios/cargo-deny check advisories"
    exit 2
fi

DENY_VERSION=$(cargo deny --version 2>/dev/null || echo "unknown")
echo -e "${BLUE}[INFO]${NC} cargo-deny version: $DENY_VERSION"

# Verify deny.toml exists
if [ ! -f deny.toml ]; then
    echo -e "${RED}[FAIL]${NC} deny.toml not found at repository root."
    echo "Create it with: cargo deny init"
    exit 1
fi

FAILED=0

# Run advisory check
echo -e "${BLUE}[INFO]${NC} Checking for RUSTSEC advisories..."
if cargo deny check advisories; then
    echo -e "${GREEN}[OK]${NC} No RUSTSEC advisories found."
else
    echo ""
    echo -e "${RED}[FAIL]${NC} RUSTSEC advisories detected in dependency tree."
    echo ""
    echo "To resolve:"
    echo "  1. Update the affected dependency to a patched version"
    echo "  2. Replace with a maintained alternative (e.g., rustls-pemfile -> rustls-pki-types)"
    echo "  3. If non-exploitable, add a documented ignore in deny.toml with expiry date:"
    echo ""
    echo "     [advisories]"
    echo "     ignore = ["
    echo "         # RUSTSEC-YYYY-NNNN: <justification>. Revisit by YYYY-MM-DD."
    echo "         \"RUSTSEC-YYYY-NNNN\","
    echo "     ]"
    FAILED=1
fi

# Optionally run all checks
if [ "$FULL_MODE" = true ]; then
    echo ""
    echo -e "${BLUE}[INFO]${NC} Running full cargo-deny checks (licenses, bans, sources)..."
    if cargo deny --all-features check; then
        echo -e "${GREEN}[OK]${NC} All cargo-deny checks passed."
    else
        echo -e "${RED}[FAIL]${NC} cargo-deny checks failed."
        FAILED=1
    fi
fi

exit $FAILED
