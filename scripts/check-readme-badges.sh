#!/usr/bin/env bash
# check-readme-badges.sh - Enforce consistent Shields.io badge styling in README files
#
# Ensures every img.shields.io badge URL in the target markdown file includes
# `style=for-the-badge` so badges render consistently.
#
# Usage:
#   ./scripts/check-readme-badges.sh            # Check README.md
#   ./scripts/check-readme-badges.sh README.md  # Check a specific markdown file
#
# Exit codes:
#   0 - All Shields badge URLs include style=for-the-badge
#   1 - One or more Shields badge URLs are missing style=for-the-badge
#   2 - Usage error (missing file)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_FILE="${1:-README.md}"

if [[ "$TARGET_FILE" = /* ]]; then
    TARGET_PATH="$TARGET_FILE"
else
    TARGET_PATH="$REPO_ROOT/$TARGET_FILE"
fi

if [[ ! -f "$TARGET_PATH" ]]; then
    echo "ERROR: File not found: $TARGET_FILE"
    exit 2
fi

violations=0
urls_found=0

while IFS=$'\t' read -r line_num url; do
    [[ -z "$url" ]] && continue
    urls_found=$((urls_found + 1))

    if [[ ! "$url" =~ (\?|&)style=for-the-badge(&|$|#) ]]; then
        echo "Missing style=for-the-badge: ${TARGET_FILE}:${line_num}"
        echo "  URL: $url"
        violations=$((violations + 1))
    fi
done < <(awk '
{
    line = $0
    while (match(line, /https:\/\/img\.shields\.io\/[^"'\'' )>]+/)) {
        url = substr(line, RSTART, RLENGTH)
        printf "%d\t%s\n", NR, url
        line = substr(line, RSTART + RLENGTH)
    }
}
' "$TARGET_PATH")

if [[ "$violations" -gt 0 ]]; then
    echo ""
    echo "FAILED: ${violations} Shields badge URL(s) missing style=for-the-badge in ${TARGET_FILE}."
    exit 1
fi

if [[ "$urls_found" -eq 0 ]]; then
    echo "PASS: No Shields badge URLs found in ${TARGET_FILE}."
else
    echo "PASS: All ${urls_found} Shields badge URL(s) use style=for-the-badge in ${TARGET_FILE}."
fi
