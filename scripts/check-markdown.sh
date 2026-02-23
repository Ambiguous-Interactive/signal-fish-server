#!/usr/bin/env bash
# check-markdown.sh - Validate markdown files with markdownlint
#
# This script runs markdownlint-cli2 on all markdown files in the repository,
# catching common issues like missing language identifiers on code blocks,
# table alignment problems, and inconsistent formatting.
#
# Security note:
#   This script intentionally avoids npx auto-download and Docker "latest"
#   fallbacks. It requires a pinned markdownlint-cli2 version from
#   .markdownlint-version for reproducible and safer local execution.
#
# Usage:
#   ./scripts/check-markdown.sh         # Check all markdown files
#   ./scripts/check-markdown.sh fix     # Auto-fix issues where possible
#
# Exit codes:
#   0 - All markdown files pass linting
#   1 - Linting errors found
#   2 - markdownlint-cli2 missing or wrong version

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

MARKDOWNLINT_MODE=""
MARKDOWNLINT_CMD=()
MARKDOWNLINT_VERSION_FILE="$REPO_ROOT/.markdownlint-version"

if [ ! -f "$MARKDOWNLINT_VERSION_FILE" ]; then
    echo -e "${RED}ERROR: Missing $MARKDOWNLINT_VERSION_FILE${NC}"
    echo ""
    echo "Expected a pinned markdownlint-cli2 version file for reproducible linting."
    exit 2
fi

REQUIRED_MARKDOWNLINT_VERSION="$(tr -d '[:space:]' < "$MARKDOWNLINT_VERSION_FILE")"

if [ -x "$REPO_ROOT/node_modules/.bin/markdownlint-cli2" ]; then
    MARKDOWNLINT_MODE="node_modules/.bin/markdownlint-cli2 (pinned)"
    MARKDOWNLINT_CMD=("$REPO_ROOT/node_modules/.bin/markdownlint-cli2")
elif command -v markdownlint-cli2 >/dev/null 2>&1; then
    MARKDOWNLINT_MODE="markdownlint-cli2 (global)"
    MARKDOWNLINT_CMD=(markdownlint-cli2)
else
    echo -e "${RED}ERROR: markdownlint-cli2 is unavailable${NC}"
    echo ""
    echo "Install the pinned version:"
    echo "  npm install -g markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"
    echo ""
    exit 2
fi

MARKDOWNLINT_VERSION_RAW="$("${MARKDOWNLINT_CMD[@]}" --version 2>/dev/null || true)"
if [[ "$MARKDOWNLINT_VERSION_RAW" =~ ([0-9]+\.[0-9]+\.[0-9]+) ]]; then
    INSTALLED_MARKDOWNLINT_VERSION="${BASH_REMATCH[1]}"
else
    echo -e "${RED}ERROR: Unable to determine markdownlint-cli2 version${NC}"
    echo "Command output: ${MARKDOWNLINT_VERSION_RAW:-<empty>}"
    exit 2
fi

if [ "$INSTALLED_MARKDOWNLINT_VERSION" != "$REQUIRED_MARKDOWNLINT_VERSION" ]; then
    echo -e "${RED}ERROR: markdownlint-cli2 version mismatch${NC}"
    echo ""
    echo "Required: ${REQUIRED_MARKDOWNLINT_VERSION}"
    echo "Detected: ${INSTALLED_MARKDOWNLINT_VERSION}"
    echo ""
    echo "Install the pinned version:"
    echo "  npm install -g markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"
    echo ""
    exit 2
fi

# Parse arguments
FIX_MODE=false
if [ "${1:-}" = "fix" ]; then
    FIX_MODE=true
fi

echo "=========================================="
echo "Markdown Linting Check"
echo "=========================================="
echo "Runner: $MARKDOWNLINT_MODE"
echo "Version: $INSTALLED_MARKDOWNLINT_VERSION (required: $REQUIRED_MARKDOWNLINT_VERSION)"
echo ""

MARKDOWNLINT_GLOBS=(
    '**/*.md'
    '#target/**'
    '#third_party/**'
    '#node_modules/**'
    '#.github/test-fixtures/**'
    '#test-fixtures/**'
)

# Run markdownlint-cli2
if [ "$FIX_MODE" = true ]; then
    echo "Running markdownlint-cli2 with auto-fix..."
    if "${MARKDOWNLINT_CMD[@]}" --fix "${MARKDOWNLINT_GLOBS[@]}"; then
        echo -e "${GREEN}All markdown files are valid (after fixes)${NC}"
        exit 0
    else
        echo -e "${RED}Some markdown issues could not be auto-fixed${NC}"
        exit 1
    fi
else
    echo "Running markdownlint-cli2..."
    if "${MARKDOWNLINT_CMD[@]}" "${MARKDOWNLINT_GLOBS[@]}"; then
        echo -e "${GREEN}All markdown files are valid${NC}"
        exit 0
    else
        echo ""
        echo -e "${RED}Markdown linting failed${NC}"
        echo ""
        echo "To auto-fix issues:"
        echo "  ./scripts/check-markdown.sh fix"
        echo ""
        echo "Common issues:"
        echo "  - MD040: Missing language identifier on code blocks"
        echo "    Fix: Add language identifier after opening backticks (e.g., \`\`\`bash)"
        echo "  - MD046: Inconsistent code block style"
        echo "    Fix: Use fenced code blocks (\`\`\`) consistently"
        echo ""
        exit 1
    fi
fi
