#!/usr/bin/env bash
# check-markdown.sh - Validate markdown files with markdownlint
#
# This script runs markdownlint-cli2 on all markdown files in the repository,
# catching common issues like missing language identifiers on code blocks,
# table alignment problems, and inconsistent formatting.
#
# Usage:
#   ./scripts/check-markdown.sh         # Check all markdown files
#   ./scripts/check-markdown.sh fix     # Auto-fix issues where possible
#
# Exit codes:
#   0 - All markdown files pass linting
#   1 - Linting errors found
#   2 - markdownlint-cli2 not installed

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

# Check if markdownlint-cli2 is installed
if ! command -v markdownlint-cli2 &> /dev/null; then
    echo -e "${RED}ERROR: markdownlint-cli2 is not installed${NC}"
    echo ""
    echo "Install with npm (globally):"
    echo "  npm install -g markdownlint-cli2"
    echo ""
    echo "Or install with npm (locally):"
    echo "  npm install --save-dev markdownlint-cli2"
    echo "  npx markdownlint-cli2 '**/*.md'"
    echo ""
    echo "Or use Docker:"
    echo "  docker run --rm -v \"\$PWD:/work\" davidanson/markdownlint-cli2:latest '**/*.md'"
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
echo ""

# Run markdownlint-cli2
if [ "$FIX_MODE" = true ]; then
    echo "Running markdownlint-cli2 with auto-fix..."
    if markdownlint-cli2 --fix '**/*.md' '#target/**' '#third_party/**' '#node_modules/**'; then
        echo -e "${GREEN}All markdown files are valid (after fixes)${NC}"
        exit 0
    else
        echo -e "${RED}Some markdown issues could not be auto-fixed${NC}"
        exit 1
    fi
else
    echo "Running markdownlint-cli2..."
    if markdownlint-cli2 '**/*.md' '#target/**' '#third_party/**' '#node_modules/**'; then
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
        echo "  - MD060: Table alignment issues"
        echo "    Fix: Use consistent spacing in table columns"
        echo ""
        exit 1
    fi
fi
