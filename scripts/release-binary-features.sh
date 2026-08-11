#!/usr/bin/env bash
# Select binary features from the exact source being released. An absent marker
# deliberately preserves historical releases' default-feature binary contract.

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <Cargo.toml>" >&2
    exit 2
fi
if [ ! -f "$1" ]; then
    echo "ERROR: Cargo manifest not found: $1" >&2
    exit 2
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
MARKER=$(bash "$SCRIPT_DIR/read-toml-string.sh" \
    "$1" built-in-tls package.metadata.signal-fish-release || true)

case "$MARKER" in
    true)
        printf '%s\n' '--features tls'
        ;;
    "" | false)
        printf '\n'
        ;;
    *)
        echo "ERROR: unsupported package.metadata.signal-fish-release built-in-tls value: $MARKER" >&2
        exit 1
        ;;
esac
