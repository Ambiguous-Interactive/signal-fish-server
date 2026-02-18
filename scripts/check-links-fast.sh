#!/usr/bin/env bash
# check-links-fast.sh - Fast link checking for modified files
#
# This script performs quick link validation on recently modified markdown files.
# It's designed for local development workflow - faster than full CI link checks.
#
# Usage:
#   ./scripts/check-links-fast.sh              # Check all modified markdown files
#   ./scripts/check-links-fast.sh --all        # Check all markdown files
#   ./scripts/check-links-fast.sh --staged     # Check only staged files
#   ./scripts/check-links-fast.sh FILE...      # Check specific files
#
# Exit codes:
#   0 - All links valid
#   1 - Broken links found
#   2 - lychee not installed

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check if lychee is installed
if ! command -v lychee &> /dev/null; then
    echo -e "${RED}ERROR: lychee is not installed${NC}"
    echo ""
    echo "Install with cargo:"
    echo "  cargo install lychee"
    echo ""
    echo "Or with homebrew (macOS):"
    echo "  brew install lychee"
    echo ""
    exit 2
fi

# Parse arguments
MODE="modified"
FILES=()

for arg in "$@"; do
    case "$arg" in
        --all)
            MODE="all"
            ;;
        --staged)
            MODE="staged"
            ;;
        --help|-h)
            echo "Usage: $0 [--all|--staged] [FILE...]"
            echo ""
            echo "Modes:"
            echo "  (default)  Check modified files (git status)"
            echo "  --all      Check all markdown files"
            echo "  --staged   Check only staged files (git diff --cached)"
            echo "  FILE...    Check specific files"
            echo ""
            echo "Examples:"
            echo "  $0                           # Check modified files"
            echo "  $0 --staged                  # Check staged files"
            echo "  $0 README.md docs/setup.md   # Check specific files"
            exit 0
            ;;
        *)
            FILES+=("$arg")
            ;;
    esac
done

echo "========================================="
echo "Fast Link Check"
echo "========================================="
echo ""

# Determine which files to check
if [ ${#FILES[@]} -gt 0 ]; then
    # Check specific files provided as arguments
    echo "Checking specified files: ${FILES[*]}"
    TO_CHECK=("${FILES[@]}")
elif [ "$MODE" = "all" ]; then
    echo "Checking all markdown files..."
    mapfile -t TO_CHECK < <(find . -type f -name "*.md" \
        -not -path "./target/*" \
        -not -path "./third_party/*" \
        -not -path "./.git/*" \
        -not -path "./node_modules/*")
elif [ "$MODE" = "staged" ]; then
    echo "Checking staged markdown files..."
    mapfile -t TO_CHECK < <(git diff --cached --name-only --diff-filter=ACM | grep -E '\.md$' || true)
else
    echo "Checking modified markdown files..."
    mapfile -t TO_CHECK < <(git status --porcelain | grep -E '\.md$' | awk '{print $2}' || true)
fi

# Check if there are any files to check
if [ ${#TO_CHECK[@]} -eq 0 ]; then
    echo -e "${YELLOW}No markdown files to check${NC}"
    exit 0
fi

echo "Files to check: ${#TO_CHECK[@]}"
echo ""

# Create temporary file for lychee input
TEMP_FILE=$(mktemp)
trap 'rm -f "$TEMP_FILE"' EXIT

printf '%s\n' "${TO_CHECK[@]}" > "$TEMP_FILE"

# Run lychee with configuration
# Use --offline flag to skip external link checks for speed (local links only)
# Remove --offline to check external links (slower but more thorough)
echo "Running lychee link checker..."
echo ""

# For fast local checks, use --offline to skip network requests
# This checks internal links and markdown structure only
if lychee --config .lychee.toml --offline --verbose --no-progress --base "$REPO_ROOT" "${TO_CHECK[@]}"; then
    echo ""
    echo -e "${GREEN}✓ All local links are valid${NC}"
    echo ""
    echo "Note: This was a fast check (--offline mode)."
    echo "To check external links, run: lychee --config .lychee.toml <file>"
    exit 0
else
    echo ""
    echo -e "${RED}✗ Link check failed${NC}"
    echo ""
    echo "Fix broken links and try again."
    echo "Common issues:"
    echo "  - Relative link points to non-existent file"
    echo "  - Anchor link to non-existent heading"
    echo "  - Typo in filename or path"
    echo ""
    echo "To check a specific file with full details:"
    echo "  lychee --config .lychee.toml --verbose <file>"
    exit 1
fi
