#!/usr/bin/env bash
# read-toml-string.sh - Read a simple TOML string or scalar value.
#
# Usage:
#   bash scripts/read-toml-string.sh <file> <key> [section]
#
# This helper is intentionally small and dependency-free for CI bootstrap steps
# that need values before Rust or taplo are installed.

set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: $0 <file> <key> [section]" >&2
    exit 2
fi

FILE="$1"
KEY="$2"
SECTION="${3:-}"

if [ ! -f "$FILE" ]; then
    echo "ERROR: TOML file not found: $FILE" >&2
    exit 2
fi

awk -v key="$KEY" -v section="$SECTION" '
    function trim(value) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
        return value
    }
    function unquote_or_strip_comment(value,    quote, quote_end) {
        value = trim(value)
        quote = substr(value, 1, 1)

        if (quote == "\"" || quote == sprintf("%c", 39)) {
            value = substr(value, 2)
            quote_end = index(value, quote)
            if (quote_end > 0) {
                value = substr(value, 1, quote_end - 1)
            }
        } else {
            sub(/[[:space:]]+#.*$/, "", value)
            value = trim(value)
        }

        return value
    }
    /^[[:space:]]*(#|$)/ { next }
    /^[[:space:]]*\[\[?[^]]+\]\]?[[:space:]]*(#.*)?$/ {
        current = $0
        sub(/[[:space:]]*#.*$/, "", current)
        gsub(/^[[:space:]]*\[\[?|\]\]?[[:space:]]*$/, "", current)
        next
    }
    (section == "" && current == "") || current == section {
        line = $0
        sub(/^[[:space:]]*/, "", line)
        if (line ~ "^" key "[[:space:]]*=") {
            sub(/^[^=]*=/, "", line)
            print unquote_or_strip_comment(line)
            found = 1
            exit
        }
    }
    END { exit found == 1 ? 0 : 1 }
' "$FILE"
