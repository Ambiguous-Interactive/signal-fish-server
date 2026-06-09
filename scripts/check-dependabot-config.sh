#!/usr/bin/env bash
# check-dependabot-config.sh
#
# Validates .github/dependabot.yml for comment-config consistency.
#
# Root cause this prevents: stale relative-terminology comments (e.g.,
# "Higher PR limit", "Moderate PR limit", "Lower PR limit") that drift
# out of sync when open-pull-requests-limit values are changed, misleading
# future maintainers about the actual policy.
#
# Checks:
#   1. No relative PR-limit terminology (Higher/Moderate/Lower) in comments.
#      Relative terms become incorrect the moment any limit changes.
#      Use exact values ("PR limit: 2") or neutral language ("Consolidated PR limit").
#   2. Every update entry has open-pull-requests-limit explicitly defined.
#      Omitted limits silently default to GitHub's ceiling (5), which contradicts
#      a consolidation policy that wants low limits.
#   3. If a comment within a section contains an inline numeric PR limit
#      (e.g., "PR limit: 5"), it must match the configured
#      open-pull-requests-limit value for that section.
#
# Usage:
#   ./scripts/check-dependabot-config.sh
#
# Exit codes:
#   0 = All checks passed
#   1 = One or more validation errors found
#   2 = Invalid usage or file not found

set -euo pipefail

# ---------------------------------------------------------------------------
# Color output (disabled when not a TTY or NO_COLOR is set)
# ---------------------------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
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

FILE=".github/dependabot.yml"

if [ ! -f "$FILE" ]; then
    echo -e "${RED}[ERROR]${NC} $FILE not found (run from repository root)"
    exit 2
fi

# Temp file for Python scripts — avoids heredoc-inside-$() which fails bash 3.2
# syntax checking on macOS (bash -n). Using a top-level heredoc into a temp
# file is safe on all bash versions.
_TMPPY=$(mktemp)
trap 'rm -f "$_TMPPY"' EXIT

ERRORS=0

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

echo -e "${BLUE}Validating${NC} $FILE..."
echo ""

# ---------------------------------------------------------------------------
# Check 1: No relative PR-limit terminology
#
# Relative terms ("Higher", "Moderate", "Lower") used to describe PR limits
# become stale and misleading whenever any limit value is changed.  They
# imply a comparison between sections that may no longer hold.  Neutral or
# exact-value language is always safe.
# ---------------------------------------------------------------------------
info "Check 1: No relative PR-limit terminology in comments..."

RELATIVE_HITS=$(grep -niE "^[[:space:]]*#[[:space:]]*(Higher|Moderate|Lower)[[:space:]]PR[[:space:]]limit" "$FILE" || true)
if [ -n "$RELATIVE_HITS" ]; then
    error "Relative PR-limit terminology found — these comments drift when limits change:"
    echo "$RELATIVE_HITS" | while IFS= read -r line; do
        echo "  $line"
    done
    echo -e "${YELLOW}  Fix:${NC} Replace with neutral language (e.g., 'Consolidated PR limit: ...') or"
    echo -e "       exact-value language (e.g., 'PR limit: 2 — reason')."
    echo ""
else
    success "No relative PR-limit terminology found"
fi

# ---------------------------------------------------------------------------
# Check 2: Every update entry has open-pull-requests-limit defined
#
# Without an explicit limit, GitHub defaults to 5.  For a consolidation
# strategy that intentionally keeps limits low, every section must declare
# its limit so it is visible in code review.
# ---------------------------------------------------------------------------
info "Check 2: All update entries have open-pull-requests-limit defined..."

# Use Python+PyYAML for reliable YAML structural validation when available.
# Falls back to awk-based heuristic if python3 or PyYAML is absent.
if command -v python3 >/dev/null 2>&1 && python3 -c 'import yaml' 2>/dev/null; then
    cat > "$_TMPPY" << 'PYEOF'
import sys
import yaml

filepath = sys.argv[1]
with open(filepath, "r") as f:
    config = yaml.safe_load(f)

updates = config.get("updates", [])
missing = []
for entry in updates:
    ecosystem = entry.get("package-ecosystem", "<unknown>")
    directory = entry.get("directory", "/")
    if "open-pull-requests-limit" not in entry:
        missing.append(f"  ecosystem={ecosystem!r} directory={directory!r}")

if missing:
    print("MISSING_LIMIT")
    for m in missing:
        print(m)
else:
    print("OK")
PYEOF
    MISSING_LIMIT=$(python3 "$_TMPPY" "$FILE")
    if grep -q "^MISSING_LIMIT" <<< "$MISSING_LIMIT"; then
        error "The following update entries are missing open-pull-requests-limit:"
        echo "$MISSING_LIMIT" | grep -v "^MISSING_LIMIT" | grep -v "^OK" || true
        echo -e "${YELLOW}  Fix:${NC} Add 'open-pull-requests-limit: <N>' to each listed entry."
        echo ""
    else
        success "All update entries have open-pull-requests-limit defined"
    fi
