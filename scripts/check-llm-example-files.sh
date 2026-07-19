#!/usr/bin/env bash
# LLM Skill Example Extraction Checker
#
# Enforces the repository policy that skills must not embed in-file example
# sections. Detailed examples belong in a skill's `references/` directory and
# must be linked from its `SKILL.md` entrypoint.
#
# Usage:
#   ./scripts/check-llm-example-files.sh
#   ./scripts/check-llm-example-files.sh --files .llm/skills/foo/SKILL.md
#
# Exit codes:
#   0 = Pass (no inline example sections found)
#   1 = Violations found
#   2 = Invalid usage

set -euo pipefail

LLM_SKILLS_DIR=".llm/skills"
FILE_ARGS_MODE=0
ERRORS=0
CHECKED=0
MISSING_INPUTS=0

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

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

ok() {
    echo -e "${GREEN}[OK]${NC} $1"
}

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

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
    if [ ! -d "$LLM_SKILLS_DIR" ]; then
        warn "Directory '$LLM_SKILLS_DIR' not found - skipping check"
        exit 0
    fi
    while IFS= read -r file; do
        FILES_TO_CHECK+=("$file")
    done < <(find "$LLM_SKILLS_DIR" -mindepth 2 -maxdepth 2 -type f -name "SKILL.md" | LC_ALL=C sort)
fi

echo -e "${BLUE}LLM Example Extraction Checker${NC}"
echo "Repository: $REPO_ROOT"
echo ""

if [ "$FILE_ARGS_MODE" -eq 1 ]; then
    info "Scanning ${#FILES_TO_CHECK[@]} explicitly provided file(s)..."
else
    info "Scanning skill files in $LLM_SKILLS_DIR/..."
fi
echo ""

for file in "${FILES_TO_CHECK[@]}"; do
    if [ ! -f "$file" ]; then
        warn "Skipping non-existent file: $file"
        MISSING_INPUTS=$((MISSING_INPUTS + 1))
        continue
    fi

    case "$file" in
        */index.md) continue ;;
    esac

    CHECKED=$((CHECKED + 1))

    # Detect headings that define inline example sections, which are disallowed.
    # Allowed location for detailed examples is the skill's references directory.
    if MATCHES=$(grep -En '^[[:space:]]*#{2,6}[[:space:]]+((Real-World[[:space:]]+)?Examples?|Example([[:space:]:-]|$))' "$file" || true); then
        if [ -n "$MATCHES" ]; then
            while IFS= read -r match; do
                [ -z "$match" ] && continue
                line_no=${match%%:*}
                heading=${match#*:}
                error "$file:$line_no: inline example heading is disallowed -> $heading"
            done <<< "$MATCHES"
            echo "        Move each example into the skill's references/ directory and link it from SKILL.md."
            echo ""
        fi
    fi
done

echo ""
if [ "$FILE_ARGS_MODE" -eq 1 ]; then
    info "Checked $CHECKED explicitly provided file(s)"
else
    info "Checked $CHECKED skill file(s)"
fi
if [ "$MISSING_INPUTS" -gt 0 ]; then
    warn "Skipped $MISSING_INPUTS missing file argument(s)."
fi
echo ""

echo "=========================================="
if [ "$ERRORS" -gt 0 ]; then
    error "Found $ERRORS inline-example policy violation(s)"
    echo ""
    echo "Policy:"
    echo "  - Parent skills may summarize examples and link out"
    echo "  - Each concrete example must live in the skill's references/ directory"
    echo ""
    echo "How to fix:"
    echo "  1. Create one focused references/example-*.md file per example"
    echo "  2. Replace inline example sections with links"
    echo "  3. Re-run ./scripts/check-llm-example-files.sh"
    exit 1
fi

ok "No inline example sections found in checked skill files."
exit 0
