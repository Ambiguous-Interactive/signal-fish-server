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
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        while IFS= read -r -d '' markdown_file; do
            case "$markdown_file" in
                target/*|third_party/*|node_modules/*|.github/test-fixtures/*|test-fixtures/*)
                    continue
                    ;;
            esac
            TO_CHECK+=("$markdown_file")
        done < <(git ls-files -z -- '*.md')
    else
        while IFS= read -r -d '' markdown_file; do
            TO_CHECK+=("$markdown_file")
        done < <(find . -type f -name "*.md" \
            -not -path "./target/*" \
            -not -path "./third_party/*" \
            -not -path "./.git/*" \
            -not -path "./node_modules/*" \
            -not -path "./.github/test-fixtures/*" \
            -not -path "./test-fixtures/*" \
            -print0)
    fi
elif [ "$MODE" = "staged" ]; then
    echo "Checking staged markdown files..."
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo -e "${RED}ERROR: --staged requires a Git worktree${NC}"
        exit 2
    fi
    while IFS= read -r -d '' markdown_file; do
        TO_CHECK+=("$markdown_file")
    done < <(git diff --cached --name-only -z --diff-filter=ACM -- '*.md')
else
    echo "Checking modified markdown files..."
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        while IFS= read -r -d '' markdown_file; do
            TO_CHECK+=("$markdown_file")
        done < <(find . -type f -name "*.md" \
            -not -path "./target/*" \
            -not -path "./third_party/*" \
            -not -path "./.git/*" \
            -not -path "./node_modules/*" \
            -not -path "./.github/test-fixtures/*" \
            -not -path "./test-fixtures/*" \
            -print0)
    else
        STATUS_ENTRIES=()
        mapfile -d '' STATUS_ENTRIES < <(git status --porcelain=v1 -z -- '*.md')
        index=0
        while [ "$index" -lt "${#STATUS_ENTRIES[@]}" ]; do
            entry="${STATUS_ENTRIES[$index]}"
            index=$((index + 1))

            [ "${#entry}" -ge 4 ] || continue
            status="${entry:0:2}"
            markdown_file="${entry:3}"

            case "$markdown_file" in
                target/*|third_party/*|node_modules/*|.github/test-fixtures/*|test-fixtures/*)
                    ;;
                *)
                    if [ -f "$markdown_file" ]; then
                        TO_CHECK+=("$markdown_file")
                    fi
                    ;;
            esac

            # In porcelain v1 -z output, rename/copy entries are followed by
            # the original path as a second NUL-delimited record.
            if [[ "$status" == R* || "$status" == C* || "$status" == ?R || "$status" == ?C ]]; then
                index=$((index + 1))
            fi
        done
    fi
fi

# Check if there are any files to check
if [ ${#TO_CHECK[@]} -eq 0 ]; then
    echo -e "${YELLOW}No markdown files to check${NC}"
    exit 0
fi

echo "Files to check: ${#TO_CHECK[@]}"
echo ""

echo "Checking internal links and tracked targets..."
if ! ./scripts/check-internal-links.sh --quiet "${TO_CHECK[@]}"; then
    echo ""
    echo -e "${RED}✗ Internal link check failed${NC}"
    echo ""
    echo "Fix broken links and try again."
    echo "Common issues:"
    echo "  - Relative link points to a non-existent file"
    echo "  - Link target exists locally but is not tracked by git"
    echo "  - Typo in filename or path"
    exit 1
fi
echo -e "${GREEN}✓ Internal links are valid${NC}"
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
