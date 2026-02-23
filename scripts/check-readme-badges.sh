#!/usr/bin/env bash
# check-readme-badges.sh - Enforce consistent Shields.io badge styling in README files
#
# Ensures every img.shields.io badge URL in the target markdown file includes
# `style=for-the-badge` so badges render consistently.
# By default this script does not require any minimum number of Shields badges.
# Use --require-at-least-one to enforce that policy explicitly.
#
# Usage:
#   ./scripts/check-readme-badges.sh            # Check README.md
#   ./scripts/check-readme-badges.sh README.md  # Check a specific markdown file
#   ./scripts/check-readme-badges.sh --require-at-least-one README.md
#
# Exit codes:
#   0 - All Shields badge URLs include style=for-the-badge
#   1 - One or more policy violations (missing style or required badges absent)
#   2 - Usage error (missing file)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_FILE="README.md"
REQUIRE_AT_LEAST_ONE=false
TARGET_SET=false

usage() {
    cat <<'EOF'
Usage:
  ./scripts/check-readme-badges.sh [--require-at-least-one] [README.md]

Options:
  --require-at-least-one  Fail if no Shields badge URLs are found.
  -h, --help              Show this help text.
EOF
}

while (($# > 0)); do
    case "$1" in
        --require-at-least-one)
            REQUIRE_AT_LEAST_ONE=true
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --)
            shift
            while (($# > 0)); do
                if [[ "$TARGET_SET" == true ]]; then
                    echo "ERROR: Too many positional arguments."
                    usage
                    exit 2
                fi
                TARGET_FILE="$1"
                TARGET_SET=true
                shift
            done
            break
            ;;
        -*)
            echo "ERROR: Unknown option: $1"
            usage
            exit 2
            ;;
        *)
            if [[ "$TARGET_SET" == true ]]; then
                echo "ERROR: Too many positional arguments."
                usage
                exit 2
            fi
            TARGET_FILE="$1"
            TARGET_SET=true
            ;;
    esac
    shift
done

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
    while (match(line, /https:\/\/img\.shields\.io\/[^"'\''[:space:])>]+/)) {
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
    if [[ "$REQUIRE_AT_LEAST_ONE" == true ]]; then
        echo "FAILED: ${TARGET_FILE} requires at least one Shields badge URL, but none were found."
        exit 1
    fi
    echo "PASS: No Shields badge URLs found in ${TARGET_FILE}."
else
    echo "PASS: All ${urls_found} Shields badge URL(s) use style=for-the-badge in ${TARGET_FILE}."
fi
