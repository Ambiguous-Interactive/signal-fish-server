#!/usr/bin/env bash
# Signal Fish Server - Outdated Dependency Checker
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Reports outdated dependencies (compatible and incompatible updates).
# This is an informational tool — outdated dependencies are NOT errors.
#
# Usage:
#   ./scripts/check-outdated.sh                # Show outdated dependencies
#   ./scripts/check-outdated.sh --root-only    # Skip transitive dependencies
#   ./scripts/check-outdated.sh --json         # JSON output
#
# Exit codes:
#   0 = Report completed (always exits 0 — informational only)
#   2 = cargo-outdated not installed (with install instructions)

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

# Move to repo root (consistent with other scripts in this directory)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

ROOT_ONLY=false
JSON_OUTPUT=false

for arg in "$@"; do
    case $arg in
        --root-only)
            ROOT_ONLY=true
            ;;
        --json)
            JSON_OUTPUT=true
            ;;
        --help|-h)
            echo "Usage: $0 [--root-only] [--json]"
            echo ""
            echo "Options:"
            echo "  --root-only  Only show direct dependencies (skip transitive)"
            echo "  --json       Output in JSON format"
            echo "  --help       Show this help message"
            echo ""
            echo "This script is informational only — it always exits 0."
            echo "Outdated dependencies are reported but not treated as errors."
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            echo "Use --help for usage information"
            exit 2
            ;;
    esac
done

# Verify cargo-outdated is installed
if ! command -v cargo-outdated >/dev/null 2>&1; then
    echo -e "${RED}[FAIL]${NC} cargo-outdated is not installed."
    echo ""
    echo "Install it with one of:"
    echo "  cargo install cargo-outdated"
    echo "  cargo binstall cargo-outdated"
    exit 2
fi

OUTDATED_VERSION=$(cargo outdated --version 2>/dev/null || echo "unknown")
echo -e "${BLUE}[INFO]${NC} cargo-outdated version: $OUTDATED_VERSION"

# Build the cargo outdated command arguments
OUTDATED_ARGS=()

if [ "$ROOT_ONLY" = true ]; then
    OUTDATED_ARGS+=(--root-deps-only)
    echo -e "${BLUE}[INFO]${NC} Showing root (direct) dependencies only."
else
    echo -e "${BLUE}[INFO]${NC} Showing all dependencies (including transitive)."
fi

echo ""

if [ "$JSON_OUTPUT" = true ]; then
    echo -e "${BLUE}[INFO]${NC} Generating JSON report..."
    echo ""
    # Run cargo outdated in JSON format; capture output to still provide summary
    if [ ${#OUTDATED_ARGS[@]} -eq 0 ]; then
        OUTPUT=$(cargo outdated --format json) || true
    else
        OUTPUT=$(cargo outdated "${OUTDATED_ARGS[@]}" --format json) || true
    fi
    echo "$OUTPUT"
else
    echo -e "${BLUE}[INFO]${NC} Checking for outdated dependencies..."
    echo ""

    # Capture output so we can count and summarize
    if [ ${#OUTDATED_ARGS[@]} -eq 0 ]; then
        OUTPUT=$(cargo outdated) || true
    else
        OUTPUT=$(cargo outdated "${OUTDATED_ARGS[@]}") || true
    fi
    echo "$OUTPUT"

    echo ""
    echo "=========================================="
    echo -e "${BLUE}[INFO]${NC} Summary"
    echo "=========================================="

    # Check if cargo outdated reported any outdated dependencies by looking
    # for dependency lines (non-empty lines after stripping the header/separator).
    # We check whether any output lines start with a non-whitespace character
    # beyond the expected header line, rather than counting lines (which is
    # fragile if cargo-outdated's output format changes).
    HAS_OUTDATED=false
    HEADER_SEEN=false
    while IFS= read -r line; do
        # Skip empty lines and separator lines (dashes possibly separated by spaces)
        [[ -z "$line" || "$line" =~ ^[-[:space:]]+$ ]] && continue
        # First non-empty non-separator line is the header
        if [ "$HEADER_SEEN" = false ]; then
            HEADER_SEEN=true
            continue
        fi
        # Any subsequent non-empty line means there are outdated deps
        HAS_OUTDATED=true
        break
    done <<< "$OUTPUT"

    if [ "$HAS_OUTDATED" = true ]; then
        echo -e "${YELLOW}[INFO]${NC} Found outdated dependencies (see report above)."
    else
        echo -e "${GREEN}[OK]${NC} All dependencies are up to date!"
    fi

    echo ""
    echo "Actionable advice:"
    echo "  - Compatible updates (within semver range): run 'cargo update'"
    echo "  - Incompatible updates (require Cargo.toml change): edit version in Cargo.toml, then 'cargo update'"
    echo "  - Review changelogs before major version bumps"
    echo ""
    echo -e "${BLUE}[INFO]${NC} This report is informational only — no action is required."
fi

exit 0
