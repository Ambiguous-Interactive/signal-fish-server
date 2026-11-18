#!/usr/bin/env bash
# check-markdown.sh - Validate markdown files with markdownlint
#
# This script runs markdownlint on all markdown files in the repository,
# catching common issues like missing language identifiers on code blocks,
# table alignment problems, and inconsistent formatting.
#
# Security note:
#   This script intentionally avoids npx auto-download and Docker "latest"
#   fallbacks. It requires a pinned markdownlint version from
#   package.json for reproducible and safer local execution.
#
# Usage:
#   ./scripts/check-markdown.sh         # Check all markdown files
#   ./scripts/check-markdown.sh fix     # Auto-fix issues where possible
#
# Exit codes:
#   0 - All markdown files pass linting
#   1 - Linting errors found
#   2 - markdownlint missing or wrong version

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

MARKDOWNLINT_MODE=""
MARKDOWNLINT_AUTO_INSTALL="${MARKDOWNLINT_AUTO_INSTALL:-1}"
MARKDOWNLINT_BOOTSTRAP_PREFIX="${MARKDOWNLINT_BOOTSTRAP_PREFIX:-${XDG_CACHE_HOME:-$HOME/.cache}/signal-fish-server}"
MARKDOWNLINT_TEMP=""
MARKDOWNLINT_LOG=""

# Called by the EXIT trap.
# shellcheck disable=SC2317
cleanup() {
    if [ -n "$MARKDOWNLINT_TEMP" ]; then
        rm -rf -- "$MARKDOWNLINT_TEMP"
    fi
    if [ -n "$MARKDOWNLINT_LOG" ]; then
        rm -f -- "$MARKDOWNLINT_LOG"
    fi
}
trap cleanup EXIT

is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

