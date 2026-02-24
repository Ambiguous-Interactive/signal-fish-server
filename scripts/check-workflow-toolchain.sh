#!/usr/bin/env bash
# Workflow Toolchain Input Validator
#
# Scans all GitHub Actions workflow files for usages of dtolnay/rust-toolchain@v1
# and validates that each occurrence has a `toolchain:` input in its `with:` block.
#
# Without an explicit `toolchain:` input, the action falls back to whatever
# rust-toolchain.toml provides (or the default stable channel), which may not
# match the intent of the workflow step. Requiring an explicit input prevents
# silent toolchain mismatches.
#
# Usage:
#   ./scripts/check-workflow-toolchain.sh
#
# Exit codes:
#   0 = All dtolnay/rust-toolchain usages specify a toolchain input
#   1 = One or more usages are missing the toolchain input
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

ERRORS=0

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BLUE}Workflow Toolchain Input Validator${NC}"
echo "Repository: $REPO_ROOT"
echo ""

# ---------------------------------------------------------------------------
# Collect workflow files
# ---------------------------------------------------------------------------
WORKFLOW_DIR=".github/workflows"

if [ ! -d "$WORKFLOW_DIR" ]; then
    echo -e "${YELLOW}[WARN]${NC} No $WORKFLOW_DIR directory found — nothing to check"
    exit 0
fi

WORKFLOW_FILES=()
for f in "$WORKFLOW_DIR"/*.yml "$WORKFLOW_DIR"/*.yaml; do
    [ -f "$f" ] && WORKFLOW_FILES+=("$f")
done

if [ ${#WORKFLOW_FILES[@]} -eq 0 ]; then
    echo -e "${YELLOW}[WARN]${NC} No workflow files found in $WORKFLOW_DIR — nothing to check"
    exit 0
fi

info "Scanning ${#WORKFLOW_FILES[@]} workflow file(s) for dtolnay/rust-toolchain usages..."
echo ""

# ---------------------------------------------------------------------------
# For each workflow file, find every dtolnay/rust-toolchain@v1 usage and
# verify that a `toolchain:` input appears within the next few lines
# (before another step marker `- name:` / `- uses:` or a dedented line
# signals the end of the `with:` block).
# ---------------------------------------------------------------------------
TOTAL_USAGES=0
VALID_USAGES=0

for workflow in "${WORKFLOW_FILES[@]}"; do
    WORKFLOW_NAME=$(basename "$workflow")
    TOTAL_LINES=$(wc -l < "$workflow")

    # Read the file into an array for line-by-line lookahead.
    # Index 0 = line 1 of the file.
    mapfile -t LINES < "$workflow"

    for (( i = 0; i < ${#LINES[@]}; i++ )); do
        line="${LINES[$i]}"

        # Match lines containing dtolnay/rust-toolchain (any version tag)
        if [[ "$line" =~ dtolnay/rust-toolchain@v[0-9] ]]; then
            LINE_NUM=$((i + 1))
            TOTAL_USAGES=$((TOTAL_USAGES + 1))
            FOUND_TOOLCHAIN=false

            # Look ahead up to 5 lines for a `toolchain:` key.
            # Stop early if we hit another step marker (indicating the
            # `with:` block has ended or was never opened).
            for (( j = 1; j <= 5; j++ )); do
                LOOK_INDEX=$((i + j))
                # Guard against reading past end of file
                if [ "$LOOK_INDEX" -ge "${#LINES[@]}" ]; then
                    break
                fi

                NEXT_LINE="${LINES[$LOOK_INDEX]}"

                # Check if this line contains a toolchain: key
                if [[ "$NEXT_LINE" =~ ^[[:space:]]+toolchain: ]]; then
                    FOUND_TOOLCHAIN=true
                    break
                fi

                # If we hit another step definition, the with: block (if any)
                # has ended without a toolchain: key.
                if [[ "$NEXT_LINE" =~ ^[[:space:]]*-[[:space:]]+(name:|uses:) ]]; then
                    break
                fi

                # If we hit a completely dedented line (a new top-level YAML
                # key or job boundary), stop looking.
                if [[ "$NEXT_LINE" =~ ^[a-zA-Z] ]] && [[ ! "$NEXT_LINE" =~ ^[[:space:]] ]]; then
                    break
                fi
            done

            if [ "$FOUND_TOOLCHAIN" = "true" ]; then
                VALID_USAGES=$((VALID_USAGES + 1))
            else
                error "$WORKFLOW_NAME:$LINE_NUM: dtolnay/rust-toolchain used without explicit 'toolchain:' input"
                error "  Line: $(echo "$line" | sed 's/^[[:space:]]*//')"
                error "  Add a 'with: toolchain: <version>' block to pin the Rust version."
                if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
                    echo "::error file=$workflow,line=$LINE_NUM::dtolnay/rust-toolchain used without explicit toolchain input"
                fi
            fi
        fi
    done
done

echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "=========================================="
info "Found $TOTAL_USAGES dtolnay/rust-toolchain usage(s) across ${#WORKFLOW_FILES[@]} workflow file(s)"

if [ "$TOTAL_USAGES" -eq 0 ]; then
    success "No dtolnay/rust-toolchain usages found — nothing to validate"
    exit 0
fi

if [ "$ERRORS" -gt 0 ]; then
    # Prefer the exact pinned toolchain from rust-toolchain.toml for remediation text.
    # This avoids suggesting moving aliases like `stable`, which violate policy tests.
    PINNED_TOOLCHAIN=$(awk -F'=' '
        /^[[:space:]]*channel[[:space:]]*=/ {
            value = $2
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            gsub(/"/, "", value)
            print value
            exit
        }
    ' rust-toolchain.toml 2>/dev/null || true)
    if [ -z "$PINNED_TOOLCHAIN" ]; then
        PINNED_TOOLCHAIN="1.88.0"
    fi

    echo ""
    error "Toolchain input check found $ERRORS violation(s) ($VALID_USAGES/$TOTAL_USAGES valid)"
    echo ""
    echo "Every dtolnay/rust-toolchain step must include an explicit 'toolchain:' input."
    echo "Without it, the action falls back to rust-toolchain.toml or the default stable"
    echo "channel, which may silently differ from the intended toolchain."
    echo ""
    echo "Fix by adding a 'with:' block:"
    echo "  - uses: dtolnay/rust-toolchain@v1"
    echo "    with:"
    echo "      toolchain: $PINNED_TOOLCHAIN  # use an exact pinned version"
    exit 1
else
    success "All $TOTAL_USAGES dtolnay/rust-toolchain usage(s) have an explicit 'toolchain:' input"
    exit 0
fi
