#!/usr/bin/env bash
# Install hax's pinned Charon and Aeneas tools with bounded retries.
#
# hax owns the download transport and verifies its embedded checksums. This
# wrapper makes transient GitHub release failures visible and recoverable
# without changing the pinned tool versions in hax.toml.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project="${1:-${repo_root}/formal/source-linked/lean}"
attempts="${SIGNAL_FISH_HAX_INSTALL_ATTEMPTS:-3}"
delay_seconds="${SIGNAL_FISH_HAX_INSTALL_DELAY_SECONDS:-5}"

case "$attempts" in
    "" | *[!0-9]* | 0)
        echo "ERROR: SIGNAL_FISH_HAX_INSTALL_ATTEMPTS must be a positive integer" >&2
        exit 2
        ;;
esac
case "$delay_seconds" in
    "" | *[!0-9]*)
        echo "ERROR: SIGNAL_FISH_HAX_INSTALL_DELAY_SECONDS must be a non-negative integer" >&2
        exit 2
        ;;
esac

if [[ ! -d "$project" ]]; then
    echo "ERROR: hax project directory not found: $project" >&2
    exit 2
fi

cd "$project"
for ((attempt = 1; attempt <= attempts; attempt++)); do
    if cargo hax tools install; then
        if ! cargo hax tools show; then
            echo "ERROR: hax tool cache is not readable after installation" >&2
            exit 1
        fi
        exit 0
    fi
    echo "hax tool install attempt ${attempt}/${attempts} failed" >&2
    if ((attempt < attempts)); then
        sleep "$delay_seconds"
    fi
done

echo "ERROR: hax tool install exhausted ${attempts} attempts" >&2
exit 1