if ! REQUIRED_MARKDOWNLINT_VERSION="$(node -p 'require("./package.json").devDependencies["markdownlint"]')" ||
    [[ ! "$REQUIRED_MARKDOWNLINT_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo -e "${RED}ERROR: Use Node.js and pin markdownlint to an exact version in package.json${NC}"
    exit 2
fi

use_markdownlint() {
    local package_root="$1"
    local mode="$2"

    if [ ! -r "$package_root/package.json" ]; then
        return 1
    fi
    MARKDOWNLINT_MODE="$mode"
    INSTALLED_MARKDOWNLINT_VERSION="$(node -p 'require(process.argv[1]).version' "$package_root/package.json")" || return 1
    [ "$INSTALLED_MARKDOWNLINT_VERSION" = "$REQUIRED_MARKDOWNLINT_VERSION" ] || return 1
    MARKDOWNLINT_CMD=(node "$REPO_ROOT/scripts/check-markdown-files.mjs" "$package_root")
}

find_global_markdownlint() {
    local global_root
    global_root="$(npm root --global 2>/dev/null)" && use_markdownlint "$global_root/markdownlint" "global npm package"
}

bootstrap_markdownlint() {
    is_truthy "$MARKDOWNLINT_AUTO_INSTALL" && command -v npm >/dev/null 2>&1 || return 1
    mkdir -p "$MARKDOWNLINT_BOOTSTRAP_PREFIX" || return 1
    local prefix
    prefix="$(cd "$MARKDOWNLINT_BOOTSTRAP_PREFIX" && pwd)" || return 1
    MARKDOWNLINT_TEMP="$(mktemp -d "$prefix/markdownlint.XXXXXXXX")" || return 1
    cp "$REPO_ROOT/package.json" "$REPO_ROOT/package-lock.json" "$MARKDOWNLINT_TEMP/" || return 1
    npm ci --prefix "$MARKDOWNLINT_TEMP" --ignore-scripts --no-audit --no-fund || return 1
    use_markdownlint "$MARKDOWNLINT_TEMP/node_modules/markdownlint" "locked temporary install"
}

if ! use_markdownlint "$REPO_ROOT/node_modules/markdownlint" "local npm package" &&
    ! find_global_markdownlint &&
    ! bootstrap_markdownlint; then
    echo -e "${RED}ERROR: markdownlint is unavailable or its version does not match${NC}"
    echo "Required: ${REQUIRED_MARKDOWNLINT_VERSION}"
    echo "Detected: ${INSTALLED_MARKDOWNLINT_VERSION:-unknown}"
    echo "Detected runner mode: ${MARKDOWNLINT_MODE:-none}"
    echo "Install the pinned version (choose one):"
    echo "  npm ci"
    echo "  npm install -g markdownlint@${REQUIRED_MARKDOWNLINT_VERSION}"
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

MARKDOWNLINT_INPUTS=()
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    while IFS= read -r -d '' markdown_file; do
        case "$markdown_file" in
            target/*|third_party/*|node_modules/*|.github/test-fixtures/*|test-fixtures/*)
                continue
                ;;
        esac
        MARKDOWNLINT_INPUTS+=("$markdown_file")
    done < <(git ls-files -z -- '*.md')
else
    MARKDOWNLINT_INPUTS=(--discover)
fi

if [ "${#MARKDOWNLINT_INPUTS[@]}" -eq 0 ]; then
    echo -e "${YELLOW}No markdown files found to lint${NC}"
    exit 0
fi

# Run markdownlint
if [ "$FIX_MODE" = true ]; then
    echo "Running markdownlint with auto-fix..."
    if "${MARKDOWNLINT_CMD[@]}" fix "${MARKDOWNLINT_INPUTS[@]}"; then
        if [ -x ./scripts/check-markdown-link-text.sh ]; then
            echo "Running markdown link text auto-fix..."
            ./scripts/check-markdown-link-text.sh --fix
        fi
        echo -e "${GREEN}All markdown files are valid (after fixes)${NC}"
        exit 0
    else
        echo -e "${RED}Some markdown issues could not be auto-fixed${NC}"
        exit 1
    fi
else
    echo "Running markdownlint..."
    MARKDOWNLINT_LOG="$(mktemp)"

    if "${MARKDOWNLINT_CMD[@]}" check "${MARKDOWNLINT_INPUTS[@]}" 2>&1 | tee "$MARKDOWNLINT_LOG"; then
        if [ -x ./scripts/check-markdown-link-text.sh ]; then
            echo "Running markdown link text policy checks..."
            ./scripts/check-markdown-link-text.sh
        fi
        echo -e "${GREEN}All markdown files are valid${NC}"
        exit 0
    else
        echo ""
        echo -e "${RED}Markdown linting failed${NC}"
        echo ""

        if grep -q "MD013/line-length" "$MARKDOWNLINT_LOG"; then
            echo -e "${YELLOW}MD013 diagnostics (source lines):${NC}"
            while IFS= read -r violation; do
                if [[ "$violation" =~ ^([^:]+):([0-9]+): ]]; then
                    file_path="${BASH_REMATCH[1]}"
                    line_number="${BASH_REMATCH[2]}"
                    source_line="$(sed -n "${line_number}p" "$file_path" 2>/dev/null || true)"
                    printf "  - %s:%s\n" "$file_path" "$line_number"
                    if [ -n "$source_line" ]; then
                        printf "    %s\n" "$source_line"
                    fi
                fi
            done < <(grep "MD013/line-length" "$MARKDOWNLINT_LOG")
            echo ""
        fi

        echo "To auto-fix issues:"
        echo "  ./scripts/check-markdown.sh fix"
        echo ""
        echo "Common issues:"
        echo "  - MD013: Line length exceeds configured limit"
        echo "    Fix: Reflow paragraphs or split long list items/links across lines"
        echo "  - MD040: Missing language identifier on code blocks"
        echo "    Fix: Add language identifier after opening backticks (e.g., \`\`\`bash)"
        echo "  - MD046: Inconsistent code block style"
        echo "    Fix: Use fenced code blocks (\`\`\`) consistently"
        echo ""
        exit 1
    fi
fi
