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
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

MARKDOWNLINT_MODE=""
MARKDOWNLINT_CMD=()
MARKDOWNLINT_VERSION_FILE="$REPO_ROOT/.markdownlint-version"
MARKDOWNLINT_AUTO_INSTALL="${MARKDOWNLINT_AUTO_INSTALL:-1}"
MARKDOWNLINT_BOOTSTRAP_PREFIX="${MARKDOWNLINT_BOOTSTRAP_PREFIX:-${XDG_CACHE_HOME:-$HOME/.cache}/signal-fish-server/npm-global}"
MARKDOWNLINT_BOOTSTRAP_BIN="$MARKDOWNLINT_BOOTSTRAP_PREFIX/bin/markdownlint-cli2"

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

detect_markdownlint_runner() {
    MARKDOWNLINT_MODE=""
    MARKDOWNLINT_CMD=()

    if [ -x "$REPO_ROOT/node_modules/.bin/markdownlint-cli2" ]; then
        MARKDOWNLINT_MODE="node_modules/.bin/markdownlint-cli2 (pinned)"
        MARKDOWNLINT_CMD=("$REPO_ROOT/node_modules/.bin/markdownlint-cli2")
    elif [ -x "$MARKDOWNLINT_BOOTSTRAP_BIN" ]; then
        MARKDOWNLINT_MODE="cache npm-global markdownlint-cli2 (pinned)"
        MARKDOWNLINT_CMD=("$MARKDOWNLINT_BOOTSTRAP_BIN")
    elif command -v markdownlint-cli2 >/dev/null 2>&1; then
        MARKDOWNLINT_MODE="markdownlint-cli2 (global)"
        MARKDOWNLINT_CMD=(markdownlint-cli2)
    else
        return 1
    fi
}

detect_markdownlint_version() {
    MARKDOWNLINT_VERSION_RAW="$("${MARKDOWNLINT_CMD[@]}" --version 2>/dev/null || true)"
    if [[ "$MARKDOWNLINT_VERSION_RAW" =~ ([0-9]+\.[0-9]+\.[0-9]+) ]]; then
        INSTALLED_MARKDOWNLINT_VERSION="${BASH_REMATCH[1]}"
        return 0
    fi
    return 1
}

try_auto_install_markdownlint() {
    if ! command -v npm >/dev/null 2>&1; then
        echo -e "${YELLOW}Auto-install skipped:${NC} npm is not available on PATH."
        return 1
    fi

    mkdir -p "$MARKDOWNLINT_BOOTSTRAP_PREFIX"
    echo "Attempting pinned markdownlint-cli2 auto-install to:"
    echo "  $MARKDOWNLINT_BOOTSTRAP_PREFIX"

    INSTALL_OUT=$(
        NPM_CONFIG_PREFIX="$MARKDOWNLINT_BOOTSTRAP_PREFIX" \
            npm install -g --save-exact --no-audit --no-fund \
            "markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}" 2>&1
    ) || {
        echo -e "${YELLOW}Auto-install failed.${NC}"
        echo "$INSTALL_OUT"
        return 1
    }

    if [ -x "$MARKDOWNLINT_BOOTSTRAP_BIN" ]; then
        return 0
    fi

    echo -e "${YELLOW}Auto-install completed but markdownlint-cli2 binary was not found.${NC}"
    return 1
}

print_markdownlint_install_help() {
    echo "Install the pinned version (choose one):"
    echo "  npm install --save-dev --save-exact markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"
    echo "  npm install -g markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"
    echo "  NPM_CONFIG_PREFIX=\"$MARKDOWNLINT_BOOTSTRAP_PREFIX\" npm install -g --save-exact markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"
    echo ""
    echo "If installed locally, this script uses:"
    echo "  $REPO_ROOT/node_modules/.bin/markdownlint-cli2"
    echo "Or cached auto-install path:"
    echo "  $MARKDOWNLINT_BOOTSTRAP_BIN"
}

