#!/usr/bin/env bash
# read-toml-string.sh - Read a simple TOML string or scalar value.
#
# Usage:
#   bash scripts/read-toml-string.sh <file> <key> [section]
#
# This entrypoint needs no Rust or taplo install for CI bootstrap steps. The
# parser is the standalone typed Python reader next to this file; the shell
# entrypoint keeps the original usage and missing-file errors.

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

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
for python in python3 python; do
    if command -v "$python" >/dev/null 2>&1 && "$python" -c 'import tomllib' >/dev/null 2>&1; then
        exec "$python" "$SCRIPT_DIR/read_toml_string.py" "$FILE" "$KEY" "$SECTION"
    fi
done

echo "ERROR: Python 3.11 or newer is required to read TOML values." >&2
exit 2