else
    # Awk-based fallback: look for package-ecosystem lines not followed by
    # open-pull-requests-limit before the next package-ecosystem entry.
    AWK_RESULT=$(awk '
        /^[[:space:]]*-[[:space:]]+package-ecosystem:/ {
            if (in_section && !found_limit) {
                print "MISSING_LIMIT: " section_name
            }
            section_name = $0
            in_section = 1
            found_limit = 0
        }
        /^[[:space:]]+open-pull-requests-limit:/ {
            found_limit = 1
        }
        END {
            if (in_section && !found_limit) {
                print "MISSING_LIMIT: " section_name
            }
        }
    ' "$FILE")
    if [ -n "$AWK_RESULT" ]; then
        error "Sections possibly missing open-pull-requests-limit (awk heuristic — install Python 3 for reliable YAML parsing):"
        echo "$AWK_RESULT" | while IFS= read -r line; do echo "  $line"; done
        echo ""
    else
        success "All update entries appear to have open-pull-requests-limit defined (awk heuristic)"
    fi
    echo -e "${YELLOW}[NOTE]${NC} Python 3 with PyYAML not found — Check 2 used awk heuristic only." \
        "Install PyYAML ('pip install pyyaml') for reliable YAML parsing."
fi

# ---------------------------------------------------------------------------
# Check 3: Inline comment PR limit numbers match configured values
#
# If a section comment says "PR limit: 5", the configured
# open-pull-requests-limit must also be 5.  Mismatches mean the comment
# was written when the limit was different and not updated afterward.
# ---------------------------------------------------------------------------
info "Check 3: Inline PR limit numbers in comments match configured values..."

if command -v python3 >/dev/null 2>&1 && python3 -c 'import yaml' 2>/dev/null; then
    cat > "$_TMPPY" << 'PYEOF'
import sys
import re
import yaml

filepath = sys.argv[1]

# Load the raw lines to extract per-section comment blocks.
with open(filepath, "r") as f:
    raw_lines = f.readlines()

# Load YAML structure to get configured limits.
with open(filepath, "r") as f:
    config = yaml.safe_load(f)

updates = config.get("updates", [])
mismatches = []

# For each update entry, find its line range by matching the
# package-ecosystem + directory keys to their position in the file,
# then scan the comment block immediately before that entry.
for entry in updates:
    ecosystem = entry.get("package-ecosystem", "<unknown>")
    directory = entry.get("directory", "/")
    configured_limit = entry.get("open-pull-requests-limit")
    if configured_limit is None:
        continue  # Already caught by Check 2.

    # Find the line in the file for this entry's package-ecosystem declaration.
    entry_line_idx = None
    ec_pattern = re.compile(
        r'^\s*-\s+package-ecosystem:\s+["\']?' + re.escape(ecosystem) + r'["\']?'
    )
    dir_pattern = re.compile(
        r'^\s+directory:\s+["\']?' + re.escape(directory) + r'["\']?'
    )

    i = 0
    while i < len(raw_lines):
        if ec_pattern.match(raw_lines[i]):
            # Confirm next few lines contain the matching directory.
            for j in range(i + 1, min(i + 6, len(raw_lines))):
                if dir_pattern.match(raw_lines[j]):
                    entry_line_idx = i
                    break
            if entry_line_idx is not None:
                break
        i += 1

    if entry_line_idx is None:
        continue

    # Collect comment lines before this entry (stop at previous blank/non-comment).
    comment_block = []
    k = entry_line_idx - 1
    while k >= 0:
        stripped = raw_lines[k].strip()
        if stripped.startswith("#") or stripped == "":
            comment_block.append(stripped)
            k -= 1
        else:
            break

    # Search for "PR limit: <N>" or "PR limit (<N>)" patterns in the comment block.
    limit_comment_pattern = re.compile(
        r'PR\s+limit[:\s(]\s*(\d+)', re.IGNORECASE
    )
    for comment_line in comment_block:
        m = limit_comment_pattern.search(comment_line)
        if m:
            commented_limit = int(m.group(1))
            if commented_limit != configured_limit:
                mismatches.append(
                    f"  ecosystem={ecosystem!r} directory={directory!r}: "
                    f"comment says {commented_limit}, "
                    f"configured open-pull-requests-limit={configured_limit}"
                )

if mismatches:
    print("MISMATCH")
    for m in mismatches:
        print(m)
else:
    print("OK")
PYEOF
    MISMATCH_RESULT=$(python3 "$_TMPPY" "$FILE")
    if grep -q "^MISMATCH" <<< "$MISMATCH_RESULT"; then
        error "PR limit numbers in comments do not match configured open-pull-requests-limit:"
        echo "$MISMATCH_RESULT" | grep -v "^MISMATCH" | grep -v "^OK" || true
        echo -e "${YELLOW}  Fix:${NC} Update the comment to match the configured value, or update"
        echo -e "       the configured value to match the intended policy."
        echo ""
    else
        success "All inline PR limit numbers match configured values"
    fi
else
    echo -e "${YELLOW}[NOTE]${NC} Python 3 with PyYAML not found — Check 3 skipped." \
        "Install PyYAML ('pip install pyyaml') for inline limit cross-check."
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
if [ "$ERRORS" -gt 0 ]; then
    echo -e "${RED}[FAIL]${NC} $ERRORS error(s) found in $FILE."
    echo ""
    echo -e "${YELLOW}Tip:${NC} Run './scripts/check-dependabot-config.sh' locally to diagnose."
    exit 1
else
    echo -e "${GREEN}[PASS]${NC} $FILE looks good — all checks passed."
fi
