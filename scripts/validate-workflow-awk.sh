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
echo "Bash: ${BASH_VERSION:-unknown}"
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

    local scan_results
    scan_results=$(awk -f scripts/validate-workflow-awk-scanner.awk "$workflow")

    local match_lines=""
    local rust_fence_pattern_lines=""
    local nul_printf_found=false
    local complex_warnings=""
    local scan_kind scan_value scan_extra

    while IFS=$'\t' read -r scan_kind scan_value scan_extra; do
        case "$scan_kind" in
            MATCH_LINES)
                match_lines="$scan_value"
                ;;
            RUST_FENCE_PATTERN_LINES)
                rust_fence_pattern_lines="$scan_value"
                ;;
            NUL_PRINTF)
                nul_printf_found=true
                ;;
            COMPLEX)
                complex_warnings+="$scan_value"$'\t'"$scan_extra"$'\n'
                ;;
            "")
                ;;
        esac
    done <<< "$scan_results"

    if [ -n "$match_lines" ]; then
        echo -e "${RED}✗${NC} Uses match() capture array in AWK - not POSIX compatible"
        echo "  Lines: $match_lines"
        echo "  Fix: Use POSIX two-argument match(), sub(), or gsub() instead"
        echo ""
        ERRORS=$((ERRORS + 1))
        file_errors=$((file_errors + 1))
    fi

    if [ -n "$rust_fence_pattern_lines" ]; then
        echo -e "${RED}✗${NC} Rust fence AWK regex lacks an explicit language-token boundary"
        echo "  Lines: $rust_fence_pattern_lines"
        printf '%s\n' "  Problem: Bare prefixes like /^\`\`\`[Rr]ust/ also match \`\`\`rustic and \`\`\`rusty"
        echo '  Fix: Use /^```+[Rr]ust([[:space:],]|$)/ or the canonical .github/scripts/extract-rust-blocks.awk extractor'
        echo ""
        ERRORS=$((ERRORS + 1))
        file_errors=$((file_errors + 1))
    fi

    # Anti-pattern 2: \0 in printf (not POSIX compatible)
    if [ "$nul_printf_found" = true ]; then
        printf '%b⚠%b Uses \\0 in printf in AWK - not POSIX compatible\n' "$YELLOW" "$NC"
        echo "  Fix: Use printf \"%c\", 0 instead"
        echo ""
        WARNINGS=$((WARNINGS + 1))
    fi

    # Anti-pattern 3: Missing comments explaining complex AWK scripts
    if [ -n "$complex_warnings" ]; then
        local awk_start awk_line_count
        while IFS=$'\t' read -r awk_start awk_line_count; do
            [ -n "$awk_start" ] || continue
            echo -e "${YELLOW}⚠${NC} Complex AWK script (${awk_line_count} lines) lacks explanatory comments"
            echo "  Near line: $awk_start"
            echo "  Consider adding comments explaining what the AWK script does"
            echo ""
            WARNINGS=$((WARNINGS + 1))
        done <<< "$complex_warnings"
    fi

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
    echo "  - Replace match() capture arrays with POSIX two-argument match(), sub(), or gsub()"
    echo "  - Use token-boundary Rust fence matches instead of bare prefix or comma-only fence regexes"
    printf '%s\n' '  - Use printf "%c", 0 instead of printf "\0"'
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
