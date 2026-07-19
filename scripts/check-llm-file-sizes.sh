#!/usr/bin/env bash
# LLM File Size Enforcer
#
# Validates that all files in the .llm/ directory are within the maximum
# allowed line count. Large files must be split into focused sub-files.
#
# The 300-line limit ensures each skill file stays focused, scannable, and
# easy for LLM agents to load without exceeding context windows.
#
# Usage:
#   ./scripts/check-llm-file-sizes.sh
#   ./scripts/check-llm-file-sizes.sh --files .llm/skills/foo.md .llm/context.md
#
# Exit codes:
#   0 = All files within the 300-line limit
#   1 = One or more files exceed the limit
#   2 = Invalid usage

set -euo pipefail

MAX_LINES=300
WARN_HEADROOM=5
LLM_DIR=".llm"

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

ERRORS=0
WARNINGS=0
FILE_ARGS_MODE=0
TOP_DIAGNOSTIC_FILES=8
declare -a FILE_SIZE_RECORDS=()

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
    WARNINGS=$((WARNINGS + 1))
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

print_size_diagnostics() {
    if [ "${#FILE_SIZE_RECORDS[@]}" -eq 0 ]; then
        return
    fi

    echo ""
    echo "Largest checked .llm files:"
    printf '%s\n' "${FILE_SIZE_RECORDS[@]}" \
        | LC_ALL=C sort -rn -k1,1 \
        | awk -F '\t' -v max="$MAX_LINES" -v limit="$TOP_DIAGNOSTIC_FILES" '
            NR > limit {
                next
            }
            {
                headroom = max - $1
                if (headroom >= 0) {
                    status = headroom " lines remaining"
                } else {
                    status = "exceeds by " (-headroom)
                }
                printf "  - %s: %s lines (%s)\n", $2, $1, status
            }
        '
}

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BLUE}LLM File Size Checker${NC}"
echo "Repository: $REPO_ROOT"
echo "Max lines per file: $MAX_LINES"
echo ""

# ---------------------------------------------------------------------------
# Determine which files to check
# ---------------------------------------------------------------------------
declare -a FILES_TO_CHECK=()

if [ "${1:-}" = "--files" ]; then
    FILE_ARGS_MODE=1
    shift
    if [ "$#" -eq 0 ]; then
        echo -e "${RED}[ERROR]${NC} --files requires at least one file argument" >&2
        exit 2
    fi
    for file in "$@"; do
        FILES_TO_CHECK+=("$file")
    done
else
    if [ ! -d "$LLM_DIR" ]; then
        echo -e "${YELLOW}[WARN]${NC} Directory '$LLM_DIR' not found — skipping check"
        exit 0
    fi
    while IFS= read -r file; do
        FILES_TO_CHECK+=("$file")
    done < <(find "$LLM_DIR" -type f -name "*.md" | LC_ALL=C sort)
fi

# ---------------------------------------------------------------------------
# Check each file
# ---------------------------------------------------------------------------
if [ "$FILE_ARGS_MODE" -eq 1 ]; then
    info "Scanning ${#FILES_TO_CHECK[@]} explicitly provided file(s) for size violations..."
else
    info "Scanning files in $LLM_DIR/ for size violations..."
fi
echo ""

CHECKED=0
VIOLATIONS=0
MISSING_INPUTS=0

for file in "${FILES_TO_CHECK[@]}"; do
    if [ ! -f "$file" ]; then
        warn "Skipping non-existent file: $file"
        MISSING_INPUTS=$((MISSING_INPUTS + 1))
        continue
    fi

    # Skip the auto-generated skills index; its size is controlled by the
    # number of skills, not by manual editing.
    case "$file" in
        .llm/skills/index.md|./.llm/skills/index.md) continue ;;
    esac

    LINE_COUNT=$(awk 'END {print NR}' "$file")
    CHECKED=$((CHECKED + 1))
    FILE_SIZE_RECORDS+=("${LINE_COUNT}"$'\t'"${file}")

    if [ "$LINE_COUNT" -gt "$MAX_LINES" ]; then
        error "$file: $LINE_COUNT lines (max: $MAX_LINES — exceeds by $((LINE_COUNT - MAX_LINES)))"
        if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
            echo "::error file=$file::LLM file exceeds ${MAX_LINES}-line limit ($LINE_COUNT lines)"
        fi
        echo "       Split into focused sub-files of ≤${MAX_LINES} lines."
        echo "       See .llm/skills/manage-skills/SKILL.md for guidance."
        VIOLATIONS=$((VIOLATIONS + 1))
    else
        # Report files in the documented warning zone as informational.
        HEADROOM=$((MAX_LINES - LINE_COUNT))
        if [ "$HEADROOM" -eq 0 ] && [ "$LINE_COUNT" -gt 0 ]; then
            warn "$file: $LINE_COUNT lines (at limit — next added line will fail)"
            if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
                echo "::warning file=$file::LLM file is at the ${MAX_LINES}-line limit"
            fi
        elif [ "$HEADROOM" -le "$WARN_HEADROOM" ] && [ "$LINE_COUNT" -gt 0 ]; then
            LINES_WORD="lines"; [ "$HEADROOM" -eq 1 ] && LINES_WORD="line"
            warn "$file: $LINE_COUNT lines (${HEADROOM} ${LINES_WORD} from limit — consider trimming)"
            if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
                echo "::warning file=$file::LLM file is close to limit (${LINE_COUNT}/${MAX_LINES} lines)"
            fi
        fi
    fi
done

echo ""
if [ "$FILE_ARGS_MODE" -eq 1 ]; then
    info "Checked $CHECKED explicitly provided file(s)"
else
    info "Checked $CHECKED file(s) in $LLM_DIR/"
fi
if [ "$MISSING_INPUTS" -gt 0 ]; then
    warn "Skipped $MISSING_INPUTS missing file argument(s)."
fi
echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "=========================================="
if [ "$VIOLATIONS" -gt 0 ]; then
    print_size_diagnostics
    error "LLM file size check found $VIOLATIONS file(s) exceeding the ${MAX_LINES}-line limit"
    echo ""
    echo "Each .llm/ file must stay within ${MAX_LINES} lines to:"
    echo "  - Stay focused on one topic"
    echo "  - Fit comfortably within LLM context windows"
    echo "  - Remain scannable and maintainable"
    echo ""
    echo "How to fix:"
    echo "  1. Identify logically distinct sections in the oversized file"
    echo "  2. Extract each section into a new file with a trigger comment header"
    echo "  3. Add cross-references between related files"
    echo "  4. Run ./scripts/generate-skills-index.sh to refresh the index"
    exit 1
elif [ "$WARNINGS" -gt 0 ]; then
    print_size_diagnostics
    echo -e "${YELLOW}[WARN]${NC} LLM file size check passed with $WARNINGS warning(s)"
    echo "Files at or near the limit should be kept trim to prevent future violations."
    exit 0
else
    success "All $CHECKED LLM file(s) are within the ${MAX_LINES}-line limit."
    exit 0
fi
