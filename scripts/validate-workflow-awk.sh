#!/usr/bin/env bash
# Signal Fish Server - Workflow AWK Script Validator
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Validates AWK scripts in GitHub Actions workflow files for common mistakes
# and anti-patterns. This prevents AWK-related CI failures.
#
# Note: This script checks for ANTI-PATTERNS, not full syntax validation.
# Full syntax validation of multi-line AWK scripts embedded in YAML is complex
# and better handled by the workflow itself during testing.
#
# Usage:
#   ./scripts/validate-workflow-awk.sh              # Check all workflows
#   ./scripts/validate-workflow-awk.sh ci.yml       # Check specific workflow
#
# Exit codes:
#   0 = No anti-patterns found
#   1 = Anti-patterns or issues found
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

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BLUE}Workflow AWK Anti-Pattern Checker${NC}"
echo "Repository: $REPO_ROOT"
echo ""

ERRORS=0
WARNINGS=0
CHECKED=0

# Check a workflow file for AWK anti-patterns
check_awk_antipatterns() {
    local workflow="$1"

    echo -e "${BLUE}Checking:${NC} $(basename "$workflow")"

    CHECKED=$((CHECKED + 1))
    local file_errors=0

    # Anti-pattern 1: GNU-specific match() function (not POSIX compatible)
    # Exclude comments (lines starting with # after whitespace)
    if grep -v '^\s*#' "$workflow" | grep -n 'match(' | grep -q 'awk'; then
        echo -e "${RED}✗${NC} Uses match() function in AWK - not POSIX compatible (mawk doesn't support it)"
        local match_lines
        match_lines=$(grep -v '^\s*#' "$workflow" | grep -n 'match(' | grep 'awk' | cut -d: -f1 | head -3)
        echo "  Lines: $match_lines"
        echo "  Fix: Use sub() or gsub() instead"
        echo ""
        ERRORS=$((ERRORS + 1))
        file_errors=$((file_errors + 1))
    fi

    # Anti-pattern 2: \0 in printf (not POSIX compatible)
    # Exclude comments (lines starting with # after whitespace)
    if grep -v '^\s*#' "$workflow" | grep -n 'printf.*\\0' | grep -q 'awk'; then
        echo -e "${YELLOW}⚠${NC} Uses \\0 in printf in AWK - not POSIX compatible"
        echo "  Fix: Use printf \"%c\", 0 instead"
        echo ""
        WARNINGS=$((WARNINGS + 1))
    fi

    # Anti-pattern 3: Overly strict regex with (,.*)?$ suffix
    # Example: /^```[Rr]ust(,.*)?$/ should be /^```[Rr]ust/
    if grep -nE '/\^\`\`\`[^/]+\(,\.\*\)\?\$/' "$workflow"; then
        echo -e "${YELLOW}⚠${NC} Uses strict regex with (,.*)?$ - might not match space-separated attributes"
        echo '  Example: /^```[Rr]ust(,.*)?$/ misses "rust ignore" (space-separated)'
        echo '  Fix: Use simpler prefix match: /^```[Rr]ust/'
        echo ""
        WARNINGS=$((WARNINGS + 1))
    fi

    # Anti-pattern 4: Missing comments explaining complex AWK scripts
    # Look for awk scripts longer than 10 lines without nearby comments
    local in_awk=false
    local awk_line_count=0
    local awk_start=0
    local has_comment=false

    while IFS= read -r line; do
        if echo "$line" | grep -qE "^\s*awk\s+['\"]"; then
            in_awk=true
            awk_start=$LINENO
            awk_line_count=0
            has_comment=false
            # Check previous 5 lines for explanatory comments
            if [ $LINENO -ge 5 ]; then
                if sed -n "$((LINENO-5)),$((LINENO-1))p" "$workflow" | grep -q '#.*AWK'; then
                    has_comment=true
                fi
            fi
        fi

        if [ "$in_awk" = true ]; then
            awk_line_count=$((awk_line_count + 1))
            # Detect end of AWK block (simplified heuristic)
            if echo "$line" | grep -qE "^\s*['\"]"; then
                if [ $awk_line_count -gt 15 ] && [ "$has_comment" = false ]; then
                    echo -e "${YELLOW}⚠${NC} Complex AWK script (${awk_line_count} lines) lacks explanatory comments"
                    echo "  Near line: $awk_start"
                    echo "  Consider adding comments explaining what the AWK script does"
                    echo ""
                    WARNINGS=$((WARNINGS + 1))
                fi
                in_awk=false
            fi
        fi
    done < "$workflow"

    if [ $file_errors -eq 0 ]; then
        echo -e "${GREEN}✓${NC} No critical anti-patterns found"
    fi

    echo ""
}

# Process workflow files
if [ $# -eq 0 ]; then
    # Check all workflows
    WORKFLOWS=(.github/workflows/*.yml .github/workflows/*.yaml)
else
    # Check specific workflow(s)
    WORKFLOWS=("$@")
fi

for workflow in "${WORKFLOWS[@]}"; do
    [ -f "$workflow" ] || continue
    check_awk_antipatterns "$workflow"
done

# Summary
echo "=========================================="
if [ "$ERRORS" -gt 0 ]; then
    echo -e "${RED}FOUND ISSUES:${NC} $ERRORS error(s) in $CHECKED workflow(s)"
    if [ "$WARNINGS" -gt 0 ]; then
        echo -e "${YELLOW}WARNINGS:${NC} $WARNINGS warning(s)"
    fi
    echo ""
    echo "Fix errors before committing. Warnings are recommendations."
    echo ""
    echo "Common fixes:"
    echo "  - Replace match() with sub() or gsub()"
    echo "  - Use prefix match /^pattern/ instead of /^pattern(,.*)?$/"
    echo "  - Use printf \"%c\", 0 instead of printf \"\\0\""
    echo ""
    exit 1
elif [ "$WARNINGS" -gt 0 ]; then
    echo -e "${YELLOW}FOUND WARNINGS:${NC} $WARNINGS warning(s) in $CHECKED workflow(s)"
    echo ""
    echo "Warnings are recommendations for better portability and maintainability."
    exit 0
else
    if [ "$CHECKED" -eq 0 ]; then
        echo -e "${BLUE}INFO:${NC} No workflow files found"
    else
        echo -e "${GREEN}SUCCESS:${NC} No AWK anti-patterns found in $CHECKED workflow(s)"
    fi
    exit 0
fi