if [ ! -f "$MARKDOWNLINT_VERSION_FILE" ]; then
    echo -e "${RED}ERROR: Missing $MARKDOWNLINT_VERSION_FILE${NC}"
    echo ""
    echo "Expected a pinned markdownlint-cli2 version file for reproducible linting."
    exit 2
fi

REQUIRED_MARKDOWNLINT_VERSION="$(tr -d '[:space:]' < "$MARKDOWNLINT_VERSION_FILE")"

if ! detect_markdownlint_runner; then
    if is_truthy "$MARKDOWNLINT_AUTO_INSTALL" && try_auto_install_markdownlint; then
        detect_markdownlint_runner || true
    fi
fi

if [ -z "$MARKDOWNLINT_MODE" ]; then
    echo -e "${RED}ERROR: markdownlint-cli2 is unavailable${NC}"
    echo ""
    echo "Searched for:"
    echo "  - $REPO_ROOT/node_modules/.bin/markdownlint-cli2"
    echo "  - $MARKDOWNLINT_BOOTSTRAP_BIN"
    echo "  - markdownlint-cli2 on PATH"
    echo ""
    if ! is_truthy "$MARKDOWNLINT_AUTO_INSTALL"; then
        echo "Auto-install disabled: set MARKDOWNLINT_AUTO_INSTALL=1 to allow bootstrap."
        echo ""
    fi
    print_markdownlint_install_help
    echo ""
    exit 2
fi

if ! detect_markdownlint_version; then
    echo -e "${RED}ERROR: Unable to determine markdownlint-cli2 version${NC}"
    echo "Detected runner mode: ${MARKDOWNLINT_MODE:-unknown}"
    echo "Command output: ${MARKDOWNLINT_VERSION_RAW:-<empty>}"
    echo ""
    print_markdownlint_install_help
    exit 2
fi

if [ "$INSTALLED_MARKDOWNLINT_VERSION" != "$REQUIRED_MARKDOWNLINT_VERSION" ]; then
    if is_truthy "$MARKDOWNLINT_AUTO_INSTALL" && try_auto_install_markdownlint; then
        detect_markdownlint_runner || true
        detect_markdownlint_version || true
    fi
fi

if [ "${INSTALLED_MARKDOWNLINT_VERSION:-}" != "$REQUIRED_MARKDOWNLINT_VERSION" ]; then
    echo -e "${RED}ERROR: markdownlint-cli2 version mismatch${NC}"
    echo ""
    echo "Required: ${REQUIRED_MARKDOWNLINT_VERSION}"
    echo "Detected: ${INSTALLED_MARKDOWNLINT_VERSION:-unknown}"
    echo "Detected runner mode: ${MARKDOWNLINT_MODE}"
    echo ""
    print_markdownlint_install_help
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
    MARKDOWNLINT_INPUTS=(
        '**/*.md'
        '#target/**'
        '#third_party/**'
        '#node_modules/**'
        '#.github/test-fixtures/**'
        '#test-fixtures/**'
    )
fi

if [ "${#MARKDOWNLINT_INPUTS[@]}" -eq 0 ]; then
    echo -e "${YELLOW}No markdown files found to lint${NC}"
    exit 0
fi

# Run markdownlint-cli2
if [ "$FIX_MODE" = true ]; then
    echo "Running markdownlint-cli2 with auto-fix..."
    if "${MARKDOWNLINT_CMD[@]}" --fix "${MARKDOWNLINT_INPUTS[@]}"; then
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
    echo "Running markdownlint-cli2..."
    MARKDOWNLINT_LOG="$(mktemp)"
    trap 'rm -f "$MARKDOWNLINT_LOG"' EXIT

    if "${MARKDOWNLINT_CMD[@]}" "${MARKDOWNLINT_INPUTS[@]}" 2>&1 | tee "$MARKDOWNLINT_LOG"; then
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
